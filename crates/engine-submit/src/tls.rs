//! TLS submit connector.
//!
//! Builds a `rustls 0.23` HTTPS client over a warm `TcpStream` to the Arbitrum sequencer
//! endpoint. The submitter writes `eth_sendRawTransaction` / `eth_sendRawTransactionConditional`
//! JSON-RPC envelopes through this connector. No async runtime — the submitter thread is
//! blocking, pinned to vCPU 6, and parks on the socket.
//!
//! `TLS_DEFAULT_PORT = 443`. The connector retains the TLS handle across submissions so
//! we never pay TLS handshake latency on the hot path.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustls::{ClientConfig, ClientConnection, HandshakeKind, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;
use thiserror::Error;
use url::Url;

use crate::hot_spare::{SubmitBoundary, SubmitBoundaryStamp};

#[inline]
fn emit_submit_boundary(
    observe: &mut Option<&mut dyn FnMut(SubmitBoundaryStamp)>,
    boundary: SubmitBoundary,
) {
    if let Some(observe) = observe.as_deref_mut() {
        observe(SubmitBoundaryStamp::now(boundary));
    }
}

/// Default TLS port.
pub const TLS_DEFAULT_PORT: u16 = 443;

/// P4 telemetry: number of TLS handshakes completed at CONNECT time (off the submit hot path). A
/// live submit should never trigger a handshake — the connection is fully handshaken before any
/// `eth_sendRawTransaction` is written — so this counter rising only on connect/reconnect (never
/// correlated with a submit) is the pass-bar signal.
static TLS_HANDSHAKES_AT_CONNECT: AtomicU64 = AtomicU64::new(0);

/// Read the count of TLS handshakes completed at connect time (P4 telemetry / heartbeat).
pub fn tls_handshakes_at_connect() -> u64 {
    TLS_HANDSHAKES_AT_CONNECT.load(Ordering::Relaxed)
}

/// P8: checked maximum JSON-RPC submit body (the `eth_sendRawTransaction` envelope handed to
/// `post_json`/`try_post`). The largest realistic body is a 3-hop Mantis `flashLoanArbMixed`
/// submission: ~1476-byte calldata ⇒ ~1610-byte signed EIP-1559 RLP ⇒ ~3290-byte hex-in-JSON body.
/// 8 KiB is ~2.5× that worst case, so a legitimate submission NEVER hits it, while an out-of-band
/// oversize body is REJECTED (fail-closed) rather than truncated — truncating would put a different,
/// invalid transaction on the wire. Buffers preallocate + prefault to this bound so the submit hot
/// path never grows/relocates or faults a page.
pub const MAX_SUBMIT_BODY: usize = 8192;

/// P8: preallocated HTTP/1.1 request buffer size — the JSON body plus a fixed framing header
/// allowance (request line + Host/Content-Type/Content-Length/Connection/Accept + CRLFs, ≤512 B).
pub const MAX_SUBMIT_REQUEST: usize = MAX_SUBMIT_BODY + 512;

/// Submit connector errors.
#[derive(Debug, Error)]
pub enum SubmitError {
    /// The caller's feed/state authority permit was revoked before this wire attempt.
    #[error("submit authority revoked before socket write")]
    AuthorityRevoked,
    /// URL parsing failed.
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    /// TLS setup or handshake failed.
    #[error("tls error: {0}")]
    Tls(String),
    /// Socket-level I/O failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Response was malformed.
    #[error("malformed response")]
    Malformed,
    /// P8: the JSON-RPC body exceeded [`MAX_SUBMIT_BODY`]. Rejected fail-closed (never truncated —
    /// a truncated submission is a different, invalid transaction). This should never fire for a
    /// real Mantis submission; it fires only on a corrupt/oversize envelope.
    #[error("submit body {len} B exceeds max {max} B")]
    OversizeBody {
        /// Actual body length.
        len: usize,
        /// The checked bound ([`MAX_SUBMIT_BODY`]).
        max: usize,
    },
    /// P8: the response used `Transfer-Encoding: chunked`. The connector frames on `Content-Length`
    /// (what the Nitro sequencer sends); chunked is unexpected and is rejected fast rather than
    /// blocking the full read timeout on a keep-alive socket. The request already went out, so the
    /// caller must treat this as delivery-ambiguous (possibly written).
    #[error("unsupported chunked response framing")]
    ChunkedResponse,
}

/// P7B: how far a submit attempt got, so the caller can decide whether the claimed nonce may be
/// RECLAIMED (returned to the lane for re-issue with DIFFERENT bytes) or must be treated as
/// possibly-consumed. This is a capital-safety classification: a `PossiblyWritten` submit whose
/// nonce is reclaimed and re-signed with different calldata would put TWO transactions on the same
/// nonce, one of which may land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// No byte of the request reached the socket on ANY attempt (e.g. the connection was never
    /// established). Safe to reclaim the nonce and re-issue it — nothing was sent under it.
    DefinitelyNotWritten,
    /// A request began transmitting on at least one attempt but no usable ack was read (write error,
    /// read timeout, partial io_uring write, or unparseable/chunked framing). The tx may be in the
    /// sequencer's mempool under this nonce, so the nonce MUST NOT be reclaimed for different bytes;
    /// the lane is disarmed pending a chain-nonce reseed.
    PossiblyWritten,
    /// The sequencer returned a parseable ack. Delivery is known and the ack's
    /// [`crate::SubmitOutcome`] governs release-vs-reclaim (this variant is the `Ok` path — it does
    /// not appear inside a [`SubmitFailure`]).
    Acknowledged,
}

/// A failed submit attempt plus how far it got on the wire ([`DeliveryState`]). Returned by the
/// hot-path `try_post`/`HotSpare::submit` so the nonce-reclaim decision is delivery-aware (P7B).
#[derive(Debug)]
pub struct SubmitFailure {
    /// The underlying error.
    pub err: SubmitError,
    /// Whether any bytes may have reached the sequencer.
    pub delivery: DeliveryState,
}

impl SubmitFailure {
    /// The request definitely never reached the socket — safe to reclaim the nonce.
    pub fn not_written(err: SubmitError) -> Self {
        Self {
            err,
            delivery: DeliveryState::DefinitelyNotWritten,
        }
    }

    /// The request may have reached the sequencer — the nonce must not be reclaimed for other bytes.
    pub fn possibly_written(err: SubmitError) -> Self {
        Self {
            err,
            delivery: DeliveryState::PossiblyWritten,
        }
    }
}

