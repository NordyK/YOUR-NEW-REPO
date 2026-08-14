//! Fork database for REVM: a `DatabaseRef` that lazily pulls account + storage from a
//! pluggable [`StateFetcher`] on a cache miss. Wrapped in `CacheDB<ForkDb<F>>`, REVM
//! reads exactly the state a tx touches (no hand-coded per-pool storage layout), the
//! cache absorbs repeats, and writes from `transact_commit` stay local.
//!
//! The `StateFetcher` boundary keeps this crate decoupled: tests use a mock; the engine
//! plugs in an RPC-backed fetcher (pinned to a snapshot block). This is the substrate
//! for the §13 EventReady-(b) parity proof — fork at block `N-1`, re-execute a real
//! swap, compare REVM's pool state to canonical at block `N`.

use std::cell::RefCell;

use revm::database_interface::DBErrorMarker;
use revm::primitives::{Address, Bytes, B256, KECCAK_EMPTY, U256};
use revm::state::{AccountInfo, Bytecode};
use revm::DatabaseRef;

/// [Stage 5 · WP4 · Fix E] A value that flowed through the CHECKED canonical-fetch seam
/// ([`StateFetcher::fetch_storage_checked`] → the reconcile stager). The mirror-commit path for a
/// checked reconcile demands `Proven<_>` at the TYPE level, so a raw value obtained from the legacy
/// infallible [`StateFetcher::fetch_storage`] (which fabricates a default on error) can NOT be applied
/// to the mirror through the checked path — fail-open is a compile error there, not a convention.
///
/// Only the checked stager constructs `Proven` (via [`Proven::checked`]); it is deliberately a thin
/// wrapper (no runtime cost) whose value is the type discipline it imposes on its consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proven<T>(T);

impl<T> Proven<T> {
    /// Wrap a value the caller has obtained through the checked fetch seam. Call this ONLY from the
    /// checked stager (the sole place that has just consulted `fetch_storage_checked`); never wrap a
    /// value read from the infallible `fetch_storage`.
    pub fn checked(value: T) -> Self {
        Proven(value)
    }

    /// Borrow the proven value (e.g. to compare against the mirror before committing).
    pub fn value(&self) -> &T {
        &self.0
    }

    /// Consume into the proven value (the mirror-commit boundary).
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: Copy> Proven<T> {
    /// Copy out the proven value (convenience for `Copy` payloads like `U256`).
    pub fn get(&self) -> T {
        self.0
    }
}

/// [Stage 5 · WP0 · A1] The reason a *checked* canonical storage read could not be resolved to a
/// value that is provably anchored to the canonical block. Kinds are the machine-classifiable causes;
/// telemetry may aggregate by kind, but control flow must branch on the [`Result`], never on a value.
///
/// The ordering is only for `PartialOrd`/dashboards — no kind is "more severe" for control flow;
/// any error means no value is available to a trust decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalFetchErrorKind {
    /// The canonical block header could not be obtained (transport error, timeout, or a null result).
    HeaderUnavailable,
    /// A header/root was obtained but its canonical identity is not independently trusted: the
    /// trusted-hash check failed, two independent sources disagreed, the returned block number/hash
    /// did not match the pinned block, a reorg occurred during resolution, or no independent anchor
    /// source was available. Never proceed to trust a value under this kind.
    UntrustedAnchor,
    /// The EIP-1186 proof could not be obtained (transport error, timeout, or a null/missing result).
    ProofUnavailable,
    /// The proof response was structurally malformed or violated an input bound before verification
    /// (bad JSON shape, wrong requested-key cardinality, non-canonical encoding, trailing data).
    MalformedProof,
    /// The account proof did not verify against the anchored state root (bad hash chain, tampered
    /// node/path, or claimed account fields that disagree with the proven leaf).
    InvalidAccountProof,
    /// The storage proof did not verify against the proven account storage root (bad hash chain,
    /// tampered node/path/value, wrong storage root, or an empty proof under a non-empty root).
    InvalidStorageProof,
    /// A retained differential (non-trust) read disagreed with the proven value. Informational only —
    /// the proven value is authoritative and this kind never *supplies* a value.
    ValueMismatch,
    /// A network/JSON input exceeded a configured bound (body bytes, node count, hex length, decoded
    /// bytes) and was rejected before large allocation or hex decode.
    InputLimitExceeded,
}

