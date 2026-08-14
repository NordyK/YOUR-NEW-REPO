//! Harness 1 — mock-sequencer integration test.
//!
//! Glues the entire live-submit wire end-to-end against a localhost mock sequencer:
//!   calldata → EIP-1559 sign (secp256k1) → `eth_sendRawTransaction` envelope → HTTP POST →
//!   response body → `classify_submit_response`.
//! Every primitive is already unit-tested in isolation; this proves they COMPOSE correctly —
//! the path the engine takes the first time it POSTs to a real endpoint. It also recovers the
//! signer from the bytes that actually crossed the wire, proving the envelope carries a
//! correctly-signed tx that resolves to the funded EOA.
//!
//! Plain HTTP (no TLS): `SequencerTlsConnection::new("http://…")` uses a `Conn::Plain` socket,
//! exactly the production "local sequencer-forwarder / test mock" path the code documents.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use engine_submit::{
    build_send_raw_tx_envelope, classify_submit_response, speculative::SpeculativeTx, DeliveryState,
    Eip1559Tx, HotSpare, SequencerTlsConnection, SigningKey, SubmitConn, SubmitError,
    SubmitFailure, SubmitOutcome, MAX_SUBMIT_BODY,
};

/// A fixed, valid secp256k1 test key (`0x11…11` is < the curve order). NOT a real funded key.
const TEST_KEY: [u8; 32] = [0x11; 32];

// ---- tiny HTTP helpers (test-local; mirror the connector's own parsing) ----

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let lower: Vec<u8> = headers.iter().map(|b| b.to_ascii_lowercase()).collect();
    let pos = find(&lower, b"content-length:")?;
    let after = &headers[pos + b"content-length:".len()..];
    let s: String = after
        .iter()
        .take_while(|&&b| b != b'\r')
        .map(|&b| b as char)
        .collect();
    s.trim().parse().ok()
}

/// Read a complete HTTP request: headers, then exactly `Content-Length` body bytes.
fn read_full_http_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(hpos) = find(&buf, b"\r\n\r\n") {
            let header_end = hpos + 4;
            let cl = content_length(&buf[..header_end]).unwrap_or(0);
            if buf.len() >= header_end + cl {
                break;
            }
        }
    }
    buf
}

/// One-shot mock: accept a single connection, capture the request, reply `200 OK` with
/// `response_body`. Returns a handle yielding the captured request bytes.
fn spawn_mock(listener: TcpListener, response_body: &'static [u8]) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("mock accept");
        let req = read_full_http_request(&mut s);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            response_body.len()
        );
        s.write_all(head.as_bytes()).ok();
        s.write_all(response_body).ok();
        s.flush().ok();
        req
    })
}

/// Build + sign a realistic EIP-1559 arb submission with a configurable calldata body.
/// Returns the raw signed RLP bytes ready for `eth_sendRawTransaction`.
fn build_signed_submission_with_calldata_len(
    key: &SigningKey,
    calldata_len: usize,
    gas_limit: u64,
) -> Vec<u8> {
    let mut calldata = vec![0u8; calldata_len];
    calldata[..4].copy_from_slice(&[0xb2, 0x46, 0x0c, 0x48]); // flashLoanArbMixed-style selector
    let tx = Eip1559Tx {
        chain_id: 42_161,
        nonce: 7,
        max_priority_fee_per_gas: U256::ZERO,
        max_fee_per_gas: U256::from(100_000_000u64),
        gas_limit,
        to: [0xAB; 20],
        value: U256::ZERO,
        data: calldata,
    };
    SpeculativeTx::build_and_sign(&tx, key)
        .expect("sign")
        .commit()
        .into_bytes()
}

/// Build + sign a representative `flashLoanArbMixed`-sized calldata body
/// (4-byte selector + 1120 zero bytes = 1124).
fn build_signed_submission(key: &SigningKey) -> Vec<u8> {
    build_signed_submission_with_calldata_len(key, 1124, 1_300_000)
}

