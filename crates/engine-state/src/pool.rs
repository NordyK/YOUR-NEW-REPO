//! Pool table: dense SoA storage indexed by `PoolId`.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::ids::{Generation, PairId, PoolId, StateSlot, TokenId, VenueId};
use crate::publication::{StateWriteTxn, UniverseId};
use crate::token::Address;

/// AMM family discriminant. New families append to this enum (never reorder — the values
/// are persisted in cache files).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PoolKind {
    /// Uniswap V2-style constant product.
    UniswapV2 = 0,
    /// Sushi V2 (same math, different factory).
    SushiV2 = 1,
    /// Camelot V2 with directional dynamic fees read from the pair.
    CamelotV2 = 2,
    /// Uniswap V3 concentrated liquidity.
    UniswapV3 = 3,
    /// Sushi V3.
    SushiV3 = 4,
    /// PancakeSwap V3 (V3 math, different deployer).
    PancakeV3 = 5,
    /// Camelot V3 / Algebra V1.9 family.
    AlgebraV19 = 6,
    /// Curve StableSwap-NG.
    CurveStableSwapNg = 7,
    /// Curve CryptoSwap.
    CurveCryptoSwap = 8,
    /// Trader Joe LB v2.1/v2.2.
    TraderJoeLb = 9,
    /// Maverick V2.
    MaverickV2 = 10,
    /// Fluid DEX.
    FluidDex = 11,
    /// DODO V2.
    DodoV2 = 12,
    /// KyberSwap Elastic.
    KyberSwapElastic = 13,
    /// Solidly V3.
    SolidlyV3 = 14,
    /// Uniswap V4.
    UniswapV4 = 15,
    /// Balancer V2 ComposableStable / MetaStable / legacy Stable (StableMath).
    BalancerStable = 16,
    /// Balancer V2 Weighted pool (power-mean `Π(B_i/W_i)^W_i` invariant via LogExpMath).
    BalancerWeighted = 17,
}

impl PoolKind {
    /// Map this engine `PoolKind` to the on-chain **Mantis executor** exchange id used in
    /// `TradeHopV2.exchangeId` (see `MantisExecutor.sol`). The two enums are numbered
    /// DIFFERENTLY, so this mapping is the single source of truth for the calldata builder.
    /// `None` for families Mantis's mixed dispatch does not support as a generic leg.
    #[inline]
    pub fn to_mantis_exchange_id(self) -> Option<u8> {
        // SolidlyV3 is FAIL-CLOSED (no Mantis exchange id → route non-submittable). Discovery tags
        // Solidly/Chronos/Ramses STABLE pools (V2-reserves, `x³y+xy³=k` curve) as SolidlyV3 and the
        // engine quotes them with `solidly_stable.rs`, but this kind was mapped to exchange id 12 =
        // RAMSES_CL — a Ramses **concentrated-liquidity V3** fork resolved via `getPool` +
        // `exactInputSingle`, a DIFFERENT contract/AMM. Executing a Solidly-stable quote as a
        // Ramses-CL swap is a wrong-route; the executor's CORRECT Solidly-stable legs are the
        // venue-specific CHRONOS_STABLE_ID/RAMSES_STABLE_ID (V2 callback path), which a single
        // PoolKind cannot select. Until a venue-aware stable mapping is wired, returning None makes
        // `build_mantis_calldata` yield None so submit skips it (REVM also gates it). Quote/state
        // still work for ranking. See engine-state pool.rs solidly_v3_is_fail_closed test.
        if matches!(self, PoolKind::SolidlyV3) {
            return None;
        }
        Some(match self {
            PoolKind::UniswapV3 => 0,
            PoolKind::CamelotV2 => 1,
            PoolKind::SushiV2 => 2,
            PoolKind::PancakeV3 => 6,
            PoolKind::KyberSwapElastic => 7,
            PoolKind::AlgebraV19 => 9, // Camelot V3 / Algebra
            PoolKind::SolidlyV3 => unreachable!("SolidlyV3 is fail-closed above"),
            PoolKind::TraderJoeLb => 15,
            PoolKind::CurveCryptoSwap => 17, // TwoCrypto-NG
            PoolKind::CurveStableSwapNg => 18,
            PoolKind::UniswapV4 => 20,
            PoolKind::FluidDex => 21,
            PoolKind::MaverickV2 => 22,
            PoolKind::DodoV2 => 23,
            PoolKind::UniswapV2 => 24,
            PoolKind::SushiV3 => 25,
            PoolKind::BalancerStable => 26,
            PoolKind::BalancerWeighted => 27,
        })
    }

