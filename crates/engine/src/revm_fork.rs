//! RPC-backed [`StateFetcher`] for the REVM fork DB (§13 EventReady-(b)).
//!
//! Pins to a snapshot block and lazily serves account/storage to `ForkDb` via the
//! engine's sync RPC client. With `CacheDB<ForkDb<RpcStateFetcher>>`, REVM reads exactly
//! the live state a swap touches — no hand-coded per-pool storage layout — which is the
//! substrate for the parity proof (re-execute a real swap, compare REVM's pool state to
//! canonical).

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::{B256, U256};
use engine_revm::{CanonicalFetchError, CanonicalFetchErrorKind, StateFetcher};
use engine_rpc::{ArbitrumRpcClient, BlockTag, RpcError};

use crate::mpt_verifier::{self, AccountProof, StorageProof};
use crate::receipt_sampler::ReceiptStatus;

/// Process-global count of synchronous RPC round-trips the REVM fork fetcher has made. The
/// validator is single-threaded with one fetcher, so this is the count of Alchemy calls served
/// on the validation path. `revm_loop` samples the delta around each candidate validation to
/// ENFORCE the "Alchemy off the hot path" invariant: any non-zero delta under `live_submit` means
/// a cold fork-miss blocked the submit decision on an RPC round-trip — surfaced as an ERROR + a
/// breach signal rather than silently paying ~30-80ms per cold candidate. (audit §5 / §10.1.)
pub static RPC_FETCHES: AtomicU64 = AtomicU64::new(0);

/// Read the cumulative validation-path RPC fetch count.
#[inline]
pub fn rpc_fetch_count() -> u64 {
    RPC_FETCHES.load(Ordering::Relaxed)
}

/// Process-global count of FAILED RPC fetches (balance/code/nonce/storage) on the validation path.
/// A failed storage read is otherwise indistinguishable from a real zero slot, so the fetcher still
/// returns a default to avoid panicking the EVM — but it bumps this counter. `revm_loop` samples the
/// delta around each candidate so a candidate whose validation touched fabricated (errored) state is
/// REJECTED fail-closed (`fetch_error_fail_closed`) instead of being trusted for capital. (Codex #1.)
pub static RPC_FETCH_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Read the cumulative validation-path RPC fetch-error count.
#[inline]
pub fn rpc_fetch_error_count() -> u64 {
    RPC_FETCH_ERRORS.load(Ordering::Relaxed)
}

/// True if `url` points at a co-located / local node, where a fork cache-miss costs ~µs (loopback +
/// local NVMe) rather than the ~30–80 ms of a remote provider (TAIL_LATENCY_PLAN §7 F). When local,
/// a `VALIDATION_RPC_COLD` event is NOT a hot-path violation, so the live-safe alarm is downgraded.
pub fn rpc_endpoint_is_local(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    if u.starts_with("unix:") || u.starts_with('/') {
        return true;
    }
    // Strip scheme to inspect the host.
    let after_scheme = u.split("://").nth(1).unwrap_or(&u);
    // IPv6 literal host is bracketed (`[::1]:port`) — handle before splitting on ':'.
    if after_scheme.starts_with("[::1]") {
        return true;
    }
    let host = after_scheme
        .split(['/', ':'])
        .next()
        .unwrap_or(after_scheme);
    host == "localhost"
        || host == "127.0.0.1"
        || host == "0.0.0.0"
        || host == "::1"
        || host.ends_with(".local")
}

/// [WP1 · ADV-F1 + FIX-4a] The normalized `host[:port]` of an RPC URL — scheme, userinfo, path, query,
/// and any path/query-embedded API key stripped, lowercased, a trailing FQDN dot removed, and the
/// scheme's DEFAULT port canonicalized away (`:443` for https/wss, `:80` for http/ws). Two URLs that
/// differ only by scheme, case, trailing slash, embedded key, an explicit default port, or a trailing
/// dot normalize to the SAME host.
fn normalized_rpc_host(url: &str) -> String {
    let u = url.trim().to_ascii_lowercase();
    let (scheme, after_scheme) = match u.split_once("://") {
        Some((s, rest)) => (s, rest),
        None => ("", u.as_str()),
    };
    // The authority ends at the first path/query/fragment delimiter; strip any `user:pass@` userinfo.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // Split host / port, keeping a bracketed IPv6 literal intact.
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        match rest.split_once("]:") {
            Some((h, p)) => (format!("[{h}]"), Some(p)),
            None => (host_port.to_string(), None),
        }
    } else {
        match host_port.rsplit_once(':') {
            // Only treat a trailing all-digit segment as a port (a hostname has none).
            Some((h, p)) if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) => {
                (h.to_string(), Some(p))
            }
            _ => (host_port.to_string(), None),
        }
    };
    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    let default_port = match scheme {
        "https" | "wss" => "443",
        "http" | "ws" => "80",
        _ => "",
    };
    match port {
        Some(p) if p == default_port => host,
        Some(p) => format!("{host}:{p}"),
        None => host,
    }
}

/// [WP1 · ADV-F1] Whether two RPC URLs target the SAME provider `host[:port]`. A cross-check between
/// two endpoints that share a host provides NO independence (it is a vacuous self-crosscheck), so the
/// anchor policy treats them as one trust domain. Empty/malformed archive hosts are NOT treated as
/// "same" (so the checked path still fails closed via a failed anchor fetch rather than silently
/// suppressing the cross-check).
///
/// LIMITATION (honest): this detects only DISTINCT-HOSTNAME independence. Two different hostnames that
/// resolve to the same provider (DNS aliases / CNAME / shared upstream) are NOT detected as the same
/// trust domain — distinct host is the guarantee, no more. True independence relies on the operator
/// configuring genuinely independent providers (the RPC policy: lava base vs Alchemy/QuickNode).
fn same_rpc_host(a: &str, b: &str) -> bool {
    let na = normalized_rpc_host(a);
    !na.is_empty() && na == normalized_rpc_host(b)
}

/// Bulk-load `(contract, slot)` storage values into `exec`'s warm cache with batched
/// `eth_getStorageAt` (one HTTP round-trip per `BATCH` slots instead of one per slot). Values
/// are the pinned-block canonical reads — a pure cache fill that can never change a validation
/// result. Per-item RPC errors are SKIPPED (that slot stays lazy-fetch, correct just not warm).
/// Returns `(loaded, errors)`.
pub fn preload_storage_batched<F: engine_revm::StateFetcher>(
    exec: &mut engine_revm::ForkExecutor<F>,
    client: &ArbitrumRpcClient,
    block: BlockTag,
    reads: &[([u8; 20], U256)],
) -> (usize, usize) {
    /// Slots per JSON-RPC batch — well under provider batch caps, large enough that a full
    /// universe prewarm is a few dozen round-trips instead of tens of thousands.
    const PRELOAD_BATCH: usize = 400;
    let mut loaded = 0usize;
    let mut errors = 0usize;
    for chunk in reads.chunks(PRELOAD_BATCH) {
        let req: Vec<([u8; 20], [u8; 32])> =
            chunk.iter().map(|(a, s)| (*a, s.to_be_bytes())).collect();
        match client.storage_batch(&req, block) {
            Ok(items) => {
                for ((addr, slot), item) in chunk.iter().zip(items) {
                    match item {
                        Ok(word) => {
                            exec.preload_storage(*addr, *slot, U256::from_be_bytes(word));
                            loaded += 1;
                        }
                        Err(_) => errors += 1,
                    }
                }
            }
            Err(_) => errors += chunk.len(),
        }
    }
    (loaded, errors)
}

/// W4.1 (FIX_PLAN_2026-07-10): carry the fork's warm storage set across a repin. The caller
/// snapshots `exec.cached_storage_keys()` BEFORE `repin()` (which wipes storage) and hands the
/// keys here AFTER — the same keys are re-read at the NEW pin in batched `eth_getStorageAt`
/// round-trips (≤400/RTT). The traffic-built warm set IS the hot read-set, so this removes the
/// post-repin lazy cold storm at its root (2026-07-09 soak: val_rpc_cold=195,578 ≈ 16 serial
/// remote fetches per validation, REVM p50 1.45s — every one a repin-wiped slot re-faulted
/// one-by-one). MUST run BEFORE `restamp_mantis_state`: the in-fork executor's own storage and
/// its constructor's token-allowance slots are LOCAL fictions whose canonical chain values are
/// empty — the re-stamp rewrites them last, so a canonical 0 loaded here can never survive.
/// `cap` bounds the refresh (largest warm sets first-come; the tail stays lazy — correct, just
/// not warm). Returns `(loaded, errors)`.
pub fn carry_warm_set_across_repin<F: engine_revm::StateFetcher>(
    exec: &mut engine_revm::ForkExecutor<F>,
    client: &ArbitrumRpcClient,
    new_block: u64,
    warm_keys: &[([u8; 20], U256)],
    cap: usize,
) -> (usize, usize) {
    let take = warm_keys.len().min(cap);
    preload_storage_batched(
        exec,
        client,
        BlockTag::Number(new_block),
        &warm_keys[..take],
    )
}

/// Outcome of a [`warm_call_prefetch`] pass (heartbeat telemetry): how many probe
/// iterations ran, what got batch-loaded, and whether the read-set fully converged
/// (an unconverged call falls back to lazy per-slot fetches for the remainder —
/// bounded, and surfaced by the cold counters).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrefetchStats {
    /// Probe→load iterations executed (0 = already fully warm).
    pub iters: u32,
    /// Accounts (code/balance/nonce) loaded, one `account_bundle` round-trip each.
    pub accounts_loaded: usize,
    /// Storage slots loaded via batched `eth_getStorageAt` (≤400 per round-trip).
    pub slots_loaded: usize,
    /// True when the final probe found everything warm (the real call will be zero-fetch).
    pub converged: bool,
    /// Deduped union of every `(address, slot)` the probe discovered cold across all
    /// iterations — the call's learned read-set, replayable as ONE batched preload on
    /// the next validation of the same route (P1 registry input).
    pub discovered_slots: Vec<([u8; 20], U256)>,
    /// Deduped accounts the probe discovered cold (code/balance/nonce warms).
    pub discovered_accounts: Vec<[u8; 20]>,
    /// Slots batch-loaded from a LEARNED read-set before probing (P1 registry hit);
    /// 0 when the route was unknown or everything was already cached.
    pub learned_preloaded: usize,
}

impl PrefetchStats {
    /// Sort + dedup the discovered read-set (call before returning to callers).
    fn dedup_discovered(mut self) -> Self {
        self.discovered_slots.sort_unstable();
        self.discovered_slots.dedup();
        self.discovered_accounts.sort_unstable();
        self.discovered_accounts.dedup();
        self
    }
}

/// Warm the EXACT read-set of `(from, to, value, data)` before executing it for real:
/// dry-run the call against the warm cache only ([`ForkExecutor::probe_call_misses`] —
/// zero network), batch-load every reported miss (one `eth_getStorageAt` batch per ≤400
/// slots + one `account_bundle` per account), and repeat until the probe converges or
/// `max_iters` is hit. Family-agnostic: the EVM itself discovers the slots, so this works
/// identically for DODO PMM state, LB bins, Curve, Balancer, V4 hooks — no hand-coded
/// storage layouts. Converts N serialized ~22ms lazy fetches during a replay/validation
/// into 1–2 batched round-trips before it; a converged call then executes with ZERO
/// Alchemy round-trips on the hot path.
pub fn warm_call_prefetch<F: engine_revm::StateFetcher>(
    exec: &mut engine_revm::ForkExecutor<F>,
    client: &ArbitrumRpcClient,
    block: BlockTag,
    from: [u8; 20],
    to: [u8; 20],
    value: U256,
    data: &[u8],
    gas_limit: u64,
    max_iters: u32,
) -> PrefetchStats {
    let mut stats = PrefetchStats::default();
    for _ in 0..max_iters {
        let misses = exec.probe_call_misses(from, to, value, data.to_vec(), gas_limit);
        if misses.is_empty() {
            stats.converged = true;
            return stats.dedup_discovered();
        }
        stats.iters += 1;
        // Accounts first (code decides which slots the next probe pass can even reach).
        // Each is ONE batched balance+code+nonce round-trip through the fetcher.
        for a in &misses.accounts {
            exec.prewarm(*a);
            stats.accounts_loaded += 1;
        }
        stats.discovered_accounts.extend_from_slice(&misses.accounts);
        stats.discovered_slots.extend_from_slice(&misses.slots);
        let (loaded, errors) = preload_storage_batched(exec, client, block, &misses.slots);
        stats.slots_loaded += loaded;
        // Batch-errored slots stay lazy-fetch; if a pass made NO progress at all, stop
        // rather than spin on a slot the provider keeps failing.
        if misses.accounts.is_empty() && loaded == 0 && errors > 0 {
            return stats.dedup_discovered();
        }
    }
    // Out of iterations: one final probe decides whether we actually converged on the
    // last load (common case: the last pass loaded the final misses).
    stats.converged = exec
        .probe_call_misses(from, to, value, data.to_vec(), gas_limit)
        .is_empty();
    stats.dedup_discovered()
}

/// Learned-read-set variant of [`warm_call_prefetch`] (P1): preload the route's
/// REMEMBERED read-set in one batched round-trip (skipping slots the cache already
/// holds), then run the probe→load loop — which converges immediately on repeat
/// validations instead of spending up to `max_iters` blind layers on data-dependent
/// slot addressing (DODO PMM, LB bins, Balancer vault, Fluid). The union of learned ∪
/// newly-discovered is recorded back so the set tracks route drift (e.g. an LB
/// active-id move discovering new bins). Values are ALWAYS fetched from RPC at `block`
/// — addresses are learned, state never is.
#[allow(clippy::too_many_arguments)]
pub fn preload_learned_then_prefetch<F: engine_revm::StateFetcher>(
    exec: &mut engine_revm::ForkExecutor<F>,
    client: &ArbitrumRpcClient,
    block: BlockTag,
    readsets: &mut crate::route_readsets::RouteReadSets,
    route: engine_state::RouteId,
    from: [u8; 20],
    to: [u8; 20],
    value: U256,
    data: &[u8],
    gas_limit: u64,
    max_iters: u32,
) -> PrefetchStats {
    let mut learned_preloaded = 0usize;
    if let Some(rs) = readsets.get(route) {
        // Accounts: `prewarm` is a cache hit when already warm (no RPC), so no filter.
        let accounts: Vec<[u8; 20]> = rs.accounts.clone();
        let to_load: Vec<([u8; 20], U256)> = rs
            .slots
            .iter()
            .filter(|(a, s)| !exec.is_storage_cached(*a, *s))
            .copied()
            .collect();
        for a in accounts {
            exec.prewarm(a);
        }
        if !to_load.is_empty() {
            let (loaded, _errors) = preload_storage_batched(exec, client, block, &to_load);
            learned_preloaded = loaded;
        }
    }
    let mut stats = warm_call_prefetch(
        exec, client, block, from, to, value, data, gas_limit, max_iters,
    );
    stats.learned_preloaded = learned_preloaded;
    let pin = match block {
        BlockTag::Number(n) => n,
        _ => 0,
    };
    if !stats.discovered_slots.is_empty()
        || !stats.discovered_accounts.is_empty()
        || readsets.get(route).is_none()
    {
        readsets.record(route, pin, &stats.discovered_slots, &stats.discovered_accounts);
    }
    stats
}

