//! Binary feed-frame protocol for the S2 (relay → bot) fast path.
//!
//! The stock nitro relay hands the bot JSON-over-WebSocket on loopback (~7–15 µs + a fat
//! JSON-parse tail). The optimization (sequencer-latency report §4) is to parse the feed **once**
//! in a sidecar/forked relay and hand the bot a fixed binary frame it consumes with zero JSON,
//! base64, RLP, or WebSocket framing. This module is that wire format + a manual little-endian
//! codec (no `unsafe`, no `transmute`, fixed endianness so a laptop x86 sidecar and an AWS ARM bot
//! agree). One frame == one already-decoded signed-tx (the unit the state writer marks dirty on);
//! over `SOCK_SEQPACKET` one datagram carries exactly one frame.
//!
//! # Two on-wire formats
//!
//! ## V1 — legacy, header = 49 bytes (magic `0xF00DFEED`)
//! Hot-path only: pre-decoded (to, selector, calldata). The engine's binary fast path drove
//! state-update fanout directly from these frames. Shadow REVM was DISABLED on the binary path
//! because there was no way to deliver the raw signed-tx envelope shadow needs to replay txs.
//!
//! ## V2 — current, header = 53 bytes (magic `0xF00DFEE2`)
//! Adds a length-prefixed **envelope** section after the calldata body — the exact bytes
//! shadow REVM needs (`recover_tx_sender`/`decode_signed_tx` input, i.e. post-L2-kind-byte
//! signed-tx). With V2 the binary fast path AND shadow co-exist: no "binary vs shadow" tradeoff.
//!
//! Layout (little-endian; V2 header is 53 bytes):
//! ```text
//!  0  magic         u32   = 0xF00DFEED (V1) | 0xF00DFEE2 (V2)
//!  4  seq           u64   sequenceNumber (FCFS order)
//! 12  recv_ns       u64   sidecar capture timestamp (monotonic ns; HW-stamped on AWS)
//! 20  has_to        u8    1 if `to` is present, else 0 (contract creation)
//! 21  selector      [u8;4] first 4 calldata bytes (0 if calldata < 4)
//! 25  to            [u8;20] destination address (zeroed if !has_to)
//! 45  data_len      u32   length of the post-selector calldata body
//! 49  envelope_len  u32   (V2 only) length of the trailing signed-tx envelope; 0 = no envelope
//! 53  data ...            `data_len` bytes
//!     envelope ...        (V2 only) `envelope_len` bytes — post-L2-kind-byte raw signed tx
//! ```
//!
//! The two magics let an engine speak both protocols (new ↔ old sidecar and vice versa) without
//! a coordinated restart: `decode_frame` inspects the magic and dispatches. The hot-path receiver
//! in the engine reads the V1 header, checks the magic, and only reads the extra `envelope_len`
//! word if it's a V2 frame — see `crates/engine/src/pipeline.rs` (`rx_loop_binunix`).

#![allow(clippy::needless_range_loop)]

/// Sanity/version magic at the head of a V1 binary feed frame (no envelope section).
pub const FEED_FRAME_MAGIC: u32 = 0xF00D_FEED;
/// V2 magic: same head as V1 plus a `(envelope_len: u32, envelope: [u8])` trailer carrying the
/// raw signed-tx envelope shadow REVM needs. Engines/sidecars can mix freely — decoder picks
/// the layout from the magic alone.
pub const FEED_FRAME_MAGIC_V2: u32 = 0xF00D_FEE2;
/// V3 magic: V2 layout plus an 8-byte `header_timestamp` (the sequencer `header.timestamp`, unix
/// seconds) inserted after the envelope_len word. Lets the binary fast path compute event age
/// (`recv_ns − header_timestamp`) — Gate 8 feed-freshness — without re-parsing the relay JSON.
pub const FEED_FRAME_MAGIC_V3: u32 = 0xF00D_FEE3;
/// V4 magic (WI-03): V3 layout plus a 4-byte canonical `inner_ordinal` and a 4-byte `flags`
/// word after `header_timestamp`. The ordinal is the depth-first, left-to-right leaf index the
/// sidecar's canonical normalizer assigned (FS-06 exact same-sequence cutoffs); the flags word
/// carries marker bits + a taint reason code so an undecoded state-bearing leaf reaches the
/// engine as a typed signal instead of disappearing in the sidecar (FL-04 on the binary path),
/// while a documented ignorable leaf can consume its canonical ordinal without carrying state.
pub const FEED_FRAME_MAGIC_V4: u32 = 0xF00D_FEE4;
/// V1 header length in bytes (everything before the calldata body).
pub const FEED_FRAME_HEADER_LEN: usize = 49;
/// V2 header length in bytes (V1 header + 4-byte envelope_len).
pub const FEED_FRAME_HEADER_LEN_V2: usize = 53;
/// V3 header length in bytes (V2 header + 8-byte sequencer header_timestamp).
pub const FEED_FRAME_HEADER_LEN_V3: usize = 61;
/// V4 header length in bytes (V3 header + 4-byte inner_ordinal + 4-byte flags).
pub const FEED_FRAME_HEADER_LEN_V4: usize = 69;

/// Control-plane magic for the exact feed-payload-conflict sentinel.
pub const FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC: u32 = 0xD2C0_0F07;

const fn make_feed_payload_conflict_sentinel() -> [u8; FEED_FRAME_HEADER_LEN] {
    let mut sentinel = [0u8; FEED_FRAME_HEADER_LEN];
    // Control-plane magic is deliberately distinct from every data-frame version. The remaining
    // bytes stay zero so the exact record cannot be mistaken for a valid frame with a payload.
    sentinel[0] = (FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC & 0xFF) as u8;
    sentinel[1] = ((FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC >> 8) & 0xFF) as u8;
    sentinel[2] = ((FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC >> 16) & 0xFF) as u8;
    sentinel[3] = ((FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC >> 24) & 0xFF) as u8;
    sentinel
}

