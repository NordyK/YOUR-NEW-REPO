//! WI-13 fix round (codex NEEDS-FIX-FL14, corrections 1+2) — the gas authority must be the
//! SOLE writer of fee bytes and freshness at running-engine scope.
//!
//! Codex's system-scope finding: WI-13 made `refresh_gas_authority_once` fail-closed in
//! isolation, but the pre-existing `engine-bg` `pipeline::background_loop` remained a second
//! gas writer — every ~1s it reloaded the legacy `GasSnapshot`, unconditionally stamped
//! `captured_at_ms = now_ms()` and stored it. After one good seed, a persistently-FAILING
//! authority refresher was therefore masked: stale L1 fee + tokenGasUnits stayed
//! fresh-looking to the submit gate's `load_fresh()` + `l1 > 0` check (FL-14 at system
//! scope).
//!
//! Two tests close it:
//! * a STATIC sole-writer contract over the engine sources (the test that would have caught
//!   the second writer), and
//! * an END-TO-END worker-topology test that spins the REAL production writer
//!   (`gas_refresher::spawn_gas_refresher` — after correction 1 the only gas writer the
//!   engine spawns) against a loopback-only JSON-RPC server: one good publication, then a
//!   persistent outage — asserting value, freshness, generation and volatility all stay
//!   byte-identical and the submit-admission check declines as the snapshot ages out.
//!
//! Money firewall: loopback (127.0.0.1) sockets only — no live endpoint, no paid RPC, no
//! broadcast.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_gas::{GasOracle, GasSnapshot};

// -------------------------------------------------------------------------------------------
// 1. Static sole-writer contract.
// -------------------------------------------------------------------------------------------

/// The engine spawns exactly ONE gas writer: the authority refresher. `pipeline.rs` must not
/// contain the legacy `engine-bg` gas loop (or any other `GasOracle` store / freshness
/// stamp); `main.rs` must still spawn the authority refresher; and the refresher must drive
/// the one-block authority transaction. This is the contract whose absence let FL-14 survive
/// at system scope.
#[test]
fn pipeline_has_no_second_gas_writer_static() {
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let pipeline = std::fs::read_to_string(src_dir.join("pipeline.rs")).expect("read pipeline.rs");
    let main_rs = std::fs::read_to_string(src_dir.join("main.rs")).expect("read main.rs");
    let refresher =
        std::fs::read_to_string(src_dir.join("gas_refresher.rs")).expect("read gas_refresher.rs");

    assert!(
        !pipeline.contains("fn background_loop"),
        "pipeline.rs still defines the legacy `background_loop` gas writer — the WI-13 \
         authority refresher must be the SOLE writer of gas fee bytes and freshness \
         (codex NEEDS-FIX-FL14 correction 1)"
    );
    assert!(
        !pipeline.contains("\"engine-bg\""),
        "pipeline.rs still spawns the `engine-bg` thread that republished the legacy \
         GasSnapshot with a fresh captured_at_ms every second, masking a failing authority \
         refresher (FL-14 at system scope)"
    );
    assert!(
        !pipeline.contains("gas.store("),
        "pipeline.rs must not store GasSnapshots — only a COMPLETE authority publication \
         (engine-gas::authority) may write fee bytes"
    );
    assert!(
        !pipeline.contains("captured_at_ms = engine_gas::now_ms()"),
        "pipeline.rs must not stamp gas freshness — a fresh captured_at_ms may come only \
         from a COMPLETE authority publication"
    );
    assert!(
        main_rs.contains("spawn_gas_refresher"),
        "main.rs must still spawn the authority gas refresher (the sole writer)"
    );
    assert!(
        refresher.contains("refresh_gas_authority_once"),
        "gas_refresher.rs must drive the one-block authority transaction"
    );
}

// -------------------------------------------------------------------------------------------
// 2. End-to-end worker topology over the real writer.
// -------------------------------------------------------------------------------------------

