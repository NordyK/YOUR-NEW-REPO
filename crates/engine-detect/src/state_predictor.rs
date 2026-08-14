//! State predictor: maps a decoded `RouteIntent` to a list of pools the transaction will
//! dirty, plus the post-swap state for each touched pool.
//!
//! Walks the intent's hops in order. For each hop:
//! 1. Resolve the pool id from the address registry (direct pool address) or from the
//!    `(token0, token1, fee_pips)` triple via the venue's deterministic address rule.
//! 2. Load the pool's current state from the V2 or V3 store.
//! 3. Apply the swap using the exact-integer AMM kernel.
//! 4. Emit the predicted post-swap state plus the dirtied `PoolId`.
//!
//! Multi-hop swaps thread the previous hop's output amount into the next hop's input. If
//! any hop's resolution fails (pool not in the resident table) the predictor returns the
//! partial list so the engine can still pick up later state changes via background
//! reconciliation. Plan §10.3 covers this fail-closed behaviour.

use alloy_primitives::U256;
use arrayvec::ArrayVec;
use thiserror::Error;

use engine_amm::balancer_stable::BalancerStableState;
use engine_amm::curve::StableSwapState;
use engine_amm::dodo_v2::DodoState;
use engine_amm::maverick_v2::MaverickState;
use engine_amm::trader_joe_lb::LBState;
use engine_amm::{curve_apply_exchange, v2_get_amount_out, TickInfo, V3Slot0};
use engine_state::{
    AddressRegistry, BalancerStateStore, BalancerWeightedSlot, BalancerWeightedStateStore,
    CryptoCurveSlot, CryptoCurveStateStore, CurveStateStore, DodoStateStore, FluidPoolSnapshot,
    FluidStateStore, LbStateStore, MaverickStateStore, PoolId, PoolKind, PoolMeta, PoolTable,
    TokenTable, V2Reserves256, V2State, V2StateStore, V3State, V3StateStore,
};

use crate::router_decoder::{Direction, RouteIntent};

/// Maximum pool dirty events per single tx.
pub const MAX_DIRTY_POOLS: usize = 8;

/// Per-pool prediction.
#[derive(Debug, Clone)]
pub enum PoolPrediction {
    /// V2-family update.
    V2 {
        /// Dense id.
        pool_id: PoolId,
        /// New reserves.
        new_state: V2State,
    },
    /// V3-family update.
    V3 {
        /// Dense id.
        pool_id: PoolId,
        /// New state (sqrtPrice, tick, liquidity, ticks unchanged).
        new_state: V3State,
    },
    /// Curve StableSwap-NG update (C2): the post-`exchange` kernel (balances moved by ±dx/dy).
    /// The slot's token addresses are unchanged and preserved on commit.
    Curve {
        /// Dense id.
        pool_id: PoolId,
        /// New kernel (balances updated; A/fee/rates unchanged).
        new_kernel: StableSwapState,
    },
    /// Curve CryptoSwap-NG update (C2): the post-`exchange` slot (active kernel's balances moved,
    /// `D` re-solved; `price_scale` held — its rate-limited repeg is re-priced by REVM). Covers
    /// both Tricrypto-NG (3-coin) and TwoCrypto-NG (2-coin); the slot's token map is preserved.
    CurveCrypto {
        /// Dense id.
        pool_id: PoolId,
        /// New slot (active kernel updated; tokens/n_coins preserved).
        new_slot: Box<CryptoCurveSlot>,
    },
    /// DODO V2 update (C2): post-swap reserves + PMM fields (r_state, targets); i/K unchanged.
    Dodo {
        /// Dense id.
        pool_id: PoolId,
        /// New full DODO state.
        new_state: Box<DodoState>,
    },
    /// Balancer V2 Stable update (C2): post-swap balances (amp/scaling/fee unchanged).
    BalancerStable {
        /// Dense id.
        pool_id: PoolId,
        /// New kernel (balances updated).
        new_kernel: Box<BalancerStableState>,
    },
    /// Trader Joe LB update (C2): post-swap active_id + traversed-bin reserves (fee params as-is).
    Lb {
        /// Dense id.
        pool_id: PoolId,
        /// New LB kernel (active_id + bin_map updated).
        new_kernel: Box<LBState>,
    },
    /// Balancer V2 Weighted update (C2): post-swap `balances_d18` (weights/fee unchanged).
    BalancerWeighted {
        /// Dense id.
        pool_id: PoolId,
        /// New slot (balances_d18 updated).
        new_slot: Box<BalancerWeightedSlot>,
    },
    /// Fluid DEX v1 update (C2): the post-swap snapshot — each routed side's price-determining
    /// imaginary reserves moved (constant-product); limits + real reserves carried as-is. Only the
    /// in-scope v1 family is predicted; v2/D3/D4 hops are not emitted.
    Fluid {
        /// Dense id.
        pool_id: PoolId,
        /// New Fluid snapshot (v1 imaginary reserves updated).
        new_snapshot: Box<FluidPoolSnapshot>,
    },
    /// Maverick V2 update (C2): post-swap `active_tick` + `sqrt_price` (per-tick liquidity is the
    /// concentrated-liquidity invariant a swap doesn't change — the quote reads liquidity + sqrt
    /// bounds, not per-tick reserves). Forward-apply is wired but DORMANT: `quote_one_hop` keeps
    /// Maverick `=> None` per operator directive, so detection never reads this until the venue is
    /// enabled (mirrors how `predict_kyber_hop` exists behind the KYBER_ENABLE-gated quote).
    Maverick {
        /// Dense id.
        pool_id: PoolId,
        /// New Maverick state (active_tick + sqrt_price updated).
        new_state: Box<MaverickState>,
    },
}

impl PoolPrediction {
    /// Return the dirtied pool id regardless of family.
    pub fn pool_id(&self) -> PoolId {
        match self {
            PoolPrediction::V2 { pool_id, .. } => *pool_id,
            PoolPrediction::V3 { pool_id, .. } => *pool_id,
            PoolPrediction::Curve { pool_id, .. } => *pool_id,
            PoolPrediction::CurveCrypto { pool_id, .. } => *pool_id,
            PoolPrediction::Dodo { pool_id, .. } => *pool_id,
            PoolPrediction::BalancerStable { pool_id, .. } => *pool_id,
            PoolPrediction::Lb { pool_id, .. } => *pool_id,
            PoolPrediction::BalancerWeighted { pool_id, .. } => *pool_id,
            PoolPrediction::Fluid { pool_id, .. } => *pool_id,
            PoolPrediction::Maverick { pool_id, .. } => *pool_id,
        }
    }
}

/// Errors during prediction.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum PredictionError {
    /// Pool address not in the resident registry.
    #[error("unknown pool address")]
    UnknownPool,
    /// V3 hop direction could not be determined (tokens not in the pool).
    #[error("v3 hop direction unresolvable")]
    V3DirectionUnresolvable,
    /// AMM kernel returned an error.
    #[error("amm kernel error")]
    Amm,
    /// Too many hops to fit in the fixed-size output vec.
    #[error("too many dirty pools")]
    TooManyPools,
    /// The hop touches a pool family the predictor cannot yet apply a state
    /// delta for (DODO / Maverick / Fluid / Solidly). Returned instead of
    /// emitting a wrong-store prediction — the dense state slots are per-store,
    /// so writing a V2 delta at a DODO pool's slot would corrupt an unrelated V2
    /// pool. The whole intent is dropped rather than corrupt resident state.
    #[error("unsupported pool family for state prediction")]
    UnsupportedFamily,
}

/// Resolver used by the predictor to map addresses → pool ids.
pub struct PredictorContext<'a> {
    /// Address → pool id registry.
    pub registry: &'a AddressRegistry,
    /// Dense pool metadata.
    pub pool_table: &'a PoolTable,
    /// Token metadata (decimals) — needed by the Solidly stable forward-apply for its
    /// decimal-normalized curve. Cold-path read; not used by the V2/V3 hot families.
    pub tokens: &'a TokenTable,
    /// V2 state store.
    pub v2_store: &'a V2StateStore,
    /// V3 state store.
    pub v3_store: &'a V3StateStore,
    /// Curve StableSwap-NG state store (C2 predictor).
    pub curve_store: &'a CurveStateStore,
    /// Curve CryptoSwap-NG state store (C2 predictor — Tricrypto/TwoCrypto).
    pub curve_crypto_store: &'a CryptoCurveStateStore,
    /// DODO V2 state store (C2 predictor).
    pub dodo_store: &'a DodoStateStore,
    /// Balancer V2 Stable state store (C2 predictor).
    pub balancer_store: &'a BalancerStateStore,
    /// Trader Joe LB state store (C2 predictor).
    pub lb_store: &'a LbStateStore,
    /// Balancer V2 Weighted state store (C2 predictor).
    pub balancer_weighted_store: &'a BalancerWeightedStateStore,
    /// Fluid DEX state store (C2 predictor — v1 forward-apply).
    pub fluid_store: &'a FluidStateStore,
    /// Maverick V2 state store (C2 predictor — dormant behind the operator-gated quote).
    pub maverick_store: &'a MaverickStateStore,
}

