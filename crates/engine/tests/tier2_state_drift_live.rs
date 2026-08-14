//! TIER-2 STATE-DRIFT (RPC-FREE) LIVE HARNESS — Axis B, measured on live Arbitrum.
//!
//! The existing per-family live-parity harnesses measure AXIS A (byte-exactness). This measures the
//! OTHER fast-lane gate, AXIS B: is the family RPC-FREE — i.e. can its in-memory mirror stay correct
//! from EVENTS ALONE, or does some state field DRIFT in a way no pool event carries (forcing the
//! periodic RPC reconcile)?
//!
//! Method: fetch the pool's full state at the head block and at `head - DRIFT_BLOCKS`, then report
//! WHICH fields changed. The drift map IS the verdict:
//!   * `balances` drift        → EXPECTED + fine (carried by Vault.Swap / TokenExchange → event-trackable)
//!   * `scaling_factors` drift → RATE-PROVIDER gap → NOT RPC-free unless we mirror the rate feed
//!   * `amp` / `swap_fee` drift→ A-ramp / governance gap (rare)
//!   * `price_scale` drift     → Curve CryptoSwap repeg (every swap, NOT evented) → NOT RPC-free
//! A field that drifts but is carried by NO event is exactly why the engine keeps a periodic RPC
//! reconcile for Balancer/Curve (see tier2_fastlane_rpcfree_readiness_static for the full verdict).
//!
//! IMPORTANT: a NON-drift sample is NOT proof of RPC-free (a rate provider can update at any time —
//! Chainlink heartbeat/deviation). The static analysis is the authoritative verdict; this harness
//! QUANTIFIES the gap on demand. Run over a wide window (hours) to reliably catch a rate update.
//!
//! Run:
//!   RUN_TIER2_STATE_DRIFT_LIVE=1 \
//!   ARBITRUM_RPC_URL=<url> TIER2_BAL_STABLE_POOL=0x<rate-scaled balancer v2 stable pool> \
//!   DRIFT_BLOCKS=200000 \
//!     cargo test -p engine --test tier2_state_drift_live -- --nocapture

use engine_rpc::{fetch_balancer_stable, ArbitrumRpcClient, BlockTag, BALANCER_VAULT_ARBITRUM};
use std::env;

fn addr(s: &str) -> [u8; 20] {
    let s = s.trim().trim_start_matches("0x");
    assert_eq!(s.len(), 40, "pool address must be 20 bytes hex");
    let mut a = [0u8; 20];
    for (k, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * k..2 * k + 2], 16).expect("hex addr");
    }
    a
}

/// Balancer V2 stable pool — profile which state fields drift over a block window. Point it at a
/// RATE-SCALED pool (e.g. a wstETH metastable) to exercise the rate-provider gap.
#[test]
fn balancer_stable_state_drift_profile() {
    if env::var("RUN_TIER2_STATE_DRIFT_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIP: set RUN_TIER2_STATE_DRIFT_LIVE=1 to hit live Arbitrum RPC");
        return;
    }
    let url = match env::var("ARBITRUM_RPC_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("SKIP: set ARBITRUM_RPC_URL");
            return;
        }
    };
    let pool = match env::var("TIER2_BAL_STABLE_POOL") {
        Ok(p) => addr(&p),
        Err(_) => {
            eprintln!(
                "SKIP: set TIER2_BAL_STABLE_POOL=0x<balancer v2 stable pool> (ideally rate-scaled)"
            );
            return;
        }
    };
    let drift_blocks: u64 = env::var("DRIFT_BLOCKS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);

    let client = ArbitrumRpcClient::new(url);
    let head = client.block_number().expect("block_number");
    let back = head.saturating_sub(drift_blocks);
    let vault = BALANCER_VAULT_ARBITRUM;

    let now = fetch_balancer_stable(&client, &vault, &pool, BlockTag::Number(head))
        .expect("fetch head (archive depth / RPC issue)");
    let then = fetch_balancer_stable(&client, &vault, &pool, BlockTag::Number(back))
        .expect("fetch back (archive depth / RPC issue)");

    let balances_drift = now.balances != then.balances;
    let scaling_drift = now.scaling_factors != then.scaling_factors;
    let amp_drift = now.amp != then.amp;
    let fee_drift = now.swap_fee_1e18 != then.swap_fee_1e18;

    eprintln!(
        "── Balancer V2 stable drift profile  pool=0x{}  blocks {back}→{head} ({drift_blocks}) ──",
        now.pool_id
            .iter()
            .take(20)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    eprintln!("  balances        drift = {balances_drift:5}  (carried by Vault.Swap → event-trackable, NOT a gap)");
    eprintln!(
        "  scaling_factors drift = {scaling_drift:5}  (RATE-PROVIDER → the RPC-free GAP if true)"
    );
    eprintln!("  amp             drift = {amp_drift:5}  (A-ramp gap if true)");
    eprintln!("  swap_fee        drift = {fee_drift:5}  (governance gap if true)");
    if scaling_drift {
        eprintln!(
            "  VERDICT: NOT RPC-free over this window — scaling_factors moved with no pool event."
        );
        eprintln!(
            "           Making it RPC-free requires mirroring the rate provider's Chainlink feed."
        );
    } else {
        eprintln!(
            "  VERDICT: no non-evented drift in THIS window — but a rate update can land anytime;"
        );
        eprintln!(
            "           this is a sample, not a proof. balances drift is fine (event-trackable)."
        );
    }

    // Mechanism assertion: two valid multi-token stable snapshots were read.
    assert!(
        now.balances.len() >= 2 && then.balances.len() >= 2,
        "expected a valid multi-token Balancer stable state at both blocks"
    );
}