impl std::fmt::Display for SubmitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (delivery={:?})", self.err, self.delivery)
    }
}

/// One warm submit connection — TLS (`https://`, production sequencer) or plain TCP (`http://`,
/// a local sequencer-forwarder or test mock). Both keep the socket warm across submissions.
enum Conn {
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
    Plain(TcpStream),
}

impl Conn {
    fn raw_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        match self {
            Conn::Tls(s) => s.get_ref().as_raw_fd(),
            Conn::Plain(t) => t.as_raw_fd(),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tls(s) => s.read(b),
            Conn::Plain(t) => t.read(b),
        }
    }
}
impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Tls(s) => s.write(b),
            Conn::Plain(t) => t.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Tls(s) => s.flush(),
            Conn::Plain(t) => t.flush(),
        }
    }
}

/// Warm connection to a JSON-RPC sequencer endpoint (HTTPS in production; HTTP for a local
/// forwarder / test mock).
pub struct SequencerTlsConnection {
    host: String,
    port: u16,
    url_path: String,
    tls: bool,
    /// Optional pinned submit IP (`SUBMIT_PIN_IP`). When unset, the connector RTT-probes all
    /// resolved IPs and picks the lowest (the sequencer endpoint is multi-A-record / location-
    /// dependent). The race-deciding leg, so the IP choice matters.
    pin_ip: Option<IpAddr>,
    config: Arc<ClientConfig>,
    /// P7C: bounded-blackhole recovery knobs (default-preserving). `connect`/`write`/`read(=ack)`
    /// socket timeouts + optional `TCP_USER_TIMEOUT`, all read once at construction from
    /// `SUBMIT_CONNECT_TIMEOUT_MS` / `SUBMIT_WRITE_TIMEOUT_MS` / `SUBMIT_ACK_TIMEOUT_MS` /
    /// `SUBMIT_TCP_USER_TIMEOUT_MS`. Defaults reproduce today's hard-coded 5s / 5s / 10s / off.
    connect_timeout: Duration,
    write_timeout: Duration,
    ack_timeout: Duration,
    tcp_user_timeout_ms: u32,
    stream: Option<Conn>,
    /// Reusable HTTP request buffer — the request build sits BEFORE the socket write on the
    /// FCFS-race leg, so it must not pay a per-submit allocation. `mem::take`n around the
    /// write (borrow split), then put back with its capacity.
    req_buf: Vec<u8>,
    /// Reusable response read buffer (8 KiB steady-state). Post-write (off the race leg), but
    /// reusing it removes per-submit allocator jitter on the pinned submit thread.
    resp_buf: Vec<u8>,
}

impl SequencerTlsConnection {
    /// P4: build the SHARED rustls client config — webpki roots, no client auth, and rustls' default
    /// in-memory TLS session resumption. Share ONE `Arc<ClientConfig>` across every connector (e.g.
    /// both hot-spare sockets) so the TLS 1.3 resumption cache is shared: a reconnect or the second
    /// socket can RESUME a prior session and skip a handshake round trip. Two independently-built
    /// configs have SEPARATE caches and never resume each other.
    pub fn shared_client_config() -> Arc<ClientConfig> {
        // Install the process CryptoProvider BEFORE `ClientConfig::builder()` below: with both
        // `aws-lc-rs` and `ring` in the workspace graph, rustls 0.23 cannot auto-select and the
        // builder PANICS. Production installs it in main(), but a consumer without that (this
        // crate's own tests under a `--workspace` build, 2026-07-08) dies here. Idempotent —
        // a second install returns Err and is ignored. Same fix as WsRelayClient::new.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(config)
    }

    /// Build a connector targeting `endpoint` (e.g. `https://arb1-sequencer.arbitrum.io/rpc`, or
    /// `http://127.0.0.1:PORT/rpc` for a local forwarder / test mock) with its OWN client config.
    /// When constructing multiple connectors (the hot spare) prefer [`Self::with_config`] with a
    /// shared config so they share the TLS resumption cache (P4).
    pub fn new(endpoint: &str) -> Result<Self, SubmitError> {
        Self::with_config(endpoint, Self::shared_client_config())
    }

    /// Build a connector targeting `endpoint` over the given SHARED client `config` (P4).
    pub fn with_config(endpoint: &str, config: Arc<ClientConfig>) -> Result<Self, SubmitError> {
        // `SUBMIT_PIN_IP` manual override (e.g. the AZ-probe-winning sequencer IP); else auto-probe.
        let pin_ip = std::env::var("SUBMIT_PIN_IP")
            .ok()
            .and_then(|s| s.trim().parse().ok());
        Self::with_config_and_optional_pin(endpoint, config, pin_ip)
    }

    /// Build a connector over a shared client `config`, explicitly pinned to `pin_ip`.
    ///
    /// Unlike [`Self::with_config`], this constructor never reads `SUBMIT_PIN_IP`. It is intended for
    /// diagnostics that must construct independent connectors for every address from one DNS
    /// snapshot without mutating process-global environment state. TLS still authenticates
    /// `endpoint`'s hostname; only the TCP destination address is pinned.
    pub fn with_config_and_pin(
        endpoint: &str,
        config: Arc<ClientConfig>,
        pin_ip: IpAddr,
    ) -> Result<Self, SubmitError> {
        Self::with_config_and_optional_pin(endpoint, config, Some(pin_ip))
    }