/// Apply a route intent and emit per-pool predictions.
pub fn predict_route(
    ctx: &PredictorContext<'_>,
    intent: &RouteIntent,
) -> Result<ArrayVec<PoolPrediction, MAX_DIRTY_POOLS>, PredictionError> {
    let mut out: ArrayVec<PoolPrediction, MAX_DIRTY_POOLS> = ArrayVec::new();
    if intent.hops.is_empty() {
        return Ok(out);
    }
    // [Codex#1] Exact-OUTPUT intents carry amount_in = 0 (only the desired output is in the calldata,
    // not the input). Forward-applying threads 0 through the hops, and predict_v3_hop /
    // predict_kyber_hop then return the pool's UNCHANGED pre-swap state as a "prediction" — which
    // commit_prediction writes to the mirror and a DirtyEvent re-evaluates arbs against (stale prices),
    // and it can suppress the targeted resync. There is no reverse (output->input) kernel for
    // concentrated-liquidity V3, so we do NOT forward-apply exact-output: drop it here so the touched
    // pools are refreshed by the resync / shadow-REVM path with TRUE post-swap state. (The V2 kernel
    // has getAmountIn, but V2 already yields no stale prediction on amount_in = 0; a feed-latency
    // back-solve for V2/V3 exact-output is a tracked future enhancement — see the audit dossier.)
    if !intent.exact_input {
        return Ok(out);
    }
    let mut current_amount = intent.amount_in;
    // Best-effort: emit predictions for the leading hops we can resolve and apply,
    // and stop at the first hop we can't (pool not in our universe, unsupported
    // family, or an AMM kernel error). The amount thread breaks there anyway, so the
    // accumulated prefix is the most we can soundly predict; the rest is left for
    // background reconciliation. Returning the partial list (rather than dropping
    // the whole intent) is what lets a mixed route — e.g. a Uniswap-V3 hop followed
    // by a Balancer hop — still refresh the V3 pool it touched.
    for hop in &intent.hops {
        let pool_id = match hop.pool {
            Some(addr) => match ctx.registry.pool_of(&addr) {
                Some(id) => id,
                None => break,
            },
            None => match resolve_by_tokens(ctx, &hop.token_in, &hop.token_out, hop.fee_pips) {
                Some(id) => id,
                None => break,
            },
        };
        // C1 same-pool guard: if an earlier hop in THIS route already produced a prediction for
        // this pool, stop here (keep the accumulated prefix — the same best-effort contract as the
        // resolve/family/kernel breaks above). Each predict_*_hop reads the pool's CURRENT store
        // snapshot, NOT the earlier hop's predicted state, so a second prediction for the same pool
        // would be (a) computed from a stale pre-first-hop base and (b) full-overwriting on commit
        // (commit_prediction replaces the slot + the DirtyEvent would carry a duplicate PoolId),
        // silently dropping the first hop's delta. Emitting that wrong second prediction is worse
        // than emitting none, so we keep the correct first-touch state and leave the pool's
        // remaining same-block deltas to background reconciliation. (Correctly composing same-pool
        // hops would require re-basing each on the prior prediction's state — which the per-store
        // snapshot read does not do; the on-chain `minNetProfit` revert backstops any mispredict.)
        if out.iter().any(|p| p.pool_id() == pool_id) {
            break;
        }
        let meta = ctx.pool_table.meta(pool_id);
        let direction = match resolve_direction(ctx, meta, hop, &hop.token_in) {
            Ok(d) => d,
            Err(_) => break,
        };
        let predicted = match meta.kind {
            PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
                predict_v2_hop(ctx, pool_id, meta, direction, current_amount, &mut current_amount)
            }
            PoolKind::UniswapV3
            | PoolKind::SushiV3
            | PoolKind::PancakeV3
            | PoolKind::AlgebraV19
            // V4 pools are registered hookless-only (factory_v4 filters hooks == address(0)), so a
            // V4 swap is pure concentrated-liquidity math and forward-applies through the V3 kernel
            // on the shared V3State. (Hooked pools never enter the universe, so no per-hop guard.)
            | PoolKind::UniswapV4 => {
                predict_v3_hop(ctx, pool_id, meta, direction, current_amount, &mut current_amount)
            }
            PoolKind::KyberSwapElastic => {
                predict_kyber_hop(ctx, pool_id, meta, direction, current_amount, &mut current_amount)
            }
            PoolKind::CurveStableSwapNg => predict_curve_hop(
                ctx,
                pool_id,
                meta,
                &hop.token_in,
                &hop.token_out,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::CurveCryptoSwap => predict_curve_crypto_hop(
                ctx,
                pool_id,
                meta,
                &hop.token_in,
                &hop.token_out,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::SolidlyV3 => predict_solidly_hop(
                ctx,
                pool_id,
                meta,
                direction,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::FluidDex => predict_fluid_hop(
                ctx,
                pool_id,
                meta,
                direction,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::DodoV2 => predict_dodo_hop(
                ctx,
                pool_id,
                meta,
                &hop.token_in,
                &hop.token_out,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::BalancerStable => predict_balancer_stable_hop(
                ctx,
                pool_id,
                meta,
                &hop.token_in,
                &hop.token_out,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::TraderJoeLb => {
                predict_lb_hop(ctx, pool_id, meta, direction, current_amount, &mut current_amount)
            }
            PoolKind::BalancerWeighted => predict_balancer_weighted_hop(
                ctx,
                pool_id,
                meta,
                &hop.token_in,
                &hop.token_out,
                current_amount,
                &mut current_amount,
            ),
            PoolKind::MaverickV2 => predict_maverick_hop(
                ctx,
                pool_id,
                meta,
                direction,
                current_amount,
                &mut current_amount,
            ),
            // EXHAUSTIVE: every in-scope PoolKind now has a same-block forward-apply. A new family
            // added to PoolKind will fail to compile here (forcing an explicit predictor decision)
            // rather than silently falling through. Per-hop errors still break the loop and keep the
            // accumulated prefix (handled below via `match predicted { Err(_) => break }`).
        };
        let prediction = match predicted {
            Ok(p) => p,
            Err(_) => break,
        };
        if out.try_push(prediction).is_err() {
            break;
        }
    }
    Ok(out)
}

fn predict_v2_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let state = ctx.v2_store.snapshot(meta.state_slot);
    let (r_in, r_out) = match direction {
        Direction::ZeroForOne => (state.reserves.reserve0, state.reserves.reserve1),
        Direction::OneForZero => (state.reserves.reserve1, state.reserves.reserve0),
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    let amount_in_u128 = u256_to_u128(amount_in).ok_or(PredictionError::Amm)?;
    let amount_out = v2_get_amount_out(amount_in_u128, r_in, r_out, meta.fee_pips_or_bps)
        .map_err(|_| PredictionError::Amm)?;
    let (new_r0, new_r1) = match direction {
        Direction::ZeroForOne => (
            state.reserves.reserve0.saturating_add(amount_in_u128),
            state.reserves.reserve1.saturating_sub(amount_out),
        ),
        Direction::OneForZero => (
            state.reserves.reserve0.saturating_sub(amount_out),
            state.reserves.reserve1.saturating_add(amount_in_u128),
        ),
        Direction::Unknown => unreachable!(),
    };
    let new_state = V2State {
        reserves: V2Reserves256 {
            reserve0: new_r0,
            reserve1: new_r1,
            block_timestamp_last: state.reserves.block_timestamp_last,
        },
    };
    *next_amount = U256::from(amount_out);
    Ok(PoolPrediction::V2 { pool_id, new_state })
}

/// C2: predict a Curve StableSwap-NG `exchange`. Resolves the coin indices from the hop's token
/// ADDRESSES (Curve is n-coin, not binary) via the slot's `index_of`, then applies the kernel's
/// `apply_exchange` (balances ±dx/dy; admin-fee approximation corrected by the next TokenExchange
/// log). Leaves A/fee/rates and the token map untouched.
fn predict_curve_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let slot = ctx.curve_store.snapshot(meta.state_slot);
    let i = slot
        .index_of(token_in)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let j = slot
        .index_of(token_out)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let dx = u256_to_u128(amount_in).ok_or(PredictionError::Amm)?;
    let (dy, new_kernel) =
        curve_apply_exchange(&slot.kernel, i, j, dx).map_err(|_| PredictionError::Amm)?;
    *next_amount = U256::from(dy);
    Ok(PoolPrediction::Curve {
        pool_id,
        new_kernel,
    })
}

/// C2: predict a Curve CryptoSwap-NG `exchange`. Resolves coin indices from the slot's token map and
/// forward-applies through the active kernel (3-coin Tricrypto or 2-coin TwoCrypto) — balances move,
/// `D` re-solves, token map preserved. Amounts stay in `U256` (crypto balances are 1e18-scale, so a
/// u128 round-trip could truncate). Closes the Tricrypto/TwoCrypto same-block blind spot.
fn predict_curve_crypto_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let slot = ctx.curve_crypto_store.snapshot(meta.state_slot);
    let i = slot
        .index_of(token_in)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let j = slot
        .index_of(token_out)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let (dy, new_slot) = slot
        .apply_exchange(i, j, amount_in)
        .ok_or(PredictionError::Amm)?;
    *next_amount = dy;
    Ok(PoolPrediction::CurveCrypto {
        pool_id,
        new_slot: Box::new(new_slot),
    })
}

/// C2: predict a Fluid DEX v1 swap. Loads the snapshot, maps the hop direction, and forward-applies
/// via the v1 kernel (each routed side's imaginary reserves move; limits + real reserves carried
/// as-is). v2/D3/D4 snapshots are out-of-scope and stop the prefix (kept, never corrupted). `now =
/// u64::MAX` fully expands native limits — the triggering swap was already sequencer-accepted so its
/// limit gate must not reject, and the predicted arb is re-validated by REVM against real limits.
fn predict_fluid_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let snap = ctx.fluid_store.snapshot(meta.state_slot);
    let swap_dir = match direction {
        Direction::ZeroForOne => engine_amm::fluid::SwapDirection::ZeroToOne,
        Direction::OneForZero => engine_amm::fluid::SwapDirection::OneToZero,
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    match &*snap {
        FluidPoolSnapshot::V1(v1) => {
            let (out, new_v1) = v1
                .apply_exact_in(swap_dir, amount_in, u64::MAX)
                .map_err(|_| PredictionError::Amm)?;
            *next_amount = out;
            Ok(PoolPrediction::Fluid {
                pool_id,
                new_snapshot: Box::new(FluidPoolSnapshot::V1(new_v1)),
            })
        }
        FluidPoolSnapshot::V2 { .. } => Err(PredictionError::UnsupportedFamily),
    }
}

/// C2: predict a Maverick V2 swap. Quotes the swap and updates `active_tick` + `sqrt_price` — the
/// concentrated-liquidity quote inputs. Per-tick liquidity is unchanged by a swap (the quote reads
/// `liquidity_d18` + sqrt bounds, NOT per-tick reserves), so this is a complete forward-apply for
/// the next quote. DORMANT: `quote_one_hop` keeps Maverick `=> None` per operator directive, so
/// detection never reads the predicted state until the venue is enabled (a refresh path + live
/// reference vectors are the enable prerequisites). Wired for parity with the other CL families,
/// exactly as `predict_kyber_hop` is wired behind the KYBER_ENABLE-gated quote.
fn predict_maverick_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let state = ctx.maverick_store.snapshot(meta.state_slot);
    let swap_dir = match direction {
        Direction::ZeroForOne => engine_amm::maverick_v2::SwapDirection::ZeroToOne,
        Direction::OneForZero => engine_amm::maverick_v2::SwapDirection::OneToZero,
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    let quote = state
        .quote_exact_in_local(amount_in, swap_dir, None)
        .map_err(|_| PredictionError::Amm)?;
    if quote.amount_out_native.is_zero() {
        return Err(PredictionError::Amm);
    }
    let mut new_state = (*state).clone();
    new_state.active_tick = quote.final_tick;
    new_state.sqrt_price_d18 = quote.final_sqrt_price_d18;
    *next_amount = quote.amount_out_native;
    Ok(PoolPrediction::Maverick {
        pool_id,
        new_state: Box::new(new_state),
    })
}

/// C2: predict a Solidly/Velodrome-style **stable** swap. SolidlyV3 reserves live in `v2_state`
/// (grouped with the V2 family for storage + Sync-log refresh), but the price curve is the
/// decimal-normalized `x³y + xy³ = k` solver — so the reserve update is the V2 update
/// (`r_in += amount_in`, `r_out -= amount_out`) with `amount_out` from the stable kernel. Emits a
/// `PoolPrediction::V2` (the post-swap reserves), which commits to `v2_state` like any V2 pool.
fn predict_solidly_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let state = ctx.v2_store.snapshot(meta.state_slot);
    let zero_for_one = match direction {
        Direction::ZeroForOne => true,
        Direction::OneForZero => false,
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    let (r_in, r_out, dec_in, dec_out) = if zero_for_one {
        (
            state.reserves.reserve0,
            state.reserves.reserve1,
            ctx.tokens.get(meta.token0).decimals,
            ctx.tokens.get(meta.token1).decimals,
        )
    } else {
        (
            state.reserves.reserve1,
            state.reserves.reserve0,
            ctx.tokens.get(meta.token1).decimals,
            ctx.tokens.get(meta.token0).decimals,
        )
    };
    if r_in == 0 || r_out == 0 {
        return Err(PredictionError::Amm);
    }
    let fee_bps: u16 = meta.fee_pips_or_bps.min(u16::MAX as u32) as u16;
    let amount_out_u256 = engine_amm::solidly_stable::calculate_stable_swap_output_normalized(
        U256::from(r_in),
        U256::from(r_out),
        amount_in,
        fee_bps,
        dec_in,
        dec_out,
    )
    .map_err(|_| PredictionError::Amm)?;
    let amount_out = u256_to_u128(amount_out_u256).ok_or(PredictionError::Amm)?;
    let amount_in_u128 = u256_to_u128(amount_in).ok_or(PredictionError::Amm)?;
    if amount_out == 0 {
        return Err(PredictionError::Amm);
    }
    let (new_r0, new_r1) = if zero_for_one {
        (
            state.reserves.reserve0.saturating_add(amount_in_u128),
            state.reserves.reserve1.saturating_sub(amount_out),
        )
    } else {
        (
            state.reserves.reserve0.saturating_sub(amount_out),
            state.reserves.reserve1.saturating_add(amount_in_u128),
        )
    };
    *next_amount = U256::from(amount_out);
    Ok(PoolPrediction::V2 {
        pool_id,
        new_state: V2State {
            reserves: V2Reserves256 {
                reserve0: new_r0,
                reserve1: new_r1,
                block_timestamp_last: state.reserves.block_timestamp_last,
            },
        },
    })
}

/// C2: predict a DODO V2 swap. Resolves the base/quote direction from the hop's token addresses,
/// then applies the kernel's `apply_swap` (reserves + post-trade PMM r_state/targets; i/K unchanged,
/// reconciled by the targeted resync).
fn predict_dodo_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let state = ctx.dodo_store.snapshot(meta.state_slot);
    let direction = state
        .direction_for_tokens(
            alloy_primitives::Address::from(*token_in),
            alloy_primitives::Address::from(*token_out),
        )
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let (dy, new_state) = state
        .apply_swap(amount_in, direction)
        .map_err(|_| PredictionError::Amm)?;
    *next_amount = dy;
    Ok(PoolPrediction::Dodo {
        pool_id,
        new_state: Box::new(new_state),
    })
}

/// C2: predict a Balancer V2 Stable swap. Resolves token indices from the slot's token map, applies
/// the kernel's `apply_swap` (balances move; amp/scaling/fee unchanged). Closes the Balancer 30 s
/// blind spot (no event path) at sub-block latency, reconciled by the next resync.
fn predict_balancer_stable_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let slot = ctx.balancer_store.snapshot(meta.state_slot);
    let i = slot
        .index_of(token_in)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let j = slot
        .index_of(token_out)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let dx = u256_to_u128(amount_in).ok_or(PredictionError::Amm)?;
    let (dy, new_kernel) = engine_amm::balancer_stable::apply_swap(&slot.kernel, i, j, dx)
        .map_err(|_| PredictionError::Amm)?;
    *next_amount = U256::from(dy);
    Ok(PoolPrediction::BalancerStable {
        pool_id,
        new_kernel: Box::new(new_kernel),
    })
}

/// C2: predict a Trader Joe LB swap. `zero_for_one` = selling token0 (binary direction, like V2/V3).
/// Applies the kernel's `apply_swap` (active_id + traversed-bin reserves in a cloned bin window).
fn predict_lb_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let zero_for_one = match direction {
        Direction::ZeroForOne => true,
        Direction::OneForZero => false,
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    let slot = ctx.lb_store.snapshot(meta.state_slot);
    let (dy, new_kernel) =
        engine_amm::trader_joe_lb::apply_swap(&slot.kernel, amount_in, zero_for_one)
            .ok_or(PredictionError::Amm)?;
    *next_amount = dy;
    Ok(PoolPrediction::Lb {
        pool_id,
        new_kernel: Box::new(new_kernel),
    })
}

/// C2: predict a Balancer V2 Weighted swap. The slot stores `balances_d18`; the swap fee stays in
/// the pool, so `balances_d18[in] += amount_in_d18` (full input) and `balances_d18[out] -= out_d18`
/// (the kernel output on the after-fee input). Uses the slot's `scaling_d18` to convert the raw
/// amount (so no token table is needed). weights/fee unchanged.
fn predict_balancer_weighted_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    const ONE: u128 = 1_000_000_000_000_000_000;
    let slot = ctx.balancer_weighted_store.snapshot(meta.state_slot);
    let i = slot
        .index_of(token_in)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let j = slot
        .index_of(token_out)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    let one = U256::from(ONE);
    let amount_in_d18 = amount_in
        .checked_mul(slot.scaling_d18[i])
        .ok_or(PredictionError::Amm)?;
    let after_fee = amount_in_d18
        .checked_mul(
            one.checked_sub(slot.swap_fee_d18)
                .ok_or(PredictionError::Amm)?,
        )
        .ok_or(PredictionError::Amm)?
        / one;
    let out_d18 = engine_amm::balancer_weighted::calc_out_given_in(
        slot.balances_d18[i],
        slot.weights_d18[i],
        slot.balances_d18[j],
        slot.weights_d18[j],
        after_fee,
    )
    .map_err(|_| PredictionError::Amm)?;
    if out_d18.is_zero() || out_d18 >= slot.balances_d18[j] {
        return Err(PredictionError::Amm);
    }
    let mut new_slot = (*slot).clone();
    new_slot.balances_d18[i] = slot.balances_d18[i] + amount_in_d18;
    new_slot.balances_d18[j] = slot.balances_d18[j] - out_d18;
    // next hop's input is in raw token-out units.
    let scale_out = if slot.scaling_d18[j].is_zero() {
        one
    } else {
        slot.scaling_d18[j]
    };
    *next_amount = out_d18 / scale_out;
    Ok(PoolPrediction::BalancerWeighted {
        pool_id,
        new_slot: Box::new(new_slot),
    })
}