    /// Whether trading this family routes through the Balancer Vault (so a Balancer flash
    /// loan is impossible and the route must fund via Aave). Mirrors `route_flash_fee_wei`.
    #[inline]
    pub fn is_balancer(self) -> bool {
        matches!(self, PoolKind::BalancerStable | PoolKind::BalancerWeighted)
    }

    /// Whether this is specifically a Balancer **Stable** pool (StableMath snapshot path). Used
    /// where the stable-vs-weighted distinction matters (e.g. which snapshot fetcher to call).
    #[inline]
    pub fn is_balancer_stable(self) -> bool {
        matches!(self, PoolKind::BalancerStable)
    }

    /// Per-swap L2 gas estimate (units) for one hop through this family, used to size the
    /// submit-stage `minNetProfit` floor and the on-wire `gas_limit`. A flat per-hop number
    /// badly under-budgets gas-heavy families — Trader Joe LB bin-crossing swaps measured
    /// ~1.24M gas/leg on an Arbitrum fork (`MantisExecutor.Fork.t.sol`), vs ~160k for a V3
    /// single-tick swap — so a generic `N*hops` formula would OOG an LB route on-chain and
    /// under-state its cost in the profit floor. These are conservative (high) estimates so the
    /// floor never accepts a money-losing arb; the on-wire limit adds margin on top (and unused
    /// gas is refunded on Arbitrum, so over-estimating the limit is free).
    #[inline]
    pub fn swap_gas_estimate(self) -> u64 {
        match self {
            PoolKind::TraderJoeLb => 1_250_000, // bin-crossing LB swaps are by far the heaviest
            PoolKind::FluidDex => 400_000,
            PoolKind::CurveStableSwapNg | PoolKind::CurveCryptoSwap => 350_000,
            PoolKind::MaverickV2 => 300_000,
            PoolKind::DodoV2 => 250_000, // PMM
            PoolKind::BalancerStable | PoolKind::BalancerWeighted => 250_000,
            PoolKind::AlgebraV19 => 220_000, // Camelot V3 / Algebra
            PoolKind::UniswapV3
            | PoolKind::PancakeV3
            | PoolKind::SushiV3
            | PoolKind::KyberSwapElastic
            | PoolKind::SolidlyV3
            | PoolKind::UniswapV4 => 160_000, // concentrated-liquidity single-/few-tick swap
            PoolKind::CamelotV2 | PoolKind::SushiV2 | PoolKind::UniswapV2 => 120_000, // constant-product
        }
    }
}

/// Promotion tier (§5.5).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PoolTier {
    /// Discovered but not yet validated.
    Discovered = 0,
    /// Validated against on-chain code hash and ABI.
    Validated = 1,
    /// State snapshot loaded at block tag B.
    SnapshotReady = 2,
    /// Event/calldata predictor wired.
    EventReady = 3,
    /// Replay parity proven over ≥100k events.
    ReplayReady = 4,
    /// Executor route kind + REVM fixture present.
    ExecutorReady = 5,
    /// All risk gates green.
    TradingReady = 6,
    /// Hot-cap resident.
    HotPool = 7,
}

