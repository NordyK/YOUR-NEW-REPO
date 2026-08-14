#![allow(clippy::type_complexity)] // test helper tuple types; cosmetic
//! End-to-end proof of the S2 binary fast path over a REAL Unix socket on this machine:
//! encode `FeedFrame`s (as the sidecar does) → write to a UDS → read with the exact length-framed
//! `read_exact(header)` + `read_exact(body)` protocol the engine's `rx_loop_unix_binary` uses →
//! `decode_frame` → assert the decoded fields match. This is the transport + framing + codec the
//! whole optimization rests on; if it round-trips here, it round-trips in the engine.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::Instant;

use engine_feed::{
    decode_frame, decode_l2msg_recursive, encode_frame, encode_frame_v2, encode_frame_v3,
    iter_envelopes, TransactionInfo, FEED_FRAME_HEADER_LEN, FEED_FRAME_HEADER_LEN_V2,
    FEED_FRAME_HEADER_LEN_V3, FEED_FRAME_MAGIC_V2, FEED_FRAME_MAGIC_V3,
};

#[test]
fn binary_frames_roundtrip_over_real_uds() {
    let sock = format!("/tmp/mantis-feedtest-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&sock);

    // Three distinct signed-txs spanning the field variations.
    let fixtures: Vec<(u64, Option<[u8; 20]>, [u8; 4], Vec<u8>)> = vec![
        (
            1001,
            Some([0xaa; 20]),
            [0x38, 0xed, 0x17, 0x39],
            vec![1, 2, 3, 4, 5],
        ),
        (1002, Some([0xbb; 20]), [0x12, 0x34, 0x56, 0x78], vec![]),
        (1003, None, [0x00, 0x00, 0x00, 0x00], vec![9; 300]), // >header, contract-create, no `to`
    ];

    // --- emitter (acts as the sidecar / forked relay) ---
    let listener = UnixListener::bind(&sock).expect("bind");
    let emit = fixtures.clone();
    let writer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = Vec::new();
        for (seq, to, sel, data) in &emit {
            encode_frame(&mut buf, *seq, 7_000 + *seq, *to, *sel, data);
            stream.write_all(&buf).expect("write frame");
        }
        stream.flush().ok();
    });

    // --- consumer (the exact rx_loop_unix_binary read protocol) ---
    let mut stream = UnixStream::connect(&sock).expect("connect");
    let mut header = [0u8; FEED_FRAME_HEADER_LEN];
    let mut got = Vec::new();
    for _ in 0..fixtures.len() {
        stream.read_exact(&mut header).expect("read header");
        let data_len =
            u32::from_le_bytes([header[45], header[46], header[47], header[48]]) as usize;
        let mut body = vec![0u8; data_len];
        stream.read_exact(&mut body).expect("read body");
        let mut frame = Vec::with_capacity(FEED_FRAME_HEADER_LEN + data_len);
        frame.extend_from_slice(&header);
        frame.extend_from_slice(&body);
        let v = decode_frame(&frame).expect("decode");
        got.push((v.seq, v.to, v.selector, v.data.to_vec()));
    }
    writer.join().ok();
    let _ = std::fs::remove_file(&sock);

    assert_eq!(got.len(), 3);
    for (i, (seq, to, sel, data)) in fixtures.iter().enumerate() {
        assert_eq!(got[i].0, *seq, "seq mismatch frame {i}");
        assert_eq!(got[i].1, *to, "to mismatch frame {i}");
        assert_eq!(got[i].2, *sel, "selector mismatch frame {i}");
        assert_eq!(&got[i].3, data, "data mismatch frame {i}");
    }
}