/// [WP3 · Fix D] PER-INSTANCE RPC counters — round-trips AND errors — owned by an [`RpcStateFetcher`].
/// The process-global [`RPC_FETCHES`]/[`RPC_FETCH_ERRORS`] are shared by EVERY fetcher (validator +
/// advancer + shadow worker + startup + audits), so sampling the global delta around a candidate
/// attributes OTHER threads' fetches/errors to that candidate — the cross-thread contamination that
/// can spuriously fail-close an in-flight candidate. A candidate's fetch-error VERDICT must read THIS
/// per-instance bundle instead; the globals stay for process-wide dashboards only.
#[derive(Default)]
pub struct RpcFetchCounters {
    /// This fetcher-lineage's RPC round-trips (the pre-2026-07-05 per-instance `fetches` counter).
    pub round_trips: AtomicU64,
    /// This fetcher-lineage's FAILED RPC fetches — the validator-local fail-closed signal (WP3).
    pub errors: AtomicU64,
}

/// Fetches canonical state from an Arbitrum RPC at a fixed block. Misses / RPC errors
/// resolve to defaults (zero / no code) so the fetcher never panics the EVM.
pub struct RpcStateFetcher<'a> {
    client: &'a ArbitrumRpcClient,
    block: BlockTag,
    /// [WP3 · Fix D] Per-instance round-trip + error counters (an `Arc` so a repin reuses the SAME
    /// bundle and a sampler holding the handle keeps working across fork repins).
    counters: std::sync::Arc<RpcFetchCounters>,
}

impl<'a> RpcStateFetcher<'a> {
    /// Pin to `block` (fresh instance counters).
    pub fn new(client: &'a ArbitrumRpcClient, block: BlockTag) -> Self {
        Self {
            client,
            block,
            counters: std::sync::Arc::new(RpcFetchCounters::default()),
        }
    }

    /// Pin to `block`, REUSING an existing counter bundle (repin keeps continuity so a sampler holding
    /// the handle keeps attributing round-trips + errors to this validator lineage across repins).
    pub fn new_with_counters(
        client: &'a ArbitrumRpcClient,
        block: BlockTag,
        counters: std::sync::Arc<RpcFetchCounters>,
    ) -> Self {
        Self {
            client,
            block,
            counters,
        }
    }

    /// Clone the instance counter bundle (sample `round_trips`/`errors` with `load`).
    pub fn counters(&self) -> std::sync::Arc<RpcFetchCounters> {
        self.counters.clone()
    }
}

impl StateFetcher for RpcStateFetcher<'_> {
    fn fetch_account(&self, address: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
        // ONE batched round-trip for balance + code + nonce (was 3 sequential — each a ~30-160ms
        // hot-path stall when cold on a remote provider). Count the round-trip locally AND globally.
        RPC_FETCHES.fetch_add(1, Ordering::Relaxed);
        self.counters.round_trips.fetch_add(1, Ordering::Relaxed);
        // On RPC error each field resolves to a default (so the EVM never panics), but we COUNT the
        // failure — LOCALLY (the validator's fail-closed verdict) and globally (dashboard) — so the
        // validator fail-closes: a default here is fabricated state, not real.
        match self.client.account_bundle(&address, self.block) {
            Ok(bundle) => {
                let balance = bundle.balance.unwrap_or_else(|_| {
                    self.count_error(1);
                    U256::ZERO
                });
                // Empty code is a LEGITIMATE answer (EOA) — only an RPC Err is a fetch failure.
                let code = match bundle.code {
                    Ok(c) => (!c.is_empty()).then_some(c),
                    Err(_) => {
                        self.count_error(1);
                        None
                    }
                };
                let nonce = bundle.nonce.unwrap_or_else(|_| {
                    self.count_error(1);
                    0
                });
                (balance, nonce, code)
            }
            Err(_) => {
                // Whole-batch transport failure: all three fields are fabricated defaults.
                self.count_error(3);
                (U256::ZERO, 0, None)
            }
        }
    }

    fn fetch_storage(&self, address: [u8; 20], slot: U256) -> U256 {
        RPC_FETCHES.fetch_add(1, Ordering::Relaxed);
        self.counters.round_trips.fetch_add(1, Ordering::Relaxed);
        let slot_bytes: [u8; 32] = slot.to_be_bytes();
        match self.client.storage_at(&address, &slot_bytes, self.block) {
            Ok(b) => U256::from_be_bytes(b),
            Err(_) => {
                // A failed storage read is indistinguishable from a real zero slot — return zero so
                // the EVM doesn't panic, but count it (local + global) so the validator rejects this
                // candidate fail-closed rather than trusting fabricated state for a capital decision.
                self.count_error(1);
                U256::ZERO
            }
        }
    }
}

impl RpcStateFetcher<'_> {
    /// [WP3 · Fix D] Record `n` failed fetches: the PER-INSTANCE `errors` counter (the validator's
    /// fail-closed verdict source) AND the process-global `RPC_FETCH_ERRORS` (dashboard telemetry).
    #[inline]
    fn count_error(&self, n: u64) {
        self.counters.errors.fetch_add(n, Ordering::Relaxed);
        RPC_FETCH_ERRORS.fetch_add(n, Ordering::Relaxed);
    }
}

// ── [Stage 5 · MPT-verified cold tier] ──────────────────────────────────────────────────────────
//
// Provider-integrity hardening (default OFF, `MIRROR_COLD_VERIFY=0`). When enabled, the advancer's
// reconcile canonical fetcher is wrapped in a [`ProofVerifyingFetcher`]: every cold storage value it
// serves is cryptographically proven against the block state root via `eth_getProof` +
// [`crate::mpt_verifier`] BEFORE it can repair the standing mirror — so a provider serving
// forked/garbage state is rejected rather than silently poisoning the mirror. Off ⇒ this code is
// never constructed and the cold fill is byte-identical to today's bare `eth_getStorageAt`.
//
// SCOPE: this wraps the BACKGROUND reconcile path only. The hot validator fetcher is intentionally
// NOT wrapped — Stage 2 drives `val_rpc_cold`→0 (so there is ~nothing cold to verify there) and S5
// is a provider-integrity/durability upgrade, NOT a capital-path change. Per-slot proofs + a
// per-block state-root cache; batching proofs by account and hedged racing across the HOT/base RPC
// split (commit 991e254) are soak-time optimizations noted in the plan, not implemented here.

/// Cold MPT verifications ATTEMPTED (telemetry; only moves when `MIRROR_COLD_VERIFY=1`).
pub static MPT_VERIFY_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
/// Cold MPT verifications that FAILED (invalid proof, value mismatch, or fetch/parse error). Each
/// failure ALSO bumps [`RPC_FETCH_ERRORS`], so the validator's existing fail-closed gate rejects any
/// candidate whose validation touched an unverifiable cold slot.
pub static MPT_VERIFY_FAILURES: AtomicU64 = AtomicU64::new(0);

/// A cold-proof fetch / parse / verify failure (reason string for logs + metrics).
#[derive(Debug, Clone)]
pub struct ProofError(String);

impl core::fmt::Display for ProofError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProofError {}

/// `0x`-prefixed lowercase hex of `bytes`.
fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", alloy_primitives::hex::encode(bytes))
}

/// Parse a `0x`-hex 32-byte field into a [`B256`].
fn b256_field(s: Option<&str>, what: &str) -> Result<B256, ProofError> {
    let s = s.ok_or_else(|| ProofError(format!("missing {what}")))?;
    let bytes = engine_rpc::client::decode_hex_with_prefix(s)
        .map_err(|e| ProofError(format!("{what}: {e}")))?;
    if bytes.len() != 32 {
        return Err(ProofError(format!("{what} is not 32 bytes")));
    }
    Ok(B256::from_slice(&bytes))
}