/// Exact 49-byte control record used only to carry a typed feed-payload conflict over the local
/// sidecar socket. Bytes 0..4 are a control magic; bytes 4..49 are zero. It is never a V4 frame.
pub const FEED_PAYLOAD_CONFLICT_SENTINEL: [u8; FEED_FRAME_HEADER_LEN] =
    make_feed_payload_conflict_sentinel();

/// Return true only for the complete typed feed-payload-conflict control record.
pub fn is_feed_payload_conflict_sentinel(buf: &[u8]) -> bool {
    buf == FEED_PAYLOAD_CONFLICT_SENTINEL.as_slice()
}

/// V4 flags bit 0: this frame is a TAINT MARKER — a canonical leaf the sidecar could not
/// decode into an authoritative transaction. It carries no calldata head and no envelope;
/// bits 8..16 hold the stable `TaintReason`/`MalformedReason` code. The engine advances its
/// taint watermark and must never derive pool state from it.
pub const FEED_FRAME_FLAG_TAINT: u32 = 1;

/// V4 flags bit 1: this frame accounts for one canonically ordered leaf that the
/// normalizer proved ignorable. It advances ordinal contiguity but carries no state,
/// does not move the taint watermark, and never authorizes publication.
pub const FEED_FRAME_FLAG_IGNORABLE_ORDINAL: u32 = 1 << 1;

const FEED_FRAME_TAINT_REASON_MASK: u32 = 0x0000_FF00;
const FEED_FRAME_KNOWN_FLAGS: u32 = FEED_FRAME_FLAG_TAINT
    | FEED_FRAME_FLAG_IGNORABLE_ORDINAL
    | FEED_FRAME_TAINT_REASON_MASK;

/// Typed V4 marker vocabulary. Marker frames have the canonical empty marker shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedFrameMarker {
    /// An undecoded state-bearing canonical leaf with its stable reason code.
    Taint(u8),
    /// A documented non-state-bearing canonical leaf that still consumes one ordinal.
    IgnorableOrdinal,
}

/// Whether a decoded binary frame can back state/submit authority. Authority requires ALL of:
/// the ordinal-bearing V4+ wire (pre-V4 versions carry no canonical `inner_ordinal`, so a
/// receiver cannot distinguish a safe single-child frame from collapsed multi-child
/// same-sequence siblings — codex WI0304_REVERIFY item 2c), a raw signed-tx envelope
/// (canonical replay identity — FS-08), and neither V4 marker. Everything else is diagnostic
/// only and must be quarantined from authoritative state/submission use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedFrameAuthority {
    /// Ordinal-bearing wire (V4+) with the canonical signed-tx envelope: full replay identity.
    Authoritative,
    /// Pre-V4 wire (ordinal-less), no envelope, or a marker: diagnostic only, never
    /// state/submit authority.
    NonAuthoritative,
}

/// A decoded binary feed frame borrowed out of a receive buffer (zero-copy body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedFrameView<'a> {
    /// FCFS sequence number.
    pub seq: u64,
    /// Sidecar capture timestamp (wall-clock UNIX ns).
    pub recv_ns: u64,
    /// Sequencer `header.timestamp` (unix seconds) — V3 frames only; `0` for V1/V2 (unknown). Lets
    /// the consumer compute feed event age = `recv_ns − header_timestamp·1e9` (Gate 8 freshness).
    pub header_timestamp: u64,
    /// Destination address, or `None` for contract creation.
    pub to: Option<[u8; 20]>,
    /// First 4 calldata bytes (selector).
    pub selector: [u8; 4],
    /// Post-selector calldata body.
    pub data: &'a [u8],
    /// Raw signed-tx envelope (post-L2-kind-byte), or `None` for V1 frames or V2 frames whose
    /// sidecar opted not to carry the envelope. Shadow REVM consumes this when present.
    pub envelope: Option<&'a [u8]>,
    /// Canonical depth-first leaf ordinal within `seq` (V4 frames; `0` for V1–V3, whose
    /// sidecars predate the canonical normalizer and cannot distinguish same-seq siblings).
    pub inner_ordinal: u32,
    /// V4 flags word: ordinary `0`, taint marker (`FEED_FRAME_FLAG_TAINT` | reason-code << 8),
    /// or `FEED_FRAME_FLAG_IGNORABLE_ORDINAL`; `0` for V1–V3.
    pub flags: u32,
    /// Wire version this frame arrived on (1–4). The receiver-side authority gate keys on
    /// it: only ordinal-bearing versions (≥ 4) can be authoritative.
    pub wire_version: u8,
}

impl FeedFrameView<'_> {
    /// Whether this frame is a V4 taint marker (an undecoded state-bearing canonical leaf).
    pub fn is_taint_marker(&self) -> bool {
        self.flags & FEED_FRAME_FLAG_TAINT != 0
    }

    /// Whether this frame is a V4 ignorable-ordinal marker.
    pub fn is_ignorable_ordinal_marker(&self) -> bool {
        self.flags == FEED_FRAME_FLAG_IGNORABLE_ORDINAL
    }

    /// Stable taint reason code carried by a V4 taint marker (`None` otherwise).
    pub fn taint_reason_code(&self) -> Option<u8> {
        self.is_taint_marker()
            .then_some(((self.flags & FEED_FRAME_TAINT_REASON_MASK) >> 8) as u8)
    }

    /// Typed authority of this frame (see [`FeedFrameAuthority`]): authoritative iff the
    /// wire carries the canonical `inner_ordinal` (version ≥ 4), the frame carries the
    /// canonical signed-tx envelope (FS-08 replay identity), and it is neither marker type.
    /// Pre-V4 frames are NEVER authoritative — envelope or not — because their leaf
    /// position within a multi-child sequence is unprovable (codex WI0304_REVERIFY 2c).
    pub fn authority(&self) -> FeedFrameAuthority {
        if self.wire_version >= 4
            && self.envelope.is_some()
            && !self.is_taint_marker()
            && !self.is_ignorable_ordinal_marker()
        {
            FeedFrameAuthority::Authoritative
        } else {
            FeedFrameAuthority::NonAuthoritative
        }
    }
}

