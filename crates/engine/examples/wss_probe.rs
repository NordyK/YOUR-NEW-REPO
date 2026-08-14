//! Frugal WSS endpoint probe (2026-07-10, QuickNode integration): connect, subscribe
//! `newHeads`, receive N frames with arrival-gap timing, unsubscribe, close. Reads the
//! endpoint from `WSS_PROBE_URL` (never hardcode keyed URLs). ~5 credits per run.
//!
//! Run: WSS_PROBE_URL=wss://… cargo run --release -p engine --example wss_probe [frames]

use std::time::Instant;

fn main() {
    // rustls dual-provider trap: the workspace enables both ring and aws-lc-rs via deps;
    // pick aws-lc-rs explicitly (same idiom as main.rs / WsRelayClient).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let url = std::env::var("WSS_PROBE_URL").expect("set WSS_PROBE_URL");
    let frames: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let t0 = Instant::now();
    let (mut ws, resp) = tungstenite::connect(&url).expect("wss connect");
    println!(
        "connected in {:.0}ms (http {})",
        t0.elapsed().as_secs_f64() * 1000.0,
        resp.status()
    );
    ws.send(tungstenite::Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}"#.into(),
    ))
    .expect("subscribe send");
    let sub_id = loop {
        let msg = ws.read().expect("subscribe reply");
        if let tungstenite::Message::Text(t) = msg {
            let v: serde_json::Value = serde_json::from_str(&t).expect("json");
            if let Some(id) = v.get("result").and_then(|r| r.as_str()) {
                println!("subscribed: {}…", &id[..id.len().min(14)]);
                break id.to_string();
            }
        }
    };
    let mut last = Instant::now();
    let mut got = 0usize;
    while got < frames {
        let msg = ws.read().expect("head frame");
        if let tungstenite::Message::Text(t) = msg {
            let v: serde_json::Value = serde_json::from_str(&t).expect("json");
            if let Some(num) = v
                .pointer("/params/result/number")
                .and_then(|n| n.as_str())
                .and_then(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).ok())
            {
                println!(
                    "  newHead block={num} (+{:.0}ms since previous)",
                    last.elapsed().as_secs_f64() * 1000.0
                );
                last = Instant::now();
                got += 1;
            }
        }
    }
    let _ = ws.send(tungstenite::Message::Text(format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"eth_unsubscribe","params":["{sub_id}"]}}"#
    )));
    println!("unsubscribed; done");
}