fn predict_v3_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let state = ctx.v3_store.snapshot(meta.state_slot);
    let zero_for_one = match direction {
        Direction::ZeroForOne => true,
        Direction::OneForZero => false,
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    let slot0 = V3Slot0 {
        sqrt_price_x96: state.sqrt_price_x96,
        tick: state.tick,
        fee_pips: state.fee_pips,
        liquidity: state.liquidity,
        tick_spacing: state.tick_spacing,
    };
    // Adapt initialised ticks to the kernel's `TickInfo` slice.
    let ticks: Vec<TickInfo> = state.initialised_ticks.clone();
    if amount_in.is_zero() {
        // No predicted amount → snapshot unchanged.
        *next_amount = U256::ZERO;
        return Ok(PoolPrediction::V3 {
            pool_id,
            new_state: state,
        });
    }
    // State prediction consumes `tick_after`, so use the exact-tick variant. (The hot sizing
    // path uses `v3_get_amount_out`, which skips the terminal full-range tick search.)
    let outcome =
        engine_amm::v3_get_amount_out_with_final_tick(slot0, amount_in, zero_for_one, &ticks)
            .map_err(|_| PredictionError::Amm)?;
    let mut new_state = state;
    new_state.sqrt_price_x96 = outcome.sqrt_price_x96_after;
    new_state.tick = outcome.tick_after;
    new_state.liquidity = outcome.liquidity_after;
    *next_amount = outcome.amount_out;
    Ok(PoolPrediction::V3 { pool_id, new_state })
}