// Fixture values mirroring the engine-gas WI-13 suite (realistic, and NOT the legacy floor);
// they must pass the authority's semantic plausibility rules.
const HEADER_NUMBER: u64 = 361_552_001;
const HEADER_HASH_BYTE: u8 = 0xAB;
const PER_L2_TX_WEI: u64 = 1_400_000_000_000;
const PER_L1_CALLDATA_UNIT_WEI: u64 = 189_100_000; // l1 estimate × 61
const PER_STORAGE_ALLOC_WEI: u64 = 2_000_000_000;
const PER_ARB_GAS_BASE_WEI: u64 = 20_000_000;
const PER_ARB_GAS_CONGESTION_WEI: u64 = 3_000_000;
const PER_ARB_GAS_TOTAL_WEI: u64 = PER_ARB_GAS_BASE_WEI + PER_ARB_GAS_CONGESTION_WEI;
const PER_L2_TX_ARB_GAS: u64 = 70_000;
const TOKEN_GAS_UNITS_LIVE: u64 = 61; // deliberately NOT 16 so a hardcode is caught
const PER_STORAGE_ALLOC_ARB_GAS: u64 = 100_000;
const MINIMUM_L2_GAS_PRICE_WEI: u64 = 20_000_000;
const L1_BASE_FEE_ESTIMATE_WEI: u64 = 3_100_000;

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn words(vals: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 32);
    for v in vals {
        let mut w = [0u8; 32];
        w[24..32].copy_from_slice(&v.to_be_bytes());
        out.extend_from_slice(&w);
    }
    out
}

/// ABI-encode a Multicall3 `aggregate3` response: `(bool success, bytes returnData)[]`, all
/// successful, one entry per component payload.
fn encode_aggregate3_response(payloads: &[Vec<u8>]) -> Vec<u8> {
    let n = payloads.len();
    let mut out = Vec::new();
    // offset to array + array length
    let mut w = [0u8; 32];
    w[31] = 0x20;
    out.extend_from_slice(&w);
    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&(n as u64).to_be_bytes());
    out.extend_from_slice(&w);
    // per-tuple head offsets, relative to the start of the array tail (after array_len)
    let mut rel = 32 * n;
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(n);
    for p in payloads {
        let mut w = [0u8; 32];
        w[24..32].copy_from_slice(&(rel as u64).to_be_bytes());
        out.extend_from_slice(&w);
        // tuple body: success, offset-to-bytes (0x40), len, padded data
        let mut body = Vec::new();
        let mut sw = [0u8; 32];
        sw[31] = 1; // success = true
        body.extend_from_slice(&sw);
        let mut ow = [0u8; 32];
        ow[31] = 0x40;
        body.extend_from_slice(&ow);
        let mut lw = [0u8; 32];
        lw[24..32].copy_from_slice(&(p.len() as u64).to_be_bytes());
        body.extend_from_slice(&lw);
        body.extend_from_slice(p);
        let pad = (32 - (p.len() % 32)) % 32;
        body.extend_from_slice(&vec![0u8; pad]);
        rel += body.len();
        bodies.push(body);
    }
    for b in bodies {
        out.extend_from_slice(&b);
    }
    out
}

/// The canned per-component return payloads, in `build_authority_calls` order.
fn authority_payloads() -> Vec<Vec<u8>> {
    vec![
        words(&[
            PER_L2_TX_WEI,
            PER_L1_CALLDATA_UNIT_WEI,
            PER_STORAGE_ALLOC_WEI,
            PER_ARB_GAS_BASE_WEI,
            PER_ARB_GAS_CONGESTION_WEI,
            PER_ARB_GAS_TOTAL_WEI,
        ]),
        words(&[PER_L2_TX_ARB_GAS, TOKEN_GAS_UNITS_LIVE, PER_STORAGE_ALLOC_ARB_GAS]),
        words(&[MINIMUM_L2_GAS_PRICE_WEI]),
        words(&[L1_BASE_FEE_ESTIMATE_WEI]),
        words(&[HEADER_NUMBER]),
    ]
}

