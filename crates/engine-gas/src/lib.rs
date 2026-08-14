//! Gas / L1 fee oracle.
//!
//! Holds the current `(P, L1P)` snapshot plus precomputed Brotli L1 size buckets so the hot
//! path computes `TXFEES = P * (L2G + ((L1P * L1S) / P))` per the Arbitrum gas estimation
//! reference (§2.3) without any compression or RPC call. The background core refreshes the
//! snapshot via `arc-swap` so reads are wait-free atomic loads of an `Arc<Snapshot>`.
//!
//! After ArbOS 51 Dia (activated 2026-01-08) the minimum base fee is 0.02 gwei and the L1
//! calldata token cost is read via `tokenGasUnits` rather than a constant 16. The snapshot
//! carries the live values so the hot path never needs to know which ArbOS release is active.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// clippy: cosmetic false-positives on the byte-exact L1-fee bucket math (manual div-ceil is the
// reference formula; range index spans the bucket table). Zero-behavior-change suppression.
#![allow(clippy::manual_div_ceil, clippy::needless_range_loop)]

pub mod authority;
pub mod brotli_estimate;
pub use brotli_estimate::{brotli_calldata_bytes, brotli_size_estimate};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use arc_swap::ArcSwap;
use thiserror::Error;

/// Errors emitted by the gas crate.
#[derive(Debug, Error, Clone, Copy)]
pub enum GasError {
    /// L1S bucket id is out of range for the given template.
    #[error("l1s bucket id out of range")]
    BucketOutOfRange,
    /// The snapshot is older than `max_age_ms`.
    #[error("gas snapshot is stale")]
    Stale,
    /// `P` (L2 gas price) is zero, which is impossible under healthy gas estimation.
    #[error("l2 gas price is zero")]
    ZeroL2Price,
}

/// Live gas snapshot. `Copy` so it can be cloned cheaply and published via `arc-swap`.
#[derive(Debug, Clone, Copy)]
pub struct GasSnapshot {
    /// `P` — current L2 gas price in wei per gas unit.
    pub l2_gas_price_wei: u128,
    /// `L1P` — current L1 base fee per L1 gas unit (wei/gas), as returned by `ArbGasInfo`'s
    /// `getL1BaseFeeEstimate`. The per-calldata-byte cost is this × `l1_gas_per_data_byte` (≈16);
    /// the full L1 data fee is computed in [`l1_data_fee_wei`].
    pub l1_base_fee_per_gas_wei: u128,
    /// `tokenGasUnits` returned by the system contract (post-ArbOS 51); legacy constant
    /// (`16`) prior. Held verbatim so the hot path can multiply.
    pub l1_gas_per_data_byte: u32,
    /// Minimum L2 base fee enforced by ArbOS (in wei). Used to floor `l2_gas_price_wei` when
    /// computing `max_fee_per_gas`.
    pub min_l2_base_fee_wei: u128,
    /// Number of bytes of static per-transaction overhead Arbitrum adds in its size
    /// estimate. Currently 140 — this is an EMPIRICAL safety pad, not a documented ArbOS
    /// constant: it is sized to over-state (never under-state) the envelope/signature overhead.
    /// Since the resulting L1 fee is a subtracted cost, padding high is safe by direction (it
    /// only tightens the reject gate; see [`l1_data_fee_wei`]).
    pub static_overhead_bytes: u32,
    /// Balancer V2 flash-loan fee as 18-decimal fixed-point (1e18 = 100%).
    pub balancer_flash_fee_1e18: U256,
    /// Snapshot capture timestamp in ms since unix epoch.
    pub captured_at_ms: u64,
    /// T1.2: σ (bps) of recent L1 base-fee returns at capture time, carried so a downstream cost
    /// site can compute the [`dynamic_l1_stale_multiplier_bps`] margin from `(age, σ)` without the
    /// oracle in scope. `0` = calm / untracked (identity margin).
    pub l1_sigma_bps: u64,
}

impl Default for GasSnapshot {
    fn default() -> Self {
        Self {
            l2_gas_price_wei: 20_000_000, // 0.02 gwei post-Dia minimum.
            l1_base_fee_per_gas_wei: 0,
            l1_gas_per_data_byte: 16,
            min_l2_base_fee_wei: 20_000_000,
            static_overhead_bytes: 140,
            balancer_flash_fee_1e18: U256::ZERO,
            captured_at_ms: 0,
            l1_sigma_bps: 0,
        }
    }
}