/// V2 round-trip: sidecar emits V2 frames carrying the raw signed-tx envelope; consumer reads
/// using the exact two-stage protocol the engine's `rx_loop_unix_binary` uses (V1-sized header
/// first → check magic → read extra envelope_len word + envelope bytes → reassemble + decode).
/// This is the wire-level proof the binary path now feeds shadow REVM end-to-end.
#[test]
fn binary_frames_v2_carry_envelope_roundtrip() {
    let sock = format!("/tmp/mantis-feedtest-v2-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&sock);

    // (seq, to, selector, calldata_body, envelope) — three with envelopes, one without (env_len=0).
    let fixtures: Vec<(u64, Option<[u8; 20]>, [u8; 4], Vec<u8>, Option<Vec<u8>>)> = vec![
        (
            2001,
            Some([0xaa; 20]),
            [0x07, 0xed, 0x23, 0x79],
            vec![0xaa, 0xbb],
            Some(vec![0x02, 0xf8, 0x6b, 0x82, 0xa4, 0xb1, 0x01, 0x02, 0x03]),
        ),
        (
            2002,
            Some([0xbb; 20]),
            [0x83, 0x80, 0x0a, 0x8e],
            vec![],
            Some(vec![0x02, 0xf8, 0x70, 0x42]),
        ),
        (2003, None, [0; 4], vec![9; 500], Some(vec![0xff; 250])), // big body + big envelope (exercises buffer growth)
        (
            2004,
            Some([0xcc; 20]),
            [0xa8, 0xc9, 0xed, 0x67],
            vec![1, 2, 3],
            None,
        ), // V2 but env_len=0
    ];

    // --- emitter (acts as the V2 sidecar) ---
    let listener = UnixListener::bind(&sock).expect("bind v2");
    let emit = fixtures.clone();
    let writer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept v2");
        let mut buf = Vec::new();
        for (seq, to, sel, data, env) in &emit {
            encode_frame_v2(
                &mut buf,
                *seq,
                7_000 + *seq,
                *to,
                *sel,
                data,
                env.as_deref(),
            );
            stream.write_all(&buf).expect("write v2 frame");
        }
        stream.flush().ok();
    });

    // --- consumer (mirrors rx_loop_unix_binary: V1-prefix read + V2 dispatch) ---
    let mut stream = UnixStream::connect(&sock).expect("connect v2");
    let mut header_v1 = [0u8; FEED_FRAME_HEADER_LEN];
    let mut env_word = [0u8; 4];
    let v2_magic_le = FEED_FRAME_MAGIC_V2.to_le_bytes();
    let mut got = Vec::new();
    for _ in 0..fixtures.len() {
        stream.read_exact(&mut header_v1).expect("read v1 header");
        let data_len =
            u32::from_le_bytes([header_v1[45], header_v1[46], header_v1[47], header_v1[48]])
                as usize;
        let is_v2 = header_v1[0..4] == v2_magic_le;
        let env_len = if is_v2 {
            stream.read_exact(&mut env_word).expect("read env_len");
            u32::from_le_bytes(env_word) as usize
        } else {
            0
        };
        let total = data_len + env_len;
        let mut body = vec![0u8; total];
        stream.read_exact(&mut body).expect("read body+env");
        let header_len = if is_v2 {
            FEED_FRAME_HEADER_LEN_V2
        } else {
            FEED_FRAME_HEADER_LEN
        };
        let mut frame = Vec::with_capacity(header_len + total);
        frame.extend_from_slice(&header_v1);
        if is_v2 {
            frame.extend_from_slice(&env_word);
        }
        frame.extend_from_slice(&body);
        let v = decode_frame(&frame).expect("decode v2");
        got.push((
            v.seq,
            v.to,
            v.selector,
            v.data.to_vec(),
            v.envelope.map(|e| e.to_vec()),
        ));
    }
    writer.join().ok();
    let _ = std::fs::remove_file(&sock);

    assert_eq!(got.len(), 4);
    for (i, (seq, to, sel, data, env)) in fixtures.iter().enumerate() {
        assert_eq!(got[i].0, *seq, "seq mismatch frame {i}");
        assert_eq!(got[i].1, *to, "to mismatch frame {i}");
        assert_eq!(got[i].2, *sel, "selector mismatch frame {i}");
        assert_eq!(&got[i].3, data, "data mismatch frame {i}");
        assert_eq!(&got[i].4, env, "envelope mismatch frame {i}");
    }
}