/// Two same-typed HotSpare legs: the first revokes authority after its pre-write permit check but
/// returns a definitely-not-written injected transport fault; the second is a real production
/// connector aimed at a loopback mock. The second leg must never write once the epoch changed.
enum AuthorityFaultConn {
    RevokeAfterCheck {
        connected: bool,
        authority: Arc<AtomicBool>,
    },
    Real(SequencerTlsConnection),
}

impl SubmitConn for AuthorityFaultConn {
    fn is_connected(&self) -> bool {
        match self {
            Self::RevokeAfterCheck { connected, .. } => *connected,
            Self::Real(conn) => conn.is_connected(),
        }
    }

    fn ensure_connected(&mut self) -> Result<(), SubmitError> {
        match self {
            Self::RevokeAfterCheck { connected, .. } => {
                *connected = true;
                Ok(())
            }
            Self::Real(conn) => conn.ensure_connected(),
        }
    }

    fn try_post(&mut self, body: &[u8]) -> Result<Vec<u8>, SubmitFailure> {
        match self {
            Self::RevokeAfterCheck { authority, .. } => {
                authority.store(false, Ordering::Release);
                Err(SubmitFailure::not_written(SubmitError::Tls(
                    "fault injected after primary permit check".into(),
                )))
            }
            Self::Real(conn) => conn.try_post(body),
        }
    }
}

#[test]
fn authority_loss_after_precheck_between_retries_sends_zero_bytes_to_mock() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mock = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
                    let mut bytes = Vec::new();
                    let _ = stream.read_to_end(&mut bytes);
                    return bytes;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Vec::new();
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("mock accept: {err}"),
            }
        }
    });

    let authority = Arc::new(AtomicBool::new(true));
    let primary = AuthorityFaultConn::RevokeAfterCheck {
        connected: false,
        authority: authority.clone(),
    };
    let spare = AuthorityFaultConn::Real(
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).unwrap(),
    );
    let mut hot_spare = HotSpare::new(primary, spare);
    assert_eq!(hot_spare.rewarm_idle(), 2);

    let failure = hot_spare
        .submit_authorized(b"signed-body-must-not-escape", &mut || {
            authority.load(Ordering::Acquire)
        })
        .expect_err("revoked retry must fail closed");
    assert!(matches!(failure.err, SubmitError::AuthorityRevoked));
    assert_eq!(failure.delivery, DeliveryState::DefinitelyNotWritten);
    assert_eq!(
        mock.join().expect("mock joins"),
        Vec::<u8>::new(),
        "the real mock sequencer connection received no request bytes"
    );
}

#[test]
fn authority_loss_after_external_prewire_check_blocks_first_real_connector_write() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let mock = thread::spawn(move || {
        // `rewarm_idle` establishes both production connector legs before the attempted submit.
        let mut peers = Vec::with_capacity(2);
        for _ in 0..2 {
            let (stream, _) = listener.accept().expect("accept warm connector");
            peers.push(stream);
        }
        let mut received = Vec::new();
        for mut peer in peers {
            peer.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
            let mut bytes = Vec::new();
            let _ = peer.read_to_end(&mut bytes);
            received.extend_from_slice(&bytes);
        }
        received
    });

    let endpoint = format!("http://127.0.0.1:{port}/rpc");
    let primary = SequencerTlsConnection::new(&endpoint).unwrap();
    let spare = SequencerTlsConnection::new(&endpoint).unwrap();
    let mut hot_spare = HotSpare::new(primary, spare);
    assert_eq!(hot_spare.rewarm_idle(), 2);

    // Model the pipeline race exactly: admission/the historical pre-wire check sees healthy,
    // degradation then linearizes, and the first real connector attempt must consult the captured
    // permit again immediately before its socket write.
    let authority = AtomicBool::new(true);
    assert!(authority.load(Ordering::Acquire), "external pre-wire check admitted");
    authority.store(false, Ordering::Release);
    let failure = hot_spare
        .submit_authorized(b"signed-body-must-not-escape", &mut || {
            authority.load(Ordering::Acquire)
        })
        .expect_err("revoked first production connector write must fail closed");
    assert!(matches!(failure.err, SubmitError::AuthorityRevoked));
    assert_eq!(failure.delivery, DeliveryState::DefinitelyNotWritten);

    drop(hot_spare);
    assert_eq!(
        mock.join().expect("mock joins"),
        Vec::<u8>::new(),
        "neither warmed production connector put any HTTP bytes on the wire"
    );
}