/// Immutable per-pool metadata.
#[derive(Debug, Clone, Copy)]
pub struct PoolMeta {
    /// 20-byte pool address.
    pub address: Address,
    /// Pool family.
    pub kind: PoolKind,
    /// `token0`.
    pub token0: TokenId,
    /// `token1`.
    pub token1: TokenId,
    /// Pair id (unordered).
    pub pair: PairId,
    /// Venue id (factory).
    pub venue: VenueId,
    /// Pool fee in pips for V3-family, in basis points for V2-family.
    pub fee_pips_or_bps: u32,
    /// Tick spacing for V3-family pools; `0` for non-V3 pools.
    pub tick_spacing: i32,
    /// State-store slot index inside the family-specific state vector.
    pub state_slot: StateSlot,
}

/// Parameter-poison levels (2026-07-10 param-mutation guard). `SOFT` = a repairable
/// parameter went stale (e.g. Algebra tickSpacing mutated — resync re-reads and clears);
/// `HARD` = the pool's live parameters left what the resident kernel can model (Camelot
/// fee/stable-swap mutation, V4 protocol fee set, directional Algebra fees) — stays
/// poisoned until reboot re-snapshots. A poisoned pool's quotes are refused fail-closed.
pub const PARAM_POISON_NONE: u8 = 0;
/// See [`PARAM_POISON_NONE`].
pub const PARAM_POISON_SOFT: u8 = 1;
/// See [`PARAM_POISON_NONE`].
pub const PARAM_POISON_HARD: u8 = 2;

/// Dense SoA pool table.
pub struct PoolTable {
    metas: Vec<PoolMeta>,
    readiness_bits: Vec<u64>,
    tier: Vec<PoolTier>,
    last_update_seq: Vec<u64>,
    last_update_l2_block: Vec<u64>,
    /// Per-pool monotonically increasing generation; `AtomicU64` so the writer increments
    /// without locking.
    generation: Vec<AtomicU64>,
    route_heat: Vec<u32>,
    pool_score_q32: Vec<u64>,
    /// Per-pool parameter-poison level (`PARAM_POISON_*`); `AtomicU8` so the logsub /
    /// resync threads flag and the sim thread reads lock-free.
    param_poison: Vec<AtomicU8>,
    /// WI-06 G4 (codex item 8-residual) per-pool POISON VERSION: bumped on EVERY `poison_param`
    /// SET (even a same-level repeat), so a publisher-owned SOFT clear can verify it is clearing the
    /// SAME poison instance it was verified against. A NEWER poison bumps this past the clear's
    /// stamped version ⇒ the stale clear is a no-op (the pool stays poisoned). Separate from
    /// `generation` (which bumps on every write) so ordinary feed writes never invalidate a clear.
    poison_version: Vec<AtomicU64>,
    /// WI-06 T3 fix round 4 (codex round-4 item 5): per-pool GLOBAL commit-sequence stamp. Every
    /// witnessed `bump_generation` (i.e. every pool commit) advances [`commit_seq`](Self::commit_seq)
    /// and records its new value here. The SHADOW REVM promoter cannot stamp a per-pool generation
    /// BEFORE its replay (pool identity IS the replay output), so its post-replay
    /// `expected_prior_generation` stamp can be defeated by a commit that lands DURING replay. The
    /// shadow instead captures the global `commit_seq` BEFORE replay and the publisher REFUSES the
    /// full-slot write if this pool's `last_commit_seq` advanced past that captured value (the pool
    /// committed since before the replay ⇒ the stale-fork reconstruction can no longer be blessed).
    /// `AtomicU64`: publisher-written under the txn, shadow-read lock-free.
    last_commit_seq: Vec<AtomicU64>,
    /// WI-06 T3 fix round 4 (codex round-4 item 5): monotonic GLOBAL commit sequence, advanced once
    /// per witnessed pool commit (`bump_generation`) and stamped into
    /// [`last_commit_seq`](Self::last_commit_seq). The shadow captures this BEFORE replay as its
    /// `pre_replay_seq` guard floor. Bounded by `u64` (2^64 commits is unreachable).
    commit_seq: AtomicU64,
    /// WI-06 F1 universe brand: every witnessed `bump_generation`/`poison_param` asserts the
    /// txn shares it (WILDCARD until `UniverseBuilder::new` brands the table).
    universe_id: UniverseId,
}

