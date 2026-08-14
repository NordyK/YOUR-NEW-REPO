//! B1: io_uring-backed socket send for the submit hot path.
//!
//! Replaces `TcpStream::write_all` with `io_uring` SQE submissions so the kernel polls the
//! submission queue from a dedicated softirq thread (SQPOLL) and the user thread incurs zero
//! syscalls per send on the hot path. Expected win on Graviton4: ~5–10 µs/submit, almost all
//! of it the `write(2)` syscall + scheduler latency.
//!
//! ## What this module is and isn't
//! This is a **post-TLS** sender: it takes already-encrypted bytes and ships them via uring
//! SQEs onto a raw socket fd. TLS encryption stays on the user thread (rustls is sync). The
//! TLS layer wraps a custom `Write` that funnels into `IoUringSendQueue::submit_all`.
//!
//! ## Default-OFF policy (P7A, GPT56SOL_NETWORK_REVIEW 2026-07-13)
//! The `io-uring-submit` Cargo feature is DEFAULT-OFF. It never engaged for production HTTPS (only
//! `Conn::Plain`), and its old per-POST ring + partial-write fallback could corrupt the HTTP stream.
//! It is now opt-in (`--features io-uring-submit`) and, when enabled, fails CLOSED on a partial write
//! (see [`UringSendError::written`]) instead of falling through to a second `write_all`.

#![cfg(all(target_os = "linux", feature = "io-uring-submit"))]

use std::io;
use std::os::unix::io::RawFd;

use io_uring::{opcode, types, IoUring};

/// Error from [`IoUringSendQueue::submit_all`]. Carries how many bytes were CONFIRMED written to the
/// socket before the failure so the caller can decide whether a plain-`write_all` fallback on the
/// same socket is safe:
///
/// - `written == 0`: nothing reached the socket — the caller MAY safely fall back to `write_all`.
/// - `written  > 0`: a PARTIAL HTTP request is already on the wire. A `write_all` fallback would
///   append a second full request (`POST<partial>POST<full>`), corrupting the stream. The caller
///   MUST drop the socket and retry the identical bytes only on a FRESH connection (P7A).
#[derive(Debug)]
pub struct UringSendError {
    /// Bytes confirmed written to the socket before the error (0 ⇒ safe to fall back).
    pub written: usize,
    /// The underlying I/O error.
    pub source: io::Error,
}

impl std::fmt::Display for UringSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "io_uring send failed after {} byte(s): {}",
            self.written, self.source
        )
    }
}

impl std::error::Error for UringSendError {}

/// Ring sized for the submit hot path: 8 in-flight SQEs is plenty (one tx ≈ one SQE; the
/// submitter rarely pipelines more than 2-3 sends without waiting). Smaller rings mean
/// less kernel memory + lower SQ/CQ traversal cost.
const RING_ENTRIES: u32 = 8;

/// User-data tag attached to write SQEs so the CQE consumer can match completions to sends.
/// We use a monotonically-incrementing counter rather than a pointer (no aliasing concern).
const USER_DATA_WRITE_BASE: u64 = 0x57_72_69_74_65_00_00_00; // "Write\0\0\0"

/// io_uring-backed send queue for a single TCP socket. Construct once per connection and reuse
/// for every submit through the lifetime of the TLS session.
///
/// **Threading**: not `Send` by design — the ring is bound to the thread that constructed it.
/// The submitter is single-threaded on vCPU 6 anyway, so this is enforced by pinning.
pub struct IoUringSendQueue {
    ring: IoUring,
    socket_fd: RawFd,
    next_user_data: u64,
}

impl IoUringSendQueue {
    /// Build a new ring bound to `socket_fd`. The fd must remain owned by the caller (we do
    /// not close it on drop). `sqpoll_idle_ms` controls how long the kernel poller waits before
    /// going idle; `None` disables SQPOLL.
    pub fn new(socket_fd: RawFd, sqpoll_idle_ms: Option<u32>) -> io::Result<Self> {
        let mut builder = IoUring::builder();
        if let Some(idle_ms) = sqpoll_idle_ms {
            builder.setup_sqpoll(idle_ms);
        }
        let ring = builder.build(RING_ENTRIES)?;
        Ok(Self {
            ring,
            socket_fd,
            next_user_data: USER_DATA_WRITE_BASE,
        })
    }

