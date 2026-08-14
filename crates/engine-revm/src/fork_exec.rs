//! Shadow executor: replay a sequencer-feed tx against a [`ForkDb`]-backed warm cache and
//! report which accounts it mutated — *without committing*.
//!
//! This is the engine-side substrate for shadow-mode detection (§13 EventReady-(b)): take
//! the exact tx the sequencer ordered, run it through REVM over live forked state, and see
//! which resident pools its storage writes touch. Because the parity proof
//! (`revm_fork_parity_slot0`) shows the fork DB feeds REVM the *exact* live pool state,
//! the set of pools a replay mutates is the set the real tx will move — at feed latency,
//! for the ~94% of flow whose calldata we can't statically decode.
//!
//! Each [`replay_tx`](ForkExecutor::replay_tx) is isolated: reads warm the shared cache,
//! but writes are discarded (no `commit`), so one tx never sees another's effects and the
//! cache stays pinned at the fork block.

use crate::compiled::RevmBackendMode;
use crate::fork_db::{ForkDb, StateFetcher};
use revm::context::result::{ExecutionResult, Output, ResultAndState};
use revm::context::{Context, TxEnv};
use revm::database::{AccountState, CacheDB};
use revm::primitives::{Address, Bytes, TxKind, U256};
use revm::state::EvmState;
use revm::{Database, DatabaseCommit, ExecuteEvm, MainBuilder, MainContext};

// NOTE: changed-slot collection is deliberately UNCAPPED. EvmState is a HashMap, so any cap keeps
// an ARBITRARY subset — a partially-applied contract post-state that poisons the standing state
// mirror (measured live 2026-07-08: pancake_v3 4a/31d, algebra 7a/22d WITH mirror vs 16a/5d,
// 12a/14d without). A tx is gas-bounded (~1.5k cold SSTOREs at the 30M replay limit), so the full
// set is small by construction; the gas limit IS the cap.

/// One EVM transact over the warm cache (revm 40 Context API). Centralizes what the four
/// call sites share; the flags preserve each site's revm-19 semantics exactly:
/// `gas_price = 0` + `disable_nonce_check` (the old `tx.nonce = None`) so replaying a
/// not-yet-mined feed tx never reverts on fee/nonce bookkeeping, optional fork-block env,
/// and the EIP-170 lift only where the old code set it (deploy/validation paths).
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(feature = "revmc"), allow(unused_variables))]
fn transact_on<F: StateFetcher>(
    db: &mut CacheDB<ForkDb<F>>,
    chain_id: u64,
    block_env: Option<(u64, u64)>,
    lift_code_size: bool,
    from: [u8; 20],
    kind: TxKind,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    // Stage 3: the RESOLVED compiled-backend policy for THIS executor (validator reads
    // REVM_BACKEND; the mirror-advancer sets its own via REVMC_ADVANCER). Feature-off it is unused
    // (no compiled path exists). `Interpreter` ⇒ plain revm, byte-identical to feature-off.
    backend: RevmBackendMode,
) -> Result<ResultAndState, String> {
    let mut evm = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = chain_id;
            cfg.disable_nonce_check = true;
            // EIP-7825 (Osaka default) caps tx gas at 2^24; replayed Arbitrum txs and the
            // 30M-gas validation calls legitimately exceed that. revm 19 had no cap.
            cfg.tx_gas_limit_cap = Some(u64::MAX);
            if lift_code_size {
                cfg.limit_contract_code_size = Some(0x40000);
            }
        })
        .modify_block_chained(|b| {
            if let Some((n, ts)) = block_env {
                b.number = U256::from(n);
                b.timestamp = U256::from(ts);
            }
        })
        .build_mainnet();
    let tx = TxEnv::builder()
        .caller(Address::from(from))
        .kind(kind)
        .value(value)
        .data(Bytes::from(data))
        .gas_limit(gas_limit)
        .gas_price(0)
        // The builder defaults chain_id to Some(1); None skips per-tx chain validation (the
        // revm-19 behaviour — these are synthetic replays, and CHAINID reads cfg.chain_id).
        .chain_id(None)
        .build()
        .map_err(|e| format!("tx_env {e:?}"))?;
    // 2026-07-10 revmc: with REVM_BACKEND=jit (feature `revmc`), wrap in JitEvm — per-code-
    // hash compiled dispatch with transparent interpreter fallback (a compile in flight never
    // blocks an execution; non-blocking backend). Default path stays byte-identical (proven by
    // `jit_matches_interpreter_byte_exact`). tx is built once above; only the EVM wrap differs.
    #[cfg(feature = "revmc")]
    if backend.uses_jit() {
        if let Some(jit_backend) = crate::compiled::global_jit_backend() {
            let mut jit = revmc::revm_evm::JitEvm::new(evm, jit_backend.clone());
            return jit.transact(tx).map_err(|e| format!("{e:?}"));
        }
    }
    evm.transact(tx).map_err(|e| format!("{e:?}"))
}

fn abi_word_usize(word: &[u8]) -> Option<usize> {
    if word.len() != 32 || word[..24].iter().any(|&b| b != 0) {
        return None;
    }
    let mut n = [0u8; 8];
    n.copy_from_slice(&word[24..]);
    Some(u64::from_be_bytes(n) as usize)
}

fn decode_solidity_error_string(out: &[u8]) -> Option<String> {
    const ERROR_STRING_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];
    if out.len() < 68 || out[..4] != ERROR_STRING_SELECTOR {
        return None;
    }
    let offset = abi_word_usize(&out[4..36])?;
    let len_pos = 4usize.checked_add(offset)?;
    let data_pos = len_pos.checked_add(32)?;
    if out.len() < data_pos {
        return None;
    }
    let len = abi_word_usize(&out[len_pos..data_pos])?;
    let data_end = data_pos.checked_add(len)?;
    if out.len() < data_end {
        return None;
    }
    std::str::from_utf8(&out[data_pos..data_end])
        .ok()
        .map(str::to_string)
}

/// A single storage slot a replay mutated, with the value REVM read going in (`original`)
/// and the value it computed (`present`). `original` is what a canonical
/// `eth_getStorageAt` at the fork block must equal for the replay to be trustworthy — the
/// per-emission parity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedSlot {
    /// Contract whose storage changed.
    pub address: [u8; 20],
    /// Storage slot key.
    pub slot: U256,
    /// Value REVM read before the write (must match canonical pre-state).
    pub original: U256,
    /// Value REVM computed (the predicted post-state).
    pub present: U256,
}

/// Outcome of a shadow replay.
#[derive(Debug, Clone)]
pub struct ReplayOutcome {
    /// Whether the tx executed without revert/halt.
    pub success: bool,
    /// Raw EVM return or revert bytes.
    pub output: Vec<u8>,
    /// Decoded revert/halt/error reason when available.
    pub reason: String,
    /// Addresses whose storage was mutated (`present_value != original_value`).
    pub changed: Vec<[u8; 20]>,
    /// The individual mutated slots — the COMPLETE set (gas-bounded, never truncated), for the
    /// canonical pre-state parity gate and the standing state-mirror publish.
    pub changed_slots: Vec<ChangedSlot>,
}

/// Outcome of a read-only validation call, including uncommitted storage writes.
#[derive(Debug, Clone)]
pub struct CallOutcome {
    /// Whether the call executed without revert/halt.
    pub success: bool,
    /// Raw EVM return or revert bytes.
    pub output: Vec<u8>,
    /// Decoded revert/halt/error reason when available.
    pub reason: String,
    /// Storage slots the call would change if committed.
    pub changed_slots: Vec<ChangedSlot>,
    /// EVM gas units consumed by the call (revm-reported, all outcomes). Used by the
    /// candidate-record economics so the persisted `estimated_gas_wei` is the measured
    /// gas, not a per-family constant.
    pub gas_used: u64,
    /// [Stage 2 · CacheOnly] `(address, slot)` reads that missed the trusted mirror and aborted
    /// this validation. Non-empty ONLY on a [`ForkExecutor::call_ro_cache_only`] `pending_state`
    /// abort (`success = false`, `reason = "pending_state"`); ALWAYS empty for `call_ro` /
    /// `call_ro_with_changes` (byte-identical to today). The caller enqueues these to the async
    /// warmer and retries the candidate — it must NEVER submit on a `pending_state` outcome.
    pub pending_slots: Vec<([u8; 20], U256)>,
}

impl CallOutcome {
    /// True when this outcome is a cache-only `pending_state` abort (a SKIPPED validation to be
    /// warmed + retried, never submitted). Equivalent to `!pending_slots.is_empty()`.
    pub fn is_pending_state(&self) -> bool {
        !self.pending_slots.is_empty()
    }
}

/// A [`StateFetcher`] that serves reads from a BORROWED warm cache and records everything
/// else as a miss, returning defaults — it can never reach a network fetcher. Backing for
/// [`ForkExecutor::probe_call_misses`]: the same lazy `ForkDb` plumbing REVM always uses,
/// with the network edge swapped for a recorder (misses are exactly the reads the real
/// execution would have paid a cold round-trip for).
struct ProbeFetcher<'a> {
    warm: &'a revm::database::Cache,
    /// The tx caller. When it is not in the warm cache it is recorded as a miss (the real
    /// replay warms its real balance) but served a SYNTHETIC max balance here so a
    /// value-carrying call does not halt on "insufficient funds" before the swap read-set is
    /// discovered — the real replay runs against the sender's real (sufficient) chain balance
    /// and executes the swap, so this keeps the probe on the SAME code path. gas_price is 0, so
    /// no ETH is actually spent; only the caller-ETH precondition is satisfied.
    caller: [u8; 20],
    accounts: std::cell::RefCell<Vec<[u8; 20]>>,
    slots: std::cell::RefCell<Vec<([u8; 20], U256)>>,
}

impl StateFetcher for ProbeFetcher<'_> {
    fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
        if let Some(acc) = self.warm.accounts.get(&Address::from(a)) {
            // Serve the cached account. Code may live inline on the account or in the
            // shared contracts map (keyed by code hash) — check both before calling it
            // an EOA.
            let inline = acc
                .info
                .code
                .as_ref()
                .map(|c| c.original_bytes().to_vec())
                .filter(|c| !c.is_empty());
            let code = match inline {
                Some(c) => Some(c),
                None if acc.info.code_hash != revm::primitives::KECCAK_EMPTY => self
                    .warm
                    .contracts
                    .get(&acc.info.code_hash)
                    .map(|b| b.original_bytes().to_vec()),
                None => None,
            };
            return (acc.info.balance, acc.info.nonce, code);
        }
        self.accounts.borrow_mut().push(a);
        // Uncached caller → synthetic max balance so a value-carrying probe doesn't halt on
        // funds before reaching the swap read-set (see the `caller` field docs).
        if a == self.caller {
            return (U256::MAX, 0, None);
        }
        (U256::ZERO, 0, None)
    }

    fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
        if let Some(acc) = self.warm.accounts.get(&Address::from(a)) {
            if let Some(v) = acc.storage.get(&slot) {
                return *v;
            }
            // A cleared / known-non-existing account serves canonical zeros from the
            // cache layer itself — that is NOT a miss the caller could warm.
            if matches!(
                acc.account_state,
                AccountState::StorageCleared | AccountState::NotExisting
            ) {
                return U256::ZERO;
            }
        }
        self.slots.borrow_mut().push((a, slot));
        U256::ZERO
    }
}

/// Accounts + storage slots a [`ForkExecutor::probe_call_misses`] dry-run needed but did
/// not find in the warm cache. The caller warms these (batched `eth_getStorageAt` /
/// `account_bundle`) and re-probes until empty — converting N serialized lazy fetches
/// during a replay/validation into one or two batched round-trips before it.
#[derive(Debug, Default, Clone)]
pub struct ProbeMisses {
    /// Accounts (code/balance/nonce) the execution touched that are not cached.
    pub accounts: Vec<[u8; 20]>,
    /// `(contract, slot)` storage reads that fell through the warm cache.
    pub slots: Vec<([u8; 20], U256)>,
}