impl PoolTable {
    /// Production constructor: the table is born BRANDED to `universe` (WI-06 F1 totality, codex
    /// item 1). `UniverseBuilder::new` is the SOLE caller — the resident pool table can never be
    /// constructed WILDCARD in a production build, so a foreign permit can never witness it.
    pub(crate) fn new_branded(universe: UniverseId) -> Self {
        Self {
            metas: Vec::new(),
            readiness_bits: Vec::new(),
            tier: Vec::new(),
            last_update_seq: Vec::new(),
            last_update_l2_block: Vec::new(),
            generation: Vec::new(),
            route_heat: Vec::new(),
            pool_score_q32: Vec::new(),
            param_poison: Vec::new(),
            poison_version: Vec::new(),
            last_commit_seq: Vec::new(),
            commit_seq: AtomicU64::new(0),
            universe_id: universe,
        }
    }

    /// Empty WILDCARD table — TEST ONLY (compiled out of every production build via the same cfg
    /// as `StateWriterPermit::test_mint`). Substrate/loom/downstream tests witness it with a
    /// WILDCARD `test_mint` permit.
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn new() -> Self {
        Self::new_branded(UniverseId::WILDCARD)
    }

    /// Pre-reserve capacity for `n` pools so the dense vectors never reallocate — TEST ONLY
    /// (WILDCARD constructor path; gated with `new`).
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn with_capacity(n: usize) -> Self {
        let mut s = Self::new();
        s.metas.reserve_exact(n);
        s.readiness_bits.reserve_exact(n);
        s.tier.reserve_exact(n);
        s.last_update_seq.reserve_exact(n);
        s.last_update_l2_block.reserve_exact(n);
        s.generation.reserve_exact(n);
        s.route_heat.reserve_exact(n);
        s.pool_score_q32.reserve_exact(n);
        s.param_poison.reserve_exact(n);
        s.poison_version.reserve_exact(n);
        s.last_commit_seq.reserve_exact(n);
        s
    }

    /// Insert a pool, returning its id.
    pub fn insert(&mut self, meta: PoolMeta) -> PoolId {
        let id = PoolId(self.metas.len() as u32);
        self.metas.push(meta);
        self.readiness_bits.push(0);
        self.tier.push(PoolTier::Discovered);
        self.last_update_seq.push(0);
        self.last_update_l2_block.push(0);
        self.generation.push(AtomicU64::new(0));
        self.route_heat.push(0);
        self.pool_score_q32.push(0);
        self.param_poison.push(AtomicU8::new(PARAM_POISON_NONE));
        self.poison_version.push(AtomicU64::new(0));
        self.last_commit_seq.push(AtomicU64::new(0));
        id
    }

    /// Get the dense metadata for a pool.
    pub fn meta(&self, id: PoolId) -> &PoolMeta {
        &self.metas[id.0 as usize]
    }

    /// Read the current generation atomically.
    pub fn generation(&self, id: PoolId) -> Generation {
        self.generation[id.0 as usize].load(Ordering::Acquire)
    }