/// V3 round-trip: sidecar emits the Gate-8 freshness frame shape (V2 + header_timestamp).
/// This mirrors the engine receiver's prefix-read dispatch and proves V3 frames do not desync
/// the binary fast path before `decode_frame` sees them.
#[test]
fn binary_frames_v3_carry_header_timestamp_roundtrip() {
    let sock = format!("/tmp/mantis-feedtest-v3-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&sock);

    let fixtures: Vec<(
        u64,
        u64,
        Option<[u8; 20]>,
        [u8; 4],
        Vec<u8>,
        Option<Vec<u8>>,
    )> = vec![
        (
            3001,
            1_781_581_234,
            Some([0xaa; 20]),
            [0x07, 0xed, 0x23, 0x79],
            vec![0xaa, 0xbb, 0xcc],
            Some(vec![0x02, 0xf8, 0x6b, 0x82, 0xa4, 0xb1]),
        ),
        (3002, 1_781_581_235, None, [0; 4], vec![9; 320], None),
    ];

    let listener = UnixListener::bind(&sock).expect("bind v3");
    let emit = fixtures.clone();
    let writer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept v3");
        let mut buf = Vec::new();
        for (seq, header_ts, to, sel, data, env) in &emit {
            encode_frame_v3(
                &mut buf,
                *seq,
                7_000 + *seq,
                *header_ts,
                *to,
                *sel,
                data,
                env.as_deref(),
            );
            stream.write_all(&buf).expect("write v3 frame");
        }
        stream.flush().ok();
    });

    let mut stream = UnixStream::connect(&sock).expect("connect v3");
    let mut header_v1 = [0u8; FEED_FRAME_HEADER_LEN];
    let mut env_word = [0u8; 4];
    let mut header_ts_word = [0u8; 8];
    let v2_magic_le = FEED_FRAME_MAGIC_V2.to_le_bytes();
    let v3_magic_le = FEED_FRAME_MAGIC_V3.to_le_bytes();
    let mut got = Vec::new();
    for _ in 0..fixtures.len() {
        stream.read_exact(&mut header_v1).expect("read v1 header");
        let data_len =
            u32::from_le_bytes([header_v1[45], header_v1[46], header_v1[47], header_v1[48]])
                as usize;
        let is_v3 = header_v1[0..4] == v3_magic_le;
        let is_v2 = header_v1[0..4] == v2_magic_le;
        let env_len = if is_v2 || is_v3 {
            stream.read_exact(&mut env_word).expect("read env_len");
            u32::from_le_bytes(env_word) as usize
        } else {
            0
        };
        if is_v3 {
            stream
                .read_exact(&mut header_ts_word)
                .expect("read header_timestamp");
        }
        let total = data_len + env_len;
        let mut body = vec![0u8; total];
        stream.read_exact(&mut body).expect("read body+env");
        let header_len = if is_v3 {
            FEED_FRAME_HEADER_LEN_V3
        } else if is_v2 {
            FEED_FRAME_HEADER_LEN_V2
        } else {
            FEED_FRAME_HEADER_LEN
        };
        let mut frame = Vec::with_capacity(header_len + total);
        frame.extend_from_slice(&header_v1);
        if is_v2 || is_v3 {
            frame.extend_from_slice(&env_word);
        }
        if is_v3 {
            frame.extend_from_slice(&header_ts_word);
        }
        frame.extend_from_slice(&body);
        let v = decode_frame(&frame).expect("decode v3");
        got.push((
            v.seq,
            v.header_timestamp,
            v.to,
            v.selector,
            v.data.to_vec(),
            v.envelope.map(|e| e.to_vec()),
        ));
    }
    writer.join().ok();
    let _ = std::fs::remove_file(&sock);

    assert_eq!(got.len(), 2);
    for (i, (seq, header_ts, to, sel, data, env)) in fixtures.iter().enumerate() {
        assert_eq!(got[i].0, *seq, "seq mismatch frame {i}");
        assert_eq!(got[i].1, *header_ts, "header timestamp mismatch frame {i}");
        assert_eq!(got[i].2, *to, "to mismatch frame {i}");
        assert_eq!(got[i].3, *sel, "selector mismatch frame {i}");
        assert_eq!(&got[i].4, data, "data mismatch frame {i}");
        assert_eq!(&got[i].5, env, "envelope mismatch frame {i}");
    }
}

