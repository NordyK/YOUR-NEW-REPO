//! Offchain Labs feed relay client.
//!
//! Connects to the local `nitro` feed relay (default `ws://127.0.0.1:9642/feed`) which
//! re-broadcasts the uncompressed sequencer feed. Each WebSocket text frame is one
//! `BroadcastMessage` JSON envelope containing an array of `messages` (each a
//! `BroadcastFeedMessage`).
//!
//! Reference: Arbitrum docs §2.1
//! * <https://docs.arbitrum.io/run-arbitrum-node/sequencer/read-sequencer-feed>
//! * <https://docs.arbitrum.io/run-arbitrum-node/run-feed-relay>
//!
//! The client:
//! * Establishes the WebSocket with `tungstenite::client::connect_with_config`.
//! * Reads frames in a tight loop and hands them to a caller-supplied closure.
//! * Tracks the last seen `sequenceNumber` per envelope; emits sequence-gap callbacks.
//! * Reconnects on disconnect with exponential backoff capped at 30 s.
//!
//! The client never owns the parser; the caller hands each frame straight to
//! `engine_feed::decode_envelope`. The `WsRelayClient::run` method returns only when the
//! `shutdown` flag flips to `true`.

use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::{debug, info, warn};
use tungstenite::client::IntoClientRequest;
use tungstenite::error::Error as WsError;
use tungstenite::handshake::client::Request;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use url::Url;

/// Receive-timestamp observations accumulated across the actual socket reads since the previous
/// delivered feed callback. Counts are per successful, non-empty `recvmsg`, not per WebSocket
/// frame. `software_ns`/`hardware_ns` belong to the latest such read; they are an attribution
/// boundary only because TLS, WebSocket buffering, TCP segmentation, and GRO prevent a guaranteed
/// one-read-to-one-frame mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayTimestampSnapshot {
    /// Successful, non-empty timestamp-instrumented socket reads in this delta.
    pub reads: u64,
    /// Reads carrying a kernel software timestamp (`SCM_TIMESTAMPING` slot `ts[0]`).
    pub software: u64,
    /// Reads carrying a NIC raw-hardware timestamp (`SCM_TIMESTAMPING` slot `ts[2]`).
    pub hardware: u64,
    /// Reads carrying neither timestamp.
    pub missing: u64,
    /// Kernel software timestamp on the latest read in this delta, if present.
    pub software_ns: Option<u64>,
    /// NIC raw-hardware timestamp on the latest read in this delta, if present.
    pub hardware_ns: Option<u64>,
}

#[derive(Debug, Default)]
struct TimestampCounters {
    reads: AtomicU64,
    software: AtomicU64,
    hardware: AtomicU64,
    missing: AtomicU64,
    software_ns: AtomicU64,
    hardware_ns: AtomicU64,
}

impl TimestampCounters {
    fn record(&self, software_ns: Option<u64>, hardware_ns: Option<u64>) {
        self.software_ns
            .store(software_ns.unwrap_or(0), Ordering::Relaxed);
        self.hardware_ns
            .store(hardware_ns.unwrap_or(0), Ordering::Relaxed);
        self.reads.fetch_add(1, Ordering::Relaxed);
        if software_ns.is_some() {
            self.software.fetch_add(1, Ordering::Relaxed);
        }
        if hardware_ns.is_some() {
            self.hardware.fetch_add(1, Ordering::Relaxed);
        }
        if software_ns.is_none() && hardware_ns.is_none() {
            self.missing.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn take(&self) -> RelayTimestampSnapshot {
        RelayTimestampSnapshot {
            reads: self.reads.swap(0, Ordering::Relaxed),
            software: self.software.swap(0, Ordering::Relaxed),
            hardware: self.hardware.swap(0, Ordering::Relaxed),
            missing: self.missing.swap(0, Ordering::Relaxed),
            software_ns: match self.software_ns.swap(0, Ordering::Relaxed) {
                0 => None,
                ns => Some(ns),
            },
            hardware_ns: match self.hardware_ns.swap(0, Ordering::Relaxed) {
                0 => None,
                ns => Some(ns),
            },
        }
    }
}

/// TCP stream adapter that preserves the ordinary `Read`/`Write` byte stream while optionally
/// collecting split kernel-software and NIC-hardware receive timestamps below rustls/tungstenite.
/// With `NETWORK_FINE=0` it takes one predictable branch and delegates directly to `TcpStream`.
#[derive(Debug)]
pub struct TimestampedTcpStream {
    inner: TcpStream,
    network_fine: bool,
    timestamps: Arc<TimestampCounters>,
}

impl TimestampedTcpStream {
    fn new(inner: TcpStream, network_fine: bool, timestamps: Arc<TimestampCounters>) -> Self {
        Self {
            inner,
            network_fine,
            timestamps,
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.inner.set_write_timeout(timeout)
    }

    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }
}

impl Read for TimestampedTcpStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.network_fine {
            return self.inner.read(buf);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let (bytes, software_ns, hardware_ns) =
                engine_sysopt::recvmsg_timestamps_split(self.inner.as_raw_fd(), buf)?;
            if bytes > 0 {
                self.timestamps.record(software_ns, hardware_ns);
            }
            Ok(bytes)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.inner.read(buf)
        }
    }
}

impl Write for TimestampedTcpStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        self.inner.write_vectored(bufs)
    }
}

impl std::os::unix::io::AsRawFd for TimestampedTcpStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        std::os::unix::io::AsRawFd::as_raw_fd(&self.inner)
    }
}

/// WebSocket stream that may be plain TCP (`ws://`) or TLS (`wss://`). Tungstenite and rustls see
/// exactly the same ordered bytes; the adapter only changes which safe receive syscall supplies
/// them when `NETWORK_FINE=1`.
pub type RelayStream = MaybeTlsStream<TimestampedTcpStream>;

/// Default Offchain Labs nitro relay endpoint per the relay docs.
pub const DEFAULT_LOCAL_RELAY_WS: &str = "ws://127.0.0.1:9642/feed";

/// Public Arbitrum One feed endpoint (the relay's upstream — the engine never speaks here
/// directly per plan §2.1 single-relay-per-DC rule, but exposing the constant lets the
/// relay binary itself reference it).
pub const PUBLIC_ARB_ONE_FEED_WS: &str = "wss://arb1-feed.arbitrum.io/feed";

/// Relay client errors.
#[derive(Debug, Error)]
pub enum RelayError {
    /// URL parsing failed.
    #[error("invalid relay url: {0}")]
    InvalidUrl(String),
    /// Underlying WebSocket error.
    #[error("websocket error: {0}")]
    WebSocket(#[from] WsError),
    /// Generic IO error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A state-bearing relay discontinuity that must revoke downstream submission authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedDegradationReason {
    /// A single-relay connection ended or failed before the driver would reconnect.
    SingleRelayDisconnect,
    /// A single-relay frame started after the next expected sequencer feed number.
    SequenceGap,
    /// Dual-relay de-duplication rejected a frame that skipped the next globally required number.
    DualRelayForwardGap,
    /// Every previously installed dual-relay socket was lost before a replacement became ready.
    DualRelayUnavailable,
    /// Sequence numbers inside one relay frame were not strictly contiguous.
    IntraFrameSequenceGap,
    /// A dual-relay frame was structurally ambiguous and therefore could not be safely de-duplicated.
    DualRelayAmbiguousFrame,
    /// A frame contained a sequence-number key whose value could not be parsed.
    MalformedSequenceFrame,
    /// Two relay legs delivered the SAME feed cursor with DIFFERENT canonical payload bytes
    /// (FS-07). This is a sticky conflict: prediction/submission must stop until an exact
    /// canonical reconciliation — later well-formed frames must NOT clear it.
    FeedPayloadConflict,
}

impl FeedDegradationReason {
    /// Stable reason label for fail-closed submission/campaign telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleRelayDisconnect => "single_relay_disconnect",
            Self::SequenceGap => "ws_sequence_gap",
            Self::DualRelayForwardGap => "dual_relay_forward_gap",
            Self::DualRelayUnavailable => "dual_relay_unavailable",
            Self::IntraFrameSequenceGap => "intra_frame_sequence_gap",
            Self::DualRelayAmbiguousFrame => "dual_relay_ambiguous_frame",
            Self::MalformedSequenceFrame => "malformed_sequence_frame",
            Self::FeedPayloadConflict => "feed_payload_conflict",
        }
    }
}

type DegradationCallback = Arc<dyn Fn(FeedDegradationReason) + Send + Sync>;

/// Per-frame callback context.
pub struct RelayCallback<'a> {
    /// 64 KiB scratch buffer holding the current text frame as raw bytes (no `String`).
    pub frame: &'a [u8],
    /// Capture timestamp in **wall-clock** (UNIX-epoch) nanoseconds, taken immediately after
    /// `read_message`. Wall-clock — NOT monotonic — on purpose: it is retained for correlation with
    /// the sequencer's wall-clock header `timestamp`. It must not be used for same-host stage
    /// durations; those use a separate `CLOCK_MONOTONIC_RAW` stamp in the engine pipeline.
    pub recv_ns: u64,
    /// Split timestamp values and per-underlying-read coverage counters since the previous
    /// delivered callback. This is not guaranteed to map one-to-one to `frame`; see
    /// [`RelayTimestampSnapshot`]. All fields are zero/`None` when `NETWORK_FINE` is disabled.
    pub timestamps: RelayTimestampSnapshot,
}

/// Public client.
pub struct WsRelayClient {
    url: Url,
    /// Shared TLS config — built ONCE and reused across reconnects so rustls' session-resumption
    /// store (tickets / TLS1.3 PSK) is warm: a reconnect skips the full 2-RTT handshake. A fresh
    /// config per connect (the old `tungstenite::connect` default) would defeat resumption.
    tls_config: std::sync::Arc<rustls::ClientConfig>,
    /// Optional pinned upstream IP (e.g. from the RTT-probe of the feed's anycast set), overriding
    /// DNS so we don't drift to a far PoP. Set via `FEED_PIN_IP`.
    pin_ip: Option<std::net::IpAddr>,
    /// Default-off kernel/NIC receive timestamp collection, read once at construction.
    network_fine: bool,
    /// Per-physical-relay receive counters shared with the socket adapter across reconnects.
    timestamp_counters: Arc<TimestampCounters>,
    /// Optional bounded blocking-read poll used by owners that require prompt graceful shutdown.
    read_poll_timeout: Option<Duration>,
    /// Optional owner hook that linearizes a detected feed gap with submission-authority revocation.
    degradation_callback: Option<DegradationCallback>,
    /// Last sequence number processed across all callbacks.
    pub last_seq: AtomicU64,
    /// Sequence-gap counter.
    pub sequence_gaps: AtomicU64,
    /// Reconnect counter.
    pub reconnects: AtomicU64,
    /// Dual-relay frames containing both already-complete and new feed messages.
    pub partial_overlaps: AtomicU64,
    /// Whole `BroadcastFeedMessage` objects skipped by dual-relay de-duplication.
    pub duplicate_message_total: AtomicU64,
}

impl WsRelayClient {
    /// Construct a client targeted at `endpoint`. Use [`DEFAULT_LOCAL_RELAY_WS`] for the
    /// canonical Offchain Labs relay endpoint.
    pub fn new(endpoint: &str) -> Result<Self, RelayError> {
        // Install the process CryptoProvider BEFORE `ClientConfig::builder()` below: with both
        // `aws-lc-rs` and `ring` compiled into the workspace, rustls 0.23 cannot auto-select and
        // the builder PANICS. Production binaries install it in main(), but any consumer that
        // reaches `new()` without that (the pipeline-boot e2e tests did) died on the rx thread —
        // observed 2026-07-08 as all four stub-relay tests wedging. Idempotent: a second install
        // returns Err and is ignored.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let url = Url::parse(endpoint).map_err(|_| RelayError::InvalidUrl(endpoint.into()))?;
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // Default ClientConfig has resumption ENABLED (ClientSessionMemoryCache + ticket support);
        // the win is reusing this one Arc across reconnects so the store persists.
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let pin_ip = std::env::var("FEED_PIN_IP")
            .ok()
            .and_then(|s| s.trim().parse().ok());
        let network_fine = network_fine_from_value(std::env::var("NETWORK_FINE").ok().as_deref());
        Ok(Self {
            url,
            tls_config: std::sync::Arc::new(tls_config),
            pin_ip,
            network_fine,
            timestamp_counters: Arc::new(TimestampCounters::default()),
            read_poll_timeout: None,
            degradation_callback: None,
            last_seq: AtomicU64::new(0),
            sequence_gaps: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            partial_overlaps: AtomicU64::new(0),
            duplicate_message_total: AtomicU64::new(0),
        })
    }

    /// Bound each blocking socket read so `shutdown` is observed even when an upstream connection
    /// remains open but silent. `None` is the default and preserves the existing feed path.
    pub fn with_read_poll_timeout(mut self, timeout: Duration) -> Self {
        self.read_poll_timeout = Some(timeout);
        self
    }

    /// Install the owner callback that revokes submission authority on a state-bearing feed gap.
    ///
    /// The callback runs synchronously before a single-relay gap-bearing frame is dispatched,
    /// before any single-relay reconnect attempt, and before a dual-relay forward-gap rejection
    /// returns. It is never called on the contiguous fast path or for loss of only one dual-relay
    /// leg while the global de-duplicated stream remains continuous.
    pub fn with_degradation_callback(
        mut self,
        callback: Arc<dyn Fn(FeedDegradationReason) + Send + Sync>,
    ) -> Self {
        self.degradation_callback = Some(callback);
        self
    }

    #[inline]
    fn signal_degradation(&self, reason: FeedDegradationReason) {
        if let Some(callback) = &self.degradation_callback {
            callback(reason);
        }
    }