/// Parse a JSON array of `0x`-hex strings into a list of RLP node byte-vectors.
fn hex_nodes(v: &serde_json::Value, what: &str) -> Result<Vec<Vec<u8>>, ProofError> {
    let arr = v
        .as_array()
        .ok_or_else(|| ProofError(format!("{what} is not an array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for n in arr {
        let s = n
            .as_str()
            .ok_or_else(|| ProofError(format!("{what} node is not a string")))?;
        out.push(
            engine_rpc::client::decode_hex_with_prefix(s)
                .map_err(|e| ProofError(format!("{what}: {e}")))?,
        );
    }
    Ok(out)
}

/// Fetch the block's `stateRoot` (`eth_getBlockByNumber`) — the anchor for account-proof verification.
///
/// WI-06 T3 fix round 5 (codex round-5 item 2, totality): the block param is a `u64` block NUMBER, NOT
/// a `BlockTag`. A `BlockTag::Hash` serialized via [`BlockTag::as_str`] would emit a BARE hash hex — an
/// INVALID positional JSON-RPC block parameter (a hash MUST use the EIP-1898 object via
/// [`BlockTag::to_json_param`]). Taking `u64` makes that TYPE-LEVEL impossible: only `BlockTag::Number`
/// is ever constructed here, whose `as_str` is the correct `0x…` number form.
pub fn fetch_state_root(client: &ArbitrumRpcClient, block: u64) -> Result<B256, ProofError> {
    let v = client
        .call(
            "eth_getBlockByNumber",
            serde_json::json!([BlockTag::Number(block).as_str(), false]),
        )
        .map_err(|e| ProofError(format!("getBlockByNumber: {e}")))?;
    b256_field(v.get("stateRoot").and_then(|x| x.as_str()), "stateRoot")
}

/// Fetch + parse an EIP-1186 proof (`eth_getProof`) for `address` + `keys` at `block` into the lean
/// [`AccountProof`] the verifier consumes.
///
/// WI-06 T3 fix round 5 (codex round-5 item 2, totality): the block param is a `u64` block NUMBER (see
/// [`fetch_state_root`]) — a `BlockTag::Hash` can never reach the `as_str` serialization here.
pub fn fetch_account_proof(
    client: &ArbitrumRpcClient,
    address: [u8; 20],
    keys: &[U256],
    block: u64,
) -> Result<AccountProof, ProofError> {
    let keys_hex: Vec<String> = keys.iter().map(|k| hex0x(&k.to_be_bytes::<32>())).collect();
    let v = client
        .call(
            "eth_getProof",
            serde_json::json!([hex0x(&address), keys_hex, BlockTag::Number(block).as_str()]),
        )
        .map_err(|e| ProofError(format!("getProof: {e}")))?;
    parse_account_proof(&v, address)
}

/// Parse an `eth_getProof` result object into an [`AccountProof`] (pure; unit-tested with fixed JSON).
pub fn parse_account_proof(v: &serde_json::Value, address: [u8; 20]) -> Result<AccountProof, ProofError> {
    use engine_rpc::client::{parse_hex_u256, parse_hex_u64};
    let balance = parse_hex_u256(
        v.get("balance").and_then(|x| x.as_str()).ok_or_else(|| ProofError("missing balance".into()))?,
    )
    .map_err(|e| ProofError(format!("balance: {e}")))?;
    let nonce = parse_hex_u64(
        v.get("nonce").and_then(|x| x.as_str()).ok_or_else(|| ProofError("missing nonce".into()))?,
    )
    .map_err(|e| ProofError(format!("nonce: {e}")))?;
    let storage_hash = b256_field(v.get("storageHash").and_then(|x| x.as_str()), "storageHash")?;
    let code_hash = b256_field(v.get("codeHash").and_then(|x| x.as_str()), "codeHash")?;
    let account_proof = hex_nodes(
        v.get("accountProof").ok_or_else(|| ProofError("missing accountProof".into()))?,
        "accountProof",
    )?;
    let mut storage_proof = Vec::new();
    if let Some(arr) = v.get("storageProof").and_then(|x| x.as_array()) {
        for sp in arr {
            let key = parse_hex_u256(
                sp.get("key").and_then(|x| x.as_str()).ok_or_else(|| ProofError("missing storageProof.key".into()))?,
            )
            .map_err(|e| ProofError(format!("storageProof.key: {e}")))?;
            let value = parse_hex_u256(
                sp.get("value").and_then(|x| x.as_str()).ok_or_else(|| ProofError("missing storageProof.value".into()))?,
            )
            .map_err(|e| ProofError(format!("storageProof.value: {e}")))?;
            let proof = hex_nodes(
                sp.get("proof").ok_or_else(|| ProofError("missing storageProof.proof".into()))?,
                "storageProof.proof",
            )?;
            storage_proof.push(StorageProof { key, value, proof });
        }
    }
    Ok(AccountProof {
        address,
        balance,
        nonce,
        storage_hash,
        code_hash,
        account_proof,
        storage_proof,
    })
}

// ── [Stage 5 · WP0 · A6] First-layer input bounds for the cold-proof tier ───────────────────────
//
// Enforced at the network/JSON boundary BEFORE `Vec::with_capacity` / hex decode, so a malicious or
// broken provider cannot force an unbounded allocation. `mpt_verifier`'s own `MAX_PROOF_NODES` /
// `MAX_NODE_SIZE` remain a SECOND defensive layer inside the pure walker (design A6). These caps are
// deliberately generous vs a real single-key Arbitrum proof (a few KiB, <10 nodes/list).

/// Max raw HTTP body bytes for an `eth_getProof` / header response (rejected before JSON parse).
const MAX_PROOF_BODY_BYTES: usize = 1 << 20; // 1 MiB
/// Max nodes in an `accountProof` array (checked before allocation).
const MAX_ACCOUNT_PROOF_NODES: usize = 32;
/// Max entries in a `storageProof` array (checked before iteration; the one-key API needs exactly 1).
const MAX_STORAGE_ENTRIES: usize = 8;
/// Max nodes in a single `storageProof[].proof` array.
const MAX_STORAGE_PROOF_NODES: usize = 32;
/// Max decoded bytes for a single trie node (per-node allocation guard).
const MAX_NODE_DECODED_BYTES: usize = 8 * 1024;
/// Max hex characters for a single node string (checked before decode ⇒ bounds the decode alloc).
const MAX_NODE_HEX_CHARS: usize = MAX_NODE_DECODED_BYTES * 2 + 2; // +2 for the "0x" prefix
/// Max aggregate decoded proof bytes across the whole response (account proof + every storage proof).
const MAX_AGGREGATE_DECODED_BYTES: usize = 128 * 1024;
/// Max raw HTTP body bytes for a block-header response (headers are small; this is generous).
const MAX_HEADER_BODY_BYTES: usize = 256 * 1024;

/// [WP0 · A5] The canonical-identity fields of a block header, fetched to cross-check the state-root
/// anchor across two independent providers (and, optionally, against a trusted feed block hash).
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeaderAnchor {
    number: u64,
    hash: B256,
    parent_hash: B256,
    state_root: B256,
}

/// [WP0 · A5] Fetch a block's header identity (number, hash, parentHash, stateRoot) under a capped
/// body. A transport/timeout error, a null result, or a missing/malformed field ⇒ `HeaderUnavailable`
/// (the caller may reclassify a *crosscheck* source's unavailability as `UntrustedAnchor`).
fn fetch_header_anchor(
    client: &ArbitrumRpcClient,
    block: u64,
    address: [u8; 20],
    slot: U256,
) -> Result<HeaderAnchor, CanonicalFetchError> {
    let unavailable =
        |reason: String| cfe(CanonicalFetchErrorKind::HeaderUnavailable, block, address, slot, reason);
    let v = client
        .call_capped(
            "eth_getBlockByNumber",
            serde_json::json!([BlockTag::Number(block).as_str(), false]),
            MAX_HEADER_BODY_BYTES,
        )
        .map_err(|e| unavailable(format!("getBlockByNumber: {e}")))?;
    if v.is_null() {
        return Err(unavailable("null block header".into()));
    }
    let number = engine_rpc::client::parse_hex_u64(
        v.get("number")
            .and_then(|x| x.as_str())
            .ok_or_else(|| unavailable("missing header number".into()))?,
    )
    .map_err(|e| unavailable(format!("header number: {e}")))?;
    let hash = b256_field(v.get("hash").and_then(|x| x.as_str()), "header hash")
        .map_err(|e| unavailable(e.to_string()))?;
    let parent_hash = b256_field(v.get("parentHash").and_then(|x| x.as_str()), "parentHash")
        .map_err(|e| unavailable(e.to_string()))?;
    let state_root = b256_field(v.get("stateRoot").and_then(|x| x.as_str()), "stateRoot")
        .map_err(|e| unavailable(e.to_string()))?;
    Ok(HeaderAnchor {
        number,
        hash,
        parent_hash,
        state_root,
    })
}

/// [WP5 · F1/F4] A block's canonical identity (number/hash/parentHash/stateRoot) as raw 32-byte
/// arrays — the persistence provenance written at close and the canonical truth compared at resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockIdentity {
    /// Block number.
    pub number: u64,
    /// Block hash.
    pub hash: [u8; 32],
    /// Parent block hash.
    pub parent_hash: [u8; 32],
    /// State root.
    pub state_root: [u8; 32],
}

impl From<HeaderAnchor> for BlockIdentity {
    fn from(a: HeaderAnchor) -> Self {
        BlockIdentity {
            number: a.number,
            hash: a.hash.0,
            parent_hash: a.parent_hash.0,
            state_root: a.state_root.0,
        }
    }
}

/// [WP5 · FIX-B / A5] Pure two-provider agreement core for a block identity. The archive's header must
/// be for exactly `block` and agree EXACTLY (number/hash/parentHash/stateRoot) with an independent
/// cross-check provider. `secondary`: `None` = no distinct source configured (no independence);
/// `Some(Err)` = distinct source unavailable; `Some(Ok)` = fetched. Any wrong-block, disagreement, or
/// absent/unavailable independent source ⇒ `UntrustedAnchor` (fail-closed). Unit-testable without a
/// network.
fn crosschecked_identity(
    block: u64,
    primary: Result<HeaderAnchor, CanonicalFetchError>,
    secondary: Option<Result<HeaderAnchor, CanonicalFetchError>>,
) -> Result<BlockIdentity, CanonicalFetchError> {
    let untrusted = |reason: String| {
        cfe(CanonicalFetchErrorKind::UntrustedAnchor, block, [0u8; 20], U256::ZERO, reason)
    };
    let primary = primary?;
    if primary.number != block {
        return Err(untrusted(format!(
            "primary header is block {} but requested {block} (wrong block / reorg)",
            primary.number
        )));
    }
    match secondary {
        None => Err(untrusted("no independent header source configured (archive == base)".into())),
        Some(Err(e)) => Err(untrusted(format!("independent source unavailable: {}", e.reason))),
        Some(Ok(sec)) if sec == primary => Ok(BlockIdentity::from(primary)),
        Some(Ok(_)) => Err(untrusted(
            "independent header disagrees on number/hash/parentHash/stateRoot".into(),
        )),
    }
}

/// [WP5 · FIX-B / A5] Two-provider CROSS-CHECKED block identity — the canonical-truth oracle for both
/// the persist-time provenance stamp AND the resume-time header validation. Fetches the header from the
/// archive endpoint (`ARBITRUM_RPC_URL_ARCHIVE` → hot → base) AND an independent base provider under the
/// same distinct-host policy as the proof anchor ([`ProofVerifyingFetcher`]), and requires FULL
/// agreement. Any wrong/reorged block, disagreement, or missing/unavailable distinct source ⇒
/// `UntrustedAnchor` — so a single malicious provider consistent at both write and read cannot
/// warm-resume a fabricated prefix. When no distinct archive is configured this fails closed (no
/// provenance stamped ⇒ resume stays cold), exactly like the checked reconcile path.
pub fn fetch_block_identity(
    base_client: &ArbitrumRpcClient,
    block: u64,
) -> Result<BlockIdentity, CanonicalFetchError> {
    let base_ep = base_client.endpoint();
    let archive_url = engine_rpc::archive_rpc_url();
    // Archive serves the primary header; base is the independent cross-check — ONLY when genuinely
    // distinct provider hosts (else no independence ⇒ fail-closed, no vacuous self-crosscheck).
    let (archive_client, crosscheck) = match &archive_url {
        Some(a) if !same_rpc_host(a, base_ep) => (
            ArbitrumRpcClient::new(a.clone()),
            Some(ArbitrumRpcClient::new(base_ep.to_string())),
        ),
        _ => (ArbitrumRpcClient::new(base_ep.to_string()), None),
    };
    let primary = fetch_header_anchor(&archive_client, block, [0u8; 20], U256::ZERO);
    let secondary =
        crosscheck.map(|cc| fetch_header_anchor(&cc, block, [0u8; 20], U256::ZERO));
    crosschecked_identity(block, primary, secondary)
}

/// Build a typed [`CanonicalFetchError`] for the bounded parse path.
fn cfe(
    kind: CanonicalFetchErrorKind,
    block: u64,
    address: [u8; 20],
    slot: U256,
    reason: impl Into<String>,
) -> CanonicalFetchError {
    CanonicalFetchError::new(kind, block, address, slot, reason)
}

/// [WP0 · A6] Decode a JSON array of `0x`-hex node strings into RLP byte-vectors, enforcing node
/// count, per-node hex length, per-node decoded size, and a running aggregate-bytes cap — every check
/// BEFORE the corresponding allocation/decode. `aggregate` accumulates across all node lists in one
/// response so the total decoded footprint is bounded, not just each list.
fn hex_nodes_bounded(
    v: &serde_json::Value,
    what: &str,
    max_nodes: usize,
    aggregate: &mut usize,
    block: u64,
    address: [u8; 20],
    slot: U256,
) -> Result<Vec<Vec<u8>>, CanonicalFetchError> {
    let limit = |reason: String| cfe(CanonicalFetchErrorKind::InputLimitExceeded, block, address, slot, reason);
    let malformed =
        |reason: String| cfe(CanonicalFetchErrorKind::MalformedProof, block, address, slot, reason);
    let arr = v
        .as_array()
        .ok_or_else(|| malformed(format!("{what} is not an array")))?;
    if arr.len() > max_nodes {
        return Err(limit(format!(
            "{what} has {} nodes (max {max_nodes})",
            arr.len()
        )));
    }
    // Safe: arr.len() is already bounded by max_nodes above.
    let mut out = Vec::with_capacity(arr.len());
    for n in arr {
        let s = n
            .as_str()
            .ok_or_else(|| malformed(format!("{what} node is not a string")))?;
        if s.len() > MAX_NODE_HEX_CHARS {
            return Err(limit(format!(
                "{what} node hex length {} exceeds {MAX_NODE_HEX_CHARS}",
                s.len()
            )));
        }
        let bytes = engine_rpc::client::decode_hex_with_prefix(s)
            .map_err(|e| malformed(format!("{what}: {e}")))?;
        if bytes.len() > MAX_NODE_DECODED_BYTES {
            return Err(limit(format!(
                "{what} node decoded {} bytes exceeds {MAX_NODE_DECODED_BYTES}",
                bytes.len()
            )));
        }
        *aggregate = aggregate
            .checked_add(bytes.len())
            .ok_or_else(|| limit("aggregate decoded-bytes overflow".to_string()))?;
        if *aggregate > MAX_AGGREGATE_DECODED_BYTES {
            return Err(limit(format!(
                "aggregate decoded bytes {} exceeds {MAX_AGGREGATE_DECODED_BYTES}",
                *aggregate
            )));
        }
        out.push(bytes);
    }
    Ok(out)
}

/// [WP0 · A6/A4] Parse an `eth_getProof` result into an [`AccountProof`] under strict input bounds and
/// EXACT requested-key cardinality: the `storageProof` array must contain exactly `requested_keys`, in
/// request order (EIP-1186 returns entries "as requested"), with no missing/extra/duplicate key. This
/// replaces any `.find`-based key selection — the checked verify path indexes position 0 only after
/// this function has proven the array is exactly the requested set. Returns a typed
/// [`CanonicalFetchError`]; never a value.
fn parse_account_proof_bounded(
    v: &serde_json::Value,
    address: [u8; 20],
    requested_keys: &[U256],
    block: u64,
) -> Result<AccountProof, CanonicalFetchError> {
    use engine_rpc::client::{parse_hex_u256, parse_hex_u64};
    let primary_slot = requested_keys.first().copied().unwrap_or(U256::ZERO);
    let malformed = |reason: String| {
        cfe(CanonicalFetchErrorKind::MalformedProof, block, address, primary_slot, reason)
    };
    let limit = |reason: String| {
        cfe(CanonicalFetchErrorKind::InputLimitExceeded, block, address, primary_slot, reason)
    };

    let balance = parse_hex_u256(
        v.get("balance")
            .and_then(|x| x.as_str())
            .ok_or_else(|| malformed("missing balance".into()))?,
    )
    .map_err(|e| malformed(format!("balance: {e}")))?;
    let nonce = parse_hex_u64(
        v.get("nonce")
            .and_then(|x| x.as_str())
            .ok_or_else(|| malformed("missing nonce".into()))?,
    )
    .map_err(|e| malformed(format!("nonce: {e}")))?;
    let storage_hash = b256_field(v.get("storageHash").and_then(|x| x.as_str()), "storageHash")
        .map_err(|e| malformed(e.to_string()))?;
    let code_hash = b256_field(v.get("codeHash").and_then(|x| x.as_str()), "codeHash")
        .map_err(|e| malformed(e.to_string()))?;

    let mut aggregate = 0usize;
    let account_proof = hex_nodes_bounded(
        v.get("accountProof")
            .ok_or_else(|| malformed("missing accountProof".into()))?,
        "accountProof",
        MAX_ACCOUNT_PROOF_NODES,
        &mut aggregate,
        block,
        address,
        primary_slot,
    )?;

    let sp_arr = v
        .get("storageProof")
        .and_then(|x| x.as_array())
        .ok_or_else(|| malformed("missing/!array storageProof".into()))?;
    if sp_arr.len() > MAX_STORAGE_ENTRIES {
        return Err(limit(format!(
            "storageProof has {} entries (max {MAX_STORAGE_ENTRIES})",
            sp_arr.len()
        )));
    }
    // Exact cardinality: one entry per requested key, in request order (no missing/extra/duplicate).
    if sp_arr.len() != requested_keys.len() {
        return Err(malformed(format!(
            "storageProof has {} entries, requested {}",
            sp_arr.len(),
            requested_keys.len()
        )));
    }
    let mut storage_proof = Vec::with_capacity(sp_arr.len());
    for (i, sp) in sp_arr.iter().enumerate() {
        let key = parse_hex_u256(
            sp.get("key")
                .and_then(|x| x.as_str())
                .ok_or_else(|| malformed("missing storageProof.key".into()))?,
        )
        .map_err(|e| malformed(format!("storageProof.key: {e}")))?;
        if key != requested_keys[i] {
            return Err(cfe(
                CanonicalFetchErrorKind::InvalidStorageProof,
                block,
                address,
                requested_keys[i],
                format!("storageProof[{i}] key mismatch: got {key}, requested {}", requested_keys[i]),
            ));
        }
        let value = parse_hex_u256(
            sp.get("value")
                .and_then(|x| x.as_str())
                .ok_or_else(|| malformed("missing storageProof.value".into()))?,
        )
        .map_err(|e| malformed(format!("storageProof.value: {e}")))?;
        let proof = hex_nodes_bounded(
            sp.get("proof")
                .ok_or_else(|| malformed("missing storageProof.proof".into()))?,
            "storageProof.proof",
            MAX_STORAGE_PROOF_NODES,
            &mut aggregate,
            block,
            address,
            key,
        )?;
        storage_proof.push(StorageProof { key, value, proof });
    }

    Ok(AccountProof {
        address,
        balance,
        nonce,
        storage_hash,
        code_hash,
        account_proof,
        storage_proof,
    })
}

/// [WP0 · A6] Fetch + bounded-parse an EIP-1186 proof for `address`+`keys` at `block` through a
/// capped HTTP body, returning a typed [`CanonicalFetchError`]. Transport/timeout/null ⇒
/// `ProofUnavailable`; an oversized body ⇒ `InputLimitExceeded` (rejected before JSON parse).
fn fetch_account_proof_bounded(
    client: &ArbitrumRpcClient,
    address: [u8; 20],
    keys: &[U256],
    block: u64,
) -> Result<AccountProof, CanonicalFetchError> {
    let primary_slot = keys.first().copied().unwrap_or(U256::ZERO);
    let keys_hex: Vec<String> = keys.iter().map(|k| hex0x(&k.to_be_bytes::<32>())).collect();
    let v = client
        .call_capped(
            "eth_getProof",
            serde_json::json!([hex0x(&address), keys_hex, BlockTag::Number(block).as_str()]),
            MAX_PROOF_BODY_BYTES,
        )
        .map_err(|e| match e {
            RpcError::BodyTooLarge { limit } => cfe(
                CanonicalFetchErrorKind::InputLimitExceeded,
                block,
                address,
                primary_slot,
                format!("eth_getProof body exceeds {limit} bytes"),
            ),
            other => cfe(
                CanonicalFetchErrorKind::ProofUnavailable,
                block,
                address,
                primary_slot,
                format!("eth_getProof: {other}"),
            ),
        })?;
    parse_account_proof_bounded(&v, address, keys, block)
}

/// [Stage 5 · receipt deep-compare] Parse an `eth_getTransactionReceipt` result into a
/// [`ReceiptStatus`] (`null` ⇒ absent; `status` 0x1 ⇒ success; 0x0 ⇒ reverted; missing ⇒ absent).
pub fn parse_receipt_status(v: &serde_json::Value) -> ReceiptStatus {
    if v.is_null() {
        return ReceiptStatus::Absent;
    }
    match v.get("status").and_then(|s| s.as_str()) {
        Some(s) => {
            if matches!(s, "0x1" | "0x01") {
                ReceiptStatus::Success
            } else {
                ReceiptStatus::Reverted
            }
        }
        None => ReceiptStatus::Absent,
    }
}

/// [Stage 5 · receipt deep-compare] Fetch a tx's canonical receipt status (soak-gated live path). A
/// transport error is treated as `Absent` — conservative, since for a journaled tx that only forces a
/// (harmless, self-healing) reconcile of that block, never an under-check.
///
/// A7 (RPC policy): the canonical receipt is part of the same S5 cold-verify tier as the proof reads,
/// so it is routed through the archive supplement ([`engine_rpc::archive_rpc_url`]) rather than
/// lava-base, which may not reliably serve historical receipts. When no archive/hot/base override is
/// configured it falls back to the caller's own `client` — preserving the default (base) behavior so
/// nothing changes while A7 is dormant.
pub fn fetch_receipt_status(client: &ArbitrumRpcClient, tx_hash: &[u8; 32]) -> ReceiptStatus {
    let params = serde_json::json!([hex0x(tx_hash)]);
    let result = match engine_rpc::archive_rpc_url() {
        Some(url) => ArbitrumRpcClient::new(url).call("eth_getTransactionReceipt", params),
        None => client.call("eth_getTransactionReceipt", params),
    };
    match result {
        Ok(v) => parse_receipt_status(&v),
        Err(_) => ReceiptStatus::Absent,
    }
}

/// A cold [`StateFetcher`] wrapper that (when `verify`) proves each cold STORAGE value against the
/// block state root via `eth_getProof` before returning it. Account/`try_*` reads pass through (the
/// account proof is verified inline as the storage anchor). A verification failure bumps
/// [`MPT_VERIFY_FAILURES`] + [`RPC_FETCH_ERRORS`] (fail-closed) and still returns the inner value —
/// the EVM never panics; the candidate is rejected by the existing gate. With `verify=false` it is a
/// transparent pass-through, byte-identical to the inner fetcher.
///
/// Single-threaded like [`RpcStateFetcher`] (owned by the advancer's reconcile). The per-block state
/// root is cached in a `RefCell` so all of a block's slot proofs share one header fetch.
///
/// A7 (2026-07-12 RPC policy): `eth_getProof` + the block state-root anchor need an ARCHIVE-capable
/// node, and lava-base (the reconcile client) won't reliably serve them. So the fetcher holds its OWN
/// archive [`ArbitrumRpcClient`] (resolved from `archive_rpc_url()`) and every proof read goes there —
/// the routing is INTERNAL, so the reconcile caller keeps passing its base client to [`Self::new`]
/// unchanged. The inner fetcher's cold STATE reads stay on the base client (non-latency); only the
/// proof supplement is archive-routed.
pub struct ProofVerifyingFetcher<F: StateFetcher> {
    inner: F,
    /// Archive-supplement client for `eth_getProof` + `eth_getBlockByNumber` (state root). Owned (not
    /// borrowed) — this is what makes the archive routing internal to the fetcher.
    archive: ArbitrumRpcClient,
    /// [WP0 · A5] Independent header source for the anchor cross-check. The checked path
    /// ([`Self::fetch_storage_verified`]) accepts the state root ONLY when this source's header for the
    /// pinned block matches the archive's exactly (number/hash/parentHash/stateRoot). `None` ⇒ no
    /// independent anchor ⇒ the checked path fails closed with `UntrustedAnchor` (a forked archive
    /// provider is never trusted as its own root of trust). The legacy infallible path is unaffected.
    crosscheck: Option<ArbitrumRpcClient>,
    /// [WP0 · A5] Optional trusted feed/local-node block hash for the pinned block. When set, the
    /// agreed header's hash must equal it — an additional binding on top of the two-provider agreement.
    /// Exposed for the feed to supply in WP1; unset in the current reconcile wiring.
    trusted_hash: Option<B256>,
    block: u64,
    verify: bool,
    state_root: std::cell::RefCell<Option<B256>>,
    /// [WP0 · A5] The cross-checked anchor state root, cached within this (single-block) instance.
    anchor_root: std::cell::RefCell<Option<B256>>,
    /// Per-instance verify attempt/failure counters (single-threaded owner) — the globals are
    /// process-wide telemetry; these give tests + per-reconcile attribution without cross-thread noise.
    attempts: std::cell::Cell<u64>,
    failures: std::cell::Cell<u64>,
}

impl<F: StateFetcher> ProofVerifyingFetcher<F> {
    /// Wrap `inner` for cold-proof verification at `block`. `verify=false` ⇒ transparent pass-through.
    ///
    /// A7: `base_client` is the reconcile's (lava) base client — retained for source-compat with the
    /// caller (shadow_revm `close_block`) and used ONLY as the archive-URL fallback source. Every
    /// `eth_getProof`/state-root read is routed to an archive-capable endpoint resolved by
    /// [`engine_rpc::archive_rpc_url`] (`ARBITRUM_RPC_URL_ARCHIVE` → hot → base), falling back to
    /// `base_client`'s own endpoint only when none is configured — never blindly to lava-base.
    pub fn new(inner: F, base_client: &ArbitrumRpcClient, block: u64, verify: bool) -> Self {
        let archive_url = engine_rpc::archive_rpc_url();
        let base_ep = base_client.endpoint();
        let url = archive_url.clone().unwrap_or_else(|| base_ep.to_string());
        // [WP0 · A5 + WP1 · ADV-F1] Wire the independent anchor cross-check from the deployment's RPC
        // split ONLY when the archive endpoint is a genuinely DISTINCT provider host from the base
        // (lava) endpoint. Comparison is by normalized host[:port] (not raw string), so two endpoints
        // that differ only by scheme, trailing slash, or an embedded API key are correctly recognized
        // as the SAME provider — a vacuous self-crosscheck that provides no independence. Same host (or
        // no distinct archive configured) ⇒ None ⇒ the checked path fails closed with `UntrustedAnchor`
        // (an explicit require-configured-independent-source policy). We never claim forked-provider
        // protection we did not wire.
        let crosscheck = match &archive_url {
            Some(a) if !same_rpc_host(a, base_ep) => {
                Some(ArbitrumRpcClient::new(base_ep.to_string()))
            }
            _ => None,
        };
        let mut f = Self::with_archive(inner, ArbitrumRpcClient::new(url), block, verify);
        f.crosscheck = crosscheck;
        f
    }

    /// Construct with an explicit `archive` client (internal delegate + deterministic test-injection
    /// point). The `archive` client serves every `eth_getProof`/state-root read this fetcher makes.
    /// No independent cross-check source is set — use [`Self::with_crosscheck`] to add one; without it
    /// the checked path ([`Self::fetch_storage_verified`]) fails closed with `UntrustedAnchor`.
    pub fn with_archive(inner: F, archive: ArbitrumRpcClient, block: u64, verify: bool) -> Self {
        Self {
            inner,
            archive,
            crosscheck: None,
            trusted_hash: None,
            block,
            verify,
            state_root: std::cell::RefCell::new(None),
            anchor_root: std::cell::RefCell::new(None),
            attempts: std::cell::Cell::new(0),
            failures: std::cell::Cell::new(0),
        }
    }

    /// [WP0 · A5] Set the independent header source used to cross-check the state-root anchor. Builder
    /// style for tests + explicit wiring; production [`Self::new`] derives it from the RPC split.
    pub fn with_crosscheck(mut self, crosscheck: ArbitrumRpcClient) -> Self {
        self.crosscheck = Some(crosscheck);
        self
    }

    /// [WP0 · A5] Bind the anchor to a trusted feed/local-node block hash for the pinned block (the
    /// agreed header's hash must equal it). Exposed for WP1's feed wiring.
    pub fn with_trusted_hash(mut self, hash: B256) -> Self {
        self.trusted_hash = Some(hash);
        self
    }

    /// This instance's cold-verify attempt count.
    pub fn verify_attempts(&self) -> u64 {
        self.attempts.get()
    }

    /// This instance's cold-verify failure count.
    pub fn verify_failures(&self) -> u64 {
        self.failures.get()
    }

    fn state_root(&self) -> Result<B256, ProofError> {
        if let Some(r) = *self.state_root.borrow() {
            return Ok(r);
        }
        let r = fetch_state_root(&self.archive, self.block)?;
        *self.state_root.borrow_mut() = Some(r);
        Ok(r)
    }

    /// Prove `(address, slot)==value` against the block state root. Verifies the account proof (⇒
    /// trusted `storage_hash`), then the storage proof, then that the proof's value equals the
    /// fetched `value`.
    fn verify_slot(&self, address: [u8; 20], slot: U256, value: U256) -> Result<(), ProofError> {
        let root = self.state_root()?;
        let proof = fetch_account_proof(&self.archive, address, &[slot], self.block)?;
        mpt_verifier::verify_account_proof(&proof, root)
            .map_err(|e| ProofError(format!("account proof: {e}")))?;
        let sp = proof
            .storage_proof
            .iter()
            .find(|p| p.key == slot)
            .ok_or_else(|| ProofError("no storage proof returned for slot".into()))?;
        mpt_verifier::verify_storage_proof(sp, proof.storage_hash)
            .map_err(|e| ProofError(format!("storage proof: {e}")))?;
        if sp.value != value {
            return Err(ProofError("proof value disagrees with fetched value".into()));
        }
        Ok(())
    }

    /// [WP0 · A5] Resolve the state root the proof will be verified against, INDEPENDENTLY of the proof
    /// provider. The archive's header for the pinned block must (a) be for exactly `self.block`, (b)
    /// match the trusted feed hash if one is set, and (c) agree exactly (number/hash/parentHash/
    /// stateRoot) with an independent cross-check provider. Any disagreement, wrong/null block, reorg
    /// during resolution, or missing independent source ⇒ `UntrustedAnchor` (fail closed). Cached
    /// within this single-block instance.
    fn resolve_anchor(&self, address: [u8; 20], slot: U256) -> Result<B256, CanonicalFetchError> {
        if let Some(r) = *self.anchor_root.borrow() {
            return Ok(r);
        }
        let untrusted = |reason: String| {
            cfe(CanonicalFetchErrorKind::UntrustedAnchor, self.block, address, slot, reason)
        };
        let primary = fetch_header_anchor(&self.archive, self.block, address, slot)?;
        if primary.number != self.block {
            return Err(untrusted(format!(
                "primary header is block {} but pinned to {} (wrong block / reorg)",
                primary.number, self.block
            )));
        }
        if let Some(th) = self.trusted_hash {
            if primary.hash != th {
                return Err(untrusted(
                    "primary header hash disagrees with the trusted feed block hash".into(),
                ));
            }
        }
        match &self.crosscheck {
            Some(cc) => {
                // An unavailable independent source cannot establish trust ⇒ UntrustedAnchor.
                let sec = fetch_header_anchor(cc, self.block, address, slot)
                    .map_err(|e| untrusted(format!("independent source unavailable: {}", e.reason)))?;
                if sec != primary {
                    return Err(untrusted(
                        "independent header disagrees on number/hash/parentHash/stateRoot".into(),
                    ));
                }
            }
            None => {
                return Err(untrusted("no independent anchor source configured".into()));
            }
        }
        let root = primary.state_root;
        *self.anchor_root.borrow_mut() = Some(root);
        Ok(root)
    }

    /// [WP0 · A3/A5/A6] The checked core: resolve the independently anchored state root, fetch the
    /// bounded EIP-1186 proof for exactly this slot, verify the account proof against the anchored root
    /// and the storage proof against the proven account storage root, and return the PROVEN
    /// `storageProof.value`. No infallible `eth_getStorageAt` enters this trust decision (A3). Every
    /// failure is a typed [`CanonicalFetchError`]; a value is returned only on full success.
    fn verify_slot_checked(&self, address: [u8; 20], slot: U256) -> Result<U256, CanonicalFetchError> {
        let root = self.resolve_anchor(address, slot)?;
        let proof = fetch_account_proof_bounded(&self.archive, address, &[slot], self.block)?;
        mpt_verifier::verify_account_proof(&proof, root).map_err(|e| {
            cfe(
                CanonicalFetchErrorKind::InvalidAccountProof,
                self.block,
                address,
                slot,
                format!("account proof: {e}"),
            )
        })?;
        // Cardinality is already guaranteed by `fetch_account_proof_bounded` (exactly one entry whose
        // key == slot), so position 0 is the requested slot — no `.find`, no first-match ambiguity.
        let sp = proof.storage_proof.first().ok_or_else(|| {
            cfe(
                CanonicalFetchErrorKind::MalformedProof,
                self.block,
                address,
                slot,
                "no storage proof for requested slot",
            )
        })?;
        mpt_verifier::verify_storage_proof(sp, proof.storage_hash).map_err(|e| {
            cfe(
                CanonicalFetchErrorKind::InvalidStorageProof,
                self.block,
                address,
                slot,
                format!("storage proof: {e}"),
            )
        })?;
        Ok(sp.value)
    }

    /// [WP0 · A2/A3] Checked, fail-closed cold storage read: returns the value PROVEN against an
    /// independently anchored canonical state root, or a typed [`CanonicalFetchError`] that carries no
    /// value. This is the seam WP1's reconcile will consume so an unavailable/invalid proof or an
    /// untrusted anchor stops a value from entering the mirror. With `verify=false` it is the
    /// feature-off compatibility wrapper: `Ok(inner.fetch_storage(..))`, no proof/header RPC.
    pub fn fetch_storage_verified(
        &self,
        address: [u8; 20],
        slot: U256,
    ) -> Result<U256, CanonicalFetchError> {
        if !self.verify {
            return Ok(self.inner.fetch_storage(address, slot));
        }
        self.attempts.set(self.attempts.get() + 1);
        MPT_VERIFY_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        match self.verify_slot_checked(address, slot) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.failures.set(self.failures.get() + 1);
                MPT_VERIFY_FAILURES.fetch_add(1, Ordering::Relaxed);
                // Note: unlike the legacy infallible path, the checked path does NOT bump the global
                // RPC_FETCH_ERRORS — it fails closed by returning Err, so it cannot contaminate an
                // unrelated candidate's validator-local error verdict (Finding 2 / WP3).
                tracing::debug!(reason = %e, "MPT checked cold-verify failed (fail-closed, no value)");
                Err(e)
            }
        }
    }
}

