//! Hot-spare submit connection: two warmed sequencer connections with instant failover
//! (TAIL_LATENCY_PLAN §2 D1).
//!
//! A single warm connection still pays a 50–150 ms TCP+TLS handshake the moment it drops mid-submit
//! — a tail spike on the FCFS-race-deciding leg. With two connections kept warm, a failed post fails
//! over to the already-connected spare and submits immediately; the dropped connection is re-warmed
//! off the hot path (idle heartbeat). The failover policy is abstracted over [`SubmitConn`] so it is
//! unit-tested against a fault-injecting mock and shares one code path with the real TLS connection.

use crate::tls::{DeliveryState, SubmitError, SubmitFailure};

/// One opt-in submit-wire measurement boundary. These are application-visible boundaries: in
/// particular, [`Self::TlsWriteLocalKernelReturn`] means rustls/plaintext `write_all` plus `flush`
/// returned after handing bytes to the local kernel. It is NOT proof that a NIC transmitted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitBoundary {
    /// A hot-spare attempt is about to frame the already-complete JSON-RPC body.
    AttemptStart,
    /// The byte-exact HTTP/1.1 request frame is complete, immediately before the socket write.
    HttpFrameComplete,
    /// TLS/plain write plus flush returned at the local-kernel boundary (not NIC transmission).
    TlsWriteLocalKernelReturn,
    /// The first application-visible response bytes returned from rustls/plain `Read`.
    ResponseFirstByte,
    /// The complete framed response was read and its JSON body copied for the caller.
    ResponseDone,
}

/// A submit measurement boundary stamped with same-host `CLOCK_MONOTONIC_RAW` nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitBoundaryStamp {
    /// Boundary that was crossed.
    pub boundary: SubmitBoundary,
    /// Same-host monotonic timestamp. It has no UTC epoch and is meaningless across hosts.
    pub mono_raw_ns: u64,
}

impl SubmitBoundaryStamp {
    #[inline]
    pub(crate) fn now(boundary: SubmitBoundary) -> Self {
        Self {
            boundary,
            mono_raw_ns: engine_sysopt::clock_monotonic_raw_ns(),
        }
    }
}

/// A submit connection the hot-spare can drive: warm it, post without inline reconnect, observe
/// connectedness. Implemented by `SequencerTlsConnection`.
pub trait SubmitConn {
    /// True if a socket is established (next post needs no handshake).
    fn is_connected(&self) -> bool;
    /// Establish the socket if not already connected (off-hot-path warm-up / re-warm).
    fn ensure_connected(&mut self) -> Result<(), SubmitError>;
    /// POST `body`, returning the response. Must NOT block on an inline reconnect — on failure it
    /// returns `Err` (and should drop its dead socket) so the hot-spare can fail over instantly. The
    /// [`SubmitFailure`] carries a [`DeliveryState`] so the caller can decide, once ALL attempts
    /// fail, whether the nonce is safe to reclaim (P7B).
    fn try_post(&mut self, body: &[u8]) -> Result<Vec<u8>, SubmitFailure>;

    /// Instrumented form of [`Self::try_post`]. Implementations that expose internal framing/I/O
    /// boundaries override this; the default preserves compatibility and submits the identical body.
    /// It is called only for a sampled `NETWORK_FINE=1` submit.
    fn try_post_observed(
        &mut self,
        body: &[u8],
        _observe: &mut dyn FnMut(SubmitBoundaryStamp),
    ) -> Result<Vec<u8>, SubmitFailure> {
        self.try_post(body)
    }

