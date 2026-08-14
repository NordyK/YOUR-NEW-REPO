//! DELIVERABLE D/E/G: build Mantis executor calldata from a detected route.
//!
//! Maps a `Route` (engine-routes) + the live `Universe` (pool/token/balancer state) into the
//! Mantis `TradeHopV2[3]` payload and selects the funding entrypoint from `touches_balancer`
//! (mirrors `route_flash_fee_wei`: Aave 0.05% iff a Balancer leg, else Balancer/DEX-flash 0%).
//! This replaces the legacy `execute(pathId,...)` registry calldata at the submit site so the
//! engine speaks the Mantis ABI. The byte-for-byte correctness of the output is proven on a
//! live Arbitrum fork by `contracts/test/MantisEngineCalldata.Fork.t.sol`.

use alloy_primitives::Address as AlloyAddress;
use engine_routes::calldata::{
    encode_mantis_mixed, encode_mantis_mixed4, MantisFunding, MantisHop,
};
use engine_state::ids::RouteId;

use crate::pipeline::DetectionContext;

pub(crate) const UNISWAP_V4_POOL_MANAGER: [u8; 20] = [
    0x36, 0x0E, 0x68, 0xFA, 0xCC, 0xCA, 0x8C, 0xA4, 0x95, 0xC1, 0xB7, 0x59, 0xFD, 0x9E, 0xEE, 0x46,
    0x6D, 0xB9, 0xFB, 0x32,
];