/// V1 encoder — header (49) + calldata body. Kept for backward-compatible sidecars; new code
/// should call [`encode_frame_v2`] to also carry the signed-tx envelope shadow needs.
pub fn encode_frame(
    out: &mut Vec<u8>,
    seq: u64,
    recv_ns: u64,
    to: Option<[u8; 20]>,
    selector: [u8; 4],
    data: &[u8],
) -> usize {
    out.clear();
    out.reserve(FEED_FRAME_HEADER_LEN + data.len());
    out.extend_from_slice(&FEED_FRAME_MAGIC.to_le_bytes()); // 0..4
    out.extend_from_slice(&seq.to_le_bytes()); // 4..12
    out.extend_from_slice(&recv_ns.to_le_bytes()); // 12..20
    out.push(to.is_some() as u8); // 20
    out.extend_from_slice(&selector); // 21..25
    out.extend_from_slice(&to.unwrap_or([0u8; 20])); // 25..45
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // 45..49
    out.extend_from_slice(data); // 49..
    out.len()
}

/// V2 encoder — header (53) + calldata body + optional signed-tx envelope.
///
/// When `envelope.is_some()`, the trailing `envelope_len` bytes carry exactly what
/// [`crate::recover_tx_sender`] / [`crate::decode_signed_tx`] expect (post-L2-kind-byte signed-tx
/// envelope). Pass `None` to emit a V2 frame with zero-length envelope (still V2 magic, so
/// receivers know to look for one; envelope_len=0 means "this sidecar didn't capture it").
pub fn encode_frame_v2(
    out: &mut Vec<u8>,
    seq: u64,
    recv_ns: u64,
    to: Option<[u8; 20]>,
    selector: [u8; 4],
    data: &[u8],
    envelope: Option<&[u8]>,
) -> usize {
    let env_len = envelope.map(|e| e.len()).unwrap_or(0);
    out.clear();
    out.reserve(FEED_FRAME_HEADER_LEN_V2 + data.len() + env_len);
    out.extend_from_slice(&FEED_FRAME_MAGIC_V2.to_le_bytes()); // 0..4
    out.extend_from_slice(&seq.to_le_bytes()); // 4..12
    out.extend_from_slice(&recv_ns.to_le_bytes()); // 12..20
    out.push(to.is_some() as u8); // 20
    out.extend_from_slice(&selector); // 21..25
    out.extend_from_slice(&to.unwrap_or([0u8; 20])); // 25..45
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // 45..49
    out.extend_from_slice(&(env_len as u32).to_le_bytes()); // 49..53
    out.extend_from_slice(data); // 53..53+data_len
    if let Some(env) = envelope {
        out.extend_from_slice(env); // 53+data_len..
    }
    out.len()
}

/// V3 encoder — V2 layout + an 8-byte sequencer `header_timestamp` (unix seconds) at offset 53, so
/// the binary consumer can compute feed event age without re-parsing the relay JSON. `0` ⇒ unknown.
///
/// Layout (little-endian; V3 header is 61 bytes): `..49` as V1, `49..53 envelope_len`,
/// `53..61 header_timestamp`, `61.. data`, then `envelope`.
#[allow(clippy::too_many_arguments)]
pub fn encode_frame_v3(
    out: &mut Vec<u8>,
    seq: u64,
    recv_ns: u64,
    header_timestamp: u64,
    to: Option<[u8; 20]>,
    selector: [u8; 4],
    data: &[u8],
    envelope: Option<&[u8]>,
) -> usize {
    let env_len = envelope.map(|e| e.len()).unwrap_or(0);
    out.clear();
    out.reserve(FEED_FRAME_HEADER_LEN_V3 + data.len() + env_len);
    out.extend_from_slice(&FEED_FRAME_MAGIC_V3.to_le_bytes()); // 0..4
    out.extend_from_slice(&seq.to_le_bytes()); // 4..12
    out.extend_from_slice(&recv_ns.to_le_bytes()); // 12..20
    out.push(to.is_some() as u8); // 20
    out.extend_from_slice(&selector); // 21..25
    out.extend_from_slice(&to.unwrap_or([0u8; 20])); // 25..45
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // 45..49
    out.extend_from_slice(&(env_len as u32).to_le_bytes()); // 49..53
    out.extend_from_slice(&header_timestamp.to_le_bytes()); // 53..61
    out.extend_from_slice(data); // 61..61+data_len
    if let Some(env) = envelope {
        out.extend_from_slice(env); // 61+data_len..
    }
    out.len()
}

