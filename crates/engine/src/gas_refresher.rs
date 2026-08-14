//! Plan §13 — background gas / L1-data-fee refresh loop.
//!
//! WI-13 (closes FL-14): each pass now runs ONE whole-snapshot, one-block gas-authority
//! transaction (`engine_gas::authority::refresh_gas_authority_once`) instead of independent
//! `Latest` reads with fresh-on-failure defaults:
//!
//! * every fee-critical field (`getPricesInWeiWithAggregator`,
//!   `getPricesInArbGasWithAggregator`, `getMinimumGasPrice`, `getL1BaseFeeEstimate`) is
//!   fetched at ONE EIP-1898 block hash through a single Multicall3 `aggregate3` with
//!   `allowFailure=false`, plus a `getBlockNumber()` binding call proving the batch ran at
//!   that block;
//! * a COMPLETE fetch publishes the typed `GasAuthoritySnapshot` AND the legacy
//!   `GasSnapshot` (same ArcSwap read model, Brotli L1 path and volatility margin as
//!   before) with the LIVE `tokenGasUnits` — the old hardcoded `16` is retired;
//! * a FAILED/partial fetch records health evidence ONLY: the previous snapshot keeps its
//!   value AND its age (the existing `load_fresh` staleness gate + the authority hard TTL
//!   decline it as it ages out). The old behaviour — the L1 leg substituting a fresh
//!   100,000-wei floor (~30x under real L1 cost) with a fresh `captured_at_ms` and no
//!   invalidity bit — is DEAD. No fresh-on-failure, no floor, no synthetic volatility
//!   sample.
//! * SOLE WRITER (fix round, codex NEEDS-FIX-FL14): this thread is the ONLY writer of gas
//!   fee bytes and freshness in the running engine. The pipeline's legacy `engine-bg` gas
//!   loop — which republished the legacy snapshot with a fresh `captured_at_ms` every ~1s
//!   and thereby masked a persistently-failing authority stream — is REMOVED; contract
//!   pinned by `tests/wi13_gas_sole_writer_e2e.rs`.
//!
//! L2/L1 legs stay disjoint in the profit model exactly as before (no double-count,
//! verified 2026-06-27): `l2_gas_price_wei` (now the block-bound per-ArbGas total price —
//! same 0.02 gwei-floored quantity `eth_gasPrice` returned) multiplies pure-L2 gas units,
//! and the L1 term is added once in `canonical_net_profit` via `l1_data_fee_wei`
//! (`l1_base_fee_per_gas × tokenGasUnits × brotli(calldata_bytes)`).
//!
//! DEFERRED (critical-path / WI-14): binding `gas_generation` into `PreparedSubmission` at
//! prepare time and the exact-generation recheck at submit consume
//! `GasOracle::submit_authority_view()` / `usable_authority()`; pipeline.rs is NOT wired
//! here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use engine_gas::authority::{
    refresh_gas_authority_once, AuthorityCall, CanonicalHeader, GasAuthorityConfig,
    GasAuthorityTransport, GasTransportError, RefreshOutcome,
};
use engine_gas::{now_ms, GasOracle};
use engine_rpc::client::BlockTag;
use engine_rpc::multicall::{aggregate3, Call3};
use engine_rpc::ArbitrumRpcClient;
use tracing::{info, warn};

/// Refresh cadence — Arbitrum seals an L2 block ~every 250 ms; polling every 250 ms keeps the
/// snapshot at most ~one block stale, well under the tightened `thresholds.gas_max_age_ms` = 500 ms
/// so the submit-path stale-gas guard's worst case is a single missed refresh.
const REFRESH_INTERVAL_MS: u64 = 250;

/// Frozen fee-aggregator identity for the `WithAggregator` price calls, from
/// `GAS_FEE_AGGREGATOR` (`0x` + 40 hex). Defaults to the zero address (ArbOS resolves the
/// default aggregator pricing) — binding the aggregator from runtime policy + run manifest
/// is the WI-14 / critical-path integration.
fn frozen_aggregator_from_env() -> [u8; 20] {
    let Some(raw) = std::env::var("GAS_FEE_AGGREGATOR").ok() else {
        return [0u8; 20];
    };
    let hex = raw.trim().trim_start_matches("0x");
    let mut out = [0u8; 20];
    if hex.len() == 40 && faster_hex::hex_decode(hex.as_bytes(), &mut out).is_ok() {
        out
    } else {
        warn!("GAS_FEE_AGGREGATOR is not 0x+40-hex; using the zero-address default");
        [0u8; 20]
    }
}

/// Monotonic nanoseconds since process start (authority capture/TTL clock — never wall).
fn mono_ns() -> u64 {
    static ANCHOR: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    ANCHOR.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Production [`GasAuthorityTransport`]: ONE `ArbitrumRpcClient` serves the whole attempt
/// (header + aggregate). A different endpoint means a different transport value, so provider
/// failover restarts the whole snapshot by construction.
struct RpcGasAuthorityTransport<'a> {
    client: &'a ArbitrumRpcClient,
    multicall3: [u8; 20],
}