impl CanonicalFetchErrorKind {
    /// Stable snake_case label for metrics/logs (never changes across kinds).
    pub fn label(&self) -> &'static str {
        match self {
            CanonicalFetchErrorKind::HeaderUnavailable => "header_unavailable",
            CanonicalFetchErrorKind::UntrustedAnchor => "untrusted_anchor",
            CanonicalFetchErrorKind::ProofUnavailable => "proof_unavailable",
            CanonicalFetchErrorKind::MalformedProof => "malformed_proof",
            CanonicalFetchErrorKind::InvalidAccountProof => "invalid_account_proof",
            CanonicalFetchErrorKind::InvalidStorageProof => "invalid_storage_proof",
            CanonicalFetchErrorKind::ValueMismatch => "value_mismatch",
            CanonicalFetchErrorKind::InputLimitExceeded => "input_limit_exceeded",
        }
    }
}

/// [Stage 5 · WP0 · A1] A typed failure of a *checked* canonical storage read. It carries the reason
/// WITHOUT a usable value: the whole point of [`StateFetcher::fetch_storage_checked`] is that an
/// `Err` can never be silently coerced into a zero, an old value, or an unproven proof value. The
/// `block`/`address`/`slot` locate the read; `reason` is a human string for logs (not parsed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalFetchError {
    /// Machine-classifiable cause (telemetry aggregates by this).
    pub kind: CanonicalFetchErrorKind,
    /// The pinned canonical block the read was anchored to.
    pub block: u64,
    /// The account the storage read was for.
    pub address: [u8; 20],
    /// The storage slot (raw key, pre-hash) the read was for.
    pub slot: U256,
    /// Human-readable detail for logs (never used for control flow).
    pub reason: String,
}

impl CanonicalFetchError {
    /// Construct a typed checked-fetch error.
    pub fn new(
        kind: CanonicalFetchErrorKind,
        block: u64,
        address: [u8; 20],
        slot: U256,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            block,
            address,
            slot,
            reason: reason.into(),
        }
    }

    /// The classified cause (telemetry key).
    pub fn kind(&self) -> CanonicalFetchErrorKind {
        self.kind
    }
}

impl core::fmt::Display for CanonicalFetchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "canonical fetch {} at block {} slot {}: {}",
            self.kind.label(),
            self.block,
            self.slot,
            self.reason
        )
    }
}

impl std::error::Error for CanonicalFetchError {}

/// Pluggable source of canonical state for the fork DB. Implementations fetch at a fixed
/// block (pinned at construction) and may cache internally; `ForkDb` + `CacheDB` also
/// cache, so a simple pass-through is fine. Misses return defaults (never error).
pub trait StateFetcher {
    /// `(balance, nonce, code)` for an account; `code = None` for an EOA / empty account.
    fn fetch_account(&self, address: [u8; 20]) -> (U256, u64, Option<Vec<u8>>);
    /// 32-byte storage value at `slot` (zero if unset).
    fn fetch_storage(&self, address: [u8; 20], slot: U256) -> U256;

    /// [Stage 2 · CacheOnly] Non-blocking account read: `Some(..)` when the value can be
    /// served WITHOUT a cold/blocking round-trip, `None` when serving it would require one.
    /// The default serves the blocking [`fetch_account`](Self::fetch_account) (so existing
    /// fetchers keep today's behaviour and NEVER abort). A mirror-backed fetcher overrides
    /// this. NOTE: [`ForkDb`] never aborts on an account miss — accounts are boot-prewarmed +
    /// block-invariant and the async warmer only refetches storage; account reads fall back.
    fn try_fetch_account(&self, address: [u8; 20]) -> Option<(U256, u64, Option<Vec<u8>>)> {
        Some(self.fetch_account(address))
    }

    /// [Stage 2 · CacheOnly] Non-blocking storage read: `Some(v)` when `v` can be served
    /// WITHOUT a cold/blocking round-trip (e.g. a trusted-mirror hit), `None` when serving it
    /// would require one — the signal [`ForkDb`] turns into a `pending_state` abort under
    /// cache-only mode. The default serves the blocking [`fetch_storage`](Self::fetch_storage),
    /// so existing fetchers compile unchanged and never abort.
    fn try_fetch_storage(&self, address: [u8; 20], slot: U256) -> Option<U256> {
        Some(self.fetch_storage(address, slot))
    }