/// Build `flashLoanArbMixed{,4}` / `flashLoanArbAaveMixed{,4}` calldata for `route_id`.
///
/// Handles **2-, 3-, and 4-hop** cycles: 2/3-hop emit the `TradeHopV2[3]` entrypoints; 4-hop emit
/// the `TradeHopV2[4]` entrypoints (`flashLoanArbMixed4`/`flashLoanArbAaveMixed4`). Returns `None`
/// when the route isn't a 2–4-hop cycle or a leg's AMM family has no Mantis exchange id — the
/// caller then skips Mantis submission rather than emitting malformed calldata.
pub fn build_mantis_calldata(
    detection: &DetectionContext,
    route_id: RouteId,
    amount_in: u128,
    min_net_profit: u128,
) -> Option<Vec<u8>> {
    // NOTE (Codex #1): this is a PURE ENCODER — it does NOT enforce a net-profit floor. The REVM
    // validator (`pipeline.rs` ~12347) calls it with `min_net_profit == 0` ON PURPOSE so REVM can
    // bytecode-execute the route and measure the realized profit itself ("accept iff it does not
    // revert/lose"). The LIVE-SAFE positive floor is enforced upstream in `prepare_submission`
    // (which feeds `gas_aware_min_profit(...)`, > 0 in every normal case and explicitly refused if
    // it computes to 0). Guarding zero HERE would make the validator reject every candidate as
    // `calldata_unexecutable` before REVM ever runs — see `build_with_zero_min_profit_still_builds_for_revm_validation`.
    let route = detection.route_table.get(route_id);
    let hop_count = route.hop_count as usize;
    // hop-count guard (fail-closed, named bounds): the Mantis mixed entrypoints carry only
    // TradeHopV2[3] (2/3-hop) or TradeHopV2[4] (4-hop). 5–8-hop routes are enumerated/scored for
    // provenance but have NO calldata path, so anything outside MANTIS_MIN_HOPS..=MANTIS_MAX_HOPS
    // returns None here (and `prepare_submission` then yields empty calldata, so submit skips it).
    use engine_routes::calldata::{MANTIS_MAX_HOPS, MANTIS_MIN_HOPS};
    if !(MANTIS_MIN_HOPS..=MANTIS_MAX_HOPS).contains(&hop_count)
        || route.tokens.len() < hop_count + 1
    {
        return None;
    }

    let pools = &detection.universe.pools;
    let tokens = &detection.universe.tokens;
    // Build up to 4 hops (default = inactive); the first `hop_count` are filled.
    let mut hops = [MantisHop::default(); 4];
    let mut touches_balancer = false;

    for i in 0..hop_count {
        let meta = pools.meta(route.hops[i].pool);
        // WI-02 boundary 6 (submission/encoding): an inactive family never encodes into
        // executor calldata — same fail-closed None as a missing exchange id.
        if !detection.family_active(meta.kind) {
            return None;
        }
        let exchange_id = meta.kind.to_mantis_exchange_id()?;
        if meta.kind.is_balancer() {
            touches_balancer = true;
        }
        // The route's token cycle defines per-hop direction: tokens[i] -> tokens[i+1].
        let token_in_addr = tokens.get(route.tokens[i]).address;
        let token_out_addr = tokens.get(route.tokens[i + 1]).address;
        let mut hop = MantisHop {
            exchange_id,
            token_in: AlloyAddress::from(token_in_addr),
            token_out: AlloyAddress::from(token_out_addr),
            // V3-family router resolves the pool by (tokenIn, tokenOut, fee); fee is in pips.
            // Curve ignores this field in the executor, and its native 1e10-scaled fee can exceed
            // the legacy uint16 hop conversion used before the Curve swap branch.
            fee: if matches!(
                meta.kind,
                engine_state::PoolKind::CurveStableSwapNg | engine_state::PoolKind::CurveCryptoSwap
            ) {
                0
            } else {
                meta.fee_pips_or_bps
            },
            // Explicit pool address (used by DODO/Fluid/Maverick/Curve/Balancer; ignored by V3).
            pool: AlloyAddress::from(meta.address),
            tick_spacing: meta.tick_spacing,
            ..Default::default()
        };
        // uint24 truncation guard (fail-closed): the Solidity TradeHopV2.fee is a uint24, so a
        // fee above 16_777_215 would silently lose its high bytes in ABI encoding and target the
        // wrong fee tier / pool. Skip Mantis submission rather than emit a mangled fee.
        if !engine_routes::calldata::hop_fee_fits_uint24(hop.fee) {
            return None;
        }
        match meta.kind {
            engine_state::PoolKind::CurveStableSwapNg => {
                let slot = detection.universe.curve_state.snapshot(meta.state_slot);
                hop.coin_in_idx = u8::try_from(slot.index_of(&token_in_addr)?).ok()?;
                hop.coin_out_idx = u8::try_from(slot.index_of(&token_out_addr)?).ok()?;
            }
            engine_state::PoolKind::CurveCryptoSwap => {
                let slot = detection
                    .universe
                    .curve_crypto_state
                    .snapshot(meta.state_slot);
                hop.coin_in_idx = u8::try_from(slot.index_of(&token_in_addr)?).ok()?;
                hop.coin_out_idx = u8::try_from(slot.index_of(&token_out_addr)?).ok()?;
            }
            engine_state::PoolKind::UniswapV4 => {
                hop.pool_manager = AlloyAddress::from(UNISWAP_V4_POOL_MANAGER);
                hop.pool_id = detection.universe.v4_pool_id(route.hops[i].pool)?;
            }
            _ => {}
        }
        if meta.kind.is_balancer() {
            // Balancer Vault swap needs the poolId — fetched once at discovery (cold path).
            // Stable + Weighted store it in their respective slots; read from the right store.
            hop.pool_id = if meta.kind.is_balancer_stable() {
                detection
                    .universe
                    .balancer_state
                    .with_snapshot(meta.state_slot, |s| s.pool_id)
            } else {
                detection
                    .universe
                    .balancer_weighted_state
                    .with_snapshot(meta.state_slot, |s| s.pool_id)
            };
        }
        hops[i] = hop;
    }

    // Aave iff the route trades a Balancer pool (Balancer Vault is non-reentrant); else 0-fee.
    let funding = MantisFunding::from_touches_balancer(touches_balancer);
    if hop_count <= 3 {
        // 2/3-hop ride the [3] entrypoints (hops[2] inactive for a 2-hop).
        let hops3: [MantisHop; 3] = [hops[0], hops[1], hops[2]];
        Some(encode_mantis_mixed(
            funding,
            amount_in,
            &hops3,
            min_net_profit,
        ))
    } else {
        // 4-hop uses the [4] entrypoints.
        Some(encode_mantis_mixed4(
            funding,
            amount_in,
            &hops,
            min_net_profit,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use alloy_primitives::U256;
    use arrayvec::ArrayVec;
    use engine_detect::SelectorRegistry;
    use engine_routes::{CsrBuilder, Hop, Route, RouteTable, StrategyKind};
    use engine_state::{PoolKind, RiskClass, UniverseBuilder, V3State, VenueId};

    fn word(cd: &[u8], i: usize) -> U256 {
        let start = 4 + i * 32;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&cd[start..start + 32]);
        U256::from_be_bytes(bytes)
    }

    fn word_bytes(cd: &[u8], i: usize) -> [u8; 32] {
        let start = 4 + i * 32;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&cd[start..start + 32]);
        bytes
    }

    fn word_addr(cd: &[u8], i: usize) -> [u8; 20] {
        let word = word_bytes(cd, i);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&word[12..]);
        addr
    }

    #[test]
    fn mantis_calldata_sets_curve_coin_indices_and_zeroes_unused_curve_fee() {
        let usdc = [
            0xAF, 0x88, 0xD0, 0x65, 0xE7, 0x7C, 0x8C, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xB3,
            0xA4, 0x32, 0x26, 0x8E, 0x58, 0x31,
        ];
        let usdt0 = [
            0xFD, 0x08, 0x6B, 0xC7, 0xCD, 0x5C, 0x48, 0x1D, 0xCC, 0x9C, 0x85, 0xEB, 0xE4, 0x78,
            0xA1, 0xC0, 0xB6, 0x9F, 0xCB, 0xB9,
        ];

        let mut builder = UniverseBuilder::new();
        let usdc_id = builder.add_token(usdc, 6, RiskClass::Permissionless);
        let usdt0_id = builder.add_token(usdt0, 6, RiskClass::Permissionless);
        let v3_pool = builder
            .add_v3_pool(
                [
                    0xBE, 0x3A, 0xD6, 0xA5, 0x66, 0x9D, 0xC0, 0xB8, 0xB1, 0x2F, 0xEB, 0xC0, 0x36,
                    0x08, 0x86, 0x0C, 0x31, 0xE2, 0xEE, 0xF6,
                ],
                PoolKind::UniswapV3,
                usdc_id,
                usdt0_id,
                VenueId(0),
                100,
                1,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000,
                    fee_pips: 100,
                    tick_spacing: 1,
                    initialised_ticks: Vec::new(),
                },
            )
            .expect("v3 pool");

        let mut curve_tokens = [[0u8; 20]; engine_amm::curve::MAX_COINS];
        curve_tokens[0] = usdc;
        curve_tokens[1] = usdt0;
        let curve_pool = builder
            .add_curve_pool(
                [
                    0x49, 0xB7, 0x20, 0xF1, 0xAA, 0xB2, 0x62, 0x60, 0xBE, 0xAE, 0xC9, 0x3A, 0x7B,
                    0xEB, 0x5B, 0xF2, 0x92, 0x5B, 0x2A, 0x8F,
                ],
                PoolKind::CurveStableSwapNg,
                usdc_id,
                usdt0_id,
                VenueId(1),
                4_000_000,
                engine_state::curve_state::CurveSlot::new(
                    engine_amm::curve::StableSwapState {
                        n_coins: 2,
                        balances: [1_000_000_000_000, 1_000_000_000_000, 0, 0, 0, 0, 0, 0],
                        rates: [1_000_000_000_000_000_000; engine_amm::curve::MAX_COINS],
                        a_param: 100 * engine_amm::curve::A_PRECISION,
                        fee: 0,
                        offpeg_fee_multiplier: 0,
                    },
                    curve_tokens,
                ),
            )
            .expect("curve pool");

        let universe = builder.build();
        let mut route_table = RouteTable::new();
        let mut hops = ArrayVec::new();
        hops.push(Hop::new(v3_pool, 0));
        hops.push(Hop::new(curve_pool, 1));
        let mut tokens = ArrayVec::new();
        tokens.push(usdc_id);
        tokens.push(usdt0_id);
        tokens.push(usdc_id);
        let route_id = route_table.insert(Route {
            id: RouteId(0),
            strategy: StrategyKind::TwoHop,
            hop_count: 2,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        });
        let n_pools = universe.pools.len();
        let detection = DetectionContext {
            universe: std::sync::Arc::new(universe),
            route_table,
            pool_to_hot_routes: CsrBuilder::new(n_pools as u32).build(),
            selectors: SelectorRegistry::canonical(),
            telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![0; n_pools])),
            family_quote_active: crate::runtime_policy::default_active_families().dense(),
        };

        let cd = build_mantis_calldata(&detection, route_id, 1_000_000, 1).expect("calldata");

        // Hop[1] starts at ABI word 12; fee/coinInIdx/coinOutIdx are words 15/17/18.
        assert_eq!(
            word(&cd, 15),
            U256::ZERO,
            "Curve fee is unused by Mantis and must not hit legacy uint16 overflow"
        );
        assert_eq!(
            word(&cd, 17),
            U256::from(1u8),
            "USDT0 must be Curve coin index 1"
        );
        assert_eq!(word(&cd, 18), U256::ZERO, "USDC must be Curve coin index 0");
    }

    #[test]
    fn over_four_hop_routes_are_not_capital_executable() {
        // Codex #6: the executor only encodes 2-4 hop routes, so anything outside 2..=4 has NO
        // submittable calldata path and can never be a capital route (>4-hop are catalog/provenance
        // only). build_mantis_calldata must return None for an out-of-range hop_count, which also
        // makes prepare_submission yield empty calldata so the submit thread skips it.
        let usdc = [
            0xAF, 0x88, 0xD0, 0x65, 0xE7, 0x7C, 0x8C, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xB3,
            0xA4, 0x32, 0x26, 0x8E, 0x58, 0x31,
        ];
        let usdt0 = [
            0xFD, 0x08, 0x6B, 0xC7, 0xCD, 0x5C, 0x48, 0x1D, 0xCC, 0x9C, 0x85, 0xEB, 0xE4, 0x78,
            0xA1, 0xC0, 0xB6, 0x9F, 0xCB, 0xB9,
        ];
        let mut builder = UniverseBuilder::new();
        let usdc_id = builder.add_token(usdc, 6, RiskClass::Permissionless);
        let usdt0_id = builder.add_token(usdt0, 6, RiskClass::Permissionless);
        let v3_pool = builder
            .add_v3_pool(
                [
                    0xBE, 0x3A, 0xD6, 0xA5, 0x66, 0x9D, 0xC0, 0xB8, 0xB1, 0x2F, 0xEB, 0xC0, 0x36,
                    0x08, 0x86, 0x0C, 0x31, 0xE2, 0xEE, 0xF6,
                ],
                PoolKind::UniswapV3,
                usdc_id,
                usdt0_id,
                VenueId(0),
                100,
                1,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000,
                    fee_pips: 100,
                    tick_spacing: 1,
                    initialised_ticks: Vec::new(),
                },
            )
            .expect("v3 pool");
        let universe = builder.build();
        let mut route_table = RouteTable::new();
        let make_route = |id: u32, hop_count: u8| {
            let mut hops = ArrayVec::new();
            hops.push(Hop::new(v3_pool, 0));
            hops.push(Hop::new(v3_pool, 1));
            let mut tokens = ArrayVec::new();
            tokens.push(usdc_id);
            tokens.push(usdt0_id);
            tokens.push(usdc_id);
            Route {
                id: RouteId(id),
                strategy: StrategyKind::TwoHop,
                hop_count,
                hops,
                tokens,
                family_signature: 0,
                calldata_template_id: 0,
                gas_template_id: 0,
                l1_size_bucket_template: 0,
                amount_grid_id: 0,
                risk_flags: 0,
                readiness_mask: 0,
                route_heat: 0,
                hot: true,
            }
        };
        let r5 = route_table.insert(make_route(0, 5));
        let r1 = route_table.insert(make_route(1, 1));
        let n_pools = universe.pools.len();
        let detection = DetectionContext {
            universe: std::sync::Arc::new(universe),
            route_table,
            pool_to_hot_routes: CsrBuilder::new(n_pools as u32).build(),
            selectors: SelectorRegistry::canonical(),
            telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![0; n_pools])),
            family_quote_active: crate::runtime_policy::default_active_families().dense(),
        };
        assert!(
            build_mantis_calldata(&detection, r5, 1_000_000, 1).is_none(),
            ">4-hop route must not be capital-executable"
        );
        assert!(
            build_mantis_calldata(&detection, r1, 1_000_000, 1).is_none(),
            "<2-hop route must not be capital-executable"
        );
    }

    #[test]
    fn over_uint24_fee_makes_calldata_none() {
        // The Solidity TradeHopV2.fee is a uint24. A hop fee above 16_777_215 would silently
        // truncate in ABI encoding (wrong fee tier / pool), so build_mantis_calldata must return
        // None for it — while a normal V3 fee tier (3000 = 0.30%) builds fine.
        let usdc = [
            0xAF, 0x88, 0xD0, 0x65, 0xE7, 0x7C, 0x8C, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xB3,
            0xA4, 0x32, 0x26, 0x8E, 0x58, 0x31,
        ];
        let usdt0 = [
            0xFD, 0x08, 0x6B, 0xC7, 0xCD, 0x5C, 0x48, 0x1D, 0xCC, 0x9C, 0x85, 0xEB, 0xE4, 0x78,
            0xA1, 0xC0, 0xB6, 0x9F, 0xCB, 0xB9,
        ];
        // Build a 2-hop USDC->USDT0->USDC route over two V3 pools with a chosen fee tier.
        let build = |fee_pips: u32, mnp: u128| -> Option<Vec<u8>> {
            let mut builder = UniverseBuilder::new();
            let usdc_id = builder.add_token(usdc, 6, RiskClass::Permissionless);
            let usdt0_id = builder.add_token(usdt0, 6, RiskClass::Permissionless);
            let mk_v3 = |b: &mut UniverseBuilder, addr: [u8; 20], venue: u16| {
                b.add_v3_pool(
                    addr,
                    PoolKind::UniswapV3,
                    usdc_id,
                    usdt0_id,
                    VenueId(venue),
                    fee_pips,
                    1,
                    V3State {
                        sqrt_price_x96: U256::from(1u128) << 96,
                        tick: 0,
                        liquidity: 1_000_000,
                        fee_pips,
                        tick_spacing: 1,
                        initialised_ticks: Vec::new(),
                    },
                )
                .expect("v3 pool")
            };
            let p0 = mk_v3(&mut builder, [0xBE; 20], 0);
            let p1 = mk_v3(&mut builder, [0xCE; 20], 1);
            let universe = builder.build();
            let mut route_table = RouteTable::new();
            let mut hops = ArrayVec::new();
            hops.push(Hop::new(p0, 0));
            hops.push(Hop::new(p1, 1));
            let mut tokens = ArrayVec::new();
            tokens.push(usdc_id);
            tokens.push(usdt0_id);
            tokens.push(usdc_id);
            let route_id = route_table.insert(Route {
                id: RouteId(0),
                strategy: StrategyKind::TwoHop,
                hop_count: 2,
                hops,
                tokens,
                family_signature: 0,
                calldata_template_id: 0,
                gas_template_id: 0,
                l1_size_bucket_template: 0,
                amount_grid_id: 0,
                risk_flags: 0,
                readiness_mask: 0,
                route_heat: 0,
                hot: true,
            });
            let n_pools = universe.pools.len();
            let detection = DetectionContext {
                universe: std::sync::Arc::new(universe),
                route_table,
                pool_to_hot_routes: CsrBuilder::new(n_pools as u32).build(),
                selectors: SelectorRegistry::canonical(),
                telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![0; n_pools])),
                family_quote_active: crate::runtime_policy::default_active_families().dense(),
            };
            build_mantis_calldata(&detection, route_id, 1_000_000, mnp)
        };

        // 2^24 fee → truncates in uint24 → must be rejected.
        assert!(
            build(0x0100_0000, 1).is_none(),
            "fee > uint24 max must return None (truncation guard)"
        );
        // A normal 0.30% tier builds fine (with a positive net-profit floor).
        assert!(build(3000, 1).is_some(), "normal V3 fee must build");
        // A ZERO net-profit floor still ENCODES (Codex #1): build_mantis_calldata is a pure encoder
        // and the REVM validator depends on the minNet=0 call. The live-safe floor lives in
        // prepare_submission, not here — see build_with_zero_min_profit_still_builds_for_revm_validation.
        assert!(
            build(3000, 0).is_some(),
            "min_net_profit == 0 must still encode (floor enforced in prepare_submission, not the encoder)"
        );
    }

    #[test]
    fn build_with_zero_min_profit_still_builds_for_revm_validation() {
        // REGRESSION GUARD (Codex #1): the REVM validator (`pipeline.rs` ~12347) builds the exact
        // executor calldata with `min_net_profit = 0` ON PURPOSE — REVM bytecode-executes it and
        // MEASURES the realized profit itself ("accept iff the route does not revert/lose"). So
        // `build_mantis_calldata` is a pure ENCODER and MUST return `Some` for a valid route even at
        // a zero floor; a `None` here would make the validator classify every candidate
        // `calldata_unexecutable` and reject it BEFORE REVM ever runs. The live-submit net-profit
        // floor is enforced upstream in `prepare_submission` (which always passes a >0 gas-covering
        // floor), NOT in this shared encoder.
        let usdc = [
            0xAF, 0x88, 0xD0, 0x65, 0xE7, 0x7C, 0x8C, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xB3,
            0xA4, 0x32, 0x26, 0x8E, 0x58, 0x31,
        ];
        let usdt0 = [
            0xFD, 0x08, 0x6B, 0xC7, 0xCD, 0x5C, 0x48, 0x1D, 0xCC, 0x9C, 0x85, 0xEB, 0xE4, 0x78,
            0xA1, 0xC0, 0xB6, 0x9F, 0xCB, 0xB9,
        ];
        let mut builder = UniverseBuilder::new();
        let usdc_id = builder.add_token(usdc, 6, RiskClass::Permissionless);
        let usdt0_id = builder.add_token(usdt0, 6, RiskClass::Permissionless);
        let mk_v3 = |b: &mut UniverseBuilder, addr: [u8; 20], venue: u16| {
            b.add_v3_pool(
                addr,
                PoolKind::UniswapV3,
                usdc_id,
                usdt0_id,
                VenueId(venue),
                3000,
                1,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000,
                    fee_pips: 3000,
                    tick_spacing: 1,
                    initialised_ticks: Vec::new(),
                },
            )
            .expect("v3 pool")
        };
        let p0 = mk_v3(&mut builder, [0xBE; 20], 0);
        let p1 = mk_v3(&mut builder, [0xCE; 20], 1);
        let universe = builder.build();
        let mut route_table = RouteTable::new();
        let mut hops = ArrayVec::new();
        hops.push(Hop::new(p0, 0));
        hops.push(Hop::new(p1, 1));
        let mut tokens = ArrayVec::new();
        tokens.push(usdc_id);
        tokens.push(usdt0_id);
        tokens.push(usdc_id);
        let route_id = route_table.insert(Route {
            id: RouteId(0),
            strategy: StrategyKind::TwoHop,
            hop_count: 2,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        });
        let n_pools = universe.pools.len();
        let detection = DetectionContext {
            universe: std::sync::Arc::new(universe),
            route_table,
            pool_to_hot_routes: CsrBuilder::new(n_pools as u32).build(),
            selectors: SelectorRegistry::canonical(),
            telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![0; n_pools])),
            family_quote_active: crate::runtime_policy::default_active_families().dense(),
        };
        assert!(
            build_mantis_calldata(&detection, route_id, 1_000_000, 0).is_some(),
            "build_mantis_calldata must encode calldata at minNet=0 so the REVM validator can \
             bytecode-execute it (the floor is enforced in prepare_submission, not the encoder)"
        );
    }

    #[test]
    fn mantis_calldata_sets_uniswap_v4_manager_and_pool_id() {
        let usdc = [
            0xAF, 0x88, 0xD0, 0x65, 0xE7, 0x7C, 0x8C, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xB3,
            0xA4, 0x32, 0x26, 0x8E, 0x58, 0x31,
        ];
        let usdt0 = [
            0xFD, 0x08, 0x6B, 0xC7, 0xCD, 0x5C, 0x48, 0x1D, 0xCC, 0x9C, 0x85, 0xEB, 0xE4, 0x78,
            0xA1, 0xC0, 0xB6, 0x9F, 0xCB, 0xB9,
        ];
        let v4_pool_manager = [
            0x36, 0x0E, 0x68, 0xFA, 0xCC, 0xCA, 0x8C, 0xA4, 0x95, 0xC1, 0xB7, 0x59, 0xFD, 0x9E,
            0xEE, 0x46, 0x6D, 0xB9, 0xFB, 0x32,
        ];
        let v4_pool_id = [0x42u8; 32];
        let mut v4_synthetic_addr = [0u8; 20];
        v4_synthetic_addr.copy_from_slice(&v4_pool_id[..20]);

        let mut builder = UniverseBuilder::new();
        let usdc_id = builder.add_token(usdc, 6, RiskClass::Permissionless);
        let usdt0_id = builder.add_token(usdt0, 6, RiskClass::Permissionless);
        let v4_pool = builder
            .add_v4_pool(
                v4_synthetic_addr,
                usdc_id,
                usdt0_id,
                VenueId(0),
                500,
                60,
                v4_pool_id,
                V3State {
                    sqrt_price_x96: U256::from(1u128) << 96,
                    tick: 0,
                    liquidity: 1_000_000,
                    fee_pips: 500,
                    tick_spacing: 60,
                    initialised_ticks: Vec::new(),
                },
            )
            .expect("v4 pool");
        let v3_pool = builder
            .add_v3_pool(
                [0xBE; 20],
                PoolKind::UniswapV3,
                usdc_id,
                usdt0_id,
                VenueId(1),
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
            .expect("v3 pool");

        let universe = builder.build();
        let mut route_table = RouteTable::new();
        let mut hops = ArrayVec::new();
        hops.push(Hop::new(v4_pool, 0));
        hops.push(Hop::new(v3_pool, 1));
        let mut tokens = ArrayVec::new();
        tokens.push(usdc_id);
        tokens.push(usdt0_id);
        tokens.push(usdc_id);
        let route_id = route_table.insert(Route {
            id: RouteId(0),
            strategy: StrategyKind::TwoHop,
            hop_count: 2,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        });
        let n_pools = universe.pools.len();
        let detection = DetectionContext {
            universe: std::sync::Arc::new(universe),
            route_table,
            pool_to_hot_routes: CsrBuilder::new(n_pools as u32).build(),
            selectors: SelectorRegistry::canonical(),
            telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![0; n_pools])),
            family_quote_active: crate::runtime_policy::default_active_families().dense(),
        };

        let cd = build_mantis_calldata(&detection, route_id, 1_000_000, 1).expect("calldata");

        // Hop[0] starts at ABI word 1. TradeHopV2 fields:
        // exchangeId=1, tickSpacing=8, poolManager=10, poolId=11.
        assert_eq!(word(&cd, 1), U256::from(20u8), "hop must be UniswapV4");
        assert_eq!(
            word(&cd, 8),
            U256::from(60u8),
            "V4 tick spacing is required"
        );
        assert_eq!(
            word_addr(&cd, 10),
            v4_pool_manager,
            "V4 PoolManager is required"
        );
        assert_eq!(
            word_bytes(&cd, 11),
            v4_pool_id,
            "full V4 poolId is required"
        );
    }
}
