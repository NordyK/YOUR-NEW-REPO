//! Feed sidecar — the S2 binary-fast-path emitter (sequencer-latency report §4, Option A).
//!
//! Consumes the local nitro relay's JSON-over-WebSocket feed ONCE, decodes each signed-tx
//! (envelope scan → base64 → recursive RLP), and writes a fixed binary `FeedFrame` per tx over a
//! Unix socket. The engine, configured with `relay_endpoint = "binunix:<sock>"`, reads those frames
//! with zero JSON / base64 / RLP / WebSocket framing — the recoverable S2 latency the report targets.
//!
//! On AWS this can be replaced by (or co-exist with) a forked nitro relay emitting the same frames;
//! on the laptop it runs against the local relay (or the public feed for a dry run) over the local
//! network. Single-writer (this sidecar) / single-reader (the engine) — LMAX single-writer principle.
//!
//! Usage:
//!   relay_sidecar [--sock /tmp/mantis-feed.sock] [--upstream ws://127.0.0.1:9642/feed] \
//!                 [--upstream2 ws://127.0.0.1:9643/feed]   # dual-relay (roadmap #2): seq-deduped
//!
//! The engine then runs with `relay_endpoint=binunix:/tmp/mantis-feed.sock`.

use std::io::{BufWriter, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_feed::{
    iter_envelopes, WsRelayClient,
};
use rtrb::{Consumer, Producer, RingBuffer};
use socket2::{Domain, SockAddr, Socket, Type};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalTransport {
    Stream,
    SeqPacket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalEndpoint {
    transport: LocalTransport,
    path: String,
}

impl LocalEndpoint {
    fn parse(raw: &str) -> Self {
        if let Some(path) = raw.strip_prefix("binseq:") {
            Self {
                transport: LocalTransport::SeqPacket,
                path: path.to_string(),
            }
        } else {
            Self {
                transport: LocalTransport::Stream,
                path: raw.strip_prefix("binunix:").unwrap_or(raw).to_string(),
            }
        }
    }
}

enum FeedListener {
    Stream(UnixListener),
    SeqPacket(Socket),
}

impl FeedListener {
    fn bind(endpoint: &LocalEndpoint) -> std::io::Result<Self> {
        match endpoint.transport {
            LocalTransport::Stream => UnixListener::bind(&endpoint.path).map(Self::Stream),
            LocalTransport::SeqPacket => {
                let socket = Socket::new(Domain::UNIX, Type::SEQPACKET, None)?;
                socket.bind(&SockAddr::unix(&endpoint.path)?)?;
                socket.listen(1)?;
                Ok(Self::SeqPacket(socket))
            }
        }
    }

    fn accept(&self) -> std::io::Result<FeedConnection> {
        match self {
            Self::Stream(listener) => listener.accept().map(|(s, _)| FeedConnection::Stream(s)),
            Self::SeqPacket(listener) => {
                listener.accept().map(|(s, _)| FeedConnection::SeqPacket(s))
            }
        }
    }

    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        match self {
            Self::Stream(listener) => listener.set_nonblocking(nonblocking),
            Self::SeqPacket(listener) => listener.set_nonblocking(nonblocking),
        }
    }
}

enum FeedConnection {
    Stream(UnixStream),
    SeqPacket(Socket),
}

enum FeedShutdownHandle {
    Stream(UnixStream),
    SeqPacket(Socket),
}

// A complete V1-sized record with an intentionally unsupported zero magic. If `shutdown(2)` is
// unavailable (for example under a restrictive seccomp profile), delivering this record still
// wakes the engine and makes its existing unknown-magic/invalid-record path revoke feed authority.
// It is control-plane evidence only: the degraded gate below prevents every later data frame.
const FEED_DEGRADATION_SENTINEL: [u8; engine_feed::FEED_FRAME_HEADER_LEN] =
    [0; engine_feed::FEED_FRAME_HEADER_LEN];

impl FeedShutdownHandle {
    fn shutdown(&self, reason: engine_feed::FeedDegradationReason) -> std::io::Result<()> {
        // Signal the peer first. A full invalid header is deterministic for both transports:
        // SOCK_SEQPACKET receives one malformed record, while the stream decoder reads a complete
        // header and rejects its magic without waiting for more bytes.
        let sentinel: &[u8] = if matches!(
            reason,
            engine_feed::FeedDegradationReason::FeedPayloadConflict
        ) {
            &engine_feed::feed_frame::FEED_PAYLOAD_CONFLICT_SENTINEL
        } else {
            &FEED_DEGRADATION_SENTINEL
        };
        let signal = match self {
            Self::Stream(stream) => {
                let mut stream = stream;
                stream.write_all(sentinel)
            }
            Self::SeqPacket(socket) => match engine_sysopt::send_packet_once(
                socket.as_raw_fd(),
                sentinel,
            ) {
                Ok(n) if n == sentinel.len() => Ok(()),
                Ok(n) => Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    format!(
                        "short feed-degradation sentinel: {n}/{}",
                        sentinel.len()
                    ),
                )),
                Err(error) => Err(error),
            },
        };
        let shutdown = match self {
            Self::Stream(stream) => stream.shutdown(std::net::Shutdown::Both),
            Self::SeqPacket(socket) => socket.shutdown(std::net::Shutdown::Both),
        };

        // Either mechanism independently revokes the peer: the sentinel trips its decoder latch;
        // shutdown produces EOF. Fail only when neither could be delivered.
        match (signal, shutdown) {
            (Ok(()), _) | (_, Ok(())) => Ok(()),
            (Err(signal), Err(shutdown)) => Err(std::io::Error::other(format!(
                "feed degradation signal failed ({signal}); shutdown failed ({shutdown})"
            ))),
        }
    }
}

impl FeedConnection {
    fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Stream(stream) => stream.set_write_timeout(timeout),
            Self::SeqPacket(socket) => socket.set_write_timeout(timeout),
        }
    }

    fn shutdown_handle(&self) -> std::io::Result<FeedShutdownHandle> {
        match self {
            Self::Stream(stream) => stream.try_clone().map(FeedShutdownHandle::Stream),
            Self::SeqPacket(socket) => socket.try_clone().map(FeedShutdownHandle::SeqPacket),
        }
    }

    fn send_frame(&mut self, frame: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Stream(stream) => stream.write_all(frame),
            Self::SeqPacket(socket) => {
                if frame.len() > SEQPACKET_FRAME_BYTES {
                    // Deliver one deliberately over-cap record so the engine's fixed 64 KiB
                    // recvmsg observes MSG_TRUNC and irreversibly degrades feed authority. Merely
                    // returning an error here would close/reconnect the sidecar while silently
                    // omitting a state-bearing transaction -- a fail-open outcome. Bounding the
                    // sentinel to cap+1 also avoids relying on the host's maximum AF_UNIX packet
                    // size when the upstream frame itself is very large.
                    let sentinel = &frame[..=SEQPACKET_FRAME_BYTES];
                    let sent = engine_sysopt::send_packet_once(socket.as_raw_fd(), sentinel)?;
                    return if sent == sentinel.len() {
                        Ok(())
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            format!(
                                "short SOCK_SEQPACKET oversize sentinel: {sent}/{}",
                                sentinel.len()
                            ),
                        ))
                    };
                }
                // One V3 frame is one record. A retry after a partial send would create a second
                // record, so accept only an exact one-shot send.
                let sent = engine_sysopt::send_packet_once(socket.as_raw_fd(), frame)?;
                if sent == frame.len() {
                    Ok(())
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        format!("short SOCK_SEQPACKET send: {sent}/{}", frame.len()),
                    ))
                }
            }
        }
    }
}