    /// Block in a read-and-dispatch loop. Returns when `shutdown` is set.
    pub fn run<F>(&self, shutdown: &Arc<AtomicBool>, mut on_frame: F) -> Result<(), RelayError>
    where
        F: FnMut(RelayCallback<'_>),
    {
        // Defense-in-depth re-install (the authoritative one runs in `new()` before the
        // ClientConfig build); no-op because install_default errs once a provider is set.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut backoff = Duration::from_millis(100);
        // One verifier spans every reconnect. A fresh dedup table per socket would reopen the
        // same-cursor identity hole exactly when a relay session is replaced.
        let mut dedup = SeqDedup::default();
        // Cap at 2s: the production relay is on localhost, so a reconnect is instant — a long
        // (30s) cap is a needless blind window where we miss feed frames. The dual-relay standby
        // (§3.3) covers the rare longer upstream outage. (Report S1 #19.)
        let max_backoff = Duration::from_secs(2);

        while !shutdown.load(Ordering::Acquire) {
            match self.connect_once() {
                Ok(mut ws) => {
                    info!(endpoint = %self.url, "connected to relay");
                    // Reset backoff on successful connect.
                    backoff = Duration::from_millis(100);
                    let fd = Self::plain_tcp_fd(&ws);
                    if let Err(e) = self.read_loop(&mut ws, shutdown, &mut dedup, &mut on_frame, fd) {
                        warn!(error = %e, "relay read loop exited");
                    }
                    // Clean close attempt.
                    let _ = ws.close(None);
                }
                Err(e) => {
                    warn!(error = %e, backoff_ms = backoff.as_millis(), "relay connect failed");
                }
            }
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            // A reconnect cannot prove that no state-bearing frames were lost. Revoke authority
            // synchronously before incrementing the reconnect counter or sleeping/connecting.
            // Dual mode intentionally does not use this path: one redundant leg may reconnect
            // while the other keeps the global de-duplicated stream continuous.
            self.signal_degradation(FeedDegradationReason::SingleRelayDisconnect);
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            self.reconnects.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(max_backoff);
        }
        Ok(())
    }

    fn connect_once(&self) -> Result<WebSocket<RelayStream>, RelayError> {
        self.connect_once_with_timeouts(Duration::from_secs(5), None)
    }

    /// Connect for the optional dual-relay worker. The TCP connect and WebSocket handshake phases
    /// are bounded; DNS and lowest-RTT address selection remain blocking but run only on this
    /// housekeeping worker, never the healthy-leg poller. The single-relay path deliberately
    /// continues to use [`Self::connect_once`] unchanged.
    fn connect_once_dual(&self) -> Result<WebSocket<RelayStream>, RelayError> {
        self.connect_once_with_timeouts(Duration::from_millis(500), Some(Duration::from_secs(1)))
    }

    fn connect_once_with_timeouts(
        &self,
        connect_timeout: Duration,
        handshake_timeout: Option<Duration>,
    ) -> Result<WebSocket<RelayStream>, RelayError> {
        // We OWN the TcpStream (resolve/connect ourselves) so we can (a) optionally pin the upstream
        // IP — bypassing DNS/anycast drift — and (b) apply the low-latency socket options BEFORE the
        // TLS/WS handshake, on BOTH the plain (`ws://` local relay) AND the TLS (`wss://` public feed)
        // path. The old `tungstenite::connect` built a fresh rustls config each time (no resumption)
        // and left the TLS socket untuned — the S1 gap this fixes.
        use std::os::unix::io::AsRawFd;
        let request: Request = self.url.as_str().into_client_request()?;
        let host = self
            .url
            .host_str()
            .ok_or_else(|| RelayError::InvalidUrl(self.url.to_string()))?;
        let tls = self.url.scheme() == "wss";
        let port = self
            .url
            .port_or_known_default()
            .unwrap_or(if tls { 443 } else { 80 });

        // Resolve + choose the upstream IP. `FEED_PIN_IP` is a manual override; otherwise we RTT-probe
        // ALL resolved IPs and pick the lowest — so the choice **self-adapts to wherever we run**
        // (laptop vs the AWS box). This matters because the feed is **Cloudflare anycast**: the best
        // IP from one location is NOT the best from another, so a statically-pinned IP would be wrong
        // off-box. Probing at connect (rare: startup + reconnects) costs a few ms once, not per-frame.
        let addr: std::net::SocketAddr = match self.pin_ip {
            Some(ip) => std::net::SocketAddr::new(ip, port),
            None => pick_best_addr(host, port)
                .ok_or_else(|| RelayError::InvalidUrl(self.url.to_string()))?,
        };
        let tcp = TcpStream::connect_timeout(&addr, connect_timeout).map_err(RelayError::Io)?;
        // §4.3 / S1: tune the feed socket for lowest RX latency + jitter (BOTH transports now):
        // TCP_NODELAY (no Nagle on small WS frames), SO_BUSY_POLL=50µs (spin the RX ring vs IRQ-sleep,
        // −3–8µs p95 on AWS), SO_RCVBUF=8MiB (absorb feed bursts; clamps to net.core.rmem_max).
        let _ = tcp.set_nodelay(true);
        let fd = tcp.as_raw_fd();
        let _ = engine_sysopt::set_so_busy_poll(fd, 50);
        let _ = engine_sysopt::set_so_rcvbuf(fd, 8 * 1024 * 1024);
        // Timestamp generation and recvmsg ancillary parsing are both default-off: when disabled,
        // the adapter below performs an ordinary TcpStream::read and the kernel does no stamp work.
        if self.network_fine {
            if let Some(iface) = engine_sysopt::default_route_iface() {
                let hw_rx_configured =
                    engine_sysopt::configure_hardware_rx_timestamping(fd, &iface);
                debug!(
                    interface = %iface,
                    hw_rx_configured,
                    "feed NIC hardware receive timestamp configuration"
                );
            }
            // This independently retains software SCM_TIMESTAMPING as the fallback. The sysopt
            // helper adds hardware flags only when SIOCGHWTSTAMP can prove NIC configuration.
            let _ = engine_sysopt::set_so_timestamping(fd);
        }

        // Dual mode performs the handshake on a housekeeping thread. Bound that blocking phase so
        // shutdown can always join the worker even if a peer accepts TCP and then sends no HTTP
        // upgrade response. Clear the deadline after the ready socket is established; the poller
        // uses non-blocking I/O. The single-relay connector passes `None`, preserving its behavior.
        if let Some(timeout) = handshake_timeout {
            tcp.set_read_timeout(Some(timeout))?;
            tcp.set_write_timeout(Some(timeout))?;
        }
        let tcp =
            TimestampedTcpStream::new(tcp, self.network_fine, self.timestamp_counters.clone());

        let to_io =
            |e: &dyn std::fmt::Display| RelayError::Io(std::io::Error::other(e.to_string()));
        let (ws, _resp) = if tls {
            // TLS+WS over our tuned socket, reusing the resumption-enabled shared config.
            tungstenite::client_tls_with_config(
                request,
                tcp,
                None,
                Some(tungstenite::Connector::Rustls(self.tls_config.clone())),
            )
            .map_err(|e| to_io(&e))?
        } else {
            // Plain WS over our tuned socket (local relay).
            tungstenite::client(request, MaybeTlsStream::Plain(tcp)).map_err(|e| to_io(&e))?
        };
        if handshake_timeout.is_some() || self.read_poll_timeout.is_some() {
            Self::set_io_timeouts(&ws, self.read_poll_timeout)?;
        }
        // HTTP Upgrade and TLS handshake reads are not feed observations. Discard their delta so
        // the first RelayCallback starts its coverage denominator at the first post-handshake read.
        let _ = self.timestamp_counters.take();
        Ok(ws)
    }

    fn set_io_timeouts(
        ws: &WebSocket<RelayStream>,
        timeout: Option<Duration>,
    ) -> std::io::Result<()> {
        let tcp = match ws.get_ref() {
            MaybeTlsStream::Plain(tcp) => tcp,
            MaybeTlsStream::Rustls(stream) => stream.get_ref(),
            _ => return Ok(()),
        };
        tcp.set_read_timeout(timeout)?;
        tcp.set_write_timeout(timeout)
    }

    /// Raw fd of the underlying TCP socket, for either transport, so `TCP_QUICKACK` can be re-armed
    /// per recv on the TLS (`wss://` feed) path as well as the plain (`ws://` relay) path. `None`
    /// only for a transport variant without a backing TcpStream.
    fn plain_tcp_fd(ws: &WebSocket<RelayStream>) -> Option<std::os::unix::io::RawFd> {
        use std::os::unix::io::AsRawFd;
        match ws.get_ref() {
            MaybeTlsStream::Plain(tcp) => Some(tcp.as_raw_fd()),
            MaybeTlsStream::Rustls(s) => Some(s.get_ref().as_raw_fd()),
            _ => None,
        }
    }

    fn read_loop<S, F>(
        &self,
        ws: &mut WebSocket<S>,
        shutdown: &Arc<AtomicBool>,
        dedup: &mut SeqDedup,
        on_frame: &mut F,
        fd: Option<std::os::unix::io::RawFd>,
    ) -> Result<(), RelayError>
    where
        S: Read + Write,
        F: FnMut(RelayCallback<'_>),
    {
        let mut last_ping = Instant::now();
        let ping_interval = Duration::from_secs(20);
        while !shutdown.load(Ordering::Acquire) {
            // Send periodic pings so the relay's keepalive stays warm.
            if last_ping.elapsed() >= ping_interval {
                if let Err(e) = ws.send(Message::Ping(Vec::new())) {
                    debug!(error = %e, "ping failed");
                    return Err(RelayError::WebSocket(e));
                }
                last_ping = Instant::now();
            }
            match ws.read() {
                Ok(Message::Text(text)) => {
                    let recv_ns = now_ns();
                    // Re-arm TCP_QUICKACK (kernel clears it after each recv) so our ACK to the
                    // relay isn't delayed. Best-effort; only on the plain-TCP local relay.
                    if let Some(fd) = fd {
                        engine_sysopt::set_tcp_quickack(fd);
                    }
                    self.dispatch_single_frame(dedup, text.as_bytes(), recv_ns, on_frame);
                }
                Ok(Message::Binary(buf)) => {
                    let recv_ns = now_ns();
                    if let Some(fd) = fd {
                        engine_sysopt::set_tcp_quickack(fd);
                    }
                    self.dispatch_single_frame(dedup, &buf, recv_ns, on_frame);
                }
                Ok(Message::Ping(payload)) => {
                    ws.send(Message::Pong(payload))?;
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => {
                    info!("relay closed connection");
                    return Ok(());
                }
                Ok(Message::Frame(_)) => {}
                Err(WsError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    // Idle; loop and check shutdown.
                    std::thread::yield_now();
                }
                Err(e) => return Err(RelayError::WebSocket(e)),
            }
        }
        Ok(())
    }

    fn dispatch_single_frame<F>(
        &self,
        dedup: &mut SeqDedup,
        frame: &[u8],
        recv_ns: u64,
        on_frame: &mut F,
    )
    where
        F: FnMut(RelayCallback<'_>),
    {
        // Physical telemetry remains per-relay, but a detected discontinuity is a synchronous
        // refusal in single topology too: the frame must never reach the owner after the latch.
        let physical_reason = self.track_sequence(frame);
        match dedup.plan_frame(frame) {
            Ok(FramePlan::Forward {
                frame,
                last_sequence,
                duplicate_messages,
                partial_overlap,
            }) if physical_reason.is_none() => {
                self.duplicate_message_total
                    .fetch_add(duplicate_messages, Ordering::Relaxed);
                if partial_overlap {
                    self.partial_overlaps.fetch_add(1, Ordering::Relaxed);
                }
                on_frame(RelayCallback {
                    frame: frame.as_ref(),
                    recv_ns,
                    timestamps: self.timestamp_counters.take(),
                });
                dedup.complete(last_sequence);
            }
            Ok(FramePlan::Forward { .. }) => {
                let reason = physical_reason.expect("physical reason guards this arm");
                self.signal_degradation(reason);
            }
            Ok(FramePlan::Drop { duplicate_messages }) => {
                self.duplicate_message_total
                    .fetch_add(duplicate_messages, Ordering::Relaxed);
                if let Some(reason) = physical_reason {
                    self.signal_degradation(reason);
                }
            }
            Err(reason) => {
                let reason = physical_reason.unwrap_or_else(|| match reason {
                    FramePlanError::ForwardSequenceGap => FeedDegradationReason::SequenceGap,
                    _ => reason.degradation_reason(),
                });
                self.signal_degradation(reason);
                warn!(endpoint = %self.url, reason = reason.as_str(), "rejecting single-relay frame");
            }
        }
    }

    /// Consume TWO independent relays from a SINGLE thread (preserving the single-producer
    /// downstream contract) and de-duplicate by `sequenceNumber`: the first relay to deliver a
    /// given sequence range wins, the other is hot standby that also covers a stall/gap. This turns
    /// a relay or upstream blip into a non-event instead of a feed gap. Honors "≤1 redundant feed
    /// connection per relay" — these are two *independent* relays, not two inputs to one relay.
    /// Non-blocking reads on both; `on_frame` fires only for frames carrying NEW sequences.
    pub fn run_dual<F>(
        primary: &WsRelayClient,
        secondary: &WsRelayClient,
        shutdown: &Arc<AtomicBool>,
        mut on_frame: F,
    ) -> Result<(), RelayError>
    where
        F: FnMut(RelayCallback<'_>),
    {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        std::thread::scope(|scope| {
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (primary_cmd_tx, primary_cmd_rx) = std::sync::mpsc::channel();
            let (secondary_cmd_tx, secondary_cmd_rx) = std::sync::mpsc::channel();

            let primary_ready_tx = ready_tx.clone();
            let primary_worker = scope.spawn(move || {
                connector_worker(
                    RelayLeg::Primary,
                    primary,
                    shutdown,
                    primary_cmd_rx,
                    primary_ready_tx,
                );
            });
            let secondary_worker = scope.spawn(move || {
                connector_worker(
                    RelayLeg::Secondary,
                    secondary,
                    shutdown,
                    secondary_cmd_rx,
                    ready_tx,
                );
            });

            let mut a = None;
            let mut b = None;
            let mut dedup = SeqDedup::default();
            while !shutdown.load(Ordering::Acquire) {
                while let Ok(ready) = ready_rx.try_recv() {
                    install_ready_socket(ready, &mut a, &mut b);
                }

                let available_before = usize::from(a.is_some()) + usize::from(b.is_some());
                let progressed_a =
                    poll_dual_slot(&mut a, primary, &primary_cmd_tx, &mut dedup, &mut on_frame);
                let progressed_b = poll_dual_slot(
                    &mut b,
                    secondary,
                    &secondary_cmd_tx,
                    &mut dedup,
                    &mut on_frame,
                );
                let available_after = usize::from(a.is_some()) + usize::from(b.is_some());
                let became_unavailable = signal_dual_unavailable_on_transition(
                    available_before,
                    available_after,
                    primary,
                    secondary,
                );
                if became_unavailable && shutdown.load(Ordering::Acquire) {
                    // The owner may synchronously stop the driver (the sidecar does this after
                    // closing the engine UDS). Never enter the idle wait after that revocation.
                    continue;
                }

                if !(progressed_a || progressed_b) {
                    // Wake the instant EITHER socket has a frame — NEVER sleep a fixed interval (a
                    // fixed sleep folds that interval into feed latency). Blocking connection and
                    // handshake work lives exclusively on the connector workers above. poll(2)'s
                    // 2ms timeout bounds only idle shutdown/ready-socket adoption cadence and is
                    // below the required 5ms healthy-leg pause floor.
                    let mut fds: [std::os::unix::io::RawFd; 2] = [0; 2];
                    let mut nfds = 0usize;
                    if let Some(ws) = a.as_ref() {
                        if let Some(fd) = Self::plain_tcp_fd(ws) {
                            fds[nfds] = fd;
                            nfds += 1;
                        }
                    }
                    if let Some(ws) = b.as_ref() {
                        if let Some(fd) = Self::plain_tcp_fd(ws) {
                            fds[nfds] = fd;
                            nfds += 1;
                        }
                    }
                    if nfds > 0 {
                        engine_sysopt::poll_readable(&fds[..nfds], 2);
                    } else if let Ok(ready) = ready_rx.recv_timeout(Duration::from_millis(2)) {
                        install_ready_socket(ready, &mut a, &mut b);
                    }
                }
            }

            if let Some(mut ws) = a.take() {
                let _ = ws.close(None);
            }
            if let Some(mut ws) = b.take() {
                let _ = ws.close(None);
            }
            let _ = primary_cmd_tx.send(ConnectorCommand::Shutdown);
            let _ = secondary_cmd_tx.send(ConnectorCommand::Shutdown);
            let _ = primary_worker.join();
            let _ = secondary_worker.join();
        });
        Ok(())
    }

    fn track_sequence(&self, frame: &[u8]) -> Option<FeedDegradationReason> {
        // A relay frame batches many `BroadcastFeedMessage` entries, each with its own
        // `sequenceNumber`. The pipeline processes every message in the frame (see
        // `iter_envelopes`), so the only real loss is a discontinuity *between* frames.
        // Compare this frame's FIRST sequence number against the previous frame's LAST —
        // comparing first-to-first would report the in-frame batch stride as a phantom gap.
        let scan = match scan_frame_sequence_range(frame) {
            Ok(Some(scan)) => scan,
            Ok(None) => return None,
            Err(()) => {
                self.sequence_gaps.fetch_add(1, Ordering::Relaxed);
                warn!("sequencer feed frame contains an invalid sequenceNumber");
                return Some(FeedDegradationReason::MalformedSequenceFrame);
            }
        };
        let first = scan.first;
        let high_water = scan.high_water;
        if scan.missing > 0 {
            self.sequence_gaps
                .fetch_add(scan.missing, Ordering::Relaxed);
            warn!(
                first,
                last = scan.last,
                missing = scan.missing,
                "sequencer feed intra-frame gap detected"
            );
        }
        // A lagging dual-relay leg may replay an old range after reconnect. Never let that replay
        // move the physical relay's high-water backwards, otherwise the next current frame creates
        // a phantom gap. The compare/exchange also keeps this correct for observational readers.
        let mut prev_last = self.last_seq.load(Ordering::Relaxed);
        loop {
            if high_water <= prev_last {
                return (scan.missing > 0).then_some(FeedDegradationReason::IntraFrameSequenceGap);
            }
            match self.last_seq.compare_exchange_weak(
                prev_last,
                high_water,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => prev_last = observed,
            }
        }
        if prev_last > 0 && first > prev_last.saturating_add(1) {
            let gap = first - prev_last - 1;
            self.sequence_gaps.fetch_add(gap, Ordering::Relaxed);
            warn!(
                prev_last,
                first,
                last = scan.last,
                gap,
                "sequencer feed gap detected"
            );
            Some(if scan.missing > 0 {
                FeedDegradationReason::IntraFrameSequenceGap
            } else {
                FeedDegradationReason::SequenceGap
            })
        } else if scan.missing > 0 {
            Some(FeedDegradationReason::IntraFrameSequenceGap)
        } else {
            None
        }
    }
}

fn signal_dual_unavailable_on_transition(
    available_before: usize,
    available_after: usize,
    primary: &WsRelayClient,
    secondary: &WsRelayClient,
) -> bool {
    if available_before == 0 || available_after != 0 {
        return false;
    }
    // Both clients normally carry the same owner callback. Invoke exactly one so a single physical
    // availability transition produces one authority-epoch bump; fall back to secondary for API
    // robustness if only that client was configured.
    if let Some(callback) = primary
        .degradation_callback
        .as_ref()
        .or(secondary.degradation_callback.as_ref())
    {
        callback(FeedDegradationReason::DualRelayUnavailable);
    }
    true
}

#[derive(Clone, Copy, Debug)]
enum RelayLeg {
    Primary,
    Secondary,
}

enum ConnectorCommand {
    Reconnect,
    Shutdown,
}

struct ReadySocket {
    leg: RelayLeg,
    ws: WebSocket<RelayStream>,
}

/// Own all blocking connect/handshake/backoff work for one physical relay. A ready, non-blocking
/// socket is moved to the single poller; the worker does not touch it again until explicitly asked
/// for a replacement. This preserves the downstream single-producer contract.
fn connector_worker(
    leg: RelayLeg,
    client: &WsRelayClient,
    shutdown: &Arc<AtomicBool>,
    commands: std::sync::mpsc::Receiver<ConnectorCommand>,
    ready: std::sync::mpsc::Sender<ReadySocket>,
) {
    let mut initial_attempt = true;
    let mut backoff = Duration::from_millis(100);
    let max_backoff = Duration::from_secs(2);

    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        if !initial_attempt {
            client.reconnects.fetch_add(1, Ordering::Relaxed);
        }
        initial_attempt = false;

        match client.connect_once_dual() {
            Ok(mut ws) => {
                if let Err(error) = set_nonblocking(&ws) {
                    warn!(endpoint = %client.url, %error, "failed to make relay socket non-blocking");
                } else {
                    if shutdown.load(Ordering::Acquire) {
                        let _ = ws.close(None);
                        return;
                    }
                    info!(endpoint = %client.url, ?leg, "dual-relay connector ready");
                    if ready.send(ReadySocket { leg, ws }).is_err() {
                        return;
                    }
                    backoff = Duration::from_millis(100);
                    match commands.recv() {
                        Ok(ConnectorCommand::Reconnect) => continue,
                        Ok(ConnectorCommand::Shutdown) | Err(_) => return,
                    }
                }
            }
            Err(error) => {
                warn!(endpoint = %client.url, ?leg, %error, "dual-relay connect failed");
            }
        }

        match commands.recv_timeout(backoff) {
            Ok(ConnectorCommand::Shutdown)
            | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return;
            }
            Ok(ConnectorCommand::Reconnect) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        backoff = (backoff * 2).min(max_backoff);
    }
}

fn install_ready_socket(
    mut ready: ReadySocket,
    primary: &mut Option<WebSocket<RelayStream>>,
    secondary: &mut Option<WebSocket<RelayStream>>,
) {
    let slot = match ready.leg {
        RelayLeg::Primary => primary,
        RelayLeg::Secondary => secondary,
    };
    if slot.is_none() {
        *slot = Some(ready.ws);
    } else {
        // A worker sends at most one socket per request. Fail closed if that invariant is ever
        // violated: retain the poller's current live socket and discard the unexpected replacement.
        warn!(leg = ?ready.leg, "discarding unexpected duplicate ready relay socket");
        let _ = ready.ws.close(None);
    }
}

fn poll_dual_slot<F>(
    slot: &mut Option<WebSocket<RelayStream>>,
    client: &WsRelayClient,
    commands: &std::sync::mpsc::Sender<ConnectorCommand>,
    dedup: &mut SeqDedup,
    on_frame: &mut F,
) -> bool
where
    F: FnMut(RelayCallback<'_>),
{
    let Some(ws) = slot.as_mut() else {
        return false;
    };
    let mut progressed = false;
    let mut disconnected = false;
    match ws.read() {
        Ok(Message::Text(text)) => {
            progressed = true;
            dispatch_dual_frame(client, dedup, text.as_bytes(), now_ns(), on_frame);
        }
        Ok(Message::Binary(bytes)) => {
            progressed = true;
            dispatch_dual_frame(client, dedup, &bytes, now_ns(), on_frame);
        }
        Ok(Message::Ping(payload)) => {
            let _ = ws.send(Message::Pong(payload));
        }
        Ok(Message::Close(_)) | Err(WsError::ConnectionClosed) | Err(WsError::AlreadyClosed) => {
            disconnected = true
        }
        Ok(_) => {}
        Err(WsError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => {
            debug!(endpoint = %client.url, %error, "dual-relay socket read failed");
            disconnected = true;
        }
    }
    if disconnected {
        if let Some(mut ws) = slot.take() {
            let _ = ws.close(None);
        }
        let _ = commands.send(ConnectorCommand::Reconnect);
    }
    progressed
}

fn dispatch_dual_frame<F>(
    source: &WsRelayClient,
    dedup: &mut SeqDedup,
    frame: &[u8],
    recv_ns: u64,
    on_frame: &mut F,
) where
    F: FnMut(RelayCallback<'_>),
{
    // This is deliberately before shared de-duplication: gaps are properties of each physical
    // relay leg, and a healthy peer filling one must not hide the degraded relay's own counter.
    let _ = source.track_sequence(frame);
    match dedup.plan_frame(frame) {
        Ok(FramePlan::Forward {
            frame,
            last_sequence,
            duplicate_messages,
            partial_overlap,
        }) => {
            source
                .duplicate_message_total
                .fetch_add(duplicate_messages, Ordering::Relaxed);
            if partial_overlap {
                source.partial_overlaps.fetch_add(1, Ordering::Relaxed);
            }

            // The callback receives complete BroadcastFeedMessage objects. In particular, l2Msg is
            // opaque here and may contain many nested transactions. Commit only after the callback
            // has synchronously received the entire filtered frame.
            on_frame(RelayCallback {
                frame: frame.as_ref(),
                recv_ns,
                timestamps: source.timestamp_counters.take(),
            });
            dedup.complete(last_sequence);
        }
        Ok(FramePlan::Drop { duplicate_messages }) => {
            source
                .duplicate_message_total
                .fetch_add(duplicate_messages, Ordering::Relaxed);
        }
        Err(reason) => {
            // In dual mode the rejected frame never reaches the owner, so this explicit
            // synchronous signal is the only place to revoke authority for the hidden loss.
            source.signal_degradation(reason.degradation_reason());
            warn!(endpoint = %source.url, reason = reason.as_str(), "rejecting ambiguous dual-relay frame");
        }
    }
}

enum FramePlan<'a> {
    Forward {
        frame: Cow<'a, [u8]>,
        last_sequence: u64,
        duplicate_messages: u64,
        partial_overlap: bool,
    },
    Drop {
        duplicate_messages: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FramePlanError {
    InvalidJson,
    MessagesNotArray,
    MissingSequence,
    IntraFrameSequenceGap,
    ForwardSequenceGap,
    RawMessagesNotFound,
    RawMessageCountMismatch,
    /// A redundant delivery of an already-complete sequence carried DIFFERENT canonical
    /// payload bytes, or the sticky conflict latch is already armed (FS-07 / WI-04).
    PayloadConflict,
    /// A redundant delivery of a sequence whose recorded payload hash was EVICTED (older
    /// than the retained window): idempotence cannot be proven, so the frame is refused
    /// rather than silently accepted (WI-04 fix round C3a).
    UnverifiableDuplicate,
    /// A message payload that could not be base64-decoded: no hash can be recorded, so its
    /// cursor could never be conflict-checked — refuse instead of passing unverified
    /// (WI-04 fix round C3b).
    UnhashablePayload,
    /// A parsed sequenced message had NO scan-visible `l2Msg` envelope (absent/non-string),
    /// or the scanned envelopes did not align one-to-one with the parsed sequences — the
    /// frame could otherwise establish `highest_complete` with no recorded payload hash
    /// (codex WI0304_REVERIFY item 4b).
    MissingOrMisalignedEnvelope,
}

impl FramePlanError {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid JSON",
            Self::MessagesNotArray => "messages is not an array",
            Self::MissingSequence => "message sequenceNumber missing or invalid",
            Self::IntraFrameSequenceGap => "message sequence range is not strictly contiguous",
            Self::ForwardSequenceGap => "forward sequence gap",
            Self::RawMessagesNotFound => "failed to locate raw messages array",
            Self::RawMessageCountMismatch => "raw and parsed message counts differ",
            Self::PayloadConflict => "same sequence delivered with different payload bytes",
            Self::UnverifiableDuplicate => {
                "duplicate older than the retained payload-hash window (idempotence unprovable)"
            }
            Self::UnhashablePayload => "message payload is not decodable base64",
            Self::MissingOrMisalignedEnvelope => {
                "sequenced message without a scan-visible aligned l2Msg envelope"
            }
        }
    }

    fn degradation_reason(self) -> FeedDegradationReason {
        match self {
            Self::IntraFrameSequenceGap => FeedDegradationReason::IntraFrameSequenceGap,
            Self::ForwardSequenceGap => FeedDegradationReason::DualRelayForwardGap,
            Self::PayloadConflict => FeedDegradationReason::FeedPayloadConflict,
            // Unverifiable/unhashable payloads are AMBIGUITY, not proven divergence: revoke
            // authority through the existing ambiguous-frame class (no sticky conflict latch).
            _ => FeedDegradationReason::DualRelayAmbiguousFrame,
        }
    }
}

/// Number of recent sequences whose canonical payload hash is retained for cross-leg
/// verification. Redundant legs are near-synchronous (ms apart), so a duplicate almost always
/// lands well inside the window; a duplicate OLDER than the window cannot be verified and is
/// counted (`unverified_duplicate_total`) rather than trusted-or-conflicted — an honest,
/// bounded-memory residual (~16k × 40 B ≈ 650 KiB, off the engine hot cores).
const PAYLOAD_HASH_WINDOW: usize = 16_384;

/// Whole-message sequence de-duplicator for the dual-relay consumer. `highest_complete` advances
/// only after a callback has synchronously received every complete `BroadcastFeedMessage` in the
/// planned contiguous suffix. Partial overlaps are filtered at the outer message boundary; the
/// opaque `l2Msg` (which can contain many inner transactions) is never split here.
///
/// WI-04 (FS-07): dedup identity is no longer sequence-only. Every message's canonical
/// decompressed `l2Msg` bytes are hashed (domain-separated per §3.1:
/// `keccak256("mantis.feed-payload.v1" ‖ chain_id ‖ redundancy_group_id ‖ sequence ‖ bytes)`,
/// which excludes JSON spelling / WS framing / relay leg / session so independent legs
/// compare equal) and recorded per sequence. A redundant delivery is verified byte-equal
/// against the recorded hash: same cursor + same hash = one idempotent event; same cursor +
/// DIFFERENT hash latches a STICKY conflict that refuses every later frame — good frames and
/// leg reconnects included — until exact canonical reconciliation (process restart). Hashing
/// runs here, in the dedup path (the relay/sidecar process in production), never on the
/// engine hot cores.
#[derive(Debug)]
pub struct SeqDedup {
    highest_complete: Option<u64>,
    /// Sequence → domain-separated payload hash for the last [`PAYLOAD_HASH_WINDOW`] sequences.
    payload_hashes: std::collections::HashMap<u64, [u8; 32]>,
    /// Insertion order for bounded eviction of `payload_hashes`.
    recorded_order: std::collections::VecDeque<u64>,
    /// Sticky same-cursor/different-payload conflict latch.
    conflict_latched: bool,
    /// Redundant deliveries verified byte-equal against the recorded hash.
    verified_duplicates: u64,
    /// Redundant deliveries older than the retained window (could not be verified).
    unverified_duplicates: u64,
    /// Base64 scratch reused across messages (no per-message allocation growth).
    b64_scratch: Vec<u8>,
    /// Retained-hash window depth (== [`PAYLOAD_HASH_WINDOW`] in production; overridable in
    /// tests so the eviction boundary is exercisable without 16k frames).
    window: usize,
    /// Chain id folded into the payload-hash domain.
    chain_id: u64,
    /// Stable redundancy-group id (§3.1): one per chain/feed, NOT per leg/session.
    redundancy_group_id: [u8; 16],
}

impl Default for SeqDedup {
    fn default() -> Self {
        Self::for_redundancy_group(crate::constants::ARB_ONE_CHAIN_ID)
    }
}

/// Domain-separated canonical payload hash (§3.1): excludes JSON spelling, WebSocket framing,
/// relay leg, and relay session, so independent legs of one redundancy group compare equal.
fn feed_payload_hash(
    chain_id: u64,
    redundancy_group_id: &[u8; 16],
    sequence: u64,
    canonical_l2msg_bytes: &[u8],
) -> [u8; 32] {
    use tiny_keccak::{Hasher, Keccak};
    let mut hasher = Keccak::v256();
    hasher.update(b"mantis.feed-payload.v1");
    hasher.update(&chain_id.to_be_bytes());
    hasher.update(redundancy_group_id);
    hasher.update(&sequence.to_be_bytes());
    hasher.update(canonical_l2msg_bytes);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

impl SeqDedup {
    /// Construct for a chain's redundancy group. The group id is derived deterministically
    /// from the chain id (identical across legs, sessions, and processes), per the §3.1
    /// design commitment: one STABLE `redundancy_group_id` for the chain/feed; per-leg
    /// identity is tracked leg-locally and never enters payload comparison.
    pub fn for_redundancy_group(chain_id: u64) -> Self {
        let mut preimage = [0u8; 28];
        preimage[..20].copy_from_slice(b"mantis.feed-group.v1");
        preimage[20..].copy_from_slice(&chain_id.to_be_bytes());
        let digest = crate::address::keccak256(&preimage);
        let mut redundancy_group_id = [0u8; 16];
        redundancy_group_id.copy_from_slice(&digest[..16]);
        Self {
            highest_complete: None,
            payload_hashes: std::collections::HashMap::new(),
            recorded_order: std::collections::VecDeque::new(),
            conflict_latched: false,
            verified_duplicates: 0,
            unverified_duplicates: 0,
            b64_scratch: Vec::with_capacity(16 * 1024),
            window: PAYLOAD_HASH_WINDOW,
            chain_id,
            redundancy_group_id,
        }
    }

    /// Shrink the retained-hash window (tests only): lets the eviction boundary be driven
    /// with a handful of frames instead of [`PAYLOAD_HASH_WINDOW`].
    #[cfg(test)]
    fn set_payload_hash_window_for_test(&mut self, window: usize) {
        self.window = window.max(1);
    }

    /// Highest sequence whose complete outer feed message has been forwarded.
    pub fn highest_complete_sequence(&self) -> Option<u64> {
        self.highest_complete
    }

    /// Whether the sticky same-cursor/different-payload conflict latch is armed (WI-04).
    /// Once armed it survives later well-formed frames and leg reconnects; only a process
    /// restart / exact canonical reconciliation may supersede it.
    pub fn payload_conflict_latched(&self) -> bool {
        self.conflict_latched
    }

    /// Redundant deliveries whose canonical payload bytes were VERIFIED byte-equal against
    /// the recorded hash for their sequence (the idempotent-duplicate proof, WI-04).
    pub fn verified_duplicate_total(&self) -> u64 {
        self.verified_duplicates
    }

    /// Redundant deliveries older than the retained hash window: dropped as duplicates but
    /// NOT verifiable. Bounded-memory residual; a persistently non-zero rate means a leg is
    /// replaying history beyond [`PAYLOAD_HASH_WINDOW`].
    pub fn unverified_duplicate_total(&self) -> u64 {
        self.unverified_duplicates
    }

    /// Hash every message's canonical decompressed `l2Msg` bytes: verify duplicates against
    /// the recorded hash (latching a sticky conflict on mismatch) and record new sequences
    /// into the bounded window. Uses the SAME byte scanner the downstream parser consumes
    /// (`iter_envelopes`), so the compared bytes are exactly the bytes that would drive state.
    ///
    /// codex WI0304_CLOSEVERIFY R2+R3 discipline:
    /// * STRUCTURAL binding (R3): exactly ONE decodable envelope must live INSIDE each raw
    ///   message's own byte range, carrying THAT message's sequence — a nested
    ///   `{sequenceNumber, l2Msg}` planted in a different message can no longer satisfy a
    ///   global list/count alignment (the `"l2Msg"`/`"sequenceNumber"` needles are
    ///   quote-delimited, so base64 payload bytes can never fake them).
    /// * ATOMIC verification (R2): all hashes and counter deltas are STAGED and committed
    ///   only after EVERY message passes — a rejected frame leaves the retained evidence
    ///   byte-identical (no recording, no eviction, no counter drift). The two deliberate
    ///   exceptions mutate ON refusal because they ARE the refusal evidence: the sticky
    ///   `conflict_latched` (proven divergence against retained bytes) and
    ///   `unverified_duplicates` (counts unverifiable-refusal events).
    fn verify_and_record_payloads(
        &mut self,
        frame: &[u8],
        expected_sequences: &[u64],
        message_ranges: &[std::ops::Range<usize>],
    ) -> Result<(), FramePlanError> {
        if expected_sequences.len() != message_ranges.len() {
            return Err(FramePlanError::MissingOrMisalignedEnvelope);
        }
        let highest = self.highest_complete;
        let mut staged_new: Vec<(u64, [u8; 32])> = Vec::with_capacity(expected_sequences.len());
        let mut staged_verified: u64 = 0;
        for (sequence, range) in expected_sequences
            .iter()
            .copied()
            .zip(message_ranges.iter().cloned())
        {
            let Some(message_bytes) = frame.get(range) else {
                return Err(FramePlanError::MissingOrMisalignedEnvelope);
            };
            let mut envelopes = 0usize;
            let mut hash: Option<[u8; 32]> = None;
            for env in crate::iter_envelopes(message_bytes) {
                envelopes += 1;
                if envelopes > 1 || env.sequence_number != sequence {
                    return Err(FramePlanError::MissingOrMisalignedEnvelope);
                }
                self.b64_scratch.clear();
                self.b64_scratch.extend_from_slice(env.l2msg_b64);
                let decoded_len =
                    match base64_simd::forgiving_decode_inplace(&mut self.b64_scratch) {
                        Ok(decoded) => decoded.len(),
                        // C3b: an undecodable payload records NO hash, so its cursor could
                        // never be conflict-checked — refuse, never pass unverified.
                        Err(_) => return Err(FramePlanError::UnhashablePayload),
                    };
                hash = Some(feed_payload_hash(
                    self.chain_id,
                    &self.redundancy_group_id,
                    sequence,
                    &self.b64_scratch[..decoded_len],
                ));
            }
            let Some(hash) = hash else {
                return Err(FramePlanError::MissingOrMisalignedEnvelope);
            };
            if highest.is_some_and(|h| sequence <= h) {
                match self.payload_hashes.get(&sequence) {
                    Some(recorded) if *recorded == hash => staged_verified += 1,
                    Some(_) => {
                        self.conflict_latched = true;
                        return Err(FramePlanError::PayloadConflict);
                    }
                    // C3a: the hash was EVICTED (in-window duplicates are always recorded —
                    // see within_window_duplicates_are_always_verifiable), so this leg is
                    // replaying history deeper than the window and its idempotence is
                    // unprovable. Count the refusal event, refuse the frame.
                    None => {
                        self.unverified_duplicates += 1;
                        return Err(FramePlanError::UnverifiableDuplicate);
                    }
                }
            } else {
                staged_new.push((sequence, hash));
            }
        }
        // COMMIT (R2): every structural/alignment/decodability/duplicate check passed.
        self.verified_duplicates += staged_verified;
        for (sequence, hash) in staged_new {
            if self.payload_hashes.insert(sequence, hash).is_none() {
                self.recorded_order.push_back(sequence);
                if self.recorded_order.len() > self.window {
                    if let Some(evicted) = self.recorded_order.pop_front() {
                        self.payload_hashes.remove(&evicted);
                    }
                }
            }
        }
        Ok(())
    }

    fn plan_frame<'a>(&mut self, frame: &'a [u8]) -> Result<FramePlan<'a>, FramePlanError> {
        // WI-04 sticky conflict: once two legs have delivered divergent bytes for one cursor,
        // NO later frame — however well-formed — may pass until canonical reconciliation.
        if self.conflict_latched {
            return Err(FramePlanError::PayloadConflict);
        }
        let root: serde_json::Value =
            serde_json::from_slice(frame).map_err(|_| FramePlanError::InvalidJson)?;

        let Some(messages_value) = root.get("messages") else {
            // Some test/legacy relay sources send one flat BroadcastFeedMessage per WS frame.
            // Preserve its bytes exactly in dual mode when no filtering is required.
            let Some(sequence) = sequence_from_value(&root) else {
                // Control-only BroadcastMessages contain no feed message and need no callback.
                return Ok(FramePlan::Drop {
                    duplicate_messages: 0,
                });
            };
            return match self.highest_complete {
                Some(highest) if sequence <= highest => {
                    self.verify_and_record_payloads(frame, &[sequence], &[0..frame.len()])?;
                    Ok(FramePlan::Drop {
                        duplicate_messages: 1,
                    })
                }
                Some(highest) if highest.checked_add(1) != Some(sequence) => {
                    Err(FramePlanError::ForwardSequenceGap)
                }
                _ => {
                    self.verify_and_record_payloads(frame, &[sequence], &[0..frame.len()])?;
                    Ok(FramePlan::Forward {
                        frame: Cow::Borrowed(frame),
                        last_sequence: sequence,
                        duplicate_messages: 0,
                        partial_overlap: false,
                    })
                }
            };
        };

        let messages = messages_value
            .as_array()
            .ok_or(FramePlanError::MessagesNotArray)?;
        if messages.is_empty() {
            return Ok(FramePlan::Drop {
                duplicate_messages: 0,
            });
        }
        let sequences = messages
            .iter()
            .map(sequence_from_value)
            .collect::<Option<Vec<_>>>()
            .ok_or(FramePlanError::MissingSequence)?;
        for pair in sequences.windows(2) {
            if pair[0].checked_add(1) != Some(pair[1]) {
                return Err(FramePlanError::IntraFrameSequenceGap);
            }
        }

        // Hoisted (R3): per-message byte ranges are the STRUCTURAL binding for envelope
        // verification, needed on the all-duplicate path too.
        let (array_content_start, raw_messages) =
            raw_message_ranges(frame).ok_or(FramePlanError::RawMessagesNotFound)?;
        if raw_messages.len() != sequences.len() {
            return Err(FramePlanError::RawMessageCountMismatch);
        }
        let duplicate_count = self.highest_complete.map_or(0, |highest| {
            sequences
                .iter()
                .take_while(|sequence| **sequence <= highest)
                .count()
        });
        if duplicate_count == sequences.len() {
            self.verify_and_record_payloads(frame, &sequences, &raw_messages)?;
            return Ok(FramePlan::Drop {
                duplicate_messages: duplicate_count as u64,
            });
        }
        if let Some(highest) = self.highest_complete {
            if highest.checked_add(1) != Some(sequences[duplicate_count]) {
                return Err(FramePlanError::ForwardSequenceGap);
            }
        }
        self.verify_and_record_payloads(frame, &sequences, &raw_messages)?;

        let last_sequence = *sequences.last().expect("non-empty checked above");
        let partial_overlap = duplicate_count > 0;
        // Duplicate messages are necessarily a prefix after strict contiguity validation. Splice
        // that prefix out of the ORIGINAL JSON instead of reserializing serde_json::Value:
        // downstream's allocation-free scanner depends on sequenceNumber preceding l2Msg, and raw
        // splicing also retains unknown fields/signatures/opaque batch bytes exactly.
        let filtered = if duplicate_count == 0 {
            Cow::Borrowed(frame)
        } else {
            let first_new = raw_messages[duplicate_count].start;
            let mut filtered = Vec::with_capacity(frame.len());
            filtered.extend_from_slice(&frame[..array_content_start]);
            filtered.extend_from_slice(&frame[first_new..]);
            Cow::Owned(filtered)
        };
        Ok(FramePlan::Forward {
            frame: filtered,
            last_sequence,
            duplicate_messages: duplicate_count as u64,
            partial_overlap,
        })
    }

    fn complete(&mut self, last_sequence: u64) {
        debug_assert!(match self.highest_complete {
            None => true,
            Some(highest) => highest
                .checked_add(1)
                .is_some_and(|next| next <= last_sequence),
        });
        self.highest_complete = Some(last_sequence);
    }
}

fn sequence_from_value(value: &serde_json::Value) -> Option<u64> {
    let sequence = value.get("sequenceNumber")?;
    sequence
        .as_u64()
        .or_else(|| sequence.as_str()?.parse::<u64>().ok())
}

/// Locate every raw value in the root BroadcastMessage's `messages` array. The frame is parsed by
/// serde_json before this scanner runs; this structural pass exists only to retain the original byte
/// ranges when removing a duplicate prefix.
fn raw_message_ranges(frame: &[u8]) -> Option<(usize, Vec<std::ops::Range<usize>>)> {
    let mut cursor = skip_json_whitespace(frame, 0);
    if frame.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor += 1;
    loop {
        cursor = skip_json_whitespace(frame, cursor);
        if frame.get(cursor) == Some(&b'}') {
            return None;
        }
        let key_start = cursor;
        let key_end = scan_json_string(frame, key_start)?;
        let is_messages = &frame[key_start..key_end] == b"\"messages\"";
        cursor = skip_json_whitespace(frame, key_end);
        if frame.get(cursor) != Some(&b':') {
            return None;
        }
        cursor = skip_json_whitespace(frame, cursor + 1);
        if is_messages {
            if frame.get(cursor) != Some(&b'[') {
                return None;
            }
            let array_content_start = cursor + 1;
            cursor = array_content_start;
            let mut ranges = Vec::new();
            loop {
                cursor = skip_json_whitespace(frame, cursor);
                if frame.get(cursor) == Some(&b']') {
                    return Some((array_content_start, ranges));
                }
                let start = cursor;
                let end = scan_json_value(frame, start)?;
                ranges.push(start..end);
                cursor = skip_json_whitespace(frame, end);
                match frame.get(cursor) {
                    Some(b',') => cursor += 1,
                    Some(b']') => return Some((array_content_start, ranges)),
                    _ => return None,
                }
            }
        }

        cursor = scan_json_value(frame, cursor)?;
        cursor = skip_json_whitespace(frame, cursor);
        match frame.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => return None,
            _ => return None,
        }
    }
}

fn skip_json_whitespace(frame: &[u8], mut cursor: usize) -> usize {
    while frame
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
    {
        cursor += 1;
    }
    cursor
}

fn scan_json_string(frame: &[u8], start: usize) -> Option<usize> {
    if frame.get(start) != Some(&b'"') {
        return None;
    }
    let mut cursor = start + 1;
    while cursor < frame.len() {
        match frame[cursor] {
            b'\\' => cursor = cursor.checked_add(2)?,
            b'"' => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    None
}

fn scan_json_value(frame: &[u8], start: usize) -> Option<usize> {
    let start = skip_json_whitespace(frame, start);
    match *frame.get(start)? {
        b'"' => scan_json_string(frame, start),
        b'{' | b'[' => {
            let mut stack = Vec::with_capacity(8);
            let mut cursor = start;
            while cursor < frame.len() {
                match frame[cursor] {
                    b'"' => cursor = scan_json_string(frame, cursor)?,
                    b'{' | b'[' => {
                        stack.push(frame[cursor]);
                        cursor += 1;
                    }
                    b'}' => {
                        if stack.pop() != Some(b'{') {
                            return None;
                        }
                        cursor += 1;
                        if stack.is_empty() {
                            return Some(cursor);
                        }
                    }
                    b']' => {
                        if stack.pop() != Some(b'[') {
                            return None;
                        }
                        cursor += 1;
                        if stack.is_empty() {
                            return Some(cursor);
                        }
                    }
                    _ => cursor += 1,
                }
            }
            None
        }
        _ => {
            let mut cursor = start;
            while cursor < frame.len()
                && !matches!(
                    frame[cursor],
                    b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t'
                )
            {
                cursor += 1;
            }
            (cursor > start).then_some(cursor)
        }
    }
}

/// Resolve `host:port` and return the **lowest-RTT** address by a quick TCP-connect probe (2 samples
/// each), so the upstream IP self-adapts to the deploy location (Cloudflare anycast routes the best
/// IP differently per location → the laptop's winner is not the AWS winner). Falls back to the first
/// resolved address if probing fails. Logs the chosen IP + measured RTT.
/// Resolve `host:port` and, when it has multiple A-records (anycast / multi-IP endpoints like the
/// Cloudflare feed or the Arbitrum sequencer submit endpoint), RTT-probe each and return the
/// lowest-latency one — so the choice self-adapts to wherever we run. Single-address hosts (e.g. the
/// localhost relay) are returned directly without probing. Reused by the submit connector
/// (`engine-submit`) for submit-IP pinning. Probing is rare (connect time only), never per-frame.
pub fn pick_best_addr(host: &str, port: u16) -> Option<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::SocketAddr> = (host, port).to_socket_addrs().ok()?.collect();
    if addrs.is_empty() {
        return None;
    }
    // Single address (e.g. the localhost relay, or a non-anycast host) → no choice to probe, and
    // probing would waste a connect (and break single-accept test stubs). Use it directly.
    if addrs.len() == 1 {
        return Some(addrs[0]);
    }
    let mut best = addrs[0];
    let mut best_ns = u128::MAX;
    for a in &addrs {
        let mut samples = Vec::with_capacity(2);
        for _ in 0..2 {
            let t = Instant::now();
            if TcpStream::connect_timeout(a, Duration::from_secs(3)).is_ok() {
                samples.push(t.elapsed().as_nanos());
            }
        }
        if let Some(min) = samples.into_iter().min() {
            if min < best_ns {
                best_ns = min;
                best = *a;
            }
        }
    }
    if best_ns != u128::MAX {
        info!(ip = %best.ip(), rtt_us = (best_ns / 1000) as u64, candidates = addrs.len(), "picked lowest-RTT IP via connect-probe");
    }
    Some(best)
}

/// Put a tungstenite WebSocket's underlying TcpStream into non-blocking mode (for the dual-relay
/// single-thread poller). Best-effort.
fn set_nonblocking(ws: &WebSocket<RelayStream>) -> std::io::Result<()> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(tcp) => tcp.set_nonblocking(true),
        MaybeTlsStream::Rustls(s) => s.get_ref().set_nonblocking(true),
        _ => Ok(()),
    }
}

/// Find the first `"sequenceNumber":N` occurrence in `frame`. Tolerant to both `:N` and
/// `:"N"` (the relay always emits unquoted but the parser is liberal).
/// Scan the first `sequenceNumber` out of a raw feed frame (used by the latency probe to tag
/// per-frame arrival for cross-AZ relative-arrival comparison). Byte-scan; no JSON allocation.
pub fn parse_first_sequence_number(frame: &[u8]) -> Option<u64> {
    let key = b"\"sequenceNumber\"";
    let pos = memchr::memmem::find(frame, key)?;
    let mut i = pos + key.len();
    while i < frame.len() && (frame[i] == b' ' || frame[i] == b':' || frame[i] == b'"') {
        i += 1;
    }
    let mut n: u64 = 0;
    while i < frame.len() && frame[i].is_ascii_digit() {
        n = n.checked_mul(10)?.checked_add((frame[i] - b'0') as u64)?;
        i += 1;
    }
    Some(n)
}

#[derive(Clone, Copy, Debug)]
struct FrameSequenceRange {
    first: u64,
    last: u64,
    high_water: u64,
    missing: u64,
}

/// Scan every sequence-number occurrence once so a batched frame cannot hide an internal loss.
/// `Err` means a key was present but its value was malformed; `Ok(None)` is a control-only frame.
fn scan_frame_sequence_range(frame: &[u8]) -> Result<Option<FrameSequenceRange>, ()> {
    let key = b"\"sequenceNumber\"";
    let mut cursor = 0usize;
    let mut first = None;
    let mut last: Option<u64> = None;
    let mut high_water = 0u64;
    let mut missing = 0u64;

    while let Some(relative) = memchr::memmem::find(&frame[cursor..], key) {
        let key_start = cursor + relative;
        let mut i = skip_json_whitespace(frame, key_start + key.len());
        if frame.get(i) != Some(&b':') {
            return Err(());
        }
        i = skip_json_whitespace(frame, i + 1);
        let quoted = frame.get(i) == Some(&b'"');
        if quoted {
            i += 1;
        }
        let mut sequence = 0u64;
        let mut saw_digit = false;
        while i < frame.len() && frame[i].is_ascii_digit() {
            sequence = sequence
                .checked_mul(10)
                .and_then(|n| n.checked_add((frame[i] - b'0') as u64))
                .ok_or(())?;
            i += 1;
            saw_digit = true;
        }
        if !saw_digit {
            return Err(());
        }
        if quoted {
            if frame.get(i) != Some(&b'"') {
                return Err(());
            }
            i += 1;
        }
        let delimiter = skip_json_whitespace(frame, i);
        if frame
            .get(delimiter)
            .is_some_and(|byte| !matches!(byte, b',' | b'}' | b']'))
        {
            return Err(());
        }

        if let Some(previous) = last {
            if previous.checked_add(1) != Some(sequence) {
                missing = missing.saturating_add(if sequence > previous {
                    sequence - previous - 1
                } else {
                    1
                });
            }
        } else {
            first = Some(sequence);
        }
        last = Some(sequence);
        high_water = high_water.max(sequence);
        cursor = i.max(key_start + key.len());
    }

    Ok(first.map(|first| FrameSequenceRange {
        first,
        last: last.expect("first sequence implies last"),
        high_water,
        missing,
    }))
}

/// Find the *last* `"sequenceNumber":N` occurrence in `frame` — i.e. the sequence number
/// of the final batched message. Mirrors [`parse_first_sequence_number`] but scans from the
/// end so cross-frame gap detection compares against the true tail of the previous frame.
fn parse_last_sequence_number(frame: &[u8]) -> Option<u64> {
    let key = b"\"sequenceNumber\"";
    let pos = memchr::memmem::rfind(frame, key)?;
    let mut i = pos + key.len();
    while i < frame.len() && (frame[i] == b' ' || frame[i] == b':' || frame[i] == b'"') {
        i += 1;
    }
    let mut n: u64 = 0;
    let mut saw_digit = false;
    while i < frame.len() && frame[i].is_ascii_digit() {
        n = n.checked_mul(10)?.checked_add((frame[i] - b'0') as u64)?;
        i += 1;
        saw_digit = true;
    }
    if saw_digit {
        Some(n)
    } else {
        None
    }
}

/// Parse `NETWORK_FINE` once during client construction; no environment access reaches `Read`.
fn network_fine_from_value(value: Option<&str>) -> bool {
    crate::parse_enabled_flag(value)
}

/// Wall-clock (UNIX-epoch) nanoseconds for the per-frame `recv_ns` stamp. INVARIANT: must match the
/// clock domain of the sequencer's header timestamp. This is retained for cross-host correlation
/// and relative arrival comparisons only; same-host stage durations use a monotonic raw stamp.
fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind_loopback_or_skip() -> Option<std::net::TcpListener> {
        match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping loopback test: sandbox denied TcpListener::bind");
                None
            }
            Err(error) => panic!("loopback listener: {error}"),
        }
    }