/// Background-published gas snapshot. `load_full()` returns an `Arc<GasSnapshot>` cheaply.
pub struct GasOracle {
    current: ArcSwap<GasSnapshot>,
    /// Maximum permissible snapshot age in milliseconds. Used by the §10.11 staleness gate.
    pub max_age_ms: u64,
    /// T1.2: recent L1 base-fee samples for the volatility-adaptive stale margin. Touched only on
    /// the background refresh; the derived σ is cached in `l1_sigma_bps` for lock-free hot reads.
    vol: Mutex<L1VolatilityTracker>,
    /// Cached σ (bps) of recent L1 returns — updated on each [`record_l1_sample`](Self::record_l1_sample).
    l1_sigma_bps: AtomicU64,
    /// WI-13: the typed, complete-only gas authority (codex §3.4) this oracle ALSO publishes,
    /// BUNDLED with the legacy `GasSnapshot` bridged from the SAME generation (fix round:
    /// one ArcSwap store per complete publication, so a paired reader can never cross-bind
    /// an authority snapshot with another generation's legacy fee bytes). `None` until the
    /// first COMPLETE one-block refresh. See [`authority`].
    pub(crate) authority: arc_swap::ArcSwapOption<authority::GasAuthorityPublication>,
    /// WI-13: authority health/attempt evidence — separate from fee bytes; updated on EVERY
    /// refresh attempt (a failure changes ONLY this).
    pub(crate) authority_health: ArcSwap<authority::GasHealth>,
    /// WI-13: next complete-generation counter (advanced only by a complete publication).
    pub(crate) next_generation: AtomicU64,
    /// WI-13: next health-generation counter (advanced on every attempt).
    pub(crate) next_health_generation: AtomicU64,
    /// WI-13 fix round: serializes every refresh publication (complete AND failed). The
    /// read model stays wait-free; writers are ordered so concurrent/interleaved refreshes
    /// cannot cross-bind generations across the publication, legacy-mirror and health
    /// stores, and generation numbering matches publication order.
    pub(crate) publish_lock: Mutex<()>,
}

impl GasOracle {
    /// Construct an oracle with the given default snapshot and staleness limit.
    pub fn new(initial: GasSnapshot, max_age_ms: u64) -> Self {
        Self {
            current: ArcSwap::from_pointee(initial),
            max_age_ms,
            vol: Mutex::new(L1VolatilityTracker::new()),
            l1_sigma_bps: AtomicU64::new(0),
            authority: arc_swap::ArcSwapOption::from(None),
            authority_health: ArcSwap::from_pointee(authority::GasHealth::seeding()),
            next_generation: AtomicU64::new(1),
            next_health_generation: AtomicU64::new(2),
            publish_lock: Mutex::new(()),
        }
    }

    /// Hot-path read.
    #[inline]
    pub fn load(&self) -> Arc<GasSnapshot> {
        self.current.load_full()
    }

    /// Background-core update.
    pub fn store(&self, snap: GasSnapshot) {
        self.current.store(Arc::new(snap));
    }

    /// Read the current snapshot, returning `Err(Stale)` if older than `max_age_ms`.
    ///
    /// `captured_at_ms == 0` is the never-refreshed sentinel (the [`Default`] snapshot, whose L1
    /// fee is also 0) — it is treated as **Stale**, never fresh: a gas snapshot that has never been
    /// stamped by the background refresher must not gate a submission (defense-in-depth alongside
    /// the pipeline's own `l1 > 0` pre-broadcast check).
    pub fn load_fresh(&self) -> Result<Arc<GasSnapshot>, GasError> {
        let snap = self.current.load_full();
        let now = now_ms();
        if snap.captured_at_ms == 0
            || now.saturating_sub(snap.captured_at_ms) > self.max_age_ms
        {
            return Err(GasError::Stale);
        }
        Ok(snap)
    }

    /// T1.2: record an L1 base-fee observation (wei/byte) on the background refresh and refresh the
    /// cached σ. Cheap; called once per gas refresh, not on the hot path.
    pub fn record_l1_sample(&self, price_wei: u128) {
        if let Ok(mut v) = self.vol.lock() {
            v.push(price_wei);
            self.l1_sigma_bps.store(v.sigma_bps(), Ordering::Release);
        }
    }

    /// Cached σ (bps) of recent L1 returns — lock-free.
    #[inline]
    pub fn l1_sigma_bps(&self) -> u64 {
        self.l1_sigma_bps.load(Ordering::Acquire)
    }

    /// T1.2: the volatility-adaptive L1 stale-margin multiplier (bps; `10_000` = 1.0×) for the
    /// current snapshot age at `now_ms_val`, combining its age with the cached L1 σ. Lock-free.
    /// Apply via [`l1_data_fee_wei_margined`] to make the L1 cost conservative when stale/volatile.
    #[inline]
    pub fn l1_stale_margin_bps(&self, now_ms_val: u64) -> u64 {
        let snap = self.current.load();
        let age = if snap.captured_at_ms > 0 {
            now_ms_val.saturating_sub(snap.captured_at_ms)
        } else {
            0
        };
        dynamic_l1_stale_multiplier_bps(age, self.l1_sigma_bps())
    }
}

// --- T1.2: volatility-adaptive L1-fee staleness margin (ported from Tigger gas_integration.rs) ---

