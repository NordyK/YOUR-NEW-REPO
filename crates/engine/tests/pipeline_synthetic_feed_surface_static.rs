use std::{
    net::TcpListener,
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

use base64_simd::STANDARD as BASE64;
use ruint::aliases::U256;
use tungstenite::Message;

use engine_config::{EngineConfig, RouterEntry};
use engine_gas::{GasOracle, GasSnapshot};
use engine_metrics::{LatencyHistograms, ReasonRing};
use engine_routes::{enumerate_two_hop_routes, CsrBuilder, RouteTable};
use engine_state::{PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, VenueId};

fn pad_u256(value: u128) -> [u8; 32] {
    U256::from(value).to_be_bytes()
}

fn pad_address(addr: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr);
    out
}

fn rlp_u(value: u64, out: &mut Vec<u8>) {
    if value == 0 {
        out.push(0x80);
        return;
    }
    let be = value.to_be_bytes();
    let lead = be.iter().position(|&byte| byte != 0).unwrap_or(7);
    let bytes = &be[lead..];
    if bytes.len() == 1 && bytes[0] < 0x80 {
        out.push(bytes[0]);
    } else {
        out.push(0x80 + bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
}

fn rlp_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        out.push(bytes[0]);
    } else if bytes.len() < 56 {
        out.push(0x80 + bytes.len() as u8);
        out.extend_from_slice(bytes);
    } else {
        let len = (bytes.len() as u64).to_be_bytes();
        let lead = len.iter().position(|&byte| byte != 0).unwrap_or(7);
        out.push(0xb7 + (8 - lead) as u8);
        out.extend_from_slice(&len[lead..]);
        out.extend_from_slice(bytes);
    }
}

fn eip1559_list_header(payload_len: usize, out: &mut Vec<u8>) {
    if payload_len < 56 {
        out.push(0xc0 + payload_len as u8);
    } else {
        let len_be = (payload_len as u64).to_be_bytes();
        let lead = len_be.iter().position(|&byte| byte != 0).unwrap_or(7);
        out.push(0xf7 + (8 - lead) as u8);
        out.extend_from_slice(&len_be[lead..]);
    }
}

fn build_router_swap_frame(
    sequence_number: u64,
    router_address: &[u8; 20],
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: u128,
) -> String {
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
    eip1559_list_header(payload.len(), &mut inner);
    inner.extend_from_slice(&payload);

    let b64 = BASE64.encode_to_string(&inner);
    format!(
        r#"{{"sequenceNumber":{sequence_number},"timestamp":1,"blockNumber":1,"l2Msg":"{b64}"}}"#
    )
}

fn build_detection_context() -> Arc<engine::pipeline::DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let weth_addr = [0x82; 20];
    let usdc_addr = [0xaf; 20];
    let weth = ub.add_token(weth_addr, 18, RiskClass::Permissionless);
    let usdc = ub.add_token(usdc_addr, 6, RiskClass::CentralisedStable);
    ub.set_token_eth_price_q96(weth, U256::from(1u128) << 96);
    ub.set_token_eth_price_q96(usdc, U256::from(250_000_000u128) << 96);

    ub.add_v2_pool(
        [0xa1; 20],
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
    .expect("add high-price pool");
    ub.add_v2_pool(
        [0xa2; 20],
        PoolKind::UniswapV2,
        weth,
        usdc,
        VenueId(1),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 3_880_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add low-price pool");

    let universe = ub.build();
    let mut route_table = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut route_table);
    assert!(
        route_table.len() >= 2,
        "two dislocated pools must enumerate both route directions"
    );
    for route_id in 0..route_table.len() {
        route_table
            .get_mut(engine_state::RouteId(route_id as u32))
            .hot = true;
    }

    let mut csr = CsrBuilder::new(universe.pools.len() as u32);
    for route in route_table.all() {
        for hop in &route.hops {
            csr.push(hop.pool.0, route.id);
        }
    }

    let n_pools = universe.pools.len();
    Arc::new(engine::pipeline::DetectionContext {
        universe: Arc::new(universe),
        route_table,
        pool_to_hot_routes: csr.build(),
        selectors: engine_detect::SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![
            1u64 << 32;
            n_pools
        ])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    })
}

fn counter_snapshot(pipeline: &engine::pipeline::Pipeline) -> (u64, u64, u64, u64, u64) {
    (
        pipeline.frames_processed.load(Ordering::Relaxed),
        pipeline.dirty_events.load(Ordering::Relaxed),
        pipeline.candidates.load(Ordering::Relaxed),
        pipeline.validated.load(Ordering::Relaxed),
        pipeline.submitted.load(Ordering::Relaxed),
    )
}

fn wait_for_would_submit(pipeline: &engine::pipeline::Pipeline) -> (u64, u64, u64, u64, u64) {
    let start = Instant::now();
    loop {
        let snapshot = counter_snapshot(pipeline);
        if snapshot.4 > 0 || start.elapsed() >= Duration::from_secs(3) {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn stub_relay_pipeline_reaches_dirty_and_candidate_counters() {
    let detection = build_detection_context();
    let router_address = [0x99; 20];

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub relay");
    let endpoint = format!(
        "ws://127.0.0.1:{}/feed",
        listener.local_addr().unwrap().port()
    );
    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept relay client");
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).expect("accept websocket");
        let frame = build_router_swap_frame(
            1,
            &router_address,
            &[0x82; 20],
            &[0xaf; 20],
            1_000_000_000_000_000_000,
        );
        ws.send(Message::Text(frame.into())).ok();
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
    cfg.routers.push(RouterEntry {
        address: format!("0x{}", faster_hex::hex_string(&router_address)),
        label: "pipeline_surface_test_router".into(),
    });
    assert!(
        !cfg.live_submit,
        "synthetic laptop guardrail must not enable live sequencer writes"
    );

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let latencies = Arc::new(LatencyHistograms::new());
    let reasons = Arc::new(ReasonRing::new());
    // WI-06 §3.3: attach the sole publisher (over the shared universe) + a write permit before
    // start_threads moves them into the state-writer thread. The permit is a type-level write
    // witness — F1 brands it, so this harness uses the WILDCARD `test_mint` permit to witness a
    // universe it did not itself seal (unreachable in production; dev-only `test-permit` feature).
    let state_writer_permit = engine_state::publication::StateWriterPermit::test_mint();
    let state_publisher = engine_state::publication::StatePublisher::new(
        std::sync::Arc::clone(&detection.universe),
        detection.route_table.len(),
        8192,
    );
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, latencies, reasons, detection);
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    pipeline.start_threads().expect("start pipeline");

    let (frames, dirty, candidates, validated, submitted) = wait_for_would_submit(&pipeline);

    pipeline.signal_shutdown();
    pipeline.join();
    let _ = server_thread.join();

    assert!(
        frames >= 1,
        "stub relay frame should traverse parser; frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}"
    );
    assert!(
        dirty >= 1,
        "router swap should create a dirty-pool event; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        candidates >= 1,
        "dirty event should reach sim and emit at least one candidate; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        validated >= 1,
        "candidate should reach the validator before the dry-run would-submit decision; frames={frames} dirty={dirty} candidates={candidates} validated={validated}"
    );
    assert!(
        submitted >= 1,
        "dry-run would-submit counter should increment after validation; frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}"
    );
}