/// Wall-clock ns of the last upstream WS frame seen while live-pumping (watchdog input).
static LAST_FRAME_NS: AtomicU64 = AtomicU64::new(0);
/// True only while `pump_feed` is actively consuming the upstream (the watchdog is disarmed
/// while idling between engine connections — no upstream is being read then, so silence is normal).
static PUMP_LIVE: AtomicBool = AtomicBool::new(false);

fn sidecar_degradation_callback(
    degraded: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    feed_shutdown: Arc<FeedShutdownHandle>,
) -> Arc<dyn Fn(engine_feed::FeedDegradationReason) + Send + Sync> {
    Arc::new(move |reason| {
        // Release-publish degradation, then synchronously send the live engine a fail-closed
        // decoder signal and attempt socket shutdown before stopping/joining relay workers. The
        // receive callback's acquire gate rejects the gap-bearing frame; the sentinel revokes the
        // engine even under a seccomp profile that denies shutdown(2), while ordinary hosts also
        // observe EOF without waiting for FeedConnection::drop.
        let first = !degraded.swap(true, Ordering::AcqRel);
        if let Err(error) = feed_shutdown.shutdown(reason) {
            eprintln!("relay_sidecar: engine feed shutdown after degradation failed: {error}");
        }
        shutdown.store(true, Ordering::Release);
        if first {
            eprintln!(
                "relay_sidecar: FEED_STATE_DEGRADED reason={}; closing engine feed before further bytes",
                reason.as_str()
            );
        }
    })
}

#[inline]
fn sidecar_feed_delivery_allowed(degraded: &AtomicBool) -> bool {
    !degraded.load(Ordering::Acquire)
}

#[derive(Clone)]
struct CaptureWriterControl {
    shutdown: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    thread: std::thread::Thread,
}

/// Process-local bridge used only by the sidecar's own silence watchdog. It lets that controlled
/// exit drain+flush async evidence before `process::exit(2)` without unsafe signal handlers.
static CAPTURE_WRITER_CONTROL: std::sync::Mutex<Option<CaptureWriterControl>> =
    std::sync::Mutex::new(None);

/// Number of preallocated raw-capture buffers exchanged between the feed and writer threads.
const CAPTURE_QUEUE_SLOTS: usize = 256;
/// Hard bound for one captured upstream WS frame. The checked-in Gate8 corpora top out below 94 KiB;
/// 128 KiB keeps the common path allocation-free while making a future oversize an explicit evidence
/// drop instead of growing memory on the feed core.
const CAPTURE_SLOT_BYTES: usize = 128 * 1024;
/// A BroadcastMessage normally contains only a handful of envelopes. Bound the joined freshness rows
/// as well; exceeding this invalidates capture evidence but never changes feed forwarding.
const CAPTURE_FRESHNESS_ROWS: usize = 256;
/// Corpus-backed maximum accepted by the engine's optional seqpacket receiver.
const SEQPACKET_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMode {
    Sync,
    Async,
    Off,
}

impl CaptureMode {
    fn parse(value: Option<&str>) -> std::io::Result<Self> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("sync") => Ok(Self::Sync),
            Some("async") => Ok(Self::Async),
            Some("off") => Ok(Self::Off),
            Some(other) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid CAPTURE_MODE={other:?}; expected sync|async|off"),
            )),
        }
    }

    fn from_env() -> std::io::Result<Self> {
        let value = std::env::var("CAPTURE_MODE").ok();
        Self::parse(value.as_deref())
    }
}

#[derive(Debug)]
struct CaptureStats {
    enqueued: AtomicU64,
    dropped: AtomicU64,
    queue_depth: AtomicU64,
    writer_stall_ns: AtomicU64,
    evidence_valid: AtomicBool,
}

impl Default for CaptureStats {
    fn default() -> Self {
        Self {
            enqueued: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            queue_depth: AtomicU64::new(0),
            writer_stall_ns: AtomicU64::new(0),
            evidence_valid: AtomicBool::new(true),
        }
    }
}

impl CaptureStats {
    #[inline]
    fn invalidate_one(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        self.evidence_valid.store(false, Ordering::Release);
    }