impl ProbeMisses {
    /// True when the probe found everything it needed in the warm cache.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty() && self.slots.is_empty()
    }
}

/// REVM executor over a [`ForkDb`] warm cache. Construct with a [`StateFetcher`] pinned to
/// a snapshot block; rebuild to advance the snapshot.
pub struct ForkExecutor<F: StateFetcher> {
    db: CacheDB<ForkDb<F>>,
    chain_id: u64,
    /// Optional (block_number, block_timestamp) applied to EVERY execution path (deploy,
    /// validation calls AND replays) so contracts that read `block.number`/`block.timestamp`
    /// (price-feed staleness, deadlines, Algebra adaptive fee / V3 oracle writes) behave as at
    /// the fork block. `None` runs at revm defaults (number 0 / timestamp ~0) — acceptable only
    /// for synthetic tests, never for replaying real pool traffic.
    block_env: Option<(u64, u64)>,
    /// Stage 3: this executor's compiled-backend policy. `None` ⇒ read `REVM_BACKEND` (the
    /// validator's default); `Some(mode)` ⇒ an explicit override (the mirror-advancer sets its own
    /// from `REVMC_ADVANCER`, independent of the validator's backend). Feature-off this only selects
    /// the interpreter (there is no compiled path), so it is behaviourally inert.
    backend_mode: Option<RevmBackendMode>,
}

impl<F: StateFetcher> ForkExecutor<F> {
    /// Wrap a fetcher in a fresh warm cache.
    pub fn new(fetcher: F) -> Self {
        Self {
            db: CacheDB::new(ForkDb::new(fetcher)),
            chain_id: 1,
            block_env: None,
            backend_mode: None,
        }
    }

    /// Override the compiled-backend policy for THIS executor (Stage 3). The validator leaves this
    /// unset (falls through to `REVM_BACKEND`); the mirror-advancer sets `Jit`/`Interpreter` from
    /// `REVMC_ADVANCER` so its background-JIT is gated independently of the validator's mode.
    pub fn set_backend_mode(&mut self, mode: RevmBackendMode) {
        self.backend_mode = Some(mode);
    }

    /// The resolved compiled-backend policy passed to every `transact_on`: the explicit override if
    /// set, else the process `REVM_BACKEND`. Always evaluated (both feature builds) so the field is
    /// never dead code; feature-off the result only ever selects the interpreter.
    #[inline]
    fn resolved_backend(&self) -> RevmBackendMode {
        self.backend_mode.unwrap_or_else(RevmBackendMode::from_env)
    }

    /// Set the chain id exposed by the `CHAINID` opcode and tx-chain validation.
    pub fn set_chain_id(&mut self, chain_id: u64) {
        self.chain_id = chain_id;
    }

    /// Set the (number, timestamp) used for deploy + [`call_ro`] EVM block env. Without this they
    /// run at number=0/timestamp=0, which makes some real contracts halt/misbehave (the
    /// in-process validator needs the real fork-block context to match forge).
    pub fn set_block_env(&mut self, number: u64, timestamp: u64) {
        self.block_env = Some((number, timestamp));
    }

    /// Current `(block_number, block_timestamp)` applied to validation calls, if set. Used to drive
    /// the per-block refresh (only re-set when the head advances) and for tests.
    pub fn block_env(&self) -> Option<(u64, u64)> {
        self.block_env
    }

    /// Force-cache an account's code/balance/nonce so later reads are cache hits (one RPC
    /// now, none on the hot replay path). Call once per resident pool at startup.
    pub fn prewarm(&mut self, address: [u8; 20]) {
        let _ = self.db.basic(Address::from(address));
    }