    fn with_config_and_optional_pin(
        endpoint: &str,
        config: Arc<ClientConfig>,
        pin_ip: Option<IpAddr>,
    ) -> Result<Self, SubmitError> {
        let url = Url::parse(endpoint).map_err(|_| SubmitError::InvalidUrl(endpoint.into()))?;
        let tls = url.scheme() != "http";
        let host = url
            .host_str()
            .ok_or_else(|| SubmitError::InvalidUrl(endpoint.into()))?
            .to_string();
        let port = url
            .port()
            .unwrap_or(if tls { TLS_DEFAULT_PORT } else { 80 });
        let mut url_path = if url.path().is_empty() {
            "/".into()
        } else {
            url.path().to_string()
        };
        if let Some(query) = url.query() {
            url_path.push('?');
            url_path.push_str(query);
        }
        // P7C: bounded-blackhole recovery knobs, read once. Defaults reproduce today's hard-coded
        // 5s connect / 5s write / 10s ack, and TCP_USER_TIMEOUT off. A blackholed sequencer (peer
        // stops ACKing, no RST/FIN) is then bounded by these instead of a multi-minute kernel stall.
        let connect_timeout = Duration::from_millis(env_u64_ms("SUBMIT_CONNECT_TIMEOUT_MS", 5_000));
        let write_timeout = Duration::from_millis(env_u64_ms("SUBMIT_WRITE_TIMEOUT_MS", 5_000));
        let ack_timeout = Duration::from_millis(env_u64_ms("SUBMIT_ACK_TIMEOUT_MS", 10_000));
        let tcp_user_timeout_ms = env_u64_ms("SUBMIT_TCP_USER_TIMEOUT_MS", 0) as u32;
        // P8: preallocate + PREFAULT both submit buffers to their checked bounds so the hot path
        // never grows/relocates them or faults a fresh page on first use. `resize` then `clear`
        // touches every page (making it resident) while leaving `len == 0` and the capacity intact,
        // so `build_http_request_into`'s `reserve` is a no-op and the JSON/HTTP framing writes into
        // already-resident memory.
        let mut req_buf = Vec::with_capacity(MAX_SUBMIT_REQUEST);
        req_buf.resize(MAX_SUBMIT_REQUEST, 0);
        req_buf.clear();
        Ok(Self {
            host,
            port,
            url_path,
            tls,
            pin_ip,
            config,
            connect_timeout,
            write_timeout,
            ack_timeout,
            tcp_user_timeout_ms,
            stream: None,
            req_buf,
            resp_buf: vec![0u8; MAX_SUBMIT_BODY],
        })
    }

    /// Return the actual remote socket address of the current connection.
    ///
    /// `Ok(None)` means the connector is not connected. A connected result comes directly from the
    /// underlying [`TcpStream::peer_addr`], so callers can label measurements with the peer that was
    /// really used rather than a separate DNS selection.
    pub fn peer_addr(&self) -> std::io::Result<Option<SocketAddr>> {
        match self.stream.as_ref() {
            Some(Conn::Tls(stream)) => stream.get_ref().peer_addr().map(Some),
            Some(Conn::Plain(stream)) => stream.peer_addr().map(Some),
            None => Ok(None),
        }
    }

    /// Return whether the current TLS connection used a full or resumed handshake.
    ///
    /// A disconnected connector and a plain-HTTP connection both return `None` because neither has
    /// a current TLS handshake to classify.
    pub fn handshake_kind(&self) -> Option<HandshakeKind> {
        match self.stream.as_ref()? {
            Conn::Tls(stream) => stream.conn.handshake_kind(),
            Conn::Plain(_) => None,
        }
    }