/// V4 encoder — V3 layout + `inner_ordinal` (u32) + `flags` (u32) at offsets 61/65 (WI-03).
///
/// * `inner_ordinal` — the canonical depth-first leaf index the sidecar's normalizer assigned.
/// * `marker` — `None` emits an ordinary decoded-transaction frame;
///   `Some(FeedFrameMarker::Taint(code))` emits a taint marker (bit 0 plus code in bits 8..16);
///   `Some(FeedFrameMarker::IgnorableOrdinal)` emits an ignorable ordinal marker (bit 1 exactly).
///   Both marker forms require empty `data` and no envelope.
#[allow(clippy::too_many_arguments)]
pub fn encode_frame_v4(
    out: &mut Vec<u8>,
    seq: u64,
    recv_ns: u64,
    header_timestamp: u64,
    to: Option<[u8; 20]>,
    selector: [u8; 4],
    data: &[u8],
    envelope: Option<&[u8]>,
    inner_ordinal: u32,
    marker: Option<FeedFrameMarker>,
) -> usize {
    debug_assert!(
        marker.is_none()
            || (to.is_none()
                && selector == [0; 4]
                && data.is_empty()
                && envelope.is_none()),
        "V4 marker frames must have the canonical empty marker shape"
    );
    let env_len = envelope.map(|e| e.len()).unwrap_or(0);
    let flags = match marker {
        None => 0,
        Some(FeedFrameMarker::Taint(code)) => FEED_FRAME_FLAG_TAINT | ((code as u32) << 8),
        Some(FeedFrameMarker::IgnorableOrdinal) => FEED_FRAME_FLAG_IGNORABLE_ORDINAL,
    };
    out.clear();
    out.reserve(FEED_FRAME_HEADER_LEN_V4 + data.len() + env_len);
    out.extend_from_slice(&FEED_FRAME_MAGIC_V4.to_le_bytes()); // 0..4
    out.extend_from_slice(&seq.to_le_bytes()); // 4..12
    out.extend_from_slice(&recv_ns.to_le_bytes()); // 12..20
    out.push(to.is_some() as u8); // 20
    out.extend_from_slice(&selector); // 21..25
    out.extend_from_slice(&to.unwrap_or([0u8; 20])); // 25..45
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // 45..49
    out.extend_from_slice(&(env_len as u32).to_le_bytes()); // 49..53
    out.extend_from_slice(&header_timestamp.to_le_bytes()); // 53..61
    out.extend_from_slice(&inner_ordinal.to_le_bytes()); // 61..65
    out.extend_from_slice(&flags.to_le_bytes()); // 65..69
    out.extend_from_slice(data); // 69..69+data_len
    if let Some(env) = envelope {
        out.extend_from_slice(env); // 69+data_len..
    }
    out.len()
}