/// Predict a KyberSwap Elastic hop. Kyber shares the V3State shape but needs its
/// own fee-reinvestment kernel. The kernel surfaces `(amount_out, final_sqrt)`
/// but not `tick_after` / `liquidity_after`; we derive the post-swap tick from
/// the final sqrt price (the price move is the arb signal) and keep the warm
/// liquidity window — intra-window liquidity drift is second-order for
/// detection, and the next real snapshot reconciles it.
fn predict_kyber_hop(
    ctx: &PredictorContext<'_>,
    pool_id: PoolId,
    meta: &PoolMeta,
    direction: Direction,
    amount_in: U256,
    next_amount: &mut U256,
) -> Result<PoolPrediction, PredictionError> {
    let state = ctx.v3_store.snapshot(meta.state_slot);
    let zero_for_one = match direction {
        Direction::ZeroForOne => true,
        Direction::OneForZero => false,
        Direction::Unknown => return Err(PredictionError::V3DirectionUnresolvable),
    };
    if amount_in.is_zero() {
        *next_amount = U256::ZERO;
        return Ok(PoolPrediction::V3 {
            pool_id,
            new_state: state,
        });
    }
    let amt = u256_to_u128(amount_in).ok_or(PredictionError::Amm)?;
    let (amount_out, final_sqrt) = engine_amm::kyber_elastic::get_amount_out(
        amt,
        state.sqrt_price_x96,
        U256::from(state.liquidity),
        state.tick,
        state.fee_pips,
        zero_for_one,
        &state.initialised_ticks,
    )
    .map_err(|_| PredictionError::Amm)?;
    let mut new_state = state;
    new_state.sqrt_price_x96 = final_sqrt;
    new_state.tick = engine_amm::tick_at_sqrt_price(final_sqrt).unwrap_or(new_state.tick);
    *next_amount = U256::from(amount_out);
    Ok(PoolPrediction::V3 { pool_id, new_state })
}

fn resolve_direction(
    ctx: &PredictorContext<'_>,
    meta: &PoolMeta,
    hop: &crate::router_decoder::DecodedHop,
    token_in: &[u8; 20],
) -> Result<Direction, PredictionError> {
    if hop.direction != Direction::Unknown {
        return Ok(hop.direction);
    }
    let token_in_id = ctx
        .registry
        .token_of(token_in)
        .ok_or(PredictionError::V3DirectionUnresolvable)?;
    if token_in_id == meta.token0 {
        Ok(Direction::ZeroForOne)
    } else if token_in_id == meta.token1 {
        Ok(Direction::OneForZero)
    } else {
        Err(PredictionError::V3DirectionUnresolvable)
    }
}

fn resolve_by_tokens(
    ctx: &PredictorContext<'_>,
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    fee_pips: u32,
) -> Option<PoolId> {
    let candidates = ctx.registry.pools_for_pair(token_in, token_out);
    if candidates.is_empty() {
        return None;
    }
    if fee_pips == 0 {
        // V2-router / pair-only path. A `fee_pips == 0` query carries no fee tier and `DecodedHop`
        // no venue field, so we resolve on the token pair alone, returning the FIRST V2-family
        // pool (Uniswap-V2 / Sushi-V2 / Camelot-V2). This is the deliberate, capital-safe behavior:
        //
        // C2 — multiple V2 venues on one pair (e.g. a Uniswap-V2 AND a Sushi-V2 WETH/USDC pool):
        // we cannot tell which one the swap hit without the router identity. `classify_and_predict`
        // has the router `to` address but the engine has no router→venue/factory map to turn it
        // into a concrete pool, so exact resolution is not yet possible. We do NOT fail closed
        // here: returning `None` would drop the same-block prediction, and the dirty-fanout/resync
        // fallback does NOT recover it for a V2-router swap — that calldata is a TOKEN path with no
        // pool address to sniff (only the live receipt-log replayer names the pool, off the
        // hot path). Dropping it therefore loses V2-router detection on every multi-venue pair
        // (the most common majors: WETH/USDC, WETH/USDT). First-match keeps detection live; any
        // misattribution writes a transient phantom delta to the sibling pool that (a) is corrected
        // at the next resync and (b) can never cost capital — REVM and the on-chain `minNetProfit`
        // revert reject a phantom arb (worst case = wasted gas). Wiring a router→venue map would
        // make this exact; until then, first-match is the right trade. The no-V2-pool case still
        // returns `None` below (a `fee_pips == 0` query on a V3-only / unknown pair must not guess
        // a non-V2 family's pool — that WOULD be a wrong-store phantom delta).
        for &id in candidates {
            let meta = ctx.pool_table.meta(id);
            if matches!(
                meta.kind,
                engine_state::PoolKind::UniswapV2
                    | engine_state::PoolKind::SushiV2
                    | engine_state::PoolKind::CamelotV2
            ) {
                return Some(id);
            }
        }
        None
    } else {
        // V3 router path. Uniswap / Sushi / Pancake V3 have static fee tiers, so the
        // pool is identified by exact `fee_pips`. Algebra (Camelot V3) is different:
        // it runs ONE pool per pair with a dynamic fee, and its router calldata carries
        // no fee tier — so it must be resolved on the token pair alone (fee-agnostic).
        // Requiring an exact fee match dropped every Algebra swap (the stored fee is the
        // live dynamic value from globalState(), never the router's), leaving Camelot V3
        // pools permanently unpredicted. Prefer an exact tiered match; otherwise fall
        // back to the pair's Algebra pool.
        let mut algebra_fallback: Option<PoolId> = None;
        for &id in candidates {
            let meta = ctx.pool_table.meta(id);
            match meta.kind {
                engine_state::PoolKind::UniswapV3
                | engine_state::PoolKind::SushiV3
                | engine_state::PoolKind::PancakeV3 => {
                    if meta.fee_pips_or_bps == fee_pips {
                        return Some(id);
                    }
                }
                engine_state::PoolKind::AlgebraV19 => {
                    if algebra_fallback.is_none() {
                        algebra_fallback = Some(id);
                    }
                }
                _ => {}
            }
        }
        algebra_fallback
    }
}