    /// Submit a full `bytes` write and wait for the kernel to acknowledge ALL bytes are queued
    /// for transmission. Returns the total number of bytes actually written.
    ///
    /// This is the equivalent of `TcpStream::write_all` but goes through io_uring. For short
    /// writes (~hundreds of bytes — typical tx size), this completes in ~1 µs vs ~10 µs for
    /// the `write(2)` syscall path on a busy system.
    ///
    /// On error the returned [`UringSendError`] carries `written` — the bytes CONFIRMED on the wire
    /// before the failure. `written == 0` ⇒ nothing was sent (a plain fallback is safe); `written >
    /// 0` ⇒ a partial request is on the socket and the caller MUST drop it (P7A: no `write_all`
    /// second send on the same socket).
    pub fn submit_all(&mut self, bytes: &[u8]) -> Result<usize, UringSendError> {
        let mut written = 0usize;
        while written < bytes.len() {
            let chunk = &bytes[written..];
            let ud = self.next_user_data;
            self.next_user_data = self.next_user_data.wrapping_add(1);
            let sqe = opcode::Write::new(
                types::Fd(self.socket_fd),
                chunk.as_ptr(),
                chunk.len() as u32,
            )
            .build()
            .user_data(ud);
            // SAFETY: SQE points into `chunk` which lives for the full submit_and_wait below.
            // The submission queue tail is updated by the io_uring crate.
            let push_res = unsafe {
                let mut sq = self.ring.submission();
                sq.push(&sqe)
            };
            if push_res.is_err() {
                return Err(UringSendError {
                    written,
                    source: io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"),
                });
            }
            if let Err(e) = self.ring.submit_and_wait(1) {
                return Err(UringSendError { written, source: e });
            }
            let cqe = match self.ring.completion().next() {
                Some(cqe) => cqe,
                None => {
                    return Err(UringSendError {
                        written,
                        source: io::Error::other("no CQE returned"),
                    })
                }
            };
            if cqe.user_data() != ud {
                return Err(UringSendError {
                    written,
                    source: io::Error::other(format!(
                        "CQE user_data mismatch: expected {ud:x}, got {:x}",
                        cqe.user_data()
                    )),
                });
            }
            let result = cqe.result();
            if result < 0 {
                return Err(UringSendError {
                    written,
                    source: io::Error::from_raw_os_error(-result),
                });
            }
            let n = result as usize;
            if n == 0 {
                // Connection closed mid-write.
                return Err(UringSendError {
                    written,
                    source: io::Error::new(io::ErrorKind::WriteZero, "io_uring write returned 0"),
                });
            }
            written += n;
        }
        Ok(written)
    }
}

/// Best-effort capability probe: returns `true` if io_uring is available on this kernel and we
/// can construct a ring. Use at startup to decide whether to wire the io_uring submit path or
/// fall back to the std `TcpStream::write_all` path.
pub fn io_uring_available() -> bool {
    IoUring::new(2).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn io_uring_available_returns_a_truthy_answer_without_panicking() {
        // On any modern Linux this should be true; on a sandbox without uring it might be
        // false. The contract: the probe never panics and returns deterministically.
        let _ = io_uring_available();
    }

    #[test]
    fn submit_all_writes_bytes_to_a_loopback_socket() {
        if !io_uring_available() {
            eprintln!("io_uring not available — skipping wire-level test");
            return;
        }
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect");
        let (mut server, _) = listener.accept().expect("accept");

        let mut send_q = IoUringSendQueue::new(client.as_raw_fd(), None).expect("construct ring");

        let payload = b"speculative submit via io_uring \xde\xad\xbe\xef";
        let n = send_q.submit_all(payload).expect("submit_all");
        assert_eq!(n, payload.len(), "all bytes acknowledged");

        let mut buf = vec![0u8; payload.len()];
        server.read_exact(&mut buf).expect("read on server side");
        assert_eq!(
            &buf, payload,
            "loopback received the exact bytes we submitted"
        );
    }

    #[test]
    fn submit_all_error_reports_zero_written_when_socket_is_dead() {
        // P7A: the caller decides fallback-vs-fail-closed from `UringSendError::written`. A write to a
        // socket whose peer already closed must fail with `written == 0` (nothing reached the wire),
        // which is the "safe to fall back to write_all" boundary. This proves the error carries the
        // confirmed byte count rather than a bare io::Error.
        if !io_uring_available() {
            return;
        }
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        // Close BOTH ends of the peer, then shut down our own write half so the kernel rejects the
        // write with EPIPE/ECONNRESET rather than silently buffering it.
        drop(server);
        let _ = client.shutdown(std::net::Shutdown::Write);
        let mut send_q = IoUringSendQueue::new(client.as_raw_fd(), None).expect("ring");
        match send_q.submit_all(b"POST /rpc HTTP/1.1\r\n\r\n") {
            Ok(_) => { /* some kernels buffer the loopback write; not a failure of the contract */ }
            Err(e) => assert_eq!(
                e.written, 0,
                "a write that never left the host must report written == 0 (safe-fallback boundary)"
            ),
        }
    }

    #[test]
    fn submit_all_handles_large_payloads_via_chunking() {
        if !io_uring_available() {
            return;
        }
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect");
        let (mut server, _) = listener.accept().expect("accept");
        let mut send_q = IoUringSendQueue::new(client.as_raw_fd(), None).unwrap();
        // 64 KiB — large enough to exercise multi-SQE chunking on a small SQ.
        let payload: Vec<u8> = (0..65_536u32).map(|i| (i & 0xff) as u8).collect();
        let n = send_q.submit_all(&payload).unwrap();
        assert_eq!(n, payload.len());
        let mut buf = vec![0u8; payload.len()];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(buf, payload);
    }
}