    /// Connect (or reconnect) the warm socket and apply the low-latency submit-FD options:
    /// `TCP_NODELAY` (flush the ~200B POST immediately), `SO_BUSY_POLL` (spin the response in),
    /// `SO_KEEPALIVE` (keep the path warm). `TCP_QUICKACK` is re-armed per recv inside
    /// `write_and_read` since the kernel clears it after each read.
    pub fn connect(&mut self) -> Result<(), SubmitError> {
        // Submit-IP selection (the FCFS-race-deciding leg). `SUBMIT_PIN_IP` is a manual override;
        // otherwise RTT-probe all resolved IPs and pick the lowest — the sequencer endpoint is
        // multi-A-record and the best IP is location-dependent (liqd latency campaign measured up to
        // ~0.26 ms/tx between IPs). Resolved ONCE on the warm connection (startup/reconnect), never on
        // the per-tx hot path; falls back to the first resolved address if probing yields nothing.
        let addr: SocketAddr = match self.pin_ip {
            Some(ip) => SocketAddr::new(ip, self.port),
            None => engine_feed::relay_client::pick_best_addr(&self.host, self.port)
                .or_else(|| {
                    (self.host.as_str(), self.port)
                        .to_socket_addrs()
                        .ok()
                        .and_then(|mut a| a.next())
                })
                .ok_or_else(|| SubmitError::InvalidUrl(self.host.clone()))?,
        };
        // P7C: bounded-blackhole recovery timeouts (default-preserving: 5s / 10s / 5s).
        let mut tcp = TcpStream::connect_timeout(&addr, self.connect_timeout)?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(self.ack_timeout))?;
        tcp.set_write_timeout(Some(self.write_timeout))?;
        // Low-latency submit-FD tuning (best-effort; isolated unsafe lives in engine-sysopt).
        {
            use std::os::unix::io::AsRawFd;
            let fd = tcp.as_raw_fd();
            let _ = engine_sysopt::set_so_busy_poll(fd, 50);
            let _ = engine_sysopt::set_so_keepalive(fd, true);
            // P7C: TCP_USER_TIMEOUT (0 = kernel default / off) bounds unacknowledged transmitted
            // data so a blackholed peer fails fast instead of a multi-minute retransmit stall.
            if self.tcp_user_timeout_ms > 0 {
                let _ = engine_sysopt::set_tcp_user_timeout(fd, self.tcp_user_timeout_ms);
            }
        }
        self.stream = Some(if self.tls {
            let server_name = ServerName::try_from(self.host.clone())
                .map_err(|e| SubmitError::Tls(e.to_string()))?;
            let mut conn = ClientConnection::new(self.config.clone(), server_name)
                .map_err(|e| SubmitError::Tls(e.to_string()))?;
            // P4: drive the TLS handshake to completion NOW (off the submit hot path) so the first
            // real `eth_sendRawTransaction` never pays a handshake RTT. `complete_io` reads/writes
            // the blocking socket until the handshake finishes; because all connectors share one
            // `Arc<ClientConfig>`, a reconnect/second socket can RESUME a prior TLS 1.3 session and
            // skip a round trip. The write timeout bounds a stalled handshake.
            while conn.is_handshaking() {
                conn.complete_io(&mut tcp)
                    .map_err(|e| SubmitError::Tls(format!("handshake: {e}")))?;
            }
            TLS_HANDSHAKES_AT_CONNECT.fetch_add(1, Ordering::Relaxed);
            Conn::Tls(Box::new(StreamOwned::new(conn, tcp)))
        } else {
            Conn::Plain(tcp)
        });
        Ok(())
    }

    /// Issue one HTTP POST with `json_body` as the request body. Returns the response
    /// body bytes. Reconnects automatically on failure.
    pub fn post_json(&mut self, json_body: &[u8]) -> Result<Vec<u8>, SubmitError> {
        // P8: fail-closed on an oversize body (never truncate — a truncated submit is a different tx).
        if json_body.len() > MAX_SUBMIT_BODY {
            return Err(SubmitError::OversizeBody {
                len: json_body.len(),
                max: MAX_SUBMIT_BODY,
            });
        }
        if self.stream.is_none() {
            self.connect()?;
        }
        // Borrow split: take the reusable buffer out of `self`, fill, write, put back — the
        // request build pays no per-submit allocation (it sits before the write on the race leg).
        let mut request = std::mem::take(&mut self.req_buf);
        build_http_request_into(&self.url_path, &self.host, json_body, &mut request);
        let mut result = self.write_and_read(&request, None, None);
        if matches!(&result, Err(f) if matches!(f.err, SubmitError::ChunkedResponse)) {
            // The request WAS delivered; the reply used framing we don't accept and left unconsumed
            // bytes on the socket. Drop the (now-desynced) socket so the next post reconnects
            // cleanly, but do NOT re-send — a chunking server would just chunk again, and the tx is
            // already on the wire (P8).
            self.stream = None;
        } else if result.is_err() {
            // Transport error: reconnect once and retry the identical bytes (the request bytes don't
            // depend on the stream, so the already-built buffer is reused as-is).
            self.stream = None;
            self.connect()?;
            result = self.write_and_read(&request, None, None);
        }
        self.req_buf = request;
        // post_json (heartbeat / tests) doesn't need the delivery classification.
        result.map_err(|f| f.err)
    }

    /// Test-visible request framing (allocating variant; the hot path uses the reusable buffer).
    #[cfg(test)]
    fn build_http_request(&self, body: &[u8]) -> Vec<u8> {
        let mut req = Vec::new();
        build_http_request_into(&self.url_path, &self.host, body, &mut req);
        req
    }

    /// Keep the warm connection alive with an `eth_chainId` heartbeat (call every 5–10 s idle) so
    /// the first real submit never pays a TCP+TLS handshake. Reconnects if the socket dropped.
    pub fn heartbeat(&mut self) -> Result<(), SubmitError> {
        let body = br#"{"jsonrpc":"2.0","id":0,"method":"eth_chainId","params":[]}"#;
        self.post_json(body).map(|_| ())
    }

    #[inline]
    fn write_plain_request<W: Write>(
        stream: &mut W,
        request: &[u8],
        reauthorize_after_definitely_not_written_fallback: bool,
        authorize: &mut Option<&mut dyn FnMut() -> bool>,
    ) -> Result<(), SubmitFailure> {
        // A zero-byte io_uring outcome is safe to retry, but it is still a distinct wire attempt:
        // degradation may have linearized while that attempt was in flight. Revalidate only on
        // this cold fallback so the ordinary TLS/plain path retains exactly one authority load.
        if reauthorize_after_definitely_not_written_fallback
            && authorize.as_deref_mut().is_some_and(|check| !check())
        {
            return Err(SubmitFailure::not_written(SubmitError::AuthorityRevoked));
        }
        stream
            .write_all(request)
            .map_err(|error| SubmitFailure::possibly_written(SubmitError::Io(error)))
    }

    fn write_and_read(
        &mut self,
        request: &[u8],
        mut observe: Option<&mut dyn FnMut(SubmitBoundaryStamp)>,
        mut authorize: Option<&mut dyn FnMut() -> bool>,
    ) -> Result<Vec<u8>, SubmitFailure> {
        // Reuse the response buffer across submits (taken before the stream borrow). Error paths
        // drop it — cold: the next call just re-grows from empty, and success always restores it.
        let mut buf = std::mem::take(&mut self.resp_buf);
        if buf.len() < MAX_SUBMIT_BODY {
            buf.resize(MAX_SUBMIT_BODY, 0);
        }
        // P7B: no stream ⇒ nothing was attempted ⇒ DefinitelyNotWritten (safe to reclaim the nonce).
        let stream = match self.stream.as_mut() {
            Some(s) => s,
            None => {
                return Err(SubmitFailure::not_written(SubmitError::Tls(
                    "no stream".into(),
                )))
            }
        };
        // Feed-authority linearization point: the complete, byte-exact HTTP request already exists,
        // but no byte has reached the socket. HotSpare supplies the candidate's captured epoch and
        // this predicate performs exactly one Acquire load per actual write/retry attempt. A revoked
        // permit is definitely-not-written, so upstream can safely reclaim the nonce unless an
        // earlier same-byte attempt was delivery-ambiguous.
        if authorize.as_deref_mut().is_some_and(|check| !check()) {
            return Err(SubmitFailure::not_written(SubmitError::AuthorityRevoked));
        }
        let fd = stream.raw_fd();
        // P7A: the io_uring send path is DEFAULT-OFF and only ever engaged for `Conn::Plain` (a local
        // forwarder / test mock) — production HTTPS stays on rustls' `StreamOwned::write_all`. When
        // opt-in (`--features io-uring-submit`) it FAILS CLOSED on a partial write: a `write_all`
        // fallback after a partial uring send would put `POST<partial>POST<full>` on the wire, so on
        // `UringDispatch::FailClosed` we return `Err` and let the caller drop the socket and retry
        // the identical bytes on a FRESH connection. Only "nothing was written" falls back to
        // `write_all` on the same socket.
        #[cfg(all(target_os = "linux", feature = "io-uring-submit"))]
        let (wrote_via_uring, reauthorize_plain_fallback) = match stream {
            Conn::Plain(_) if crate::io_uring_send::io_uring_available() => {
                match crate::io_uring_send::IoUringSendQueue::new(fd, None) {
                    Ok(mut q) => match classify_uring_send(q.submit_all(request)) {
                        UringDispatch::Done => (true, false),
                        UringDispatch::FallBack => (false, true),
                        // Partial request already on the wire (P7A) — PossiblyWritten (P7B).
                        UringDispatch::FailClosed(e) => {
                            return Err(SubmitFailure::possibly_written(SubmitError::Io(e)))
                        }
                    },
                    // Ring construction wrote no bytes, but authority can change while it fails;
                    // treat the subsequent plain write as a new attempt and revalidate it.
                    Err(_) => (false, true),
                }
            }
            _ => (false, false),
        };
        #[cfg(not(all(target_os = "linux", feature = "io-uring-submit")))]
        let (wrote_via_uring, reauthorize_plain_fallback) = (false, false);
        // P7B: from the moment we invoke the write, any failure is PossiblyWritten — TCP cannot
        // prove that a `write`/`flush`/`read` error left nothing at the sequencer.
        if !wrote_via_uring {
            Self::write_plain_request(stream, request, reauthorize_plain_fallback, &mut authorize)?;
        }
        if let Err(e) = stream.flush() {
            return Err(SubmitFailure::possibly_written(SubmitError::Io(e)));
        }
        // Production HTTPS reaches this only after rustls drained its encrypted records into the
        // local TcpStream. This is a LOCAL-KERNEL write-return boundary, never a NIC-TX timestamp.
        emit_submit_boundary(&mut observe, SubmitBoundary::TlsWriteLocalKernelReturn);
        // Read HTTP response. Simple parse: read headers until \r\n\r\n, then content-length bytes.
        let mut total = 0usize;
        let mut header_end: Option<usize> = None;
        loop {
            if total == buf.len() {
                buf.resize(buf.len() * 2, 0);
            }
            // Re-arm QUICKACK so our ACK of the response isn't delayed (kernel clears it per read).
            engine_sysopt::set_tcp_quickack(fd);
            // P7B: the request is already out — a read error/timeout means the ack was lost, NOT that
            // the tx never landed. PossiblyWritten (do not reclaim the nonce for different bytes).
            let n = match stream.read(&mut buf[total..]) {
                Ok(n) => n,
                Err(e) => return Err(SubmitFailure::possibly_written(SubmitError::Io(e))),
            };
            if n == 0 {
                break;
            }
            if total == 0 {
                // Application-visible plaintext after rustls read/decrypt, not a kernel/NIC RX stamp.
                emit_submit_boundary(&mut observe, SubmitBoundary::ResponseFirstByte);
            }
            total += n;
            if header_end.is_none() {
                if let Some(pos) = find_header_end(&buf[..total]) {
                    header_end = Some(pos);
                    // P8: reject chunked framing the moment the header block is complete. The
                    // connector frames on Content-Length (what the Nitro sequencer sends); the
                    // no-Content-Length fallback would otherwise read until close and block the full
                    // read timeout on a keep-alive socket. The request already went out, so the
                    // caller treats this as delivery-ambiguous.
                    if is_chunked_encoding(&buf[..pos]) {
                        return Err(SubmitFailure::possibly_written(
                            SubmitError::ChunkedResponse,
                        ));
                    }
                }
            }
            if let Some(hend) = header_end {
                if let Some(cl) = parse_content_length(&buf[..hend]) {
                    if total >= hend + cl {
                        break;
                    }
                } else {
                    // No Content-Length and not chunked: a keep-alive socket will not close, so we
                    // cannot determine the body length — do NOT block until the read timeout. Take
                    // what has arrived and let the caller classify (fail-closed).
                    break;
                }
            }
        }
        // P7B: socket closed before a complete header block — the request went out but the ack is
        // unusable, so this is PossiblyWritten (not a clean "never sent").
        let body_start = match header_end {
            Some(h) => h,
            None => return Err(SubmitFailure::possibly_written(SubmitError::Malformed)),
        };
        let body = buf[body_start..total].to_vec();
        self.resp_buf = buf;
        // Keep this after response framing and the body copy: the legacy SubmitWire timer ends when
        // `submit()` returns, so the fine split must retain response parsing/allocation in its tail.
        emit_submit_boundary(&mut observe, SubmitBoundary::ResponseDone);
        Ok(body)
    }

    fn try_post_inner(
        &mut self,
        body: &[u8],
        mut observe: Option<&mut dyn FnMut(SubmitBoundaryStamp)>,
        authorize: Option<&mut dyn FnMut() -> bool>,
    ) -> Result<Vec<u8>, SubmitFailure> {
        if body.len() > MAX_SUBMIT_BODY {
            return Err(SubmitFailure::not_written(SubmitError::OversizeBody {
                len: body.len(),
                max: MAX_SUBMIT_BODY,
            }));
        }
        if self.stream.is_none() {
            return Err(SubmitFailure::not_written(SubmitError::Tls(
                "not connected".into(),
            )));
        }
        let mut request = std::mem::take(&mut self.req_buf);
        build_http_request_into(&self.url_path, &self.host, body, &mut request);
        emit_submit_boundary(&mut observe, SubmitBoundary::HttpFrameComplete);
        let result = self.write_and_read(&request, observe, authorize);
        self.req_buf = request;
        if result.is_err() {
            self.stream = None;
        }
        result
    }
}