fn u256_to_u128(x: U256) -> Option<u128> {
    if x.bit_len() > 128 {
        None
    } else {
        let limbs = x.as_limbs();
        Some((limbs[0] as u128) | ((limbs[1] as u128) << 64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_state::{PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, VenueId};

    fn make_universe_with_v2_pool() -> (
        engine_state::Universe,
        PoolId,
        [u8; 20], // token0
        [u8; 20], // token1
    ) {
        let mut ub = UniverseBuilder::new();
        let weth = ub.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = ub.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let pool_addr = [0xa1u8; 20];
        let pool = ub
            .add_v2_pool(
                pool_addr,
                PoolKind::UniswapV2,
                weth,
                usdc,
                VenueId(0),
                30,
                V2State {
                    reserves: V2Reserves256 {
                        reserve0: 1_000_000_000_000_000_000_000,
                        reserve1: 4_000_000_000_000_000_000_000,
                        block_timestamp_last: 0,
                    },
                },
            )
            .unwrap();
        let universe = ub.build();
        (universe, pool, [0x82; 20], [0xaf; 20])
    }

    #[test]
    fn predict_v2_direct_pool_swap_dirty_event() {
        let (universe, pool_id, _t0, _t1) = make_universe_with_v2_pool();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        // Direct V2 pool swap: pool address = [0xa1; 20], take amount1 out, push token0 in.
        // Direction = ZeroForOne (caller pushes token0).
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xa1u8; 20]),
            token_in: [0u8; 20],
            token_out: [0u8; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1_000_000_000_000_000_000u128),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].pool_id(), pool_id);
        match &preds[0] {
            PoolPrediction::V2 { new_state, .. } => {
                assert!(new_state.reserves.reserve0 > 1_000_000_000_000_000_000_000);
                assert!(new_state.reserves.reserve1 < 4_000_000_000_000_000_000_000);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn predict_route_drops_exact_output_intent_no_stale_commit() {
        // [Codex#1] An exact-OUTPUT swap (UR V3_SWAP_EXACT_OUT / V2_SWAP_EXACT_OUT) carries
        // amount_in = 0 — only the desired OUTPUT is in the calldata, not the input. Forward-applying
        // it threads amount_in = 0 through the hops, and predict_*_hop returns the pool's UNCHANGED
        // (stale, pre-swap) state as a "prediction" — which commit_prediction writes to the mirror and
        // a DirtyEvent then re-evaluates arbs against. That is actively wrong (stale prices) and can
        // suppress the resync. There is no reverse (output->input) kernel for concentrated-liquidity
        // V3, so exact-output must NOT forward-apply: it is left to the resync / shadow-REVM path,
        // which reads the true post-swap state. A V3 pool is used because that is the family whose
        // predictor emits the stale snapshot on amount_in = 0 (V2 already drops). Assert ZERO preds.
        use engine_state::V3State;
        let mut ub = UniverseBuilder::new();
        let a = ub.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let b = ub.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let _pool = ub
            .add_v3_pool(
                [0xceu8; 20],
                PoolKind::UniswapV3,
                a,
                b,
                VenueId(0),
                500,
                60,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000_000_000_000_000,
                    fee_pips: 500,
                    tick_spacing: 60,
                    initialised_ticks: vec![
                        TickInfo {
                            tick: -10_000,
                            liquidity_net: 0,
                        },
                        TickInfo {
                            tick: 10_000,
                            liquidity_net: 0,
                        },
                    ],
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xceu8; 20]),
            token_in: [0x11; 20],
            token_out: [0x22; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        // exact_input = false, amount_in = 0, amount_out_min = the known output (as the decoder emits).
        let intent = RouteIntent {
            hops,
            amount_in: U256::ZERO,
            amount_out_min: U256::from(123u64),
            exact_input: false,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert!(
            preds.is_empty(),
            "exact-output must not forward-apply — doing so commits stale pre-swap state"
        );
    }

    #[test]
    fn predict_route_breaks_on_repeated_pool_no_duplicate_prediction() {
        // C1: a route that touches the SAME pool twice (e.g. a V3 multicall splitting a swap
        // across one pool, or a swap-and-swap-back) must NOT emit two predictions for it. Each
        // predict_*_hop reads the pool's CURRENT store snapshot — so a second prediction would be
        // computed from the stale PRE-first-hop base AND would full-overwrite the first on commit
        // (commit_prediction replaces the slot), silently dropping the first hop's delta. The
        // predictor keeps the correct first-touch prefix and leaves the rest to reconciliation.
        let (universe, pool_id, _t0, _t1) = make_universe_with_v2_pool();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        // Hop 1: pool [0xa1;20], ZeroForOne.
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xa1u8; 20]),
            token_in: [0u8; 20],
            token_out: [0u8; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        // Hop 2: SAME pool again (split / revisit). Without the guard the predictor would emit a
        // second V2 prediction computed from the original (pre-hop-1) reserves.
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xa1u8; 20]),
            token_in: [0u8; 20],
            token_out: [0u8; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1_000_000_000_000_000_000u128),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(
            preds.len(),
            1,
            "repeated pool → only the first-touch prediction is emitted (no stale-base overwrite)"
        );
        assert_eq!(preds[0].pool_id(), pool_id);
        // No duplicate pool id may reach the commit loop / DirtyEvent.
        let mut ids: Vec<u32> = preds.iter().map(|p| p.pool_id().0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            preds.len(),
            "no duplicate pool ids in the prediction set"
        );
    }

    #[test]
    fn predict_v4_swap_forward_applies_as_v3() {
        use engine_state::V3State;
        // V4 pools are registered hookless-only (factory_v4 filters hooks == address(0)), so a V4
        // swap is pure concentrated-liquidity (V3) tick math. predict_route must forward-apply it
        // through the same V3 kernel and emit a V3 prediction (V4 reuses V3State). Before the fix
        // a V4 hop fell through `_ => break` and produced no prediction.
        let mut ub = UniverseBuilder::new();
        let a = ub.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let b = ub.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let pool_addr = [0xd4u8; 20];
        let mut pid = [0u8; 32];
        pid[0..20].copy_from_slice(&pool_addr);
        let v4 = ub
            .add_v4_pool(
                pool_addr,
                a,
                b,
                VenueId(0),
                500,
                60,
                pid,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000_000_000_000_000,
                    fee_pips: 500,
                    tick_spacing: 60,
                    // Wide warm window (no initialised tick in the swap's price path), matching the
                    // V3 kernel's own basic-swap test so the in-range step is well-defined.
                    initialised_ticks: vec![
                        TickInfo {
                            tick: -10_000,
                            liquidity_net: 0,
                        },
                        TickInfo {
                            tick: 10_000,
                            liquidity_net: 0,
                        },
                    ],
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some(pool_addr),
            token_in: [0x11; 20], // token0 in → zero_for_one
            token_out: [0x22; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1_000_000_000_000u128),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(
            preds.len(),
            1,
            "V4 swap must forward-apply (was dropped at `_ => break`)"
        );
        assert_eq!(preds[0].pool_id(), v4);
        match &preds[0] {
            PoolPrediction::V3 { new_state, .. } => {
                assert!(
                    new_state.sqrt_price_x96 < (U256::from(1u128) << 96),
                    "zero_for_one pushes token0 in → sqrt price moves down"
                );
            }
            other => panic!("expected V3 prediction for a V4 pool, got {other:?}"),
        }
    }

    #[test]
    fn predict_kyber_elastic_swap_forward_applies_as_v3() {
        use engine_state::V3State;
        // KyberSwapElastic is V3-state-backed; predict_kyber_hop forward-applies via the Elastic
        // kernel and emits a V3 prediction. The QUOTE is KYBER_ENABLE-gated, but the forward-apply
        // runs regardless (the gate is on detection's use, not the prediction). This closes the only
        // predict_*_hop that previously had no unit test.
        let mut ub = UniverseBuilder::new();
        let a = ub.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let b = ub.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let pool = ub
            .add_v3_pool(
                [0xceu8; 20],
                PoolKind::KyberSwapElastic,
                a,
                b,
                VenueId(0),
                500,
                60,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000_000_000_000_000,
                    fee_pips: 500,
                    tick_spacing: 60,
                    initialised_ticks: vec![
                        TickInfo {
                            tick: -10_000,
                            liquidity_net: 0,
                        },
                        TickInfo {
                            tick: 10_000,
                            liquidity_net: 0,
                        },
                    ],
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xceu8; 20]),
            token_in: [0x11; 20],
            token_out: [0x22; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1_000_000_000_000u64),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1, "Kyber swap must forward-apply");
        match &preds[0] {
            PoolPrediction::V3 { pool_id, new_state } => {
                assert_eq!(*pool_id, pool);
                assert!(
                    new_state.sqrt_price_x96 < (U256::from(1u128) << 96),
                    "zero_for_one lowers the sqrt price"
                );
            }
            other => panic!("expected V3 prediction for a Kyber pool, got {other:?}"),
        }
    }

    #[test]
    fn resolve_algebra_pool_is_fee_agnostic() {
        use engine_state::V3State;
        // Camelot V3 (Algebra): one pool per pair, dynamic fee. The stored fee is the
        // live globalState() value (500 here); a router's calldata never carries it.
        let mut ub = UniverseBuilder::new();
        let a = ub.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let b = ub.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let algebra = ub
            .add_v3_pool(
                [0xc3u8; 20],
                PoolKind::AlgebraV19,
                a,
                b,
                VenueId(0),
                500,
                60,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000,
                    fee_pips: 500,
                    tick_spacing: 60,
                    initialised_ticks: Vec::new(),
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        // A fee that does NOT equal the stored 500 still resolves to the Algebra pool,
        // in either token order.
        assert_eq!(
            resolve_by_tokens(&ctx, &[0x11; 20], &[0x22; 20], 3000),
            Some(algebra)
        );
        assert_eq!(
            resolve_by_tokens(&ctx, &[0x22; 20], &[0x11; 20], 100),
            Some(algebra)
        );
    }

    #[test]
    fn resolve_by_tokens_fee0_without_v2_pool_returns_none() {
        use engine_state::V3State;
        // A pair served ONLY by a V3 pool. A `fee_pips == 0` query — the shape produced
        // by V2-router calldata AND by token-pair-only aggregator decoders (Paraswap / 0x /
        // Odos / 1inch generic `swap`), which carry no fee tier — must NOT guess this V3
        // pool. The swap's exact pool/fee is unknown, so forward-applying V3 math to it
        // could write a wrong delta to a slot the swap may never have touched. Safe
        // behavior: return None → the touched pair falls through to dirty-fanout / targeted
        // resync instead of a phantom same-block prediction.
        let mut ub = UniverseBuilder::new();
        let a = ub.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let b = ub.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let _v3 = ub
            .add_v3_pool(
                [0xc3u8; 20],
                PoolKind::UniswapV3,
                a,
                b,
                VenueId(0),
                500,
                60,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000,
                    fee_pips: 500,
                    tick_spacing: 60,
                    initialised_ticks: Vec::new(),
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        // Today this returns Some(candidates[0]) = the V3 pool (the bug); after the fix it
        // must be None.
        assert_eq!(resolve_by_tokens(&ctx, &[0x11; 20], &[0x22; 20], 0), None);
    }

    #[test]
    fn resolve_by_tokens_fee0_single_v2_pool_is_unambiguous_returns_it() {
        // The unambiguous case the C2 fix must PRESERVE: a pair served by exactly one V2-family
        // pool resolves to it on a fee==0 (V2-router) query — there is no venue ambiguity.
        let (universe, pool_id, t0, t1) = make_universe_with_v2_pool();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        assert_eq!(resolve_by_tokens(&ctx, &t0, &t1, 0), Some(pool_id));
        // Token order must not matter.
        assert_eq!(resolve_by_tokens(&ctx, &t1, &t0, 0), Some(pool_id));
    }

    #[test]
    fn resolve_by_tokens_fee0_multiple_v2_venues_returns_a_v2_pool_capital_safe_trigger() {
        // C2: a pair served by TWO V2-family venues (Uniswap-V2 + Sushi-V2) is ambiguous on a
        // fee==0 query — the router identity that would disambiguate it is not carried on the hop,
        // and the engine has no router→venue map to recover it. We deliberately do NOT fail closed
        // here: returning None would drop the same-block prediction, and the resync fallback can't
        // recover a V2-router swap (token-path calldata, no pool address to sniff), so detection on
        // every multi-venue major pair (WETH/USDC, WETH/USDT) would be lost. First-match keeps
        // detection live; any misattribution is a transient phantom delta on the sibling pool that
        // is resync-corrected and can never cost capital (REVM + the on-chain minNetProfit revert
        // reject a phantom arb). The resolver must therefore return one of the pair's V2 pools.
        let mut ub = UniverseBuilder::new();
        let weth = ub.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = ub.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let state = V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 4_000_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        };
        // Two distinct V2-family pools on the SAME WETH/USDC pair, different venues/addresses.
        let uni = ub
            .add_v2_pool(
                [0xa1; 20],
                PoolKind::UniswapV2,
                weth,
                usdc,
                VenueId(0),
                30,
                state.clone(),
            )
            .unwrap();
        let sushi = ub
            .add_v2_pool(
                [0xa2; 20],
                PoolKind::SushiV2,
                weth,
                usdc,
                VenueId(1),
                30,
                state,
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        // Returns a V2 pool on the pair (the first match) — a capital-safe detection trigger, NOT
        // None and NOT a non-V2 pool.
        let got = resolve_by_tokens(&ctx, &[0x82; 20], &[0xaf; 20], 0);
        assert!(
            got == Some(uni) || got == Some(sushi),
            "multiple V2 venues → returns one of the pair's V2 pools (capital-safe trigger), got {got:?}"
        );
    }

    #[test]
    fn predict_unknown_pool_returns_empty_prefix() {
        // A swap whose first hop is on a pool we don't track is not an error — there
        // is simply nothing in our universe to update, so the prefix is empty.
        let (universe, _pool_id, _t0, _t1) = make_universe_with_v2_pool();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xffu8; 20]),
            token_in: [0u8; 20],
            token_out: [0u8; 20],
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1u64),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert!(preds.is_empty());
    }

    #[test]
    fn predict_mixed_route_keeps_decodable_prefix() {
        // Hop 1 is our known V2 pool (predictable); hop 2 is a pool not in the
        // registry. The predictor must still emit hop 1's update, not drop both.
        let (universe, pool_id, t0, t1) = make_universe_with_v2_pool();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops: ArrayVec<crate::router_decoder::DecodedHop, MAX_DIRTY_POOLS> =
            ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: None,
            token_in: t0,
            token_out: t1,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xffu8; 20]), // unknown second pool
            token_in: t1,
            token_out: [0xcd; 20],
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1_000_000_000_000_000_000u128),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].pool_id(), pool_id);
    }

    // C2: a Curve StableSwap-NG exchange produces a Curve prediction whose balances move by
    // (+dx on coin i, -dy on coin j) — the sub-block freshness the predictor adds for Curve.
    #[test]
    fn predict_curve_exchange_updates_balances() {
        use engine_amm::curve::{StableSwapState, A_PRECISION};
        use engine_state::CurveSlot;
        let mut ub = UniverseBuilder::new();
        let dai = [0xd0u8; 20];
        let usdc = [0xc0u8; 20];
        let dai_id = ub.add_token(dai, 18, RiskClass::CentralisedStable);
        let usdc_id = ub.add_token(usdc, 18, RiskClass::CentralisedStable);
        let mut tokens = [[0u8; 20]; engine_amm::curve::MAX_COINS];
        tokens[0] = dai;
        tokens[1] = usdc;
        let kernel = StableSwapState {
            n_coins: 2,
            balances: [
                1_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        let pool = ub
            .add_curve_pool(
                [0xceu8; 20],
                PoolKind::CurveStableSwapNg,
                dai_id,
                usdc_id,
                VenueId(0),
                4,
                CurveSlot::new(kernel, tokens),
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let dx = 50_000_000_000_000_000_000u128; // 50 DAI in
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xceu8; 20]),
            token_in: dai,
            token_out: usdc,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(dx),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        match &preds[0] {
            PoolPrediction::Curve {
                pool_id,
                new_kernel,
            } => {
                assert_eq!(*pool_id, pool);
                // DAI (coin 0) up by dx; USDC (coin 1) down by the realized dy (< dx).
                assert_eq!(new_kernel.balances[0], 1_000_000_000_000_000_000_000 + dx);
                assert!(new_kernel.balances[1] < 1_000_000_000_000_000_000_000);
                assert!(new_kernel.balances[1] > 1_000_000_000_000_000_000_000 - dx);
                // Invariant params unchanged.
                assert_eq!(new_kernel.a_param, kernel.a_param);
                assert_eq!(new_kernel.fee, kernel.fee);
            }
            other => panic!("expected Curve prediction, got {other:?}"),
        }
    }

    #[test]
    fn predict_curve_crypto_exchange_updates_balances() {
        use engine_amm::curve_cryptoswap::CryptoSwapState;
        use engine_state::CryptoCurveSlot;
        // 3-coin Tricrypto-NG reference pool (same vectors as the kernel parity gate). A router-
        // routed coin0 → coin1 swap must produce a CurveCrypto prediction whose active kernel's
        // balances move (coin0 += dx, coin1 -= dy) and whose D is re-solved — the same-block twin
        // of the existing Tricrypto quote. dx = 10e9 pins dy = 16_027_909.
        let mut ub = UniverseBuilder::new();
        let c0 = [0xa0u8; 20];
        let c1 = [0xb0u8; 20];
        let c2 = [0xc0u8; 20];
        let c0_id = ub.add_token(c0, 6, RiskClass::CentralisedStable);
        let c1_id = ub.add_token(c1, 8, RiskClass::Permissionless);
        let _c2_id = ub.add_token(c2, 18, RiskClass::Permissionless);
        let mut tokens = [[0u8; 20]; 3];
        tokens[0] = c0;
        tokens[1] = c1;
        tokens[2] = c2;
        let kernel = CryptoSwapState {
            balances: [
                U256::from(5_000_000_000_000u64),
                U256::from(8_000_000_000u64),
                U256::from(1_600_000_000_000_000_000_000u128),
            ],
            precisions: [
                U256::from(1_000_000_000_000u64),
                U256::from(10_000_000_000u64),
                U256::from(1u64),
            ],
            price_scale: [
                U256::from(60_000_000_000_000_000_000_000u128),
                U256::from(3_000_000_000_000_000_000_000u128),
            ],
            ann: U256::from(1_707_629u64),
            gamma: U256::from(11_809_167_828_997u64),
            mid_fee: U256::from(3_000_000u64),
            out_fee: U256::from(30_000_000u64),
            fee_gamma: U256::from(5_000_000_000_000_000u64),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        };
        let pool = ub
            .add_curve_crypto_pool(
                [0xceu8; 20],
                PoolKind::CurveCryptoSwap,
                c0_id,
                c1_id,
                VenueId(0),
                4,
                CryptoCurveSlot {
                    kernel,
                    twocrypto_kernel: None,
                    tokens,
                    n_coins: 3,
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let dx = 10_000_000_000u128;
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xceu8; 20]),
            token_in: c0,
            token_out: c1,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(dx),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(
            preds.len(),
            1,
            "CurveCrypto hop must forward-apply (was `_ => break`)"
        );
        match &preds[0] {
            PoolPrediction::CurveCrypto { pool_id, new_slot } => {
                assert_eq!(*pool_id, pool);
                assert_eq!(new_slot.n_coins, 3);
                assert_eq!(
                    new_slot.kernel.balances[0],
                    U256::from(5_000_000_000_000u64) + U256::from(dx),
                    "coin0 (in) up by dx"
                );
                assert_eq!(
                    new_slot.kernel.balances[1],
                    U256::from(8_000_000_000u64) - U256::from(16_027_909u64),
                    "coin1 (out) down by the quoted dy"
                );
                assert!(
                    !new_slot.kernel.d.is_zero(),
                    "D re-solved on the predicted slot"
                );
                assert_eq!(new_slot.tokens[2], c2, "token map preserved");
            }
            other => panic!("expected CurveCrypto prediction, got {other:?}"),
        }
    }

    #[test]
    fn predict_solidly_stable_swap_updates_reserves() {
        // SolidlyV3 (Solidly/Velodrome stable) reuses V2State reserves but prices on x³y+xy³=k. A
        // swap must forward-apply via the stable kernel and emit a V2 prediction (reserves move),
        // not drop at `_ => break`. Cross-decimal pair (USDC 6dp / DAI 18dp) exercises the decimal
        // normalization the predictor reads from the token table.
        let mut ub = UniverseBuilder::new();
        let usdc = [0xc6u8; 20];
        let dai = [0xd8u8; 20];
        let usdc_id = ub.add_token(usdc, 6, RiskClass::CentralisedStable);
        let dai_id = ub.add_token(dai, 18, RiskClass::CentralisedStable);
        const R0: u128 = 1_000_000_000_000; // 1,000,000 USDC (6dp)
        const R1: u128 = 1_000_000_000_000_000_000_000_000; // 1,000,000 DAI (18dp)
        let pool = ub
            .add_v2_pool(
                [0x5du8; 20],
                PoolKind::SolidlyV3,
                usdc_id,
                dai_id,
                VenueId(0),
                5, // 5 bps stable fee
                V2State {
                    reserves: V2Reserves256 {
                        reserve0: R0,
                        reserve1: R1,
                        block_timestamp_last: 0,
                    },
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let dx = 10_000_000_000u128; // 10,000 USDC in (6dp)
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0x5du8; 20]),
            token_in: usdc, // token0 → zero_for_one
            token_out: dai,
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(dx),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(
            preds.len(),
            1,
            "SolidlyV3 swap must forward-apply (was `_ => break`)"
        );
        match &preds[0] {
            PoolPrediction::V2 { pool_id, new_state } => {
                assert_eq!(*pool_id, pool);
                assert_eq!(
                    new_state.reserves.reserve0,
                    R0 + dx,
                    "USDC (in) reserve rises by the full input"
                );
                assert!(new_state.reserves.reserve1 < R1, "DAI (out) reserve falls");
                // ~1:1 stable pair: 10,000 USDC in yields ≈10,000 DAI out (1e22), so the drop is in a
                // sane band — proves the stable kernel priced it (not a dropped / absurd output).
                assert!(
                    new_state.reserves.reserve1 > R1 - 12_000_000_000_000_000_000_000,
                    "stable output is ~1:1, not blown out"
                );
            }
            other => panic!("expected V2 prediction for a SolidlyV3 pool, got {other:?}"),
        }
    }

    #[test]
    fn predict_fluid_v1_swap_updates_imaginary_reserves() {
        use engine_amm::fluid::{
            TokenScale, V1ExpandableLimit, V1LimitsNative, V1PoolState, V1SideReservesAdjusted,
        };
        use engine_state::FluidPoolSnapshot;
        // Fluid v1 (smart-collateral + smart-debt) symmetric pool. A router-routed token0→token1
        // swap must forward-apply via the v1 kernel and emit a Fluid prediction whose price-
        // determining imaginary reserves moved — not drop at `_ => break`.
        let mut ub = UniverseBuilder::new();
        let t0 = [0x70u8; 20];
        let t1 = [0x71u8; 20];
        let t0_id = ub.add_token(t0, 18, RiskClass::Permissionless);
        let t1_id = ub.add_token(t1, 18, RiskClass::Permissionless);
        let unit = TokenScale {
            numerator: U256::from(1u8),
            denominator: U256::from(1u8),
        };
        let side = V1SideReservesAdjusted {
            token0_imaginary: U256::from(1_000_000_000u64),
            token1_imaginary: U256::from(1_000_000_000u64),
            ..Default::default()
        };
        let big = V1ExpandableLimit {
            available_native: U256::from(1_000_000_000u64),
            expands_to_native: U256::from(1_000_000_000u64),
            expand_duration_left_secs: 0,
        };
        let v1 = V1PoolState {
            fee_1e6: 0,
            token0_scale: unit,
            token1_scale: unit,
            collateral: side,
            debt: side,
            limits: V1LimitsNative {
                withdrawable_token0: big,
                withdrawable_token1: big,
                borrowable_token0: big,
                borrowable_token1: big,
                snapshot_timestamp: 0,
            },
            smart_collateral_enabled: true,
            smart_debt_enabled: true,
            swaps_paused: false,
        };
        let pool = ub
            .add_fluid_pool(
                [0xf1u8; 20],
                t0_id,
                t1_id,
                VenueId(0),
                0,
                FluidPoolSnapshot::V1(v1),
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xf1u8; 20]),
            token_in: t0, // token0 → ZeroForOne → SwapDirection::ZeroToOne
            token_out: t1,
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(100_000u64),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(
            preds.len(),
            1,
            "Fluid v1 swap must forward-apply (was `_ => break`)"
        );
        match &preds[0] {
            PoolPrediction::Fluid {
                pool_id,
                new_snapshot,
            } => {
                assert_eq!(*pool_id, pool);
                match &**new_snapshot {
                    FluidPoolSnapshot::V1(nv1) => {
                        // Even split, fee=0: +50k to each side's token0 imaginary reserve.
                        assert_eq!(
                            nv1.collateral.token0_imaginary,
                            U256::from(1_000_050_000u64)
                        );
                        assert_eq!(nv1.debt.token0_imaginary, U256::from(1_000_050_000u64));
                        assert!(nv1.collateral.token1_imaginary < U256::from(1_000_000_000u64));
                    }
                    _ => panic!("expected a V1 snapshot"),
                }
            }
            other => panic!("expected Fluid prediction, got {other:?}"),
        }
    }

    #[test]
    fn predict_maverick_v2_swap_updates_active_tick_and_price() {
        use engine_amm::maverick_v2::{MaverickState, MaverickTick, TokenScale, D18};
        // Maverick V2 forward-apply is wired (dormant behind the operator-gated quote). A swap must
        // update active_tick + sqrt_price (per-tick liquidity unchanged — the quote reads liquidity +
        // sqrt bounds, not per-tick reserves) and emit a Maverick prediction, not drop. Pool mirrors
        // the maverick_quote_vectors balanced fixture (two ticks around active_tick 0).
        let d18 = U256::from(D18);
        let mut ub = UniverseBuilder::new();
        let t0 = [0x22u8; 20];
        let t1 = [0x33u8; 20];
        let t0_id = ub.add_token(t0, 18, RiskClass::Permissionless);
        let t1_id = ub.add_token(t1, 18, RiskClass::Permissionless);
        let s18 = TokenScale::from_pool_scale(U256::from(1u64)).expect("identity scale (18dp)");
        let tick = |t: i32, lo: U256, hi: U256| MaverickTick {
            tick: t,
            reserve0_d18: U256::from(1_000_000u64) * d18,
            reserve1_d18: U256::from(1_000_000u64) * d18,
            total_supply: U256::from(1_000_000u64),
            liquidity_d18: U256::from(1_000_000u64) * d18,
            sqrt_lower_price_d18: lo,
            sqrt_upper_price_d18: hi,
        };
        let state = MaverickState {
            // Kernel-internal Address fields (alloy type); only used by the kernel's own
            // direction_for_tokens, which predict_maverick_hop bypasses (it passes a resolved dir).
            address: [0xafu8; 20].into(),
            token0: t0.into(),
            token1: t1.into(),
            token0_scale: s18,
            token1_scale: s18,
            fee_zto_d18: U256::from(3_000_000_000_000_000u64),
            fee_otz_d18: U256::from(3_000_000_000_000_000u64),
            tick_spacing: 1,
            lookback: 60,
            kinds: 15,
            lending_fee_rate_d18: U256::ZERO,
            protocol_fee_ratio_d3: 100,
            permissioned_swap: false,
            sqrt_price_d18: d18,
            active_tick: 0,
            reserve0_d18: U256::from(2_000_000u64) * d18,
            reserve1_d18: U256::from(2_000_000u64) * d18,
            last_timestamp: 0,
            block_number: 1,
            log_index: 0,
            tick_radius: 1,
            ticks: vec![
                tick(-1, d18 / U256::from(2u64), d18),
                tick(0, d18, U256::from(2u64) * d18),
            ],
        };
        let pool = ub
            .add_maverick_pool([0xafu8; 20], t0_id, t1_id, VenueId(0), 30, state)
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xafu8; 20]),
            token_in: t0, // token0 → ZeroForOne → SwapDirection::ZeroToOne
            token_out: t1,
            fee_pips: 0,
            direction: Direction::ZeroForOne,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(1_000_000_000_000_000u64), // 0.001 token (18dp) — stays in-range
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(
            preds.len(),
            1,
            "Maverick swap must forward-apply (was `_ => break`)"
        );
        match &preds[0] {
            PoolPrediction::Maverick { pool_id, new_state } => {
                assert_eq!(*pool_id, pool);
                // Maverick walks to higher ticks for ZeroToOne (kernel quote_d18), so token0-in
                // RAISES the sqrt price (opposite to V3's zero_for_one). The essential property is
                // that the forward-apply moved the price off its starting point.
                assert!(
                    new_state.sqrt_price_d18 > d18,
                    "ZeroToOne moved the sqrt price up from its starting bound"
                );
            }
            other => panic!("expected Maverick prediction, got {other:?}"),
        }
    }

    // C2: a router-routed DODO swap produces a Dodo prediction whose reserves + PMM fields move.
    #[test]
    fn predict_dodo_swap_updates_reserves_and_targets() {
        use engine_amm::dodo_v2::{DodoPoolClass, DodoRState, DodoState};
        const D18: u128 = 1_000_000_000_000_000_000;
        let base = [0x22u8; 20];
        let quote = [0x33u8; 20];
        let mut ub = UniverseBuilder::new();
        let base_id = ub.add_token(base, 18, RiskClass::Permissionless);
        let quote_id = ub.add_token(quote, 18, RiskClass::CentralisedStable);
        let dodo = DodoState {
            address: base.into(), // unused for the predictor
            pool_class: DodoPoolClass::Dpp,
            base_token: base.into(),
            quote_token: quote.into(),
            i: U256::from(2u8) * U256::from(D18),
            k: U256::from(D18 / 2),
            base_reserve: U256::from(100u64) * U256::from(D18),
            quote_reserve: U256::from(200u64) * U256::from(D18),
            base_target: U256::from(100u64) * U256::from(D18),
            quote_target: U256::from(200u64) * U256::from(D18),
            r_state: DodoRState::One,
            lp_fee_rate_d18: U256::from(3_000_000_000_000_000u64),
            mt_fee_rate_d18: U256::from(1_000_000_000_000_000u64),
            fee_ppm: 4_000,
            block_number: 1,
            log_index: 0,
        };
        let pool = ub
            .add_dodo_pool([0xdeu8; 20], base_id, quote_id, VenueId(0), 4_000, dodo)
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let dx = U256::from(10u64) * U256::from(D18);
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xdeu8; 20]),
            token_in: base,
            token_out: quote,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: dx,
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        match &preds[0] {
            PoolPrediction::Dodo { pool_id, new_state } => {
                assert_eq!(*pool_id, pool);
                assert_eq!(
                    new_state.base_reserve,
                    U256::from(100u64) * U256::from(D18) + dx
                );
                assert!(new_state.quote_reserve < U256::from(200u64) * U256::from(D18));
                assert_eq!(
                    new_state.i,
                    U256::from(2u8) * U256::from(D18),
                    "oracle i unchanged"
                );
            }
            other => panic!("expected Dodo prediction, got {other:?}"),
        }
    }

    // C2: a Balancer V2 Stable swap produces a BalancerStable prediction whose balances move.
    #[test]
    fn predict_balancer_stable_swap_updates_balances() {
        use engine_amm::balancer_stable::{BalancerStableState, AMP_PRECISION, MAX_TOKENS};
        use engine_state::BalancerStableSlot;
        const ONE: u128 = 1_000_000_000_000_000_000;
        let dai = [0xd1u8; 20];
        let usdc = [0xc1u8; 20];
        let mut ub = UniverseBuilder::new();
        let dai_id = ub.add_token(dai, 18, RiskClass::CentralisedStable);
        let usdc_id = ub.add_token(usdc, 18, RiskClass::CentralisedStable);
        let mut balances = [0u128; MAX_TOKENS];
        balances[0] = 1_000_000 * ONE;
        balances[1] = 1_000_000 * ONE;
        let mut sf = [U256::ZERO; MAX_TOKENS];
        sf[0] = U256::from(ONE);
        sf[1] = U256::from(ONE);
        let kernel = BalancerStableState {
            n: 2,
            balances,
            scaling_factors: sf,
            amp: 1000 * AMP_PRECISION,
            swap_fee_1e18: 1_000_000_000_000_000,
        };
        let mut tokens = [[0u8; 20]; MAX_TOKENS];
        tokens[0] = dai;
        tokens[1] = usdc;
        let pool = ub
            .add_balancer_stable_pool(
                [0xb5u8; 20],
                dai_id,
                usdc_id,
                VenueId(0),
                1_000,
                BalancerStableSlot {
                    kernel,
                    tokens,
                    pool_id: [0x07u8; 32],
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        let dx = 1000u128 * ONE;
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xb5u8; 20]),
            token_in: dai,
            token_out: usdc,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(dx),
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        match &preds[0] {
            PoolPrediction::BalancerStable {
                pool_id,
                new_kernel,
            } => {
                assert_eq!(*pool_id, pool);
                assert_eq!(new_kernel.balances[0], 1_000_000 * ONE + dx);
                assert!(new_kernel.balances[1] < 1_000_000 * ONE);
                assert_eq!(new_kernel.amp, kernel.amp, "amp unchanged");
            }
            other => panic!("expected BalancerStable prediction, got {other:?}"),
        }
    }

    // C2: a Trader Joe LB swap produces an Lb prediction whose active-bin reserves move.
    #[test]
    fn predict_lb_swap_updates_active_bin() {
        use engine_amm::trader_joe_lb::{BinData, LBState};
        use engine_state::LbSlot;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        const ONE: u128 = 1_000_000_000_000_000_000;
        let x = [0x01u8; 20]; // token0 (lower address) == kernel tokenX
        let y = [0x02u8; 20];
        let mut ub = UniverseBuilder::new();
        let x_id = ub.add_token(x, 18, RiskClass::Permissionless);
        let y_id = ub.add_token(y, 18, RiskClass::Permissionless);
        let active = 8_388_608u32;
        let mut bins = BTreeMap::new();
        bins.insert(
            active,
            BinData {
                reserve_x: ONE,
                reserve_y: ONE,
            },
        );
        let kernel = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: ONE,
            reserve_y: ONE,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bins),
        };
        let pool = ub
            .add_lb_pool(
                [0x71u8; 20],
                x_id,
                y_id,
                VenueId(0),
                20,
                LbSlot {
                    kernel,
                    token0: x,
                    token1: y,
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        // X→Y (zero_for_one): token_in = x (token0). Direction resolves to ZeroForOne.
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0x71u8; 20]),
            token_in: x,
            token_out: y,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: U256::from(10_000_000_000_000_000u128), // 0.01 X
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        match &preds[0] {
            PoolPrediction::Lb {
                pool_id,
                new_kernel,
            } => {
                assert_eq!(*pool_id, pool);
                let bin = new_kernel.bin_map.get(&new_kernel.active_id).unwrap();
                assert!(bin.reserve_x > ONE, "active bin X up after X→Y");
                assert!(bin.reserve_y < ONE, "active bin Y down after X→Y");
            }
            other => panic!("expected Lb prediction, got {other:?}"),
        }
    }

    // C2: a Balancer V2 Weighted swap produces a BalancerWeighted prediction whose balances_d18 move.
    #[test]
    fn predict_balancer_weighted_swap_updates_balances() {
        use engine_state::balancer_weighted_state::MAX_BALANCER_TOKENS;
        use engine_state::BalancerWeightedSlot;
        const ONE: u128 = 1_000_000_000_000_000_000;
        let weth = [0xe1u8; 20];
        let usdc = [0xf1u8; 20];
        let mut ub = UniverseBuilder::new();
        let weth_id = ub.add_token(weth, 18, RiskClass::Permissionless);
        let usdc_id = ub.add_token(usdc, 18, RiskClass::Permissionless);
        let mut balances = [U256::ZERO; MAX_BALANCER_TOKENS];
        balances[0] = U256::from(1_000_000u128 * ONE);
        balances[1] = U256::from(1_000_000u128 * ONE);
        let mut scaling = [U256::ZERO; MAX_BALANCER_TOKENS];
        scaling[0] = U256::from(ONE); // 18-dec → 10^0 = 1e18? no: 10^(18-18)=1; but stored as 1e18 scaling here
        scaling[1] = U256::from(ONE);
        let mut weights = [U256::ZERO; MAX_BALANCER_TOKENS];
        weights[0] = U256::from(500_000_000_000_000_000u128); // 0.5
        weights[1] = U256::from(500_000_000_000_000_000u128);
        let mut tokens = [[0u8; 20]; MAX_BALANCER_TOKENS];
        tokens[0] = weth;
        tokens[1] = usdc;
        let pool = ub
            .add_balancer_weighted_pool(
                [0xb7u8; 20],
                weth_id,
                usdc_id,
                VenueId(0),
                1_000,
                BalancerWeightedSlot {
                    balances_d18: balances,
                    scaling_d18: scaling,
                    weights_d18: weights,
                    tokens,
                    n_tokens: 2,
                    swap_fee_d18: U256::from(1_000_000_000_000_000u128), // 0.1%
                    pool_id: [0u8; 32],
                },
            )
            .unwrap();
        let universe = ub.build();
        let ctx = PredictorContext {
            registry: &universe.registry,
            pool_table: &universe.pools,
            tokens: &universe.tokens,
            v2_store: &universe.v2_state,
            v3_store: &universe.v3_state,
            curve_store: &universe.curve_state,
            curve_crypto_store: &universe.curve_crypto_state,
            dodo_store: &universe.dodo_state,
            balancer_store: &universe.balancer_state,
            lb_store: &universe.lb_state,
            balancer_weighted_store: &universe.balancer_weighted_state,
            fluid_store: &universe.fluid_state,
            maverick_store: &universe.maverick_state,
        };
        // NOTE: scaling here is 1e18 (not 1) to keep amount_in_d18 in the d18 domain for 18-dec
        // tokens where the test's "raw" amount is already 1e18-scaled — the predictor multiplies by
        // scaling_d18, so use 1 (10^0) for a true 18-dec token. Use a small raw amount accordingly.
        let dx = U256::from(1000u128); // raw units; scaling 1e18 → d18 = 1000e18
        let mut hops = ArrayVec::new();
        hops.push(crate::router_decoder::DecodedHop {
            pool: Some([0xb7u8; 20]),
            token_in: weth,
            token_out: usdc,
            fee_pips: 0,
            direction: Direction::Unknown,
        });
        let intent = RouteIntent {
            hops,
            amount_in: dx,
            amount_out_min: U256::ZERO,
            exact_input: true,
        };
        let preds = predict_route(&ctx, &intent).unwrap();
        assert_eq!(preds.len(), 1);
        match &preds[0] {
            PoolPrediction::BalancerWeighted { pool_id, new_slot } => {
                assert_eq!(*pool_id, pool);
                assert!(
                    new_slot.balances_d18[0] > U256::from(1_000_000u128 * ONE),
                    "in balance up"
                );
                assert!(
                    new_slot.balances_d18[1] < U256::from(1_000_000u128 * ONE),
                    "out balance down"
                );
                assert_eq!(new_slot.weights_d18[0], weights[0], "weights unchanged");
            }
            other => panic!("expected BalancerWeighted prediction, got {other:?}"),
        }
    }
}