    /// Authority-gated form of [`Self::try_post`]. The default checks immediately before invoking
    /// the connection write. Production connections override this to check after HTTP framing at
    /// the closest safe boundary before the socket write.
    fn try_post_authorized(
        &mut self,
        body: &[u8],
        authorize: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, SubmitFailure> {
        if !authorize() {
            return Err(SubmitFailure::not_written(SubmitError::AuthorityRevoked));
        }
        self.try_post(body)
    }

    /// Authority-gated form of [`Self::try_post_observed`]. It preserves the exact body and
    /// measurement callback while refusing the attempt before any write if authority was revoked.
    fn try_post_observed_authorized(
        &mut self,
        body: &[u8],
        observe: &mut dyn FnMut(SubmitBoundaryStamp),
        authorize: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, SubmitFailure> {
        if !authorize() {
            return Err(SubmitFailure::not_written(SubmitError::AuthorityRevoked));
        }
        self.try_post_observed(body, observe)
    }
}

/// Two submit connections with instant failover. `primary` is preferred; on its failure the next
/// post goes to the spare with no handshake. Both are kept warm by [`Self::rewarm_idle`].
pub struct HotSpare<C: SubmitConn> {
    conns: [C; 2],
    /// Index of the currently-preferred connection (0 or 1).
    active: usize,
    /// Total failovers performed (telemetry).
    failovers: u64,
}

impl<C: SubmitConn> HotSpare<C> {
    /// Build from two connections (e.g. the same endpoint, two sockets). Neither is connected yet;
    /// call [`Self::rewarm_idle`] (or rely on the first submit) to establish them.
    pub fn new(primary: C, spare: C) -> Self {
        Self {
            conns: [primary, spare],
            active: 0,
            failovers: 0,
        }
    }

    /// Warm any disconnected connection. Call on the idle/heartbeat path so a spare is always ready
    /// and a dropped connection is re-established off the submit critical path. Returns the number
    /// currently connected (0–2).
    pub fn rewarm_idle(&mut self) -> usize {
        for c in &mut self.conns {
            if !c.is_connected() {
                let _ = c.ensure_connected();
            }
        }
        self.conns.iter().filter(|c| c.is_connected()).count()
    }

    /// Submit `body`, preferring the active connection and failing over to the spare on error
    /// **without** an inline handshake. Returns the response, or the last error if both fail. A
    /// successful spare becomes the new active connection (sticky failover). As a last resort, if
    /// neither warm connection works, it tries one reconnect+post on the active so a cold start
    /// still submits.
    ///
    /// **Idempotency (E4):** every retry re-posts the *same* signed `body`, and this cannot
    /// double-submit a trade. `eth_sendRawTransaction` is idempotent by transaction hash — a
    /// re-post of an already-accepted tx returns that same hash (or an "already known" error),
    /// never a second on-chain inclusion. Retries fire only after a `try_post` returned a
    /// *transport* error (so the attempt most likely never reached the sequencer); in the rare
    /// case the first post did land but its response was lost, the fixed nonce makes the resend a
    /// no-op on-chain. The nonce is therefore NOT consumed or reclaimed per-attempt here — that
    /// decision is made once upstream on the final classified ack (see
    /// [`crate::SubmitOutcome::nonce_should_reclaim`]).
    pub fn submit(&mut self, body: &[u8]) -> Result<Vec<u8>, SubmitFailure> {
        let mut last_err: Option<SubmitError> = None;
        // P7B: taints the aggregate delivery. Same-byte retries are always safe (idempotent by tx
        // hash), so one PossiblyWritten attempt does not risk a double-spend — but if EVERY attempt
        // fails, an earlier PossiblyWritten means the tx may already be in the mempool under this
        // nonce, so the caller must NOT reclaim it for different bytes. Monotonic: once possibly
        // written, always possibly written.
        let mut possibly_written = false;
        // First, try the two warm connections (active, then spare) with no handshake.
        for attempt in 0..2 {
            let idx = (self.active + attempt) % 2;
            if !self.conns[idx].is_connected() {
                continue;
            }
            match self.conns[idx].try_post(body) {
                Ok(resp) => {
                    if idx != self.active {
                        self.failovers += 1;
                        self.active = idx; // stick to the one that worked
                    }
                    return Ok(resp);
                }
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    last_err = Some(f.err);
                }
            }
        }
        // Both warm posts failed (or neither was connected): last-resort reconnect on the active.
        let idx = self.active;
        if self.conns[idx].ensure_connected().is_ok() {
            match self.conns[idx].try_post(body) {
                Ok(resp) => return Ok(resp),
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    last_err = Some(f.err);
                }
            }
        }
        let err = last_err.unwrap_or_else(|| SubmitError::Tls("no submit connection".into()));
        Err(SubmitFailure {
            err,
            delivery: if possibly_written {
                DeliveryState::PossiblyWritten
            } else {
                DeliveryState::DefinitelyNotWritten
            },
        })
    }