#[test]
fn healthy_authorized_and_observed_paths_are_http_byte_exact_to_ordinary() {
    let key = SigningKey::from_bytes(&TEST_KEY).unwrap();
    let raw = build_signed_submission(&key);
    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 77, &mut body);

    // One stable endpoint keeps the Host header identical across all three production connector
    // entrypoints; the server captures the complete request bytes for exact equality.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let mock = thread::spawn(move || {
        let response = br#"{"jsonrpc":"2.0","id":77,"result":"0xbyteexact"}"#;
        let mut requests = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("accept");
            requests.push(read_full_http_request(&mut stream));
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                response.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(response).unwrap();
            stream.flush().unwrap();
        }
        requests
    });
    let endpoint = format!("http://127.0.0.1:{port}/rpc");

    let mut ordinary = SequencerTlsConnection::new(&endpoint).unwrap();
    ordinary.ensure_connected().unwrap();
    ordinary.try_post(&body).unwrap();

    let mut authorized = SequencerTlsConnection::new(&endpoint).unwrap();
    authorized.ensure_connected().unwrap();
    authorized
        .try_post_authorized(&body, &mut || true)
        .unwrap();

    let mut observed_authorized = SequencerTlsConnection::new(&endpoint).unwrap();
    observed_authorized.ensure_connected().unwrap();
    let mut boundaries = Vec::new();
    observed_authorized
        .try_post_observed_authorized(
            &body,
            &mut |stamp| boundaries.push(stamp.boundary),
            &mut || true,
        )
        .unwrap();

    let requests = mock.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1], requests[0], "authorized request bytes changed");
    assert_eq!(
        requests[2], requests[0],
        "observed-authorized request bytes changed"
    );
    assert!(!boundaries.is_empty(), "observed path still reports boundaries");
}

#[test]
fn live_submit_wire_roundtrips_against_mock_sequencer() {
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let eoa = key.address();
    let raw = build_signed_submission(&key);

    // (1) The signed tx recovers to the EOA we "funded" — signature wellformed, chain id correct.
    let recovered = engine_feed::recover_tx_sender(&raw).expect("recover sender from signed tx");
    assert_eq!(recovered, eoa, "signed tx must recover to the signing EOA");

    // (2) Wrap in the JSON-RPC envelope.
    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);

    // (3) POST to the mock over a real socket.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let mock = spawn_mock(
        listener,
        br#"{"jsonrpc":"2.0","id":1,"result":"0xdeadbeefcafe"}"#,
    );

    let mut conn =
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("conn");
    let resp = conn.post_json(&body).expect("post_json");

    // (4) Response parsed + classified as Accepted (carried a "result").
    assert!(
        String::from_utf8_lossy(&resp).contains("0xdeadbeefcafe"),
        "response body should carry the tx hash"
    );
    assert_eq!(classify_submit_response(&resp), SubmitOutcome::Accepted);

    // (5) The mock received a well-formed POST carrying our EXACT signed bytes.
    let req = mock.join().expect("mock join");
    let req_s = String::from_utf8_lossy(&req);
    assert!(req_s.starts_with("POST /rpc HTTP/1.1"), "method/path wrong");
    assert!(req_s.contains("Content-Type: application/json"));
    assert!(
        req_s.contains(&format!("Content-Length: {}", body.len())),
        "Content-Length must match the body length"
    );
    assert!(req_s.contains("\"method\":\"eth_sendRawTransaction\""));

    // params[0] must equal "0x" + hex(raw signed tx) — the wire carried exactly what we signed.
    let mut want = String::from("0x");
    for b in &raw {
        want.push_str(&format!("{b:02x}"));
    }
    assert!(
        req_s.contains(&want),
        "envelope params[0] must be the exact signed-tx hex"
    );
}