impl crate::hot_spare::SubmitConn for SequencerTlsConnection {
    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    fn ensure_connected(&mut self) -> Result<(), SubmitError> {
        if self.stream.is_none() {
            self.connect()?;
        }
        Ok(())
    }

    /// POST without the automatic inline reconnect (D1): on any I/O error the dead socket is dropped
    /// and the error returned, so the hot-spare can fail over to an already-warm connection instead
    /// of paying a 50–150 ms handshake on the submit critical path.
    fn try_post(&mut self, body: &[u8]) -> Result<Vec<u8>, SubmitFailure> {
        self.try_post_inner(body, None, None)
    }

    fn try_post_observed(
        &mut self,
        body: &[u8],
        observe: &mut dyn FnMut(SubmitBoundaryStamp),
    ) -> Result<Vec<u8>, SubmitFailure> {
        self.try_post_inner(body, Some(observe), None)
    }

    fn try_post_authorized(
        &mut self,
        body: &[u8],
        authorize: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, SubmitFailure> {
        self.try_post_inner(body, None, Some(authorize))
    }

    fn try_post_observed_authorized(
        &mut self,
        body: &[u8],
        observe: &mut dyn FnMut(SubmitBoundaryStamp),
        authorize: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, SubmitFailure> {
        self.try_post_inner(body, Some(observe), Some(authorize))
    }
}

/// P7A: how an optional io_uring send maps onto the plain-`write_all` fallback. Kept pure + separate
/// from the ring so the safety rule — a PARTIAL write must NEVER be followed by a second `write_all`
/// on the same socket (which would put `POST<partial>POST<full>` on the wire) — is unit-tested
/// without a live ring.
#[cfg(all(target_os = "linux", feature = "io-uring-submit"))]
#[derive(Debug)]
enum UringDispatch {
    /// io_uring wrote the whole request — do not write again.
    Done,
    /// Nothing reached the socket (ring build failed, or the first SQE errored with 0 bytes) — a
    /// `write_all` fallback on the same socket is safe.
    FallBack,
    /// A partial request is already on the socket — fail closed; the caller drops the socket and
    /// retries the identical bytes only on a FRESH connection.
    FailClosed(std::io::Error),
}

#[cfg(all(target_os = "linux", feature = "io-uring-submit"))]
fn classify_uring_send(
    result: Result<usize, crate::io_uring_send::UringSendError>,
) -> UringDispatch {
    match result {
        Ok(_) => UringDispatch::Done,
        Err(e) if e.written == 0 => UringDispatch::FallBack,
        Err(e) => UringDispatch::FailClosed(e.source),
    }
}

/// Frame an HTTP/1.1 POST into `out` (cleared first; capacity reused across submits).
fn build_http_request_into(url_path: &str, host: &str, body: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(256 + body.len());
    out.extend_from_slice(b"POST ");
    out.extend_from_slice(url_path.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(b"\r\nContent-Type: application/json\r\nContent-Length: ");
    write_usize_dec(body.len(), out);
    out.extend_from_slice(b"\r\nConnection: keep-alive\r\nAccept: application/json\r\n\r\n");
    out.extend_from_slice(body);
}

/// Decimal-format `n` into `out` without a temporary `String` (submit hot path).
fn write_usize_dec(mut n: usize, out: &mut Vec<u8>) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.extend_from_slice(&buf[i..]);
}

/// Parse a `u64` millisecond env var, falling back to `default` when unset/empty/unparseable (P7C).
fn env_u64_ms(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    memchr::memmem::find(buf, b"\r\n\r\n").map(|i| i + 4)
}

/// P8: alloc-free, case-insensitive lookup of an HTTP header value. `name_lower` is the header name
/// in lowercase WITHOUT the trailing colon. Scans the CRLF-delimited header block line by line,
/// compares the field name case-insensitively, and returns the trimmed (OWS-stripped) value slice
/// of the first match. Replaces the old "lowercase the entire response Vec then substring-search"
/// (an allocation + full copy on every submit ack).
fn header_value_ci<'a>(headers: &'a [u8], name_lower: &[u8]) -> Option<&'a [u8]> {
    let mut line_start = 0usize;
    while line_start < headers.len() {
        let line_end = memchr::memmem::find(&headers[line_start..], b"\r\n")
            .map(|i| line_start + i)
            .unwrap_or(headers.len());
        let line = &headers[line_start..line_end];
        if let Some(colon) = memchr::memchr(b':', line) {
            let name = &line[..colon];
            if name.len() == name_lower.len()
                && name
                    .iter()
                    .zip(name_lower)
                    .all(|(a, b)| a.to_ascii_lowercase() == *b)
            {
                let val = &line[colon + 1..];
                let start = val
                    .iter()
                    .position(|&b| b != b' ' && b != b'\t')
                    .unwrap_or(val.len());
                let end = val
                    .iter()
                    .rposition(|&b| b != b' ' && b != b'\t')
                    .map(|i| i + 1)
                    .unwrap_or(start);
                return Some(&val[start..end]);
            }
        }
        if line_end >= headers.len() {
            break;
        }
        line_start = line_end + 2;
    }
    None
}