    /// [Stage 5 · WP0 · A2] Checked canonical storage read: `Ok(v)` only when `v` is a canonical
    /// value the fetcher can stand behind, `Err(CanonicalFetchError)` when it cannot — and an error
    /// NEVER carries a value. This is the seam a fail-closed reconcile (WP1) consumes so that an
    /// unavailable/invalid proof or an untrusted anchor stops a value from entering the mirror,
    /// instead of the historical infallible [`fetch_storage`](Self::fetch_storage) that returns a
    /// fabricated default on error.
    ///
    /// The default is the *feature-off compatibility wrapper*: it returns `Ok(self.fetch_storage(..))`,
    /// preserving every ordinary fetcher's exact value semantics (this is the `MIRROR_COLD_VERIFY=0`
    /// path — no proof work, no new failure modes). A proof-verifying fetcher OVERRIDES this to return
    /// the value proven against an independently anchored state root, or a typed error. Mirrors the
    /// `try_fetch_*` default so no existing implementor changes.
    fn fetch_storage_checked(
        &self,
        address: [u8; 20],
        slot: U256,
    ) -> Result<U256, CanonicalFetchError> {
        Ok(self.fetch_storage(address, slot))
    }
}

/// [Stage 2] Error type for [`ForkDb`]'s `DatabaseRef`. Replaces the former `Infallible` so a
/// cache-only miss can ABORT the in-flight transaction instead of blocking on a cold fetch: revm
/// surfaces this as `EVMError::Database(..)` from `transact`, which [`crate::ForkExecutor`] maps to
/// a non-committing `pending_state` outcome (a SKIPPED validation — the candidate is warmed + retried,
/// never submitted on partial state). The only variant today is [`ForkDbError::PendingState`]; it is
/// `is_fatal` (the default) so the tx aborts exactly like any DB error would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkDbError {
    /// A cache-only read missed the trusted mirror; the touched `(address, slot)` was recorded on
    /// the [`ForkDb`] miss list for the async warmer, and execution must abort (never serve a
    /// fabricated default into a capital-path validation).
    PendingState,
}

impl core::fmt::Display for ForkDbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ForkDbError::PendingState => f.write_str("pending_state (cache-only mirror miss)"),
        }
    }
}

impl std::error::Error for ForkDbError {}
impl DBErrorMarker for ForkDbError {}

/// REVM `DatabaseRef` backed by a [`StateFetcher`]. Use as `CacheDB::new(ForkDb::new(f))`.
///
/// [Stage 2] Optionally runs in CACHE-ONLY mode (`set_cache_only(true)`): a storage read that the
/// fetcher cannot serve without a cold round-trip ([`StateFetcher::try_fetch_storage`] → `None`) is
/// recorded on `pending` and aborts the transaction with [`ForkDbError::PendingState`] instead of
/// blocking. Default (cache-only OFF) is byte-identical to the historical `Infallible` fork DB: every
/// read serves the blocking fetcher and never errors. The mode is toggled per validation call by
/// [`crate::ForkExecutor`] and is a no-op for account/code/blockhash reads (those never abort).
pub struct ForkDb<F: StateFetcher> {
    fetcher: F,
    /// When true, storage misses abort (record + `PendingState`) instead of blocking-fetching.
    cache_only: bool,
    /// `(address, slot)` storage reads that aborted under cache-only mode this call — drained by
    /// the executor to drive the async warmer. `RefCell` because `DatabaseRef` reads take `&self`;
    /// the fork DB is owned single-threaded by its executor (same discipline as `ProbeFetcher`).
    pending: RefCell<Vec<([u8; 20], U256)>>,
}

impl<F: StateFetcher> ForkDb<F> {
    /// Wrap a fetcher (cache-only OFF — byte-identical to the historical infallible fork DB).
    pub fn new(fetcher: F) -> Self {
        Self {
            fetcher,
            cache_only: false,
            pending: RefCell::new(Vec::new()),
        }
    }

    /// Unwrap the fetcher (the probe path recovers its recorded misses this way).
    pub fn into_inner(self) -> F {
        self.fetcher
    }

    /// Borrow the fetcher (tests assert on its fetch counters; production reads its
    /// round-trip counter without unwrapping).
    pub fn fetcher_ref(&self) -> &F {
        &self.fetcher
    }