impl<F: StateFetcher> StateFetcher for ProofVerifyingFetcher<F> {
    fn fetch_account(&self, address: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
        self.inner.fetch_account(address)
    }

    fn fetch_storage(&self, address: [u8; 20], slot: U256) -> U256 {
        let value = self.inner.fetch_storage(address, slot);
        if self.verify {
            self.attempts.set(self.attempts.get() + 1);
            MPT_VERIFY_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = self.verify_slot(address, slot, value) {
                self.failures.set(self.failures.get() + 1);
                // [WP3 · Fix D] A proof failure moves MPT telemetry ONLY — never the process-global
                // RPC_FETCH_ERRORS. A proof failure is not an RPC fetch error, and must not contaminate
                // any candidate's validator-local fail-closed verdict. (This legacy infallible path is
                // no longer on the reconcile trust path — WP1 rewired reconcile to fetch_storage_checked
                // — so it renders no fail-closed effect here; the checked path returns a typed Err.)
                MPT_VERIFY_FAILURES.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(reason = %e, "MPT cold-verify failed (legacy infallible path; MPT telemetry only)");
            }
        }
        value
    }

    /// [WP0 · A2] The checked hook delegates to [`Self::fetch_storage_verified`], so a fail-closed
    /// caller (WP1) gets the independently anchored, bounded, proven-value path with typed errors —
    /// never a fabricated value on failure. The legacy infallible [`Self::fetch_storage`] above is
    /// retained only for the pre-WP1 reconcile wiring and is removed when reconcile is rewired.
    fn fetch_storage_checked(
        &self,
        address: [u8; 20],
        slot: U256,
    ) -> Result<U256, CanonicalFetchError> {
        self.fetch_storage_verified(address, slot)
    }
}