/// Alloc-free case-insensitive substring test (`needle_lower` must already be ASCII-lowercase).
fn contains_ci(hay: &[u8], needle_lower: &[u8]) -> bool {
    let n = needle_lower.len();
    if n == 0 {
        return true;
    }
    if hay.len() < n {
        return false;
    }
    hay.windows(n).any(|w| {
        w.iter()
            .zip(needle_lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let val = header_value_ci(headers, b"content-length")?;
    let mut n = 0usize;
    for &b in val {
        if !b.is_ascii_digit() {
            break;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(n)
}

/// True if the response declares `Transfer-Encoding: chunked` (case-insensitive, alloc-free).
fn is_chunked_encoding(headers: &[u8]) -> bool {
    header_value_ci(headers, b"transfer-encoding").is_some_and(|v| contains_ci(v, b"chunked"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_submit_preserves_exact_http_request_and_emits_ordered_boundaries() {
        use crate::hot_spare::{SubmitBoundary, SubmitConn};
        use std::io::{Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;
        use std::thread;

        fn read_request(stream: &mut TcpStream) -> Vec<u8> {
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).expect("read request");
                assert!(n > 0, "peer closed before a complete request");
                request.extend_from_slice(&chunk[..n]);
                let Some(header_end) = find_header_end(&request) else {
                    continue;
                };
                let content_len = parse_content_length(&request[..header_end]).expect("length");
                if request.len() >= header_end + content_len {
                    request.truncate(header_end + content_len);
                    return request;
                }
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                request_tx.send(read_request(&mut stream)).unwrap();
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                    .unwrap();
                stream.flush().unwrap();
            }
        });

        let endpoint = format!("http://127.0.0.1:{port}/rpc");
        let body = br#"{"jsonrpc":"2.0","id":91,"method":"eth_sendRawTransaction","params":["0x02f8feed"]}"#;

        let mut ordinary = SequencerTlsConnection::new(&endpoint).unwrap();
        ordinary.ensure_connected().unwrap();
        ordinary.try_post(body).unwrap();
        drop(ordinary);

        let mut observed = SequencerTlsConnection::new(&endpoint).unwrap();
        observed.ensure_connected().unwrap();
        let mut boundaries = Vec::new();
        observed
            .try_post_observed(body, &mut |stamp| boundaries.push(stamp.boundary))
            .unwrap();
        drop(observed);

        let ordinary_request = request_rx.recv().unwrap();
        let observed_request = request_rx.recv().unwrap();
        assert_eq!(
            observed_request, ordinary_request,
            "instrumentation changed POST bytes"
        );
        assert!(observed_request.ends_with(body), "JSON-RPC body changed");
        assert_eq!(
            boundaries,
            [
                SubmitBoundary::HttpFrameComplete,
                SubmitBoundary::TlsWriteLocalKernelReturn,
                SubmitBoundary::ResponseFirstByte,
                SubmitBoundary::ResponseDone,
            ],
        );
        server.join().unwrap();
    }

    #[test]
    fn build_request_includes_host_and_body() {
        let conn = SequencerTlsConnection::new("https://arb1-sequencer.arbitrum.io/rpc").unwrap();
        let body = b"{\"id\":1}";
        let req = conn.build_http_request(body);
        let s = std::str::from_utf8(&req).unwrap();
        assert!(s.contains("Host: arb1-sequencer.arbitrum.io"));
        assert!(s.contains("Content-Length: 8"));
        assert!(s.ends_with("{\"id\":1}"));
    }

    #[test]
    fn build_request_preserves_endpoint_query() {
        let conn = SequencerTlsConnection::new(
            "https://arb1-sequencer.arbitrum.io/rpc?key=credential-placeholder",
        )
        .unwrap();
        let req = conn.build_http_request(br#"{"id":1}"#);
        let text = std::str::from_utf8(&req).unwrap();
        assert!(text.starts_with("POST /rpc?key=credential-placeholder HTTP/1.1\r\n"));
    }

    #[test]
    fn request_buffer_reuse_reframes_cleanly_across_different_body_lengths() {
        // The hot path reuses one request buffer across submits; a longer request followed by a
        // shorter one must not leak stale tail bytes into the frame.
        let mut buf = Vec::new();
        build_http_request_into(
            "/rpc",
            "h.example",
            b"{\"id\":1,\"padding\":\"xxxxxxxx\"}",
            &mut buf,
        );
        let long_len = buf.len();
        build_http_request_into("/rpc", "h.example", b"{\"id\":2}", &mut buf);
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(buf.len() < long_len, "shorter body must shrink the frame");
        assert!(s.starts_with("POST /rpc HTTP/1.1\r\n"));
        assert!(s.contains("Content-Length: 8\r\n"));
        assert!(
            s.ends_with("{\"id\":2}"),
            "no stale tail from the longer frame"
        );
    }

    #[test]
    fn write_usize_dec_matches_display() {
        for n in [0usize, 1, 9, 10, 99, 8192, usize::MAX] {
            let mut out = Vec::new();
            write_usize_dec(n, &mut out);
            assert_eq!(String::from_utf8(out).unwrap(), n.to_string());
        }
    }

    #[test]
    fn parse_content_length_works() {
        let headers =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 42\r\n\r\n";
        assert_eq!(parse_content_length(headers), Some(42));
    }

    #[test]
    fn parse_content_length_is_case_insensitive_and_alloc_free() {
        // P8: the alloc-free scanner must match regardless of header-name casing and OWS, and must
        // NOT be fooled by a similarly-named header or a value substring elsewhere.
        for h in [
            &b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nCONTENT-LENGTH:7\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length:  \t7\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nX-Foo: content-length: 999\r\nContent-Length: 7\r\n\r\n"[..],
        ] {
            assert_eq!(parse_content_length(h), Some(7), "case/OWS/decoy: {h:?}");
        }
        // A header whose NAME merely contains the substring must not match.
        let decoy = b"HTTP/1.1 200 OK\r\nX-Content-Length-Note: 5\r\n\r\n";
        assert_eq!(parse_content_length(decoy), None);
    }

    #[test]
    fn chunked_transfer_encoding_detected_case_insensitively() {
        // P8: chunked framing must be recognised (any casing) so the read loop rejects it fast
        // instead of blocking the full read timeout on a keep-alive socket.
        assert!(is_chunked_encoding(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
        ));
        assert!(is_chunked_encoding(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: CHUNKED\r\n\r\n"
        ));
        assert!(is_chunked_encoding(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n"
        ));
        // Content-Length framing (the sequencer's real reply) is NOT chunked.
        assert!(!is_chunked_encoding(
            b"HTTP/1.1 200 OK\r\nContent-Length: 42\r\n\r\n"
        ));
    }

    #[test]
    fn oversize_body_is_rejected_before_any_connect() {
        // P8: an oversize body is refused fail-closed (never truncated). The guard sits before the
        // connect, so this needs no server — a body one byte over the bound must error.
        let mut conn = SequencerTlsConnection::new("http://127.0.0.1:1/rpc").unwrap();
        let too_big = vec![0u8; MAX_SUBMIT_BODY + 1];
        match conn.post_json(&too_big) {
            Err(SubmitError::OversizeBody { len, max }) => {
                assert_eq!(len, MAX_SUBMIT_BODY + 1);
                assert_eq!(max, MAX_SUBMIT_BODY);
            }
            other => panic!("expected OversizeBody, got {other:?}"),
        }
    }

    #[test]
    fn hot_spare_connectors_share_one_client_config_for_resumption() {
        // P4: both connectors must hold the SAME Arc<ClientConfig> so the TLS 1.3 session-resumption
        // cache is shared (a reconnect/second socket resumes a prior session, skipping a round trip).
        // Two independently-`new()`d connectors would have distinct caches.
        let cfg = SequencerTlsConnection::shared_client_config();
        let a = SequencerTlsConnection::with_config(
            "https://arb1-sequencer.arbitrum.io/rpc",
            cfg.clone(),
        )
        .unwrap();
        let b = SequencerTlsConnection::with_config(
            "https://arb1-sequencer.arbitrum.io/rpc",
            cfg.clone(),
        )
        .unwrap();
        assert!(
            Arc::ptr_eq(&a.config, &b.config),
            "hot-spare connectors must share one ClientConfig (shared resumption cache)"
        );
        // `new()` builds an independent config (distinct cache) — the non-shared path.
        let c = SequencerTlsConnection::new("https://arb1-sequencer.arbitrum.io/rpc").unwrap();
        assert!(!Arc::ptr_eq(&a.config, &c.config));
    }

    #[test]
    fn explicit_pin_reports_actual_plain_peer_without_dns_or_tls_label() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local peer");
        let peer = listener.local_addr().expect("local peer address");
        let pin_ip = peer.ip();
        let endpoint = format!("http://must-not-resolve.invalid:{}/rpc", peer.port());
        let config = SequencerTlsConnection::shared_client_config();
        let mut conn =
            SequencerTlsConnection::with_config_and_pin(&endpoint, config, pin_ip).unwrap();

        assert_eq!(conn.pin_ip, Some(pin_ip));
        assert_eq!(conn.peer_addr().unwrap(), None, "disconnected has no peer");
        assert_eq!(
            conn.handshake_kind(),
            None,
            "plain HTTP has no TLS handshake"
        );

        conn.connect().expect("connect through explicit pin");
        let (_accepted, _) = listener.accept().expect("accept pinned connection");
        assert_eq!(conn.peer_addr().unwrap(), Some(peer));
        assert_eq!(
            conn.handshake_kind(),
            None,
            "plain HTTP remains unclassified"
        );
    }

    #[test]
    fn submit_timeout_knobs_default_preserve_todays_values() {
        // P7C: with no env set, the connector must reproduce today's hard-coded 5s connect / 5s
        // write / 10s ack, and TCP_USER_TIMEOUT off. (Env-override paths are exercised elsewhere;
        // this test avoids mutating process env to stay parallel-safe.)
        let conn = SequencerTlsConnection::new("https://arb1-sequencer.arbitrum.io/rpc").unwrap();
        assert_eq!(conn.connect_timeout, Duration::from_secs(5));
        assert_eq!(conn.write_timeout, Duration::from_secs(5));
        assert_eq!(conn.ack_timeout, Duration::from_secs(10));
        assert_eq!(
            conn.tcp_user_timeout_ms, 0,
            "TCP_USER_TIMEOUT off by default"
        );
    }

    #[test]
    fn env_u64_ms_parses_and_falls_back() {
        assert_eq!(env_u64_ms("SUBMIT_NONEXISTENT_KNOB_XYZ", 4242), 4242);
    }

    #[test]
    fn prefaulted_request_buffer_has_full_capacity_and_zero_len() {
        // P8: the request buffer is preallocated + prefaulted to MAX_SUBMIT_REQUEST and left empty,
        // so the hot-path framing never reallocates.
        let conn = SequencerTlsConnection::new("https://arb1-sequencer.arbitrum.io/rpc").unwrap();
        assert_eq!(conn.req_buf.len(), 0);
        assert!(conn.req_buf.capacity() >= MAX_SUBMIT_REQUEST);
        assert!(conn.resp_buf.capacity() >= MAX_SUBMIT_BODY);
    }

    #[test]
    fn find_header_end_works() {
        let buf = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let pos = find_header_end(buf).unwrap();
        assert_eq!(&buf[pos..], b"ok");
    }

    #[test]
    fn invalid_url_rejected() {
        let r = SequencerTlsConnection::new("not-a-url");
        assert!(matches!(r, Err(SubmitError::InvalidUrl(_))));
    }

    /// P7A: the fallback-vs-fail-closed decision is the whole safety property. A partial io_uring
    /// write MUST NOT be followed by a plain `write_all` on the same socket (that double-sends the
    /// request head). Prove the pure classifier here — deterministic, no live ring.
    #[test]
    #[cfg(all(target_os = "linux", feature = "io-uring-submit"))]
    fn uring_dispatch_fails_closed_on_partial_write_only() {
        use crate::io_uring_send::UringSendError;
        // Full write → Done (never write_all again).
        assert!(matches!(classify_uring_send(Ok(200)), UringDispatch::Done));
        // Nothing written → safe to fall back to write_all on the same socket.
        assert!(matches!(
            classify_uring_send(Err(UringSendError {
                written: 0,
                source: std::io::Error::new(std::io::ErrorKind::WouldBlock, "sq full"),
            })),
            UringDispatch::FallBack
        ));
        // Partial write then error → FAIL CLOSED. A write_all here corrupts the stream.
        assert!(matches!(
            classify_uring_send(Err(UringSendError {
                written: 17,
                source: std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset mid-write"),
            })),
            UringDispatch::FailClosed(_)
        ));
    }

    #[test]
    fn definitely_not_written_uring_fallback_rechecks_authority_before_plain_write() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let authority = AtomicBool::new(true);
        assert!(
            authority.load(Ordering::Acquire),
            "the first attempt's pre-write check admitted the request"
        );
        // Fault injection: io_uring returned a definitely-no-bytes fallback, then feed authority
        // changed before the connector's subsequent plain write.
        authority.store(false, Ordering::Release);
        let mut authorize_attempt = || authority.load(Ordering::Acquire);
        let mut authorize: Option<&mut dyn FnMut() -> bool> = Some(&mut authorize_attempt);
        let mut wire = Vec::new();

        let failure = SequencerTlsConnection::write_plain_request(
            &mut wire,
            b"request-must-not-reach-wire",
            true,
            &mut authorize,
        )
        .expect_err("revoked fallback write must fail closed");

        assert!(matches!(failure.err, SubmitError::AuthorityRevoked));
        assert_eq!(failure.delivery, DeliveryState::DefinitelyNotWritten);
        assert!(wire.is_empty(), "revoked fallback emitted request bytes");
    }

    /// End-to-end proof on the laptop: POST an `eth_sendRawTransaction` envelope over a real
    /// (plain-HTTP) socket to a local mock sequencer, and read back the tx-hash response. Exercises
    /// connect → socket tuning → build_http_request → write_and_read → response parse.
    #[test]
    fn post_json_roundtrips_against_local_mock() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            // Read the request (headers + body); we only need to consume it before replying.
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let body = br#"{"jsonrpc":"2.0","id":1,"result":"0xabc123"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            s.write_all(resp.as_bytes()).ok();
            s.write_all(body).ok();
            s.flush().ok();
        });

        let mut conn =
            SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).expect("new");
        let req =
            br#"{"jsonrpc":"2.0","id":1,"method":"eth_sendRawTransaction","params":["0x02f8..."]}"#;
        let resp = conn.post_json(req).expect("post_json");
        let s = std::str::from_utf8(&resp).unwrap();
        assert!(s.contains("0xabc123"), "expected tx-hash result, got: {s}");
        server.join().ok();
    }

    /// B1-wired: exercises the `write_and_read` path on `Conn::Plain` — when io_uring is
    /// available on the host (modern Linux), the actual write goes through `IoUringSendQueue`;
    /// on a sandbox without uring it transparently falls back to `write_all`. Either way the
    /// HTTP round-trip must succeed end-to-end. This is the live-codepath proof that the
    /// `tls::write_and_read` integration site doesn't drop / mangle bytes regardless of which
    /// underlying writer engages.
    #[test]
    #[cfg(all(target_os = "linux", feature = "io-uring-submit"))]
    fn write_and_read_succeeds_with_io_uring_path_when_available() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // Mirror back a fixed JSON response so the client can verify byte-for-byte.
            let mut buf = vec![0u8; 8192];
            let _ = s.read(&mut buf);
            let body = br#"{"jsonrpc":"2.0","id":42,"result":"0xio_uring_path_ok"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
            s.write_all(body).unwrap();
            s.flush().ok();
        });

        let mut conn =
            SequencerTlsConnection::new(&format!("http://127.0.0.1:{port}/rpc")).unwrap();
        let req = br#"{"jsonrpc":"2.0","id":42,"method":"eth_chainId","params":[]}"#;
        let resp = conn
            .post_json(req)
            .expect("post_json via io_uring-or-fallback");
        let s = std::str::from_utf8(&resp).unwrap();
        assert!(
            s.contains("0xio_uring_path_ok"),
            "io_uring fast-path wiring lost or mangled the response: {s}",
        );
        server.join().ok();
    }
}
