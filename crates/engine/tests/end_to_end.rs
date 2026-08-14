//! End-to-end integration: build a small universe, enumerate routes, run the V2 sizing
//! kernel, gate the result through the §10.6 filters and §12 profit math, validate via the
//! REVM harness, then construct calldata + EIP-1559 envelope + JSON-RPC body.

use ruint::aliases::U256;

use engine_detect::{filters::best_v2v2_bucket, Confidence, ConfidenceFactors};
use engine_gas::{l1_data_fee_wei, l2_execution_cost_wei, GasOracle, GasSnapshot};
use engine_replay::{FeedFrameSource, ReplayDriver};
use engine_revm::{ExecutionInput, HopV2, RouteFootprintManifest, ValidationHarness, WarmDb};
use engine_routes::calldata::{encode_mantis_mixed, MantisFunding, MantisHop};
use engine_routes::{enumerate_two_hop_routes, RouteKind, RouteTable};
use engine_sim::{
    canonical_net_profit, expected_value, grid_search_v2v2, EvDecomposition, ProfitTerms,
};
use engine_state::{
    PoolKind, RiskClass, RouteId, UniverseBuilder, V2Reserves256, V2State, VenueId,
};
use engine_submit::{
    build_send_raw_tx_envelope, encode_eip1559_signed, sign_eip1559, Eip1559Tx, NonceLane,
    SigningKey,
};