    fn report(&self, mode: CaptureMode) {
        eprintln!(
            "relay_sidecar: capture_metrics mode={} capture_enqueued_total={} \
             capture_dropped_total={} capture_queue_depth={} capture_writer_stall_ns={} \
             evidence_valid={}",
            match mode {
                CaptureMode::Sync => "sync",
                CaptureMode::Async => "async",
                CaptureMode::Off => "off",
            },
            self.enqueued.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            self.queue_depth.load(Ordering::Relaxed),
            self.writer_stall_ns.load(Ordering::Relaxed),
            self.evidence_valid.load(Ordering::Acquire),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FreshnessRow {
    seq: u64,
    sequencer_timestamp: u64,
    capture_recv_ns: u64,
}

fn write_freshness_row<W: Write>(writer: &mut W, row: FreshnessRow) -> std::io::Result<()> {
    let recv_ms = row.capture_recv_ns / 1_000_000;
    let seq_ms = row.sequencer_timestamp.saturating_mul(1_000);
    let event_age_ms = recv_ms.saturating_sub(seq_ms);
    writeln!(
        writer,
        "{{\"seq\":{},\"sequencer_timestamp\":{},\"capture_recv_ns\":{},\"event_age_ms\":{}}}",
        row.seq, row.sequencer_timestamp, row.capture_recv_ns, event_age_ms
    )
}

#[derive(Debug)]
struct CaptureSlot {
    id: usize,
    raw: Vec<u8>,
    freshness: Vec<FreshnessRow>,
    write: bool,
}

impl CaptureSlot {
    fn new(id: usize) -> Self {
        Self {
            id,
            raw: Vec::with_capacity(CAPTURE_SLOT_BYTES),
            freshness: Vec::with_capacity(CAPTURE_FRESHNESS_ROWS),
            write: true,
        }
    }

    fn reset(&mut self) {
        self.raw.clear();
        self.freshness.clear();
        self.write = true;
    }
}

struct SyncCapture {
    raw: std::fs::File,
    freshness: std::fs::File,
    invalid_path: String,
    stats: Arc<CaptureStats>,
    frame_active: bool,
    frame_failed: bool,
}

impl SyncCapture {
    fn fail_current_frame(&mut self) {
        if !self.frame_failed {
            self.frame_failed = true;
            self.stats.invalidate_one();
        }
    }

    fn timed_write<F>(&mut self, write: F)
    where
        F: FnOnce(&mut Self) -> std::io::Result<()>,
    {
        let started = Instant::now();
        let result = write(self);
        self.stats
            .writer_stall_ns
            .fetch_max(elapsed_ns(started), Ordering::Relaxed);
        if result.is_err() {
            self.fail_current_frame();
        }
    }
}

impl Drop for SyncCapture {
    fn drop(&mut self) {
        if self.raw.flush().is_err() || self.freshness.flush().is_err() {
            self.fail_current_frame();
        }
        persist_invalid_marker(&self.invalid_path, &self.stats);
        self.stats.report(CaptureMode::Sync);
    }
}

struct AsyncCapture {
    free: Consumer<CaptureSlot>,
    ready: Producer<CaptureSlot>,
    pending: Option<CaptureSlot>,
    stats: Arc<CaptureStats>,
    shutdown: Arc<AtomicBool>,
    writer_thread: std::thread::Thread,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl AsyncCapture {
    fn invalidate_one(&self) {
        self.stats.invalidate_one();
        self.writer_thread.unpark();
    }

    fn finish_frame(&mut self) {
        let Some(slot) = self.pending.take() else {
            return;
        };
        let count_enqueued = slot.write;
        // Increment before publishing: the consumer cannot pop until the release-store performed by
        // `push`, so its decrement can never race ahead and underflow this exact occupancy gauge.
        self.stats.queue_depth.fetch_add(1, Ordering::Relaxed);
        match self.ready.push(slot) {
            Ok(()) => {
                if count_enqueued {
                    self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                }
                self.writer_thread.unpark();
            }
            Err(rtrb::PushError::Full(slot)) => {
                self.stats.queue_depth.fetch_sub(1, Ordering::Relaxed);
                // Impossible if the free/ready ownership invariant holds: acquiring one of N free
                // slots proves at most N-1 slots are in ready. Never overwrite; retire the slot and
                // invalidate evidence if the invariant is ever violated.
                let _retired_slot_id = slot.id;
                self.invalidate_one();
            }
        }
    }
}

impl Drop for AsyncCapture {
    fn drop(&mut self) {
        // Normal shutdown, the synchronous sigwait path, and every test-owned lifecycle drain all
        // already-published slots, flush both BufWriters, and join before returning.
        self.finish_frame();
        self.shutdown.store(true, Ordering::Release);
        self.writer_thread.unpark();
        if let Some(writer) = self.writer.take() {
            if writer.join().is_err() {
                self.stats.invalidate_one();
            }
        }
        if let Ok(mut control) = CAPTURE_WRITER_CONTROL.lock() {
            if control
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(&c.shutdown, &self.shutdown))
            {
                *control = None;
            }
        }
    }
}

enum CaptureSink {
    Disabled,
    Sync(SyncCapture),
    Async(AsyncCapture),
}

impl CaptureSink {
    fn open(mode: CaptureMode, path: Option<&str>) -> std::io::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::Disabled);
        };
        if mode == CaptureMode::Off {
            eprintln!("relay_sidecar: capture disabled by CAPTURE_MODE=off");
            return Ok(Self::Disabled);
        }

        let freshness_path = format!("{path}.freshness.jsonl");
        let invalid_path = format!("{path}.invalid");
        // The raw/freshness files are truncated for a new run; clear only their matching stale
        // invalid marker at the same startup boundary. A new drop/write failure recreates it.
        match std::fs::remove_file(&invalid_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let open = |p: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(p)
        };
        let raw = open(path)?;
        let freshness = open(&freshness_path)?;
        let stats = Arc::new(CaptureStats::default());

        match mode {
            CaptureMode::Sync => {
                eprintln!("relay_sidecar: capturing raw feed JSONL synchronously to {path}");
                Ok(Self::Sync(SyncCapture {
                    raw,
                    freshness,
                    invalid_path,
                    stats,
                    frame_active: false,
                    frame_failed: false,
                }))
            }
            CaptureMode::Async => {
                eprintln!("relay_sidecar: capturing raw feed JSONL asynchronously to {path}");
                let (mut free_producer, free) = RingBuffer::<CaptureSlot>::new(CAPTURE_QUEUE_SLOTS);
                for id in 0..CAPTURE_QUEUE_SLOTS {
                    free_producer
                        .push(CaptureSlot::new(id))
                        .expect("new free-slot ring has exact advertised capacity");
                }
                let (ready, ready_consumer) = RingBuffer::<CaptureSlot>::new(CAPTURE_QUEUE_SLOTS);
                let shutdown = Arc::new(AtomicBool::new(false));
                let done = Arc::new(AtomicBool::new(false));
                let writer_stats = stats.clone();
                let writer_shutdown = shutdown.clone();
                let writer_done = done.clone();
                let writer_invalid_path = invalid_path.clone();
                let writer = std::thread::Builder::new()
                    .name("capture-writer".into())
                    .spawn(move || {
                        engine::runtime::pin_to_core(0, "capture-writer");
                        capture_writer_loop(
                            BufWriter::with_capacity(1024 * 1024, raw),
                            BufWriter::with_capacity(256 * 1024, freshness),
                            free_producer,
                            ready_consumer,
                            writer_stats,
                            writer_shutdown,
                            writer_done,
                            writer_invalid_path,
                        );
                    })?;
                let writer_thread = writer.thread().clone();
                if let Ok(mut control) = CAPTURE_WRITER_CONTROL.lock() {
                    *control = Some(CaptureWriterControl {
                        shutdown: shutdown.clone(),
                        done,
                        thread: writer_thread.clone(),
                    });
                }
                Ok(Self::Async(AsyncCapture {
                    free,
                    ready,
                    pending: None,
                    stats,
                    shutdown,
                    writer_thread,
                    writer: Some(writer),
                }))
            }
            CaptureMode::Off => unreachable!("handled above"),
        }
    }

    fn capture_raw_frame(&mut self, frame: &[u8]) {
        match self {
            Self::Disabled => {}
            Self::Sync(capture) => {
                debug_assert!(
                    !capture.frame_active,
                    "capture frame must be finished exactly once"
                );
                capture.frame_active = true;
                capture.frame_failed = false;
                capture.timed_write(|c| {
                    c.raw.write_all(frame)?;
                    c.raw.write_all(b"\n")
                });
            }
            Self::Async(capture) => {
                debug_assert!(
                    capture.pending.is_none(),
                    "capture frame must be finished exactly once"
                );
                if frame.len() > CAPTURE_SLOT_BYTES {
                    capture.invalidate_one();
                    return;
                }
                let Ok(mut slot) = capture.free.pop() else {
                    capture.invalidate_one();
                    return;
                };
                slot.reset();
                slot.raw.extend_from_slice(frame);
                capture.pending = Some(slot);
            }
        }
    }

    fn capture_freshness(&mut self, seq: u64, sequencer_timestamp: u64, capture_recv_ns: u64) {
        let row = FreshnessRow {
            seq,
            sequencer_timestamp,
            capture_recv_ns,
        };
        match self {
            Self::Disabled => {}
            Self::Sync(capture) => {
                capture.timed_write(|c| write_freshness_row(&mut c.freshness, row));
            }
            Self::Async(capture) => {
                let Some(slot) = capture.pending.as_mut() else {
                    return;
                };
                if !slot.write {
                    return;
                }
                if slot.freshness.len() == CAPTURE_FRESHNESS_ROWS {
                    slot.write = false;
                    capture.invalidate_one();
                    return;
                }
                slot.freshness.push(row);
            }
        }
    }

    fn finish_frame(&mut self) {
        match self {
            Self::Disabled => {}
            Self::Sync(capture) => {
                if capture.frame_active && !capture.frame_failed {
                    capture.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                }
                capture.frame_active = false;
            }
            Self::Async(capture) => capture.finish_frame(),
        }
    }

    fn invalidate_evidence(&self) {
        match self {
            Self::Disabled => {}
            Self::Sync(capture) => capture.stats.invalidate_one(),
            Self::Async(capture) => capture.stats.invalidate_one(),
        }
    }