    /// Bump the generation by one (single-writer guarantee). Returns the new generation.
    /// WI-06 §3.3: requires a [`StateWriteTxn`] witness — only the sole state-thread writer
    /// (holding the permit) or the publisher's drain path may advance a pool generation.
    pub fn bump_generation(&self, id: PoolId, _witness: &StateWriteTxn<'_>) -> Generation {
        _witness.assert_universe(self.universe_id);
        let g = self.generation[id.0 as usize].fetch_add(1, Ordering::Release) + 1;
        // WI-06 T3 fix round 4 (codex round-4 item 5): stamp the GLOBAL commit sequence on this pool.
        // Every witnessed pool commit advances the monotonic global counter and records its new value
        // as this pool's `last_commit_seq`, so the SHADOW promoter's pre-replay-seq guard can refuse a
        // stale-fork full-slot write whose pool committed AFTER the shadow captured the counter. Folded
        // into `bump_generation` (the single per-pool commit primitive) so it is TOTAL — no commit site
        // can forget to stamp. Release-store pairs with the shadow's Acquire-load of `commit_seq`.
        //
        // WI-06 T3 fix round 5 (codex round-5 item 5, store-order hygiene): the mutation is
        // sole-writer (this method requires a `StateWriteTxn` witness = the one permit holder), so we
        // RESERVE the next global sequence, stamp THIS pool's `last_commit_seq` FIRST, then publish the
        // global counter LAST. Any observer that sees `commit_seq == seq` is thereby guaranteed to also
        // see this pool's `last_commit_seq >= seq` — the round-4 order published the global counter
        // before the per-pool stamp, letting a shadow capture `seq` while the pool still read the old
        // value. This is monotone-consistency hygiene ONLY; the shadow value-parity (CAS) guard below
        // carries the correctness claim (the seq guard cannot close a lagging fork whose captured seq
        // already equals the committed seq).
        let seq = self.commit_seq.load(Ordering::Relaxed).wrapping_add(1);
        self.last_commit_seq[id.0 as usize].store(seq, Ordering::Release);
        self.commit_seq.store(seq, Ordering::Release);
        g
    }

    /// WI-06 T3 fix round 4 (codex round-4 item 5): the current monotonic GLOBAL commit sequence.
    /// The SHADOW REVM promoter captures this BEFORE its replay (`pre_replay_seq`) and the publisher
    /// refuses its full-slot write if the pool's [`last_commit_seq`](Self::last_commit_seq) advanced
    /// past that captured value. Acquire-load pairs with the publisher's Release stamp in
    /// [`bump_generation`](Self::bump_generation).
    #[inline]
    pub fn commit_seq(&self) -> u64 {
        self.commit_seq.load(Ordering::Acquire)
    }