#[test]
fn max_engine_calldata_submit_wire_roundtrips_against_mock_sequencer() {
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let eoa = key.address();
    let raw = build_signed_submission_with_calldata_len(&key, 1476, 1_600_000);

    let recovered = engine_feed::recover_tx_sender(&raw).expect("recover sender from signed tx");
    assert_eq!(
        recovered, eoa,
        "signed max-calldata tx must recover to signer"
    );

    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let mock = spawn_mock(
        listener,
        br#"{"jsonrpc":"2.0","id":1,"result":"0xlargecalldata"}"#,
    );

    let mut conn =
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("conn");
    let resp = conn.post_json(&body).expect("post_json");
    assert_eq!(classify_submit_response(&resp), SubmitOutcome::Accepted);

    let req = mock.join().expect("mock join");
    let req_s = String::from_utf8_lossy(&req);
    assert!(
        req_s.contains(&format!("Content-Length: {}", body.len())),
        "Content-Length must match the large calldata body"
    );

    let mut want = String::from("0x");
    for b in &raw {
        want.push_str(&format!("{b:02x}"));
    }
    assert!(
        req_s.contains(&want),
        "large calldata envelope params[0] must be the exact signed-tx hex"
    );
}

#[test]
fn realistic_submission_corpus_fits_under_max_submit_body_with_headroom() {
    // P8: the checked MAX_SUBMIT_BODY buffer bound must comfortably cover every realistic Mantis
    // submission, so a legitimate tx is NEVER rejected/truncated. Sweep calldata lengths from a
    // 1-hop (~200 B) to the largest 3-hop mixed dispatch (~1476 B) and assert the JSON-RPC body
    // stays well under the bound. If a future calldata shape approaches it, this test forces a
    // conscious re-measure rather than a silent truncation.
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let mut worst = 0usize;
    for calldata_len in [196usize, 388, 580, 1124, 1476, 1600] {
        let raw = build_signed_submission_with_calldata_len(&key, calldata_len, 2_000_000);
        let mut body = Vec::new();
        build_send_raw_tx_envelope(&raw, u64::MAX, &mut body); // widest id
        worst = worst.max(body.len());
        assert!(
            body.len() <= MAX_SUBMIT_BODY,
            "calldata {calldata_len} B ⇒ body {} B exceeds MAX_SUBMIT_BODY {MAX_SUBMIT_BODY}",
            body.len()
        );
    }
    // Material headroom: the worst realistic body must be under 80% of the bound.
    assert!(
        worst * 5 <= MAX_SUBMIT_BODY * 4,
        "worst realistic body {worst} B has < 20% headroom under MAX_SUBMIT_BODY {MAX_SUBMIT_BODY}"
    );
}

#[test]
fn delivery_ambiguity_ack_lost_after_full_post_classifies_possibly_written() {
    // P7B capital-safety proof: the mock reads the FULL POST (so the tx reached the sequencer) and
    // then closes WITHOUT replying (the ack is lost). try_post must classify this PossiblyWritten so
    // the caller never reclaims the nonce for different bytes. Fast (EOF, no read-timeout wait).
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let raw = build_signed_submission(&key);
    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let mock = thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let _req = read_full_http_request(&mut s); // consume the whole request ⇒ tx delivered
        drop(s); // no response — ack lost
    });

    let mut conn =
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("conn");
    conn.ensure_connected().expect("connect");
    let started = std::time::Instant::now();
    match conn.try_post(&body) {
        Err(f) => assert_eq!(
            f.delivery,
            DeliveryState::PossiblyWritten,
            "a delivered request whose ack is lost must be PossiblyWritten"
        ),
        Ok(r) => panic!("expected failure, got {}", String::from_utf8_lossy(&r)),
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "EOF classification must be fast, not blocked on the read timeout"
    );
    mock.join().expect("mock join");
}

/// A mock that replies with `Transfer-Encoding: chunked` (no Content-Length) and then STALLS the
/// socket open — the pre-P8 read-until-close fallback would block here for the full read timeout.
fn spawn_mock_chunked_then_stall(listener: TcpListener) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let _req = read_full_http_request(&mut s);
        // Headers only; a first chunk but never the terminating 0-length chunk, socket kept open.
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n").ok();
        s.write_all(b"5\r\nhello\r\n").ok();
        s.flush().ok();
        // Hold the connection open briefly so a broken client would block on read; then drop.
        thread::sleep(std::time::Duration::from_millis(200));
    })
}

