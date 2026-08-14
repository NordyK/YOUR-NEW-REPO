//! Replay driver.
//!
//! Walks a `FeedFrameSource`, runs the full decode pipeline for each frame, and emits
//! `ReplayStats` describing the outcome.

use engine_feed::{
    decode_envelope, decode_l2msg_batch, decode_signed_tx, EnvelopeFields, L2MsgKind,
};

use crate::corpus::FeedFrame;

/// Aggregate statistics for a replay run.
#[derive(Debug, Default, Clone)]
pub struct ReplayStats {
    /// Number of frames consumed.
    pub frames_seen: u64,
    /// Number of frames that parsed successfully.
    pub frames_parsed: u64,
    /// Number of signed transactions decoded.
    pub signed_tx_count: u64,
    /// Number of batch transactions decoded.
    pub batch_tx_count: u64,
    /// Sequence-number gaps observed.
    pub sequence_gaps: u64,
    /// Last seen sequence number.
    pub last_sequence: u64,
    /// Known L2 messages that are not signed swap-bearing traffic.
    pub unsupported_non_swap_messages: u64,
    /// Unknown/reserved/compressed envelope kinds that this replay path cannot decode.
    pub unsupported_envelope_kind_drops: u64,
    /// Malformed frames in a place where the feed shape should have been decodable.
    pub parser_bug_drops: u64,
    /// Signed swap-bearing traffic that failed tx decoding and may affect arbitrage.
    pub relevant_swap_drops: u64,
    /// Drops that do not fit a more precise taxonomy bucket yet.
    pub unknown_drops: u64,
}

/// Correlated per-frame decode boundaries from one `CLOCK_MONOTONIC_RAW` clock domain.
///
/// This is opt-in through [`ReplayDriver::process_timed`]. The ordinary [`ReplayDriver::process`]
/// path performs no additional clock reads and keeps its existing output/counters unchanged.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReplayFrameTiming {
    /// Boundary immediately before envelope decoding.
    pub start_ns: u64,
    /// Boundary immediately after the JSON feed envelope decoded.
    pub envelope_done_ns: u64,
    /// Boundary immediately after the base64 L2 message decoded.
    pub base64_done_ns: u64,
    /// Boundary after the L2 message/transaction decode completed.
    pub l2_decode_done_ns: u64,
}

impl ReplayFrameTiming {
    fn start() -> Self {
        Self {
            start_ns: engine_sysopt::clock_monotonic_raw_ns(),
            ..Self::default()
        }
    }

    /// Complete correlated frame-decode duration.
    pub fn total_ns(&self) -> u64 {
        self.l2_decode_done_ns.saturating_sub(self.start_ns)
    }
}

/// Driver wraps a stats accumulator + base64 decode scratch.
pub struct ReplayDriver {
    stats: ReplayStats,
    decode_scratch: Vec<u8>,
}

impl ReplayDriver {
    /// New driver.
    pub fn new() -> Self {
        Self {
            stats: ReplayStats::default(),
            decode_scratch: Vec::with_capacity(64 * 1024),
        }
    }

    /// Process one frame.
    pub fn process(&mut self, frame: &FeedFrame) -> Result<(), DriverError> {
        self.process_inner(frame, None)
    }

    /// Process one frame while returning correlated RAW-clock decode boundaries.
    ///
    /// A decoding error is returned exactly as on [`Self::process`]. Timing is intentionally not
    /// reported for a partial/error chain, preventing callers from treating incomplete boundaries
    /// as a valid per-event sample.
    pub fn process_timed(&mut self, frame: &FeedFrame) -> Result<ReplayFrameTiming, DriverError> {
        let mut timing = ReplayFrameTiming::start();
        self.process_inner(frame, Some(&mut timing))?;
        Ok(timing)
    }