impl GasAuthorityTransport for RpcGasAuthorityTransport<'_> {
    fn provider_id(&self) -> u16 {
        // Single hot endpoint today; provider ids become meaningful with WI-13's full
        // multi-provider failover in the critical-path integration.
        0
    }

    fn canonical_header(&self) -> Result<CanonicalHeader, GasTransportError> {
        let (number, hash) = self
            .client
            .block_header(BlockTag::Latest)
            .map_err(|e| GasTransportError::Rpc(e.to_string()))?;
        Ok(CanonicalHeader { number, hash })
    }

    fn aggregate3_at_hash(
        &self,
        block_hash: [u8; 32],
        calls: &[AuthorityCall],
    ) -> Result<Vec<Vec<u8>>, GasTransportError> {
        let rpc_calls: Vec<Call3> = calls
            .iter()
            .map(|c| Call3 {
                target: c.target,
                allow_failure: c.allow_failure,
                call_data: c.call_data.clone(),
            })
            .collect();
        let results = aggregate3(
            self.client,
            &self.multicall3,
            &rpc_calls,
            BlockTag::Hash(block_hash),
        )
        .map_err(|e| GasTransportError::Rpc(e.to_string()))?;
        // allowFailure=false: an inner failure reverts the whole aggregate; a per-call
        // `success=false` here would mean a decode/contract anomaly — fail the attempt.
        results
            .into_iter()
            .map(|r| {
                if r.success {
                    Ok(r.return_data)
                } else {
                    Err(GasTransportError::Rpc("inner call failed".into()))
                }
            })
            .collect()
    }
}

/// Launch the gas refresher on a dedicated background thread. Owns its own shutdown
/// flag so the engine can join it cleanly on Ctrl+C. The thread name is
/// `engine-gas-refresher` so `perf` / `top -H` can spot it.
pub fn spawn_gas_refresher(
    rpc_url: String,
    oracle: Arc<GasOracle>,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>> {
    let handle = thread::Builder::new()
        .name("engine-gas-refresher".into())
        .spawn(move || {
            // RPC split: gas snapshots feed the submit profit floor — hot endpoint.
            let client = ArbitrumRpcClient::new(engine_rpc::hot_rpc_url().unwrap_or(rpc_url));
            let cfg = GasAuthorityConfig::arbitrum_one(frozen_aggregator_from_env());
            debug_assert_eq!(
                cfg.multicall3,
                engine_rpc::multicall::MULTICALL3_ARBITRUM,
                "authority Multicall3 address must match the engine-rpc deployment constant"
            );
            // §3.4 amended constant rule: the envelope pad is POLICY-BOUND config, logged at
            // boot as operator evidence; every published snapshot also carries the value
            // actually used (`GasSnapshot::static_overhead_bytes`). Safe by direction — it
            // over-states a subtracted cost.
            info!(
                legacy_static_overhead_bytes = cfg.legacy_static_overhead_bytes,
                default_aggregator = cfg.frozen_aggregator == [0u8; 20],
                "gas authority boot policy (sole gas writer; pad bound into every published snapshot)"
            );
            // Pre-warm: do one synchronous refresh at boot so the very first dirty
            // event sees a real snapshot, not the default.
            if let Err(e) = refresh_once(&client, &cfg, &oracle) {
                warn!(?e, "gas refresher: initial refresh failed; oracle will use default until next attempt");
                crate::diag::report(
                    crate::diag::Diag::GasRefreshFail,
                    &format!("initial refresh failed: {e}"),
                );
            }
            let mut last_attempt = Instant::now();
            // Codex 2026-07-10 (observed live the same day): a rate-limited provider turned the
            // fixed 250 ms cadence into a self-inflicted 429 storm (1,000+ consecutive failures
            // in minutes) that ALSO starved every other consumer of the same endpoint. Back off
            // exponentially on consecutive failures (cap 8 s), reset on the first success —
            // the stale-gas submit guard already fails closed while the snapshot ages.
            let mut consecutive_failures: u32 = 0;
            while !shutdown.load(Ordering::Acquire) {
                let interval_ms = REFRESH_INTERVAL_MS
                    .saturating_mul(1u64 << consecutive_failures.min(5))
                    .min(8_000);
                if last_attempt.elapsed() < Duration::from_millis(interval_ms) {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                last_attempt = Instant::now();
                match refresh_once(&client, &cfg, &oracle) {
                    Ok(()) => consecutive_failures = 0,
                    Err(e) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        if consecutive_failures <= 3 || consecutive_failures.is_multiple_of(64) {
                            warn!(
                                ?e,
                                consecutive_failures,
                                backoff_ms = interval_ms,
                                "gas refresher: refresh failed; oracle keeps last snapshot AND its age (no floor)"
                            );
                        }
                        crate::diag::report(
                            crate::diag::Diag::GasRefreshFail,
                            &format!("refresh failed: {e}"),
                        );
                    }
                }
            }
            info!("gas refresher thread exiting");
        })?;
    Ok(handle)
}

/// One refresh pass = one whole-snapshot authority transaction (see module docs). A failed
/// or partial pass returns `Err` (driving the caller's backoff) after recording health
/// evidence; it NEVER publishes a value, a floor, or a fresh timestamp.
fn refresh_once(
    client: &ArbitrumRpcClient,
    cfg: &GasAuthorityConfig,
    oracle: &GasOracle,
) -> Result<()> {
    let transport = RpcGasAuthorityTransport {
        client,
        multicall3: cfg.multicall3,
    };
    match refresh_gas_authority_once(&transport, cfg, oracle, mono_ns(), now_ms()) {
        RefreshOutcome::Complete { .. } => Ok(()),
        RefreshOutcome::Failed { component_status } => Err(anyhow::anyhow!(
            "gas authority refresh incomplete: {component_status:?}"
        )),
    }
}
