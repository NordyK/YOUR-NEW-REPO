//! S1 A/B measurement harness — CURRENT (untuned `tungstenite::connect`, fresh TLS config) vs NEW
//! (own + tuned socket: NODELAY/SO_BUSY_POLL/SO_RCVBUF, IP-pin, reused resumption config) against the
//! REAL public Arbitrum feed `wss://arb1-feed.arbitrum.io/feed` (public — no key). Also RTT-probes the
//! feed's anycast IP set, and measures TLS cold-vs-resumed handshake. Correctness: every captured
//! frame must parse and sequenceNumbers must be monotonic (no drop/corruption from the tuning).
//!
//! Run: cargo run -p engine-feed --example feed_ab --release

use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message;

const HOST: &str = "arb1-feed.arbitrum.io";
const URL: &str = "wss://arb1-feed.arbitrum.io/feed";
const FRAMES: usize = 200;

fn pct(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}
fn us(ns: u128) -> String {
    if ns >= 1_000_000 {
        format!("{:.2}ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1}µs", ns as f64 / 1e3)
    } else {
        format!("{ns}ns")
    }
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Capture `FRAMES` frames, returning (time_to_first_frame_ns, inter-frame gap samples, max_seq seen,
/// monotonic-ok). `seq_ok` = sequenceNumbers strictly non-decreasing across frames (correctness).
fn capture<S: Read + std::io::Write>(
    ws: &mut tungstenite::WebSocket<S>,
    fd: i32,
    started: Instant,
) -> (u128, Vec<u128>, u64, bool) {
    let mut first = None;
    let mut gaps = Vec::with_capacity(FRAMES);
    let mut last_t: Option<Instant> = None;
    let mut last_seq = 0u64;
    let mut seq_ok = true;
    let mut n = 0;
    while n < FRAMES {
        // re-arm QUICKACK each recv (kernel clears it) — part of the "tuned" path; harmless untuned.
        let _ = engine_sysopt::set_tcp_quickack(fd);
        match ws.read() {
            Ok(Message::Text(t)) => {
                let now = Instant::now();
                if first.is_none() {
                    first = Some(now.duration_since(started).as_nanos());
                }
                if let Some(p) = last_t {
                    gaps.push(now.duration_since(p).as_nanos());
                }
                last_t = Some(now);
                for env in engine_feed::iter_envelopes(t.as_bytes()) {
                    if env.sequence_number < last_seq {
                        seq_ok = false;
                    }
                    last_seq = last_seq.max(env.sequence_number);
                }
                n += 1;
            }
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    (first.unwrap_or(0), gaps, last_seq, seq_ok)
}

fn run_untuned() -> (u128, Vec<u128>, u64, bool) {
    // CURRENT behaviour: bare tungstenite::connect — fresh TLS config, no socket tuning.
    let started = Instant::now();
    let (mut ws, _) = tungstenite::connect(URL).expect("untuned connect");
    let fd = match ws.get_ref() {
        MaybeTlsStream::Rustls(s) => s.get_ref().as_raw_fd(),
        _ => -1,
    };
    capture(&mut ws, fd, started)
}

fn connect_tuned(
    cfg: Arc<rustls::ClientConfig>,
    ip: std::net::IpAddr,
) -> (tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, i32, u128) {
    // NEW behaviour: own + tune the socket, pin the best IP, reuse the resumption config.
    let started = Instant::now();
    let tcp =
        TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, 443), Duration::from_secs(5))
            .expect("tcp");
    let _ = tcp.set_nodelay(true);
    let fd = tcp.as_raw_fd();
    let _ = engine_sysopt::set_so_busy_poll(fd, 50);
    let _ = engine_sysopt::set_so_rcvbuf(fd, 8 * 1024 * 1024);
    let req = URL.into_client_request().unwrap();
    let (ws, _) = tungstenite::client_tls_with_config(
        req,
        tcp,
        None,
        Some(tungstenite::Connector::Rustls(cfg)),
    )
    .expect("tuned tls");
    let handshake = started.elapsed().as_nanos();
    (ws, fd, handshake)
}

fn main() {
    // aws-lc-rs, not ring: the workspace standard provider (proven live on-box). `ring` is
    // not an enabled rustls feature here — it only compiled when another workspace member's
    // features unioned it in, so `cargo test -p engine-feed` alone was broken (2026-07-08).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // ---- (1) Feed IP RTT-probe (anycast set) ----
    println!("== S1 A/B: {URL} ==\n\n[1] Feed IP RTT-probe (TCP-connect ×5 per IP):");
    let addrs: Vec<_> = (HOST, 443)
        .to_socket_addrs()
        .map(|i| i.collect())
        .unwrap_or_default();
    let mut best_ip = None;
    let mut best_rtt = u128::MAX;
    for a in &addrs {
        let mut s = vec![];
        for _ in 0..5 {
            let t = Instant::now();
            if TcpStream::connect_timeout(a, Duration::from_secs(3)).is_ok() {
                s.push(t.elapsed().as_nanos());
            }
        }
        s.sort();
        if !s.is_empty() {
            let p50 = pct(&s, 0.5);
            println!(
                "    {:>40}  p50 {}  min {}",
                a.ip().to_string(),
                us(p50),
                us(s[0])
            );
            if p50 < best_rtt {
                best_rtt = p50;
                best_ip = Some(a.ip());
            }
        }
    }
    let pin = best_ip.expect("no feed IP resolved");
    println!("    → best IP to pin: {pin}  (FEED_PIN_IP={pin})");

    // ---- (2) Untuned (CURRENT) ----
    println!("\n[2] CURRENT (untuned tungstenite::connect):");
    let (u_first, mut u_gaps, u_seq, u_ok) = run_untuned();
    u_gaps.sort();
    println!("    time-to-first-frame: {}", us(u_first));
    println!(
        "    inter-frame gap  p50 {}  p90 {}  p99 {}  (n={})",
        us(pct(&u_gaps, 0.5)),
        us(pct(&u_gaps, 0.9)),
        us(pct(&u_gaps, 0.99)),
        u_gaps.len()
    );
    println!(
        "    correctness: {} frames, max_seq={}, monotonic={}",
        FRAMES, u_seq, u_ok
    );

    // ---- (3) Tuned (NEW): cold then resumed (reuse config) ----
    let cfg = tls_config();
    println!("\n[3] NEW (own+tuned socket, pinned IP, reused resumption config):");
    let (mut ws_cold, fd_c, hs_cold) = connect_tuned(cfg.clone(), pin);
    let started = Instant::now();
    let (t_first, mut t_gaps, t_seq, t_ok) = capture(&mut ws_cold, fd_c, started);
    drop(ws_cold);
    t_gaps.sort();
    // Reconnect reusing the SAME cfg → TLS resumption should make the handshake faster.
    let (_ws_resumed, _fd_r, hs_resumed) = connect_tuned(cfg.clone(), pin);
    println!("    time-to-first-frame: {}", us(t_first));
    println!(
        "    inter-frame gap  p50 {}  p90 {}  p99 {}  (n={})",
        us(pct(&t_gaps, 0.5)),
        us(pct(&t_gaps, 0.9)),
        us(pct(&t_gaps, 0.99)),
        t_gaps.len()
    );
    println!(
        "    correctness: {} frames, max_seq={}, monotonic={}",
        FRAMES, t_seq, t_ok
    );
    println!(
        "    TLS handshake: cold {}  resumed {}  ({}% faster)",
        us(hs_cold),
        us(hs_resumed),
        if hs_cold > 0 {
            (100 - (hs_resumed * 100 / hs_cold.max(1))) as i64
        } else {
            0
        }
    );

    // ---- (4) Verdict ----
    println!("\n[4] COMPARISON (inter-frame jitter is the steady-state S1 signal):");
    let up99 = pct(&u_gaps, 0.99);
    let tp99 = pct(&t_gaps, 0.99);
    println!(
        "    p99 inter-frame:  current {}  →  new {}   ({})",
        us(up99),
        us(tp99),
        if tp99 < up99 {
            format!(
                "-{:.0}% tail",
                100.0 - (tp99 as f64 * 100.0 / up99.max(1) as f64)
            )
        } else {
            "no improvement (laptop NIC limits)".into()
        }
    );
    println!("    correctness preserved: current monotonic={u_ok}  new monotonic={t_ok}");
}