    /// WI-06 T3 fix round 4 (codex round-4 item 5): the GLOBAL commit sequence at which this pool
    /// last committed. The publisher refuses a shadow full-slot write whose pool `last_commit_seq`
    /// exceeds the shadow's captured `pre_replay_seq` (the pool advanced since before the replay).
    #[inline]
    pub fn last_commit_seq(&self, id: PoolId) -> u64 {
        self.last_commit_seq
            .get(id.0 as usize)
            .map(|v| v.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Current parameter-poison level for a pool (`PARAM_POISON_*`). Relaxed load —
    /// the sim gate tolerates a few-ns-stale read; the poison transition itself is rare.
    #[inline]
    pub fn param_poison(&self, id: PoolId) -> u8 {
        self.param_poison
            .get(id.0 as usize)
            .map(|a| a.load(Ordering::Relaxed))
            .unwrap_or(PARAM_POISON_NONE)
    }

    /// Raise the poison level (monotonic: `fetch_max`, so HARD is never downgraded by a
    /// concurrent SOFT). Returns the previous level. WI-06 G4: also bumps the pool's POISON VERSION
    /// (every SET, even a same-level repeat) so a later publisher-owned clear can tell whether a
    /// NEWER poison intent arrived since the clear was verified.
    pub fn poison_param(&self, id: PoolId, level: u8, _witness: &StateWriteTxn<'_>) -> u8 {
        _witness.assert_universe(self.universe_id);
        let i = id.0 as usize;
        if let Some(v) = self.poison_version.get(i) {
            v.fetch_add(1, Ordering::AcqRel);
        }
        self.param_poison
            .get(i)
            .map(|a| a.fetch_max(level, Ordering::AcqRel))
            .unwrap_or(PARAM_POISON_NONE)
    }

    /// WI-06 G4 (codex item 8-residual): the pool's current poison version (bumped on every
    /// `poison_param` SET). A publisher-owned SOFT clear stamps the version it verified against and
    /// [`clear_soft_poison`](Self::clear_soft_poison) only clears if it still matches.
    #[inline]
    pub fn poison_version(&self, id: PoolId) -> u64 {
        self.poison_version
            .get(id.0 as usize)
            .map(|v| v.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Clear a SOFT poison after a successful resync repair. HARD is sticky by design
    /// (only a reboot re-snapshot restores trust); CAS ensures we never clear HARD.
    ///
    /// WI-06 F1/F4 (codex items 1 + 8): a WITNESSED post-seal mutation. The ONLY caller is the
    /// sole publisher's drain, which executes a batch's `clear_soft` op AFTER the repair write it
    /// rides with commits — so a poison is never cleared off-thread ahead of (or instead of) the
    /// repair that justifies it. A rejected repair batch carries its clear with it, so neither
    /// lands and the pool stays poisoned (fail-closed).
    /// WI-06 G4 (codex item 8-residual): `expected_version` is the poison version the clear was
    /// VERIFIED against (read via [`poison_version`](Self::poison_version) when the resync decided
    /// the params match). The clear is a NO-OP if the pool's current poison version has advanced
    /// past it — a NEWER poison intent arrived since the verification, so the pool must STAY
    /// poisoned rather than be un-poisoned by a stale clear that verified a different instance.
    pub fn clear_soft_poison(
        &self,
        id: PoolId,
        expected_version: u64,
        _witness: &StateWriteTxn<'_>,
    ) -> bool {
        _witness.assert_universe(self.universe_id);
        let i = id.0 as usize;
        // Version guard: a newer poison (bumped version) supersedes this verified-NoChange clear.
        if self.poison_version(id) != expected_version {
            return false;
        }
        self.param_poison
            .get(i)
            .map(|a| {
                a.compare_exchange(
                    PARAM_POISON_SOFT,
                    PARAM_POISON_NONE,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            })
            .unwrap_or(false)
    }

    /// Number of pools.
    pub fn len(&self) -> usize {
        self.metas.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.metas.is_empty()
    }

    /// Slice over all metas (useful for cold control-plane scans only).
    pub fn metas(&self) -> &[PoolMeta] {
        &self.metas
    }

    /// Set the tier and readiness bits for a pool (control-plane / builder pre-seal only).
    ///
    /// WI-06 round 3 (codex item 1 residual): `pub(crate)` — a `&mut self` mutator must NOT be part
    /// of the public post-seal mutation surface. It is reachable only through the builder (pre-seal)
    /// or in-crate control-plane; combined with the sealed universe being handed out `Arc`-wrapped
    /// (no owned `&mut Universe`), there is no path to call it out of band after sealing.
    pub(crate) fn set_tier(&mut self, id: PoolId, tier: PoolTier) {
        self.tier[id.0 as usize] = tier;
    }

    /// Tier read.
    pub fn tier(&self, id: PoolId) -> PoolTier {
        self.tier[id.0 as usize]
    }

    /// Update score (control-plane only). WI-06 round 3 (codex item 1 residual): `pub(crate)` — not
    /// a public post-seal `&mut self` mutation surface.
    pub(crate) fn set_score(&mut self, id: PoolId, score_q32: u64) {
        self.pool_score_q32[id.0 as usize] = score_q32;
    }

    /// Score read.
    pub fn score(&self, id: PoolId) -> u64 {
        self.pool_score_q32[id.0 as usize]
    }

    /// Record an update for a pool: sequence number, L2 block. WI-06 round 3 (codex item 1
    /// residual): `pub(crate)` — not a public post-seal `&mut self` mutation surface.
    pub(crate) fn record_update(&mut self, id: PoolId, seq: u64, l2_block: u64) {
        let i = id.0 as usize;
        self.last_update_seq[i] = seq;
        self.last_update_l2_block[i] = l2_block;
    }

    /// Returns the last observed sequencer sequence number that touched this pool.
    pub fn last_update_seq(&self, id: PoolId) -> u64 {
        self.last_update_seq[id.0 as usize]
    }
}

#[cfg(any(test, loom, feature = "test-permit"))]
impl Default for PoolTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balancer_stable_vs_weighted_classification() {
        // The resync loop routes STABLE pools through fetch_balancer_stable and WEIGHTED through
        // fetch_balancer_weighted. `is_balancer_stable()` MUST exclude weighted (else a weighted
        // pool gets the stable fetcher, which reverts getAmplificationParameter and leaves it
        // stale forever). `is_balancer()` must include BOTH (Vault non-reentrancy / flash gating).
        assert!(PoolKind::BalancerStable.is_balancer_stable());
        assert!(!PoolKind::BalancerWeighted.is_balancer_stable());
        assert!(PoolKind::BalancerStable.is_balancer());
        assert!(PoolKind::BalancerWeighted.is_balancer());
        // Non-balancer families are neither.
        assert!(!PoolKind::UniswapV3.is_balancer());
        assert!(!PoolKind::UniswapV3.is_balancer_stable());
        assert_eq!(PoolKind::BalancerWeighted.to_mantis_exchange_id(), Some(27));
    }

    #[test]
    fn mantis_exchange_id_mapping() {
        // Source of truth: MantisExecutor.sol exchange-id constants. The engine PoolKind enum
        // is numbered DIFFERENTLY, so this mapping must be exact.
        assert_eq!(PoolKind::UniswapV3.to_mantis_exchange_id(), Some(0));
        // SolidlyV3 is fail-closed: quote=stable curve, but its only executor mapping (id 12 =
        // Ramses-CL V3) is a wrong-route, so it has NO Mantis exchange id → non-submittable.
        assert_eq!(PoolKind::SolidlyV3.to_mantis_exchange_id(), None);
        assert_eq!(PoolKind::CamelotV2.to_mantis_exchange_id(), Some(1));
        assert_eq!(PoolKind::SushiV2.to_mantis_exchange_id(), Some(2));
        assert_eq!(PoolKind::PancakeV3.to_mantis_exchange_id(), Some(6));
        assert_eq!(PoolKind::KyberSwapElastic.to_mantis_exchange_id(), Some(7));
        assert_eq!(PoolKind::AlgebraV19.to_mantis_exchange_id(), Some(9));
        assert_eq!(PoolKind::TraderJoeLb.to_mantis_exchange_id(), Some(15));
        assert_eq!(
            PoolKind::CurveStableSwapNg.to_mantis_exchange_id(),
            Some(18)
        );
        assert_eq!(PoolKind::UniswapV4.to_mantis_exchange_id(), Some(20));
        assert_eq!(PoolKind::FluidDex.to_mantis_exchange_id(), Some(21));
        assert_eq!(PoolKind::MaverickV2.to_mantis_exchange_id(), Some(22));
        assert_eq!(PoolKind::DodoV2.to_mantis_exchange_id(), Some(23));
        assert_eq!(PoolKind::UniswapV2.to_mantis_exchange_id(), Some(24));
        assert_eq!(PoolKind::SushiV3.to_mantis_exchange_id(), Some(25));
        assert_eq!(PoolKind::BalancerStable.to_mantis_exchange_id(), Some(26));
        // Only Balancer routes through the (non-reentrant) Vault → must fund via Aave.
        assert!(PoolKind::BalancerStable.is_balancer());
        assert!(!PoolKind::UniswapV3.is_balancer());
    }

    fn dummy_meta(seed: u8) -> PoolMeta {
        let mut addr = [0u8; 20];
        addr[0] = seed;
        PoolMeta {
            address: addr,
            kind: PoolKind::UniswapV2,
            token0: TokenId(0),
            token1: TokenId(1),
            pair: PairId(0),
            venue: VenueId(0),
            fee_pips_or_bps: 30,
            tick_spacing: 0,
            state_slot: StateSlot(seed as u32),
        }
    }

    #[test]
    fn insert_and_generation_increments() {
        let mut t = PoolTable::new();
        let mut wp = crate::publication::StateWriterPermit::test_mint();
        let w = wp.begin_txn();
        let id = t.insert(dummy_meta(1));
        assert_eq!(t.generation(id), 0);
        let g1 = t.bump_generation(id, &w);
        let g2 = t.bump_generation(id, &w);
        assert_eq!(g1, 1);
        assert_eq!(g2, 2);
        assert_eq!(t.generation(id), 2);
    }

    #[test]
    fn tier_round_trip() {
        let mut t = PoolTable::new();
        let id = t.insert(dummy_meta(0));
        assert_eq!(t.tier(id), PoolTier::Discovered);
        t.set_tier(id, PoolTier::HotPool);
        assert_eq!(t.tier(id), PoolTier::HotPool);
    }

    /// Param-poison semantics (2026-07-10 guard): default clean; SOFT set + clear;
    /// HARD is sticky (fetch_max never downgrades, clear_soft never clears HARD);
    /// out-of-range ids are clean no-ops (malformed external input must not panic).
    #[test]
    fn param_poison_levels() {
        let mut t = PoolTable::new();
        let mut wp = crate::publication::StateWriterPermit::test_mint();
        let w = wp.begin_txn();
        let id = t.insert(dummy_meta(0));
        assert_eq!(t.param_poison(id), PARAM_POISON_NONE);

        assert_eq!(t.poison_param(id, PARAM_POISON_SOFT, &w), PARAM_POISON_NONE);
        assert_eq!(t.param_poison(id), PARAM_POISON_SOFT);
        assert!(t.clear_soft_poison(id, t.poison_version(id), &w));
        assert_eq!(t.param_poison(id), PARAM_POISON_NONE);

        t.poison_param(id, PARAM_POISON_HARD, &w);
        // SOFT after HARD must not downgrade…
        t.poison_param(id, PARAM_POISON_SOFT, &w);
        assert_eq!(t.param_poison(id), PARAM_POISON_HARD);
        // …and clear_soft must not clear HARD.
        assert!(!t.clear_soft_poison(id, t.poison_version(id), &w));
        assert_eq!(t.param_poison(id), PARAM_POISON_HARD);

        // Unknown id: clean, no panic.
        assert_eq!(t.param_poison(PoolId(999)), PARAM_POISON_NONE);
        assert_eq!(t.poison_param(PoolId(999), PARAM_POISON_HARD, &w), PARAM_POISON_NONE);
        assert!(!t.clear_soft_poison(PoolId(999), 0, &w));
    }

    /// WI-06 G4 (codex item 8-residual): a publisher-owned SOFT clear stamped with the poison
    /// version it VERIFIED against must be a NO-OP once a NEWER poison intent has bumped the version
    /// — the pool stays poisoned rather than being un-poisoned by a stale clear.
    #[test]
    fn stale_clear_against_a_newer_poison_is_a_noop() {
        let mut t = PoolTable::new();
        let mut wp = crate::publication::StateWriterPermit::test_mint();
        let w = wp.begin_txn();
        let id = t.insert(dummy_meta(0));
        // SOFT poison instance A; capture the version the clear will be verified against.
        t.poison_param(id, PARAM_POISON_SOFT, &w);
        let verified = t.poison_version(id);
        // A NEWER SOFT poison intent B arrives (drift re-detected) — the version advances past A.
        t.poison_param(id, PARAM_POISON_SOFT, &w);
        assert_ne!(t.poison_version(id), verified, "a new poison SET must bump the version");
        // The clear verified against A is stale ⇒ NO-OP; the newer poison B stays.
        assert!(
            !t.clear_soft_poison(id, verified, &w),
            "a stale clear (older poison version) must not clear a newer poison"
        );
        assert_eq!(t.param_poison(id), PARAM_POISON_SOFT, "the newer poison must stay");
        // A clear stamped with the CURRENT version does clear it.
        assert!(t.clear_soft_poison(id, t.poison_version(id), &w));
        assert_eq!(t.param_poison(id), PARAM_POISON_NONE);
    }
}