/// Loopback JSON-RPC server: serves ONE good header + ONE good aggregate3 (→ exactly one
/// complete publication), then answers EVERY request with a JSON-RPC error (persistent
/// outage). Loopback only; `Connection: close` per request.
fn spawn_outage_server(
    shutdown: Arc<AtomicBool>,
) -> (String, std::thread::JoinHandle<()>, Arc<AtomicU32>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().unwrap().port();
    let aggregates_served = Arc::new(AtomicU32::new(0));
    let served = aggregates_served.clone();
    let handle = std::thread::spawn(move || {
        while !shutdown.load(Ordering::Acquire) {
            let (mut sock, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("loopback accept: {e}"),
            };
            sock.set_nonblocking(false).ok();
            // Read one HTTP request (headers + Content-Length body).
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body_start = loop {
                let n = match sock.read(&mut tmp) {
                    Ok(0) => break usize::MAX,
                    Ok(n) => n,
                    Err(_) => break usize::MAX,
                };
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            if body_start == usize::MAX {
                continue;
            }
            let headers = String::from_utf8_lossy(&buf[..body_start]).to_string();
            let clen: usize = headers
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            while buf.len() < body_start + clen {
                match sock.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
            let body: serde_json::Value =
                match serde_json::from_slice(&buf[body_start..body_start + clen]) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
            let id = body.get("id").cloned().unwrap_or(serde_json::json!(0));
            let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let outage = served.load(Ordering::Acquire) >= 1;
            let reply = if outage {
                // Persistent outage: EVERY request fails after the one good publication.
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32000,\
                     \"message\":\"injected outage\"}}}}"
                )
            } else {
                match method {
                    "eth_getBlockByNumber" => {
                        let h = hex(&[HEADER_HASH_BYTE; 32]);
                        format!(
                            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\
                             \"number\":\"{:#x}\",\"hash\":\"{h}\"}}}}",
                            HEADER_NUMBER
                        )
                    }
                    "eth_call" => {
                        served.fetch_add(1, Ordering::AcqRel);
                        let resp = hex(&encode_aggregate3_response(&authority_payloads()));
                        format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":\"{resp}\"}}")
                    }
                    _ => format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32601,\
                         \"message\":\"unexpected method\"}}}}"
                    ),
                }
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), handle, aggregates_served)
}

fn wait_until(deadline: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// End-to-end over the REAL production writer topology: `spawn_gas_refresher` (the sole gas
/// writer after correction 1) sharing an `Arc<GasOracle>` with the test's reader (standing
/// where the pipeline's submit gate reads). One good publication, then a persistent outage:
///
/// * the legacy snapshot's VALUE, `captured_at_ms`, the authority generation AND the
///   volatility sample stay byte-identical across persistently-failing refreshes (only
///   health evidence advances), and
/// * the submit-admission check (`load_fresh()` + `l1 > 0` — exactly what the pipeline's
///   stale-gas guard evaluates) DECLINES once the snapshot ages past `gas_max_age_ms`, and
///   the typed hard TTL declines via `usable_authority`.
#[test]
fn sole_writer_refresher_failure_keeps_legacy_and_declines_admission() {
    // Money firewall: never resolve a real endpoint from the environment.
    std::env::remove_var("ARBITRUM_RPC_URL_HOT");
    std::env::remove_var("ARBITRUM_RPC_URL");
    std::env::remove_var("GAS_FEE_AGGREGATOR");

    let server_shutdown = Arc::new(AtomicBool::new(false));
    let (endpoint, server, aggregates_served) = spawn_outage_server(server_shutdown.clone());

    const GAS_MAX_AGE_MS: u64 = 300;
    let oracle = Arc::new(GasOracle::new(GasSnapshot::default(), GAS_MAX_AGE_MS));
    let refresher_shutdown = Arc::new(AtomicBool::new(false));
    let refresher = engine::gas_refresher::spawn_gas_refresher(
        endpoint,
        oracle.clone(),
        refresher_shutdown.clone(),
    )
    .expect("spawn gas refresher");

    // Phase 1 — exactly one complete publication seeds the oracle through the REAL wire
    // (HTTP → EIP-1898 eth_call → ABI decode → authority → legacy bridge).
    wait_until(Duration::from_secs(10), "first complete authority publication", || {
        oracle.authority().is_some()
    });
    let seeded_authority = oracle.authority().expect("seeded");
    let seeded_legacy = oracle.load();
    assert_eq!(seeded_authority.canonical_block_number, HEADER_NUMBER);
    assert_eq!(seeded_authority.canonical_block_hash, [HEADER_HASH_BYTE; 32]);
    assert_eq!(
        seeded_legacy.l1_base_fee_per_gas_wei,
        L1_BASE_FEE_ESTIMATE_WEI as u128,
        "legacy bridge must carry the wire-decoded L1 estimate"
    );
    assert_eq!(
        seeded_legacy.l1_gas_per_data_byte,
        TOKEN_GAS_UNITS_LIVE as u32,
        "legacy bridge must carry the LIVE tokenGasUnits (not a hardcoded 16)"
    );
    assert_eq!(seeded_legacy.l2_gas_price_wei, PER_ARB_GAS_TOTAL_WEI as u128);
    assert!(seeded_legacy.captured_at_ms > 0, "seed must be stamped fresh");
    let seeded_sigma = oracle.l1_sigma_bps();
    let health_seed = oracle.authority_health().health_generation;

    // Phase 2 — persistent outage: at least two further attempts fail (health advances).
    wait_until(
        Duration::from_secs(30),
        "two failed refresh attempts after the outage began",
        || oracle.authority_health().health_generation.get() >= health_seed.get() + 2,
    );
    assert_eq!(
        aggregates_served.load(Ordering::Acquire),
        1,
        "exactly one aggregate may ever have succeeded"
    );

    // The authority pointer, generation and ORIGINAL capture time are untouched.
    let after_authority = oracle.authority().expect("still seeded");
    assert!(
        Arc::ptr_eq(&seeded_authority, &after_authority),
        "failed refreshes must leave the authority snapshot pointer-identical"
    );
    assert_eq!(after_authority.generation, seeded_authority.generation);
    assert_eq!(
        after_authority.captured_at_mono_ns,
        seeded_authority.captured_at_mono_ns
    );
    // The legacy snapshot's value AND freshness are byte-identical — no writer renewed it.
    let after_legacy = oracle.load();
    assert_eq!(after_legacy.l1_base_fee_per_gas_wei, seeded_legacy.l1_base_fee_per_gas_wei);
    assert_eq!(after_legacy.l2_gas_price_wei, seeded_legacy.l2_gas_price_wei);
    assert_eq!(after_legacy.l1_gas_per_data_byte, seeded_legacy.l1_gas_per_data_byte);
    assert_eq!(
        after_legacy.captured_at_ms, seeded_legacy.captured_at_ms,
        "a failing authority stream must never renew legacy freshness (FL-14 system scope)"
    );
    assert_eq!(
        oracle.l1_sigma_bps(),
        seeded_sigma,
        "failed refreshes must not add volatility samples"
    );

    // Phase 3 — submit admission declines. This is the EXACT expression the pipeline's
    // stale-gas broadcast guard evaluates before any wire write.
    wait_until(Duration::from_secs(10), "legacy snapshot aging past gas_max_age_ms", || {
        engine_gas::now_ms().saturating_sub(seeded_legacy.captured_at_ms) > GAS_MAX_AGE_MS
    });
    assert!(
        oracle
            .load_fresh()
            .ok()
            .filter(|s| s.l1_base_fee_per_gas_wei > 0)
            .is_none(),
        "submit admission must DECLINE once the sole writer's stream fails (stale snapshot)"
    );
    // And the typed hard TTL declines against the ORIGINAL capture time.
    const TTL_NS: u64 = 1_000_000; // 1ms — long past by now
    assert!(
        oracle
            .usable_authority(seeded_authority.captured_at_mono_ns + TTL_NS + 1, TTL_NS)
            .is_err(),
        "typed authority admission must decline past its original hard TTL"
    );
    assert!(
        oracle
            .usable_authority(seeded_authority.captured_at_mono_ns + TTL_NS - 1, TTL_NS)
            .is_ok(),
        "typed authority admission within the original TTL stays usable"
    );

    refresher_shutdown.store(true, Ordering::Release);
    refresher.join().expect("refresher joins");
    server_shutdown.store(true, Ordering::Release);
    server.join().expect("server joins");
}