    fn process_inner(
        &mut self,
        frame: &FeedFrame,
        mut timing: Option<&mut ReplayFrameTiming>,
    ) -> Result<(), DriverError> {
        self.stats.frames_seen += 1;
        let env: EnvelopeFields = match decode_envelope(&frame.raw) {
            Ok(e) => e,
            Err(_) => return Err(DriverError::EnvelopeDecodeFailed),
        };
        if let Some(stamps) = timing.as_deref_mut() {
            stamps.envelope_done_ns = engine_sysopt::clock_monotonic_raw_ns();
        }
        if self.stats.frames_parsed > 0 && env.sequence_number != self.stats.last_sequence + 1 {
            self.stats.sequence_gaps += 1;
        }
        self.stats.last_sequence = env.sequence_number;
        self.stats.frames_parsed += 1;
        // Decode the base64 body into scratch.
        let body = env.l2msg_b64;
        // Worst-case base64 decoded length is `body.len() * 3 / 4`.
        let max_out_len = body.len().saturating_mul(3) / 4 + 4;
        self.decode_scratch.clear();
        self.decode_scratch.resize(max_out_len, 0);
        // Copy into scratch buffer first because base64-simd decodes in place.
        self.decode_scratch.truncate(0);
        self.decode_scratch.extend_from_slice(body);
        // Pad to even alignment.
        let scratch = &mut self.decode_scratch[..];
        let decoded = match base64_simd::forgiving_decode_inplace(scratch) {
            Ok(d) => d.len(),
            Err(_) => return Err(DriverError::Base64DecodeFailed),
        };
        if let Some(stamps) = timing.as_deref_mut() {
            stamps.base64_done_ns = engine_sysopt::clock_monotonic_raw_ns();
        }
        let payload = &self.decode_scratch[..decoded];
        if payload.is_empty() {
            if let Some(stamps) = timing.as_deref_mut() {
                stamps.l2_decode_done_ns = engine_sysopt::clock_monotonic_raw_ns();
            }
            return Ok(());
        }
        let kind = match payload.first().and_then(|b| L2MsgKind::from_byte(*b)) {
            Some(kind) => kind,
            None => {
                self.stats.unsupported_envelope_kind_drops += 1;
                return Err(DriverError::L2MsgDecodeFailed);
            }
        };
        let rest = &payload[1..];
        match kind {
            L2MsgKind::SignedTx => {
                if decode_signed_tx(rest).is_some() {
                    self.stats.signed_tx_count += 1;
                } else if is_supported_signed_tx_head(rest.first().copied()) {
                    self.stats.relevant_swap_drops += 1;
                    return Err(DriverError::L2MsgDecodeFailed);
                } else {
                    self.stats.unsupported_envelope_kind_drops += 1;
                    return Err(DriverError::L2MsgDecodeFailed);
                }
            }
            L2MsgKind::Batch => {
                self.stats.batch_tx_count += 1;
                let mut signed = 0u64;
                let mut relevant_drops = 0u64;
                let mut unsupported = 0u64;
                decode_l2msg_batch(rest, |inner| {
                    match inner.first().and_then(|b| L2MsgKind::from_byte(*b)) {
                        Some(L2MsgKind::SignedTx) => {
                            if decode_signed_tx(&inner[1..]).is_some() {
                                signed += 1;
                            } else if is_supported_signed_tx_head(inner.get(1).copied()) {
                                relevant_drops += 1;
                            } else {
                                self.stats.unsupported_envelope_kind_drops += 1;
                            }
                        }
                        Some(
                            L2MsgKind::UnsignedUserTx
                            | L2MsgKind::ContractTx
                            | L2MsgKind::NonMutatingCall
                            | L2MsgKind::Heartbeat,
                        ) => unsupported += 1,
                        Some(L2MsgKind::Batch) => {
                            // Nested batches are valid but rare in the Nitro feed; decode_l2msg_batch
                            // itself walks one level, so mark this as an unsupported envelope rather
                            // than silently treating it as signed traffic.
                            self.stats.unsupported_envelope_kind_drops += 1;
                        }
                        Some(L2MsgKind::Reserved5 | L2MsgKind::SignedCompressedTx) | None => {
                            self.stats.unsupported_envelope_kind_drops += 1;
                        }
                    }
                    Ok(())
                })
                .map_err(|_| {
                    self.stats.parser_bug_drops += 1;
                    DriverError::L2MsgDecodeFailed
                })?;
                self.stats.signed_tx_count += signed;
                self.stats.relevant_swap_drops += relevant_drops;
                self.stats.unsupported_non_swap_messages += unsupported;
            }
            L2MsgKind::UnsignedUserTx
            | L2MsgKind::ContractTx
            | L2MsgKind::NonMutatingCall
            | L2MsgKind::Heartbeat => {
                self.stats.unsupported_non_swap_messages += 1;
            }
            L2MsgKind::Reserved5 | L2MsgKind::SignedCompressedTx => {
                self.stats.unsupported_envelope_kind_drops += 1;
                return Err(DriverError::L2MsgDecodeFailed);
            }
        }
        if let Some(stamps) = timing.as_deref_mut() {
            stamps.l2_decode_done_ns = engine_sysopt::clock_monotonic_raw_ns();
        }
        Ok(())
    }

    /// Borrow current stats.
    pub fn stats(&self) -> &ReplayStats {
        &self.stats
    }
}

fn is_supported_signed_tx_head(head: Option<u8>) -> bool {
    matches!(head, Some(0x01 | 0x02 | 0x04 | 0xc0..=0xff))
}