/// Integer square root of a `u128` (Newton's method; exact floor). Used for the rolling-σ.
fn isqrt_u128(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Grace window: snapshots younger than this carry no *age* margin (only the volatility term).
pub const L1_STALE_GRACE_MS: u64 = 5_000;
/// Ramp window over which the age margin grows from 1.0× to its 3.0× ceiling.
pub const L1_STALE_RAMP_MS: u64 = 25_000;

/// Volatility-adaptive staleness multiplier (basis points; `10_000` = 1.0×) applied to the L1 cost
/// term when the gas snapshot is stale. Combines an **age** component (grows past
/// [`L1_STALE_GRACE_MS`] over [`L1_STALE_RAMP_MS`]) with a **volatility** component (`sigma_bps` of
/// recent L1 returns). Calm-but-stale → small margin (keeps trading, recovers profit); stale AND
/// volatile → up to 5.0× (more conservative, avoids underpricing gas into a loss). Pure integer.
pub fn dynamic_l1_stale_multiplier_bps(age_ms: u64, sigma_bps: u64) -> u64 {
    let over = age_ms
        .saturating_sub(L1_STALE_GRACE_MS)
        .min(L1_STALE_RAMP_MS);
    let age_component = 10_000u64 + over * 20_000 / L1_STALE_RAMP_MS;
    let vol_component = 10_000u64 + sigma_bps.min(20_000);
    let combined = (age_component as u128 * vol_component as u128 / 10_000u128) as u64;
    combined.clamp(10_000, 50_000)
}

/// Scale an L1 data fee up by a [`dynamic_l1_stale_multiplier_bps`] result. `10_000` bps = identity.
#[inline]
pub fn l1_data_fee_wei_margined(
    snap: &GasSnapshot,
    raw_calldata_bytes: u32,
    mult_bps: u64,
) -> u128 {
    let base = l1_data_fee_wei(snap, raw_calldata_bytes);
    (U256::from(base) * U256::from(mult_bps) / U256::from(10_000u64))
        .try_into()
        .unwrap_or(u128::MAX)
}

/// Rolling tracker of recent L1 base-fee observations, for the volatility component of the stale
/// margin. Fixed 20-slot ring; lock-free reads are NOT required (updated on the background gas
/// refresh, read on the cost path under the oracle's mutex). Prices held in raw wei — the σ is
/// computed over *returns* (per-step % change in bps), which is scale-invariant, so no gwei
/// down-conversion / precision loss is needed.
#[derive(Clone, Debug)]
pub struct L1VolatilityTracker {
    prices_wei: [u128; Self::CAPACITY],
    cursor: usize,
    count: u64,
}

impl Default for L1VolatilityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl L1VolatilityTracker {
    const CAPACITY: usize = 20;

    /// Empty tracker.
    pub const fn new() -> Self {
        Self {
            prices_wei: [0u128; Self::CAPACITY],
            cursor: 0,
            count: 0,
        }
    }

    /// Record one L1 price observation (wei per byte).
    pub fn push(&mut self, price_wei: u128) {
        self.prices_wei[self.cursor] = price_wei;
        self.cursor = (self.cursor + 1) % Self::CAPACITY;
        self.count += 1;
    }

    /// Number of valid samples currently held (saturates at capacity).
    pub fn len(&self) -> usize {
        (self.count as usize).min(Self::CAPACITY)
    }

    /// Whether no samples have been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Rolling standard deviation of per-step returns, in basis points. `0` with < 2 samples.
    pub fn sigma_bps(&self) -> u64 {
        let n = self.len();
        if n < 2 {
            return 0;
        }
        // Oldest sample index (the ring is full once count > CAPACITY).
        let start = if self.count as usize <= Self::CAPACITY {
            0
        } else {
            self.cursor
        };
        let num_returns = n - 1;
        let mut returns = [0i128; Self::CAPACITY - 1];
        for i in 0..num_returns {
            let prev = self.prices_wei[(start + i) % Self::CAPACITY];
            let curr = self.prices_wei[(start + i + 1) % Self::CAPACITY];
            returns[i] = if prev == 0 {
                0
            } else {
                ((curr as i128 - prev as i128) * 10_000) / prev as i128
            };
        }
        let slice = &returns[..num_returns];
        let sum: i128 = slice.iter().sum();
        let mean = sum / num_returns as i128;
        let var_sum: u128 = slice
            .iter()
            .map(|r| {
                let d = r - mean;
                (d * d) as u128
            })
            .sum();
        let variance = var_sum / num_returns as u128;
        isqrt_u128(variance) as u64
    }
}

/// Current wall-clock time in milliseconds since unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// B12: Arbitrum L2 block-boundary clock. The sequencer seals an L2 block on a fixed cadence
/// (~250 ms on Arbitrum One); a tx that arrives just before a seal lands in THAT block, and one
/// arriving just after waits a full period for the next. For a backrun/arb the same-block edge
/// is worth knowing, so the submitter can see how much slack remains before the current block
/// seals and prioritise accordingly.
///
/// Lock-free: `last_block_ns` is an atomic anchor updated each time a new L2 block timestamp is
/// observed from the feed; the period is fixed (tunable). `time_to_boundary_ns(now)` is a pure
/// modular computation off the anchor — no allocation, safe to call on the hot submit path.
pub struct BatchClock {
    /// L2 block period in ns (Arbitrum One ≈ 250 ms).
    period_ns: u64,
    /// Wall-clock ns of the most recently observed block boundary (the phase anchor).
    last_block_ns: core::sync::atomic::AtomicU64,
    /// Highest L2 block number observed (so re-anchoring happens once per new block, not per log).
    last_block_num: core::sync::atomic::AtomicU64,
    /// Highest L2 block timestamp (unix seconds) observed from the feed header. Feeds the REVM
    /// validator's per-block `block.timestamp` so deadline/TWAP-reading contracts behave as at the
    /// live head instead of the boot-pinned block (TAIL_LATENCY_PLAN §2 A3). Zero until observed.
    last_block_ts_secs: core::sync::atomic::AtomicU64,
}

impl BatchClock {
    /// New clock with `period_ms` block cadence (250 for Arbitrum One). The anchor starts at 0
    /// (no block observed yet → `time_to_boundary_ns` returns the full period as a safe default).
    pub fn new(period_ms: u64) -> Self {
        Self {
            period_ns: period_ms.max(1).saturating_mul(1_000_000),
            last_block_ns: core::sync::atomic::AtomicU64::new(0),
            last_block_num: core::sync::atomic::AtomicU64::new(0),
            last_block_ts_secs: core::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Record an observed L2 block boundary (call when a new block's first message is seen on the
    /// feed). Re-anchors the phase so drift between the fixed period and the real sequencer is
    /// continuously corrected.
    pub fn observe_block(&self, block_wall_ns: u64) {
        self.last_block_ns
            .store(block_wall_ns, core::sync::atomic::Ordering::Relaxed);
    }

    /// Re-anchor the phase when a STRICTLY NEWER block number is seen (the natural boundary
    /// signal from per-log `blockNumber`). Called per log on the event path, but only the first
    /// log of each new block actually re-anchors. Returns `true` if it re-anchored. Lock-free.
    pub fn observe_block_number(&self, block_num: u64, now_ns: u64) -> bool {
        use core::sync::atomic::Ordering;
        let mut cur = self.last_block_num.load(Ordering::Relaxed);
        loop {
            if block_num <= cur {
                return false; // not newer → ignore (later logs of the same/old block)
            }
            match self.last_block_num.compare_exchange_weak(
                cur,
                block_num,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.observe_block(now_ns);
                    return true;
                }
                Err(actual) => cur = actual, // race: retry with the updated value
            }
        }
    }

    /// Record an observed L2 block timestamp (unix seconds, from the feed message header). Stores
    /// the running maximum so out-of-order or duplicate observations never move the clock backward.
    /// Lock-free; safe on the hot feed path.
    pub fn observe_block_timestamp(&self, ts_secs: u64) {
        use core::sync::atomic::Ordering;
        let mut cur = self.last_block_ts_secs.load(Ordering::Relaxed);
        while ts_secs > cur {
            match self.last_block_ts_secs.compare_exchange_weak(
                cur,
                ts_secs,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Highest L2 block number observed so far (0 if none yet).
    #[inline]
    pub fn last_block_number(&self) -> u64 {
        self.last_block_num
            .load(core::sync::atomic::Ordering::Relaxed)
    }

    /// Highest L2 block timestamp (unix seconds) observed so far (0 if none yet).
    #[inline]
    pub fn last_block_timestamp_secs(&self) -> u64 {
        self.last_block_ts_secs
            .load(core::sync::atomic::Ordering::Relaxed)
    }

    /// The configured block period (ns).
    #[inline]
    pub fn period_ns(&self) -> u64 {
        self.period_ns
    }

    /// Nanoseconds remaining until the next block boundary at wall-clock `now_ns`. If no block has
    /// been observed yet (anchor 0) OR `now` precedes the anchor, returns the full period (the
    /// conservative "a fresh block just started" assumption). Result is in `(0, period_ns]`.
    #[inline]
    pub fn time_to_boundary_ns(&self, now_ns: u64) -> u64 {
        let anchor = self
            .last_block_ns
            .load(core::sync::atomic::Ordering::Relaxed);
        if anchor == 0 || now_ns < anchor {
            return self.period_ns;
        }
        let elapsed = now_ns - anchor;
        let phase = elapsed % self.period_ns;
        // phase==0 means we're exactly on a boundary → a full period remains until the *next* one.
        self.period_ns - phase
    }

    /// Wall-clock ns of the next block boundary at `now_ns`.
    #[inline]
    pub fn next_boundary_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_add(self.time_to_boundary_ns(now_ns))
    }
}

/// Precomputed L1 calldata Brotli-size bucket: `template_id × amount_bucket_id → bytes`.
///
/// The hot path never compresses; it indexes this table.
pub struct L1SizeBuckets {
    /// Buckets stored in row-major order: index = `template_id * amount_buckets + amount_id`.
    pub data: Vec<u32>,
    /// Number of amount buckets per template.
    pub amount_buckets: u32,
    /// Total number of templates.
    pub templates: u32,
}

impl L1SizeBuckets {
    /// Allocate a fixed table.
    pub fn new(templates: u32, amount_buckets: u32) -> Self {
        let n = (templates as usize) * (amount_buckets as usize);
        Self {
            data: vec![0u32; n],
            amount_buckets,
            templates,
        }
    }

    /// Hot-path bucket lookup.
    #[inline]
    pub fn get(&self, template_id: u32, amount_bucket_id: u32) -> Result<u32, GasError> {
        if template_id >= self.templates || amount_bucket_id >= self.amount_buckets {
            return Err(GasError::BucketOutOfRange);
        }
        let idx =
            (template_id as usize) * (self.amount_buckets as usize) + (amount_bucket_id as usize);
        Ok(self.data[idx])
    }

    /// Background-only setter.
    pub fn set(&mut self, template_id: u32, amount_bucket_id: u32, bytes: u32) {
        let idx =
            (template_id as usize) * (self.amount_buckets as usize) + (amount_bucket_id as usize);
        self.data[idx] = bytes;
    }
}

/// Computes Arbitrum L1 data fee per §12.3.
///
/// Arbitrum charges for the **brotli-zero-compressed** size of the posted tx
/// (then × the LIVE `tokenGasUnits` — [`GasSnapshot::l1_gas_per_data_byte`], read from
/// `getPricesInArbGasWithAggregator` at the snapshot block since WI-13; the pre-Dia
/// constant 16 is retired), so the `raw_calldata_bytes` budget is first run through
/// [`brotli_calldata_bytes`] before being charged — arb calldata (zero-padded
/// address/amount words) compresses materially, and charging the raw count would
/// over-state the fee and reject profitable arbs. The static per-tx overhead
/// (signature/envelope) is high-entropy and does not compress, so it is added
/// after compression:
///
/// `L1P * tokenGasUnits * (static_overhead + brotli0(raw_calldata_bytes))`.
///
/// INTENTIONAL conservative OVER-charge: compressing only the calldata budget (not the whole
/// envelope) and then adding the full raw [`GasSnapshot::static_overhead_bytes`] pad on top is a
/// deliberate upper bound on the true L1 fee, not a precise reproduction of ArbOS accounting. This
/// is safe by direction: the L1 data fee is a SUBTRACTED cost in `canonical_net_profit`, so any
/// over-estimate here can only *lower* net profit and *tighten* the reject gate — it never lets an
/// unprofitable arb through. The worst case is leaving a marginal arb on the table, never a
/// capital loss.
#[inline]
pub fn l1_data_fee_wei(snap: &GasSnapshot, raw_calldata_bytes: u32) -> u128 {
    let l1_price = snap.l1_base_fee_per_gas_wei;
    let token_units = snap.l1_gas_per_data_byte as u128;
    let compressed = brotli_calldata_bytes(raw_calldata_bytes) as u128;
    let total_bytes = (snap.static_overhead_bytes as u128) + compressed;
    l1_price
        .saturating_mul(token_units)
        .saturating_mul(total_bytes)
}

/// G6 (Codex 2026-07-10): L1 data fee computed from the ACTUAL calldata bytes — the exact
/// brotli-zero compression of THESE bytes — instead of the length-keyed representative blob
/// (content-dependent compressibility can diverge from the structural estimate in either
/// direction). Costs 50–200µs of compression: VALIDATOR-THREAD (prepare) use only, never the
/// sim hot path. Same static envelope overhead as [`l1_data_fee_wei`]; the compressed size is
/// capped at the raw length (framing amortizes across the batch, never billed per-tx above raw).
pub fn l1_data_fee_wei_exact(snap: &GasSnapshot, calldata: &[u8]) -> u128 {
    let compressed = (brotli_estimate::brotli_size_estimate(calldata) as u128)
        .min(calldata.len() as u128)
        .max(1);
    snap.l1_base_fee_per_gas_wei
        .saturating_mul(snap.l1_gas_per_data_byte as u128)
        .saturating_mul((snap.static_overhead_bytes as u128) + compressed)
}

/// L2 execution cost per §12.2.
#[inline]
pub fn l2_execution_cost_wei(snap: &GasSnapshot, gas_units: u64) -> u128 {
    snap.l2_gas_price_wei.saturating_mul(gas_units as u128)
}

/// Flash-loan fee per §12.4.
///
/// `principal_wei * fee_1e18 / 1e18`.
pub fn flash_loan_fee_wei(snap: &GasSnapshot, principal_wei: U256) -> U256 {
    if snap.balancer_flash_fee_1e18.is_zero() {
        return U256::ZERO;
    }
    let scale = U256::from(10u64).pow(U256::from(18));
    let prod = principal_wei.saturating_mul(snap.balancer_flash_fee_1e18);
    prod / scale
}

/// Compute total submit cost (excluding executor overhead / safety buffer) for a route at a
/// given amount bucket.
pub fn submit_cost_wei(
    snap: &GasSnapshot,
    gas_units: u64,
    raw_calldata_bytes: u32,
    principal_wei: U256,
) -> U256 {
    let l2 = U256::from(l2_execution_cost_wei(snap, gas_units));
    let l1 = U256::from(l1_data_fee_wei(snap, raw_calldata_bytes));
    let flash = flash_loan_fee_wei(snap, principal_wei);
    l2 + l1 + flash
}

#[cfg(test)]
mod batch_clock_tests {
    use super::BatchClock;

    const P: u64 = 250_000_000; // 250 ms in ns

    #[test]
    fn full_period_before_any_block_observed() {
        let c = BatchClock::new(250);
        assert_eq!(c.period_ns(), P);
        // No anchor yet → full period (safe "block just started" default).
        assert_eq!(c.time_to_boundary_ns(1_000_000_000), P);
    }

    #[test]
    fn time_to_boundary_is_modular_off_anchor() {
        let c = BatchClock::new(250);
        c.observe_block(1_000_000_000); // anchor at t=1.0s
                                        // 100ms into the period → 150ms remain.
        assert_eq!(
            c.time_to_boundary_ns(1_000_000_000 + 100_000_000),
            150_000_000
        );
        // 249ms in → 1ms remains.
        assert_eq!(
            c.time_to_boundary_ns(1_000_000_000 + 249_000_000),
            1_000_000
        );
        // Exactly on a later boundary (2 full periods in) → a full period until the next.
        assert_eq!(c.time_to_boundary_ns(1_000_000_000 + 2 * P), P);
    }

    #[test]
    fn boundary_result_always_in_half_open_period_range() {
        let c = BatchClock::new(250);
        c.observe_block(500_000_000);
        for dt in [0u64, 1, 123_456, 250_000_000, 251_000_000, 999_999_999] {
            let ttb = c.time_to_boundary_ns(500_000_000 + dt);
            assert!(ttb > 0 && ttb <= P, "ttb {ttb} out of (0, P] at dt={dt}");
        }
    }

    #[test]
    fn now_before_anchor_returns_full_period() {
        let c = BatchClock::new(250);
        c.observe_block(2_000_000_000);
        assert_eq!(c.time_to_boundary_ns(1_000_000_000), P); // now < anchor → safe default
    }

    #[test]
    fn next_boundary_is_now_plus_time_to_boundary() {
        let c = BatchClock::new(250);
        c.observe_block(1_000_000_000);
        let now = 1_000_000_000 + 100_000_000;
        assert_eq!(c.next_boundary_ns(now), now + c.time_to_boundary_ns(now));
    }

    #[test]
    fn observe_block_number_reanchors_once_per_new_block() {
        let c = BatchClock::new(250);
        // First block at t=1.0s anchors.
        assert!(c.observe_block_number(1000, 1_000_000_000));
        // Later logs of the SAME block don't re-anchor.
        assert!(!c.observe_block_number(1000, 1_050_000_000));
        // An OLDER block (out-of-order log) doesn't re-anchor.
        assert!(!c.observe_block_number(999, 1_060_000_000));
        // The phase is still anchored at 1.0s → 100ms in → 150ms remain.
        assert_eq!(c.time_to_boundary_ns(1_100_000_000), 150_000_000);
        // A new block re-anchors.
        assert!(c.observe_block_number(1001, 1_250_000_000));
        assert_eq!(
            c.time_to_boundary_ns(1_250_000_000 + 10_000_000),
            240_000_000
        );
    }

    #[test]
    fn reanchor_corrects_drift() {
        let c = BatchClock::new(250);
        c.observe_block(1_000_000_000);
        // A fresh block observed slightly off the fixed grid re-anchors the phase.
        c.observe_block(1_260_000_000); // 260ms later (10ms drift)
        assert_eq!(
            c.time_to_boundary_ns(1_260_000_000 + 50_000_000),
            200_000_000
        );
    }

    #[test]
    fn block_number_and_timestamp_getters_track_observations() {
        let c = BatchClock::new(250);
        assert_eq!(c.last_block_number(), 0, "none observed yet");
        assert_eq!(c.last_block_timestamp_secs(), 0);
        c.observe_block_number(1234, 1_000_000_000);
        c.observe_block_timestamp(1_700_000_000);
        assert_eq!(c.last_block_number(), 1234);
        assert_eq!(c.last_block_timestamp_secs(), 1_700_000_000);
    }

    #[test]
    fn observe_block_timestamp_is_monotonic_max() {
        let c = BatchClock::new(250);
        c.observe_block_timestamp(1_700_000_010);
        // An older (out-of-order) timestamp must not move the clock backward.
        c.observe_block_timestamp(1_700_000_005);
        assert_eq!(c.last_block_timestamp_secs(), 1_700_000_010);
        // A newer one advances it.
        c.observe_block_timestamp(1_700_000_020);
        assert_eq!(c.last_block_timestamp_secs(), 1_700_000_020);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_stores_and_loads() {
        let o = GasOracle::new(GasSnapshot::default(), 1_000);
        let s = o.load();
        assert_eq!(s.l1_gas_per_data_byte, 16);
        let mut new = *s;
        new.l1_gas_per_data_byte = 18;
        o.store(new);
        assert_eq!(o.load().l1_gas_per_data_byte, 18);
    }

    #[test]
    fn l1_size_bucket_lookup() {
        let mut t = L1SizeBuckets::new(4, 10);
        t.set(1, 5, 240);
        assert_eq!(t.get(1, 5).unwrap(), 240);
        assert!(matches!(t.get(4, 0), Err(GasError::BucketOutOfRange)));
    }

    #[test]
    fn l2_cost_basic() {
        let snap = GasSnapshot {
            l2_gas_price_wei: 20_000_000,
            ..GasSnapshot::default()
        };
        let cost = l2_execution_cost_wei(&snap, 200_000);
        assert_eq!(cost, 200_000u128 * 20_000_000u128);
    }

    #[test]
    fn flash_loan_zero_when_fee_zero() {
        let snap = GasSnapshot::default();
        let p = U256::from(1_000_000u64);
        assert_eq!(flash_loan_fee_wei(&snap, p), U256::ZERO);
    }

    #[test]
    fn flash_loan_one_percent() {
        let snap = GasSnapshot {
            balancer_flash_fee_1e18: U256::from(10u64).pow(U256::from(16)), // 1e16 == 1%
            ..GasSnapshot::default()
        };
        let p = U256::from(1_000_000u64);
        let fee = flash_loan_fee_wei(&snap, p);
        assert_eq!(fee, U256::from(10_000u64));
    }
}

#[cfg(test)]
mod t1_2_stale_margin_tests {
    use super::*;

    #[test]
    fn isqrt_is_exact_floor() {
        assert_eq!(isqrt_u128(0), 0);
        assert_eq!(isqrt_u128(1), 1);
        assert_eq!(isqrt_u128(3), 1);
        assert_eq!(isqrt_u128(4), 2);
        assert_eq!(isqrt_u128(99), 9);
        assert_eq!(isqrt_u128(100), 10);
        assert_eq!(isqrt_u128(1_000_000), 1_000);
        // floor for a perfect square minus one
        assert_eq!(isqrt_u128(10_000 * 10_000 - 1), 9_999);
    }

    #[test]
    fn fresh_and_calm_is_identity() {
        // age below the grace window, zero volatility → exactly 1.0× (no margin).
        assert_eq!(dynamic_l1_stale_multiplier_bps(0, 0), 10_000);
        assert_eq!(
            dynamic_l1_stale_multiplier_bps(L1_STALE_GRACE_MS, 0),
            10_000
        );
        assert_eq!(dynamic_l1_stale_multiplier_bps(499, 0), 10_000);
    }

    #[test]
    fn stale_and_volatile_clamps_at_5x() {
        // Far past the ramp AND max volatility → clamped to 5.0× (50_000 bps).
        let m = dynamic_l1_stale_multiplier_bps(1_000_000, 1_000_000);
        assert_eq!(m, 50_000);
    }

    #[test]
    fn calm_but_stale_grows_modestly_with_age() {
        // Calm (σ=0): only the age component applies. At grace+ramp it reaches 3.0× exactly.
        let calm_full_age =
            dynamic_l1_stale_multiplier_bps(L1_STALE_GRACE_MS + L1_STALE_RAMP_MS, 0);
        assert_eq!(calm_full_age, 30_000); // 3.0×
                                           // Mid-ramp calm is between 1.0× and 3.0× and strictly monotonic in age.
        let a = dynamic_l1_stale_multiplier_bps(L1_STALE_GRACE_MS + 5_000, 0);
        let b = dynamic_l1_stale_multiplier_bps(L1_STALE_GRACE_MS + 15_000, 0);
        assert!(10_000 < a && a < b && b < 30_000, "a={a} b={b}");
    }

    #[test]
    fn margin_is_monotonic_nondecreasing_in_age_and_sigma() {
        let mut prev = 0;
        for age in (0..40_000).step_by(1_000) {
            let m = dynamic_l1_stale_multiplier_bps(age, 0);
            assert!(m >= prev, "age margin must be non-decreasing");
            prev = m;
        }
        let mut prev = 0;
        for sigma in (0..30_000).step_by(1_000) {
            let m = dynamic_l1_stale_multiplier_bps(10_000, sigma);
            assert!(m >= prev, "vol margin must be non-decreasing");
            prev = m;
        }
    }

    #[test]
    fn margined_fee_identity_and_scaling() {
        let snap = GasSnapshot {
            l1_base_fee_per_gas_wei: 3_100_000,
            ..GasSnapshot::default()
        };
        let base = l1_data_fee_wei(&snap, 256);
        assert_eq!(l1_data_fee_wei_margined(&snap, 256, 10_000), base); // 1.0× identity
        assert_eq!(l1_data_fee_wei_margined(&snap, 256, 20_000), base * 2); // 2.0×
        assert_eq!(l1_data_fee_wei_margined(&snap, 256, 15_000), base * 3 / 2); // 1.5×
    }

    #[test]
    fn tracker_sigma_zero_for_constant_or_single() {
        let mut t = L1VolatilityTracker::new();
        assert_eq!(t.sigma_bps(), 0); // empty
        t.push(3_100_000);
        assert_eq!(t.sigma_bps(), 0); // one sample
        for _ in 0..10 {
            t.push(3_100_000); // constant price → zero returns → zero σ
        }
        assert_eq!(t.sigma_bps(), 0);
    }

    #[test]
    fn tracker_sigma_positive_when_prices_move() {
        let mut t = L1VolatilityTracker::new();
        // Alternating ±~3% steps → non-trivial return σ.
        for i in 0..12 {
            t.push(if i % 2 == 0 { 3_000_000 } else { 3_100_000 });
        }
        assert!(t.sigma_bps() > 0, "moving prices must show volatility");
    }

    #[test]
    fn tracker_ring_wraps_and_uses_only_last_20() {
        let mut t = L1VolatilityTracker::new();
        // 25 constant samples (ring holds 20) → still calm → σ 0 and len saturates at 20.
        for _ in 0..25 {
            t.push(5_000_000);
        }
        assert_eq!(t.len(), 20);
        assert_eq!(t.sigma_bps(), 0);
        // A burst of volatility then fills the ring; old calm samples roll off.
        for i in 0..25 {
            t.push(if i % 2 == 0 { 4_000_000 } else { 6_000_000 });
        }
        assert!(t.sigma_bps() > 0);
    }

    #[test]
    fn oracle_records_samples_and_exposes_margin() {
        let snap = GasSnapshot {
            l1_base_fee_per_gas_wei: 3_100_000,
            captured_at_ms: 1_000_000,
            ..GasSnapshot::default()
        };
        let o = GasOracle::new(snap, 500);
        // Calm series → σ 0 → fresh margin is identity.
        for _ in 0..10 {
            o.record_l1_sample(3_100_000);
        }
        assert_eq!(o.l1_sigma_bps(), 0);
        assert_eq!(o.l1_stale_margin_bps(1_000_000), 10_000); // age 0, calm → 1.0×
                                                              // Same-calm but very stale → age margin only (3.0× at full ramp).
        let stale_now = 1_000_000 + L1_STALE_GRACE_MS + L1_STALE_RAMP_MS;
        assert_eq!(o.l1_stale_margin_bps(stale_now), 30_000);
        // Inject volatility → σ rises → margin rises further at the same stale age.
        for i in 0..12 {
            o.record_l1_sample(if i % 2 == 0 { 2_000_000 } else { 4_000_000 });
        }
        assert!(o.l1_sigma_bps() > 0);
        assert!(o.l1_stale_margin_bps(stale_now) > 30_000);
    }
}

#[cfg(test)]
mod sentinel_freshness_tests {
    //! AUD-GAS-SENTINEL: a never-refreshed (captured_at_ms==0) snapshot must be Stale, not fresh.
    use super::*;
    #[test]
    fn load_fresh_rejects_zero_timestamp_sentinel() {
        // Default snapshot has captured_at_ms==0 and l1==0 → must be Stale.
        let oracle = GasOracle::new(GasSnapshot::default(), 500);
        assert!(matches!(oracle.load_fresh(), Err(GasError::Stale)));
    }

    /// G6: the exact-bytes L1 fee must (a) charge structured (zero-heavy) calldata LESS than the
    /// raw count, (b) charge incompressible bytes their full raw count (never less), and (c) sit
    /// within sane distance of the length-keyed representative estimate for realistic calldata.
    #[test]
    fn l1_fee_exact_tracks_actual_content() {
        let snap = GasSnapshot {
            l1_base_fee_per_gas_wei: 1_000_000,
            l1_gas_per_data_byte: 16,
            static_overhead_bytes: 140,
            ..GasSnapshot::default()
        };
        // Structured, zero-heavy ABI-ish blob (mostly zero-padded words) — compresses hard.
        let mut structured = vec![0u8; 1124];
        for (i, b) in structured.iter_mut().enumerate() {
            if i % 32 >= 28 {
                *b = (i % 251) as u8;
            }
        }
        let exact = l1_data_fee_wei_exact(&snap, &structured);
        let raw_billed = snap.l1_base_fee_per_gas_wei
            * (snap.l1_gas_per_data_byte as u128)
            * (snap.static_overhead_bytes as u128 + structured.len() as u128);
        assert!(exact < raw_billed, "structured calldata must compress: {exact} vs {raw_billed}");

        // Pseudo-random (xorshift) incompressible bytes: exact fee == raw-count fee (cap binds).
        let mut x = 0x9E3779B97F4A7C15u64;
        let noise: Vec<u8> = (0..1124)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x & 0xFF) as u8
            })
            .collect();
        let exact_noise = l1_data_fee_wei_exact(&snap, &noise);
        assert_eq!(exact_noise, raw_billed, "incompressible bytes bill the raw count");

        // Representative estimate for the same length is in the same regime as the structured
        // exact fee (both far below raw) — the estimate stays a sane hot-path proxy.
        let est = l1_data_fee_wei(&snap, structured.len() as u32);
        assert!(est < raw_billed);
    }
}