    /// Submit `body` only while the caller's captured authority permit remains current. The
    /// predicate is evaluated once at the closest pre-write boundary of every warm, spare, and
    /// reconnect attempt. Revocation stops the retry chain immediately. If an earlier same-byte
    /// attempt may have written, its [`DeliveryState::PossiblyWritten`] taint is retained.
    pub fn submit_authorized(
        &mut self,
        body: &[u8],
        authorize: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, SubmitFailure> {
        let mut last_err: Option<SubmitError> = None;
        let mut possibly_written = false;
        for attempt in 0..2 {
            let idx = (self.active + attempt) % 2;
            if !self.conns[idx].is_connected() {
                continue;
            }
            match self.conns[idx].try_post_authorized(body, authorize) {
                Ok(resp) => {
                    if idx != self.active {
                        self.failovers += 1;
                        self.active = idx;
                    }
                    return Ok(resp);
                }
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    if matches!(f.err, SubmitError::AuthorityRevoked) {
                        return Err(SubmitFailure {
                            err: f.err,
                            delivery: if possibly_written {
                                DeliveryState::PossiblyWritten
                            } else {
                                DeliveryState::DefinitelyNotWritten
                            },
                        });
                    }
                    last_err = Some(f.err);
                }
            }
        }
        let idx = self.active;
        if self.conns[idx].ensure_connected().is_ok() {
            match self.conns[idx].try_post_authorized(body, authorize) {
                Ok(resp) => return Ok(resp),
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    if matches!(f.err, SubmitError::AuthorityRevoked) {
                        return Err(SubmitFailure {
                            err: f.err,
                            delivery: if possibly_written {
                                DeliveryState::PossiblyWritten
                            } else {
                                DeliveryState::DefinitelyNotWritten
                            },
                        });
                    }
                    last_err = Some(f.err);
                }
            }
        }
        let err = last_err.unwrap_or_else(|| SubmitError::Tls("no submit connection".into()));
        Err(SubmitFailure {
            err,
            delivery: if possibly_written {
                DeliveryState::PossiblyWritten
            } else {
                DeliveryState::DefinitelyNotWritten
            },
        })
    }

    /// Submit the same byte-exact `body` while reporting attempt-scoped internal wire boundaries.
    /// This is the sampled `NETWORK_FINE=1` path only; ordinary submits continue through
    /// [`Self::submit`]. A retry starts a fresh chain with [`SubmitBoundary::AttemptStart`] so a
    /// failed first connection is never misrepresented as one monotonic stage on the spare.
    pub fn submit_observed(
        &mut self,
        body: &[u8],
        observe: &mut dyn FnMut(SubmitBoundaryStamp),
    ) -> Result<Vec<u8>, SubmitFailure> {
        let mut last_err: Option<SubmitError> = None;
        let mut possibly_written = false;
        for attempt in 0..2 {
            let idx = (self.active + attempt) % 2;
            if !self.conns[idx].is_connected() {
                continue;
            }
            observe(SubmitBoundaryStamp::now(SubmitBoundary::AttemptStart));
            match self.conns[idx].try_post_observed(body, observe) {
                Ok(resp) => {
                    if idx != self.active {
                        self.failovers += 1;
                        self.active = idx;
                    }
                    return Ok(resp);
                }
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    last_err = Some(f.err);
                }
            }
        }
        let idx = self.active;
        if self.conns[idx].ensure_connected().is_ok() {
            observe(SubmitBoundaryStamp::now(SubmitBoundary::AttemptStart));
            match self.conns[idx].try_post_observed(body, observe) {
                Ok(resp) => return Ok(resp),
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    last_err = Some(f.err);
                }
            }
        }
        let err = last_err.unwrap_or_else(|| SubmitError::Tls("no submit connection".into()));
        Err(SubmitFailure {
            err,
            delivery: if possibly_written {
                DeliveryState::PossiblyWritten
            } else {
                DeliveryState::DefinitelyNotWritten
            },
        })
    }

    /// Instrumented counterpart of [`Self::submit_authorized`]. Each retry still begins a fresh
    /// attempt timing chain, and each actual connection write consumes the authority predicate once.
    pub fn submit_observed_authorized(
        &mut self,
        body: &[u8],
        observe: &mut dyn FnMut(SubmitBoundaryStamp),
        authorize: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, SubmitFailure> {
        let mut last_err: Option<SubmitError> = None;
        let mut possibly_written = false;
        for attempt in 0..2 {
            let idx = (self.active + attempt) % 2;
            if !self.conns[idx].is_connected() {
                continue;
            }
            observe(SubmitBoundaryStamp::now(SubmitBoundary::AttemptStart));
            match self.conns[idx].try_post_observed_authorized(body, observe, authorize) {
                Ok(resp) => {
                    if idx != self.active {
                        self.failovers += 1;
                        self.active = idx;
                    }
                    return Ok(resp);
                }
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    if matches!(f.err, SubmitError::AuthorityRevoked) {
                        return Err(SubmitFailure {
                            err: f.err,
                            delivery: if possibly_written {
                                DeliveryState::PossiblyWritten
                            } else {
                                DeliveryState::DefinitelyNotWritten
                            },
                        });
                    }
                    last_err = Some(f.err);
                }
            }
        }
        let idx = self.active;
        if self.conns[idx].ensure_connected().is_ok() {
            observe(SubmitBoundaryStamp::now(SubmitBoundary::AttemptStart));
            match self.conns[idx].try_post_observed_authorized(body, observe, authorize) {
                Ok(resp) => return Ok(resp),
                Err(f) => {
                    possibly_written |= f.delivery == DeliveryState::PossiblyWritten;
                    if matches!(f.err, SubmitError::AuthorityRevoked) {
                        return Err(SubmitFailure {
                            err: f.err,
                            delivery: if possibly_written {
                                DeliveryState::PossiblyWritten
                            } else {
                                DeliveryState::DefinitelyNotWritten
                            },
                        });
                    }
                    last_err = Some(f.err);
                }
            }
        }
        let err = last_err.unwrap_or_else(|| SubmitError::Tls("no submit connection".into()));
        Err(SubmitFailure {
            err,
            delivery: if possibly_written {
                DeliveryState::PossiblyWritten
            } else {
                DeliveryState::DefinitelyNotWritten
            },
        })
    }

    /// Keep BOTH connections warm: re-establish any that dropped, then send `keepalive_body`
    /// (e.g. an `eth_chainId`) on each so an idle socket isn't closed by the peer. Call on the idle
    /// heartbeat cadence. Errors are swallowed — a failed keepalive just drops that socket and the
    /// next call re-warms it. Returns the number connected afterward.
    pub fn heartbeat_all(&mut self, keepalive_body: &[u8]) -> usize {
        for c in &mut self.conns {
            if c.ensure_connected().is_ok() {
                let _ = c.try_post(keepalive_body);
            }
        }
        self.conns.iter().filter(|c| c.is_connected()).count()
    }

    /// Index of the currently-preferred connection (telemetry/tests).
    pub fn active(&self) -> usize {
        self.active
    }

    /// Number of failovers performed so far (telemetry).
    pub fn failovers(&self) -> u64 {
        self.failovers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Fault-injecting mock: `connected` tracks socket state; `fail_next` makes the next `try_post`
    /// fail (and drop the socket, as the real `try_post` does); `posts` counts successful posts.
    struct MockConn {
        connected: bool,
        fail_posts: Cell<u32>, // number of upcoming posts that should fail
        posts: Cell<u32>,
        connects: Cell<u32>,
        ever_fails_connect: bool,
        // P7B: delivery a failing post reports. Default PossiblyWritten (a real write failure can't
        // prove nothing reached the peer); tests can flip it to DefinitelyNotWritten.
        fail_delivery: DeliveryState,
        observed_bodies: RefCell<Vec<Vec<u8>>>,
    }
    impl MockConn {
        fn new() -> Self {
            Self {
                connected: false,
                fail_posts: Cell::new(0),
                posts: Cell::new(0),
                connects: Cell::new(0),
                ever_fails_connect: false,
                fail_delivery: DeliveryState::PossiblyWritten,
                observed_bodies: RefCell::new(Vec::new()),
            }
        }
        fn dead_socket() -> Self {
            let mut m = Self::new();
            m.ever_fails_connect = true;
            m
        }
    }
    impl SubmitConn for MockConn {
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn ensure_connected(&mut self) -> Result<(), SubmitError> {
            self.connects.set(self.connects.get() + 1);
            if self.ever_fails_connect {
                return Err(SubmitError::Tls("connect refused".into()));
            }
            self.connected = true;
            Ok(())
        }
        fn try_post(&mut self, _body: &[u8]) -> Result<Vec<u8>, SubmitFailure> {
            if !self.connected {
                // Never connected ⇒ nothing was attempted.
                return Err(SubmitFailure::not_written(SubmitError::Tls(
                    "not connected".into(),
                )));
            }
            if self.fail_posts.get() > 0 {
                self.fail_posts.set(self.fail_posts.get() - 1);
                self.connected = false; // real try_post drops the dead socket
                return Err(SubmitFailure {
                    err: SubmitError::Tls("write failed".into()),
                    delivery: self.fail_delivery,
                });
            }
            self.posts.set(self.posts.get() + 1);
            Ok(b"ok".to_vec())
        }

        fn try_post_observed(
            &mut self,
            body: &[u8],
            observe: &mut dyn FnMut(SubmitBoundaryStamp),
        ) -> Result<Vec<u8>, SubmitFailure> {
            self.observed_bodies.borrow_mut().push(body.to_vec());
            observe(SubmitBoundaryStamp::now(SubmitBoundary::HttpFrameComplete));
            self.try_post(body)
        }
    }

    #[test]
    fn observed_failover_is_attempt_scoped_and_reuses_identical_body() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        hs.rewarm_idle();
        hs.conns[0].fail_posts.set(1);
        let body = b"signed-json-body-must-not-change";
        let mut boundaries = Vec::new();

        assert_eq!(
            hs.submit_observed(body, &mut |stamp| boundaries.push(stamp.boundary))
                .unwrap(),
            b"ok"
        );
        assert_eq!(
            boundaries,
            [
                SubmitBoundary::AttemptStart,
                SubmitBoundary::HttpFrameComplete,
                SubmitBoundary::AttemptStart,
                SubmitBoundary::HttpFrameComplete,
            ],
            "each retry must begin a fresh timing chain",
        );
        assert_eq!(&*hs.conns[0].observed_bodies.borrow(), &[body.to_vec()]);
        assert_eq!(&*hs.conns[1].observed_bodies.borrow(), &[body.to_vec()]);
    }

    #[test]
    fn rewarm_connects_both_and_submit_uses_active() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        assert_eq!(hs.rewarm_idle(), 2, "both warmed");
        assert_eq!(hs.submit(b"x").unwrap(), b"ok");
        assert_eq!(hs.active(), 0, "active stays primary on success");
        assert_eq!(hs.failovers(), 0);
    }

    #[test]
    fn primary_failure_fails_over_to_warm_spare_instantly() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        hs.rewarm_idle();
        // Make the primary's next post fail; the spare is warm and must take over with no reconnect.
        hs.conns[0].fail_posts.set(1);
        let connects_before = hs.conns[1].connects.get();
        assert_eq!(hs.submit(b"x").unwrap(), b"ok");
        assert_eq!(hs.active(), 1, "failover made the spare active (sticky)");
        assert_eq!(hs.failovers(), 1);
        assert_eq!(
            hs.conns[1].connects.get(),
            connects_before,
            "spare was already warm — no handshake on the failover"
        );
    }

    #[test]
    fn authority_revoked_before_first_attempt_writes_nothing() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        hs.rewarm_idle();

        let failure = hs
            .submit_authorized(b"must-not-reach-wire", &mut || false)
            .expect_err("revoked authority must refuse the submit");

        assert!(matches!(failure.err, SubmitError::AuthorityRevoked));
        assert_eq!(failure.delivery, DeliveryState::DefinitelyNotWritten);
        assert_eq!(hs.conns[0].posts.get(), 0);
        assert_eq!(hs.conns[1].posts.get(), 0);
    }

    #[test]
    fn authority_revoked_between_hot_spare_attempts_blocks_retry() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        hs.rewarm_idle();
        hs.conns[0].fail_posts.set(1);
        let checks = Cell::new(0u32);

        let failure = hs
            .submit_authorized(b"same-byte-body", &mut || {
                let check = checks.get();
                checks.set(check + 1);
                check == 0
            })
            .expect_err("the spare retry must observe revoked authority");

        assert!(matches!(failure.err, SubmitError::AuthorityRevoked));
        assert_eq!(
            failure.delivery,
            DeliveryState::PossiblyWritten,
            "the failed first attempt still taints nonce delivery conservatively"
        );
        assert_eq!(checks.get(), 2, "one authority check per attempted write");
        assert_eq!(hs.conns[1].posts.get(), 0, "spare received no write");
        assert_eq!(
            hs.conns[1].connects.get(),
            1,
            "revocation must also suppress the last-resort reconnect path"
        );
    }

    #[test]
    fn both_warm_fail_then_last_resort_reconnect_on_active() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        hs.rewarm_idle();
        // Both warm posts fail once; the last-resort reconnect on active then succeeds.
        hs.conns[0].fail_posts.set(1);
        hs.conns[1].fail_posts.set(1);
        assert_eq!(hs.submit(b"x").unwrap(), b"ok");
    }

    #[test]
    fn returns_error_when_everything_is_down() {
        let mut hs = HotSpare::new(MockConn::dead_socket(), MockConn::dead_socket());
        assert_eq!(hs.rewarm_idle(), 0, "neither can connect");
        assert!(hs.submit(b"x").is_err(), "both dead → error, not a panic");
    }

    #[test]
    fn all_attempts_possibly_written_aggregates_to_possibly_written() {
        // P7B core guarantee: when EVERY attempt fails and at least one may have written, the
        // aggregate delivery is PossiblyWritten (disarm the lane; never reclaim for different bytes).
        // Both connections are warm but permanently fail their posts as PossiblyWritten, and the
        // last-resort reconnect also fails (ever_fails_connect on active after drop).
        let mut a = MockConn::new();
        a.fail_posts.set(u32::MAX); // always fail
        a.fail_delivery = DeliveryState::PossiblyWritten;
        a.ever_fails_connect = true; // last-resort reconnect on active fails too
        let mut b = MockConn::new();
        b.fail_posts.set(u32::MAX);
        b.fail_delivery = DeliveryState::PossiblyWritten;
        // Warm both first (ever_fails_connect blocks re-warm, so connect them by hand).
        a.connected = true;
        b.connected = true;
        let mut hs = HotSpare::new(a, b);
        match hs.submit(b"x") {
            Err(f) => assert_eq!(
                f.delivery,
                DeliveryState::PossiblyWritten,
                "any possibly-written attempt taints the aggregate"
            ),
            Ok(_) => panic!("all attempts should have failed"),
        }
    }

    #[test]
    fn definitely_not_written_when_no_attempt_reached_the_wire() {
        // P7B: if nothing was ever connected/written, the aggregate is DefinitelyNotWritten — the
        // nonce is safe to reclaim. Both connections are dead (connect refused), so no post is ever
        // attempted.
        let mut hs = HotSpare::new(MockConn::dead_socket(), MockConn::dead_socket());
        hs.rewarm_idle();
        match hs.submit(b"x") {
            Err(f) => assert_eq!(
                f.delivery,
                DeliveryState::DefinitelyNotWritten,
                "nothing was sent — safe to reclaim the nonce"
            ),
            Ok(_) => panic!("both dead → error"),
        }
    }

    #[test]
    fn heartbeat_all_keeps_both_warm_and_revives_dropped() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        assert_eq!(hs.heartbeat_all(b"ping"), 2, "both warmed + kept alive");
        let posts0 = hs.conns[0].posts.get();
        let posts1 = hs.conns[1].posts.get();
        assert!(posts0 >= 1 && posts1 >= 1, "keepalive posted on both");
        // Drop the spare's socket; heartbeat must revive it.
        hs.conns[1].connected = false;
        assert_eq!(hs.heartbeat_all(b"ping"), 2, "dropped spare revived");
        assert!(hs.conns[1].is_connected());
    }

    #[test]
    fn recovers_after_failed_connection_is_rewarmed() {
        let mut hs = HotSpare::new(MockConn::new(), MockConn::new());
        hs.rewarm_idle();
        hs.conns[0].fail_posts.set(1);
        hs.submit(b"x").unwrap(); // fails over to spare (now active=1), primary socket dropped
        assert!(
            !hs.conns[0].is_connected(),
            "primary socket dropped after its failure"
        );
        // Idle re-warm re-establishes the primary off the hot path.
        assert_eq!(hs.rewarm_idle(), 2, "primary re-warmed");
        assert!(hs.conns[0].is_connected());
    }
}