/// Decode a binary feed frame from `buf`. Recognizes both V1 and V2 magics:
/// * V1 → `envelope = None`.
/// * V2 → `envelope = Some(...)` when the sidecar attached one (envelope_len > 0), else `None`.
///
/// Fail-closed on every bounds violation or declared-length mismatch — never reads OOB and never
/// accepts undeclared trailing bytes. The exact-length check is required for message transports such
/// as `SOCK_SEQPACKET`: one received record must contain exactly one complete feed frame.
pub fn decode_frame(buf: &[u8]) -> Option<FeedFrameView<'_>> {
    if buf.len() < FEED_FRAME_HEADER_LEN {
        return None;
    }
    if is_feed_payload_conflict_sentinel(buf) {
        return None;
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let seq = u64::from_le_bytes(buf[4..12].try_into().ok()?);
    let recv_ns = u64::from_le_bytes(buf[12..20].try_into().ok()?);
    let has_to = buf[20] != 0;
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&buf[21..25]);
    let to = if has_to {
        let mut a = [0u8; 20];
        a.copy_from_slice(&buf[25..45]);
        Some(a)
    } else {
        None
    };
    let data_len = u32::from_le_bytes(buf[45..49].try_into().ok()?) as usize;

    match magic {
        FEED_FRAME_MAGIC => {
            let end = FEED_FRAME_HEADER_LEN.checked_add(data_len)?;
            if end != buf.len() {
                return None;
            }
            Some(FeedFrameView {
                seq,
                recv_ns,
                header_timestamp: 0,
                to,
                selector,
                data: &buf[FEED_FRAME_HEADER_LEN..end],
                envelope: None,
                inner_ordinal: 0,
                flags: 0,
                wire_version: 1,
            })
        }
        FEED_FRAME_MAGIC_V2 => {
            if buf.len() < FEED_FRAME_HEADER_LEN_V2 {
                return None;
            }
            let env_len = u32::from_le_bytes(buf[49..53].try_into().ok()?) as usize;
            let data_end = FEED_FRAME_HEADER_LEN_V2.checked_add(data_len)?;
            let env_end = data_end.checked_add(env_len)?;
            if env_end != buf.len() {
                return None;
            }
            let envelope = if env_len > 0 {
                Some(&buf[data_end..env_end])
            } else {
                None
            };
            Some(FeedFrameView {
                seq,
                recv_ns,
                header_timestamp: 0,
                to,
                selector,
                data: &buf[FEED_FRAME_HEADER_LEN_V2..data_end],
                envelope,
                inner_ordinal: 0,
                flags: 0,
                wire_version: 2,
            })
        }
        FEED_FRAME_MAGIC_V3 => {
            if buf.len() < FEED_FRAME_HEADER_LEN_V3 {
                return None;
            }
            let env_len = u32::from_le_bytes(buf[49..53].try_into().ok()?) as usize;
            let header_timestamp = u64::from_le_bytes(buf[53..61].try_into().ok()?);
            let data_end = FEED_FRAME_HEADER_LEN_V3.checked_add(data_len)?;
            let env_end = data_end.checked_add(env_len)?;
            if env_end != buf.len() {
                return None;
            }
            let envelope = if env_len > 0 {
                Some(&buf[data_end..env_end])
            } else {
                None
            };
            Some(FeedFrameView {
                seq,
                recv_ns,
                header_timestamp,
                to,
                selector,
                data: &buf[FEED_FRAME_HEADER_LEN_V3..data_end],
                envelope,
                inner_ordinal: 0,
                flags: 0,
                wire_version: 3,
            })
        }
        FEED_FRAME_MAGIC_V4 => {
            if buf.len() < FEED_FRAME_HEADER_LEN_V4 {
                return None;
            }
            let env_len = u32::from_le_bytes(buf[49..53].try_into().ok()?) as usize;
            let header_timestamp = u64::from_le_bytes(buf[53..61].try_into().ok()?);
            let inner_ordinal = u32::from_le_bytes(buf[61..65].try_into().ok()?);
            let flags = u32::from_le_bytes(buf[65..69].try_into().ok()?);
            let marker_bits = flags & (FEED_FRAME_FLAG_TAINT | FEED_FRAME_FLAG_IGNORABLE_ORDINAL);
            let reason_without_taint = flags & FEED_FRAME_TAINT_REASON_MASK != 0
                && flags & FEED_FRAME_FLAG_TAINT == 0;
            if flags & !FEED_FRAME_KNOWN_FLAGS != 0
                || marker_bits == (FEED_FRAME_FLAG_TAINT | FEED_FRAME_FLAG_IGNORABLE_ORDINAL)
                || reason_without_taint
            {
                return None;
            }
            let data_end = FEED_FRAME_HEADER_LEN_V4.checked_add(data_len)?;
            let env_end = data_end.checked_add(env_len)?;
            if env_end != buf.len() {
                return None;
            }
            let is_marker = marker_bits != 0;
            if is_marker
                && (has_to
                    || selector != [0; 4]
                    || buf[25..45].iter().any(|&byte| byte != 0)
                    || data_len != 0
                    || env_len != 0)
            {
                return None;
            }
            let envelope = if env_len > 0 {
                Some(&buf[data_end..env_end])
            } else {
                None
            };
            Some(FeedFrameView {
                seq,
                recv_ns,
                header_timestamp,
                to,
                selector,
                data: &buf[FEED_FRAME_HEADER_LEN_V4..data_end],
                envelope,
                inner_ordinal,
                flags,
                wire_version: 4,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_roundtrip_with_to_and_data() {
        let to = [0x11u8; 20];
        let sel = [0xde, 0xad, 0xbe, 0xef];
        let data = vec![1u8, 2, 3, 4, 5, 6, 7];
        let mut buf = Vec::new();
        let n = encode_frame(&mut buf, 42, 1_700_000_000_000, Some(to), sel, &data);
        assert_eq!(n, FEED_FRAME_HEADER_LEN + data.len());
        let v = decode_frame(&buf).expect("decode");
        assert_eq!(v.seq, 42);
        assert_eq!(v.recv_ns, 1_700_000_000_000);
        assert_eq!(v.to, Some(to));
        assert_eq!(v.selector, sel);
        assert_eq!(v.data, &data[..]);
        assert!(v.envelope.is_none(), "V1 frames have no envelope section");
    }

    #[test]
    fn v1_roundtrip_no_to_empty_data() {
        let mut buf = Vec::new();
        encode_frame(&mut buf, 0, 0, None, [0; 4], &[]);
        let v = decode_frame(&buf).expect("decode");
        assert_eq!(v.to, None);
        assert!(v.data.is_empty());
        assert_eq!(v.seq, 0);
        assert!(v.envelope.is_none());
    }

    #[test]
    fn rejects_bad_magic_short_and_overrun() {
        // too short
        assert!(decode_frame(&[0u8; 10]).is_none());
        // bad magic
        let mut buf = vec![0u8; FEED_FRAME_HEADER_LEN];
        assert!(decode_frame(&buf).is_none());
        // valid V1 magic but declared body overruns the buffer → fail closed
        buf[0..4].copy_from_slice(&FEED_FRAME_MAGIC.to_le_bytes());
        buf[45..49].copy_from_slice(&9999u32.to_le_bytes());
        assert!(decode_frame(&buf).is_none());
    }

    #[test]
    fn rejects_trailing_bytes_for_every_wire_version() {
        let selector = [0xde, 0xad, 0xbe, 0xef];
        let data = [0x11, 0x22, 0x33];
        let envelope = [0x02, 0xc0];

        let mut frames = Vec::new();
        let mut frame = Vec::new();
        encode_frame(&mut frame, 1, 2, Some([0x44; 20]), selector, &data);
        frames.push(frame.clone());
        encode_frame_v2(
            &mut frame,
            1,
            2,
            Some([0x44; 20]),
            selector,
            &data,
            Some(&envelope),
        );
        frames.push(frame.clone());
        encode_frame_v3(
            &mut frame,
            1,
            2,
            3,
            Some([0x44; 20]),
            selector,
            &data,
            Some(&envelope),
        );
        frames.push(frame);

        for mut encoded in frames {
            assert!(decode_frame(&encoded).is_some(), "fixture must start valid");
            encoded.push(0xaa);
            assert!(
                decode_frame(&encoded).is_none(),
                "a record with undeclared trailing bytes must fail closed"
            );
        }
    }

    #[test]
    fn all_versions_preserve_selector_and_data_boundary_matrix_byte_exactly() {
        // `data` is the wire protocol's post-selector calldata body. Exercise the boundary lengths
        // that catch empty/short/off-by-one handling plus a body well beyond the selector width.
        // V1 has no envelope by design; V2/V3 must preserve the same hot-path fields and the exact
        // envelope bytes in addition.
        let selector = [0x12, 0x34, 0x56, 0x78];
        let to = Some([0x9a; 20]);
        let envelope = [0x02, 0xf8, 0x01, 0x80, 0xaa, 0x55];

        for data_len in [0usize, 1, 2, 3, 4, 257] {
            let data: Vec<u8> = (0..data_len).map(|i| (i as u8).wrapping_mul(17)).collect();
            let mut v1 = Vec::new();
            let mut v2 = Vec::new();
            let mut v3 = Vec::new();
            encode_frame(&mut v1, 42, 99, to, selector, &data);
            encode_frame_v2(&mut v2, 42, 99, to, selector, &data, Some(&envelope));
            encode_frame_v3(&mut v3, 42, 99, 123, to, selector, &data, Some(&envelope));

            let d1 = decode_frame(&v1).expect("V1 boundary fixture");
            let d2 = decode_frame(&v2).expect("V2 boundary fixture");
            let d3 = decode_frame(&v3).expect("V3 boundary fixture");
            for decoded in [d1, d2, d3] {
                assert_eq!(decoded.seq, 42);
                assert_eq!(decoded.recv_ns, 99);
                assert_eq!(decoded.to, to);
                assert_eq!(decoded.selector, selector);
                assert_eq!(decoded.data, data.as_slice(), "data_len={data_len}");
            }
            assert_eq!(d1.header_timestamp, 0);
            assert_eq!(d2.header_timestamp, 0);
            assert_eq!(d3.header_timestamp, 123);
            assert_eq!(d1.envelope, None);
            assert_eq!(d2.envelope, Some(envelope.as_slice()));
            assert_eq!(d3.envelope, Some(envelope.as_slice()));
        }
    }

    #[test]
    fn v2_roundtrip_with_envelope() {
        let to = [0x22u8; 20];
        let sel = [0x12, 0xaa, 0x3c, 0xaf];
        let data = vec![0xaa, 0xbb, 0xcc, 0xdd];
        // A short signed-tx envelope (post-L2-kind-byte) — type byte + RLP list head; the codec
        // doesn't parse it, just transports it.
        let envelope = vec![0x02u8, 0xf8, 0x6b, 0x82, 0xa4, 0xb1, 0x01];
        let mut buf = Vec::new();
        let n = encode_frame_v2(
            &mut buf,
            99,
            1_700_000_999_999,
            Some(to),
            sel,
            &data,
            Some(&envelope),
        );
        assert_eq!(n, FEED_FRAME_HEADER_LEN_V2 + data.len() + envelope.len());
        let v = decode_frame(&buf).expect("decode v2");
        assert_eq!(v.seq, 99);
        assert_eq!(v.recv_ns, 1_700_000_999_999);
        assert_eq!(v.to, Some(to));
        assert_eq!(v.selector, sel);
        assert_eq!(v.data, &data[..]);
        assert_eq!(v.envelope, Some(envelope.as_slice()));
    }

    #[test]
    fn v2_with_no_envelope_is_v1_equivalent_semantically() {
        // A sidecar might emit V2 magic but choose to not attach an envelope (env_len=0). The
        // decoded view should look like a V1 frame: envelope = None, data intact.
        let data = vec![0x33u8; 20];
        let mut buf = Vec::new();
        encode_frame_v2(&mut buf, 1, 2, None, [0; 4], &data, None);
        let v = decode_frame(&buf).expect("decode");
        assert_eq!(v.data, &data[..]);
        assert!(v.envelope.is_none());
    }

    #[test]
    fn v2_rejects_short_header_and_overrun() {
        let env = vec![0xff, 0xee];
        let data = vec![0x77u8; 16];
        let mut buf = Vec::new();
        encode_frame_v2(&mut buf, 1, 2, None, [0; 4], &data, Some(&env));
        // truncate inside the envelope section → must reject
        let trunc_idx = buf.len() - 1;
        assert!(decode_frame(&buf[..trunc_idx]).is_none());
        // valid V2 magic but tamper envelope_len to overrun
        buf[49..53].copy_from_slice(&999_999u32.to_le_bytes());
        assert!(decode_frame(&buf).is_none());
    }

    #[test]
    fn v3_roundtrip_carries_header_timestamp() {
        let to = [0x33u8; 20];
        let sel = [0x07, 0xed, 0x23, 0x79];
        let data = vec![0xab, 0xcd, 0xef];
        let envelope = vec![0x02u8, 0xf8, 0x6b];
        let header_timestamp = 1_715_000_123u64; // sequencer header.timestamp (unix seconds)
        let mut buf = Vec::new();
        let n = encode_frame_v3(
            &mut buf,
            7,
            1_700_000_000_123_456_789,
            header_timestamp,
            Some(to),
            sel,
            &data,
            Some(&envelope),
        );
        assert_eq!(n, FEED_FRAME_HEADER_LEN_V3 + data.len() + envelope.len());
        let v = decode_frame(&buf).expect("decode v3");
        assert_eq!(v.seq, 7);
        assert_eq!(v.recv_ns, 1_700_000_000_123_456_789);
        assert_eq!(v.header_timestamp, header_timestamp);
        assert_eq!(v.to, Some(to));
        assert_eq!(v.selector, sel);
        assert_eq!(v.data, &data[..]);
        assert_eq!(v.envelope, Some(envelope.as_slice()));
    }

    #[test]
    fn v1_and_v2_frames_report_zero_header_timestamp() {
        let mut v1 = Vec::new();
        encode_frame(&mut v1, 1, 2, None, [0; 4], &[]);
        assert_eq!(decode_frame(&v1).unwrap().header_timestamp, 0);
        let mut v2 = Vec::new();
        encode_frame_v2(&mut v2, 1, 2, None, [0; 4], &[0xaa], None);
        assert_eq!(decode_frame(&v2).unwrap().header_timestamp, 0);
    }

    #[test]
    fn v3_rejects_short_header() {
        let mut buf = Vec::new();
        encode_frame_v3(&mut buf, 1, 2, 3, None, [0; 4], &[0x11; 8], None);
        // Truncate inside the V3 header (below 61 bytes) → fail closed.
        assert!(decode_frame(&buf[..58]).is_none());
    }

    /// FS-08 / WI-03 (RED): a V1 frame has NO envelope section, hence no canonical tx
    /// identity (no tx hash, no validator replay, no cross-leg payload comparison) — it must
    /// be TYPED non-authoritative. V2/V3 frames with an envelope preserve identity and stay
    /// authoritative. The ordinal-bearing V4 wire must round-trip `inner_ordinal` and the
    /// taint marker so the sidecar's canonical normalizer output reaches the engine.
    #[test]
    fn binary_v1_without_envelope_is_typed_non_authoritative_while_v2_v3_preserve_identity() {
        let selector = [0xde, 0xad, 0xbe, 0xef];
        let data = [0x11, 0x22, 0x33];
        let envelope = [0x02u8, 0xf8, 0x6b, 0x82, 0xa4, 0xb1];

        // V1: no envelope, ever → non-authoritative.
        let mut v1 = Vec::new();
        encode_frame(&mut v1, 9, 100, Some([0x44; 20]), selector, &data);
        let d1 = decode_frame(&v1).expect("v1 fixture");
        assert_eq!(
            d1.authority(),
            FeedFrameAuthority::NonAuthoritative,
            "a V1 frame (no envelope section) must be typed non-authoritative"
        );

        // V2/V3 with envelope: identity preserved. Authority is version-gated (codex
        // WI0304_REVERIFY 2c): pre-V4 wires carry no ordinal, so even WITH an envelope they
        // are non-authoritative — see pre_v4_wire_versions_are_never_authoritative.
        let mut v2 = Vec::new();
        encode_frame_v2(&mut v2, 9, 100, Some([0x44; 20]), selector, &data, Some(&envelope));
        let d2 = decode_frame(&v2).expect("v2 fixture");
        assert_eq!(d2.envelope, Some(envelope.as_slice()), "V2 preserves the envelope");
        assert_eq!(d2.authority(), FeedFrameAuthority::NonAuthoritative);
        let mut v3 = Vec::new();
        encode_frame_v3(&mut v3, 9, 100, 7, Some([0x44; 20]), selector, &data, Some(&envelope));
        let d3 = decode_frame(&v3).expect("v3 fixture");
        assert_eq!(d3.envelope, Some(envelope.as_slice()), "V3 preserves the envelope");
        assert_eq!(d3.authority(), FeedFrameAuthority::NonAuthoritative);

        // A V2 frame whose sidecar attached NO envelope has the same identity gap as V1.
        let mut v2_bare = Vec::new();
        encode_frame_v2(&mut v2_bare, 9, 100, Some([0x44; 20]), selector, &data, None);
        let d2_bare = decode_frame(&v2_bare).expect("bare v2 fixture");
        assert_eq!(
            d2_bare.authority(),
            FeedFrameAuthority::NonAuthoritative,
            "an envelope-less V2 frame has no replay identity either"
        );

        // V4 ordinary frame: ordinal must survive the wire; identity preserved.
        let mut v4 = Vec::new();
        encode_frame_v4(
            &mut v4,
            9,
            100,
            7,
            Some([0x44; 20]),
            selector,
            &data,
            Some(&envelope),
            3,
            None,
        );
        let d4 = decode_frame(&v4).expect("v4 fixture");
        assert_eq!(d4.seq, 9);
        assert_eq!(d4.header_timestamp, 7);
        assert_eq!(d4.data, &data[..]);
        assert_eq!(d4.envelope, Some(envelope.as_slice()), "V4 preserves the envelope");
        assert_eq!(
            d4.inner_ordinal, 3,
            "the canonical inner_ordinal must survive the wire (FS-06 binary path)"
        );
        assert!(!d4.is_taint_marker());
        assert_eq!(d4.authority(), FeedFrameAuthority::Authoritative);

        // V4 taint marker: the undecoded-state-bearing signal must reach the engine typed.
        let mut v4_taint = Vec::new();
        encode_frame_v4(
            &mut v4_taint,
            9,
            100,
            7,
            None,
            [0; 4],
            &[],
            None,
            4,
            Some(FeedFrameMarker::Taint(2)),
        );
        let dt = decode_frame(&v4_taint).expect("v4 taint fixture");
        assert!(
            dt.is_taint_marker(),
            "a sidecar taint leaf must arrive as a TYPED taint marker frame"
        );
        assert_eq!(dt.taint_reason_code(), Some(2));
        assert_eq!(dt.inner_ordinal, 4, "taint leaves consume canonical ordinals too");
        assert_eq!(
            dt.authority(),
            FeedFrameAuthority::NonAuthoritative,
            "a taint marker never authorizes state"
        );
    }

    #[test]
    fn v4_ignorable_ordinal_marker_roundtrips_distinct_from_taint() {
        let mut frame = Vec::new();
        let encoded_len = encode_frame_v4(
            &mut frame,
            9,
            100,
            7,
            None,
            [0; 4],
            &[],
            None,
            4,
            Some(FeedFrameMarker::IgnorableOrdinal),
        );

        assert_eq!(encoded_len, FEED_FRAME_HEADER_LEN_V4);
        assert_eq!(
            u32::from_le_bytes(frame[65..69].try_into().expect("V4 flags word")),
            FEED_FRAME_FLAG_IGNORABLE_ORDINAL
        );
        let decoded = decode_frame(&frame).expect("ignorable marker decodes");
        assert_eq!(decoded.seq, 9);
        assert_eq!(decoded.inner_ordinal, 4);
        assert!(decoded.is_ignorable_ordinal_marker());
        assert!(!decoded.is_taint_marker());
        assert_eq!(decoded.taint_reason_code(), None);
        assert_eq!(decoded.authority(), FeedFrameAuthority::NonAuthoritative);
        assert!(decoded.data.is_empty());
        assert_eq!(decoded.envelope, None);
    }

    #[test]
    fn v4_rejects_invalid_marker_flag_and_payload_shapes() {
        let mut marker = Vec::new();
        encode_frame_v4(
            &mut marker,
            9,
            100,
            7,
            None,
            [0; 4],
            &[],
            None,
            4,
            Some(FeedFrameMarker::IgnorableOrdinal),
        );
        let canonical_marker = marker.clone();

        marker[65..69].copy_from_slice(
            &(FEED_FRAME_FLAG_TAINT | FEED_FRAME_FLAG_IGNORABLE_ORDINAL).to_le_bytes(),
        );
        assert!(decode_frame(&marker).is_none(), "contradictory marker bits reject");

        marker.copy_from_slice(&canonical_marker);
        marker[65..69].copy_from_slice(&(1u32 << 8).to_le_bytes());
        assert!(
            decode_frame(&marker).is_none(),
            "reason bits without taint reject"
        );

        marker.copy_from_slice(&canonical_marker);
        marker[65..69].copy_from_slice(&(1u32 << 16).to_le_bytes());
        assert!(decode_frame(&marker).is_none(), "reserved flags reject");

        marker.copy_from_slice(&canonical_marker);
        marker[25] = 1;
        assert!(
            decode_frame(&marker).is_none(),
            "marker with hidden destination bytes rejects"
        );

        let mut payload = Vec::new();
        encode_frame_v4(
            &mut payload,
            9,
            100,
            7,
            Some([0x44; 20]),
            [1, 2, 3, 4],
            &[5, 6, 7],
            Some(&[0x02, 0xc0]),
            4,
            None,
        );
        payload[65..69].copy_from_slice(&FEED_FRAME_FLAG_IGNORABLE_ORDINAL.to_le_bytes());
        assert!(
            decode_frame(&payload).is_none(),
            "payload-bearing ignorable marker rejects"
        );
    }

    /// codex WI0304_REVERIFY item 2c (RED): pre-V4 wire versions carry NO canonical
    /// `inner_ordinal`, so a receiver cannot distinguish a safe single-child frame from
    /// collapsed multi-child same-sequence siblings — V1, V2 and V3 must NEVER be
    /// authoritative, envelope or not. Envelope IDENTITY is still preserved (the bytes
    /// round-trip); what pre-V4 frames lose is AUTHORITY. Only the ordinal-bearing V4 wire
    /// (with an envelope, not a taint marker) may back authoritative state.
    #[test]
    fn pre_v4_wire_versions_are_never_authoritative() {
        let selector = [0xde, 0xad, 0xbe, 0xef];
        let data = [0x11, 0x22, 0x33];
        let envelope = [0x02u8, 0xf8, 0x6b, 0x82, 0xa4, 0xb1];

        let mut v1 = Vec::new();
        encode_frame(&mut v1, 5, 9, Some([0x44; 20]), selector, &data);
        assert_eq!(
            decode_frame(&v1).expect("v1").authority(),
            FeedFrameAuthority::NonAuthoritative
        );

        let mut v2 = Vec::new();
        encode_frame_v2(&mut v2, 5, 9, Some([0x44; 20]), selector, &data, Some(&envelope));
        let d2 = decode_frame(&v2).expect("v2");
        assert_eq!(d2.envelope, Some(envelope.as_slice()), "identity preserved");
        assert_eq!(
            d2.authority(),
            FeedFrameAuthority::NonAuthoritative,
            "a V2 frame carries no ordinal — even WITH an envelope it cannot prove its \
             leaf position and must never be authoritative"
        );

        let mut v3 = Vec::new();
        encode_frame_v3(&mut v3, 5, 9, 7, Some([0x44; 20]), selector, &data, Some(&envelope));
        let d3 = decode_frame(&v3).expect("v3");
        assert_eq!(d3.envelope, Some(envelope.as_slice()), "identity preserved");
        assert_eq!(
            d3.authority(),
            FeedFrameAuthority::NonAuthoritative,
            "a V3 frame carries no ordinal — even WITH an envelope it must never be \
             authoritative"
        );

        let mut v4 = Vec::new();
        encode_frame_v4(
            &mut v4,
            5,
            9,
            7,
            Some([0x44; 20]),
            selector,
            &data,
            Some(&envelope),
            2,
            None,
        );
        assert_eq!(
            decode_frame(&v4).expect("v4").authority(),
            FeedFrameAuthority::Authoritative,
            "the ordinal-bearing V4 wire with an envelope is the ONLY authoritative shape"
        );

        let mut v4_bare = Vec::new();
        encode_frame_v4(&mut v4_bare, 5, 9, 7, Some([0x44; 20]), selector, &data, None, 2, None);
        assert_eq!(
            decode_frame(&v4_bare).expect("bare v4").authority(),
            FeedFrameAuthority::NonAuthoritative,
            "no envelope = no replay identity, V4 or not (FS-08)"
        );
    }

    #[test]
    fn old_decoder_still_handles_v1_frames() {
        // Forward-compat sanity: an engine running the V2 decoder reading a frame produced by
        // a V1-only sidecar must still work (envelope = None, data + header intact).
        let mut v1 = Vec::new();
        encode_frame(
            &mut v1,
            5,
            6,
            Some([0xab; 20]),
            [0x07, 0xed, 0x23, 0x79],
            &[0x42; 4],
        );
        let v = decode_frame(&v1).expect("v2 decoder reads v1");
        assert_eq!(v.seq, 5);
        assert_eq!(v.selector, [0x07, 0xed, 0x23, 0x79]);
        assert_eq!(v.data, &[0x42; 4]);
        assert!(v.envelope.is_none());
    }
}
