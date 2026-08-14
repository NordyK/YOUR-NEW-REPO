//! End-to-end detection test.
//!
//! Builds a real universe (two V2 pools on the same WETH/USDC pair, dislocated so a
//! two-hop cycle profits), enumerates routes, builds the CSR pool→hot-route index, boots
//! the pipeline, injects a sequencer-feed frame that touches one of the pools through a
//! V2 router call, and asserts the engine emits at least one validated candidate.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64_simd::STANDARD as BASE64;
use ruint::aliases::U256;

use engine_config::EngineConfig;
use engine_gas::{GasOracle, GasSnapshot};
use engine_metrics::{LatencyHistograms, ReasonRing};
use engine_routes::{enumerate_two_hop_routes, CsrBuilder, RouteTable};
use engine_state::{PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, VenueId};

fn pad_u256(value: u128) -> [u8; 32] {
    let v = U256::from(value);
    v.to_be_bytes()
}

fn pad_address(addr: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr);
    out
}

/// Build a synthetic feed frame whose `l2Msg` is a `SignedTx` carrying an EIP-1559
/// transaction calling the V2 router's `swapExactTokensForTokens(amountIn, amountOutMin,
/// path, to, deadline)`.
fn build_router_swap_frame(
    sequence_number: u64,
    router_address: &[u8; 20],
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: u128,
) -> Vec<u8> {
    // V2 swapExactTokensForTokens selector = 0x38ed1739
    let mut data = Vec::new();
    data.extend_from_slice(&[0x38, 0xed, 0x17, 0x39]);
    data.extend_from_slice(&pad_u256(amount_in));
    data.extend_from_slice(&pad_u256(0)); // amountOutMin
    data.extend_from_slice(&pad_u256(0xa0)); // path offset = 5*32 = 160
    data.extend_from_slice(&pad_address(&[0x11; 20])); // to
    data.extend_from_slice(&pad_u256(0)); // deadline
    data.extend_from_slice(&pad_u256(2)); // path length
    data.extend_from_slice(&pad_address(token_in));
    data.extend_from_slice(&pad_address(token_out));

    // EIP-1559 RLP: [chainId, nonce, maxPrio, maxFee, gas, to, value, data, accessList,
    //                v, r, s]
    fn rlp_u(v: u64, out: &mut Vec<u8>) {
        if v == 0 {
            out.push(0x80);
            return;
        }
        let be = v.to_be_bytes();
        let lead = be.iter().position(|&b| b != 0).unwrap_or(7);
        let bytes = &be[lead..];
        if bytes.len() == 1 && bytes[0] < 0x80 {
            out.push(bytes[0]);
        } else {
            out.push(0x80 + bytes.len() as u8);
            out.extend_from_slice(bytes);
        }
    }
    fn rlp_bytes(b: &[u8], out: &mut Vec<u8>) {
        if b.len() == 1 && b[0] < 0x80 {
            out.push(b[0]);
        } else if b.len() < 56 {
            out.push(0x80 + b.len() as u8);
            out.extend_from_slice(b);
        } else {
            let len = (b.len() as u64).to_be_bytes();
            let lead = len.iter().position(|&x| x != 0).unwrap_or(7);
            out.push(0xb7 + (8 - lead) as u8);
            out.extend_from_slice(&len[lead..]);
            out.extend_from_slice(b);
        }
    }
    let mut payload = Vec::new();
    rlp_u(42_161, &mut payload); // chainId
    rlp_u(1, &mut payload); // nonce
    rlp_u(0, &mut payload); // maxPriority
    rlp_u(20_000_000, &mut payload); // maxFee
    rlp_u(500_000, &mut payload); // gasLimit
    payload.push(0x94); // to: 20-byte string
    payload.extend_from_slice(router_address);
    rlp_u(0, &mut payload); // value
    rlp_bytes(&data, &mut payload); // data
    payload.push(0xc0); // empty access list
    rlp_u(0, &mut payload); // y_parity
    rlp_u(0, &mut payload); // r
    rlp_u(0, &mut payload); // s
    let mut inner = Vec::new();
    inner.push(0x04); // L2 kind: SignedTx
    inner.push(0x02); // tx type: EIP-1559
                      // List header for `payload`.
    if payload.len() < 56 {
        inner.push(0xc0 + payload.len() as u8);
    } else {
        let len_be = (payload.len() as u64).to_be_bytes();
        let lead = len_be.iter().position(|&b| b != 0).unwrap_or(7);
        inner.push(0xf7 + (8 - lead) as u8);
        inner.extend_from_slice(&len_be[lead..]);
    }
    inner.extend_from_slice(&payload);
    let b64 = BASE64.encode_to_string(&inner);
    format!(
        r#"{{"sequenceNumber":{sequence_number},"timestamp":1,"blockNumber":1,"l2Msg":"{b64}"}}"#
    )
    .into_bytes()
}