    #[cfg(test)]
    fn stats(&self) -> Option<&Arc<CaptureStats>> {
        match self {
            Self::Disabled => None,
            Self::Sync(capture) => Some(&capture.stats),
            Self::Async(capture) => Some(&capture.stats),
        }
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn persist_invalid_marker(path: &str, stats: &CaptureStats) {
    if stats.evidence_valid.load(Ordering::Acquire) {
        return;
    }
    let dropped = stats.dropped.load(Ordering::Relaxed);
    let result = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .and_then(|mut marker| {
            writeln!(
                marker,
                "capture_evidence_invalid=1 capture_dropped_total={dropped}"
            )
        });
    if let Err(err) = result {
        eprintln!("relay_sidecar: failed to persist capture invalid marker {path}: {err}");
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_writer_loop(
    mut raw: BufWriter<std::fs::File>,
    mut freshness: BufWriter<std::fs::File>,
    mut free: Producer<CaptureSlot>,
    mut ready: Consumer<CaptureSlot>,
    stats: Arc<CaptureStats>,
    shutdown: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    invalid_path: String,
) {
    let mut last_report = Instant::now();
    let mut invalid_marker_written = false;
    loop {
        let mut progressed = false;
        while let Ok(mut slot) = ready.pop() {
            progressed = true;
            stats.queue_depth.fetch_sub(1, Ordering::Relaxed);
            if slot.write {
                let started = Instant::now();
                let result = (|| -> std::io::Result<()> {
                    raw.write_all(&slot.raw)?;
                    raw.write_all(b"\n")?;
                    for row in &slot.freshness {
                        write_freshness_row(&mut freshness, *row)?;
                    }
                    Ok(())
                })();
                stats
                    .writer_stall_ns
                    .fetch_max(elapsed_ns(started), Ordering::Relaxed);
                if result.is_err() {
                    stats.invalidate_one();
                }
            }
            slot.reset();
            if let Err(rtrb::PushError::Full(slot)) = free.push(slot) {
                let _retired_slot_id = slot.id;
                stats.invalidate_one();
            }
        }

        if !stats.evidence_valid.load(Ordering::Acquire) && !invalid_marker_written {
            persist_invalid_marker(&invalid_path, &stats);
            invalid_marker_written = true;
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            // Bound user-space BufWriter exposure between graceful-signal drains. The flush and
            // metrics snapshot are entirely on housekeeping core 0.
            let started = Instant::now();
            if raw.flush().is_err() || freshness.flush().is_err() {
                stats.invalidate_one();
            }
            stats
                .writer_stall_ns
                .fetch_max(elapsed_ns(started), Ordering::Relaxed);
            stats.report(CaptureMode::Async);
            last_report = Instant::now();
        }
        if shutdown.load(Ordering::Acquire) && ready.is_empty() {
            break;
        }
        if !progressed {
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }

    let started = Instant::now();
    if raw.flush().is_err() || freshness.flush().is_err() {
        stats.invalidate_one();
    }
    stats
        .writer_stall_ns
        .fetch_max(elapsed_ns(started), Ordering::Relaxed);
    persist_invalid_marker(&invalid_path, &stats);
    stats.report(CaptureMode::Async);
    done.store(true, Ordering::Release);
}

fn drain_async_capture_before_watchdog_exit(timeout: Duration) {
    let control = CAPTURE_WRITER_CONTROL
        .lock()
        .ok()
        .and_then(|control| control.clone());
    let Some(control) = control else {
        return;
    };
    control.shutdown.store(true, Ordering::Release);
    control.thread.unpark();
    let started = Instant::now();
    while !control.done.load(Ordering::Acquire) && started.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(1));
    }
    if !control.done.load(Ordering::Acquire) {
        eprintln!("relay_sidecar: capture writer did not drain before watchdog exit timeout");
    }
}

/// Watchdog decision: has the live pump been silent longer than `max_silence_s`?
/// Factored out of the thread so the tripwire logic is unit-testable.
fn silence_exceeded(last_frame_ns: u64, now_ns: u64, max_silence_s: u64) -> bool {
    last_frame_ns != 0 && now_ns.saturating_sub(last_frame_ns) > max_silence_s * 1_000_000_000
}

/// The observed 24/7 killer (2026-07 box run): the upstream relay connection stays OPEN but goes
/// SILENT — the sidecar blocks in the WS read looking healthy, forwards nothing, and the engine
/// starves into its ENGINE_NO_FEED kill. `Restart=always` can't help a hung-but-running process,
/// so the sidecar must turn "silent upstream" into "dead process": exit(2) after
/// `SIDECAR_MAX_SILENCE_S` (default 30 s; 0 disables) of no upstream frames while live-pumping,
/// and systemd relaunches it fresh (which re-dials the upstream). Keep the default ≥30 s: the
/// Arbitrum feed emits multiple frames/s, and exits must stay under the unit's
/// StartLimitBurst=5/60s so a persistent upstream outage cycles instead of latching `failed`.
fn spawn_silence_watchdog() {
    let max_silence_s: u64 = std::env::var("SIDECAR_MAX_SILENCE_S")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(30);
    if max_silence_s == 0 {
        eprintln!("relay_sidecar: silence watchdog disabled (SIDECAR_MAX_SILENCE_S=0)");
        return;
    }
    if let Err(err) = std::thread::Builder::new()
        .name("relay-watchdog".into())
        .spawn(move || {
            engine::runtime::pin_to_core(0, "relay-watchdog");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if !PUMP_LIVE.load(Ordering::Relaxed) {
                    continue;
                }
                let last = LAST_FRAME_NS.load(Ordering::Relaxed);
                if silence_exceeded(last, now_ns(), max_silence_s) {
                    eprintln!(
                        "relay_sidecar: SIDECAR_STALLED — no upstream frame for >{max_silence_s}s while \
                         live; exiting for systemd to relaunch fresh"
                    );
                    drain_async_capture_before_watchdog_exit(Duration::from_secs(2));
                    std::process::exit(2);
                }
            }
        })
    {
        eprintln!("relay_sidecar: failed to spawn silence watchdog: {err}");
    }
}

fn now_ns() -> u64 {
    // CLOCK_MONOTONIC-equivalent via Instant base would need a shared epoch; for the frame stamp we
    // use the wall clock (the engine differences it only coarsely). On AWS, replace with the NIC
    // hardware RX timestamp (SO_TIMESTAMPING / PHC) for true wire-arrival.
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn main() {
    // The service's outer affinity allows cores 0+2. Keep the actual WS read/decode/UDS callback
    // exclusively on feed core 2; named writer/watchdog threads explicitly move to core 0.
    engine::runtime::pin_to_core(2, "relay-sidecar-feed");
    // Install the rustls CryptoProvider FIRST — before WsRelayClient::new() builds a ClientConfig.
    // aws-lc-rs (workspace) AND ring (pulled transitively by tungstenite) are both compiled, so
    // rustls cannot auto-select a provider and ClientConfig::builder() panics without this call.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let args = parse_args_from(std::env::args());
    let capture_mode = match CaptureMode::from_env() {
        Ok(mode) => mode,
        Err(err) => {
            eprintln!("relay_sidecar: {err}");
            std::process::exit(1);
        }
    };
    let graceful_signals = match engine_sysopt::block_termination_signals() {
        Ok(()) => true,
        Err(err) => {
            eprintln!("relay_sidecar: graceful signal setup failed: {err}");
            false
        }
    };

    // Fresh listener (remove a stale socket file first). `binseq:` is explicit/default-off;
    // unprefixed and `binunix:` endpoints retain the stream transport.
    let local = LocalEndpoint::parse(&args.sock);
    let _ = std::fs::remove_file(&local.path);
    let listener = match FeedListener::bind(&local) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("relay_sidecar: bind {} failed: {e}", local.path);
            std::process::exit(1);
        }
    };
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("relay_sidecar: set listener nonblocking failed: {err}");
        std::process::exit(1);
    }
    eprintln!(
        "relay_sidecar: listening on {} ({:?}), upstream {}",
        local.path,
        local.transport,
        args.file.as_deref().unwrap_or(&args.upstream)
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    let signal_thread = graceful_signals.then(|| {
        let shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("sidecar-signal".into())
            .spawn(move || {
                engine::runtime::pin_to_core(0, "sidecar-signal");
                match engine_sysopt::wait_termination_signal() {
                    Ok(signal) => {
                        eprintln!("relay_sidecar: signal {signal}; draining capture and stopping");
                        shutdown.store(true, Ordering::Release);
                    }
                    Err(err) => eprintln!("relay_sidecar: signal wait failed: {err}"),
                }
            })
            .expect("spawn sidecar signal waiter")
    });

    let mut capture = match CaptureSink::open(capture_mode, args.capture_jsonl.as_deref()) {
        Ok(capture) => capture,
        Err(err) => {
            eprintln!("relay_sidecar: capture setup failed: {err}");
            std::process::exit(1);
        }
    };

    let mut relay_degraded = false;
    // Silent-upstream tripwire — live feed mode only (a replayed corpus legitimately idles forever
    // after exhaustion, so the watchdog must not shoot the file-replay path).
    if args.file.is_none() {
        spawn_silence_watchdog();
    }
    // Accept one engine connection at a time; nonblocking accept keeps external shutdown bounded.
    while !shutdown.load(Ordering::Acquire) {
        let mut stream = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(e) => {
                eprintln!("relay_sidecar: accept failed: {e}");
                continue;
            }
        };
        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
        // (Unix-domain sockets have no Nagle/TCP_NODELAY — message delivery is immediate.)
        eprintln!("relay_sidecar: engine connected");
        match &args.file {
            Some(path) => pump_file(path, &mut stream, &shutdown, &mut capture),
            None => {
                relay_degraded = pump_feed(
                    &args.upstream,
                    args.upstream2.as_deref(),
                    &mut stream,
                    &shutdown,
                    &mut capture,
                );
            }
        }
        if relay_degraded {
            break;
        }
        eprintln!("relay_sidecar: engine disconnected; awaiting reconnect");
    }
    drop(capture); // async mode drains, flushes, and joins its writer here
    if relay_degraded {
        // The local feed connection has already dropped, so the engine observes EOF and revokes
        // authority. Exit only after capture Drop has flushed/marked evidence; systemd restarts a
        // fresh authoritative feed process.
        std::process::exit(2);
    }
    if let Some(handle) = signal_thread {
        let _ = handle.join();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidecarArgs {
    sock: String,
    upstream: String,
    /// Optional 2nd upstream relay (latency roadmap #2, dual-relay). When set, the sidecar consumes
    /// BOTH relays via the latency-safe `run_dual` (poll-based immediate wake, no fixed sleep) and
    /// dedups by feed sequence — first-to-deliver wins, the other covers a single-relay stall. The
    /// dedup + redundancy live HERE so the engine keeps its simple single-UDS binary read (the
    /// binary rx path has no engine-level dual; putting it in the sidecar is the correct topology).
    upstream2: Option<String>,
    file: Option<String>,
    capture_jsonl: Option<String>,
}

fn parse_args_from<I, S>(args: I) -> SidecarArgs
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut cfg = SidecarArgs {
        sock: "/tmp/mantis-feed.sock".to_string(),
        upstream: engine_feed::DEFAULT_LOCAL_RELAY_WS.to_string(),
        upstream2: None,
        file: None,
        capture_jsonl: None,
    };
    let mut iter = args.into_iter();
    let _ = iter.next();
    while let Some(arg) = iter.next() {
        match arg.as_ref() {
            "--sock" => {
                if let Some(v) = iter.next() {
                    cfg.sock = v.as_ref().to_string();
                }
            }
            "--upstream" => {
                if let Some(v) = iter.next() {
                    cfg.upstream = v.as_ref().to_string();
                }
            }
            "--upstream2" => {
                if let Some(v) = iter.next() {
                    cfg.upstream2 = Some(v.as_ref().to_string());
                }
            }
            // Replay mode: stream a saved JSONL feed corpus as the BINARY feed (same encoder as the
            // live WS path) so a historical replay exercises the S2 binary fast path end-to-end.
            "--file" => {
                if let Some(v) = iter.next() {
                    cfg.file = Some(v.as_ref().to_string());
                }
            }
            "--capture-jsonl" => {
                if let Some(v) = iter.next() {
                    cfg.capture_jsonl = Some(v.as_ref().to_string());
                }
            }
            other => eprintln!("relay_sidecar: ignoring unknown arg {other}"),
        }
    }
    cfg
}

/// Replay a JSONL feed corpus as the binary feed: each line is a relay envelope; decode it and
/// emit the same binary `FeedFrame`(s) the live WS path would. After the corpus is exhausted, keep
/// the socket open (idle) so the engine drains + measures rather than seeing EOF. `REPLAY_PACE_US`
/// optionally spaces frames to mimic sequencer cadence.
fn pump_file(
    path: &str,
    out: &mut FeedConnection,
    shutdown: &AtomicBool,
    capture: &mut CaptureSink,
) {
    use std::io::{BufRead, BufReader};
    let pace_us: u64 = std::env::var("REPLAY_PACE_US")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("relay_sidecar: open {path} failed: {e}");
            return;
        }
    };
    let mut b64 = Vec::with_capacity(16 * 1024);
    let mut frame = Vec::with_capacity(64 * 1024);
    let mut pushed = 0u64;
    for line in BufReader::new(f).lines() {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let bytes = line.trim().as_bytes().to_vec();
        if bytes.is_empty() {
            continue;
        }
        capture.capture_raw_frame(&bytes);
        let mut write_err = false;
        for env in iter_envelopes(&bytes) {
            b64.clear();
            b64.extend_from_slice(env.l2msg_b64);
            let decoded = match base64_simd::forgiving_decode_inplace(&mut b64) {
                Ok(d) => d.len(),
                Err(_) => {
                    // C3b: ship a typed V4 taint marker so the ENGINE watermark moves for a
                    // payload this process cannot decode into leaves (TAINT_CODE_UNDECODABLE_PAYLOAD).
                    engine_feed::encode_frame_v4(
                        &mut frame,
                        env.sequence_number,
                        now_ns(),
                        env.timestamp,
                        None,
                        [0u8; 4],
                        &[],
                        None,
                        0,
                        Some(engine_feed::FeedFrameMarker::Taint(
                            engine_feed::TAINT_CODE_UNDECODABLE_PAYLOAD,
                        )),
                    );
                    if out.send_frame(&frame).is_err() {
                        write_err = true;
                    }
                    continue;
                }
            };
            let seq = env.sequence_number;
            let recv_ns = now_ns();
            capture.capture_freshness(seq, env.timestamp, recv_ns);
            // V4 path (WI-03): canonical normalizer walk — leaves in official Nitro depth-first
            // input order, each normalized leaf ships exactly one V4 record: decoded leaves ship
            // the raw envelope (shadow REVM) + header.timestamp (Gate 8 freshness) + hot-path
            // head; undecoded state-bearing leaves ship a taint marker; documented ignorable
            // leaves ship a non-tainting ordinal marker.
            engine_feed::normalize_l2msg(seq, &b64[..decoded], |item| {
                if write_err {
                    return;
                }
                match item {
                    engine_feed::NormalizedItem::Decoded {
                        cursor,
                        info,
                        envelope,
                    } => {
                        let mut sel = [0u8; 4];
                        if info.data.len() >= 4 {
                            sel.copy_from_slice(&info.data[..4]);
                        }
                        let body: &[u8] = if info.data.len() > 4 {
                            &info.data[4..]
                        } else {
                            &[]
                        };
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            Some(info.to),
                            sel,
                            body,
                            Some(envelope),
                            cursor.inner_ordinal,
                            None,
                        );
                        if out.send_frame(&frame).is_err() {
                            write_err = true;
                        }
                    }
                    engine_feed::NormalizedItem::Tainted { cursor, reason } => {
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            None,
                            [0u8; 4],
                            &[],
                            None,
                            cursor.inner_ordinal,
                            Some(engine_feed::FeedFrameMarker::Taint(reason.code())),
                        );
                        if out.send_frame(&frame).is_err() {
                            write_err = true;
                        }
                    }
                    engine_feed::NormalizedItem::Malformed { cursor, reason } => {
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            None,
                            [0u8; 4],
                            &[],
                            None,
                            cursor.inner_ordinal,
                            Some(engine_feed::FeedFrameMarker::Taint(reason.code())),
                        );
                        if out.send_frame(&frame).is_err() {
                            write_err = true;
                        }
                    }
                    // Documented Ignorable (Nitro-unimplemented kinds 2/7, deprecated 6):
                    // ships one non-tainting V4 ordinal marker.
                    engine_feed::NormalizedItem::Unsupported { cursor, .. } => {
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            None,
                            [0u8; 4],
                            &[],
                            None,
                            cursor.inner_ordinal,
                            Some(engine_feed::FeedFrameMarker::IgnorableOrdinal),
                        );
                        if out.send_frame(&frame).is_err() {
                            write_err = true;
                        }
                    }
                }
            });
            if write_err {
                capture.finish_frame();
                eprintln!("relay_sidecar: engine closed after {pushed} frames");
                return;
            }
            pushed += 1;
            if pace_us > 0 {
                std::thread::sleep(std::time::Duration::from_micros(pace_us));
            }
        }
        capture.finish_frame();
    }
    eprintln!("relay_sidecar: corpus exhausted ({pushed} frames); idling");
    while !shutdown.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Subscribe to the upstream WS relay and write a binary frame per decoded signed-tx to `out`.