#[test]
fn chunked_response_is_rejected_fast_not_blocked() {
    // P8: a chunked (no Content-Length) reply on a keep-alive socket must be rejected immediately
    // via SubmitError::ChunkedResponse rather than reading until close (the full read timeout).
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let raw = build_signed_submission(&key);
    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let mock = spawn_mock_chunked_then_stall(listener);

    let mut conn =
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("conn");
    let started = std::time::Instant::now();
    let result = conn.post_json(&body);
    let elapsed = started.elapsed();
    assert!(
        matches!(result, Err(SubmitError::ChunkedResponse)),
        "chunked reply must be rejected as ChunkedResponse, got {result:?}"
    );
    // The whole point: it returns fast (well under the 10 s read timeout), not blocked to timeout.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "chunked reply must fail fast, took {elapsed:?}"
    );
    mock.join().expect("mock join");
}

/// POST a real signed envelope, build it once, reuse across the classification tests.
fn post_and_get_response(response_body: &'static [u8]) -> Vec<u8> {
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let raw = build_signed_submission(&key);
    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let mock = spawn_mock(listener, response_body);
    let mut conn =
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("conn");
    let resp = conn.post_json(&body).expect("post_json");
    mock.join().expect("mock join");
    resp
}

#[test]
fn sequencer_error_response_classifies_as_rejected() {
    // The sequencer accepted the HTTP call but REJECTED the tx pre-inclusion (reverted in validation
    // / underpriced). A genuine pre-inclusion reject ⇒ Rejected (adverse signal; the nonce was never
    // sequenced, so it is reclaimed). [Codex-F6] Note: "nonce too low" / "already known" now classify
    // as AmbiguousLanded (release, not reclaim) — see the jsonrpc unit tests.
    let resp = post_and_get_response(
        br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"execution reverted"}}"#,
    );
    assert_eq!(classify_submit_response(&resp), SubmitOutcome::Rejected);
}

#[test]
fn malformed_response_classifies_as_malformed() {
    // Neither `"result"` nor `"error"` ⇒ Malformed (fail-closed: never counted as accepted).
    let resp = post_and_get_response(br#"{"jsonrpc":"2.0","id":1}"#);
    assert_eq!(classify_submit_response(&resp), SubmitOutcome::Malformed);
}

/// A one-shot mock that DROPS the first connection (immediate EOF, no response) and serves the
/// second — exercises `post_json`'s reconnect-once-on-error path.
fn spawn_mock_drop_then_serve(
    listener: TcpListener,
    response_body: &'static [u8],
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // First attempt: accept and immediately close → client read hits EOF → SubmitError.
        let (s1, _) = listener.accept().expect("first accept");
        drop(s1);
        // Second attempt (the reconnect): serve a normal response.
        let (mut s2, _) = listener.accept().expect("second accept");
        let _req = read_full_http_request(&mut s2);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            response_body.len()
        );
        s2.write_all(head.as_bytes()).ok();
        s2.write_all(response_body).ok();
        s2.flush().ok();
    })
}

#[test]
fn post_json_reconnects_once_after_dropped_connection() {
    let key = SigningKey::from_bytes(&TEST_KEY).expect("key");
    let raw = build_signed_submission(&key);
    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let mock =
        spawn_mock_drop_then_serve(listener, br#"{"jsonrpc":"2.0","id":1,"result":"0xrecon"}"#);

    let mut conn =
        SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("conn");
    // First socket is dropped by the mock; post_json must reconnect and succeed on the retry.
    let resp = conn
        .post_json(&body)
        .expect("post_json should reconnect and succeed");
    assert_eq!(classify_submit_response(&resp), SubmitOutcome::Accepted);
    assert!(String::from_utf8_lossy(&resp).contains("0xrecon"));
    mock.join().expect("mock join");
}
