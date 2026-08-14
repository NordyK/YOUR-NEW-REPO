//! End-to-end triangle detection test.
//!
//! Builds a three-pool A-B, B-C, C-A universe with dislocation, ships a V2 router swap
//! through a stub WebSocket relay that dirties one of the pools, and asserts the engine
//! produces at least one triangle candidate.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64_simd::STANDARD as BASE64;
use ruint::aliases::U256;

use engine_config::EngineConfig;
use engine_gas::{GasOracle, GasSnapshot};
use engine_metrics::{LatencyHistograms, ReasonRing};
use engine_routes::{enumerate_triangles, enumerate_two_hop_routes, CsrBuilder, RouteTable};
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

fn build_router_swap_frame(
    sequence_number: u64,
    router_address: &[u8; 20],
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: u128,
) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[0x38, 0xed, 0x17, 0x39]);
    data.extend_from_slice(&pad_u256(amount_in));
    data.extend_from_slice(&pad_u256(0));
    data.extend_from_slice(&pad_u256(0xa0));
    data.extend_from_slice(&pad_address(&[0x11; 20]));
    data.extend_from_slice(&pad_u256(0));
    data.extend_from_slice(&pad_u256(2));
    data.extend_from_slice(&pad_address(token_in));
    data.extend_from_slice(&pad_address(token_out));

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
    rlp_u(42_161, &mut payload);
    rlp_u(1, &mut payload);
    rlp_u(0, &mut payload);
    rlp_u(20_000_000, &mut payload);
    rlp_u(500_000, &mut payload);
    payload.push(0x94);
    payload.extend_from_slice(router_address);
    rlp_u(0, &mut payload);
    rlp_bytes(&data, &mut payload);
    payload.push(0xc0);
    rlp_u(0, &mut payload);
    rlp_u(0, &mut payload);
    rlp_u(0, &mut payload);
    let mut inner = Vec::new();
    inner.push(0x04);
    inner.push(0x02);
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
fn detection_picks_up_triangle_candidate() {
    use std::net::TcpListener;
    use tungstenite::Message;

    // 1. Universe: three tokens A, B, C and three V2 pools A-B, B-C, C-A. Dislocated
    //    so that A→B→C→A profits (A and C trade ~1:1 directly, but A→B→C is cheaper).
    let mut ub = UniverseBuilder::new();
    let token_a = ub.add_token([0xaa; 20], 18, RiskClass::Permissionless);
    let token_b = ub.add_token([0xbb; 20], 18, RiskClass::Permissionless);
    let token_c = ub.add_token([0xcc; 20], 18, RiskClass::Permissionless);
    // Pool A-B: 1 A → 2 B
    ub.add_v2_pool(
        [0xa1u8; 20],
        PoolKind::UniswapV2,
        token_a,
        token_b,
        VenueId(0),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 2_000_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .unwrap();
    // Pool B-C: 1 B → 2 C
    ub.add_v2_pool(
        [0xb1u8; 20],
        PoolKind::UniswapV2,
        token_b,
        token_c,
        VenueId(0),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 2_000_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .unwrap();
    // Pool C-A: 4 C → 1.05 A  (5% dislocation, well above the 60 bps cycle fee floor).
    ub.add_v2_pool(
        [0xc1u8; 20],
        PoolKind::UniswapV2,
        token_c,
        token_a,
        VenueId(0),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 4_000_000_000_000_000_000_000,
                reserve1: 1_050_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .unwrap();

    let mut rt = RouteTable::new();
    enumerate_two_hop_routes(ub.pools(), &mut rt);
    enumerate_triangles(ub.pools(), &mut rt);
    for r in 0..rt.len() {
        rt.get_mut(engine_state::RouteId(r as u32)).hot = true;
    }
    let mut csr = CsrBuilder::new(ub.pools().len() as u32);
    for r in rt.all() {
        for h in &r.hops {
            csr.push(h.pool.0, r.id);
        }
    }
    let pool_to_hot_routes = csr.build();
    // Profit accounting is denominated in ETH-wei: the cycle's base (funding) token
    // must carry an ETH-price factor or the triangle evaluator fails closed
    // (UnsupportedShape). Production sets these at startup from the token universe;
    // replicate it here with ratios consistent with the pools (1 A = 2 B = 4 C, so
    // A=1, B=0.5, C=0.25 ETH).
    ub.set_token_eth_price_q96(token_a, U256::from(1u128) << 96);
    ub.set_token_eth_price_q96(token_b, U256::from(1u128) << 95);
    ub.set_token_eth_price_q96(token_c, U256::from(1u128) << 94);
    // WI-06 §3.3: seal the universe → the sole StateWriterPermit + the shared Arc<Universe> the state
    // thread publishes over (attached to the pipeline below before start_threads).
    let (universe, state_writer_permit) = ub.seal().into_parts();
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

    let router_address = [0x99u8; 20];
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("ws://127.0.0.1:{port}/feed");
    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).unwrap();
        let frame = build_router_swap_frame(
            1,
            &router_address,
            &[0xaa; 20],
            &[0xbb; 20],
            1_000_000_000_000_000_000,
        );
        ws.send(Message::Text(String::from_utf8(frame).unwrap()))
            .ok();
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
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
    pipeline.start_threads().unwrap();

    let start = Instant::now();
    let mut saw_candidate = false;
    while start.elapsed() < Duration::from_secs(3) {
        if pipeline.candidates.load(Ordering::Relaxed) > 0 {
            saw_candidate = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    pipeline.signal_shutdown();
    pipeline.join();
    let _ = server_thread.join();
    assert!(
        saw_candidate,
        "expected a candidate; frames={} dirty={} candidates={} validated={}",
        pipeline.frames_processed.load(Ordering::Relaxed),
        pipeline.dirty_events.load(Ordering::Relaxed),
        pipeline.candidates.load(Ordering::Relaxed),
        pipeline.validated.load(Ordering::Relaxed),
    );
}