    /// [Stage 2] Enter/leave cache-only mode. The executor sets this true only around a single
    /// validation transact and clears it after, so warming/commit/deploy paths never abort.
    pub fn set_cache_only(&mut self, cache_only: bool) {
        self.cache_only = cache_only;
    }

    /// Whether cache-only mode is active.
    pub fn is_cache_only(&self) -> bool {
        self.cache_only
    }

    /// Drain the recorded cache-only storage misses since the last drain (leaves the list empty).
    pub fn take_pending(&self) -> Vec<([u8; 20], U256)> {
        self.pending.take()
    }
}

impl<F: StateFetcher> DatabaseRef for ForkDb<F> {
    type Error = ForkDbError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, ForkDbError> {
        // Accounts NEVER abort under cache-only: they are boot-prewarmed + block-invariant (code)
        // and the async warmer only refetches storage, so a rare cold account falls back to the
        // blocking fetcher (which, for a mirror-backed fetcher, still hits the mirror first). A cold
        // remote fetch here is caught by the validator's existing `remote_cold_on_submit` fail-closed
        // gate, so this never weakens capital safety.
        let (balance, nonce, code) = self.fetcher.fetch_account(address.into_array());
        let bytecode = code
            .filter(|c| !c.is_empty())
            .map(|c| Bytecode::new_raw(Bytes::from(c)));
        let code_hash = bytecode
            .as_ref()
            .map(|b| b.hash_slow())
            .unwrap_or(KECCAK_EMPTY);
        Ok(Some(AccountInfo {
            balance,
            nonce,
            code_hash,
            code: bytecode,
            ..Default::default()
        }))
    }

    fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, ForkDbError> {
        // CacheDB resolves code from the AccountInfo returned by basic_ref, so this is
        // not exercised on the normal path. Return empty rather than panic.
        Ok(Bytecode::default())
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, ForkDbError> {
        let addr = address.into_array();
        if self.cache_only {
            // Cache-only: serve only what the fetcher can give WITHOUT a cold round-trip. A miss is
            // recorded (for the async warmer) and aborts the tx — never a fabricated default into a
            // capital-path validation. On the retry the warmed slot is a CacheDB hit (this ref is
            // not even reached), so convergence is one warm RTT per missing slot.
            match self.fetcher.try_fetch_storage(addr, index) {
                Some(v) => Ok(v),
                None => {
                    self.pending.borrow_mut().push((addr, index));
                    Err(ForkDbError::PendingState)
                }
            }
        } else {
            Ok(self.fetcher.fetch_storage(addr, index))
        }
    }

    fn block_hash_ref(&self, _number: u64) -> Result<B256, ForkDbError> {
        // Swap paths don't depend on BLOCKHASH; return zero.
        Ok(B256::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahash::AHashMap;
    use revm::context::result::{ExecutionResult, Output, ResultAndState};
    use revm::context::{Context, TxEnv};
    use revm::database::CacheDB;
    use revm::primitives::TxKind;
    use revm::{Database, DatabaseCommit, ExecuteEvm, MainBuilder, MainContext};

    /// Mock fetcher: a fixed map of account code + storage. Records nothing fancy —
    /// just proves REVM pulls state through the fetcher lazily (no pre-load) and that
    /// the wrapping CacheDB commits writes locally.
    struct MockFetcher {
        code: AHashMap<[u8; 20], Vec<u8>>,
        storage: AHashMap<([u8; 20], U256), U256>,
        balance: AHashMap<[u8; 20], U256>,
    }
    impl StateFetcher for MockFetcher {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (
                self.balance.get(&a).copied().unwrap_or(U256::ZERO),
                0,
                self.code.get(&a).cloned(),
            )
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            self.storage.get(&(a, slot)).copied().unwrap_or(U256::ZERO)
        }
    }

    /// Bytecode: SLOAD slot0, ADD calldata[0], SSTORE slot0 — read-modify-write.
    /// 0x6000546000350160005500
    #[test]
    fn revm_executes_against_lazily_fetched_state_then_commits() {
        let contract = [0xee; 20];
        let sender = [0x11; 20];
        let mut code = AHashMap::new();
        code.insert(
            contract,
            vec![
                0x60, 0x00, 0x54, 0x60, 0x00, 0x35, 0x01, 0x60, 0x00, 0x55, 0x00,
            ],
        );
        let mut storage = AHashMap::new();
        // The pool's "current reserve" lives ONLY in the fetcher (not pre-loaded) — REVM
        // must lazily pull it.
        storage.insert((contract, U256::ZERO), U256::from(1000u64));
        let mut balance = AHashMap::new();
        balance.insert(sender, U256::from(10u64).pow(U256::from(18)));
        let fetcher = MockFetcher {
            code,
            storage,
            balance,
        };

        let mut db = CacheDB::new(ForkDb::new(fetcher));

        // calldata delta = 7
        let mut calldata = vec![0u8; 32];
        calldata[31] = 7;
        let ResultAndState { result, state } = {
            let mut evm = Context::mainnet()
                .with_db(&mut db)
                .modify_cfg_chained(|cfg| {
                    cfg.disable_nonce_check = true;
                    cfg.tx_gas_limit_cap = Some(u64::MAX); // EIP-7825 cap off (revm-19 parity)
                })
                .build_mainnet();
            let tx = TxEnv::builder()
                .caller(Address::from(sender))
                .kind(TxKind::Call(Address::from(contract)))
                .data(Bytes::from(calldata))
                .gas_limit(200_000)
                .gas_price(1u128)
                .build()
                .expect("tx env");
            evm.transact(tx).expect("transact")
        };
        assert!(
            matches!(result, ExecutionResult::Success { .. }),
            "exec: {result:?}"
        );
        let _ = match result {
            ExecutionResult::Success { output, .. } => match output {
                Output::Call(b) => b,
                Output::Create(b, _) => b,
            },
            _ => Bytes::new(),
        };
        db.commit(state);
        // 1000 (lazily fetched) + 7 (computed) = 1007 (committed to the cache)
        let v = db.storage(Address::from(contract), U256::ZERO).unwrap();
        assert_eq!(
            v,
            U256::from(1007u64),
            "REVM must read fetched state, compute, commit"
        );
    }

    // ── Stage 2 · CacheOnly abort plumbing ────────────────────────────────────────────────────

    /// A fetcher with a set of RESIDENT storage slots. Under cache-only, `try_fetch_storage`
    /// returns `Some` only for resident slots and `None` (→ abort) otherwise — the mirror-hit /
    /// mirror-miss shape. `fetch_storage` (blocking) always serves a value (the fallback).
    struct ResidentMock {
        resident: AHashMap<([u8; 20], U256), U256>,
        /// Value the blocking fallback serves for any slot (proves cache-only does NOT use it).
        fallback_val: U256,
        fallback_calls: std::cell::Cell<u64>,
    }
    impl StateFetcher for ResidentMock {
        fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, None)
        }
        fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
            self.fallback_calls.set(self.fallback_calls.get() + 1);
            self.fallback_val
        }
        fn try_fetch_storage(&self, a: [u8; 20], s: U256) -> Option<U256> {
            self.resident.get(&(a, s)).copied()
        }
    }

    #[test]
    fn try_fetch_defaults_wrap_blocking_fetch() {
        // A fetcher that does NOT override try_fetch_* gets Some(blocking) — never aborts.
        struct Plain;
        impl StateFetcher for Plain {
            fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
                (U256::from(9u64), 1, None)
            }
            fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
                U256::from(7u64)
            }
        }
        let p = Plain;
        assert_eq!(p.try_fetch_storage([0; 20], U256::ZERO), Some(U256::from(7u64)));
        assert_eq!(
            p.try_fetch_account([0; 20]),
            Some((U256::from(9u64), 1, None))
        );
    }

    #[test]
    fn fetch_storage_checked_default_wraps_blocking_fetch_as_ok() {
        // [WP0 · A2] The defaulted checked hook is the feature-off compatibility wrapper: a fetcher
        // that does NOT override it returns Ok(blocking value) — identical value semantics, no new
        // error path. WP1 consumes this seam; an infallible fetcher can never yield an Err here.
        struct Plain;
        impl StateFetcher for Plain {
            fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
                (U256::ZERO, 0, None)
            }
            fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
                U256::from(77u64)
            }
        }
        let p = Plain;
        assert_eq!(
            p.fetch_storage_checked([0xAB; 20], U256::from(3u64)),
            Ok(U256::from(77u64)),
            "default checked hook returns the blocking value as Ok (feature-off identity)"
        );
    }

    #[test]
    fn canonical_fetch_error_carries_no_value_and_labels_kinds() {
        // [WP0 · A1] The typed error preserves the reason + location without any value channel, and
        // every kind has a stable metrics label. Control flow branches on the Result; telemetry keys
        // on the kind/label.
        let e = CanonicalFetchError::new(
            CanonicalFetchErrorKind::UntrustedAnchor,
            123,
            [0x11; 20],
            U256::from(9u64),
            "sources disagree on state root",
        );
        assert_eq!(e.kind(), CanonicalFetchErrorKind::UntrustedAnchor);
        assert_eq!(e.kind().label(), "untrusted_anchor");
        assert_eq!(e.block, 123);
        assert!(e.to_string().contains("untrusted_anchor"));
        // All kinds have distinct, non-empty labels.
        let kinds = [
            CanonicalFetchErrorKind::HeaderUnavailable,
            CanonicalFetchErrorKind::UntrustedAnchor,
            CanonicalFetchErrorKind::ProofUnavailable,
            CanonicalFetchErrorKind::MalformedProof,
            CanonicalFetchErrorKind::InvalidAccountProof,
            CanonicalFetchErrorKind::InvalidStorageProof,
            CanonicalFetchErrorKind::ValueMismatch,
            CanonicalFetchErrorKind::InputLimitExceeded,
        ];
        let labels: std::collections::HashSet<&str> = kinds.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), kinds.len(), "each kind has a distinct label");
    }

    #[test]
    fn cache_only_off_is_byte_identical_never_errors_or_records() {
        // Flag-off identity at the DB level: storage_ref serves the blocking fetcher and the miss
        // list stays empty, exactly like the historical Infallible fork DB.
        let mock = ResidentMock {
            resident: AHashMap::new(),
            fallback_val: U256::from(42u64),
            fallback_calls: std::cell::Cell::new(0),
        };
        let db = ForkDb::new(mock);
        assert!(!db.is_cache_only());
        let v = db.storage_ref(Address::from([0xAB; 20]), U256::from(5u64)).unwrap();
        assert_eq!(v, U256::from(42u64), "off mode serves the blocking fetcher");
        assert!(db.take_pending().is_empty(), "off mode records no misses");
        assert_eq!(db.fetcher_ref().fallback_calls.get(), 1);
    }

    #[test]
    fn cache_only_storage_miss_records_and_aborts_without_fallback() {
        let mut resident = AHashMap::new();
        let pool = [0xEE; 20];
        resident.insert((pool, U256::from(0u64)), U256::from(111u64)); // slot 0 resident
        let mock = ResidentMock {
            resident,
            fallback_val: U256::from(999u64),
            fallback_calls: std::cell::Cell::new(0),
        };
        let mut db = ForkDb::new(mock);
        db.set_cache_only(true);
        // Resident slot → hit, no abort, no fallback.
        assert_eq!(
            db.storage_ref(Address::from(pool), U256::from(0u64)).unwrap(),
            U256::from(111u64)
        );
        // Missing slot → PendingState abort, recorded, and the blocking fallback is NEVER consulted.
        assert_eq!(
            db.storage_ref(Address::from(pool), U256::from(1u64)),
            Err(ForkDbError::PendingState)
        );
        assert_eq!(
            db.fetcher_ref().fallback_calls.get(),
            0,
            "cache-only must never touch the blocking (RPC) fallback"
        );
        let pending = db.take_pending();
        assert_eq!(pending, vec![(pool, U256::from(1u64))]);
        assert!(db.take_pending().is_empty(), "take drains the miss list");
    }

    #[test]
    fn cache_only_accounts_never_abort() {
        // basic_ref must serve (via fallback) even under cache-only — accounts are prewarmed +
        // block-invariant and the warmer can't refetch them, so aborting would strand routes.
        let mock = ResidentMock {
            resident: AHashMap::new(),
            fallback_val: U256::ZERO,
            fallback_calls: std::cell::Cell::new(0),
        };
        let mut db = ForkDb::new(mock);
        db.set_cache_only(true);
        assert!(
            db.basic_ref(Address::from([0x01; 20])).is_ok(),
            "account reads never abort under cache-only"
        );
        assert!(db.take_pending().is_empty());
    }
}