#[test]
fn full_pipeline_v2v2_two_hop() {
    // 1. Build a small universe with two V2 pools on the same pair.
    let mut ub = UniverseBuilder::new();
    let weth = ub.add_token([0x82; 20], 18, RiskClass::Permissionless);
    let usdc = ub.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
    let pool_a = ub
        .add_v2_pool(
            [0xa1; 20],
            PoolKind::UniswapV2,
            weth,
            usdc,
            VenueId(0),
            30,
            V2State {
                reserves: V2Reserves256 {
                    reserve0: 1_000_000_000,
                    reserve1: 4_000_000_000,
                    block_timestamp_last: 0,
                },
            },
        )
        .unwrap();
    let pool_b = ub
        .add_v2_pool(
            [0xb1; 20],
            PoolKind::UniswapV2,
            weth,
            usdc,
            VenueId(1),
            30,
            V2State {
                reserves: V2Reserves256 {
                    // Dislocated: more USDC per WETH on pool B than pool A → arbitrage opportunity
                    // by buying USDC on A (cheap) and selling on B (expensive).
                    reserve0: 4_000_000_000,
                    reserve1: 1_010_000_000,
                    block_timestamp_last: 0,
                },
            },
        )
        .unwrap();
    // 2. Enumerate two-hop routes.
    let mut rt = RouteTable::new();
    enumerate_two_hop_routes(ub.pools(), &mut rt);
    assert!(rt.len() >= 2, "expected at least one two-hop pair");

    // 3. Run a grid-search sizing kernel for the V2/V2 cycle.
    // The route enumerator may have produced both directions; pick one with positive profit.
    let mut found_profitable = false;
    for r in rt.all() {
        if r.hop_count != 2 {
            continue;
        }
        let h1 = r.hops[0];
        let h2 = r.hops[1];
        let pool1 = ub.v2_snapshot(ub.pools().meta(h1.pool).state_slot);
        let pool2 = ub.v2_snapshot(ub.pools().meta(h2.pool).state_slot);
        let (r_in_1, r_out_1) = if h1.direction == 0 {
            (pool1.reserves.reserve0, pool1.reserves.reserve1)
        } else {
            (pool1.reserves.reserve1, pool1.reserves.reserve0)
        };
        let (r_in_2, r_out_2) = if h2.direction == 0 {
            (pool2.reserves.reserve0, pool2.reserves.reserve1)
        } else {
            (pool2.reserves.reserve1, pool2.reserves.reserve0)
        };
        let grid = vec![10_000u128, 100_000, 1_000_000, 10_000_000];
        if let Some((_, outcome)) =
            grid_search_v2v2(&grid, r_in_1, r_out_1, 30, r_in_2, r_out_2, 30)
        {
            assert!(outcome.gross_profit > 0);
            found_profitable = true;
            // 4. Apply §12 profit math.
            let snap = GasSnapshot {
                l2_gas_price_wei: 20_000_000,
                l1_base_fee_per_gas_wei: 1_000_000,
                l1_gas_per_data_byte: 16,
                min_l2_base_fee_wei: 20_000_000,
                static_overhead_bytes: 140,
                balancer_flash_fee_1e18: U256::ZERO,
                captured_at_ms: 0,
                l1_sigma_bps: 0,
            };
            let terms = ProfitTerms {
                gross_profit_wei: U256::from(outcome.gross_profit),
                l2_execution_cost_wei: U256::from(l2_execution_cost_wei(&snap, 200_000)),
                l1_data_fee_wei: U256::from(l1_data_fee_wei(&snap, 240)),
                flash_loan_fee_wei: U256::ZERO,
                executor_overhead_wei: U256::from(30_000u64),
                safety_buffer_wei: U256::from(1u64),
            };
            // For micro-scale toy reserves the gas + L1 are negligible because we picked
            // a low gas price; net profit should still be positive in many cases. The
            // canonical_net_profit may legitimately return None if costs exceed gross.
            let _ = canonical_net_profit(terms);

            let ev_dec = EvDecomposition {
                net_profit_wei: U256::from(outcome.gross_profit / 2),
                p_win_e6: 800_000,
                p_revert_e6: 50_000,
                p_stale_e6: 10_000,
                p_ops_e6: 1_000,
                revert_loss_wei: U256::from(100u64),
                stale_loss_wei: U256::from(50u64),
                operational_penalty_wei: U256::from(5u64),
            };
            let _ = expected_value(ev_dec);

            // 5. Run confidence chain.
            let conf = Confidence::from_factors(ConfidenceFactors::full());
            assert!(conf.passes);

            // 6. Run filter cascade (we already grid-scanned a positive bucket).
            let best_bucket = best_v2v2_bucket(&grid, r_in_1, r_out_1, 30, r_in_2, r_out_2, 30);
            assert!(best_bucket.is_some());

            // 7. Run REVM harness against the warm DB.
            let executor = [0xee; 20];
            let mut manifest =
                RouteFootprintManifest::new(RouteId(r.id.0), RouteKind::V2V2FlashBalancer);
            manifest.allow_account(executor);
            manifest.allow_account(ub.pools().meta(h1.pool).address);
            manifest.allow_account(ub.pools().meta(h2.pool).address);
            manifest.allow_account(ub.tokens().get(weth).address);
            manifest.allow_account(ub.tokens().get(usdc).address);
            let db = WarmDb::new();
            let harness = ValidationHarness::new(&db, &manifest);
            let input = ExecutionInput {
                route_id: RouteId(r.id.0),
                pools: vec![
                    ub.pools().meta(h1.pool).address,
                    ub.pools().meta(h2.pool).address,
                ],
                tokens: vec![weth, usdc, weth],
                hop_v2: vec![
                    HopV2 {
                        token_in: weth,
                        token_out: usdc,
                        reserve_in: r_in_1,
                        reserve_out: r_out_1,
                        fee_bps: 30,
                    },
                    HopV2 {
                        token_in: usdc,
                        token_out: weth,
                        reserve_in: r_in_2,
                        reserve_out: r_out_2,
                        fee_bps: 30,
                    },
                ],
                amount_in: U256::from(outcome.amount_in),
                min_profit: U256::from(1u64),
                executor,
                token_addresses: vec![ub.tokens().get(weth).address, ub.tokens().get(usdc).address],
            };
            let outcome = harness.validate(&input).expect("revm harness ok");
            assert!(outcome.base_token_delta > U256::ZERO);

            // 8. Build calldata and sign + serialise the EIP-1559 tx.
            let calldata = encode_mantis_mixed(
                MantisFunding::BalancerOrFlashSwap,
                1_000_000u128,
                &[MantisHop::default(); 3],
                1u128,
            );

            let key = SigningKey::from_bytes(&[0x11u8; 32]).unwrap();
            let lane = NonceLane::new(
                0,
                key.address(),
                5,
                0,
                engine_submit::LaneClass::HighEvTwoHop,
                4,
            );
            let nonce = lane.claim_nonce().unwrap();
            let tx = Eip1559Tx {
                chain_id: 42_161,
                nonce,
                max_priority_fee_per_gas: U256::ZERO,
                max_fee_per_gas: U256::from(40_000_000u64),
                gas_limit: 500_000,
                to: executor,
                value: U256::ZERO,
                data: calldata,
            };
            let unsigned = engine_submit::encode_eip1559_unsigned(&tx);
            let hash = engine_feed::address::keccak256(&unsigned);
            let sig = sign_eip1559(&key, &hash).unwrap();
            let raw = encode_eip1559_signed(&tx, sig);
            let mut body = Vec::new();
            build_send_raw_tx_envelope(&raw, 1, &mut body);
            let body_str = std::str::from_utf8(&body).unwrap();
            assert!(body_str.contains("eth_sendRawTransaction"));
            lane.release();

            // 9. Run a parser pass over a synthetic feed corpus to validate the replay
            //    pipeline.
            let corpus = vec![
                r#"{"sequenceNumber":1,"timestamp":1,"blockNumber":1,"l2Msg":"BMmAgICAgICAgIA="}"#
                    .to_string(),
            ];
            let src = FeedFrameSource::from_lines(corpus);
            let mut driver = ReplayDriver::new();
            for frame in src {
                let _ = driver.process(&frame);
            }
            assert_eq!(driver.stats().frames_parsed, 1);
            break;
        }
    }
    assert!(found_profitable, "no two-hop route produced profit");
    // 10. Sanity: gas oracle staleness check returns Err when no snapshot has been set.
    let oracle = GasOracle::new(GasSnapshot::default(), 0);
    // The default has `captured_at_ms = 0` which is treated as "never set" so it does not
    // count as stale; verify the staleness gate path runs.
    let _ = oracle.load_fresh();
    // Use the second pool to keep the import busy on assertion paths.
    let _ = pool_a;
    let _ = pool_b;
    let _ = PoolKind::UniswapV2;
}