impl Default for ReplayDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Driver errors.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// JSON envelope decode failed.
    #[error("envelope decode failed")]
    EnvelopeDecodeFailed,
    /// Base64 decode failed.
    #[error("base64 decode failed")]
    Base64DecodeFailed,
    /// L2 message decode failed.
    #[error("l2 message decode failed")]
    L2MsgDecodeFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::FeedFrameSource;
    use base64_simd::STANDARD as BASE64_STANDARD;

    #[test]
    fn driver_counts_signed_tx() {
        let inner = signed_tx_payload();
        let encoded = BASE64_STANDARD.encode_to_string(&inner);
        let line =
            format!(r#"{{"sequenceNumber":1,"timestamp":1,"blockNumber":2,"l2Msg":"{encoded}"}}"#);
        let src = FeedFrameSource::from_lines(vec![line]);
        let mut d = ReplayDriver::new();
        for frame in src {
            d.process(&frame).expect("process ok");
        }
        let s = d.stats();
        assert_eq!(s.frames_parsed, 1);
        assert_eq!(s.signed_tx_count, 1);
    }

    #[test]
    fn timed_process_preserves_decode_result_and_uses_one_correlated_clock_chain() {
        let inner = signed_tx_payload();
        let encoded = BASE64_STANDARD.encode_to_string(&inner);
        let line =
            format!(r#"{{"sequenceNumber":9,"timestamp":1,"blockNumber":2,"l2Msg":"{encoded}"}}"#);
        let frame = FeedFrameSource::from_lines(vec![line])
            .next()
            .expect("one frame");

        let mut ordinary = ReplayDriver::new();
        ordinary.process(&frame).expect("ordinary process");

        let mut timed = ReplayDriver::new();
        let timing = timed.process_timed(&frame).expect("timed process");

        assert_eq!(timed.stats().frames_seen, ordinary.stats().frames_seen);
        assert_eq!(timed.stats().frames_parsed, ordinary.stats().frames_parsed);
        assert_eq!(
            timed.stats().signed_tx_count,
            ordinary.stats().signed_tx_count
        );
        assert!(timing.envelope_done_ns >= timing.start_ns);
        assert!(timing.base64_done_ns >= timing.envelope_done_ns);
        assert!(timing.l2_decode_done_ns >= timing.base64_done_ns);
        assert_eq!(
            timing.total_ns(),
            timing.l2_decode_done_ns.saturating_sub(timing.start_ns)
        );
    }

    #[test]
    fn driver_counts_signed_txs_inside_batch() {
        let signed = signed_tx_payload();
        let mut batch = Vec::new();
        batch.push(0x03u8); // kind=3 Batch
        batch.extend_from_slice(&(signed.len() as u64).to_be_bytes());
        batch.extend_from_slice(&signed);

        let encoded = BASE64_STANDARD.encode_to_string(&batch);
        let line =
            format!(r#"{{"sequenceNumber":1,"timestamp":1,"blockNumber":2,"l2Msg":"{encoded}"}}"#);
        let src = FeedFrameSource::from_lines(vec![line]);
        let mut d = ReplayDriver::new();
        for frame in src {
            d.process(&frame).expect("process ok");
        }
        let s = d.stats();
        assert_eq!(s.frames_parsed, 1);
        assert_eq!(s.batch_tx_count, 1);
        assert_eq!(s.signed_tx_count, 1);
    }

    #[test]
    fn driver_taxonomizes_non_swap_messages() {
        let encoded = BASE64_STANDARD.encode_to_string([0x06u8]); // kind=6 Heartbeat
        let line =
            format!(r#"{{"sequenceNumber":1,"timestamp":1,"blockNumber":2,"l2Msg":"{encoded}"}}"#);
        let src = FeedFrameSource::from_lines(vec![line]);
        let mut d = ReplayDriver::new();
        for frame in src {
            d.process(&frame).expect("process ok");
        }
        let s = d.stats();
        assert_eq!(s.unsupported_non_swap_messages, 1);
        assert_eq!(s.parser_bug_drops, 0);
    }

    #[test]
    fn driver_taxonomizes_unsupported_signed_tx_envelope_head() {
        let encoded = BASE64_STANDARD.encode_to_string([0x04u8, 0x6au8, 0x00u8]);
        let line =
            format!(r#"{{"sequenceNumber":1,"timestamp":1,"blockNumber":2,"l2Msg":"{encoded}"}}"#);
        let src = FeedFrameSource::from_lines(vec![line]);
        let mut d = ReplayDriver::new();
        for frame in src {
            assert!(d.process(&frame).is_err());
        }
        let s = d.stats();
        assert_eq!(s.unsupported_envelope_kind_drops, 1);
        assert_eq!(s.relevant_swap_drops, 0);
    }

    fn signed_tx_payload() -> Vec<u8> {
        let mut inner = Vec::new();
        inner.push(0x04u8); // kind=4 SignedTx
        let payload: Vec<u8> = (0..9).map(|_| 0x80u8).collect();
        inner.push(0xc9);
        inner.extend_from_slice(&payload);
        inner
    }
}