/// Returns when the engine disconnects (write error) or the relay loop ends.
fn pump_feed(
    upstream: &str,
    upstream2: Option<&str>,
    out: &mut FeedConnection,
    shutdown: &Arc<AtomicBool>,
    capture: &mut CaptureSink,
) -> bool {
    let degraded = Arc::new(AtomicBool::new(false));
    let feed_shutdown = match out.shutdown_handle() {
        Ok(handle) => Arc::new(handle),
        Err(error) => {
            eprintln!("relay_sidecar: cannot arm fail-closed engine-feed shutdown handle: {error}");
            capture.invalidate_evidence();
            shutdown.store(true, Ordering::Release);
            return true;
        }
    };
    let degradation_callback =
        sidecar_degradation_callback(degraded.clone(), shutdown.clone(), feed_shutdown);
    let p = match WsRelayClient::new(upstream).map(|client| {
        client
            .with_degradation_callback(degradation_callback.clone())
            .with_read_poll_timeout(Duration::from_millis(250))
    }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("relay_sidecar: invalid upstream {upstream}: {e}");
            return false;
        }
    };
    // Dual-relay (roadmap #2): optional 2nd upstream, consumed via the latency-safe `run_dual`
    // (poll-based immediate wake, no fixed sleep) + feed-seq dedup. The redundancy lives HERE in the
    // sidecar so the engine keeps its simple single-UDS binary read (the binary rx path has no
    // engine-level dual). An invalid 2nd upstream degrades to single-relay — never take the feed down.
    let secondary = match upstream2 {
        Some(u2) => match WsRelayClient::new(u2).map(|client| {
            client
                .with_degradation_callback(degradation_callback.clone())
                .with_read_poll_timeout(Duration::from_millis(250))
        }) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("relay_sidecar: invalid upstream2 {u2}: {e}; running single-relay");
                None
            }
        },
        None => None,
    };
    let mut b64 = Vec::with_capacity(16 * 1024);
    let mut frame = Vec::with_capacity(64 * 1024);
    // Arm the silence watchdog for the duration of this pump: a fresh baseline (so a pre-pump idle
    // gap can't instantly trip it), then every upstream frame — including ones that decode to zero
    // txs — refreshes the timestamp. Upstream LIVENESS is what's being watched, not tx flow.
    LAST_FRAME_NS.store(now_ns(), Ordering::Relaxed);
    PUMP_LIVE.store(true, Ordering::Relaxed);
    let dead = Arc::new(AtomicBool::new(false));
    let mut stamp_stats = engine::seq_latency_monitor::timestamping::StampStats::default();
    let on_frame = |cb: engine_feed::RelayCallback<'_>| {
        if dead.load(Ordering::Relaxed) || !sidecar_feed_delivery_allowed(&degraded) {
            return;
        }
        stamp_stats.record_counts(
            cb.timestamps.reads,
            cb.timestamps.software,
            cb.timestamps.hardware,
            cb.timestamps.missing,
        );
        LAST_FRAME_NS.store(now_ns(), Ordering::Relaxed);
        capture.capture_raw_frame(cb.frame);
        // Host wall-clock receive time, stamped at read_message (NOT a later now_ns()), so the
        // sequencer→engine event age it anchors is true wire-arrival latency. (Codex #3.)
        let recv_ns = cb.recv_ns;
        for env in iter_envelopes(cb.frame) {
            b64.clear();
            b64.extend_from_slice(env.l2msg_b64);
            let decoded = match base64_simd::forgiving_decode_inplace(&mut b64) {
                Ok(d) => d.len(),
                Err(_) => {
                    // C3b: ship a typed V4 taint marker so the ENGINE watermark moves for a
                    // payload this process cannot decode into leaves (TAINT_CODE_UNDECODABLE_PAYLOAD).
                    engine_feed::encode_frame_v4(
                        &mut frame,
                        env.sequence_number,
                        recv_ns,
                        env.timestamp,
                        None,
                        [0u8; 4],
                        &[],
                        None,
                        0,
                        Some(engine_feed::FeedFrameMarker::Taint(
                            engine_feed::TAINT_CODE_UNDECODABLE_PAYLOAD,
                        )),
                    );
                    if out.send_frame(&frame).is_err() {
                        dead.store(true, Ordering::Relaxed);
                        break;
                    }
                    continue;
                }
            };
            let seq = env.sequence_number;
            capture.capture_freshness(seq, env.timestamp, recv_ns);
            // WI-03: canonical normalizer walk (official Nitro depth-first input order). One V4
            // record per normalized leaf: decoded leaves carry raw envelope (shadow REVM) +
            // header.timestamp (Gate 8) + hot-path head; state-bearing leaves emit taint markers;
            // documented ignorable leaves emit non-tainting ordinal markers.
            let mut write_err = false;
            engine_feed::normalize_l2msg(seq, &b64[..decoded], |item| {
                if write_err {
                    return;
                }
                let sent = match item {
                    engine_feed::NormalizedItem::Decoded {
                        cursor,
                        info,
                        envelope,
                    } => {
                        let mut sel = [0u8; 4];
                        if info.data.len() >= 4 {
                            sel.copy_from_slice(&info.data[..4]);
                        }
                        let body: &[u8] = if info.data.len() > 4 {
                            &info.data[4..]
                        } else {
                            &[]
                        };
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            Some(info.to),
                            sel,
                            body,
                            Some(envelope),
                            cursor.inner_ordinal,
                            None,
                        );
                        true
                    }
                    engine_feed::NormalizedItem::Tainted { cursor, reason } => {
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            None,
                            [0u8; 4],
                            &[],
                            None,
                            cursor.inner_ordinal,
                            Some(engine_feed::FeedFrameMarker::Taint(reason.code())),
                        );
                        true
                    }
                    engine_feed::NormalizedItem::Malformed { cursor, reason } => {
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            None,
                            [0u8; 4],
                            &[],
                            None,
                            cursor.inner_ordinal,
                            Some(engine_feed::FeedFrameMarker::Taint(reason.code())),
                        );
                        true
                    }
                    // Documented Ignorable (Nitro-unimplemented 2/7, deprecated 6): ships one
                    // non-tainting V4 ordinal marker through the common send below.
                    engine_feed::NormalizedItem::Unsupported { cursor, .. } => {
                        engine_feed::encode_frame_v4(
                            &mut frame,
                            seq,
                            recv_ns,
                            env.timestamp,
                            None,
                            [0u8; 4],
                            &[],
                            None,
                            cursor.inner_ordinal,
                            Some(engine_feed::FeedFrameMarker::IgnorableOrdinal),
                        );
                        true
                    }
                };
                if sent && out.send_frame(&frame).is_err() {
                    write_err = true;
                }
            });
            if write_err {
                dead.store(true, Ordering::Relaxed);
                break;
            }
        }
        capture.finish_frame();
    };
    // Single vs dual dispatch — same decode closure, one of two relay drivers.
    match secondary.as_ref() {
        Some(s) => {
            let _ = WsRelayClient::run_dual(&p, s, shutdown, on_frame);
        }
        None => {
            let _ = p.run(shutdown, on_frame);
        }
    }
    if stamp_stats.total > 0 {
        eprintln!(
            "relay_sidecar: feed_timestamp_coverage reads={} software={} hardware={} missing={} hardware_cmsg_coverage_trustworthy={}",
            stamp_stats.total,
            stamp_stats.software,
            stamp_stats.hardware,
            stamp_stats.missing,
            stamp_stats.hardware_trustworthy(),
        );
    }
    // Disarm the watchdog while idling for the next engine connection (no upstream read happens
    // between pumps, so silence there is expected, not a stall).
    PUMP_LIVE.store(false, Ordering::Relaxed);
    let relay_degraded = degraded.load(Ordering::Acquire);
    if relay_degraded {
        capture.invalidate_evidence();
    }
    relay_degraded
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_capture_path(label: &str) -> String {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "mantis-sidecar-{label}-{}-{id}.jsonl",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned()
    }

    fn cleanup_capture(path: &str) {
        for suffix in ["", ".freshness.jsonl", ".invalid"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }

    fn record_two_frames(capture: &mut CaptureSink) {
        capture.capture_raw_frame(br#"{"messages":[{"sequenceNumber":11}]}"#);
        capture.capture_freshness(11, 1_700_000_000, 1_700_000_000_123_000_000);
        capture.finish_frame();
        capture.capture_raw_frame(br#"{"messages":[{"sequenceNumber":12}],"x":"AA=="}"#);
        capture.capture_freshness(12, 1_700_000_001, 1_700_000_001_456_000_000);
        capture.capture_freshness(13, 1_700_000_001, 1_700_000_001_789_000_000);
        capture.finish_frame();
    }

    #[test]
    fn silence_watchdog_trips_only_past_the_threshold_and_never_before_first_frame() {
        // `s` = one second in ns.
        let s = 1_000_000_000u64;
        // Unarmed (no frame stamped yet) — never trips regardless of elapsed time.
        assert!(!silence_exceeded(0, 100 * s, 30));
        // Fresh frame — quiet for 29s with a 30s budget: no trip.
        assert!(!silence_exceeded(70 * s, 99 * s, 30));
        // Exactly at the budget: no trip (strictly-greater comparison).
        assert!(!silence_exceeded(70 * s, 100 * s, 30));
        // Past the budget: trip.
        assert!(silence_exceeded(70 * s, 101 * s, 30));
        // Clock skew (now < last, e.g. NTP step): saturates to 0 elapsed — no spurious trip.
        assert!(!silence_exceeded(100 * s, 70 * s, 30));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn relay_gap_latches_before_delivery_and_tears_down_the_mock_engine_feed() {
        let degraded = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let mut mock_engine_feed = FeedConnection::SeqPacket(tx);
        let feed_shutdown = Arc::new(mock_engine_feed.shutdown_handle().unwrap());
        let callback =
            sidecar_degradation_callback(degraded.clone(), shutdown.clone(), feed_shutdown);
        callback(engine_feed::FeedDegradationReason::DualRelayForwardGap);

        assert!(degraded.load(Ordering::Acquire));
        assert!(shutdown.load(Ordering::Acquire));
        assert!(!sidecar_feed_delivery_allowed(&degraded));

        if sidecar_feed_delivery_allowed(&degraded) {
            mock_engine_feed
                .send_frame(b"must-not-reach-engine")
                .unwrap();
        }

        let mut received = [0u8; 64];
        let (bytes, truncated) =
            engine_sysopt::recvmsg_packet(rx.as_raw_fd(), &mut received).unwrap();
        assert_eq!(
            bytes,
            FEED_DEGRADATION_SENTINEL.len(),
            "mock engine must receive exactly one fail-closed degradation record"
        );
        assert!(!truncated);
        assert!(
            engine_feed::decode_frame(&received[..bytes]).is_none(),
            "degradation record must be rejected by the engine's binary feed decoder"
        );

        // The production delivery callback consults the false gate asserted above before every
        // data send. Dropping the pump's owned connection (what happens immediately after relay
        // shutdown) must then yield EOF after the single control record, even when this sandbox
        // denies shutdown(2).
        drop(callback);
        drop(mock_engine_feed);
        let (bytes, truncated) =
            engine_sysopt::recvmsg_packet(rx.as_raw_fd(), &mut received).unwrap();
        assert_eq!(
            bytes, 0,
            "mock engine must observe EOF when the stopped pump drops"
        );
        assert!(!truncated);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn payload_conflict_callback_sends_typed_control_record_before_eof() {
        let degraded = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let mock_engine_feed = FeedConnection::SeqPacket(tx);
        let feed_shutdown = Arc::new(mock_engine_feed.shutdown_handle().unwrap());
        let callback =
            sidecar_degradation_callback(degraded.clone(), shutdown.clone(), feed_shutdown);
        callback(engine_feed::FeedDegradationReason::FeedPayloadConflict);

        let mut received = [0u8; 64];
        let (bytes, truncated) =
            engine_sysopt::recvmsg_packet(rx.as_raw_fd(), &mut received).unwrap();
        assert_eq!(bytes, engine_feed::FEED_FRAME_HEADER_LEN);
        assert!(!truncated);
        assert_eq!(
            &received[..bytes],
            engine_feed::feed_frame::FEED_PAYLOAD_CONFLICT_SENTINEL.as_slice()
        );
        assert!(engine_feed::feed_frame::is_feed_payload_conflict_sentinel(
            &received[..bytes]
        ));
        assert!(engine_feed::decode_frame(&received[..bytes]).is_none());

        drop(callback);
        drop(mock_engine_feed);
        let (bytes, truncated) =
            engine_sysopt::recvmsg_packet(rx.as_raw_fd(), &mut received).unwrap();
        assert_eq!(bytes, 0);
        assert!(!truncated);
    }

    #[test]
    fn parses_capture_jsonl_option() {
        let cfg = parse_args_from([
            "relay_sidecar",
            "--sock",
            "/tmp/test.sock",
            "--upstream",
            "ws://127.0.0.1:9642/feed",
            "--capture-jsonl",
            "/tmp/feed.jsonl",
        ]);

        assert_eq!(cfg.sock, "/tmp/test.sock");
        assert_eq!(cfg.upstream, "ws://127.0.0.1:9642/feed");
        assert_eq!(cfg.capture_jsonl.as_deref(), Some("/tmp/feed.jsonl"));
    }

    #[test]
    fn local_endpoint_defaults_stream_and_binseq_is_explicit() {
        assert_eq!(
            LocalEndpoint::parse("/tmp/feed.sock"),
            LocalEndpoint {
                transport: LocalTransport::Stream,
                path: "/tmp/feed.sock".into(),
            }
        );
        assert_eq!(
            LocalEndpoint::parse("binunix:/tmp/feed.sock"),
            LocalEndpoint {
                transport: LocalTransport::Stream,
                path: "/tmp/feed.sock".into(),
            }
        );
        assert_eq!(
            LocalEndpoint::parse("binseq:/run/mantis/mantis-feed.sock"),
            LocalEndpoint {
                transport: LocalTransport::SeqPacket,
                path: "/run/mantis/mantis-feed.sock".into(),
            }
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn seqpacket_sender_emits_exactly_one_complete_frame() {
        let (tx, rx) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let mut out = FeedConnection::SeqPacket(tx);
        let mut frame = Vec::new();
        engine_feed::encode_frame_v4(
            &mut frame,
            11,
            12,
            13,
            Some([0x44; 20]),
            [1, 2, 3, 4],
            &[5, 6, 7],
            Some(&[0x02, 0xc0]),
            0,
            None,
        );
        out.send_frame(&frame).unwrap();

        let mut received = vec![0u8; SEQPACKET_FRAME_BYTES];
        let (n, truncated) = engine_sysopt::recvmsg_packet(rx.as_raw_fd(), &mut received).unwrap();
        assert!(!truncated);
        assert_eq!(&received[..n], frame.as_slice());
        assert!(engine_feed::decode_frame(&received[..n]).is_some());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn seqpacket_sender_turns_oversize_into_engine_visible_truncation() {
        let (tx, rx) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let mut out = FeedConnection::SeqPacket(tx);
        let frame = vec![0x5a; SEQPACKET_FRAME_BYTES + 4096];

        out.send_frame(&frame).unwrap();

        let mut received = vec![0u8; SEQPACKET_FRAME_BYTES];
        let (n, truncated) = engine_sysopt::recvmsg_packet(rx.as_raw_fd(), &mut received).unwrap();
        assert_eq!(n, SEQPACKET_FRAME_BYTES);
        assert!(
            truncated,
            "oversize must reach the engine as MSG_TRUNC, never disappear on the sidecar"
        );
    }

    #[test]
    fn capture_mode_defaults_to_sync_and_rejects_unknown_values() {
        assert_eq!(CaptureMode::parse(None).unwrap(), CaptureMode::Sync);
        assert_eq!(CaptureMode::parse(Some("")).unwrap(), CaptureMode::Sync);
        assert_eq!(CaptureMode::parse(Some("sync")).unwrap(), CaptureMode::Sync);
        assert_eq!(
            CaptureMode::parse(Some("async")).unwrap(),
            CaptureMode::Async
        );
        assert_eq!(CaptureMode::parse(Some("off")).unwrap(), CaptureMode::Off);
        let err = CaptureMode::parse(Some("best-effort")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("sync|async|off"));
    }

    #[test]
    fn async_capture_matches_sync_artifacts_byte_for_byte_when_lossless() {
        let sync_path = temp_capture_path("sync-parity");
        let async_path = temp_capture_path("async-parity");
        cleanup_capture(&sync_path);
        cleanup_capture(&async_path);

        {
            let mut capture = CaptureSink::open(CaptureMode::Sync, Some(&sync_path)).unwrap();
            record_two_frames(&mut capture);
        }
        let async_stats = {
            let mut capture = CaptureSink::open(CaptureMode::Async, Some(&async_path)).unwrap();
            let stats = capture.stats().unwrap().clone();
            record_two_frames(&mut capture);
            drop(capture); // drains, flushes and joins before artifact comparison
            stats
        };

        assert_eq!(
            std::fs::read(&async_path).unwrap(),
            std::fs::read(&sync_path).unwrap(),
            "raw replay corpus bytes differ"
        );
        assert_eq!(
            std::fs::read(format!("{async_path}.freshness.jsonl")).unwrap(),
            std::fs::read(format!("{sync_path}.freshness.jsonl")).unwrap(),
            "freshness rows differ"
        );
        assert_eq!(async_stats.enqueued.load(Ordering::Relaxed), 2);
        assert_eq!(async_stats.dropped.load(Ordering::Relaxed), 0);
        assert_eq!(async_stats.queue_depth.load(Ordering::Relaxed), 0);
        assert!(async_stats.evidence_valid.load(Ordering::Acquire));
        assert!(!std::path::Path::new(&format!("{async_path}.invalid")).exists());

        cleanup_capture(&sync_path);
        cleanup_capture(&async_path);
    }

    #[test]
    fn async_capture_oversize_is_nonblocking_and_durably_invalidates_evidence() {
        let path = temp_capture_path("oversize");
        cleanup_capture(&path);
        let stats = {
            let mut capture = CaptureSink::open(CaptureMode::Async, Some(&path)).unwrap();
            let stats = capture.stats().unwrap().clone();
            let raw = vec![0x5a; CAPTURE_SLOT_BYTES + 1];
            capture.capture_raw_frame(&raw);
            capture.capture_freshness(1, 2, 3);
            capture.finish_frame();
            drop(capture);
            stats
        };

        assert_eq!(stats.enqueued.load(Ordering::Relaxed), 0);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 1);
        assert!(!stats.evidence_valid.load(Ordering::Acquire));
        assert!(std::fs::read(&path).unwrap().is_empty());
        let marker = std::fs::read_to_string(format!("{path}.invalid")).unwrap();
        assert!(marker.contains("capture_evidence_invalid=1"));
        assert!(marker.contains("capture_dropped_total=1"));
        cleanup_capture(&path);
    }

    #[test]
    fn full_free_slot_ring_drops_capture_without_overwriting_inflight_slot() {
        let (mut free_producer, free) = RingBuffer::<CaptureSlot>::new(1);
        free_producer.push(CaptureSlot::new(7)).unwrap();
        let (ready, ready_consumer) = RingBuffer::<CaptureSlot>::new(1);
        let stats = Arc::new(CaptureStats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let writer = std::thread::Builder::new()
            .name("capture-test-stalled".into())
            .spawn(move || {
                let _owned_endpoints = (free_producer, ready_consumer);
                while !worker_shutdown.load(Ordering::Acquire) {
                    std::thread::park_timeout(Duration::from_millis(1));
                }
            })
            .unwrap();
        let writer_thread = writer.thread().clone();
        let capture = AsyncCapture {
            free,
            ready,
            pending: None,
            stats: stats.clone(),
            shutdown,
            writer_thread,
            writer: Some(writer),
        };
        let mut sink = CaptureSink::Async(capture);
        sink.capture_raw_frame(b"first");
        sink.finish_frame();
        assert_eq!(stats.enqueued.load(Ordering::Relaxed), 1);
        assert_eq!(stats.queue_depth.load(Ordering::Relaxed), 1);
        sink.capture_raw_frame(b"second"); // no free slot: fail capture closed, forwarding caller continues
        sink.finish_frame();
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 1);
        assert!(!stats.evidence_valid.load(Ordering::Acquire));
        drop(sink);
    }

    #[test]
    fn capture_off_creates_no_artifacts_even_when_path_is_supplied() {
        let path = temp_capture_path("off");
        cleanup_capture(&path);
        {
            let mut capture = CaptureSink::open(CaptureMode::Off, Some(&path)).unwrap();
            capture.capture_raw_frame(b"must-not-be-written");
            capture.capture_freshness(1, 2, 3);
            capture.finish_frame();
            assert!(capture.stats().is_none());
        }
        assert!(!std::path::Path::new(&path).exists());
        assert!(!std::path::Path::new(&format!("{path}.freshness.jsonl")).exists());
        assert!(!std::path::Path::new(&format!("{path}.invalid")).exists());
    }
}
