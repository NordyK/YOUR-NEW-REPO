//! WARMUP readiness barrier (B3).
//!
//! Gates LIVE feed ingestion until the engine is actually warm, so the first real opportunities
//! don't hit a cold path and the alpha_02 stale-gas / REVM live-safety guards never trip on a
//! normal boot. Conditions (only those that apply for the current run are required):
//!   * REVM validator deployed + pre-warmed (signalled by the revm loop), and
//!   * gas oracle holding a fresh, non-zero-L1 snapshot.
//!
//! Hard rules that keep it safe:
//!   * **File/offline replay bypasses the barrier entirely** (the caller does not invoke `wait`),
//!     so deterministic replay/cert runs never depend on RPC-backed warmth.
//!   * `wait` is **timeout-bounded** — it can never hang the engine; on timeout it proceeds and
//!     logs a warning. It also returns immediately if shutdown is requested.
//!
//! Pure orchestration: no effect on detection, AMM math, sizing, or submission *results* — it only
//! delays when live ingestion begins.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_gas::GasOracle;
use tracing::{info, warn};

/// Shared readiness state. Construct via [`WarmupBarrier::new`], share as `Arc`.
pub struct WarmupBarrier {
    revm_ready: AtomicBool,
    gas: Arc<GasOracle>,
    /// REVM validator must be warm before we ingest live (REVM_VALIDATE on + RPC present + live).
    require_revm: bool,
    /// A fresh, non-zero-L1 gas snapshot must exist before we ingest live (RPC present + live).
    require_gas: bool,
    /// Max time `wait` blocks before proceeding anyway (never hang).
    timeout: Duration,
}

impl WarmupBarrier {
    /// Build the barrier with the requirements that apply to this run.
    pub fn new(
        gas: Arc<GasOracle>,
        require_revm: bool,
        require_gas: bool,
        timeout: Duration,
    ) -> Self {
        Self {
            revm_ready: AtomicBool::new(false),
            gas,
            require_revm,
            require_gas,
            timeout,
        }
    }

    /// Signalled by the REVM loop once the validator is deployed + resident pools pre-warmed
    /// (or once it has entered its consume loop in dry-run/gross-only mode).
    pub fn mark_revm_ready(&self) {
        self.revm_ready.store(true, Ordering::Release);
    }

    /// A5 (boot-6 fix): emit the `ENGINE_READY` marker for a live-ingestion start that proceeded
    /// WITHOUT a clean ready — i.e. the barrier timed out in a NON-FATAL (dry-run) run and the
    /// caller is about to ingest anyway. [`wait`] emits `ENGINE_READY` on its two ready paths, but
    /// on a timeout it logged only the `WARMUP timed out` WARN and returned `false`; so on boot-6 a
    /// dry-run whose validator deploy failed (never `mark_revm_ready`) timed out, proceeded to
    /// ingest, and produced NO `ENGINE_READY` line. The rx caller invokes this on the timeout-proceed
    /// path so the marker is UNCONDITIONAL whenever live ingestion begins (and is still absent on the
    /// fatal-halt / shutdown paths, which do not ingest). Kept as a WARN — a cold start is degraded.
    pub fn note_cold_ingestion_begin(&self) {
        warn!(
            require_revm = self.require_revm,
            require_gas = self.require_gas,
            "ENGINE_READY: warmup timed out — beginning live ingestion anyway (cold start / degraded)"
        );
    }

    /// True once every *required* condition is met.
    pub fn is_ready(&self) -> bool {
        if self.require_revm && !self.revm_ready.load(Ordering::Acquire) {
            return false;
        }
        if self.require_gas {
            let fresh_nonzero_l1 = self
                .gas
                .load_fresh()
                .map(|s| s.l1_base_fee_per_gas_wei > 0)
                .unwrap_or(false);
            if !fresh_nonzero_l1 {
                return false;
            }
        }
        true
    }

    /// Block (polling) until ready or the timeout elapses or shutdown is requested.
    /// Returns `true` if it became ready, `false` if it timed out / shut down. NEVER hangs.
    pub fn wait(&self, shutdown: &AtomicBool) -> bool {
        if self.is_ready() {
            info!(
                require_revm = self.require_revm,
                require_gas = self.require_gas,
                "ENGINE_READY: warmup conditions already satisfied"
            );
            return true;
        }
        let start = Instant::now();
        loop {
            if shutdown.load(Ordering::Acquire) {
                return false;
            }
            if self.is_ready() {
                info!(
                    waited_ms = start.elapsed().as_millis() as u64,
                    "ENGINE_READY: warmup complete; beginning live ingestion"
                );
                return true;
            }
            if start.elapsed() >= self.timeout {
                warn!(
                    timeout_ms = self.timeout.as_millis() as u64,
                    revm_ready = self.revm_ready.load(Ordering::Acquire),
                    require_revm = self.require_revm,
                    require_gas = self.require_gas,
                    "WARMUP timed out — proceeding with live ingestion anyway (cold start)"
                );
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_gas::{now_ms, GasSnapshot};

    fn oracle_default() -> Arc<GasOracle> {
        Arc::new(GasOracle::new(GasSnapshot::default(), 1_000))
    }

    fn store_fresh_l1(o: &GasOracle, l1: u128) {
        o.store(GasSnapshot {
            l1_base_fee_per_gas_wei: l1,
            captured_at_ms: now_ms(),
            ..GasSnapshot::default()
        });
    }

    #[test]
    fn no_requirements_is_immediately_ready() {
        let b = WarmupBarrier::new(oracle_default(), false, false, Duration::from_millis(50));
        assert!(b.is_ready());
        assert!(b.wait(&AtomicBool::new(false)));
    }

    #[test]
    fn revm_requirement_blocks_until_marked() {
        let b = WarmupBarrier::new(oracle_default(), true, false, Duration::from_millis(50));
        assert!(!b.is_ready());
        b.mark_revm_ready();
        assert!(b.is_ready());
    }

    #[test]
    fn gas_requirement_needs_fresh_nonzero_l1() {
        let o = oracle_default(); // default snapshot has l1_price = 0 → not ready
        let b = WarmupBarrier::new(Arc::clone(&o), false, true, Duration::from_millis(50));
        assert!(
            !b.is_ready(),
            "default zero-L1 snapshot must not satisfy gas requirement"
        );
        store_fresh_l1(&o, 3_000_000);
        assert!(
            b.is_ready(),
            "fresh non-zero-L1 snapshot must satisfy gas requirement"
        );
    }

    #[test]
    fn wait_times_out_without_hanging() {
        let b = WarmupBarrier::new(oracle_default(), true, false, Duration::from_millis(30));
        let start = Instant::now();
        // Never marked ready → must return false after ~timeout, never hang.
        assert!(!b.wait(&AtomicBool::new(false)));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "wait must be timeout-bounded"
        );
    }

    #[test]
    fn wait_returns_early_on_shutdown() {
        let b = WarmupBarrier::new(oracle_default(), true, false, Duration::from_secs(60));
        let shutdown = AtomicBool::new(true); // already shutting down
        let start = Instant::now();
        assert!(!b.wait(&shutdown));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "shutdown must short-circuit wait"
        );
    }
}