    /// Force-cache specific storage slots for `address` (B2). This pre-populates the CacheDB with
    /// the SAME pinned-block values REVM would otherwise fetch lazily on first access — so it can
    /// NEVER change a validation result; it only moves the RPC round-trip off the per-candidate
    /// critical path (warm cache on the first touch instead of a cold `storage_ref` fetch). Safe to
    /// call with any slots (unknown slots just warm to their on-chain value). Returns the number of
    /// slots warmed.
    pub fn prewarm_storage(&mut self, address: [u8; 20], slots: &[U256]) -> usize {
        let a = Address::from(address);
        let mut n = 0;
        for &slot in slots {
            if self.db.storage(a, slot).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Insert an ALREADY-FETCHED canonical storage value into the warm cache (the batched
    /// prewarm path: values arrive from one `eth_getStorageAt` batch instead of one lazy fetch
    /// per slot). The value MUST be the pinned-block canonical value — this is a cache fill,
    /// not an override, so it can never change a validation result. The account's basic info
    /// loads through the fetcher if absent (no fabricated empty accounts).
    pub fn preload_storage(&mut self, address: [u8; 20], slot: U256, value: U256) {
        let _ = self
            .db
            .insert_account_storage(Address::from(address), slot, value);
    }

    /// Every (address, slot) currently held in the warm storage cache — the EXACT set a
    /// `repin` is about to wipe. W4.1 (FIX_PLAN_2026-07-10): the caller snapshots this before
    /// the repin and batch-re-reads the same keys at the new pin (`preload_storage_batched`),
    /// converting the post-repin lazy cold storm (measured 2026-07-09: val_rpc_cold=195k,
    /// ~16 serial remote fetches per validation, REVM p50 1.45s) into a few batched
    /// round-trips per repin. The traffic-built warm set IS the hot read-set — no route
    /// bookkeeping needed.
    pub fn cached_storage_keys(&self) -> Vec<([u8; 20], U256)> {
        let mut keys = Vec::new();
        for (addr, acc) in self.db.cache.accounts.iter() {
            for slot in acc.storage.keys() {
                keys.push((addr.into_array(), *slot));
            }
        }
        keys
    }

    /// Advance to a fresh snapshot WITHOUT discarding warmed contract code: swap in the
    /// new-block fetcher, then drop only the cached *storage* (and reset cleared/non-
    /// existing markers) so pool reserves and token balances re-fetch fresh on next access.
    /// Bytecode (the expensive, block-invariant part) stays warm across the re-pin.
    pub fn repin(&mut self, fetcher: F) {
        self.db.db = ForkDb::new(fetcher);
        for acc in self.db.cache.accounts.values_mut() {
            acc.storage.clear();
            // A "cleared"/"not-existing" marker would make CacheDB serve ZERO without
            // re-fetching; reset to None so the next slot read falls through to the fork.
            if matches!(
                acc.account_state,
                AccountState::StorageCleared | AccountState::NotExisting
            ) {
                acc.account_state = AccountState::None;
            }
        }
    }

    /// [local-state] Invalidate the CacheDB's cached *storage* for `address` so the NEXT read
    /// re-consults the underlying fetcher. With a mirror-backed fetcher this is how the STANDING
    /// mirror's freshness reaches REVM per-candidate WITHOUT the per-candidate overlay (which
    /// conflicts with a mirror-backed fork): the CacheDB otherwise freezes each slot at its
    /// first-read value for the whole re-pin window, so a validation late in the window would price
    /// stale state. Clears only the storage map (keeps warm balance/nonce/code) and resets the
    /// cleared/not-existing marker so the next slot read falls through — same discipline as `repin`,
    /// scoped to one account. Cheap: a `HashMap::clear` on the pool's slot set.
    pub fn invalidate_account_storage(&mut self, address: [u8; 20]) {
        if let Some(acc) = self.db.cache.accounts.get_mut(&Address::from(address)) {
            acc.storage.clear();
            if matches!(
                acc.account_state,
                AccountState::StorageCleared | AccountState::NotExisting
            ) {
                acc.account_state = AccountState::None;
            }
        }
    }

    /// Number of accounts currently warm in the cache (telemetry).
    pub fn warm_accounts(&self) -> usize {
        self.db.cache.accounts.len()
    }

    /// [Stage 3] The DERIVED AOT precompile manifest: every distinct `(code_hash, runtime bytecode)`
    /// currently resident in the warm cache. After deploy + `prewarm_validator_fork` this is exactly
    /// the set of contracts the standing prewarm touches — pools, tokens, Balancer Vault, WETH, the
    /// V4 PoolManager, and the deployed MantisExecutor runtime — NOT a hand-listed set. Keying by
    /// code hash naturally dedups the hundreds of pools sharing a family's bytecode down to the
    /// ~dozen distinct programs actually worth compiling. Feed to
    /// [`crate::compiled::precompile_manifest`].
    #[cfg(feature = "revmc")]
    pub fn warm_code_hashes(&self) -> Vec<(alloy_primitives::B256, Bytes)> {
        use std::collections::HashSet;
        let mut seen: HashSet<alloy_primitives::B256> = HashSet::new();
        let mut out: Vec<(alloy_primitives::B256, Bytes)> = Vec::new();
        // Shared contracts map (revm dedups bytecode here, keyed by hash) is the primary source.
        for (hash, code) in self.db.cache.contracts.iter() {
            let bytes = code.original_bytes();
            if bytes.is_empty() {
                continue;
            }
            if seen.insert(*hash) {
                out.push((*hash, bytes));
            }
        }
        // Belt-and-suspenders: any account carrying non-empty INLINE code whose hash the contracts
        // map somehow missed (e.g. a freshly-deployed runtime not yet interned there).
        for acc in self.db.cache.accounts.values() {
            if acc.info.code_hash == revm::primitives::KECCAK_EMPTY {
                continue;
            }
            if let Some(code) = acc.info.code.as_ref() {
                let bytes = code.original_bytes();
                if !bytes.is_empty() && seen.insert(acc.info.code_hash) {
                    out.push((acc.info.code_hash, bytes));
                }
            }
        }
        out
    }

    /// True when `(address, slot)` is already in the warm storage cache — lets a learned
    /// read-set preload skip slots the cache (or the repin warm-carry) already holds, so
    /// repeat validations within a pin window never re-fetch a slot they have.
    pub fn is_storage_cached(&self, address: [u8; 20], slot: U256) -> bool {
        self.db
            .cache
            .accounts
            .get(&Address::from(address))
            .is_some_and(|acc| acc.storage.contains_key(&slot))
    }

    /// Dry-run `(from, to, value, data)` against the WARM CACHE ONLY and report every
    /// account / storage slot the execution would need that is NOT already cached —
    /// without a single fetcher (network) round-trip. Misses are served as defaults
    /// (zero / empty account), so a miss can divert the execution path; callers warm the
    /// reported misses (batched) and re-probe until the miss set is empty (it converges:
    /// each pass executes with strictly more canonical state). The shared warm cache is
    /// untouched — probe reads/writes land in a throwaway overlay. The tx outcome is
    /// irrelevant here (a probe that reverts on fabricated defaults still reports the
    /// misses it discovered before/at the divergence).
    pub fn probe_call_misses(
        &self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
    ) -> ProbeMisses {
        let probe = ProbeFetcher {
            warm: &self.db.cache,
            caller: from,
            accounts: std::cell::RefCell::new(Vec::new()),
            slots: std::cell::RefCell::new(Vec::new()),
        };
        // Throwaway overlay: probe reads/writes accumulate here and are dropped; the
        // shared warm cache is only ever READ (by reference) through `ProbeFetcher`.
        let mut db = CacheDB::new(ForkDb::new(probe));
        let _ = transact_on(
            &mut db,
            self.chain_id,
            self.block_env,
            false,
            from,
            TxKind::Call(Address::from(to)),
            value,
            data,
            gas_limit,
            // A throwaway read-set discovery dry-run: the interpreter touches the exact same slots
            // a compiled run would (byte-exactness), with no wasted compile of a probe-only path.
            RevmBackendMode::Interpreter,
        );
        let probe = db.db.into_inner();
        ProbeMisses {
            accounts: probe.accounts.into_inner(),
            slots: probe.slots.into_inner(),
        }
    }

    /// Replay `(from, to, value, data)` against the fork **without committing**. Reads warm
    /// the shared cache; writes are discarded. Returns success + the mutated addresses.
    ///
    /// `gas_price` is 0 and the nonce check is skipped so a faithful replay of a not-yet-
    /// mined feed tx doesn't revert on fee/nonce bookkeeping — the swap math (which is what
    /// determines the touched pools) is unaffected. The sender's real token balance and
    /// allowance are still read from the fork, so `transferFrom(from, …)` behaves exactly
    /// as it will on-chain. Runs under [`Self::set_block_env`] like every other call path:
    /// pools WRITE time-derived state on swap (Algebra timepoints + adaptive fee, V3 oracle
    /// observations, V2 price cumulatives), so a timestamp-0 replay would compute post-state
    /// the real chain never had.
    pub fn replay_tx(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
    ) -> ReplayOutcome {
        let revm_backend = self.resolved_backend();
        let ResultAndState { result, state } = match transact_on(
            &mut self.db,
            self.chain_id,
            self.block_env,
            false,
            from,
            TxKind::Call(Address::from(to)),
            value,
            data,
            gas_limit,
            revm_backend,
        ) {
            Ok(r) => r,
            Err(e) => {
                return ReplayOutcome {
                    success: false,
                    output: Vec::new(),
                    reason: format!("evm_err {e}"),
                    changed: Vec::new(),
                    changed_slots: Vec::new(),
                }
            }
        };
        let (changed, changed_slots) = collect_replay_changes(&state);
        // Deliberately NOT committing: shadow replay must not accumulate writes.
        ReplayOutcome {
            success: matches!(result, ExecutionResult::Success { .. }),
            output: replay_output(&result),
            reason: replay_reason(&result),
            changed,
            changed_slots,
        }
    }

    /// Replay `(from, to, value, data)` and commit successful storage writes into the warm cache.
    ///
    /// This is for ordered trigger-state handoff into validation. Keep [`replay_tx`] for shadow
    /// classification because that path must stay isolated and non-accumulating.
    pub fn replay_tx_commit(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
    ) -> ReplayOutcome {
        let revm_backend = self.resolved_backend();
        let ResultAndState { result, state } = match transact_on(
            &mut self.db,
            self.chain_id,
            self.block_env,
            false,
            from,
            TxKind::Call(Address::from(to)),
            value,
            data,
            gas_limit,
            revm_backend,
        ) {
            Ok(r) => r,
            Err(e) => {
                return ReplayOutcome {
                    success: false,
                    output: Vec::new(),
                    reason: format!("evm_err {e}"),
                    changed: Vec::new(),
                    changed_slots: Vec::new(),
                }
            }
        };
        let success = matches!(result, ExecutionResult::Success { .. });
        let (changed, changed_slots) = collect_replay_changes(&state);
        if success {
            self.db.commit(state);
        }
        ReplayOutcome {
            success,
            output: replay_output(&result),
            reason: replay_reason(&result),
            changed,
            changed_slots,
        }
    }

    /// [local-state] Commit a replay's changed slots into the warm cache **permanently** (no
    /// restore) — the storage-level "apply this feed tx" primitive that makes consecutive shadow
    /// replays COMPOSE: the next replay of the same pool starts from this one's post-state instead
    /// of the pinned snapshot (uncommitted replays each re-run against the SAME pre-state, so the
    /// mirror would end at f(pin, last_tx), silently missing every intermediate swap — corruption
    /// that scales with churn). Storage-only by design: ETH balance moves are irrelevant to pool
    /// math (native-ETH V4 pools are excluded at discovery), and the reconstruction paths need the
    /// PRE-state fork, so callers commit AFTER all pre-state work for the tx is done.
    pub fn commit_changed_slots(&mut self, changes: &[ChangedSlot]) {
        for cs in changes {
            let _ = self
                .db
                .insert_account_storage(Address::from(cs.address), cs.slot, cs.present);
        }
    }

    /// Read a storage slot from the warm cache (post-replay state is *not* retained, so this
    /// reflects the fork snapshot plus any reads warmed so far).
    pub fn storage(&mut self, address: [u8; 20], slot: U256) -> U256 {
        self.db
            .storage(Address::from(address), slot)
            .unwrap_or(U256::ZERO)
    }

    /// [fresh-overlay] Overlay a single storage slot with a fresh value, returning the PRIOR value so
    /// the caller can RESTORE it afterwards (non-accumulating — `insert_account_storage` writes only
    /// the slot, never `account_state`; the same discipline as [`Self::read_getters_over_changes`]).
    ///
    /// The hot-path REVM validator runs over a periodically-repinned snapshot fork, but a backrun's
    /// opportunity lives in the CURRENT block's post-swap state. Without freshening, the validator
    /// prices the route against pre-opportunity reserves and rejects real arbs (the measured
    /// `fl_agree=0`). This overlays the route's pools' fresh reserves before a per-candidate
    /// validation; the caller restores them after, so the shared fork is byte-identical.
    pub fn overlay_storage(&mut self, address: [u8; 20], slot: U256, value: U256) -> U256 {
        let original = self.storage(address, slot);
        let _ = self
            .db
            .insert_account_storage(Address::from(address), slot, value);
        original
    }

    /// [row-7] Read each getter's return against a pool's POST-swap state, reconstructed by
    /// overlaying a shadow replay's changed slots (`present`) onto the warm cache, running the
    /// getters (non-committing `call_ro`), then RESTORING the pre-values (`original`) — so the shared
    /// fork is byte-identical afterwards (non-accumulating; `insert_account_storage` only writes the
    /// slot, it does not touch account_state). This lets the shadow worker recover a touched pool's
    /// typed state for ANY family by running its EXISTING resync getters against the local REVM
    /// post-state and decoding with the EXISTING engine-rpc decoders — no RPC for warm slots.
    ///
    /// `changes` must be the full set of slots the replay mutated (pool + co-mutated tokens) so a
    /// getter reading a co-mutated contract also sees post-swap values. `getter_calldatas` are ABI
    /// calldata (selector [+ args]) executed against `pool`. Returns one raw return per getter
    /// (`None` if that getter reverted).
    pub fn read_getters_over_changes(
        &mut self,
        changes: &[ChangedSlot],
        pool: [u8; 20],
        getter_calldatas: &[Vec<u8>],
    ) -> Vec<Option<Vec<u8>>> {
        for cs in changes {
            let _ = self
                .db
                .insert_account_storage(Address::from(cs.address), cs.slot, cs.present);
        }
        let out: Vec<Option<Vec<u8>>> = getter_calldatas
            .iter()
            .map(|cd| {
                let (ok, bytes, _) =
                    self.call_ro([0x11; 20], pool, U256::ZERO, cd.clone(), 5_000_000);
                ok.then_some(bytes)
            })
            .collect();
        for cs in changes {
            let _ = self
                .db
                .insert_account_storage(Address::from(cs.address), cs.slot, cs.original);
        }
        out
    }

    /// [`Self::deploy`] variant that ALSO returns the deploy's full committed slot set (the
    /// constructor's complete storage effect: the contract's own slots AND every slot it wrote in
    /// OTHER contracts, e.g. token allowances). The validator captures this once at boot and
    /// RE-STAMPS it after anything that wipes cached storage (repin, per-candidate invalidation):
    /// the deployed contract exists only in this fork, so any canonical re-read fabricates zeros
    /// for its state — measured live 2026-07-09 as "Not gateway" (own slot wiped by repin) and
    /// then "STF" (allowance slots wiped by Stage-3 token invalidation). Re-stamping is local
    /// (`insert_account_storage`), zero-RPC and µs-scale — unlike a redeploy, whose constructor
    /// re-pays hundreds of cold reads (~13.8 s measured) and churns the address (nonce advances).
    pub fn deploy_and_capture(
        &mut self,
        deployer: [u8; 20],
        init_code: Vec<u8>,
        gas_limit: u64,
    ) -> Option<([u8; 20], Vec<ChangedSlot>)> {
        let (addr, changes) = self.deploy_inner(deployer, init_code, gas_limit)?;
        Some((addr, changes))
    }

    /// Deploy `init_code` (creation bytecode ++ ABI-encoded constructor args) from `deployer`
    /// and **COMMIT** the resulting contract into the warm cache so later [`replay_tx`] calls can
    /// invoke it. Returns the created contract address. Used by the hot-path REVM validator to
    /// stand up the MantisExecutor once over the fork; the per-candidate validation calls then run
    /// non-committing against it. `gas_price` is 0 and the nonce check skipped (deterministic, free).
    pub fn deploy(
        &mut self,
        deployer: [u8; 20],
        init_code: Vec<u8>,
        gas_limit: u64,
    ) -> Option<[u8; 20]> {
        self.deploy_inner(deployer, init_code, gas_limit)
            .map(|(a, _)| a)
    }

    fn deploy_inner(
        &mut self,
        deployer: [u8; 20],
        init_code: Vec<u8>,
        gas_limit: u64,
    ) -> Option<([u8; 20], Vec<ChangedSlot>)> {
        // The MantisExecutor runtime exceeds the 24 KB EIP-170 limit (forge disables it
        // too); lift the cap so the deploy doesn't fail with CreateContractSizeLimit.
        let ResultAndState { result, state } = transact_on(
            &mut self.db,
            self.chain_id,
            self.block_env,
            true,
            deployer,
            TxKind::Create,
            U256::ZERO,
            init_code,
            gas_limit,
            // One-time constructor run at boot — its init code is not the resident runtime code the
            // manifest precompiles, so never worth compiling; the interpreter deploys it.
            RevmBackendMode::Interpreter,
        )
        .ok()?;
        let addr = match &result {
            ExecutionResult::Success {
                output: Output::Create(_, Some(a)),
                ..
            } => {
                let mut x = [0u8; 20];
                x.copy_from_slice(a.as_slice());
                Some(x)
            }
            _ => None,
        };
        // Capture the constructor's full storage effect BEFORE committing (own slots + slots it
        // wrote in other contracts) — the re-stampable state for post-wipe restores.
        let changes = collect_changed_slots(&state);
        // Commit so the deployed code + constructor-set storage persist for later calls.
        self.db.commit(state);
        addr.map(|a| (a, changes))
    }

    /// Execute `(from, to, value, data)` against the fork and return `(success, return_data)`
    /// **without committing** (each call isolated, cache pinned at the fork block). Used to
    /// REVM-validate a candidate route: a successful `flashLoanArbMixed` means the route nets
    /// >= minNetProfit at the snapshot block; a `Loss`/other revert means reject.
    pub fn call_ro(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
    ) -> (bool, Vec<u8>, String) {
        let out = self.call_ro_with_changes(from, to, value, data, gas_limit);
        (out.success, out.output, out.reason)
    }

    /// Execute a read-only validation call and expose the uncommitted storage deltas.
    pub fn call_ro_with_changes(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
    ) -> CallOutcome {
        let revm_backend = self.resolved_backend();
        let res = transact_on(
            &mut self.db,
            self.chain_id,
            self.block_env,
            true,
            from,
            TxKind::Call(Address::from(to)),
            value,
            data,
            gas_limit,
            revm_backend,
        );
        call_outcome_from_result(res)
    }

    /// [Stage 2 · CacheOnly] Read-only validation call run in CACHE-ONLY mode: a storage read that
    /// misses the trusted mirror ABORTS the tx (records the `(address, slot)`) instead of blocking
    /// on a cold RPC round-trip. Returns a `pending_state` [`CallOutcome`] (`success = false`,
    /// `reason = "pending_state"`, `pending_slots` = the missing reads) — a SKIPPED validation the
    /// caller warms + retries, NEVER submits. Atomic by construction: the tx is non-committing (revm
    /// aborts before producing a committable post-state), so the shared fork is byte-identical after
    /// an abort. Cache-only is scoped to THIS call only (set before the transact, cleared after), so
    /// warming / commit / deploy / repin paths are unaffected. On a fully-warm read-set this is
    /// identical to [`call_ro_with_changes`].
    pub fn call_ro_cache_only(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
    ) -> CallOutcome {
        self.db.db.set_cache_only(true);
        let _ = self.db.db.take_pending(); // defensive: start from a clean miss list
        let revm_backend = self.resolved_backend();
        let res = transact_on(
            &mut self.db,
            self.chain_id,
            self.block_env,
            true,
            from,
            TxKind::Call(Address::from(to)),
            value,
            data,
            gas_limit,
            revm_backend,
        );
        let pending = self.db.db.take_pending();
        self.db.db.set_cache_only(false);
        if pending.is_empty() {
            // Every read was mirror-served — a real verdict, identical to the blocking path.
            call_outcome_from_result(res)
        } else {
            // Cache-only miss ⇒ abort. Never a partial/committed state (the tx errored out).
            CallOutcome {
                success: false,
                output: Vec::new(),
                reason: "pending_state".to_string(),
                changed_slots: Vec::new(),
                gas_used: 0,
                pending_slots: pending,
            }
        }
    }
}

/// Map a `transact_on` result to a [`CallOutcome`] (shared by `call_ro_with_changes` and the
/// non-aborting path of `call_ro_cache_only`). `pending_slots` is always empty here — a cache-only
/// abort is handled by the caller before this runs, so this is byte-identical to the historical
/// mapping.
fn call_outcome_from_result(res: Result<ResultAndState, String>) -> CallOutcome {
    match res {
        Ok(ResultAndState {
            result: ExecutionResult::Success { output, gas, .. },
            state,
        }) => {
            let out = match output {
                Output::Call(b) => b.to_vec(),
                Output::Create(b, _) => b.to_vec(),
            };
            CallOutcome {
                success: true,
                output: out,
                reason: String::new(),
                changed_slots: collect_changed_slots(&state),
                gas_used: gas.tx_gas_used(),
                pending_slots: Vec::new(),
            }
        }
        Ok(ResultAndState {
            result: ExecutionResult::Revert { output, gas, .. },
            state,
        }) => {
            let gas_used = gas.tx_gas_used();
            let out = output.to_vec();
            let reason = match decode_solidity_error_string(&out) {
                Some(s) => format!("revert \"{s}\" gas_used={gas_used}"),
                None => format!("revert gas_used={gas_used}"),
            };
            CallOutcome {
                success: false,
                output: out,
                reason,
                changed_slots: collect_changed_slots(&state),
                gas_used,
                pending_slots: Vec::new(),
            }
        }
        Ok(ResultAndState {
            result: ExecutionResult::Halt { reason, gas, .. },
            state,
        }) => CallOutcome {
            success: false,
            output: Vec::new(),
            reason: format!("halt {reason:?} gas_used={}", gas.tx_gas_used()),
            changed_slots: collect_changed_slots(&state),
            gas_used: gas.tx_gas_used(),
            pending_slots: Vec::new(),
        },
        Err(e) => CallOutcome {
            success: false,
            output: Vec::new(),
            reason: format!("evm_err {e}"),
            changed_slots: Vec::new(),
            gas_used: 0,
            pending_slots: Vec::new(),
        },
    }
}

impl<F: StateFetcher> crate::erc20_slots::StorageProbe for ForkExecutor<F> {
    fn token_balance_of(&mut self, token: [u8; 20], holder: [u8; 20]) -> Option<U256> {
        // balanceOf(address) selector 0x70a08231, holder left-padded to a 32-byte word.
        let mut data = Vec::with_capacity(36);
        data.extend_from_slice(&[0x70, 0xa0, 0x82, 0x31]);
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(&holder);
        data.extend_from_slice(&word);
        let (ok, out, _) = self.call_ro([0x11; 20], token, U256::ZERO, data, 200_000);
        if !ok || out.len() < 32 {
            return None;
        }
        Some(U256::from_be_slice(&out[..32]))
    }

    fn read_storage_slot(&mut self, contract: [u8; 20], slot: U256) -> U256 {
        self.storage(contract, slot)
    }

    fn warm_storage_slot(&mut self, contract: [u8; 20], slot: U256) {
        self.prewarm_storage(contract, &[slot]);
    }
}

fn collect_changed_slots(state: &EvmState) -> Vec<ChangedSlot> {
    let mut changed_slots = Vec::new();
    for (addr, acct) in state.iter() {
        let mut a = [0u8; 20];
        a.copy_from_slice(addr.as_slice());
        for (slot, s) in acct.storage.iter() {
            if s.present_value != s.original_value {
                changed_slots.push(ChangedSlot {
                    address: a,
                    slot: *slot,
                    original: s.original_value,
                    present: s.present_value,
                });
            }
        }
    }
    changed_slots
}

fn replay_output(result: &ExecutionResult) -> Vec<u8> {
    match result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Call(b) => b.to_vec(),
            Output::Create(b, _) => b.to_vec(),
        },
        ExecutionResult::Revert { output, .. } => output.to_vec(),
        ExecutionResult::Halt { .. } => Vec::new(),
    }
}

fn replay_reason(result: &ExecutionResult) -> String {
    match result {
        ExecutionResult::Success { .. } => String::new(),
        ExecutionResult::Revert { output, gas, .. } => {
            let gas_used = gas.tx_gas_used();
            match decode_solidity_error_string(output) {
                Some(s) => format!("revert \"{s}\" gas_used={gas_used}"),
                None => format!("revert gas_used={gas_used}"),
            }
        }
        ExecutionResult::Halt { reason, gas, .. } => {
            format!("halt {reason:?} gas_used={}", gas.tx_gas_used())
        }
    }
}

fn collect_replay_changes(state: &EvmState) -> (Vec<[u8; 20]>, Vec<ChangedSlot>) {
    let changed_slots = collect_changed_slots(state);
    let mut changed = Vec::new();
    for slot in &changed_slots {
        if !changed.contains(&slot.address) {
            changed.push(slot.address);
        }
    }
    (changed, changed_slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fork_db::StateFetcher;
    use ahash::AHashMap;

    /// Mock fetcher: a contract that SLOADs slot0, ADDs calldata[0..32], SSTOREs slot0.
    struct Mock {
        code: AHashMap<[u8; 20], Vec<u8>>,
        storage: AHashMap<([u8; 20], U256), U256>,
    }
    impl StateFetcher for Mock {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, self.code.get(&a).cloned())
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            self.storage.get(&(a, slot)).copied().unwrap_or(U256::ZERO)
        }
    }