    fn broadcast_frame(first: u64, last: u64) -> Vec<u8> {
        broadcast_sequences(&(first..=last).collect::<Vec<_>>())
    }

    fn broadcast_sequences(sequences: &[u64]) -> Vec<u8> {
        let messages = sequences
            .iter()
            .map(|sequence_number| {
                format!(
                    r#"{{"sequenceNumber":{sequence_number},"message":{{"message":{{"header":{{"timestamp":1}},"l2Msg":"AA=="}}}}}}"#
                )
            })
            .collect::<Vec<_>>();
        format!(r#"{{"version":1,"messages":[{}]}}"#, messages.join(",")).into_bytes()
    }

    fn dispatch_sequences(
        source: &WsRelayClient,
        dedup: &mut SeqDedup,
        frame: &[u8],
        emitted: &mut Vec<u64>,
    ) {
        dispatch_dual_frame(source, dedup, frame, 1, &mut |cb| {
            emitted.extend(crate::iter_envelopes(cb.frame).map(|env| env.sequence_number));
        });
    }

    #[test]
    fn parse_seq_basic() {
        let raw = br#"{"sequenceNumber":12345,"foo":1}"#;
        assert_eq!(parse_first_sequence_number(raw), Some(12345));
    }

    #[test]
    fn network_fine_uses_the_shared_enabled_flag_vocabulary() {
        assert!(!network_fine_from_value(None));
        assert!(!network_fine_from_value(Some("0")));
        assert!(network_fine_from_value(Some("1")));
        assert!(network_fine_from_value(Some("true")));
        assert!(network_fine_from_value(Some(" TRUE ")));
        assert!(network_fine_from_value(Some("on")));
        assert!(network_fine_from_value(Some("yes")));
        assert!(!network_fine_from_value(Some("enabled")));
    }

    #[test]
    fn single_relay_gap_revokes_authority_before_dispatching_gap_frame() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(match reason {
                    FeedDegradationReason::SingleRelayDisconnect => {
                        "degraded:single_relay_disconnect"
                    }
                    FeedDegradationReason::SequenceGap => "degraded:ws_sequence_gap",
                    FeedDegradationReason::DualRelayForwardGap => "degraded:dual_relay_forward_gap",
                    FeedDegradationReason::DualRelayUnavailable => {
                        "degraded:dual_relay_unavailable"
                    }
                    FeedDegradationReason::IntraFrameSequenceGap => {
                        "degraded:intra_frame_sequence_gap"
                    }
                    FeedDegradationReason::DualRelayAmbiguousFrame => {
                        "degraded:dual_relay_ambiguous_frame"
                    }
                    FeedDegradationReason::MalformedSequenceFrame => {
                        "degraded:malformed_sequence_frame"
                    }
                    FeedDegradationReason::FeedPayloadConflict => "degraded:feed_payload_conflict",
                });
            }));

        let mut dedup = SeqDedup::default();
        source.dispatch_single_frame(&mut dedup, &broadcast_frame(100, 100), 1, &mut |_| {});
        events.lock().unwrap().clear();
        let dispatch_events = events.clone();
        source.dispatch_single_frame(&mut dedup, &broadcast_frame(102, 102), 2, &mut |_| {
            dispatch_events.lock().unwrap().push("frame");
        });

        assert_eq!(
            *events.lock().unwrap(),
            ["degraded:ws_sequence_gap"],
            "submission authority must be revoked before the gap-bearing frame is rejected"
        );
    }

    fn broadcast_frame_with_payload(sequence: u64, l2msg: &str) -> Vec<u8> {
        format!(
            r#"{{"version":1,"messages":[{{"sequenceNumber":{sequence},"message":{{"message":{{"header":{{"timestamp":1}},"l2Msg":"{l2msg}"}}}}}}]}}"#
        )
        .into_bytes()
    }

    #[test]
    fn equal_cursor_different_identity_is_refused_before_owner_callback() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }));
        let mut emitted = 0usize;
        let mut dedup = SeqDedup::default();

        source.dispatch_single_frame(
            &mut dedup,
            &broadcast_frame_with_payload(700, "AA=="),
            1,
            &mut |_| emitted += 1,
        );
        source.dispatch_single_frame(
            &mut dedup,
            &broadcast_frame_with_payload(700, "Ag=="),
            2,
            &mut |_| emitted += 1,
        );

        assert_eq!(
            emitted, 1,
            "a same-cursor divergent payload must not reach the owner/state ingress"
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["feed_payload_conflict"],
            "the refusal must be reported through the topology-neutral conflict reason"
        );

        // An exact duplicate is idempotent and does not itself degrade the feed.
        let duplicate_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let duplicate_callback_events = duplicate_events.clone();
        let duplicate_source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                duplicate_callback_events
                    .lock()
                    .unwrap()
                    .push(reason.as_str());
            }));
        let mut duplicate_emitted = 0usize;
        let mut duplicate_dedup = SeqDedup::default();
        let exact = broadcast_frame_with_payload(701, "AA==");
        duplicate_source.dispatch_single_frame(
            &mut duplicate_dedup,
            &exact,
            3,
            &mut |_| duplicate_emitted += 1,
        );
        duplicate_source.dispatch_single_frame(
            &mut duplicate_dedup,
            &exact,
            4,
            &mut |_| duplicate_emitted += 1,
        );
        assert_eq!(duplicate_emitted, 1, "exact duplicate must be dropped idempotently");
        assert!(duplicate_events.lock().unwrap().is_empty());
    }

    #[test]
    fn single_relay_intra_frame_gap_revokes_before_dispatch() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }));
        let mut dedup = SeqDedup::default();
        let dispatch_events = events.clone();
        source.dispatch_single_frame(&mut dedup, &broadcast_sequences(&[100, 102]), 1, &mut |_| {
            dispatch_events.lock().unwrap().push("frame");
        });
        assert_eq!(
            *events.lock().unwrap(),
            ["intra_frame_sequence_gap"],
            "intra-frame discontinuity must revoke authority before dispatch"
        );
    }

    #[test]
    fn malformed_sequence_value_revokes_before_dispatch() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }));
        let mut dedup = SeqDedup::default();
        let dispatch_events = events.clone();
        let malformed = br#"{"messages":[{"sequenceNumber":"100junk"}]}"#;
        source.dispatch_single_frame(&mut dedup, malformed, 1, &mut |_| {
            dispatch_events.lock().unwrap().push("frame");
        });
        assert_eq!(
            *events.lock().unwrap(),
            ["malformed_sequence_frame"],
            "numeric prefixes must not be accepted as complete sequence values"
        );
    }

    #[test]
    fn dual_relay_intra_frame_gap_revokes_before_rejection() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }));
        let mut dedup = SeqDedup::default();
        let mut emitted = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_sequences(&[100, 102]),
            &mut emitted,
        );
        assert!(emitted.is_empty(), "ambiguous frame must remain rejected");
        assert_eq!(*events.lock().unwrap(), ["intra_frame_sequence_gap"]);
    }

    /// One complete BroadcastMessage carrying `sequence` with a caller-chosen base64 `l2Msg`
    /// — the WI-04 payload-equality fixtures ("AA==" and "AQ==" are one-byte payloads 0x00 /
    /// 0x01: same cursor, different canonical decompressed bytes).
    fn broadcast_with_l2msg(sequence: u64, l2msg_b64: &str) -> Vec<u8> {
        format!(
            r#"{{"version":1,"messages":[{{"sequenceNumber":{sequence},"message":{{"message":{{"header":{{"timestamp":1}},"l2Msg":"{l2msg_b64}"}}}}}}]}}"#
        )
        .into_bytes()
    }

    fn conflict_test_source(events: &Arc<std::sync::Mutex<Vec<&'static str>>>) -> WsRelayClient {
        let callback_events = events.clone();
        WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }))
    }

    /// FS-07 (RED): dual-relay dedup keyed on sequence alone silently trusts whichever leg
    /// delivered first — a second leg delivering DIFFERENT bytes for the SAME cursor is
    /// dropped as an "idempotent duplicate". It must instead latch a sticky payload conflict
    /// and revoke authority.
    #[test]
    fn dual_relay_same_sequence_different_payload_hash_latches_conflict() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();

        let mut emitted = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(100, "AA=="),
            &mut emitted,
        );
        assert_eq!(emitted, [100], "first delivery forwards normally");
        assert!(!dedup.payload_conflict_latched());

        let mut emitted2 = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(100, "AQ=="),
            &mut emitted2,
        );
        assert!(
            emitted2.is_empty(),
            "the conflicting redelivery must never be forwarded"
        );
        assert!(
            dedup.payload_conflict_latched(),
            "same (group, sequence) with DIFFERENT canonical payload bytes must latch a \
             sticky conflict — sequence-only dedup silently trusted the divergent leg (FS-07)"
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["feed_payload_conflict"],
            "the conflict must revoke submission authority via the degradation callback"
        );
    }

    /// WI-04 (RED): a redundant delivery with IDENTICAL canonical payload bytes is one
    /// idempotent event — dropped, no conflict, and VERIFIED against the recorded hash
    /// (sequence-only dedup never verified anything).
    #[test]
    fn same_payload_redundant_delivery_is_one_idempotent_event() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();

        let mut emitted = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(200, "AA=="),
            &mut emitted,
        );
        let mut emitted2 = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(200, "AA=="),
            &mut emitted2,
        );
        assert_eq!(emitted, [200]);
        assert!(emitted2.is_empty(), "the redundant delivery is deduplicated");
        assert!(!dedup.payload_conflict_latched(), "identical bytes are no conflict");
        assert!(
            events.lock().unwrap().is_empty(),
            "an idempotent duplicate must not degrade the feed"
        );
        assert_eq!(
            dedup.verified_duplicate_total(),
            1,
            "the duplicate must be VERIFIED byte-equal against the recorded payload hash, \
             not merely assumed equal by sequence number"
        );
    }

    /// C3a invariant (proof): `highest_complete` only ever advances CONTIGUOUSLY (forward
    /// gaps are refused), and every new sequence is recorded at plan time — so every
    /// duplicate INSIDE the retained window always has a recorded hash to verify against.
    /// "Unverifiable" can only mean "older than the window", never a silent recording gap.
    #[test]
    fn within_window_duplicates_are_always_verifiable() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();
        let mut emitted = Vec::new();
        for seq in 400..403u64 {
            dispatch_sequences(
                &source,
                &mut dedup,
                &broadcast_with_l2msg(seq, "AA=="),
                &mut emitted,
            );
        }
        for seq in 400..403u64 {
            let mut dup = Vec::new();
            dispatch_sequences(
                &source,
                &mut dedup,
                &broadcast_with_l2msg(seq, "AA=="),
                &mut dup,
            );
            assert!(dup.is_empty());
        }
        assert_eq!(dedup.verified_duplicate_total(), 3, "every in-window duplicate verified");
        assert_eq!(dedup.unverified_duplicate_total(), 0, "no in-window recording gap");
        assert!(!dedup.payload_conflict_latched());
        assert!(events.lock().unwrap().is_empty());
    }

    /// C3a (RED): a duplicate whose recorded hash has been EVICTED (older than the retained
    /// window) cannot be proven idempotent — it must be REFUSED with authority revocation,
    /// not silently accepted as a duplicate. (In-window duplicates are always verifiable —
    /// see `within_window_duplicates_are_always_verifiable` — so this fires only for legs
    /// replaying history deeper than the window, far beyond any sane redundancy lag.)
    #[test]
    fn evicted_window_duplicate_is_refused_not_silently_accepted() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();
        dedup.set_payload_hash_window_for_test(2);
        let mut emitted = Vec::new();
        // Forward 500..=503; with window 2 the hashes for 500/501 are evicted.
        for seq in 500..=503u64 {
            dispatch_sequences(
                &source,
                &mut dedup,
                &broadcast_with_l2msg(seq, "AA=="),
                &mut emitted,
            );
        }
        assert_eq!(emitted, [500, 501, 502, 503]);
        assert!(events.lock().unwrap().is_empty(), "clean forward run");

        // Redeliver 500 — SAME bytes, but its hash is gone: idempotence is unprovable.
        let mut replay = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(500, "AA=="),
            &mut replay,
        );
        assert!(replay.is_empty(), "the unverifiable duplicate must never be forwarded");
        assert!(
            dedup.unverified_duplicate_total() >= 1,
            "the unverifiable duplicate must be counted as evidence"
        );
        assert!(
            !events.lock().unwrap().is_empty(),
            "an unverifiable (post-eviction) duplicate cannot be proven idempotent — it \
             must revoke authority via the degradation callback, not silently pass"
        );
    }

    /// C3b (RED): a payload that cannot be base64-decoded records NO hash — a later
    /// conflicting redelivery of that cursor would be invisible to the conflict check. An
    /// unhashable payload must REFUSE the frame (authority revoked), not silently pass into
    /// the engine unverified.
    #[test]
    fn unhashable_relay_payload_refuses_the_frame_instead_of_passing_unverified() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();
        let mut emitted = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(600, "!!not-base64!!"),
            &mut emitted,
        );
        assert!(
            emitted.is_empty(),
            "an unhashable l2Msg must not be forwarded past the payload-equality boundary"
        );
        assert!(
            !events.lock().unwrap().is_empty(),
            "an unhashable payload must revoke authority (its cursor can never be \
             conflict-checked), not silently pass"
        );
    }

    /// codex WI0304_REVERIFY item 4b (RED): a sequenced message with an ABSENT or NON-STRING
    /// `l2Msg` yields NOTHING from the envelope scanner, so payload verification runs zero
    /// iterations — the corrupt frame silently establishes `highest_complete` with no
    /// recorded hash and no authority revocation. Planning must PROVE one scan-visible,
    /// decodable envelope per parsed sequenced message (sequence-aligned) BEFORE recording,
    /// forwarding, or advancing `highest_complete`; a zero-envelope message must REFUSE.
    #[test]
    fn missing_or_nonstring_l2msg_message_refuses_the_frame() {
        // (a) absent l2Msg
        let absent = br#"{"version":1,"messages":[{"sequenceNumber":700,"message":{"message":{"header":{"timestamp":1}}}}]}"#;
        // (b) non-string l2Msg
        let nonstring = br#"{"version":1,"messages":[{"sequenceNumber":700,"message":{"message":{"header":{"timestamp":1},"l2Msg":7}}}]}"#;
        // (c) mixed frame: a good message followed by an envelope-less one — the count/
        //     alignment proof must catch the partial scan, not just the empty scan.
        let mixed = format!(
            r#"{{"version":1,"messages":[{{"sequenceNumber":700,"message":{{"message":{{"header":{{"timestamp":1}},"l2Msg":"AA=="}}}}}},{{"sequenceNumber":701,"message":{{"message":{{"header":{{"timestamp":1}}}}}}}}]}}"#
        )
        .into_bytes();

        for (label, frame) in [
            ("absent l2Msg", absent.to_vec()),
            ("non-string l2Msg", nonstring.to_vec()),
            ("mixed good+envelope-less", mixed),
        ] {
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            let source = conflict_test_source(&events);
            let mut dedup = SeqDedup::default();
            let mut emitted = Vec::new();
            dispatch_sequences(&source, &mut dedup, &frame, &mut emitted);
            assert!(
                emitted.is_empty(),
                "{label}: a frame whose message has no hashable envelope must never be \
                 forwarded — its cursor could silently become authoritative truth"
            );
            assert_eq!(
                dedup.highest_complete_sequence(),
                None,
                "{label}: the corrupt frame must not establish highest_complete"
            );
            assert!(
                !events.lock().unwrap().is_empty(),
                "{label}: the refusal must revoke authority via the degradation callback"
            );
        }
    }

    /// codex WI0304_CLOSEVERIFY R2 (RED): verification must be ATOMIC — stage, prove
    /// EVERYTHING, then commit. Previously each matching prefix recorded hashes (and could
    /// EVICT retained evidence) before the final count check rejected a missing suffix, so a
    /// rejected partial frame destroyed valid completed-sequence evidence and a later
    /// LEGITIMATE duplicate was falsely refused as unverifiable (over-refusal, regressing 4c).
    #[test]
    fn rejected_partial_frame_leaves_dedup_evidence_untouched() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();
        dedup.set_payload_hash_window_for_test(2);
        let mut emitted = Vec::new();
        for seq in 500..=503u64 {
            dispatch_sequences(
                &source,
                &mut dedup,
                &broadcast_with_l2msg(seq, "AA=="),
                &mut emitted,
            );
        }
        assert_eq!(emitted, [500, 501, 502, 503]);
        // Retained window (2): hashes for 502 and 503.

        // A PARTIAL frame: 504 is good, 505 has no l2Msg. It must be refused — and must
        // leave the retained evidence EXACTLY as it was (previously 504's hash was recorded
        // pre-rejection, evicting 502's).
        let partial = br#"{"version":1,"messages":[{"sequenceNumber":504,"message":{"message":{"header":{"timestamp":1},"l2Msg":"AA=="}}},{"sequenceNumber":505,"message":{"message":{"header":{"timestamp":1}}}}]}"#;
        let mut rejected = Vec::new();
        dispatch_sequences(&source, &mut dedup, partial, &mut rejected);
        assert!(rejected.is_empty(), "the partial frame must be refused");
        assert_eq!(dedup.highest_complete_sequence(), Some(503));
        events.lock().unwrap().clear();

        // A later LEGITIMATE duplicate of completed sequence 502 (same bytes) must still be
        // verified-accepted — its retained hash must have survived the rejected frame.
        let mut dup = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(502, "AA=="),
            &mut dup,
        );
        assert!(dup.is_empty(), "a duplicate is deduplicated, not re-forwarded");
        assert!(
            events.lock().unwrap().is_empty(),
            "the legitimate duplicate must NOT be refused — a rejected partial frame must \
             not have evicted its retained hash (rollback of staged evidence)"
        );
        assert!(
            dedup.verified_duplicate_total() >= 1,
            "the duplicate must be VERIFIED against the still-retained hash"
        );
        assert_eq!(
            dedup.unverified_duplicate_total(),
            0,
            "no unverifiable refusal may have occurred — evidence was never corrupted"
        );

        // And the CORRECTED redelivery of 504+505 must forward normally afterwards.
        let mut fixed = Vec::new();
        let fixed_frame = format!(
            r#"{{"version":1,"messages":[{{"sequenceNumber":504,"message":{{"message":{{"header":{{"timestamp":1}},"l2Msg":"AA=="}}}}}},{{"sequenceNumber":505,"message":{{"message":{{"header":{{"timestamp":1}},"l2Msg":"AA=="}}}}}}]}}"#
        );
        dispatch_sequences(&source, &mut dedup, fixed_frame.as_bytes(), &mut fixed);
        assert_eq!(fixed, [504, 505], "the corrected redelivery forwards cleanly");
        assert_eq!(dedup.highest_complete_sequence(), Some(505));
    }

    /// codex WI0304_CLOSEVERIFY R3 (RED): envelope binding must be STRUCTURAL, per raw
    /// message byte range — not a global lexical scan. A nested `{sequenceNumber:B, l2Msg}`
    /// planted INSIDE message A (while outer message B carries NO l2Msg) satisfies the
    /// global list/count alignment and previously completed B misbound to A's nested
    /// envelope. Exactly one decodable envelope must live INSIDE each message's own bytes.
    #[test]
    fn nested_envelope_cannot_misbind_to_an_envelope_less_message() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();
        // Message 800 carries its own l2Msg AND a nested {801, l2Msg}; outer message 801 has
        // NO l2Msg. Global scan yields [800, 801] — list and count align — but 801's
        // "payload" lives inside 800's bytes.
        let evil = br#"{"version":1,"messages":[{"sequenceNumber":800,"message":{"message":{"header":{"timestamp":1},"l2Msg":"AA==","evil":{"sequenceNumber":801,"l2Msg":"AQ=="}}}},{"sequenceNumber":801,"message":{"message":{"header":{"timestamp":1}}}}]}"#;
        let mut emitted = Vec::new();
        dispatch_sequences(&source, &mut dedup, evil, &mut emitted);
        assert!(
            emitted.is_empty(),
            "a frame whose envelope-less message is satisfied by an envelope NESTED IN A \
             DIFFERENT message must be refused, not forwarded"
        );
        assert_eq!(
            dedup.highest_complete_sequence(),
            None,
            "the misbound frame must not establish highest_complete"
        );
        assert!(
            !events.lock().unwrap().is_empty(),
            "the structural misbinding must revoke authority"
        );

        // Positive control: a normal one-envelope-per-message frame still accepts.
        let good = br#"{"version":1,"messages":[{"sequenceNumber":800,"message":{"message":{"header":{"timestamp":1},"l2Msg":"AA=="}}},{"sequenceNumber":801,"message":{"message":{"header":{"timestamp":1},"l2Msg":"AQ=="}}}]}"#;
        let events2 = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source2 = conflict_test_source(&events2);
        let mut dedup2 = SeqDedup::default();
        let mut ok = Vec::new();
        dispatch_sequences(&source2, &mut dedup2, good, &mut ok);
        assert_eq!(ok, [800, 801], "structurally sound frames still forward");
        assert!(events2.lock().unwrap().is_empty());
    }

    /// WI-04 (RED): the payload conflict is STICKY — later well-formed frames and a leg
    /// reconnect (fresh physical client, same redundancy group) must not clear it or slip
    /// frames past it. Only exact canonical reconciliation / restart supersedes it.
    #[test]
    fn conflict_taint_survives_later_good_frames_and_reconnect() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let source = conflict_test_source(&events);
        let mut dedup = SeqDedup::default();

        let mut emitted = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(300, "AA=="),
            &mut emitted,
        );
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(300, "AQ=="),
            &mut emitted,
        );
        assert_eq!(emitted, [300], "only the pre-conflict frame was forwarded");

        // A later GOOD contiguous frame on the same leg must be refused while latched.
        let mut after_conflict = Vec::new();
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_with_l2msg(301, "AA=="),
            &mut after_conflict,
        );
        assert!(
            after_conflict.is_empty(),
            "a later good frame must NOT clear the conflict or be forwarded — the divergent \
             history demands exact canonical reconciliation first"
        );
        assert!(dedup.payload_conflict_latched(), "latch survives good frames");

        // Leg reconnect: a FRESH physical client (new leg session) joining the same
        // redundancy group must still be refused — the latch is group-level state.
        let reconnect_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let reconnected_leg = conflict_test_source(&reconnect_events);
        let mut after_reconnect = Vec::new();
        dispatch_sequences(
            &reconnected_leg,
            &mut dedup,
            &broadcast_with_l2msg(302, "AA=="),
            &mut after_reconnect,
        );
        assert!(
            after_reconnect.is_empty(),
            "a reconnected leg must not slip frames past the sticky conflict"
        );
        assert!(
            dedup.payload_conflict_latched(),
            "the conflict latch survives leg reconnects"
        );
        assert!(
            reconnect_events
                .lock()
                .unwrap()
                .contains(&"feed_payload_conflict"),
            "every post-latch refusal must re-signal the conflict to the authority owner"
        );
    }

    #[test]
    fn dual_relay_only_revokes_when_all_installed_legs_are_lost() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let primary = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }));
        let secondary = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();

        assert!(
            !signal_dual_unavailable_on_transition(2, 1, &primary, &secondary),
            "one surviving redundant leg preserves global feed availability"
        );
        assert!(events.lock().unwrap().is_empty());
        assert!(
            signal_dual_unavailable_on_transition(1, 0, &primary, &secondary),
            "loss of the final installed leg must revoke authority before waiting"
        );
        assert_eq!(*events.lock().unwrap(), ["dual_relay_unavailable"]);
    }

    #[test]
    fn timestamp_snapshot_counts_underlying_reads_and_resets_delta() {
        let counters = TimestampCounters::default();
        counters.record(Some(10), Some(20));
        counters.record(Some(30), None);
        counters.record(None, None);

        assert_eq!(
            counters.take(),
            RelayTimestampSnapshot {
                reads: 3,
                software: 2,
                hardware: 1,
                missing: 1,
                software_ns: None,
                hardware_ns: None,
            }
        );
        assert_eq!(counters.take(), RelayTimestampSnapshot::default());
    }

    #[test]
    fn timestamped_tcp_stream_is_byte_exact_enabled_and_disabled() {
        use std::os::unix::io::AsRawFd;
        use std::thread;

        const PAYLOAD: &[u8] = b"tls-and-websocket-must-see-these-exact-bytes";
        for network_fine in [false, true] {
            let Some(listener) = bind_loopback_or_skip() else {
                return;
            };
            let addr = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                stream.write_all(PAYLOAD).expect("write exact payload");
            });

            let tcp = TcpStream::connect(addr).expect("connect");
            if network_fine {
                let _ = engine_sysopt::set_so_timestamping(tcp.as_raw_fd());
            }
            let counters = Arc::new(TimestampCounters::default());
            let mut stream = TimestampedTcpStream::new(tcp, network_fine, counters.clone());
            let mut received = [0u8; PAYLOAD.len()];
            stream
                .read_exact(&mut received)
                .expect("read exact payload");
            assert_eq!(&received, PAYLOAD, "NETWORK_FINE={network_fine}");
            server.join().unwrap();

            let snapshot = counters.take();
            if network_fine {
                assert!(snapshot.reads >= 1, "enabled reads must enter recvmsg");
                assert!(snapshot.software <= snapshot.reads);
                assert!(snapshot.hardware <= snapshot.reads);
                assert!(snapshot.missing <= snapshot.reads);
            } else {
                assert_eq!(snapshot, RelayTimestampSnapshot::default());
            }
        }
    }

    #[test]
    fn dual_relay_dedup_exactly_once_first_wins_covers_gaps() {
        // Two relays delivering the SAME sequence stream, interleaved, with the secondary lagging
        // and the primary briefly stalling — the classic dual-relay scenario. Each (single-seq)
        // frame must be forwarded exactly once, in order, with NO gaps and NO full-frame duplicates.
        let primary = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let secondary = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let mut dedup = SeqDedup::default();
        // (relay, seq) arrival order: primary leads 1,2,3; stalls; secondary covers 4,5 (gap fill);
        // primary catches up re-sending 4,5 (dup, must drop) then leads 6,7; secondary re-sends 6 (dup).
        let arrivals = [
            ('A', 1u64),
            ('B', 1),
            ('A', 2),
            ('B', 2),
            ('A', 3),
            ('B', 4),
            ('B', 5), // primary stalled — secondary fills the gap
            ('A', 4),
            ('A', 5), // primary catches up — duplicates, drop
            ('A', 6),
            ('B', 6),
            ('A', 7), // primary leads again; B's 6 is a dup
        ];
        let mut forwarded = vec![];
        for (relay, seq) in arrivals {
            let source = if relay == 'A' { &primary } else { &secondary };
            dispatch_sequences(
                source,
                &mut dedup,
                &broadcast_frame(seq, seq),
                &mut forwarded,
            );
        }
        // Exactly-once, in order, no gaps 1..=7.
        assert_eq!(
            forwarded,
            vec![1, 2, 3, 4, 5, 6, 7],
            "every seq forwarded once, in order, gap-free"
        );
        // Idempotent: old sequences remain dropped, while the next sequence advances only after
        // the callback has received it.
        dispatch_sequences(
            &secondary,
            &mut dedup,
            &broadcast_frame(7, 7),
            &mut forwarded,
        );
        dispatch_sequences(&primary, &mut dedup, &broadcast_frame(3, 3), &mut forwarded);
        dispatch_sequences(
            &secondary,
            &mut dedup,
            &broadcast_frame(8, 8),
            &mut forwarded,
        );
        assert_eq!(forwarded, (1..=8).collect::<Vec<_>>());
    }

    #[test]
    fn dual_relay_dedup_batched_frames() {
        // A partially-overlapping frame must never be forwarded whole: doing so would apply
        // sequences 108 and 109 twice. The message-level dispatcher retains only 110..115.
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let mut dedup = SeqDedup::default();
        let mut emitted = Vec::new();
        for frame in [
            broadcast_frame(100, 109),
            broadcast_frame(100, 105),
            broadcast_frame(108, 115),
            broadcast_frame(110, 115),
            broadcast_frame(116, 116),
        ] {
            dispatch_sequences(&source, &mut dedup, &frame, &mut emitted);
        }

        assert_eq!(emitted, (100..=116).collect::<Vec<_>>());
        assert_eq!(source.partial_overlaps.load(Ordering::Relaxed), 1);
        assert_eq!(source.duplicate_message_total.load(Ordering::Relaxed), 14);
    }

    #[test]
    fn dual_relay_partial_overlap_emits_each_sequence_exactly_once() {
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let mut dedup = SeqDedup::default();
        let mut emitted = Vec::new();

        for frame in [broadcast_frame(100, 109), broadcast_frame(108, 115)] {
            dispatch_dual_frame(&source, &mut dedup, &frame, 1, &mut |cb| {
                emitted.extend(crate::iter_envelopes(cb.frame).map(|env| env.sequence_number));
            });
        }

        assert_eq!(emitted, (100..=115).collect::<Vec<_>>());
        assert_eq!(source.partial_overlaps.load(Ordering::Relaxed), 1);
        assert_eq!(source.duplicate_message_total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn dual_relay_keeps_every_inner_transaction_in_a_new_sequence() {
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let mut dedup = SeqDedup::default();
        // RAW wire-order fixture: the relay puts sequenceNumber BEFORE l2Msg, which is what
        // the allocation-free envelope scanner (shared with the downstream parser) requires.
        // (The old fixture re-serialized via serde_json, whose alphabetized keys put l2Msg
        // first — making the envelope SCAN-INVISIBLE. Such a frame is now REFUSED by the
        // 4b one-aligned-envelope proof: if the scanner can't see the payload here, the
        // downstream parser can't either, and forwarding it would establish
        // highest_complete while silently losing its state-bearing content.)
        // The l2Msg is an opaque batch payload containing two inner transactions. Dedup must
        // never inspect or split this field; forwarding the complete BFM retains both.
        let frame: Vec<u8> = br#"{"version":1,"messages":[{"sequenceNumber":900,"message":{"message":{"header":{"timestamp":1},"l2Msg":"AwR0eC1hBHR4LWI="},"delayedMessagesRead":0},"signatureV2":"opaque-signature","unknownFutureField":{"innerTransactionSentinels":["tx-a","tx-b"]}}]}"#.to_vec();
        let mut forwarded_messages = Vec::new();

        dispatch_dual_frame(&source, &mut dedup, &frame, 1, &mut |cb| {
            assert_eq!(cb.frame, frame.as_slice());
            let value: serde_json::Value = serde_json::from_slice(cb.frame).unwrap();
            forwarded_messages.extend(value["messages"].as_array().unwrap().iter().cloned());
        });

        assert_eq!(forwarded_messages.len(), 1, "the complete BFM must forward unsplit");
        assert_eq!(forwarded_messages[0]["sequenceNumber"], 900);
        assert_eq!(
            forwarded_messages[0]["message"]["message"]["l2Msg"], "AwR0eC1hBHR4LWI=",
            "the opaque multi-tx payload must be retained byte-intact"
        );
        assert_eq!(
            forwarded_messages[0]["unknownFutureField"]["innerTransactionSentinels"][1], "tx-b",
            "unknown fields must survive forwarding"
        );
        assert_eq!(dedup.highest_complete_sequence(), Some(900));
    }

    #[test]
    fn dual_relay_forward_gap_fails_closed_until_missing_sequence_arrives() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let source = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
            }));
        let mut dedup = SeqDedup::default();
        let mut emitted = Vec::new();

        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_frame(100, 100),
            &mut emitted,
        );
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_frame(102, 102),
            &mut emitted,
        );
        assert_eq!(emitted, vec![100]);
        assert_eq!(dedup.highest_complete_sequence(), Some(100));
        assert_eq!(source.sequence_gaps.load(Ordering::Relaxed), 1);
        assert_eq!(
            *events.lock().unwrap(),
            ["dual_relay_forward_gap"],
            "a rejected forward gap must revoke submission authority"
        );

        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_frame(101, 101),
            &mut emitted,
        );
        dispatch_sequences(
            &source,
            &mut dedup,
            &broadcast_frame(102, 102),
            &mut emitted,
        );
        assert_eq!(emitted, vec![100, 101, 102]);
        assert_eq!(dedup.highest_complete_sequence(), Some(102));
    }

    #[test]
    fn dual_relay_counters_track_gaps_per_physical_relay() {
        let primary = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let secondary = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();

        let _ = primary.track_sequence(&broadcast_frame(100, 101));
        let _ = primary.track_sequence(&broadcast_frame(104, 104));
        // An old replay must not regress the relay-local high-water or create a phantom gap when
        // the next contiguous sequence arrives.
        let _ = primary.track_sequence(&broadcast_frame(90, 91));
        let _ = primary.track_sequence(&broadcast_frame(105, 105));

        let _ = secondary.track_sequence(&broadcast_frame(100, 101));
        let _ = secondary.track_sequence(&broadcast_frame(102, 104));

        assert_eq!(primary.sequence_gaps.load(Ordering::Relaxed), 2);
        assert_eq!(primary.last_seq.load(Ordering::Relaxed), 105);
        assert_eq!(secondary.sequence_gaps.load(Ordering::Relaxed), 0);
        assert_eq!(secondary.last_seq.load(Ordering::Relaxed), 104);
    }

    #[test]
    fn blackholed_primary_reconnect_does_not_pause_healthy_relay() {
        use std::thread;

        const FRAME_COUNT: u64 = 30;
        const STEADY_INTERVAL: Duration = Duration::from_millis(20);

        let Some(primary_listener) = bind_loopback_or_skip() else {
            return;
        };
        let primary_port = primary_listener.local_addr().unwrap().port();
        let Some(secondary_listener) = bind_loopback_or_skip() else {
            return;
        };
        let secondary_port = secondary_listener.local_addr().unwrap().port();
        let sent = Arc::new(AtomicU64::new(0));
        let blackhole_window = Arc::new(std::sync::Mutex::new(None));

        let sent_for_primary = sent.clone();
        let blackhole_window_for_primary = blackhole_window.clone();
        let primary_server = thread::spawn(move || {
            let (stream, _) = primary_listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            while sent_for_primary.load(Ordering::Acquire) < 12 {
                thread::sleep(Duration::from_millis(1));
            }
            let _ = ws.close(None);
            drop(ws);

            // Accept the reconnect TCP connection but deliberately never complete its WebSocket
            // handshake. The healthy leg must continue to be polled throughout this interval.
            let (blackhole, _) = primary_listener.accept().unwrap();
            let blackhole_start = Instant::now();
            thread::sleep(Duration::from_millis(250));
            let blackhole_end = Instant::now();
            *blackhole_window_for_primary.lock().unwrap() = Some((blackhole_start, blackhole_end));
            drop(blackhole);
        });

        let sent_for_secondary = sent.clone();
        let secondary_server = thread::spawn(move || {
            let (stream, _) = secondary_listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            for seq in 1..=FRAME_COUNT {
                ws.send(Message::Text(
                    String::from_utf8(broadcast_frame(seq, seq)).unwrap(),
                ))
                .unwrap();
                sent_for_secondary.store(seq, Ordering::Release);
                thread::sleep(STEADY_INTERVAL);
            }
            let _ = ws.close(None);
        });

        let primary = WsRelayClient::new(&format!("ws://127.0.0.1:{primary_port}/feed")).unwrap();
        let secondary =
            WsRelayClient::new(&format!("ws://127.0.0.1:{secondary_port}/feed")).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut sequences = Vec::new();
        let mut arrivals = Vec::new();

        WsRelayClient::run_dual(&primary, &secondary, &shutdown, |cb| {
            for env in crate::iter_envelopes(cb.frame) {
                sequences.push(env.sequence_number);
                arrivals.push(Instant::now());
                if env.sequence_number == FRAME_COUNT {
                    shutdown.store(true, Ordering::Release);
                }
            }
        })
        .unwrap();

        primary_server.join().unwrap();
        secondary_server.join().unwrap();
        assert_eq!(sequences, (1..=FRAME_COUNT).collect::<Vec<_>>());
        assert!(
            primary.reconnects.load(Ordering::Relaxed) >= 1,
            "the primary replacement attempt must be counted"
        );

        let gaps = arrivals
            .windows(2)
            .map(|pair| (pair[0], pair[1], pair[1].duration_since(pair[0])))
            .collect::<Vec<_>>();
        let mut baseline = gaps[..10]
            .iter()
            .map(|(_, _, duration)| *duration)
            .collect::<Vec<_>>();
        baseline.sort_unstable();
        let steady_p99 = *baseline.last().unwrap();
        let bound = Duration::from_millis(5).max(steady_p99.saturating_mul(2));
        let (blackhole_start, blackhole_end) = blackhole_window.lock().unwrap().unwrap();
        let max_pause = gaps
            .into_iter()
            .filter(|(start, end, _)| *start < blackhole_end && *end > blackhole_start)
            .map(|(_, _, duration)| duration)
            .max()
            .expect("healthy callbacks must span the primary blackhole");
        assert!(
            max_pause <= bound,
            "healthy relay paused for {max_pause:?}; bound from steady p99 is {bound:?}"
        );
    }

    #[test]
    fn parse_seq_quoted() {
        let raw = br#"{"sequenceNumber":"77"}"#;
        assert_eq!(parse_first_sequence_number(raw), Some(77));
    }

    #[test]
    fn parse_seq_missing() {
        let raw = br#"{"foo":1}"#;
        assert_eq!(parse_first_sequence_number(raw), None);
    }

    #[test]
    fn parse_last_seq_picks_final_message() {
        let raw = br#"{"messages":[{"sequenceNumber":1000},{"sequenceNumber":1001},{"sequenceNumber":1002}]}"#;
        assert_eq!(parse_first_sequence_number(raw), Some(1000));
        assert_eq!(parse_last_sequence_number(raw), Some(1002));
    }

    #[test]
    fn batched_frames_do_not_report_phantom_gap() {
        // Frame A covers 1000..=1002, frame B covers 1003..=1004 — contiguous across the
        // boundary, so no gap even though each frame's first-seq jumps by the batch stride.
        let c = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let a = br#"{"messages":[{"sequenceNumber":1000},{"sequenceNumber":1001},{"sequenceNumber":1002}]}"#;
        let b = br#"{"messages":[{"sequenceNumber":1003},{"sequenceNumber":1004}]}"#;
        let _ = c.track_sequence(a);
        let _ = c.track_sequence(b);
        assert_eq!(c.sequence_gaps.load(Ordering::Relaxed), 0);
        assert_eq!(c.last_seq.load(Ordering::Relaxed), 1004);
    }

    #[test]
    fn genuine_discontinuity_reports_gap() {
        // Frame A ends at 1002; frame B starts at 1006 — 1003,1004,1005 are missing (gap=3).
        let c = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        let a = br#"{"messages":[{"sequenceNumber":1000},{"sequenceNumber":1001},{"sequenceNumber":1002}]}"#;
        let b = br#"{"messages":[{"sequenceNumber":1006},{"sequenceNumber":1007}]}"#;
        let _ = c.track_sequence(a);
        let _ = c.track_sequence(b);
        assert_eq!(c.sequence_gaps.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn relay_client_constructs() {
        let c = WsRelayClient::new(DEFAULT_LOCAL_RELAY_WS).unwrap();
        assert_eq!(c.last_seq.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn relay_invalid_url_rejected() {
        let r = WsRelayClient::new("not-a-url");
        assert!(matches!(r, Err(RelayError::InvalidUrl(_))));
    }

    /// End-to-end test that boots a local WebSocket "relay" stub on a free port,
    /// pushes two frames, and verifies the client receives both with monotonic
    /// sequence numbers.
    #[test]
    fn relay_receives_two_frames_local_loopback() {
        use std::thread;
        use std::time::Duration;

        // Bind a TCP listener on a free port.
        let Some(listener) = bind_loopback_or_skip() else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        let endpoint = format!("ws://127.0.0.1:{port}/feed");

        // Stub relay server: accept one connection, complete a WebSocket handshake,
        // emit two text frames, then close.
        let server_thread = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            stream.set_nodelay(true).ok();
            let mut ws = tungstenite::accept(stream).expect("accept ws");
            ws.send(Message::Text(
                r#"{"sequenceNumber":1,"timestamp":1,"blockNumber":1,"l2Msg":"AA=="}"#.into(),
            ))
            .ok();
            ws.send(Message::Text(
                r#"{"sequenceNumber":2,"timestamp":2,"blockNumber":2,"l2Msg":"AA=="}"#.into(),
            ))
            .ok();
            thread::sleep(Duration::from_millis(50));
            ws.close(None).ok();
        });

        let client = WsRelayClient::new(&endpoint).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicU64::new(0));
        let received_clone = received.clone();
        let shutdown_for_run = shutdown.clone();
        let client_thread = thread::spawn(move || {
            let mut frames_seen = 0u32;
            let _ = client.run(&shutdown_for_run, |cb| {
                frames_seen += 1;
                received_clone.fetch_add(1, Ordering::Relaxed);
                assert!(!cb.frame.is_empty());
                if frames_seen >= 2 {
                    shutdown_for_run.store(true, Ordering::Release);
                }
            });
        });

        // Allow up to 2s for both frames to arrive.
        for _ in 0..200 {
            if received.load(Ordering::Relaxed) >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        shutdown.store(true, Ordering::Release);
        let _ = client_thread.join();
        let _ = server_thread.join();
        assert_eq!(received.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn single_relay_disconnect_revokes_authority_before_reconnect() {
        use std::thread;

        let Some(listener) = bind_loopback_or_skip() else {
            return;
        };
        let endpoint = format!(
            "ws://127.0.0.1:{}/feed",
            listener.local_addr().unwrap().port()
        );
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut ws = tungstenite::accept(stream).expect("accept ws");
            ws.send(Message::Text(
                r#"{"sequenceNumber":1,"timestamp":1,"blockNumber":1,"l2Msg":"AA=="}"#.into(),
            ))
            .unwrap();
            ws.close(None).unwrap();
        });

        let shutdown = Arc::new(AtomicBool::new(false));
        let callback_shutdown = shutdown.clone();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_events = events.clone();
        let client = WsRelayClient::new(&endpoint)
            .unwrap()
            .with_degradation_callback(Arc::new(move |reason| {
                callback_events.lock().unwrap().push(reason.as_str());
                callback_shutdown.store(true, Ordering::Release);
            }));
        let frame_events = events.clone();
        client
            .run(&shutdown, |_| frame_events.lock().unwrap().push("frame"))
            .unwrap();
        server.join().unwrap();

        assert_eq!(
            *events.lock().unwrap(),
            ["frame", "single_relay_disconnect"],
            "disconnect must revoke authority before any reconnect attempt"
        );
        assert_eq!(
            client.reconnects.load(Ordering::Relaxed),
            0,
            "revocation callback completed and stopped the driver before reconnect"
        );
    }
}