/// Cross-version compatibility: a V1 sidecar (legacy `encode_frame`) emits a frame, a V2-aware
/// engine reads it and treats `envelope = None` — no shadow tee, but the hot path keeps working.
/// This is the safe-restart property: deploy V2 engine first, then sidecar; the engine still
/// runs against the old sidecar.
#[test]
fn binary_frames_v1_sidecar_works_with_v2_engine() {
    let sock = format!("/tmp/mantis-feedtest-mixed-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&sock);

    let listener = UnixListener::bind(&sock).expect("bind mixed");
    let writer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept mixed");
        let mut buf = Vec::new();
        encode_frame(
            &mut buf,
            3001,
            1_700_000_000,
            Some([0xab; 20]),
            [0xde, 0xad, 0xbe, 0xef],
            &[0x55; 16],
        );
        stream.write_all(&buf).expect("write v1 frame");
        stream.flush().ok();
    });

    let mut stream = UnixStream::connect(&sock).expect("connect mixed");
    let mut header_v1 = [0u8; FEED_FRAME_HEADER_LEN];
    stream.read_exact(&mut header_v1).expect("read v1 header");
    let data_len =
        u32::from_le_bytes([header_v1[45], header_v1[46], header_v1[47], header_v1[48]]) as usize;
    // V1 magic ⇒ V2-aware reader skips the extra envelope_len read.
    let v2_magic_le = FEED_FRAME_MAGIC_V2.to_le_bytes();
    assert_ne!(header_v1[0..4], v2_magic_le, "this fixture is V1");
    let mut body = vec![0u8; data_len];
    stream.read_exact(&mut body).expect("read body");
    let mut frame = Vec::with_capacity(FEED_FRAME_HEADER_LEN + data_len);
    frame.extend_from_slice(&header_v1);
    frame.extend_from_slice(&body);
    let v = decode_frame(&frame).expect("decode v1");
    assert_eq!(v.seq, 3001);
    assert_eq!(v.selector, [0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(v.data, &[0x55; 16]);
    assert!(
        v.envelope.is_none(),
        "V1 frame → no envelope, no shadow tee"
    );

    writer.join().ok();
    let _ = std::fs::remove_file(&sock);
}

/// A/B: bot-side per-frame cost of the JSON-WS path (envelope scan + base64 + RLP walk) vs the
/// binary path (`decode_frame`). The binary path eliminates all three, so it must be faster — this
/// is the rationale behind the report's "promote the forked relay if it saves ≥10µs p95" gate.
/// (Absolute numbers are dev-laptop; the AWS gate replays identical captured frames through both.)
#[test]
fn binary_decode_beats_json_parse() {
    // A minimal but structurally-real feed frame: kind=4 SignedTx, base64-encoded l2Msg in JSON.
    let inner = {
        let mut v = vec![0x04u8]; // kind=4 SignedTx
        v.push(0xc9);
        v.extend_from_slice(&[0x80u8; 9]);
        v
    };
    let b64 = base64_simd::STANDARD.encode_to_string(&inner);
    let json = format!(
        r#"{{"version":1,"messages":[{{"sequenceNumber":12345,"message":{{"message":{{"header":{{"kind":3,"sender":"0x","blockNumber":22300000,"timestamp":1715000000}},"l2Msg":"{b64}"}}}}}}]}}"#
    ).into_bytes();

    // Equivalent binary frame (what the sidecar would emit for this tx).
    let mut binframe = Vec::new();
    encode_frame(
        &mut binframe,
        12345,
        1715000000,
        Some([0u8; 20]),
        [0u8; 4],
        &[],
    );

    const ITERS: u32 = 100_000;
    // JSON path: scan envelopes → base64 decode → RLP-extract each tx (full bot-side work).
    let mut b64buf = Vec::with_capacity(256);
    let mut sink_json = 0u64;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        for env in iter_envelopes(&json) {
            b64buf.clear();
            b64buf.extend_from_slice(env.l2msg_b64);
            if let Ok(d) = base64_simd::forgiving_decode_inplace(&mut b64buf) {
                let n = d.len();
                decode_l2msg_recursive(&b64buf[..n], |tx: TransactionInfo<'_>| {
                    sink_json = sink_json.wrapping_add(tx.data.len() as u64 + 1);
                });
            }
        }
    }
    let json_ns = t0.elapsed().as_nanos() / ITERS as u128;

    // Binary path: single decode_frame, no JSON/base64/RLP.
    let mut sink_bin = 0u64;
    let t1 = Instant::now();
    for _ in 0..ITERS {
        if let Some(v) = decode_frame(&binframe) {
            sink_bin = sink_bin.wrapping_add(v.data.len() as u64 + v.seq);
        }
    }
    let bin_ns = t1.elapsed().as_nanos() / ITERS as u128;

    println!("S2 per-frame bot cost — JSON-parse: {json_ns} ns  binary-decode: {bin_ns} ns  (sinks {sink_json}/{sink_bin})");
    assert!(
        bin_ns < json_ns,
        "binary decode ({bin_ns} ns) must be faster than JSON parse ({json_ns} ns)"
    );
}