    /// Cache-only mock: `try_fetch_storage` serves only `resident` slots (the mirror hit/miss
    /// shape); the blocking `fetch_storage` serves `fallback` and bumps a counter to prove the
    /// cache-only abort NEVER blocks on it.
    struct CacheOnlyMock {
        code: AHashMap<[u8; 20], Vec<u8>>,
        resident: AHashMap<([u8; 20], U256), U256>,
        fallback: AHashMap<([u8; 20], U256), U256>,
        fallback_calls: std::sync::atomic::AtomicU64,
    }
    impl StateFetcher for CacheOnlyMock {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, self.code.get(&a).cloned())
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            self.fallback_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.fallback.get(&(a, slot)).copied().unwrap_or(U256::ZERO)
        }
        fn try_fetch_storage(&self, a: [u8; 20], slot: U256) -> Option<U256> {
            self.resident.get(&(a, slot)).copied()
        }
    }

    /// RMW bytecode: SLOAD slot0 / ADD calldata[0..32] / SSTORE slot0 / STOP.
    fn rmw_code() -> Vec<u8> {
        vec![
            0x60, 0x00, 0x54, 0x60, 0x00, 0x35, 0x01, 0x60, 0x00, 0x55, 0x00,
        ]
    }

    #[test]
    fn cache_only_call_succeeds_when_read_set_is_resident() {
        // Every slot the validation touches is mirror-resident ⇒ identical to the blocking path.
        let contract = [0xEE; 20];
        let mut code = AHashMap::new();
        code.insert(contract, rmw_code());
        let mut resident = AHashMap::new();
        resident.insert((contract, U256::ZERO), U256::from(100u64));
        let mut exec = ForkExecutor::new(CacheOnlyMock {
            code,
            resident,
            fallback: AHashMap::new(),
            fallback_calls: std::sync::atomic::AtomicU64::new(0),
        });
        exec.prewarm(contract); // account (never aborts) warm in the cache
        let mut cd = vec![0u8; 32];
        cd[31] = 5;
        let out = exec.call_ro_cache_only([0x11; 20], contract, U256::ZERO, cd, 200_000);
        assert!(out.success, "resident read-set must validate: {}", out.reason);
        assert!(!out.is_pending_state());
        assert!(out.pending_slots.is_empty());
        assert_eq!(
            exec.db
                .db
                .fetcher_ref()
                .fallback_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "cache-only must never consult the blocking fallback"
        );
    }

    #[test]
    fn cache_only_call_aborts_atomically_on_cold_slot_then_fork_is_unchanged() {
        let contract = [0xEE; 20];
        let mut code = AHashMap::new();
        code.insert(contract, rmw_code());
        // slot0 is NOT resident; the blocking fallback has 100 (must NOT be used by the abort).
        let mut fallback = AHashMap::new();
        fallback.insert((contract, U256::ZERO), U256::from(100u64));
        let mut exec = ForkExecutor::new(CacheOnlyMock {
            code,
            resident: AHashMap::new(),
            fallback,
            fallback_calls: std::sync::atomic::AtomicU64::new(0),
        });
        exec.prewarm(contract);
        let mut cd = vec![0u8; 32];
        cd[31] = 5;
        let out = exec.call_ro_cache_only([0x11; 20], contract, U256::ZERO, cd.clone(), 200_000);
        // ABORT: pending_state, the cold slot recorded, the fallback never touched.
        assert!(!out.success);
        assert!(out.is_pending_state());
        assert_eq!(out.reason, "pending_state");
        assert_eq!(out.pending_slots, vec![(contract, U256::ZERO)]);
        assert!(out.changed_slots.is_empty(), "abort commits nothing");
        assert_eq!(
            exec.db
                .db
                .fetcher_ref()
                .fallback_calls
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "cache-only abort must not block on the RPC fallback"
        );
        // ATOMICITY: the cold slot was NOT warmed by the aborted read, and cache-only was cleared —
        // a subsequent BLOCKING validation reads the fallback fresh and succeeds.
        assert!(
            !exec.is_storage_cached(contract, U256::ZERO),
            "an aborted read must not warm the cold slot"
        );
        let normal = exec.call_ro_with_changes([0x11; 20], contract, U256::ZERO, cd, 200_000);
        assert!(normal.success, "blocking path recovers: {}", normal.reason);
        assert!(!normal.is_pending_state());
    }

    #[test]
    fn cache_only_abort_then_warm_then_retry_succeeds() {
        // The full CacheOnly cycle at the executor level: a cold slot aborts; warming it (exactly as
        // the async warmer's apply_warm_bundles → preload_storage would) turns the retry into a
        // CacheDB hit that validates without touching the fetcher at all. This is the laptop analogue
        // of the live abort→warm→retry-success gate.
        let contract = [0xEE; 20];
        let mut code = AHashMap::new();
        code.insert(contract, rmw_code());
        let mut exec = ForkExecutor::new(CacheOnlyMock {
            code,
            resident: AHashMap::new(), // mirror holds nothing → first read misses
            fallback: AHashMap::new(),
            fallback_calls: std::sync::atomic::AtomicU64::new(0),
        });
        exec.prewarm(contract);
        let mut cd = vec![0u8; 32];
        cd[31] = 5;
        // 1) Cold slot ⇒ abort, records the exact miss.
        let out1 = exec.call_ro_cache_only([0x11; 20], contract, U256::ZERO, cd.clone(), 200_000);
        assert!(out1.is_pending_state());
        assert_eq!(out1.pending_slots, vec![(contract, U256::ZERO)]);
        // 2) Warm the recorded miss (what the fork-warmer bundle → preload_storage does).
        for (a, s) in &out1.pending_slots {
            exec.preload_storage(*a, *s, U256::from(100u64));
        }
        // 3) Retry: the slot is now a CacheDB hit ⇒ no ForkDb miss ⇒ no abort ⇒ validates.
        let out2 = exec.call_ro_cache_only([0x11; 20], contract, U256::ZERO, cd, 200_000);
        assert!(out2.success, "retry after warm must validate: {}", out2.reason);
        assert!(!out2.is_pending_state());
        assert!(out2.pending_slots.is_empty());
    }

    #[test]
    fn stage3_wipe_destroys_pending_warm_on_retry_unless_exempted() {
        // [B2] Reproduces the PIPELINE's per-candidate cycle at the executor level. Each validation
        // first runs the Stage-3 route-account invalidation (`invalidate_account_storage`) so the
        // fork re-reads the route's pools/tokens fresh from the mirror. Under cache-only a mirror
        // miss aborts (`pending_state`) and the missed slot is async-warmed into the CacheDB
        // (`preload_storage` — exactly what the fork-warmer's PendingWarm bundle does).
        //
        // The BUG (scenario A): a retry that RE-RUNS the Stage-3 wipe clears the just-warmed
        // route-account slot BEFORE re-reading it, so the retry misses the SAME slot again and every
        // retry burns → pending_state_reject = a lost trade that never progressed past its first
        // cold slot. The FIX (scenario B): EXEMPT the route account from the wipe on the retry
        // (`stage3_invalidate_this_attempt(cache_only=true, attempt>=1) == false` in the pipeline),
        // so the warm survives and the retry validates. The mirror NEVER holds the slot here, so the
        // ONLY thing that can serve it is the warm — isolating the wipe-destroys-warm effect.
        let route_pool = [0xEE; 20]; // a route "pool" account: the Stage-3 wipe target
        let mk_exec = || {
            let mut code = AHashMap::new();
            code.insert(route_pool, rmw_code()); // SLOAD slot0 = the route-account read
            let mut exec = ForkExecutor::new(CacheOnlyMock {
                code,
                resident: AHashMap::new(), // mirror NEVER holds slot0 → only the warm can serve it
                fallback: AHashMap::new(),
                fallback_calls: std::sync::atomic::AtomicU64::new(0),
            });
            exec.prewarm(route_pool); // account warm (never aborts); storage still cold
            exec
        };
        let mut cd = vec![0u8; 32];
        cd[31] = 5;

        // ── Scenario A — the BUG: the retry re-wipes (pre-B2 "always invalidate") ──
        {
            let mut exec = mk_exec();
            exec.invalidate_account_storage(route_pool); // Stage-3 wipe (attempt 0)
            let a0 =
                exec.call_ro_cache_only([0x11; 20], route_pool, U256::ZERO, cd.clone(), 200_000);
            assert!(a0.is_pending_state(), "attempt 0 aborts on the cold route-account slot");
            assert_eq!(a0.pending_slots, vec![(route_pool, U256::ZERO)]);
            for (a, s) in &a0.pending_slots {
                exec.preload_storage(*a, *s, U256::from(100u64)); // PendingWarm → CacheDB
            }
            exec.invalidate_account_storage(route_pool); // retry RE-WIPES → destroys the warm
            let a1 =
                exec.call_ro_cache_only([0x11; 20], route_pool, U256::ZERO, cd.clone(), 200_000);
            assert!(
                a1.is_pending_state(),
                "BUG REPRO: re-wiping the route account on the retry clears the warm ⇒ aborts again"
            );
        }

        // ── Scenario B — the FIX: the retry EXEMPTS the route account (skips the wipe) ──
        {
            let mut exec = mk_exec();
            exec.invalidate_account_storage(route_pool); // Stage-3 wipe (attempt 0)
            let a0 =
                exec.call_ro_cache_only([0x11; 20], route_pool, U256::ZERO, cd.clone(), 200_000);
            assert!(a0.is_pending_state(), "attempt 0 aborts on the cold route-account slot");
            for (a, s) in &a0.pending_slots {
                exec.preload_storage(*a, *s, U256::from(100u64)); // PendingWarm → CacheDB
            }
            // Retry with NO wipe (the B2 exemption): the warm survives → the retry validates.
            let a1 = exec.call_ro_cache_only([0x11; 20], route_pool, U256::ZERO, cd, 200_000);
            assert!(
                a1.success,
                "FIX: exempting the route account on retry preserves the warm ⇒ validates: {}",
                a1.reason
            );
            assert!(!a1.is_pending_state());
            assert!(a1.pending_slots.is_empty());
        }
    }

    #[test]
    fn is_storage_cached_tracks_the_warm_cache() {
        let pool = [0xAA; 20];
        let mut exec = ForkExecutor::new(Mock {
            code: AHashMap::new(),
            storage: AHashMap::new(),
        });
        assert!(
            !exec.is_storage_cached(pool, U256::from(5u64)),
            "cold slot must report uncached"
        );
        exec.preload_storage(pool, U256::from(5u64), U256::from(0x55u64));
        assert!(
            exec.is_storage_cached(pool, U256::from(5u64)),
            "preloaded slot must report cached"
        );
        assert!(
            !exec.is_storage_cached(pool, U256::from(6u64)),
            "sibling slot stays uncached"
        );
        exec.invalidate_account_storage(pool);
        assert!(
            !exec.is_storage_cached(pool, U256::from(5u64)),
            "invalidation must clear cache membership"
        );
    }

    /// Counts how many times each (addr, slot) was fetched, and serves a configurable
    /// per-slot value, so a test can prove `repin` re-fetches storage at the new snapshot
    /// while NOT re-fetching code.
    struct CountingMock {
        code: AHashMap<[u8; 20], Vec<u8>>,
        slot_value: std::cell::Cell<U256>,
        code_fetches: std::cell::Cell<u32>,
        storage_fetches: std::cell::Cell<u32>,
    }
    impl StateFetcher for CountingMock {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            let code = self.code.get(&a).cloned();
            if code.is_some() {
                self.code_fetches.set(self.code_fetches.get() + 1);
            }
            (U256::ZERO, 0, code)
        }
        fn fetch_storage(&self, _a: [u8; 20], _slot: U256) -> U256 {
            self.storage_fetches.set(self.storage_fetches.get() + 1);
            self.slot_value.get()
        }
    }

    #[test]
    fn overlay_storage_freshens_stale_fork_slot_then_restores() {
        // ROOT-CAUSE + FIX in miniature. The fork's StateFetcher serves the pinned-snapshot (STALE)
        // value for a pool's reserves slot — exactly what the hot validator sees for a freshly-dirtied
        // backrun pool. overlay_storage writes the FRESH mirror value so a subsequent read sees fresh
        // (served from the overlay, no re-fetch), and restoring returns the fork byte-identical. This
        // is the precise mechanism that turns the validator's stale-fork fl_agree=0 into agreement.
        let pool = [0xab; 20];
        let slot = U256::from(8u8); // canonical UniV2 packed-reserves slot
        let stale = U256::from(1000u64);
        let fresh = U256::from(9999u64);
        let mut exec = ForkExecutor::new(CountingMock {
            code: AHashMap::new(),
            slot_value: std::cell::Cell::new(stale),
            code_fetches: std::cell::Cell::new(0),
            storage_fetches: std::cell::Cell::new(0),
        });
        // Cold read → the fork serves the STALE pinned-snapshot value (validator's current behaviour).
        assert_eq!(exec.storage(pool, slot), stale, "fork serves the stale snapshot value");
        // Overlay the FRESH value; the returned original is the stale one (kept for restore).
        let original = exec.overlay_storage(pool, slot, fresh);
        assert_eq!(original, stale, "overlay returns the prior (stale) value");
        // Fork now reads FRESH — this is the validator seeing post-swap reserves → agreement.
        assert_eq!(exec.storage(pool, slot), fresh, "fork reads FRESH after overlay");
        // Restore → the shared fork is byte-identical again (non-accumulating).
        let back = exec.overlay_storage(pool, slot, original);
        assert_eq!(back, fresh);
        assert_eq!(exec.storage(pool, slot), stale, "restored to the original snapshot value");
    }

    #[test]
    fn overlay_changes_what_a_validation_call_reads() {
        // The validator prices via an EVM CALL over the fork (call_ro / call_ro_with_changes), not a
        // raw slot read. Prove the fresh overlay changes what that EXECUTION reads: a contract that
        // SLOADs the reserves slot returns the STALE snapshot value before the overlay and the FRESH
        // value after — i.e. REVM now prices the backrun's post-swap state. This is the fix's
        // validation-LEVEL effect (the exact reason fl_agree flips from 0 with REVM_FRESH_OVERLAY).
        let pool = [0xcd; 20];
        let slot8 = U256::from(8u8);
        let stale = U256::from(1111u64);
        let fresh = U256::from(8888u64);
        // PUSH1 8; SLOAD; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN — returns storage slot 8.
        let mut code = AHashMap::new();
        code.insert(
            pool,
            vec![0x60, 0x08, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3],
        );
        let mut storage = AHashMap::new();
        storage.insert((pool, slot8), stale);
        let mut exec = ForkExecutor::new(Mock { code, storage });
        // Validation CALL over the stale snapshot → the execution reads STALE.
        let (ok, out, _) = exec.call_ro([0x11; 20], pool, U256::ZERO, Vec::new(), 1_000_000);
        assert!(ok, "call over the stale snapshot must succeed");
        assert_eq!(
            U256::from_be_slice(&out),
            stale,
            "the validation execution reads the STALE snapshot value"
        );
        // Overlay FRESH, re-run the CALL → the execution now reads FRESH (post-swap reserves).
        let orig = exec.overlay_storage(pool, slot8, fresh);
        let (ok2, out2, _) = exec.call_ro([0x11; 20], pool, U256::ZERO, Vec::new(), 1_000_000);
        assert!(ok2);
        assert_eq!(
            U256::from_be_slice(&out2),
            fresh,
            "the validation execution reads FRESH after the overlay"
        );
        // Restore → the shared fork is byte-identical for the next candidate.
        exec.overlay_storage(pool, slot8, orig);
        let (_, out3, _) = exec.call_ro([0x11; 20], pool, U256::ZERO, Vec::new(), 1_000_000);
        assert_eq!(U256::from_be_slice(&out3), stale, "restored to the snapshot value");
    }

    #[test]
    fn replay_changed_slots_are_complete_beyond_64() {
        // [local-state D1] The standing mirror is only correct if a replay publishes its FULL
        // changed-slot set: EvmState is a HashMap, so any truncation keeps an ARBITRARY subset —
        // a partially-applied contract post-state (slot0 updated, ticks not) that poisons the
        // mirror for exactly the heavy-write families (Algebra timepoints, V4 singleton batches,
        // aggregator multi-pool routes). A tx is gas-bounded (~1.5k cold SSTOREs at 30M), so the
        // complete set is small by construction — collect it ALL.
        let target = [0xaa; 20];
        // Unrolled: PUSH1 1, PUSH1 i, SSTORE for i in 0..100, then STOP.
        let mut code = Vec::with_capacity(100 * 5 + 1);
        for i in 0..100u8 {
            code.extend_from_slice(&[0x60, 0x01, 0x60, i, 0x55]);
        }
        code.push(0x00);
        let mut codes = AHashMap::new();
        codes.insert(target, code);
        let mut exec = ForkExecutor::new(Mock {
            code: codes,
            storage: AHashMap::new(),
        });
        let out = exec.replay_tx([0x11; 20], target, U256::ZERO, Vec::new(), 10_000_000);
        assert!(out.success, "100-slot writer must execute: {}", out.reason);
        assert_eq!(
            out.changed_slots.len(),
            100,
            "the FULL changed-slot set must be reported (truncation = arbitrary-subset mirror poison)"
        );
        assert_eq!(out.changed, vec![target]);
    }

    #[test]
    fn replay_tx_runs_with_the_configured_block_env() {
        // [local-state D3] Shadow replays previously ran with block_env=None → block.timestamp≈0.
        // Real pools WRITE time-derived state on swap (Algebra timepoints + adaptive fee, V3
        // oracle observations, V2 price cumulatives), so a timestamp-0 replay computes post-state
        // the REAL chain never had — and the mirror served it to the validator. A replay must run
        // under the SAME feed-driven block env the validator uses.
        let target = [0xbb; 20];
        // TIMESTAMP; PUSH1 0; SSTORE; STOP → stores block.timestamp into slot 0.
        let mut codes = AHashMap::new();
        codes.insert(target, vec![0x42, 0x60, 0x00, 0x55, 0x00]);
        let mk = || Mock {
            code: codes.clone(),
            storage: AHashMap::new(),
        };
        let ts = 1_700_000_999u64;
        let mut exec = ForkExecutor::new(mk());
        exec.set_block_env(123, ts);
        let out = exec.replay_tx([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);
        assert!(out.success, "{}", out.reason);
        assert_eq!(out.changed_slots.len(), 1);
        assert_eq!(
            out.changed_slots[0].present,
            U256::from(ts),
            "replay must execute at the configured block timestamp (time-dependent \
             families write wrong slots at ts=0)"
        );
    }

    #[test]
    fn commit_changed_slots_composes_sequential_replays() {
        // [local-state D2] Two consecutive feed swaps on the same pool must COMPOSE: the second
        // replay's pre-state is the first one's post-state. Without committing, both replays run
        // against the identical pinned snapshot and the mirror ends at f(pin, tx2) — the real
        // chain is at f(f(pin, tx1), tx2). A counter contract makes the divergence exact.
        let target = [0xcc; 20];
        // PUSH1 0; SLOAD; PUSH1 1; ADD; PUSH1 0; SSTORE; STOP → slot0 += 1.
        let mut codes = AHashMap::new();
        codes.insert(
            target,
            vec![0x60, 0x00, 0x54, 0x60, 0x01, 0x01, 0x60, 0x00, 0x55, 0x00],
        );
        let mut exec = ForkExecutor::new(Mock {
            code: codes,
            storage: AHashMap::new(),
        });
        let first = exec.replay_tx([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);
        assert!(first.success);
        assert_eq!(first.changed_slots.len(), 1);
        assert_eq!(first.changed_slots[0].original, U256::ZERO);
        assert_eq!(first.changed_slots[0].present, U256::from(1u64));
        // The shadow worker commits the replay's slots once all pre-state work is done…
        exec.commit_changed_slots(&first.changed_slots);
        // …so the NEXT replay composes on top of it (1 → 2), exactly like the real chain.
        let second = exec.replay_tx([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);
        assert!(second.success);
        assert_eq!(second.changed_slots.len(), 1);
        assert_eq!(
            second.changed_slots[0].original,
            U256::from(1u64),
            "second replay must start from the first one's post-state"
        );
        assert_eq!(second.changed_slots[0].present, U256::from(2u64));
    }

    #[test]
    fn repin_refetches_storage_keeps_code() {
        let pool = [0xab; 20];
        let mut code = AHashMap::new();
        // PUSH1 0 SLOAD PUSH1 0 MSTORE PUSH1 32 PUSH1 0 RETURN — returns slot0.
        code.insert(
            pool,
            vec![
                0x60, 0x00, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
            ],
        );
        let mk = || CountingMock {
            code: code.clone(),
            slot_value: std::cell::Cell::new(U256::from(100u64)),
            code_fetches: std::cell::Cell::new(0),
            storage_fetches: std::cell::Cell::new(0),
        };

        let mut exec = ForkExecutor::new(mk());
        // Warm it: a replay reads code + slot0 (=100).
        let out = exec.replay_tx([0x11; 20], pool, U256::ZERO, vec![], 1_000_000);
        assert!(out.success);
        // Re-pin to a "new block" whose fetcher reports slot0 = 250.
        let fresh = mk();
        fresh.slot_value.set(U256::from(250u64));
        exec.repin(fresh);
        // Code is still warm (repin must not have dropped it): a replay succeeds and the
        // storage value reflects the NEW snapshot (250), proving storage re-fetched.
        assert_eq!(
            exec.storage(pool, U256::ZERO),
            U256::from(250u64),
            "storage must re-fetch fresh after repin"
        );
    }

    /// W4.1 carry-forward: `cached_storage_keys` enumerates exactly the warm set, and after a
    /// repin a `preload_storage` of those keys serves the NEW-pin values with ZERO lazy fetches —
    /// the post-repin cold storm becomes one batched fill.
    #[test]
    fn cached_keys_survive_repin_via_preload_with_zero_lazy_fetches() {
        let pool = [0xab; 20];
        let mut code = AHashMap::new();
        code.insert(
            pool,
            vec![
                0x60, 0x00, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
            ],
        );
        let mk = || CountingMock {
            code: code.clone(),
            slot_value: std::cell::Cell::new(U256::from(100u64)),
            code_fetches: std::cell::Cell::new(0),
            storage_fetches: std::cell::Cell::new(0),
        };
        let mut exec = ForkExecutor::new(mk());
        // Warm slot 0 through real traffic.
        let out = exec.replay_tx([0x11; 20], pool, U256::ZERO, vec![], 1_000_000);
        assert!(out.success);
        let keys = exec.cached_storage_keys();
        assert!(
            keys.contains(&(pool, U256::ZERO)),
            "warm set must contain the traffic-read slot; got {keys:?}"
        );
        // Repin to a new snapshot whose canonical slot0 is 250, then carry the warm set
        // forward by preloading the snapshotted keys with new-pin values (what the batched
        // eth_getStorageAt fill does in production).
        let fresh = mk();
        fresh.slot_value.set(U256::from(250u64));
        exec.repin(fresh);
        for (addr, slot) in &keys {
            exec.preload_storage(*addr, *slot, U256::from(250u64));
        }
        let lazy_before = exec.db.db.fetcher_ref().storage_fetches.get();
        assert_eq!(
            exec.storage(pool, U256::ZERO),
            U256::from(250u64),
            "carried-forward key must serve the NEW pin's value"
        );
        assert_eq!(
            exec.db.db.fetcher_ref().storage_fetches.get(),
            lazy_before,
            "the read must be a cache hit — zero lazy fetches after the carry-forward fill"
        );
    }

    // B2: prewarm_storage must (a) fetch each slot exactly once, (b) preserve the on-chain value
    // (never alter it), and (c) make the subsequent read a CACHE HIT (no re-fetch) — proving it only
    // moves the fetch earlier, never changes a result.
    #[test]
    fn prewarm_storage_caches_value_and_avoids_refetch() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct CountF {
            fetches: Rc<Cell<u32>>,
            val: U256,
        }
        impl StateFetcher for CountF {
            fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
                (U256::ZERO, 0, None)
            }
            fn fetch_storage(&self, _a: [u8; 20], _slot: U256) -> U256 {
                self.fetches.set(self.fetches.get() + 1);
                self.val
            }
        }
        let fetches = Rc::new(Cell::new(0u32));
        let pool = [0x42; 20];
        let slot = U256::from(7u64);
        let mut exec = ForkExecutor::new(CountF {
            fetches: Rc::clone(&fetches),
            val: U256::from(999u64),
        });
        assert_eq!(exec.prewarm_storage(pool, &[slot]), 1, "one slot warmed");
        assert_eq!(fetches.get(), 1, "prewarm fetches the slot exactly once");
        // Subsequent read: same value, served from cache (no additional fetch).
        assert_eq!(
            exec.storage(pool, slot),
            U256::from(999u64),
            "value preserved"
        );
        assert_eq!(fetches.get(), 1, "second read is a cache hit (no re-fetch)");
    }

    #[test]
    fn replay_reports_changed_without_committing() {
        let pool = [0xee; 20];
        let mut code = AHashMap::new();
        // PUSH1 0 SLOAD / PUSH1 0 CALLDATALOAD / ADD / PUSH1 0 SSTORE / STOP
        code.insert(
            pool,
            vec![
                0x60, 0x00, 0x54, 0x60, 0x00, 0x35, 0x01, 0x60, 0x00, 0x55, 0x00,
            ],
        );
        let mut storage = AHashMap::new();
        storage.insert((pool, U256::ZERO), U256::from(1000u64));
        let mut exec = ForkExecutor::new(Mock { code, storage });

        let mut calldata = vec![0u8; 32];
        calldata[31] = 5;
        let out = exec.replay_tx([0x11; 20], pool, U256::ZERO, calldata.clone(), 1_000_000);
        assert!(out.success, "replay failed");
        assert!(
            out.changed.contains(&pool),
            "must report the pool as changed: {:?}",
            out.changed
        );

        // Not committed: a second replay sees the ORIGINAL slot0 (1000), not 1005.
        let out2 = exec.replay_tx([0x11; 20], pool, U256::ZERO, calldata, 1_000_000);
        assert!(out2.success);
        // slot0 read back is still the fork value (1000) — writes were discarded.
        assert_eq!(
            exec.storage(pool, U256::ZERO),
            U256::from(1000u64),
            "shadow must not persist writes"
        );
    }

    #[test]
    fn preload_storage_fills_cache_instead_of_fetching() {
        // Fetcher says slot 7 = 0xDEAD; the batched prewarm preloads the canonical 0xBEEF it
        // already fetched — a later read must be the cache hit (0xBEEF), not a lazy fetch.
        let pool = [0xE1u8; 20];
        let mut storage = AHashMap::new();
        storage.insert((pool, U256::from(7u64)), U256::from(0xDEADu64));
        let mut exec = ForkExecutor::new(Mock {
            code: AHashMap::new(),
            storage,
        });
        exec.preload_storage(pool, U256::from(7u64), U256::from(0xBEEFu64));
        assert_eq!(
            exec.storage(pool, U256::from(7u64)),
            U256::from(0xBEEFu64),
            "preloaded value must be served from the warm cache (no lazy fetch)"
        );
    }

    #[test]
    fn read_getters_over_changes_sees_post_state_then_restores() {
        // A getter that returns storage[8]: PUSH1 8 SLOAD PUSH1 0 MSTORE PUSH1 32 PUSH1 0 RETURN.
        let pool = [0xd0u8; 20];
        let mut code = AHashMap::new();
        code.insert(
            pool,
            vec![
                0x60, 0x08, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
            ],
        );
        let mut storage = AHashMap::new();
        storage.insert((pool, U256::from(8u64)), U256::from(1000u64));
        let mut exec = ForkExecutor::new(Mock { code, storage });

        // The shadow replay found slot 8 moved 1000 -> 2500. Run the getter against the POST-state.
        let changes = vec![ChangedSlot {
            address: pool,
            slot: U256::from(8u64),
            original: U256::from(1000u64),
            present: U256::from(2500u64),
        }];
        let out = exec.read_getters_over_changes(&changes, pool, &[Vec::new()]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].as_ref().map(|b| U256::from_be_slice(b)),
            Some(U256::from(2500u64)),
            "getter must see the OVERLAID post-swap value"
        );
        // And the warm cache MUST be restored to the pre-swap value (non-accumulating).
        assert_eq!(
            exec.storage(pool, U256::from(8u64)),
            U256::from(1000u64),
            "read_getters_over_changes must restore the pre-swap slot"
        );
    }

    #[test]
    fn replay_tx_commit_persists_storage_for_later_validation() {
        let pool = [0xdd; 20];
        let mut code = AHashMap::new();
        // PUSH1 0 SLOAD / PUSH1 0 CALLDATALOAD / ADD / PUSH1 0 SSTORE / STOP
        code.insert(
            pool,
            vec![
                0x60, 0x00, 0x54, 0x60, 0x00, 0x35, 0x01, 0x60, 0x00, 0x55, 0x00,
            ],
        );
        let mut storage = AHashMap::new();
        storage.insert((pool, U256::ZERO), U256::from(1000u64));
        let mut exec = ForkExecutor::new(Mock { code, storage });

        let mut calldata = vec![0u8; 32];
        calldata[31] = 5;
        let out = exec.replay_tx_commit([0x11; 20], pool, U256::ZERO, calldata, 1_000_000);

        assert!(out.success, "committing replay failed");
        assert_eq!(out.changed_slots.len(), 1);
        assert_eq!(out.changed_slots[0].address, pool);
        assert_eq!(out.changed_slots[0].slot, U256::ZERO);
        assert_eq!(out.changed_slots[0].original, U256::from(1000u64));
        assert_eq!(out.changed_slots[0].present, U256::from(1005u64));
        assert_eq!(exec.storage(pool, U256::ZERO), U256::from(1005u64));
    }

    #[test]
    fn replay_tx_commit_reports_revert_reason() {
        let target = [0xcd; 20];
        let mut code = AHashMap::new();
        // PUSH1 0 / PUSH1 0 / REVERT
        code.insert(target, vec![0x60, 0x00, 0x60, 0x00, 0xfd]);
        let storage = AHashMap::new();
        let mut exec = ForkExecutor::new(Mock { code, storage });

        let out = exec.replay_tx_commit([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);

        assert!(!out.success);
        assert!(
            out.reason.starts_with("revert gas_used="),
            "unexpected replay failure reason: {}",
            out.reason
        );
    }

    #[test]
    fn replay_tx_commit_uses_configured_chain_id() {
        let target = [0xce; 20];
        let mut code = AHashMap::new();
        // CHAINID / PUSH1 0 / MSTORE / PUSH1 32 / PUSH1 0 / RETURN
        code.insert(
            target,
            vec![0x46, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3],
        );
        let storage = AHashMap::new();
        let mut exec = ForkExecutor::new(Mock { code, storage });
        exec.set_chain_id(42_161);

        let out = exec.replay_tx_commit([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);

        assert!(out.success, "replay failed: {}", out.reason);
        assert_eq!(U256::from_be_slice(&out.output), U256::from(42_161u64));
    }

    #[test]
    fn block_env_getter_and_per_block_refresh_reflected_in_timestamp_opcode() {
        let target = [0xcf; 20];
        let mut code = AHashMap::new();
        // TIMESTAMP / PUSH1 0 / MSTORE / PUSH1 32 / PUSH1 0 / RETURN — returns block.timestamp.
        code.insert(
            target,
            vec![0x42, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3],
        );
        let storage = AHashMap::new();
        let mut exec = ForkExecutor::new(Mock { code, storage });

        assert_eq!(exec.block_env(), None, "unset by default");
        exec.set_block_env(100, 1_700_000_000);
        assert_eq!(exec.block_env(), Some((100, 1_700_000_000)));
        let out = exec.call_ro([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);
        assert!(out.0, "call failed");
        assert_eq!(U256::from_be_slice(&out.1), U256::from(1_700_000_000u64));

        // A3 per-block refresh: bumping the block env makes the SAME contract see the new time —
        // proving the validator can be kept current without a rebuild (zero RPC at call time).
        exec.set_block_env(101, 1_700_000_250);
        assert_eq!(exec.block_env(), Some((101, 1_700_000_250)));
        let out2 = exec.call_ro([0x11; 20], target, U256::ZERO, Vec::new(), 1_000_000);
        assert!(out2.0);
        assert_eq!(U256::from_be_slice(&out2.1), U256::from(1_700_000_250u64));
    }

    #[test]
    fn decodes_solidity_error_string_revert_payload() {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x08, 0xc3, 0x79, 0xa0]);
        out.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
        out.extend_from_slice(&U256::from(11u64).to_be_bytes::<32>());
        let mut word = [0u8; 32];
        word[..11].copy_from_slice(b"Not gateway");
        out.extend_from_slice(&word);

        assert_eq!(
            decode_solidity_error_string(&out),
            Some("Not gateway".to_string())
        );
    }

    #[test]
    fn storage_probe_balance_and_prewarm_over_real_evm() {
        use crate::erc20_slots::{
            erc20_balance_slot, prewarm_token_balance_slots, StorageProbe, TokenHolders,
            COMMON_BALANCE_BASE_SLOTS,
        };
        // A minimal real ERC-20 `balanceOf`: reads the holder word from calldata[4..36], computes
        // the mapping slot keccak256(holder ++ baseSlot=2) at runtime (SHA3), SLOADs it, returns it.
        // 60 04 35 60 00 52  60 02 60 20 52  60 40 60 00 20  54  60 00 52 60 20 60 00 f3
        let token = [0x7eu8; 20];
        let mut code = AHashMap::new();
        code.insert(
            token,
            vec![
                0x60, 0x04, 0x35, 0x60, 0x00, 0x52, 0x60, 0x02, 0x60, 0x20, 0x52, 0x60, 0x40, 0x60,
                0x00, 0x20, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
            ],
        );
        let holder = [0x55u8; 20];
        let bal = U256::from(12_345u64);
        let true_slot = erc20_balance_slot(holder, U256::from(2u64));
        let mut storage = AHashMap::new();
        storage.insert((token, true_slot), bal);
        let mut exec = ForkExecutor::new(Mock { code, storage });

        // 1) The real EVM balanceOf returns the value stored at the derived slot.
        assert_eq!(exec.token_balance_of(token, holder), Some(bal));

        // 2) The orchestration discovers base slot 2 and warms the holder's balance slot.
        let audit = prewarm_token_balance_slots(
            &mut exec,
            &[TokenHolders {
                token,
                holders: vec![holder],
            }],
            COMMON_BALANCE_BASE_SLOTS,
        );
        assert_eq!(audit.tokens_resolved, 1);
        assert_eq!(audit.balance_slots_warmed, 1);
        // 3) The warmed slot reads back the correct on-chain value (warm, no behavior change).
        assert_eq!(exec.read_storage_slot(token, true_slot), bal);
    }

    #[test]
    fn call_ro_with_changes_reports_storage_delta_without_committing() {
        let target = [0xcc; 20];
        let mut code = AHashMap::new();
        // PUSH1 0 SLOAD / PUSH1 0 CALLDATALOAD / ADD / PUSH1 0 SSTORE / STOP
        code.insert(
            target,
            vec![
                0x60, 0x00, 0x54, 0x60, 0x00, 0x35, 0x01, 0x60, 0x00, 0x55, 0x00,
            ],
        );
        let mut storage = AHashMap::new();
        storage.insert((target, U256::ZERO), U256::from(7u64));
        let mut exec = ForkExecutor::new(Mock { code, storage });
        let mut calldata = vec![0u8; 32];
        calldata[31] = 5;

        let out = exec.call_ro_with_changes([0x11; 20], target, U256::ZERO, calldata, 1_000_000);

        assert!(out.success);
        assert_eq!(out.changed_slots.len(), 1);
        assert_eq!(out.changed_slots[0].address, target);
        assert_eq!(out.changed_slots[0].slot, U256::ZERO);
        assert_eq!(out.changed_slots[0].original, U256::from(7u64));
        assert_eq!(out.changed_slots[0].present, U256::from(12u64));
        assert_eq!(exec.storage(target, U256::ZERO), U256::from(7u64));
    }

    /// Shared-count fetcher for the probe tests: serves fixed code/storage and counts
    /// round-trips through externally-held `Rc` handles (the fetcher itself moves into
    /// the executor, so `Cell` fields would become unreadable).
    struct RcCountingFetcher {
        code: AHashMap<[u8; 20], Vec<u8>>,
        storage: AHashMap<([u8; 20], U256), U256>,
        account_fetches: std::rc::Rc<std::cell::Cell<u32>>,
        storage_fetches: std::rc::Rc<std::cell::Cell<u32>>,
    }
    impl StateFetcher for RcCountingFetcher {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            self.account_fetches.set(self.account_fetches.get() + 1);
            (U256::ZERO, 0, self.code.get(&a).cloned())
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            self.storage_fetches.set(self.storage_fetches.get() + 1);
            self.storage.get(&(a, slot)).copied().unwrap_or(U256::ZERO)
        }
    }

    #[test]
    fn invalidate_account_storage_forces_reread_of_a_frozen_slot() {
        // [local-state] The CacheDB freezes a slot at its first-read value for the whole re-pin
        // window (a second read is a cache hit — no fetcher round-trip). invalidate_account_storage
        // clears that account's cached storage so the NEXT read re-consults the fetcher — the
        // mechanism that lets the STANDING mirror-backed fetcher freshen REVM per-candidate WITHOUT
        // the per-candidate overlay: invalidate the route's pools, then each validation re-reads the
        // current mirror value instead of a value frozen at first touch. Scoped to one account.
        let pool = [0xC1u8; 20];
        let slot = U256::from(8u64);
        let stor = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let mut storage_map = AHashMap::new();
        storage_map.insert((pool, slot), U256::from(100u64));
        let mut exec = ForkExecutor::new(RcCountingFetcher {
            code: AHashMap::new(),
            storage: storage_map,
            account_fetches: std::rc::Rc::new(std::cell::Cell::new(0u32)),
            storage_fetches: stor.clone(),
        });
        assert_eq!(exec.storage(pool, slot), U256::from(100u64));
        assert_eq!(stor.get(), 1, "first read fetches");
        // Frozen: a second read is a CacheDB hit — no re-fetch.
        let _ = exec.storage(pool, slot);
        assert_eq!(stor.get(), 1, "second read is frozen (CacheDB hit, no re-fetch)");
        // Invalidate → the next read re-consults the fetcher (where the fresh mirror value is served).
        exec.invalidate_account_storage(pool);
        let _ = exec.storage(pool, slot);
        assert_eq!(stor.get(), 2, "invalidation forces a re-fetch (mirror freshness reaches REVM)");
        // A different, un-invalidated account stays frozen — invalidation is per-account.
        let other = [0xC2u8; 20];
        let _ = exec.storage(other, slot); // cold fetch (#3)
        let _ = exec.storage(other, slot); // cache hit
        assert_eq!(stor.get(), 3, "other account still frozen — invalidation is per-account");
    }

    #[test]
    fn probe_reports_slot_misses_without_any_fetcher_round_trip() {
        // Pool bytecode: SLOAD(5); POP; SLOAD(7); POP; STOP — touches exactly slots 5 and 7.
        let pool = [0xB0u8; 20];
        let code = vec![0x60, 0x05, 0x54, 0x50, 0x60, 0x07, 0x54, 0x50, 0x00];
        let acct = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let stor = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let mut code_map = AHashMap::new();
        code_map.insert(pool, code);
        let mut storage_map = AHashMap::new();
        storage_map.insert((pool, U256::from(5u64)), U256::from(0x55u64));
        storage_map.insert((pool, U256::from(7u64)), U256::from(0x77u64));
        let fetcher = RcCountingFetcher {
            code: code_map,
            storage: storage_map,
            account_fetches: acct.clone(),
            storage_fetches: stor.clone(),
        };
        let mut exec = ForkExecutor::new(fetcher);
        exec.prewarm(pool); // pool code+account warm (1 account fetch)
        assert_eq!(acct.get(), 1);

        let eoa = [0xEAu8; 20];
        let misses = exec.probe_call_misses(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);

        // The probe must be NETWORK-SILENT: no account/storage fetcher calls at all.
        assert_eq!(
            (acct.get(), stor.get()),
            (1, 0),
            "probe must not reach the fetcher (accounts={}, storage={})",
            acct.get(),
            stor.get()
        );
        // ...and must report exactly the two uncached slots the execution needs.
        let mut slots = misses.slots.clone();
        slots.sort();
        assert_eq!(
            slots,
            vec![(pool, U256::from(5u64)), (pool, U256::from(7u64))],
            "probe must discover the SLOAD(5)/SLOAD(7) read-set"
        );
        // The uncached caller account is a miss too (tx setup reads it).
        assert!(
            misses.accounts.contains(&eoa),
            "uncached caller must be reported as an account miss"
        );
        // The shared warm cache must be untouched by the probe (still just the pool).
        assert_eq!(
            exec.warm_accounts(),
            1,
            "probe must not mutate the warm cache"
        );

        // Warm every reported miss, then the REAL replay must be fully warm: the only
        // fetcher traffic is the warming itself (1 account + 2 slots), none from the replay.
        for a in &misses.accounts {
            exec.prewarm(*a);
        }
        for (a, s) in &misses.slots {
            exec.prewarm_storage(*a, &[*s]);
        }
        let (acct_after_warm, stor_after_warm) = (acct.get(), stor.get());
        let out = exec.replay_tx_commit(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);
        assert!(out.success, "warm replay must succeed: {}", out.reason);
        assert_eq!(
            (acct.get(), stor.get()),
            (acct_after_warm, stor_after_warm),
            "replay after warming the probe's miss set must be ZERO-fetch"
        );
    }

    // GOAL 2026-07-05 (revm_ns p99 tail): a value-carrying replay tx (value>0) from a caller
    // whose balance is not in the probe overlay (served as 0) must NOT halt the probe on
    // "insufficient funds" before the swap read-set is discovered. The REAL replay runs against
    // the sender's actual chain balance and executes the swap, so a balance-halted probe
    // under-reports the read-set → the replay then cold-fetches lazily → the 1.68s p99 tail
    // (prefetch_unconverged). Disabling the balance check in the probe makes it explore the
    // same path the real replay takes.
    #[test]
    fn probe_explores_value_carrying_call_despite_zero_caller_balance() {
        let pool = [0xC5u8; 20];
        let code = vec![0x60, 0x03, 0x54, 0x50, 0x00]; // SLOAD(3); POP; STOP
        let mut code_map = AHashMap::new();
        code_map.insert(pool, code);
        let mut storage_map = AHashMap::new();
        storage_map.insert((pool, U256::from(3u64)), U256::from(0x33u64));
        let fetcher = RcCountingFetcher {
            code: code_map,
            storage: storage_map,
            account_fetches: std::rc::Rc::new(std::cell::Cell::new(0)),
            storage_fetches: std::rc::Rc::new(std::cell::Cell::new(0)),
        };
        let mut exec = ForkExecutor::new(fetcher);
        exec.prewarm(pool);
        let eoa = [0xE5u8; 20];
        // value = 1 wei, caller balance in the overlay = 0.
        let misses = exec.probe_call_misses(eoa, pool, U256::from(1u64), Vec::new(), 1_000_000);
        assert!(
            misses.slots.contains(&(pool, U256::from(3u64))),
            "probe must reach SLOAD(3) even on a value-carrying call from a 0-balance caller: {:?}",
            misses.slots
        );
    }

    #[test]
    fn probe_converges_when_a_missing_slot_diverts_the_path() {
        // Branchy bytecode: v = SLOAD(1); if v != 0 jump -> SLOAD(2).
        // 0x6001 54 6007 57 00 5b 6002 54 50 00
        let pool = [0xB1u8; 20];
        let code = vec![
            0x60, 0x01, 0x54, 0x60, 0x07, 0x57, 0x00, 0x5b, 0x60, 0x02, 0x54, 0x50, 0x00,
        ];
        let acct = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let stor = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let mut code_map = AHashMap::new();
        code_map.insert(pool, code);
        let mut storage_map = AHashMap::new();
        storage_map.insert((pool, U256::from(1u64)), U256::from(1u64)); // branch WILL be taken
        storage_map.insert((pool, U256::from(2u64)), U256::from(42u64));
        let fetcher = RcCountingFetcher {
            code: code_map,
            storage: storage_map,
            account_fetches: acct.clone(),
            storage_fetches: stor.clone(),
        };
        let eoa = [0xEBu8; 20];
        let mut exec = ForkExecutor::new(fetcher);
        exec.prewarm(pool);
        exec.prewarm(eoa);

        // Pass 1: cache has no slots — probe sees SLOAD(1)=0 (default), branch NOT taken,
        // so only slot 1 is discovered.
        let m1 = exec.probe_call_misses(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);
        let mut s1 = m1.slots.clone();
        s1.sort();
        assert_eq!(
            s1,
            vec![(pool, U256::from(1u64))],
            "pass 1 discovers slot 1 only"
        );

        // Warm pass-1 misses with CANONICAL values (accounts too — REVM touches the
        // beneficiary account for gas bookkeeping, and production warms every reported
        // miss), re-probe: now the branch IS taken and the deeper slot 2 surfaces. This
        // is the convergence loop callers run.
        for a in &m1.accounts {
            exec.prewarm(*a);
        }
        for (a, s) in &m1.slots {
            exec.prewarm_storage(*a, &[*s]);
        }
        let m2 = exec.probe_call_misses(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);
        let mut s2 = m2.slots.clone();
        s2.sort();
        assert_eq!(
            s2,
            vec![(pool, U256::from(2u64))],
            "pass 2 discovers slot 2"
        );

        for a in &m2.accounts {
            exec.prewarm(*a);
        }
        for (a, s) in &m2.slots {
            exec.prewarm_storage(*a, &[*s]);
        }
        let m3 = exec.probe_call_misses(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);
        assert!(m3.is_empty(), "pass 3 must be fully warm: {m3:?}");
    }

    /// revmc byte-exactness (feature `revmc`) — THE Stage-3 correctness contract: the SAME contract
    /// executed through (1) the plain interpreter, (2) a background-JIT backend forced resident, and
    /// (3) a blocking-AOT backend MUST agree on accept/reject AND gas AND output. A compiled answer
    /// that diverges on any of the three would mis-validate a route, so this is a HARD assertion.
    /// Two bytecodes bracket a real validation: a compute contract (revmc's best case) and a
    /// SSTORE+SLOAD contract (the storage-bridge / realistic case). Both JIT and AOT run the
    /// COMPILED function (not interpreter fallback): AOT via `blocking:true`, JIT via an explicit
    /// `compile_jit_sync` that makes the program resident before the dispatch.
    #[cfg(feature = "revmc")]
    #[test]
    fn jit_matches_interpreter_byte_exact() {
        use alloy_primitives::keccak256;
        use revmc::runtime::{JitBackend, LookupRequest, RuntimeCacheKey, RuntimeConfig, RuntimeTuning};

        // (10*3)+7 = 37 → MSTORE → RETURN 32 (MUL/ADD/MSTORE/RETURN).
        let compute: Vec<u8> = vec![
            0x60, 0x0a, 0x60, 0x03, 0x02, 0x60, 0x07, 0x01, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60,
            0x00, 0xf3,
        ];
        // SSTORE(0)=7 → SLOAD(0)=7 → MSTORE → RETURN 32 (exercises the SSTORE/SLOAD gas + storage
        // bridge every AMM swap hits — the case where a compiled-gas bug would actually bite).
        let storage: Vec<u8> = vec![
            0x60, 0x07, 0x60, 0x00, 0x55, // PUSH1 7; PUSH1 0; SSTORE
            0x60, 0x00, 0x54, // PUSH1 0; SLOAD
            0x60, 0x00, 0x52, // PUSH1 0; MSTORE
            0x60, 0x20, 0x60, 0x00, 0xf3, // PUSH1 32; PUSH1 0; RETURN
        ];
        let contract = [0xcc; 20];

        struct CodeMock {
            addr: [u8; 20],
            code: Vec<u8>,
        }
        impl StateFetcher for CodeMock {
            fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
                if a == self.addr {
                    (U256::ZERO, 0, Some(self.code.clone()))
                } else {
                    (U256::ZERO, 0, None)
                }
            }
            fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
                U256::ZERO
            }
        }

        #[derive(Clone, Copy)]
        enum Backend {
            Interpreter,
            Jit,
            Aot,
        }

        // (success, gas_used, output) — the full accept/reject + gas + bytes tuple the contract
        // asserts identical across all three backends.
        let run = |code: &[u8], which: Backend| -> (bool, u64, Vec<u8>) {
            let build_evm = || {
                let db = CacheDB::new(ForkDb::new(CodeMock { addr: contract, code: code.to_vec() }));
                Context::mainnet()
                    .with_db(db)
                    .modify_cfg_chained(|cfg| {
                        cfg.chain_id = 42161;
                        cfg.disable_nonce_check = true;
                        cfg.tx_gas_limit_cap = Some(u64::MAX);
                    })
                    .build_mainnet()
            };
            let build_tx = || {
                TxEnv::builder()
                    .caller(Address::from([0xaa; 20]))
                    .kind(TxKind::Call(Address::from(contract)))
                    .value(U256::ZERO)
                    .data(Bytes::new())
                    .gas_limit(1_000_000)
                    .gas_price(0)
                    .chain_id(None)
                    .build()
                    .expect("tx")
            };
            let blocking_backend = || {
                JitBackend::new(RuntimeConfig {
                    blocking: true,
                    tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
                    ..Default::default()
                })
                .expect("jit backend")
            };
            let result = match which {
                Backend::Interpreter => {
                    let mut evm = build_evm();
                    evm.transact(build_tx()).expect("interp transact")
                }
                Backend::Aot => {
                    // blocking:true ⇒ the first lookup synchronously compiles ⇒ runs compiled.
                    let mut jit = revmc::revm_evm::JitEvm::new(build_evm(), blocking_backend());
                    jit.transact(build_tx()).expect("aot transact")
                }
                Backend::Jit => {
                    // Non-blocking backend, but force the program resident FIRST so the JitEvm
                    // dispatch path returns the (background-)compiled function deterministically.
                    let backend = JitBackend::new(RuntimeConfig {
                        blocking: false,
                        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
                        ..Default::default()
                    })
                    .expect("jit backend");
                    let spec = crate::compiled::precompile_spec_id();
                    backend
                        .compile_jit_sync(LookupRequest {
                            key: RuntimeCacheKey { code_hash: keccak256(code), spec_id: spec },
                            code: Bytes::from(code.to_vec()),
                        })
                        .expect("force jit compile");
                    assert!(
                        backend.get_compiled(keccak256(code), spec).is_some(),
                        "jit program must be resident before dispatch"
                    );
                    let mut jit = revmc::revm_evm::JitEvm::new(build_evm(), backend);
                    jit.transact(build_tx()).expect("jit transact")
                }
            };
            // `tx_gas_used` (post EIP-8037 state-gas split) — the same accessor for all three
            // backends, so any compiled-vs-interpreter gas divergence is caught.
            let gas = result.result.tx_gas_used();
            match result.result {
                ExecutionResult::Success { output: Output::Call(b), .. } => (true, gas, b.to_vec()),
                other => (false, gas, format!("{other:?}").into_bytes()),
            }
        };

        for (label, code, want_last) in
            [("compute", &compute, 37u8), ("storage", &storage, 7u8)]
        {
            let i = run(code, Backend::Interpreter);
            let j = run(code, Backend::Jit);
            let a = run(code, Backend::Aot);
            assert!(i.0, "{label}: interpreter must succeed");
            assert_eq!(i, j, "{label}: JIT must match interpreter on (success,gas,output)");
            assert_eq!(i, a, "{label}: AOT must match interpreter on (success,gas,output)");
            // Baseline sanity: right-aligned 32-byte word ending in the expected value.
            assert_eq!(i.2.len(), 32);
            assert_eq!(i.2.last(), Some(&want_last), "{label}: unexpected interpreter output");
            assert!(i.2[..31].iter().all(|&b| b == 0));
            assert!(i.1 > 0, "{label}: gas must be nonzero");
        }
    }
}