#[cfg(test)]
/// Multi-connection mock RPC: answers every request until the test process exits
/// (thread detached). Serves `code_for` bytecode for that address (empty code for
/// everyone else), zero balances/nonces, and per-(addr,slot) storage words. Counts
/// connections so the test can prove BATCHED (not per-slot) round-trips.
pub(crate) fn spawn_mock_rpc(
    code_for: [u8; 20],
    code: Vec<u8>,
    storage: Vec<(([u8; 20], U256), U256)>,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU32>) {
    use alloy_primitives::hex;
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let conns = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let conns_srv = conns.clone();
    std::thread::spawn(move || {
        let code_hex = format!("0x{}", hex::encode(&code));
        let addr_hex = format!("0x{}", hex::encode(code_for));
        for stream in listener.incoming() {
            let Ok(mut sock) = stream else { break };
            conns_srv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            let body_start;
            while let Ok(n) = sock.read(&mut tmp) {
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        match serde_json::from_slice(&buf[body_start..body_start + clen]) {
                            Ok(v) => v,
                            Err(_) => break,
                        };
                    let answer_one = |item: &serde_json::Value| -> serde_json::Value {
                        let id = item["id"].clone();
                        let method = item["method"].as_str().unwrap_or("");
                        let result = match method {
                            "eth_getBalance" | "eth_getTransactionCount" => "0x0".to_string(),
                            "eth_getCode" => {
                                let a = item["params"][0].as_str().unwrap_or("");
                                if a.eq_ignore_ascii_case(&addr_hex) {
                                    code_hex.clone()
                                } else {
                                    "0x".to_string()
                                }
                            }
                            "eth_getStorageAt" => {
                                let a = item["params"][0].as_str().unwrap_or("");
                                let s = item["params"][1].as_str().unwrap_or("");
                                let hit = storage.iter().find(|((ad, sl), _)| {
                                    format!("0x{}", hex::encode(ad)).eq_ignore_ascii_case(a)
                                        && format!("0x{}", hex::encode(sl.to_be_bytes::<32>()))
                                            .eq_ignore_ascii_case(s)
                                });
                                match hit {
                                    Some((_, v)) => {
                                        format!("0x{}", hex::encode(v.to_be_bytes::<32>()))
                                    }
                                    None => format!("0x{}", "00".repeat(32)),
                                }
                            }
                            _ => "0x0".to_string(),
                        };
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
                    };
                    let reply = if let Some(arr) = body.as_array() {
                        serde_json::Value::Array(arr.iter().map(answer_one).collect()).to_string()
                    } else {
                        answer_one(&body).to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        }
    });
    (port, conns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ha(number: u64, hash: u8, parent: u8, root: u8) -> HeaderAnchor {
        HeaderAnchor {
            number,
            hash: B256::repeat_byte(hash),
            parent_hash: B256::repeat_byte(parent),
            state_root: B256::repeat_byte(root),
        }
    }

    #[test]
    fn crosschecked_identity_requires_two_provider_agreement() {
        // [WP5 · FIX-B] The block-identity oracle (used at BOTH the persist provenance stamp and the
        // resume header validation) must require an INDEPENDENT provider to agree exactly — so a single
        // malicious provider consistent at both write and read cannot warm-resume a fabricated prefix.
        let b = 100u64;

        // Agreement ⇒ Ok(identity).
        let id = crosschecked_identity(b, Ok(ha(b, 0x11, 0x10, 0x5a)), Some(Ok(ha(b, 0x11, 0x10, 0x5a))))
            .expect("two agreeing providers ⇒ trusted identity");
        assert_eq!(id.number, b);
        assert_eq!(id.hash, [0x11; 32]);
        assert_eq!(id.parent_hash, [0x10; 32]);

        // Disagreement (independent provider returns a different hash) ⇒ UntrustedAnchor.
        let e = crosschecked_identity(b, Ok(ha(b, 0x11, 0x10, 0x5a)), Some(Ok(ha(b, 0xEE, 0x10, 0x5a))))
            .unwrap_err();
        assert_eq!(e.kind, CanonicalFetchErrorKind::UntrustedAnchor, "disagreement ⇒ untrusted");

        // No distinct independent source configured ⇒ UntrustedAnchor (no vacuous self-crosscheck).
        assert_eq!(
            crosschecked_identity(b, Ok(ha(b, 0x11, 0x10, 0x5a)), None).unwrap_err().kind,
            CanonicalFetchErrorKind::UntrustedAnchor
        );

        // Independent source configured but unavailable ⇒ UntrustedAnchor (can't establish trust).
        let unavail = cfe(CanonicalFetchErrorKind::HeaderUnavailable, b, [0u8; 20], U256::ZERO, "down".to_string());
        assert_eq!(
            crosschecked_identity(b, Ok(ha(b, 0x11, 0x10, 0x5a)), Some(Err(unavail))).unwrap_err().kind,
            CanonicalFetchErrorKind::UntrustedAnchor
        );

        // Primary header is for the wrong block (reorg during resolution) ⇒ UntrustedAnchor.
        assert_eq!(
            crosschecked_identity(b, Ok(ha(b + 1, 0x11, 0x10, 0x5a)), Some(Ok(ha(b + 1, 0x11, 0x10, 0x5a))))
                .unwrap_err()
                .kind,
            CanonicalFetchErrorKind::UntrustedAnchor
        );
    }

    #[test]
    fn fetch_account_uses_one_batched_round_trip() {
        use std::io::{Read as _, Write as _};
        // Serves EXACTLY ONE connection then goes away. A batched fetcher gets all three fields
        // from that one round-trip; a sequential (3-call) fetcher gets only the first field and
        // connection-refused for the rest → defaulted nonce/code → this test fails.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body_start;
            loop {
                let n = sock.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    body_start = p + 4;
                    break;
                }
            }
            let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .expect("content-length");
            while buf.len() < body_start + clen {
                let n = sock.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
            let reply = if let Some(arr) = body.as_array() {
                assert_eq!(arr.len(), 3, "account bundle must batch balance+code+nonce");
                let ids: Vec<u64> = arr.iter().map(|i| i["id"].as_u64().unwrap()).collect();
                format!(
                    r#"[{{"jsonrpc":"2.0","id":{},"result":"0x10"}},{{"jsonrpc":"2.0","id":{},"result":"0x6001"}},{{"jsonrpc":"2.0","id":{},"result":"0x2a"}}]"#,
                    ids[0], ids[1], ids[2]
                )
            } else {
                // Sequential first call — answer it; the remaining sequential calls have no
                // server to talk to.
                let id = body["id"].as_u64().unwrap();
                format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"0x10"}}"#)
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
            sock.write_all(resp.as_bytes()).unwrap();
        });
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let fetcher = RpcStateFetcher::new(&client, BlockTag::Number(1));
        let (balance, nonce, code) = fetcher.fetch_account([0x42; 20]);
        server.join().unwrap();
        assert_eq!(balance, U256::from(0x10u64));
        assert_eq!(nonce, 42, "nonce must arrive in the SAME single round-trip");
        assert_eq!(
            code,
            Some(vec![0x60, 0x01]),
            "code must arrive in the SAME single round-trip"
        );
    }

    #[test]
    fn preload_storage_batched_one_round_trip_fills_cache() {
        use std::io::{Read as _, Write as _};
        // One HTTP round-trip for BOTH slots; a lazy fetch would return 0xDEAD — reads after the
        // preload must serve the batch's canonical words from cache.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body_start;
            loop {
                let n = sock.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    body_start = p + 4;
                    break;
                }
            }
            let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            while buf.len() < body_start + clen {
                let n = sock.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
            let arr = body.as_array().expect("batched eth_getStorageAt");
            assert_eq!(arr.len(), 2);
            for item in arr {
                assert_eq!(item["method"].as_str(), Some("eth_getStorageAt"));
            }
            let reply = format!(
                r#"[{{"jsonrpc":"2.0","id":{},"result":"0x{}"}},{{"jsonrpc":"2.0","id":{},"result":"0x{}"}}]"#,
                arr[0]["id"].as_u64().unwrap(),
                "11".repeat(32),
                arr[1]["id"].as_u64().unwrap(),
                "22".repeat(32),
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
            sock.write_all(resp.as_bytes()).unwrap();
        });
        struct DeadFetcher;
        impl engine_revm::StateFetcher for DeadFetcher {
            fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
                (U256::ZERO, 0, None)
            }
            fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
                U256::from(0xDEADu64)
            }
        }
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let mut exec = engine_revm::ForkExecutor::new(DeadFetcher);
        let pool = [0xA1u8; 20];
        let (loaded, errors) = preload_storage_batched(
            &mut exec,
            &client,
            BlockTag::Number(5),
            &[(pool, U256::ZERO), (pool, U256::from(4u64))],
        );
        server.join().unwrap();
        assert_eq!((loaded, errors), (2, 0));
        assert_eq!(
            exec.storage(pool, U256::ZERO),
            U256::from_be_bytes([0x11u8; 32]),
            "slot 0 must be the batch value, not a lazy fetch"
        );
        assert_eq!(
            exec.storage(pool, U256::from(4u64)),
            U256::from_be_bytes([0x22u8; 32]),
            "slot 4 must be the batch value, not a lazy fetch"
        );
    }

    #[test]
    fn warm_call_prefetch_converges_and_makes_the_real_call_zero_fetch() {
        // Pool bytecode: SLOAD(5); POP; SLOAD(7); POP; STOP — the probe must discover
        // slots 5+7, the prefetch must load them in ONE storage batch, and the real
        // replay afterwards must do ZERO lazy fetcher round-trips.
        let pool = [0xC0u8; 20];
        let code = vec![0x60, 0x05, 0x54, 0x50, 0x60, 0x07, 0x54, 0x50, 0x00];
        let (port, conns) = spawn_mock_rpc(
            pool,
            code,
            vec![
                ((pool, U256::from(5u64)), U256::from(0x55u64)),
                ((pool, U256::from(7u64)), U256::from(0x77u64)),
            ],
        );
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let fetcher = RpcStateFetcher::new(&client, BlockTag::Number(9));
        let mut exec = engine_revm::ForkExecutor::new(fetcher);
        exec.prewarm(pool); // code warm (1 conn)

        let eoa = [0xEEu8; 20];
        let stats = warm_call_prefetch(
            &mut exec,
            &client,
            BlockTag::Number(9),
            eoa,
            pool,
            U256::ZERO,
            &[],
            1_000_000,
            4,
        );
        assert!(stats.converged, "prefetch must converge: {stats:?}");
        assert_eq!(stats.slots_loaded, 2, "both probed slots batch-loaded");
        assert!(
            stats.accounts_loaded >= 1,
            "uncached caller (and beneficiary) account-warmed"
        );

        // THE invariant: the real execution after a converged prefetch is ZERO-fetch —
        // no lazy Alchemy round-trip on the hot path.
        let pre = rpc_fetch_count();
        let conns_pre = conns.load(std::sync::atomic::Ordering::SeqCst);
        let out = exec.replay_tx_commit(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);
        assert!(out.success, "warm replay must succeed: {}", out.reason);
        assert_eq!(
            rpc_fetch_count() - pre,
            0,
            "converged call must not touch the lazy fetcher"
        );
        assert_eq!(
            conns.load(std::sync::atomic::Ordering::SeqCst),
            conns_pre,
            "converged call must open no RPC connection at all"
        );
        // Canonical values actually landed (not fabricated zeros).
        assert_eq!(exec.storage(pool, U256::from(5u64)), U256::from(0x55u64));
        assert_eq!(exec.storage(pool, U256::from(7u64)), U256::from(0x77u64));
    }

    #[test]
    fn prefetch_reports_discovered_readset() {
        // The probe-discovered read-set must be surfaced on PrefetchStats so the
        // learned-read-set registry (P1) can replay it as ONE batched preload on the
        // next validation of the same route.
        let pool = [0xC1u8; 20];
        // SLOAD(5); POP; SLOAD(7); POP; STOP
        let code = vec![0x60, 0x05, 0x54, 0x50, 0x60, 0x07, 0x54, 0x50, 0x00];
        let (port, _conns) = spawn_mock_rpc(
            pool,
            code,
            vec![
                ((pool, U256::from(5u64)), U256::from(0x55u64)),
                ((pool, U256::from(7u64)), U256::from(0x77u64)),
            ],
        );
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let fetcher = RpcStateFetcher::new(&client, BlockTag::Number(9));
        let mut exec = engine_revm::ForkExecutor::new(fetcher);
        exec.prewarm(pool);

        let eoa = [0xEDu8; 20];
        let stats = warm_call_prefetch(
            &mut exec,
            &client,
            BlockTag::Number(9),
            eoa,
            pool,
            U256::ZERO,
            &[],
            1_000_000,
            4,
        );
        assert!(stats.converged, "prefetch must converge: {stats:?}");
        assert!(
            stats.discovered_slots.contains(&(pool, U256::from(5u64)))
                && stats.discovered_slots.contains(&(pool, U256::from(7u64))),
            "both probed slots must be reported as the discovered read-set: {:?}",
            stats.discovered_slots
        );
        assert!(
            stats.discovered_accounts.contains(&eoa),
            "the uncached caller must be reported: {:?}",
            stats.discovered_accounts
        );
        // Dedup invariant: replaying the learned set must not double-fetch.
        let mut slots_sorted = stats.discovered_slots.clone();
        slots_sorted.sort_unstable();
        slots_sorted.dedup();
        assert_eq!(
            slots_sorted.len(),
            stats.discovered_slots.len(),
            "discovered_slots must be deduped"
        );
        let mut accs_sorted = stats.discovered_accounts.clone();
        accs_sorted.sort_unstable();
        accs_sorted.dedup();
        assert_eq!(
            accs_sorted.len(),
            stats.discovered_accounts.len(),
            "discovered_accounts must be deduped"
        );
    }

    #[test]
    fn learned_readset_converges_repeat_validation_without_probe_iterations() {
        // Two-layer data-dependent contract: SLOAD(5) yields the NEXT slot to SLOAD —
        // exactly the exotic-family shape (LB bins / DODO PMM) that defeats blind
        // probing (one probe layer per dependency level). SLOAD(5)=7 → SLOAD(7).
        let pool = [0xC2u8; 20];
        // PUSH1 05; SLOAD; SLOAD; POP; STOP
        let code = vec![0x60, 0x05, 0x54, 0x54, 0x50, 0x00];
        let (port, _conns) = spawn_mock_rpc(
            pool,
            code,
            vec![
                ((pool, U256::from(5u64)), U256::from(7u64)),
                ((pool, U256::from(7u64)), U256::from(0x77u64)),
            ],
        );
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let route = engine_state::RouteId(42);
        let mut readsets = crate::route_readsets::RouteReadSets::new(16, 64);
        let eoa = [0xECu8; 20];

        // First validation: registry empty → blind probing needs >=2 layers.
        let mut exec1 =
            engine_revm::ForkExecutor::new(RpcStateFetcher::new(&client, BlockTag::Number(9)));
        exec1.prewarm(pool);
        let pf1 = preload_learned_then_prefetch(
            &mut exec1,
            &client,
            BlockTag::Number(9),
            &mut readsets,
            route,
            eoa,
            pool,
            U256::ZERO,
            &[],
            1_000_000,
            8,
        );
        assert!(pf1.converged, "first validation must converge: {pf1:?}");
        assert!(
            pf1.iters >= 2,
            "two-layer dependency needs >=2 blind probe layers, got {}",
            pf1.iters
        );
        assert_eq!(pf1.learned_preloaded, 0, "no learned set on first sight");
        assert!(
            readsets.get(route).is_some(),
            "read-set must be recorded after first validation"
        );

        // Second validation on a FRESH fork (simulates post-repin wipe): the learned
        // set preloads in one batch and the probe finds everything warm — zero layers.
        let mut exec2 =
            engine_revm::ForkExecutor::new(RpcStateFetcher::new(&client, BlockTag::Number(9)));
        exec2.prewarm(pool);
        let pf2 = preload_learned_then_prefetch(
            &mut exec2,
            &client,
            BlockTag::Number(9),
            &mut readsets,
            route,
            eoa,
            pool,
            U256::ZERO,
            &[],
            1_000_000,
            8,
        );
        assert!(pf2.converged, "second validation must converge: {pf2:?}");
        assert_eq!(
            pf2.iters, 0,
            "learned preload must make the probe converge with ZERO blind layers"
        );
        assert!(
            pf2.learned_preloaded >= 2,
            "both dependency-layer slots must arrive via the learned batch: {pf2:?}"
        );
        // The real call after a learned-converged prefetch is zero-fetch.
        let pre = rpc_fetch_count();
        let out = exec2.replay_tx_commit(eoa, pool, U256::ZERO, Vec::new(), 1_000_000);
        assert!(out.success, "warm replay must succeed: {}", out.reason);
        assert_eq!(rpc_fetch_count() - pre, 0, "zero lazy fetches after learned preload");
    }

    /// A definitely-closed local port (bound then released) — a connect refuses fast, so every RPC
    /// against it is a deterministic error (no 10s read timeout).
    fn closed_port_wp3() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    }

    #[test]
    fn instance_counters_round_trips_and_errors_are_per_fetcher() {
        // WP3 RED-1: two fetchers have INDEPENDENT round-trip AND error counters — bumping one never
        // moves the other. A closed endpoint ⇒ every fetch errors, so both counters move locally.
        let dead = closed_port_wp3();
        let c1 = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
        let c2 = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
        let f1 = RpcStateFetcher::new(&c1, BlockTag::Number(1));
        let f2 = RpcStateFetcher::new(&c2, BlockTag::Number(1));
        let h1 = f1.counters();
        let h2 = f2.counters();
        let _ = f1.fetch_storage([0x01; 20], U256::ZERO); // 1 round-trip, 1 error
        let _ = f1.fetch_account([0x02; 20]); // 1 round-trip, whole-batch fail → 3 errors
        assert_eq!(h1.round_trips.load(Ordering::Relaxed), 2, "this fetcher's round-trips");
        assert_eq!(h1.errors.load(Ordering::Relaxed), 4, "this fetcher's errors (1 storage + 3 account)");
        assert_eq!(h2.round_trips.load(Ordering::Relaxed), 0, "sibling round-trips untouched");
        assert_eq!(h2.errors.load(Ordering::Relaxed), 0, "sibling errors untouched");
    }

    #[test]
    fn validator_local_error_delta_unaffected_by_concurrent_advancer_error() {
        // WP3 RED-2 (deterministic barrier race — channels, not sleeps): the validator snapshots its
        // LOCAL pre-count and pauses; the advancer forces an RPC error (global RPC_FETCH_ERRORS moves +
        // the advancer's own local errors); the validator resumes and its LOCAL delta is ZERO. No
        // cross-thread contamination can fail-close an error-free candidate.
        use std::sync::mpsc;
        let dead = closed_port_wp3();
        let cval = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
        let f_val = RpcStateFetcher::new(&cval, BlockTag::Number(1));
        let val_counters = f_val.counters();
        let g_before = rpc_fetch_error_count();
        let err_pre = val_counters.errors.load(Ordering::Relaxed);
        let (tx_go, rx_go) = mpsc::channel::<()>();
        let (tx_done, rx_done) = mpsc::channel::<()>();
        std::thread::scope(|s| {
            s.spawn(move || {
                rx_go.recv().unwrap(); // wait until the validator has taken its pre-snapshot
                let cadv = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
                let f_adv = RpcStateFetcher::new(&cadv, BlockTag::Number(1));
                let _ = f_adv.fetch_account([0xAA; 20]); // forces RPC errors (global + advancer-local)
                let _ = f_adv.fetch_storage([0xBB; 20], U256::ZERO);
                tx_done.send(()).unwrap();
            });
            tx_go.send(()).unwrap(); // release the advancer only AFTER the pre-snapshot
            rx_done.recv().unwrap(); // wait for the advancer to finish erroring
        });
        let err_post = val_counters.errors.load(Ordering::Relaxed);
        assert_eq!(err_post - err_pre, 0, "validator-local error delta must be ZERO");
        assert!(
            rpc_fetch_error_count() > g_before,
            "global telemetry DID move (advancer errors) — proving the contamination the local delta ignores"
        );
    }

    #[test]
    fn validator_owned_error_moves_local_delta() {
        // WP3 RED-3: a real validator-OWNED RPC error moves its local error delta (→ fetch_error_fail_closed).
        let dead = closed_port_wp3();
        let c = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
        let f = RpcStateFetcher::new(&c, BlockTag::Number(1));
        let cn = f.counters();
        let pre = cn.errors.load(Ordering::Relaxed);
        let _ = f.fetch_storage([0x01; 20], U256::ZERO);
        assert!(
            cn.errors.load(Ordering::Relaxed) - pre > 0,
            "a validator-owned RPC error moves the validator-local delta"
        );
    }

    #[test]
    fn preload_errors_are_attributed_only_when_snapshot_precedes_them() {
        // WP3 FIX-F1 (runtime): the validator-local ERROR snapshot must precede the candidate PRELOAD.
        // Simulate a candidate whose PRELOAD RPC errors, then whose REVM-call RPC errors, against one
        // validator fetcher. A snapshot taken BEFORE the preload attributes BOTH errors (fail-close);
        // a snapshot taken AFTER the preload (the pre-FIX placement) MISSES the preload error.
        let dead = closed_port_wp3();
        let c = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
        let f = RpcStateFetcher::new(&c, BlockTag::Number(1));
        let ctr = f.counters();
        let err_pre_correct = ctr.errors.load(Ordering::Relaxed); // snapshot BEFORE the preload (FIX)
        let _ = f.fetch_storage([0x01; 20], U256::ZERO); // "preload" cold read → 1 error
        let err_pre_buggy = ctr.errors.load(Ordering::Relaxed); // snapshot AFTER the preload (pre-FIX)
        let _ = f.fetch_storage([0x02; 20], U256::ZERO); // "REVM call" cold read → 1 error
        let err_post = ctr.errors.load(Ordering::Relaxed);
        let correct_delta = err_post - err_pre_correct;
        let buggy_delta = err_post - err_pre_buggy;
        assert!(
            correct_delta >= 2,
            "a snapshot BEFORE the preload attributes both the preload + REVM errors (fail-close)"
        );
        assert!(
            buggy_delta < correct_delta,
            "a snapshot AFTER the preload MISSES the preload error — exactly the escape FIX-F1 closes"
        );
    }

    #[test]
    fn repin_retains_the_same_counter_bundle() {
        // WP3 RED-4: new_with_counters reuses the SAME bundle across a repin, so accumulated
        // round-trips + errors survive the repin.
        let dead = closed_port_wp3();
        let c = ArbitrumRpcClient::new(format!("http://127.0.0.1:{dead}"));
        let f1 = RpcStateFetcher::new(&c, BlockTag::Number(1));
        let bundle = f1.counters();
        let _ = f1.fetch_storage([0x01; 20], U256::ZERO);
        let e1 = bundle.errors.load(Ordering::Relaxed);
        assert!(e1 > 0);
        let f2 = RpcStateFetcher::new_with_counters(&c, BlockTag::Number(2), bundle.clone());
        assert!(std::sync::Arc::ptr_eq(&bundle, &f2.counters()), "repin shares the same bundle");
        let _ = f2.fetch_storage([0x02; 20], U256::ZERO);
        assert!(
            bundle.errors.load(Ordering::Relaxed) > e1,
            "the shared bundle accumulates across the repin"
        );
    }

    #[test]
    fn rpc_endpoint_local_classification() {
        for url in [
            "http://127.0.0.1:8547",
            "http://localhost:8547/rpc",
            "https://localhost",
            "http://0.0.0.0:8547",
            "http://[::1]:8547",
            "unix:/tmp/nitro.sock",
            "/tmp/nitro.sock",
            "http://nitro.local:8547",
        ] {
            assert!(super::rpc_endpoint_is_local(url), "{url} should be local");
        }

        for url in [
            "https://arb-mainnet.g.alchemy.com/v2/KEY",
            "https://arb1.arbitrum.io/rpc",
            "http://10.0.0.5:8547",
            "https://example.com",
        ] {
            assert!(!super::rpc_endpoint_is_local(url), "{url} should be remote");
        }
    }

    #[test]
    fn same_rpc_host_ignores_scheme_case_slash_and_key() {
        // WP1 ADV-F1: two endpoints on the SAME provider host are a vacuous self-crosscheck, even when
        // they differ by scheme, case, trailing slash, or an embedded API key.
        assert!(super::same_rpc_host(
            "https://arb-mainnet.g.alchemy.com/v2/KEY1",
            "http://arb-mainnet.g.alchemy.com/v2/KEY2"
        ));
        assert!(super::same_rpc_host("https://Base.LAVA.build/rpc", "https://base.lava.build/rpc/"));
        assert!(super::same_rpc_host(
            "https://x.example.com:8547/a?key=1",
            "https://x.example.com:8547/b?key=2"
        ));
        assert!(super::same_rpc_host(
            "http://user:pass@node.local:9642/feed",
            "http://node.local:9642/other"
        ));
        // Genuinely different provider hosts ⇒ an independent cross-check.
        assert!(!super::same_rpc_host(
            "https://arb1.arbitrum.io/rpc",
            "https://arb-mainnet.g.alchemy.com/v2/KEY"
        ));
        assert!(!super::same_rpc_host("https://base.lava.build/rpc", "https://quicknode.example/abc"));
        // Different port on the same host ⇒ distinct endpoint (not vacuous).
        assert!(!super::same_rpc_host("http://host.local:8547", "http://host.local:9547"));
        // FIX-4a: the scheme's DEFAULT port is canonicalized away (h == h:443 for https, h == h:80 for
        // http), and a trailing FQDN dot is stripped — all still vacuous self-crosschecks.
        assert!(super::same_rpc_host("https://h.example.com", "https://h.example.com:443"));
        assert!(super::same_rpc_host("http://h.example.com:80", "http://h.example.com"));
        assert!(super::same_rpc_host("https://h.example.com.", "https://h.example.com"));
        assert!(super::same_rpc_host("wss://node.example:443/ws", "wss://node.example/ws"));
        // A NON-default explicit port stays distinct (https default is 443, so :8547 is kept).
        assert!(!super::same_rpc_host("https://h.example.com:8547", "https://h.example.com"));
    }

    // ── [Stage 5 · MPT-verified cold tier] ──────────────────────────────────────────────────────

    use alloy_primitives::{keccak256, hex};

    // Minimal RLP builders (test-only) to assemble a real EIP-1186 proof for the mock server.
    fn min_be(v: U256) -> Vec<u8> {
        let full = v.to_be_bytes::<32>();
        let first = full.iter().position(|&b| b != 0).unwrap_or(32);
        full[first..].to_vec()
    }
    fn rlp_string(bytes: &[u8]) -> Vec<u8> {
        if bytes.len() == 1 && bytes[0] < 0x80 {
            return vec![bytes[0]];
        }
        let mut out = Vec::new();
        if bytes.len() <= 55 {
            out.push(0x80 + bytes.len() as u8);
        } else {
            let len_be = bytes.len().to_be_bytes();
            let first = len_be.iter().position(|&b| b != 0).unwrap();
            out.push(0xb7 + (len_be.len() - first) as u8);
            out.extend_from_slice(&len_be[first..]);
        }
        out.extend_from_slice(bytes);
        out
    }
    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = items.concat();
        let mut out = Vec::new();
        if body.len() <= 55 {
            out.push(0xc0 + body.len() as u8);
        } else {
            let len_be = body.len().to_be_bytes();
            let first = len_be.iter().position(|&b| b != 0).unwrap();
            out.push(0xf7 + (len_be.len() - first) as u8);
            out.extend_from_slice(&len_be[first..]);
        }
        out.extend_from_slice(&body);
        out
    }
    fn nibbles(k: B256) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        for b in k.as_slice() {
            out.push(b >> 4);
            out.push(b & 0x0f);
        }
        out
    }
    fn compact_leaf(path: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let start = if path.len() % 2 == 1 {
            out.push(0x30 | path[0]);
            1
        } else {
            out.push(0x20);
            0
        };
        let mut i = start;
        while i < path.len() {
            out.push((path[i] << 4) | path[i + 1]);
            i += 2;
        }
        out
    }
    fn leaf_node(path: &[u8], value_bytes: &[u8]) -> Vec<u8> {
        rlp_list(&[rlp_string(&compact_leaf(path)), rlp_string(value_bytes)])
    }
    fn hx(b: &[u8]) -> String {
        format!("0x{}", hex::encode(b))
    }

    /// Build a consistent single-slot proof: returns (state_root_hex, proof_json).
    fn build_good_proof(
        address: [u8; 20],
        slot: U256,
        value: U256,
        nonce: u64,
        balance: U256,
    ) -> (String, serde_json::Value) {
        let slot_key = keccak256(B256::from(slot).as_slice());
        let storage_leaf = leaf_node(&nibbles(slot_key), &rlp_string(&min_be(value)));
        let storage_root = keccak256(storage_leaf.as_slice());
        let code_hash = crate::mpt_verifier::EMPTY_CODE_HASH;
        let acct_rlp = rlp_list(&[
            rlp_string(&min_be(U256::from(nonce))),
            rlp_string(&min_be(balance)),
            rlp_string(storage_root.as_slice()),
            rlp_string(code_hash.as_slice()),
        ]);
        let addr_key = keccak256(address.as_slice());
        let account_leaf = leaf_node(&nibbles(addr_key), &acct_rlp);
        let state_root = keccak256(account_leaf.as_slice());
        let proof = serde_json::json!({
            "address": hx(&address),
            "accountProof": [hx(&account_leaf)],
            "balance": hx(&min_be(balance)),
            "codeHash": hx(code_hash.as_slice()),
            "nonce": hx(&min_be(U256::from(nonce))),
            "storageHash": hx(storage_root.as_slice()),
            "storageProof": [{
                "key": hx(&slot.to_be_bytes::<32>()),
                "value": hx(&min_be(value)),
                "proof": [hx(&storage_leaf)],
            }],
        });
        (hx(state_root.as_slice()), proof)
    }

    /// Mock JSON-RPC server answering `eth_getBlockByNumber` (stateRoot) + `eth_getProof`.
    fn spawn_proof_mock(state_root_hex: String, proof_json: serde_json::Value) -> u16 {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
                    let id = body["id"].clone();
                    let result = match body["method"].as_str().unwrap_or("") {
                        "eth_getBlockByNumber" => {
                            serde_json::json!({ "stateRoot": state_root_hex })
                        }
                        "eth_getProof" => proof_json.clone(),
                        _ => serde_json::Value::Null,
                    };
                    let reply =
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        });
        port
    }

    /// Like [`spawn_proof_mock`] but counts `eth_getProof` requests so a test can PROVE the proof read
    /// was routed to this specific (archive) endpoint. Returns `(port, getproof_hits)`.
    fn spawn_proof_mock_counting(
        state_root_hex: String,
        proof_json: serde_json::Value,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU32>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let hits_srv = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
                    let id = body["id"].clone();
                    let result = match body["method"].as_str().unwrap_or("") {
                        "eth_getBlockByNumber" => {
                            serde_json::json!({ "stateRoot": state_root_hex })
                        }
                        "eth_getProof" => {
                            hits_srv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            proof_json.clone()
                        }
                        _ => serde_json::Value::Null,
                    };
                    let reply =
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        });
        (port, hits)
    }

    /// Inner fetcher returning a fixed storage value (proxy for the cold RPC read being verified).
    struct FixedFetcher(U256);
    impl StateFetcher for FixedFetcher {
        fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, None)
        }
        fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
            self.0
        }
    }

    #[test]
    fn proof_verify_disabled_is_transparent_passthrough() {
        // verify=false ⇒ inner value passes through, ZERO proof work, no counters move. This is the
        // MIRROR_COLD_VERIFY=0 byte-identity: no RPC endpoint is even contacted.
        let client = ArbitrumRpcClient::new("http://127.0.0.1:9".to_string()); // dead endpoint
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(U256::from(123u64)), client, 100, false);
        assert_eq!(f.fetch_storage([0x11; 20], U256::from(5u64)), U256::from(123u64));
        assert_eq!(f.verify_attempts(), 0, "disabled ⇒ no verification attempted");
        assert_eq!(f.verify_failures(), 0);
    }

    #[test]
    fn proof_verify_enabled_good_proof_passes() {
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64);
        let (root_hex, proof) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        let port = spawn_proof_mock(root_hex, proof);
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        // Inner serves the SAME value the proof attests → verification passes. The mock IS the archive
        // client (A7): all proof reads route to it.
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(value), client, 100, true);
        assert_eq!(f.fetch_storage(address, slot), value);
        assert_eq!(f.verify_attempts(), 1);
        assert_eq!(f.verify_failures(), 0, "a valid proof for the fetched value verifies clean");
    }

    #[test]
    fn legacy_infallible_verify_mismatch_moves_mpt_telemetry_not_rpc_errors() {
        // WP3 RED-5: a proof failure on the legacy infallible path moves MPT telemetry (the fetcher's
        // per-instance verify_failures / MPT_VERIFY_FAILURES) — a proof failure is NOT an RPC fetch
        // error. The code no longer bumps RPC_FETCH_ERRORS from this path (removed in WP3, git-visible;
        // RED-demonstrable by restoring it). We assert the reliable, fetcher-LOCAL telemetry rather than
        // the process-global RPC_FETCH_ERRORS: that global is shared by every fetcher across the whole
        // parallel test process, so asserting its absolute value here would be racy — and the point of
        // WP3 is precisely that the candidate verdict reads the per-instance bundle, not the global.
        // (The checked path `checked_tampered_proof_and_wrong_key_fail_closed` is the real fail-closed;
        // this legacy path is off the reconcile trust path since WP1.)
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let attested = U256::from(0xc8u64);
        let (root_hex, proof) = build_good_proof(address, slot, attested, 1, U256::from(1000u64));
        let port = spawn_proof_mock(root_hex, proof);
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(U256::from(0xc9u64)), client, 100, true);
        let v = f.fetch_storage(address, slot);
        assert_eq!(v, U256::from(0xc9u64), "legacy infallible path still returns the untrusted value");
        assert_eq!(f.verify_attempts(), 1);
        assert_eq!(
            f.verify_failures(),
            1,
            "the mismatch moves the fetcher-local MPT telemetry (not an RPC fetch error)"
        );
    }

    #[test]
    fn proof_getproof_routes_to_archive_client() {
        // A7 (RPC policy): the S5 cold tier's `eth_getProof` (+ state-root anchor) must be served by
        // the fetcher's OWN archive client, never lava-base. Point the fetcher's archive client at a
        // counting mock; a verify pass must land `eth_getProof` on THAT endpoint.
        let address = [0x51u8; 20];
        let slot = U256::from(9u64);
        let value = U256::from(0x1234u64);
        let (root_hex, proof) = build_good_proof(address, slot, value, 2, U256::from(42u64));
        let (port, getproof_hits) = spawn_proof_mock_counting(root_hex, proof);
        let archive = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        // `with_archive` injects the archive client directly — no base/lava client is involved at all.
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(value), archive, 100, true);
        assert_eq!(f.fetch_storage(address, slot), value);
        assert_eq!(f.verify_attempts(), 1);
        assert_eq!(f.verify_failures(), 0, "proof served by the archive client verifies clean");
        assert!(
            getproof_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "eth_getProof must be routed to the archive-supplement client (A7)"
        );
    }

    #[test]
    fn parse_account_proof_from_json() {
        let address = [0x42u8; 20];
        let (_root, proof) = build_good_proof(address, U256::from(7u64), U256::from(200u64), 3, U256::from(555u64));
        let parsed = parse_account_proof(&proof, address).expect("parses");
        assert_eq!(parsed.address, address);
        assert_eq!(parsed.nonce, 3);
        assert_eq!(parsed.balance, U256::from(555u64));
        assert_eq!(parsed.storage_proof.len(), 1);
        assert_eq!(parsed.storage_proof[0].key, U256::from(7u64));
        assert_eq!(parsed.storage_proof[0].value, U256::from(200u64));
        assert_eq!(parsed.account_proof.len(), 1);
    }

    // ── [WP0 · A6] pre-allocation input bounds + exact requested-key cardinality ─────────────────

    #[test]
    fn bounded_parse_rejects_oversized_arrays_hex_and_aggregate() {
        // WP0 RED-6: each input limit rejects BEFORE the large allocation/hex decode, with a typed
        // InputLimitExceeded. The pure-walker caps are the SECOND layer; these are the first.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let (_root, good) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        parse_account_proof_bounded(&good, address, &[slot], 100).expect("good proof parses bounded");

        // (a) oversized accountProof node array → rejected before decode.
        let mut big_acct = good.clone();
        let node = big_acct["accountProof"][0].clone();
        big_acct["accountProof"] =
            serde_json::Value::Array(vec![node; MAX_ACCOUNT_PROOF_NODES + 1]);
        assert_eq!(
            parse_account_proof_bounded(&big_acct, address, &[slot], 100).unwrap_err().kind,
            CanonicalFetchErrorKind::InputLimitExceeded,
            "oversized accountProof array must be rejected pre-allocation"
        );

        // (b) oversized storageProof entry array → rejected before iteration.
        let mut big_sp = good.clone();
        let sp0 = big_sp["storageProof"][0].clone();
        big_sp["storageProof"] = serde_json::Value::Array(vec![sp0; MAX_STORAGE_ENTRIES + 1]);
        assert_eq!(
            parse_account_proof_bounded(&big_sp, address, &[slot], 100).unwrap_err().kind,
            CanonicalFetchErrorKind::InputLimitExceeded,
            "oversized storageProof array must be rejected pre-allocation"
        );

        // (c) oversized single node hex string → rejected before hex decode.
        let mut big_hex = good.clone();
        let huge = format!("0x{}", "ab".repeat(MAX_NODE_DECODED_BYTES + 1));
        big_hex["accountProof"] = serde_json::json!([huge]);
        assert_eq!(
            parse_account_proof_bounded(&big_hex, address, &[slot], 100).unwrap_err().kind,
            CanonicalFetchErrorKind::InputLimitExceeded,
            "over-long node hex string must be rejected before decode"
        );

        // (d) aggregate decoded bytes across many in-bounds nodes → rejected on the running total.
        let mut agg = good.clone();
        let node_hex = format!("0x{}", "cd".repeat(MAX_NODE_DECODED_BYTES));
        let n = MAX_AGGREGATE_DECODED_BYTES / MAX_NODE_DECODED_BYTES + 1;
        assert!(n <= MAX_ACCOUNT_PROOF_NODES, "test setup: n within per-list node cap");
        agg["accountProof"] =
            serde_json::Value::Array(vec![serde_json::Value::String(node_hex); n]);
        assert_eq!(
            parse_account_proof_bounded(&agg, address, &[slot], 100).unwrap_err().kind,
            CanonicalFetchErrorKind::InputLimitExceeded,
            "aggregate decoded-bytes cap must reject on the running total"
        );
    }

    #[test]
    fn bounded_parse_enforces_exact_requested_key_cardinality() {
        // WP0 RED-4: no `.find`. The storageProof array must be exactly the requested keys, in order —
        // a wrong key, a missing entry, a duplicate, or an extra entry all fail (no first-match).
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let (_root, good) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        parse_account_proof_bounded(&good, address, &[slot], 100).expect("requested key present once");

        // Wrong key: entry attests a DIFFERENT slot than requested.
        let mut wrong = good.clone();
        wrong["storageProof"][0]["key"] = serde_json::json!(hx(&U256::from(8u64).to_be_bytes::<32>()));
        assert_eq!(
            parse_account_proof_bounded(&wrong, address, &[slot], 100).unwrap_err().kind,
            CanonicalFetchErrorKind::InvalidStorageProof,
            "a storageProof for a different key than requested must be rejected"
        );

        // Missing: empty storageProof array for a one-key request.
        let mut missing = good.clone();
        missing["storageProof"] = serde_json::json!([]);
        assert!(
            parse_account_proof_bounded(&missing, address, &[slot], 100).is_err(),
            "a missing storage proof for the requested key must be rejected"
        );

        // Duplicate: the same requested key twice — cardinality (2 != 1) fails; no first-match.
        let mut dup = good.clone();
        let sp0 = dup["storageProof"][0].clone();
        dup["storageProof"] = serde_json::Value::Array(vec![sp0.clone(), sp0]);
        assert!(
            parse_account_proof_bounded(&dup, address, &[slot], 100).is_err(),
            "a duplicated requested key must be rejected (no first-.find selection)"
        );
    }

    /// Mock JSON-RPC server whose response body is padded past a byte size (oversized-body vector).
    fn spawn_big_body_mock(pad_bytes: usize) -> u16 {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
                    let id = body["id"].clone();
                    // A valid envelope whose `result` string alone exceeds `pad_bytes`.
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": "0x".to_string() + &"ab".repeat(pad_bytes),
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        });
        port
    }

    #[test]
    fn call_capped_rejects_oversized_body_before_parse() {
        // WP0 RED-6 (HTTP body): the raw body is bounded before JSON parse. A body larger than the cap
        // is a typed BodyTooLarge, not a parse of an unbounded allocation.
        let port = spawn_big_body_mock(4096); // ~8 KiB result string
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let err = client
            .call_capped("eth_getProof", serde_json::json!([]), 1024)
            .unwrap_err();
        assert!(
            matches!(err, engine_rpc::RpcError::BodyTooLarge { limit } if limit == 1024),
            "oversized body must be rejected before parse, got {err:?}"
        );
        // A cap comfortably above the body still parses fine.
        let ok = client.call_capped("eth_getProof", serde_json::json!([]), 1 << 20);
        assert!(ok.is_ok(), "an in-bounds body still parses: {ok:?}");
    }

    #[test]
    fn fetch_account_proof_bounded_maps_body_cap_to_input_limit() {
        // WP0 RED-6 end-to-end: an oversized eth_getProof body surfaces as a typed InputLimitExceeded
        // from the checked fetch path (the classification the reconcile seam will branch on).
        let port = spawn_big_body_mock(MAX_PROOF_BODY_BYTES); // result alone > MAX_PROOF_BODY_BYTES
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let err = fetch_account_proof_bounded(&client, [0x42; 20], &[U256::from(7u64)], 100)
            .unwrap_err();
        assert_eq!(
            err.kind,
            CanonicalFetchErrorKind::InputLimitExceeded,
            "an oversized proof body must fail closed as InputLimitExceeded, got {err:?}"
        );
    }

    // ── [WP0 · A5/A3/A2] independently-anchored, checked, proven-value cold read ─────────────────

    /// A block header JSON with the four canonical-identity fields the anchor cross-check reads.
    fn header_json(number: u64, hash: &str, parent: &str, state_root: &str) -> serde_json::Value {
        serde_json::json!({
            "number": format!("0x{number:x}"),
            "hash": hash,
            "parentHash": parent,
            "stateRoot": state_root,
        })
    }

    /// Mock serving a FULL header for `eth_getBlockByNumber` and `proof` for `eth_getProof`.
    fn spawn_full_mock(header: serde_json::Value, proof: serde_json::Value) -> u16 {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
                    let id = body["id"].clone();
                    let result = match body["method"].as_str().unwrap_or("") {
                        "eth_getBlockByNumber" => header.clone(),
                        "eth_getProof" => proof.clone(),
                        _ => serde_json::Value::Null,
                    };
                    let reply =
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        });
        port
    }

    /// Mock whose header path is healthy but whose `eth_getProof` path accepts the request and then
    /// disconnects without an HTTP response. This isolates proof-source transport unavailability from
    /// header availability and independent-anchor disagreement.
    fn spawn_header_then_drop_proof_mock(
        header: serde_json::Value,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU32>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let proof_hits = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let proof_hits_server = std::sync::Arc::clone(&proof_hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
                    let id = body["id"].clone();
                    match body["method"].as_str().unwrap_or("") {
                        "eth_getProof" => {
                            proof_hits_server.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let _ = sock.shutdown(std::net::Shutdown::Both);
                            break;
                        }
                        "eth_getBlockByNumber" => {
                            let reply = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": header,
                            })
                            .to_string();
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                reply.len(),
                                reply
                            );
                            let _ = sock.write_all(resp.as_bytes());
                            break;
                        }
                        _ => break,
                    }
                }
            }
        });
        (port, proof_hits)
    }

    /// [WP0 F2 sweep] Stateful mock: serves `header_a` to the FIRST `eth_getBlockByNumber` and
    /// `header_b` to every later one — modelling a chain REORG that lands between the primary and the
    /// cross-check header fetch during anchor resolution. The request counter is shared across
    /// connections (primary and crosscheck use separate clients ⇒ separate sockets).
    fn spawn_reorging_header_mock(
        header_a: serde_json::Value,
        header_b: serde_json::Value,
    ) -> u16 {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};
        use std::sync::Arc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let hits = Arc::clone(&hits);
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..body_start + clen]).unwrap();
                    let id = body["id"].clone();
                    let result = match body["method"].as_str().unwrap_or("") {
                        "eth_getBlockByNumber" => {
                            if hits.fetch_add(1, AtOrd::SeqCst) == 0 {
                                header_a.clone()
                            } else {
                                header_b.clone()
                            }
                        }
                        _ => serde_json::Value::Null,
                    };
                    let reply =
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        });
        port
    }

    /// A port that is bound then immediately released — a connect there refuses fast (dead endpoint).
    fn closed_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    }

    fn client_at(port: u16) -> ArbitrumRpcClient {
        ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"))
    }

    #[test]
    fn checked_verified_inclusion_with_crosscheck_returns_proven_value() {
        // WP0 EXIT GATE (positive): with BOTH a cross-checked exact-block anchor AND a complete valid
        // inclusion proof, the checked API returns the PROVEN storageProof.value (A3) — NOT the inner
        // infallible read. Two independent providers agree on the header; the proof verifies to it.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        let header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        let archive = client_at(spawn_full_mock(header.clone(), proof));
        let cc = client_at(spawn_full_mock(header, serde_json::Value::Null));
        // Inner serves 999 — the checked path must ignore it and return the proven 0xc8.
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(U256::from(999u64)), archive, block, true)
            .with_crosscheck(cc);
        assert_eq!(
            f.fetch_storage_verified(address, slot),
            Ok(value),
            "checked path returns the proven value, anchored + cross-checked"
        );
        assert_eq!(f.verify_attempts(), 1);
        assert_eq!(f.verify_failures(), 0);
        // The trait hook delegates to the same checked path.
        assert_eq!(f.fetch_storage_checked(address, slot), Ok(value));
    }

    #[test]
    fn checked_valid_non_inclusion_returns_zero() {
        // WP0 RED-2 (checked level): a walked proof of absence (zero slot) under a cross-checked anchor
        // returns Ok(0) — the checked path handles non-inclusion, it does not require a present value.
        let address = [0x51u8; 20];
        let present = U256::from(7u64);
        // Find an absent slot whose hashed path diverges from `present` at nibble 0.
        let present_key = keccak256(B256::from(present).as_slice());
        let mut absent = 8u64;
        loop {
            let k = keccak256(B256::from(U256::from(absent)).as_slice());
            if nibbles(k)[0] != nibbles(present_key)[0] {
                break;
            }
            absent += 1;
        }
        let absent_slot = U256::from(absent);
        // Build a storage trie with only `present` in it; query `absent` → divergent-leaf miss → 0.
        let storage_leaf = leaf_node(&nibbles(present_key), &rlp_string(&min_be(U256::from(9u64))));
        let storage_root = keccak256(storage_leaf.as_slice());
        let code_hash = crate::mpt_verifier::EMPTY_CODE_HASH;
        let acct_rlp = rlp_list(&[
            rlp_string(&min_be(U256::from(1u64))),
            rlp_string(&min_be(U256::from(1000u64))),
            rlp_string(storage_root.as_slice()),
            rlp_string(code_hash.as_slice()),
        ]);
        let addr_key = keccak256(address.as_slice());
        let account_leaf = leaf_node(&nibbles(addr_key), &acct_rlp);
        let state_root = keccak256(account_leaf.as_slice());
        let proof = serde_json::json!({
            "address": hx(&address),
            "accountProof": [hx(&account_leaf)],
            "balance": hx(&min_be(U256::from(1000u64))),
            "codeHash": hx(code_hash.as_slice()),
            "nonce": hx(&min_be(U256::from(1u64))),
            "storageHash": hx(storage_root.as_slice()),
            "storageProof": [{
                "key": hx(&absent_slot.to_be_bytes::<32>()),
                "value": "0x0",
                "proof": [hx(&storage_leaf)],
            }],
        });
        let block = 200u64;
        let header = header_json(block, &hx(&[1u8; 32]), &hx(&[2u8; 32]), &hx(state_root.as_slice()));
        let archive = client_at(spawn_full_mock(header.clone(), proof));
        let cc = client_at(spawn_full_mock(header, serde_json::Value::Null));
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(U256::from(999u64)), archive, block, true)
            .with_crosscheck(cc);
        assert_eq!(
            f.fetch_storage_verified(address, absent_slot),
            Ok(U256::ZERO),
            "a walked non-inclusion proof yields the proven zero"
        );
    }

    #[test]
    fn checked_without_independent_source_is_untrusted_anchor() {
        // WP0 A5: a proof provider serving both the header and the proof is NOT its own root of trust.
        // With no independent cross-check source, the checked path fails closed with UntrustedAnchor —
        // we never claim forked-provider protection we did not wire.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        let header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        let archive = client_at(spawn_full_mock(header, proof));
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(U256::from(1u64)), archive, block, true);
        let e = f.fetch_storage_verified(address, slot).unwrap_err();
        assert_eq!(
            e.kind,
            CanonicalFetchErrorKind::UntrustedAnchor,
            "no independent anchor ⇒ UntrustedAnchor, got {e:?}"
        );
        assert_eq!(f.verify_failures(), 1);
    }

    #[test]
    fn checked_anchor_disagreement_is_untrusted_anchor() {
        // WP0 RED-7: header sources disagree on state root (or hash) ⇒ UntrustedAnchor. A reorg
        // between the two header lookups presents as exactly this disagreement.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        let archive_header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        // Cross-check provider reports a DIFFERENT state root for the same block.
        let cc_header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &hx(&[0x99; 32]));
        let archive = client_at(spawn_full_mock(archive_header, proof));
        let cc = client_at(spawn_full_mock(cc_header, serde_json::Value::Null));
        let f = ProofVerifyingFetcher::with_archive(FixedFetcher(U256::from(1u64)), archive, block, true)
            .with_crosscheck(cc);
        let e = f.fetch_storage_verified(address, slot).unwrap_err();
        assert_eq!(
            e.kind,
            CanonicalFetchErrorKind::UntrustedAnchor,
            "state-root disagreement ⇒ UntrustedAnchor, got {e:?}"
        );

        // Disagreement on block HASH (same root) is equally untrusted.
        let (root2, proof2) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        let ah = header_json(block, &hx(&[0x01; 32]), &hx(&[0xBB; 32]), &root2);
        let ch = header_json(block, &hx(&[0x02; 32]), &hx(&[0xBB; 32]), &root2);
        let f2 = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(1u64)),
            client_at(spawn_full_mock(ah, proof2)),
            block,
            true,
        )
        .with_crosscheck(client_at(spawn_full_mock(ch, serde_json::Value::Null)));
        assert_eq!(
            f2.fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::UntrustedAnchor,
            "block-hash disagreement ⇒ UntrustedAnchor"
        );
    }

    #[test]
    fn checked_reorg_between_header_lookups_is_untrusted_anchor() {
        // [WP0 F2 sweep] The header MUTATES between the primary and cross-check fetch — a reorg during
        // anchor resolution. A single stateful endpoint returns header A on the primary fetch and a
        // reorged header B on the cross-check fetch; the two-provider agreement must catch the change and
        // fail closed (UntrustedAnchor), never returning a value proven against a now-orphaned root.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let block = 100u64;
        let (root_hex, _proof) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        let header_a = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        // Same block number, but a DIFFERENT hash (and root) after the reorg.
        let header_b = header_json(block, &hx(&[0xCC; 32]), &hx(&[0xBB; 32]), &hx(&[0x99; 32]));
        let port = spawn_reorging_header_mock(header_a, header_b);
        // The SAME stateful endpoint is both the primary (archive) and the independent cross-check, so
        // the primary fetch observes A (call 1) and the cross-check observes the reorged B (call 2).
        let f = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(1u64)),
            client_at(port),
            block,
            true,
        )
        .with_crosscheck(client_at(port));
        assert_eq!(
            f.fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::UntrustedAnchor,
            "a reorg between the two header lookups ⇒ UntrustedAnchor (fail-closed)"
        );
    }

    #[test]
    fn checked_wrong_block_and_null_and_dead_header_are_typed_errors() {
        // WP0 RED-8: a header for the wrong block ⇒ UntrustedAnchor; a null header ⇒ HeaderUnavailable;
        // a dead endpoint (timeout/refused) ⇒ HeaderUnavailable. Never a value.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));

        // Wrong block number in the primary header.
        let wrong = header_json(block + 1, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        let f = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(1u64)),
            client_at(spawn_full_mock(wrong, proof.clone())),
            block,
            true,
        )
        .with_crosscheck(client_at(spawn_full_mock(
            header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex),
            serde_json::Value::Null,
        )));
        assert_eq!(
            f.fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::UntrustedAnchor,
            "wrong block number ⇒ UntrustedAnchor"
        );

        // Null header from the archive.
        let f_null = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(1u64)),
            client_at(spawn_full_mock(serde_json::Value::Null, proof.clone())),
            block,
            true,
        )
        .with_crosscheck(client_at(spawn_full_mock(
            header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex),
            serde_json::Value::Null,
        )));
        assert_eq!(
            f_null.fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::HeaderUnavailable,
            "null header ⇒ HeaderUnavailable"
        );

        // Dead archive endpoint (connection refused).
        let f_dead = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(1u64)),
            client_at(closed_port()),
            block,
            true,
        )
        .with_crosscheck(client_at(closed_port()));
        assert_eq!(
            f_dead.fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::HeaderUnavailable,
            "dead header endpoint ⇒ HeaderUnavailable"
        );
    }

    #[test]
    fn checked_unavailable_crosscheck_source_is_untrusted_anchor() {
        // WP0 A5: an UNAVAILABLE independent source cannot establish trust ⇒ UntrustedAnchor (not a
        // pass-through of the primary's header).
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        let header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        let f = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(1u64)),
            client_at(spawn_full_mock(header, proof)),
            block,
            true,
        )
        .with_crosscheck(client_at(closed_port())); // independent source down
        assert_eq!(
            f.fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::UntrustedAnchor,
            "unavailable independent source ⇒ UntrustedAnchor"
        );
    }

    #[test]
    fn checked_proof_source_disconnect_is_proof_unavailable_after_valid_anchor() {
        // Gate-2 availability fault: both header sources are healthy and agree, then the ARCHIVE
        // proof source accepts eth_getProof and drops the connection without a response. This keeps
        // the failure distinct from HeaderUnavailable and UntrustedAnchor and must return no value.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let block = 100u64;
        let (root_hex, _proof) =
            build_good_proof(address, slot, U256::from(1u64), 1, U256::from(1u64));
        let header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        let (archive_port, proof_hits) = spawn_header_then_drop_proof_mock(header.clone());
        let archive = client_at(archive_port);
        let crosscheck = client_at(spawn_full_mock(header, serde_json::Value::Null));
        let f = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(999u64)),
            archive,
            block,
            true,
        )
        .with_crosscheck(crosscheck);

        let err = f.fetch_storage_verified(address, slot).unwrap_err();
        assert_eq!(
            err.kind,
            CanonicalFetchErrorKind::ProofUnavailable,
            "a dropped proof response after a valid two-source anchor must be ProofUnavailable"
        );
        assert_eq!(f.verify_attempts(), 1);
        assert_eq!(f.verify_failures(), 1);
        assert_eq!(
            proof_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the archive accepted exactly one eth_getProof request before disconnecting"
        );
    }

    #[test]
    fn checked_tampered_proof_and_wrong_key_fail_closed() {
        // WP0 RED-3/RED-4 (checked level): with a valid cross-checked anchor, a tampered account node,
        // a tampered storage value, and a wrong requested key each fail closed with a typed error and
        // NO value.
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64);
        let block = 100u64;
        let mk = |proof: serde_json::Value, root_hex: String| {
            let header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
            ProofVerifyingFetcher::with_archive(
                FixedFetcher(value),
                client_at(spawn_full_mock(header.clone(), proof)),
                block,
                true,
            )
            .with_crosscheck(client_at(spawn_full_mock(header, serde_json::Value::Null)))
        };

        // Tampered storage value: JSON value disagrees with what the leaf encodes.
        let (root_hex, mut bad_val) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        bad_val["storageProof"][0]["value"] = serde_json::json!(hx(&U256::from(0xc9u64).to_be_bytes::<32>()));
        assert_eq!(
            mk(bad_val, root_hex).fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::InvalidStorageProof,
            "storage value the proof does not attest ⇒ InvalidStorageProof"
        );

        // Tampered account node: corrupt the account leaf ⇒ hash chain to the anchored root breaks.
        let (root_hex2, mut bad_acct) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        let node = bad_acct["accountProof"][0].as_str().unwrap().to_string();
        let mut corrupt: Vec<u8> = engine_rpc::client::decode_hex_with_prefix(&node).unwrap();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        bad_acct["accountProof"][0] = serde_json::json!(hx(&corrupt));
        assert_eq!(
            mk(bad_acct, root_hex2).fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::InvalidAccountProof,
            "corrupted account node ⇒ InvalidAccountProof"
        );

        // Wrong requested key: the storageProof attests a DIFFERENT slot than requested.
        let (root_hex3, mut wrong_key) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        wrong_key["storageProof"][0]["key"] = serde_json::json!(hx(&U256::from(8u64).to_be_bytes::<32>()));
        assert_eq!(
            mk(wrong_key, root_hex3).fetch_storage_verified(address, slot).unwrap_err().kind,
            CanonicalFetchErrorKind::InvalidStorageProof,
            "a proof for a different key than requested ⇒ InvalidStorageProof"
        );
    }

    #[test]
    fn checked_feature_off_returns_inner_value_with_zero_rpc() {
        // WP0 feature-off identity at the checked-API level: verify=false ⇒ Ok(inner value), and NO
        // proof/header RPC is issued (a dead endpoint is never contacted). This is the checked-path
        // analogue of MIRROR_COLD_VERIFY=0 (the reconcile wiring additionally constructs NO adapter).
        let f = ProofVerifyingFetcher::with_archive(
            FixedFetcher(U256::from(123u64)),
            client_at(closed_port()),
            100,
            false,
        )
        .with_crosscheck(client_at(closed_port()));
        assert_eq!(f.fetch_storage_verified([0x11; 20], U256::from(5u64)), Ok(U256::from(123u64)));
        assert_eq!(f.fetch_storage_checked([0x11; 20], U256::from(5u64)), Ok(U256::from(123u64)));
        assert_eq!(f.verify_attempts(), 0, "feature-off ⇒ no verification, no RPC");
    }

    // ── [WP1] end-to-end: a REAL ProofVerifyingFetcher through the checked reconcile ─────────────

    #[test]
    fn wp1_cold_verify_healthy_reconcile_is_publishable_e2e() {
        // WP1 exit gate (MIRROR_COLD_VERIFY=1 healthy path): a real ProofVerifyingFetcher (proven value
        // vs a cross-checked anchor) driving reconcile_block_checked over a matching mirror ⇒ clean,
        // complete, publishable — the end-to-end healthy close must still work.
        use crate::mirror_journal::{MirrorJournal, ReconcileMode};
        use crate::state_mirror::StateMirror;
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        let header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        let archive = client_at(spawn_full_mock(header.clone(), proof));
        let cc = client_at(spawn_full_mock(header, serde_json::Value::Null));
        let fetcher = ProofVerifyingFetcher::with_archive(FixedFetcher(value), archive, block, true)
            .with_crosscheck(cc);

        let mirror = StateMirror::new();
        let mut journal = MirrorJournal::new(8);
        // The mirror holds exactly the proven value (a caught-up advancer).
        journal.record_and_apply(
            &mirror,
            block,
            [1u8; 32],
            &[engine_revm::ChangedSlot { address, slot, original: value, present: value }],
        );
        let out = journal.reconcile_block_checked(&mirror, block, &fetcher, ReconcileMode::Logs, 64, None);
        assert!(out.complete && out.publishable, "healthy proven reconcile is publishable: {out:?}");
        assert_eq!(out.checked, 1);
        assert_eq!(out.canonical_failures, 0);
        assert!(journal.debt().is_empty());
    }

    #[test]
    fn wp1_cold_verify_untrusted_anchor_reconcile_fails_closed_e2e() {
        // WP1 exit gate (fail-closed): a real ProofVerifyingFetcher whose independent anchor DISAGREES
        // ⇒ UntrustedAnchor ⇒ reconcile_block_checked performs no mutation, records debt, and is not
        // publishable — end to end, with a genuine proof fetcher.
        use crate::mirror_journal::{MirrorJournal, ReconcileMode};
        use crate::state_mirror::StateMirror;
        let address = [0x42u8; 20];
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64);
        let block = 100u64;
        let (root_hex, proof) = build_good_proof(address, slot, value, 1, U256::from(1000u64));
        let archive_header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &root_hex);
        // Independent source reports a DIFFERENT state root ⇒ UntrustedAnchor.
        let cc_header = header_json(block, &hx(&[0xAA; 32]), &hx(&[0xBB; 32]), &hx(&[0x99; 32]));
        let archive = client_at(spawn_full_mock(archive_header, proof));
        let cc = client_at(spawn_full_mock(cc_header, serde_json::Value::Null));
        let fetcher = ProofVerifyingFetcher::with_archive(FixedFetcher(value), archive, block, true)
            .with_crosscheck(cc);

        let mirror = StateMirror::new();
        let mut journal = MirrorJournal::new(8);
        journal.record_and_apply(
            &mirror,
            block,
            [2u8; 32],
            &[engine_revm::ChangedSlot { address, slot, original: value, present: value }],
        );
        let before = mirror.get_storage(address, slot);
        let out = journal.reconcile_block_checked(&mirror, block, &fetcher, ReconcileMode::Logs, 64, None);
        assert!(!out.complete && !out.publishable, "untrusted anchor ⇒ fail closed: {out:?}");
        assert_eq!(out.canonical_failures, 1);
        assert!(!journal.debt().is_empty(), "the affected footprint enters debt");
        assert_eq!(mirror.get_storage(address, slot), before, "no mirror mutation on a failed close");
    }

    #[test]
    fn parse_receipt_status_maps_all_cases() {
        assert_eq!(
            parse_receipt_status(&serde_json::json!({"status":"0x1"})),
            ReceiptStatus::Success
        );
        assert_eq!(
            parse_receipt_status(&serde_json::json!({"status":"0x0"})),
            ReceiptStatus::Reverted
        );
        assert_eq!(parse_receipt_status(&serde_json::Value::Null), ReceiptStatus::Absent);
        assert_eq!(
            parse_receipt_status(&serde_json::json!({"cumulativeGasUsed":"0x1"})),
            ReceiptStatus::Absent,
            "a receipt without a status field is treated as absent"
        );
    }
}