#[test]
fn detection_picks_up_router_swap_through_stub_relay() {
    use std::net::TcpListener;
    use tungstenite::Message;

    // 1. Universe: WETH, USDC, two V2 pools on (WETH, USDC), dislocated.
    let mut ub = UniverseBuilder::new();
    let weth_addr = [0x82u8; 20];
    let usdc_addr = [0xafu8; 20];
    let weth = ub.add_token(weth_addr, 18, RiskClass::Permissionless);
    let usdc = ub.add_token(usdc_addr, 6, RiskClass::CentralisedStable);
    let pool_a = [0xa1u8; 20];
    let pool_b = [0xa2u8; 20];
    ub.add_v2_pool(
        pool_a,
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
    ub.add_v2_pool(
        pool_b,
        PoolKind::UniswapV2,
        weth,
        usdc,
        VenueId(1),
        30,
        V2State {
            reserves: V2Reserves256 {
                // Dislocated by ~3% vs pool A (WETH cheaper here: 3880 vs 4000 USDC
                // per WETH). A realistic gap so the WETH→USDC(A)→WETH(B) cycle nets a
                // few percent after two 30 bps fees — comfortably below the 30%-ROI
                // phantom-artifact guard, which a larger gap would (correctly) trip.
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 3_880_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .unwrap();
    // Profit accounting is denominated in ETH-wei, so the base (funding) token must
    // carry an ETH-price factor or the evaluator fails closed (UnsupportedShape). The
    // production startup path sets these from the token universe; replicate it here.
    // WETH: 1 raw wei == 1 ETH wei (q96 identity). USDC: ~$4000/ETH ⇒ 1 raw 6-dp unit
    // ≈ 2.5e8 ETH wei.
    ub.set_token_eth_price_q96(weth, U256::from(1u128) << 96);
    ub.set_token_eth_price_q96(usdc, U256::from(250_000_000u128) << 96);
    // WI-06 §3.3: seal the universe → the sole StateWriterPermit + the shared Arc<Universe> the state
    // thread publishes over (attached to the pipeline below before start_threads).
    let (universe, state_writer_permit) = ub.seal().into_parts();

    // 2. Routes: enumerate two-hop. Mark every route hot for the test.
    let mut rt = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut rt);
    for r in 0..rt.len() {
        let mut_r = rt.get_mut(engine_state::RouteId(r as u32));
        mut_r.hot = true;
    }

    // 3. CSR pool→hot routes.
    let mut csr = CsrBuilder::new(universe.pools.len() as u32);
    for r in rt.all() {
        for h in &r.hops {
            csr.push(h.pool.0, r.id);
        }
    }
    let pool_to_hot_routes = csr.build();

    // 4. Pipeline.
    let n_pools = universe.pools.len();
    let detection = Arc::new(engine::pipeline::DetectionContext {
        universe,
        route_table: rt,
        pool_to_hot_routes,
        selectors: engine_detect::SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![
            1u64 << 32;
            n_pools
        ])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    });

    // 5. Stub WebSocket relay on a free port.
    let router_address = [0x99u8; 20];
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("ws://127.0.0.1:{port}/feed");

    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).expect("accept ws");
        // Send a router swap from token A (WETH = pool's token0) → token B (USDC).
        let frame = build_router_swap_frame(
            1,
            &router_address,
            &[0x82; 20],
            &[0xaf; 20],
            1_000_000_000_000_000_000, // 1 ETH
        );
        ws.send(Message::Text(String::from_utf8(frame).expect("utf8")))
            .ok();
        // Hold the connection open briefly so the engine can drain.
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
    // Register the test router so the classifier matches the swap tx.
    cfg.routers.push(engine_config::RouterEntry {
        address: format!("0x{}", faster_hex::hex_string(&router_address)),
        label: "test_router".into(),
    });

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let lat = Arc::new(LatencyHistograms::new());
    let reas = Arc::new(ReasonRing::new());
    // WI-06 §3.3: build the sole publisher over the shared universe + hand it (with the sealed
    // permit) to the pipeline before start_threads moves them into the state-writer thread.
    let state_publisher = engine_state::publication::StatePublisher::new(
        std::sync::Arc::clone(&detection.universe),
        detection.route_table.len(),
        8192,
    );
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, lat, reas, detection);
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    pipeline.start_threads().expect("start_threads");

    // Wait for the engine to process the frame. Breaks immediately on success; the generous
    // budget only matters on a loaded machine (a full-suite run showed 3 s can be too tight
    // for connect+handshake+process under contention).
    let start = Instant::now();
    let mut got_candidate = false;
    while start.elapsed() < Duration::from_secs(10) {
        if pipeline.candidates.load(Ordering::Relaxed) > 0 {
            got_candidate = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    pipeline.signal_shutdown();
    pipeline.join();
    // Liveness: if the engine never connected (exactly the failure this test exists to catch),
    // the stub is still parked in `accept()` and joining it would hang the suite forever
    // (observed 2026-07-08: whole-workspace run wedged here, 0 CPU). Poke one throwaway
    // connection so `accept` returns and the thread runs out — the assert below then reports
    // the real failure instead of a silent hang.
    let _ = std::net::TcpStream::connect(("127.0.0.1", port));
    let _ = server_thread.join();

    assert!(
        got_candidate,
        "expected at least one candidate; frames={} dirty={} candidates={} validated={}",
        pipeline.frames_processed.load(Ordering::Relaxed),
        pipeline.dirty_events.load(Ordering::Relaxed),
        pipeline.candidates.load(Ordering::Relaxed),
        pipeline.validated.load(Ordering::Relaxed),
    );
}

/// Sanity helper to silence the unused-import warning when running the test in isolation.
#[allow(dead_code)]
fn _link_pool_kind() -> PoolKind {
    PoolKind::UniswapV2
}
