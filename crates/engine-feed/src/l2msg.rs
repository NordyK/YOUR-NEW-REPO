//! L2 message-kind dispatch.

use thiserror::Error;

use crate::address::Address;
use crate::rlp::{
    decode_signed_tx_1559, decode_signed_tx_2930, decode_signed_tx_7702, decode_signed_tx_legacy,
};

/// Maximum batch nesting depth, matching Arbitrum Nitro's `arbos/parse_l2.go`
/// (`MaxL2MessageSize` / nested-batch guard caps batch recursion at 16). A frame nested deeper
/// is treated as malformed and its contents are dropped (fail-closed) rather than walked — an
/// adversarial feed can't drive unbounded stack growth / work through nested Batch wrappers.
const MAX_BATCH_DEPTH: u32 = 16;

/// L2 message kind, matching Nitro `arbos/parse_l2.go`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L2MsgKind {
    /// Unsigned user tx.
    UnsignedUserTx = 0,
    /// Contract creation tx.
    ContractTx = 1,
    /// Non-mutating call (eth_call style).
    NonMutatingCall = 2,
    /// Batch of signed txs.
    Batch = 3,
    /// Single signed tx.
    SignedTx = 4,
    /// Reserved/unnamed: Nitro leaves kind 5 with no symbolic name in `arbos/parse_l2.go`
    /// (a gap between `SignedTx` = 4 and `Heartbeat` = 6). This variant is a no-op placeholder
    /// kept only so the discriminant range stays contiguous; it carries no decode behavior.
    Reserved5 = 5,
    /// Heartbeat (deprecated).
    Heartbeat = 6,
    /// Signed compressed tx.
    SignedCompressedTx = 7,
}

impl L2MsgKind {
    /// Recognise a byte as a known kind, returning `None` for unknown values.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::UnsignedUserTx),
            1 => Some(Self::ContractTx),
            2 => Some(Self::NonMutatingCall),
            3 => Some(Self::Batch),
            4 => Some(Self::SignedTx),
            5 => Some(Self::Reserved5),
            6 => Some(Self::Heartbeat),
            7 => Some(Self::SignedCompressedTx),
            _ => None,
        }
    }
}

/// L2 message decode errors.
#[derive(Debug, Clone, Copy, Error)]
pub enum L2MsgError {
    /// Empty payload.
    #[error("empty l2 payload")]
    Empty,
    /// Unknown L2 message kind byte.
    #[error("unknown l2 message kind")]
    UnknownKind,
    /// RLP decode error.
    #[error("rlp decode failed")]
    Rlp,
    /// Batch header malformed (length prefix incomplete).
    #[error("malformed batch header")]
    BatchMalformed,
}

/// A decoded transaction: `to`, `data`, `value` plus the kind byte the engine should
/// classify. `from` is recovered separately because secp256k1 recovery is expensive and
/// only worth doing for transactions the engine has classified as interesting (e.g. routes
/// passing through known router/pool addresses).
#[derive(Debug, Clone, Copy)]
pub struct TransactionInfo<'a> {
    /// EIP-1559 tx type byte (0x02), legacy (0xLE), 2930 (0x01).
    pub tx_type: u8,
    /// Destination address.
    pub to: Address,
    /// Calldata.
    pub data: &'a [u8],
    /// Value transferred (wei).
    pub value: alloy_primitives::U256,
    /// Nonce.
    pub nonce: u64,
    /// Chain id (recovered from the tx envelope; `0` if legacy non-EIP-155).
    pub chain_id: u64,
    /// Raw RLP body of the EIP-2930/1559 `access_list` field (entries concatenated, outer list
    /// header stripped), or `&[]` for legacy txs / no access list. Iterate the named addresses
    /// with [`crate::rlp::access_list_addresses`] — used by the wakeup-widening path to mark a
    /// touched pool dirty without decoding the selector (EIP-2930 §"touched pools").
    pub access_list: &'a [u8],
    /// Whether the RLP `to` field was present (non-empty). `false` means CONTRACT CREATION:
    /// the legacy decode maps an empty `to` to the zero address (see `rlp_address`), which made
    /// creations indistinguishable from a genuine `to == 0x0` call and therefore invisible as
    /// state-bearing activity (FL-05 ingest half). The canonical normalizer taints creations;
    /// this flag is how it can tell.
    pub to_present: bool,
}

/// Decode a single l2 message frame after base64 decoding. The first byte is the kind;
/// callers must pass the full decoded buffer.
///
/// For `SignedTx` (kind 4) the rest of the buffer is the canonical EIP-1559 / legacy /
/// EIP-2930 envelope: type byte `0x02` / `0x01` / RLP list head.
pub fn decode_l2msg(buf: &[u8]) -> Result<(L2MsgKind, Option<TransactionInfo<'_>>), L2MsgError> {
    let kind_byte = *buf.first().ok_or(L2MsgError::Empty)?;
    let kind = L2MsgKind::from_byte(kind_byte).ok_or(L2MsgError::UnknownKind)?;
    let rest = &buf[1..];
    match kind {
        L2MsgKind::SignedTx => {
            let tx = decode_signed_tx_envelope(rest).map_err(|_| L2MsgError::Rlp)?;
            Ok((kind, Some(tx)))
        }
        _ => Ok((kind, None)),
    }
}

/// One canonical leaf of the depth-first, left-to-right walk — the shared vocabulary the
/// three public walkers ([`decode_l2msg_recursive`], [`decode_l2msg_recursive_raw`],
/// [`normalize_l2msg`]) adapt to their callers. Internal so classification policy lives in
/// exactly one place.
enum CanonicalLeaf<'a> {
    /// Zero-length payload.
    Empty,
    /// Kind byte outside the official Nitro range.
    UnknownKind(u8),
    /// Kind 5 — officially reserved, no semantics.
    ReservedKind(u8),
    /// Kinds 2/6/7 — documented non-state-bearing (Nitro-unimplemented / deprecated).
    Ignorable(L2MsgKind),
    /// Kinds 0/1 — real state-bearing kinds this engine does not decode.
    StateBearingUndecoded(u8),
    /// Kind 4 — the RAW signed-tx envelope (post-kind-byte), NOT yet decoded.
    SignedTx(&'a [u8]),
    /// A Batch nested past [`MAX_BATCH_DEPTH`]; its whole subtree is one undecoded leaf.
    OverDeepBatch,
    /// Batch length-prefix framing violated; the remainder of that batch is unwalkable.
    MalformedFraming,
}

/// Canonical forward recursive descent over one L2 message, matching official Nitro
/// `arbos/parse_l2.go`: a Batch reads its `<u64 BE length><bytes>` children sequentially and
/// each child is walked DEPTH-FIRST in INPUT order (Nitro appends nested segments FIFO — no
/// stack, no reversal). Recursion depth is bounded by [`MAX_BATCH_DEPTH`] (16) + 1, so the
/// walk is allocation-free (the old iterative version allocated a `Vec` stack AND popped LIFO,
/// reversing every sibling pair — FL-03). Every anomaly is emitted as a typed leaf; the
/// legacy walkers ignore the ones they never surfaced, [`normalize_l2msg`] types all of them.
fn walk_l2msg_canonical<'a>(payload: &'a [u8], depth: u32, emit: &mut dyn FnMut(CanonicalLeaf<'a>)) {
    let Some(&kind_byte) = payload.first() else {
        emit(CanonicalLeaf::Empty);
        return;
    };
    let Some(kind) = L2MsgKind::from_byte(kind_byte) else {
        emit(CanonicalLeaf::UnknownKind(kind_byte));
        return;
    };
    let rest = &payload[1..];
    match kind {
        L2MsgKind::SignedTx => emit(CanonicalLeaf::SignedTx(rest)),
        L2MsgKind::Batch => {
            // Refuse to descend past the cap: the whole subtree becomes one undecoded leaf
            // (fail-closed; a nested-Batch bomb can't drive unbounded stack growth or work).
            if depth >= MAX_BATCH_DEPTH {
                emit(CanonicalLeaf::OverDeepBatch);
                return;
            }
            let mut cursor = 0usize;
            while cursor < rest.len() {
                if cursor + 8 > rest.len() {
                    emit(CanonicalLeaf::MalformedFraming);
                    return;
                }
                let mut len_bytes = [0u8; 8];
                len_bytes.copy_from_slice(&rest[cursor..cursor + 8]);
                let len = u64::from_be_bytes(len_bytes) as usize;
                cursor += 8;
                let Some(end) = cursor.checked_add(len) else {
                    emit(CanonicalLeaf::MalformedFraming);
                    return;
                };
                if end > rest.len() {
                    emit(CanonicalLeaf::MalformedFraming);
                    return;
                }
                walk_l2msg_canonical(&rest[cursor..end], depth + 1, emit);
                cursor = end;
            }
        }
        L2MsgKind::UnsignedUserTx => emit(CanonicalLeaf::StateBearingUndecoded(0)),
        L2MsgKind::ContractTx => emit(CanonicalLeaf::StateBearingUndecoded(1)),
        L2MsgKind::NonMutatingCall | L2MsgKind::Heartbeat | L2MsgKind::SignedCompressedTx => {
            emit(CanonicalLeaf::Ignorable(kind))
        }
        L2MsgKind::Reserved5 => emit(CanonicalLeaf::ReservedKind(kind_byte)),
    }
}

/// Recursively decode an L2 message into the flat list of every transaction it carries.
///
/// `kind=SignedTx` yields one entry. `kind=Batch` recurses into inner messages (each is a
/// `<u64 BE length><bytes>` envelope wrapping another L2 message — typically another
/// `SignedTx`) in canonical DEPTH-FIRST INPUT order (official Nitro FIFO — FL-03 fix; the old
/// LIFO stack reversed siblings). Malformed inner entries are skipped here so a single bad
/// child doesn't drop the whole batch — use [`normalize_l2msg`] to observe them typed.
pub fn decode_l2msg_recursive<F>(buf: &[u8], mut callback: F)
where
    F: FnMut(TransactionInfo<'_>),
{
    walk_l2msg_canonical(buf, 0, &mut |leaf| {
        if let CanonicalLeaf::SignedTx(envelope) = leaf {
            if let Ok(tx) = decode_signed_tx_envelope(envelope) {
                callback(tx);
            }
        }
    });
}

/// Like [`decode_l2msg_recursive`] but yields the RAW signed-tx envelope slice — the bytes
/// after the L2 kind byte, i.e. exactly the input [`crate::recover_tx_sender`] and
/// [`decode_signed_tx`] expect — for every SignedTx, including those nested in Batch
/// envelopes, in canonical depth-first input order (FL-03 fix). Used by the off-hot-path
/// shadow-REVM worker, which needs the raw bytes to recover the sender (the hot path never
/// recovers senders).
pub fn decode_l2msg_recursive_raw<F>(buf: &[u8], mut callback: F)
where
    F: FnMut(&[u8]),
{
    walk_l2msg_canonical(buf, 0, &mut |leaf| {
        if let CanonicalLeaf::SignedTx(envelope) = leaf {
            callback(envelope);
        }
    });
}

/// Decode a signed-tx envelope (the bytes after the L2 kind byte) into a
/// [`TransactionInfo`]. Returns `None` on malformed input.
pub fn decode_signed_tx(envelope: &[u8]) -> Option<TransactionInfo<'_>> {
    decode_signed_tx_envelope(envelope).ok()
}

// ───────────────────────── canonical feed normalizer (WI-03) ─────────────────────────

/// Canonical leaf position of one inner L2 message item inside its feed message: the
/// sequencer message number plus the depth-first, left-to-right leaf index. This is the
/// Phase-1 subset of the §3.1 `FeedCursor` (redundancy group / payload hash / envelope
/// identity land with the full `StatePoint` contract in Phase 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedLeafCursor {
    /// Sequencer feed message number (`sequenceNumber`).
    pub sequence: u64,
    /// Depth-first, left-to-right leaf index within this message. EVERY emitted leaf —
    /// decoded, unsupported, malformed, or tainted — consumes exactly one ordinal, so the
    /// ordinal stream is stable regardless of decode success.
    pub inner_ordinal: u32,
}

/// Structural decode failures: input that could not even be classified as a known L2
/// message item. Fail-closed: all of these move the taint watermark because the parser
/// cannot prove the bytes carried no state-bearing transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedReason {
    /// Zero-length L2 message payload.
    EmptyPayload,
    /// Kind byte outside the official Nitro `parse_l2.go` range.
    UnknownKind(u8),
    /// Kind 5 (reserved gap) / kind 8 (reserved BLS): officially reserved, no semantics.
    ReservedKind(u8),
}

/// A state-bearing item the engine could not decode into an authoritative transaction —
/// the typed replacement for the old silent `continue` drops (FL-04, recursive path only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaintReason {
    /// Official Nitro kinds 0 (`UnsignedUserTx`) / 1 (`ContractTx`): real state-bearing
    /// message kinds this engine does not decode (FS-01).
    StateBearingUndecoded(u8),
    /// A kind-4 `SignedTx` whose envelope failed RLP/type decode.
    SignedTxDecodeFailed,
    /// A decoded signed tx with an EMPTY `to` field — contract creation (FL-05 ingest half).
    ContractCreation,
    /// A batch nested deeper than [`MAX_BATCH_DEPTH`]; its entire subtree is undecoded.
    OverDepthBatch,
    /// Batch length-prefix framing violated (truncated prefix or child overrunning the
    /// parent) — the remainder of that batch cannot be walked.
    MalformedBatchFraming,
}

impl MalformedReason {
    /// Stable wire/watermark code for this reason (shared vocabulary with [`TaintReason`]).
    pub const fn code(self) -> u8 {
        match self {
            Self::EmptyPayload => 6,
            Self::UnknownKind(_) => 7,
            Self::ReservedKind(_) => 8,
        }
    }
}

impl TaintReason {
    /// Stable wire/watermark code for this reason.
    pub const fn code(self) -> u8 {
        match self {
            Self::StateBearingUndecoded(_) => 1,
            Self::SignedTxDecodeFailed => 2,
            Self::ContractCreation => 3,
            Self::OverDepthBatch => 4,
            Self::MalformedBatchFraming => 5,
        }
    }
}

/// One canonically ordered leaf of an L2 feed message — the typed replacement for the old
/// `Option<ParsedTx>`-shaped recursion in which every non-decodable item silently vanished.
/// Exhaustive matching at the consumer prevents a state-bearing parser result from becoming
/// `None` (WI-03 wiring proof).
#[derive(Debug, Clone, Copy)]
pub enum NormalizedItem<'a> {
    /// A fully decoded signed transaction (kind 4) with a destination.
    Decoded {
        /// Canonical leaf cursor.
        cursor: FeedLeafCursor,
        /// Decoded transaction head.
        info: TransactionInfo<'a>,
        /// The raw signed-tx envelope (post-kind-byte) — replay/shadow identity.
        envelope: &'a [u8],
    },
    /// Officially non-state-bearing kinds the engine deliberately ignores, with the
    /// documentation stating WHY each is ignorable: kind 2 `NonmutatingCall` and kind 7
    /// `SignedCompressedTx` are UNIMPLEMENTED in official Nitro (`parse_l2.go` returns an
    /// error for them, so they can never carry executed state); kind 6 `Heartbeat` is
    /// deprecated and carries no transaction. These do NOT move the taint watermark.
    Unsupported {
        /// Canonical leaf cursor.
        cursor: FeedLeafCursor,
        /// The ignorable kind (2, 6 or 7).
        kind: L2MsgKind,
    },
    /// Structurally undecodable input (fail-closed: moves the taint watermark).
    Malformed {
        /// Canonical leaf cursor.
        cursor: FeedLeafCursor,
        /// Why the bytes could not be classified.
        reason: MalformedReason,
    },
    /// A known state-bearing item that could not be decoded into an authoritative
    /// transaction (fail-closed: moves the taint watermark).
    Tainted {
        /// Canonical leaf cursor.
        cursor: FeedLeafCursor,
        /// Why the item taints state authority.
        reason: TaintReason,
    },
}

impl NormalizedItem<'_> {
    /// The canonical leaf cursor of this item, regardless of variant. Every leaf kind carries one
    /// (the ordinal stream is dense — see [`FeedLeafCursor`]), so a consumer can track per-sequence
    /// contiguity uniformly without matching each arm (WI-06 D2(3) prospective contiguity).
    pub fn cursor(&self) -> FeedLeafCursor {
        match self {
            NormalizedItem::Decoded { cursor, .. }
            | NormalizedItem::Unsupported { cursor, .. }
            | NormalizedItem::Malformed { cursor, .. }
            | NormalizedItem::Tainted { cursor, .. } => *cursor,
        }
    }
}

use core::sync::atomic::{AtomicU64, Ordering};

/// Process-lifetime count of state-bearing-undecoded / malformed feed leaves. Monotonic; no
/// clear API on purpose — only an authoritative canonical reconcile (Phase 2) may supersede
/// the taint, exactly like the pipeline's feed-authority latch.
static FEED_TAINT_COUNT: AtomicU64 = AtomicU64::new(0);
/// Sequence of the most recent tainted leaf (diagnostic; the COUNT is the watermark).
static FEED_TAINT_LAST_SEQ: AtomicU64 = AtomicU64::new(0);
/// Packed `(inner_ordinal << 8) | reason_code` of the most recent tainted leaf.
static FEED_TAINT_LAST_ORDINAL_REASON: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the feed taint watermark. `count` strictly advances on every undecoded
/// state-bearing item; downstream authority (Phase 2) invalidates against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedTaintWatermark {
    /// Total tainted/malformed state-bearing leaves observed by this process.
    pub count: u64,
    /// Sequence of the most recent tainted leaf.
    pub last_sequence: u64,
    /// Inner ordinal of the most recent tainted leaf.
    pub last_inner_ordinal: u32,
    /// Stable reason code of the most recent tainted leaf.
    pub last_reason_code: u8,
}

/// Stable reason code for a payload (base64 `l2Msg`) that could not be decoded into L2
/// message bytes AT ALL — emitted by the pre-normalizer layers (engine JSON parser, sidecar
/// pumps, dual-relay payload hasher), which sit BEFORE [`normalize_l2msg`] can type the
/// content. Shares the watermark / V4-marker code space with [`TaintReason::code`] (1–5) and
/// [`MalformedReason::code`] (6–8).
pub const TAINT_CODE_UNDECODABLE_PAYLOAD: u8 = 9;

/// Stable reason code for a NON-AUTHORITATIVE binary frame quarantined by the receiver gate:
/// pre-V4 wire (no canonical ordinal — leaf position unprovable) or a frame without the
/// canonical envelope (no replay identity). Emitted by the engine's binary RX admission
/// point; the frame publishes no state (codex WI0304_REVERIFY item 2c).
pub const TAINT_CODE_NONAUTHORITATIVE_FRAME: u8 = 10;

/// Read the current feed taint watermark.
pub fn feed_taint_watermark() -> FeedTaintWatermark {
    let packed = FEED_TAINT_LAST_ORDINAL_REASON.load(Ordering::Acquire);
    FeedTaintWatermark {
        count: FEED_TAINT_COUNT.load(Ordering::Acquire),
        last_sequence: FEED_TAINT_LAST_SEQ.load(Ordering::Acquire),
        last_inner_ordinal: (packed >> 8) as u32,
        last_reason_code: (packed & 0xff) as u8,
    }
}

/// Advance the feed taint watermark for one undecoded state-bearing leaf. Returns the new
/// count. Called by [`normalize_l2msg`] itself (fail-closed by construction) and by binary-RX
/// consumers that receive a sidecar-emitted taint marker frame.
pub fn record_feed_taint(sequence: u64, inner_ordinal: u32, reason_code: u8) -> u64 {
    FEED_TAINT_LAST_SEQ.store(sequence, Ordering::Release);
    FEED_TAINT_LAST_ORDINAL_REASON.store(
        ((inner_ordinal as u64) << 8) | reason_code as u64,
        Ordering::Release,
    );
    FEED_TAINT_COUNT.fetch_add(1, Ordering::AcqRel) + 1
}

/// Canonically normalize one L2 feed message (`buf` = kind byte + payload, i.e. the base64-
/// decoded `l2Msg`) into its typed, ordered leaves, matching official Nitro `parse_l2.go`
/// semantics: batches walk children depth-first in INPUT order and every leaf — decodable or
/// not — is emitted as a typed [`NormalizedItem`] with its canonical [`FeedLeafCursor`].
/// Tainted/Malformed leaves also advance the process taint watermark
/// ([`feed_taint_watermark`]) so a decode failure can invalidate downstream authority.
pub fn normalize_l2msg<F>(sequence: u64, buf: &[u8], mut callback: F)
where
    F: FnMut(NormalizedItem<'_>),
{
    let mut next_ordinal: u32 = 0;
    walk_l2msg_canonical(buf, 0, &mut |leaf| {
        let cursor = FeedLeafCursor {
            sequence,
            inner_ordinal: next_ordinal,
        };
        next_ordinal += 1;
        let item = match leaf {
            CanonicalLeaf::SignedTx(envelope) => match decode_signed_tx_envelope(envelope) {
                Ok(info) if info.to_present => NormalizedItem::Decoded {
                    cursor,
                    info,
                    envelope,
                },
                // Empty RLP `to` = contract creation: a real state-bearing event with no
                // modeled semantics — typed taint, never a Decoded item with `to == 0x0`
                // (FL-05 ingest half).
                Ok(_) => NormalizedItem::Tainted {
                    cursor,
                    reason: TaintReason::ContractCreation,
                },
                Err(_) => NormalizedItem::Tainted {
                    cursor,
                    reason: TaintReason::SignedTxDecodeFailed,
                },
            },
            CanonicalLeaf::StateBearingUndecoded(kind_byte) => NormalizedItem::Tainted {
                cursor,
                reason: TaintReason::StateBearingUndecoded(kind_byte),
            },
            CanonicalLeaf::OverDeepBatch => NormalizedItem::Tainted {
                cursor,
                reason: TaintReason::OverDepthBatch,
            },
            CanonicalLeaf::MalformedFraming => NormalizedItem::Tainted {
                cursor,
                reason: TaintReason::MalformedBatchFraming,
            },
            CanonicalLeaf::Ignorable(kind) => NormalizedItem::Unsupported { cursor, kind },
            CanonicalLeaf::Empty => NormalizedItem::Malformed {
                cursor,
                reason: MalformedReason::EmptyPayload,
            },
            CanonicalLeaf::UnknownKind(kind_byte) => NormalizedItem::Malformed {
                cursor,
                reason: MalformedReason::UnknownKind(kind_byte),
            },
            CanonicalLeaf::ReservedKind(kind_byte) => NormalizedItem::Malformed {
                cursor,
                reason: MalformedReason::ReservedKind(kind_byte),
            },
        };
        // Fail-closed by construction: the normalizer itself advances the watermark for every
        // undecoded state-bearing / structurally undecodable leaf, so no consumer can forget.
        // `Unsupported` (kinds 2/6/7) is the documented Ignorable set and does not taint.
        match &item {
            NormalizedItem::Tainted { cursor, reason } => {
                record_feed_taint(cursor.sequence, cursor.inner_ordinal, reason.code());
            }
            NormalizedItem::Malformed { cursor, reason } => {
                record_feed_taint(cursor.sequence, cursor.inner_ordinal, reason.code());
            }
            NormalizedItem::Decoded { .. } | NormalizedItem::Unsupported { .. } => {}
        }
        callback(item);
    });
}

/// Decode a `Batch` (kind 3) frame by walking length-prefixed inner messages.
///
/// Each inner message is `<u64 BE length><bytes>`. The closure `f` is invoked with each
/// inner message; iteration stops on the first error.
pub fn decode_l2msg_batch<F>(buf: &[u8], mut f: F) -> Result<(), L2MsgError>
where
    F: FnMut(&[u8]) -> Result<(), L2MsgError>,
{
    let mut cursor = 0;
    while cursor < buf.len() {
        if cursor + 8 > buf.len() {
            return Err(L2MsgError::BatchMalformed);
        }
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&buf[cursor..cursor + 8]);
        let len = u64::from_be_bytes(len_bytes) as usize;
        cursor += 8;
        if cursor + len > buf.len() {
            return Err(L2MsgError::BatchMalformed);
        }
        let inner = &buf[cursor..cursor + len];
        f(inner)?;
        cursor += len;
    }
    Ok(())
}

fn decode_signed_tx_envelope(buf: &[u8]) -> Result<TransactionInfo<'_>, crate::rlp::RlpError> {
    let first = *buf.first().ok_or(crate::rlp::RlpError::Eof)?;
    match first {
        // EIP-1559: type=0x02, payload is RLP list.
        0x02 => decode_signed_tx_1559(&buf[1..]),
        // EIP-2930: type=0x01. Different field layout from 1559 — must use a dedicated
        // decoder; the 1559 helper would silently shift every field after `nonce` and
        // make 2930 traffic invisible to the (to, selector) classifier.
        0x01 => decode_signed_tx_2930(&buf[1..]),
        // EIP-7702 set-code tx: type=0x04. Prefix fields (incl. to/value/data) match 1559;
        // a dedicated decoder skips the trailing authorization_list cleanly. Real Arbitrum
        // user txs (EOA-delegated router/aggregator swaps) ride this type — dropping them
        // makes their pool moves invisible. See decode_signed_tx_7702.
        0x04 => decode_signed_tx_7702(&buf[1..]),
        // Legacy: list head 0xc0..0xff.
        v if v >= 0xc0 => decode_signed_tx_legacy(buf),
        _ => Err(crate::rlp::RlpError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_byte() {
        assert_eq!(L2MsgKind::from_byte(0), Some(L2MsgKind::UnsignedUserTx));
        assert_eq!(L2MsgKind::from_byte(3), Some(L2MsgKind::Batch));
        assert_eq!(L2MsgKind::from_byte(4), Some(L2MsgKind::SignedTx));
        assert_eq!(L2MsgKind::from_byte(99), None);
    }

    #[test]
    fn decodes_real_eip7702_setcode_tx_from_corpus() {
        // Real frame from anvil/corpus/win_463159679_20.jsonl (seq 34): L2 kind 0x04 (SignedTx)
        // wrapping an EIP-7702 (type 0x04) set-code tx on Arbitrum One. Before the 0x04 arm
        // this frame was dropped ("l2 message decode failed") despite being a real user tx.
        let full = "BAT5BLWCpLGCQTOEAbd0AIQBt3QAgxa6C5R2UlYkwh74MseyRG8meS7nuor59oC5A+SSZbudAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAKAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABQMqiu46Pe8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABqB1qsAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAuAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABIAAAAAAAAAAAAAAAAK+I0GXnfIzCI5Mnxe2zpDImjlgxAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABECV6nswAAAAAAAAAAAAAAAEWC9naYhD37ap8ZXA3e4FsKjJc/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEVR0YAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAARYL2dpiEPftqnxlcDd7gWwqMlz8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAGS0YeKZAAAAAAAAAAAAAAAAr4jQZed8jMIjkyfF7bOkMiaOWDEAAAAAAAAAAAAAAAC+APPbeGiNlwS8tOCoJ66jqcwNYgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABFUdGAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABBFvm8paozpZzPVN8tC9fl1ueDX+WMC9MfWXm+nBmyQq53MbC+F9+tnW5I6DyneZH0U0D/a3lPpeO4V9JKi4pfYxwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEEDMp+iY6N1P2thNpvUCztqmt00KwWE+TupGBeIYcwWCCRuhmZc9gXuZL675YOSEYD+dOx7lj7iAtXLUu4XF+rPGwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADA+F74XIKksZRJCqx3yWCwVpyORGrH4SSQvUTKHYABoNu1xRcdQo3+zYfOTwtY+fELn1qmAH7jppkdIR/x9dIAoD5eG9vNk6Fuc8ST9eT21SpZUWtrkHCOcmQCDUx933WWgKALCwQoe323Bl9GCtcZlUzSa6gBUzZJh2druuZMATAdFKAPmgqAfV3OOlxF5JyXOlXpDelfp1klCA4bpTTId3wGjA==";
        let raw = base64_decode(full);
        let (kind, txo) = decode_l2msg(&raw).expect("decode 7702 frame");
        assert_eq!(kind, L2MsgKind::SignedTx);
        let tx = txo.expect("7702 tx must decode (not be dropped)");
        assert_eq!(tx.tx_type, 0x04, "tx_type must be EIP-7702 set-code");
        assert_eq!(tx.chain_id, 42161, "Arbitrum One chain id");
        let mut expected_to = [0u8; 20];
        expected_to.copy_from_slice(&hex_to_vec("76525624c21ef832c7b2446f26792ee7ba8af9f6"));
        assert_eq!(tx.to.as_slice(), &expected_to, "to address");
        assert!(tx.data.len() >= 4, "calldata present");
        assert_eq!(
            &tx.data[..4],
            &[0x92, 0x65, 0xbb, 0x9d],
            "selector 0x9265bb9d"
        );
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut rev = [255u8; 256];
        for (i, &c) in T.iter().enumerate() {
            rev[c as usize] = i as u8;
        }
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &c in s.as_bytes() {
            if c == b'=' {
                break;
            }
            let v = rev[c as usize];
            if v == 255 {
                continue;
            }
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        out
    }

    fn hex_to_vec(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Wrap an L2 message `inner` in one Batch (kind 3) level: `0x03 ++ u64_be(len) ++ inner`.
    fn wrap_batch(inner: &[u8]) -> Vec<u8> {
        let mut out = vec![3u8]; // L2MsgKind::Batch
        out.extend_from_slice(&(inner.len() as u64).to_be_bytes());
        out.extend_from_slice(inner);
        out
    }

    /// A SignedTx (kind 4) frame whose envelope is arbitrary marker bytes — the raw walker yields
    /// the envelope slice without RLP-decoding it, which is all the depth-bound test needs.
    fn signed_tx_frame(marker: &[u8]) -> Vec<u8> {
        let mut out = vec![4u8]; // L2MsgKind::SignedTx
        out.extend_from_slice(marker);
        out
    }

    /// Arbitrum Nitro caps batch nesting at 16. A batch nested deeper than that must be bounded
    /// (the over-deep SignedTx is NOT yielded) and must never panic / recurse unboundedly. A batch
    /// at or under the cap still resolves its inner tx.
    #[test]
    fn batch_depth_is_capped_at_16() {
        let marker = b"\xde\xad\xbe\xef";

        // Nest a SignedTx under 20 batch levels (> 16): the inner tx is beyond the cap.
        let mut deep = signed_tx_frame(marker);
        for _ in 0..20 {
            deep = wrap_batch(&deep);
        }
        let mut deep_hits = 0u32;
        decode_l2msg_recursive_raw(&deep, |env| {
            if env == marker {
                deep_hits += 1;
            }
        });
        assert_eq!(
            deep_hits, 0,
            "a tx nested > 16 batch levels must be dropped, not yielded"
        );

        // A SignedTx nested under exactly 16 batch levels is within the cap and must resolve.
        let mut ok = signed_tx_frame(marker);
        for _ in 0..16 {
            ok = wrap_batch(&ok);
        }
        let mut ok_hits = 0u32;
        decode_l2msg_recursive_raw(&ok, |env| {
            if env == marker {
                ok_hits += 1;
            }
        });
        assert_eq!(
            ok_hits, 1,
            "a tx nested within 16 batch levels must still decode"
        );

        // The TransactionInfo walker must apply the same bound without panicking on the deep input.
        let mut info_hits = 0u32;
        decode_l2msg_recursive(&deep, |_tx| info_hits += 1);
        // (The marker isn't valid RLP, so even at a reachable depth it wouldn't yield a tx; the
        // point is the deep traversal terminates safely.)
        assert_eq!(info_hits, 0);
    }

    /// Multi-child Batch (kind 3) frame: `0x03 ++ (u64_be(len) ++ child)*` — hand-built from
    /// official Nitro `parse_l2.go` FIFO semantics (children are read and appended in INPUT
    /// order; nested batches recurse depth-first). These fixtures are the FL-03 crux.
    fn batch_frame(children: &[&[u8]]) -> Vec<u8> {
        let mut out = vec![3u8]; // L2MsgKind::Batch
        for child in children {
            out.extend_from_slice(&(child.len() as u64).to_be_bytes());
            out.extend_from_slice(child);
        }
        out
    }

    // ── minimal RLP encoders (tests only; the production engine never builds RLP) ──

    fn rlp_enc_bytes(b: &[u8]) -> Vec<u8> {
        if b.len() == 1 && b[0] < 0x80 {
            vec![b[0]]
        } else if b.len() <= 55 {
            let mut out = vec![0x80 + b.len() as u8];
            out.extend_from_slice(b);
            out
        } else {
            panic!("test fixture strings stay short");
        }
    }

    fn rlp_enc_u64(v: u64) -> Vec<u8> {
        if v == 0 {
            return vec![0x80];
        }
        let be = v.to_be_bytes();
        let start = be.iter().position(|&b| b != 0).unwrap();
        rlp_enc_bytes(&be[start..])
    }

    fn rlp_enc_list(items: &[Vec<u8>]) -> Vec<u8> {
        let payload: Vec<u8> = items.iter().flatten().copied().collect();
        assert!(payload.len() <= 55, "test fixture lists stay short");
        let mut out = vec![0xc0 + payload.len() as u8];
        out.extend_from_slice(&payload);
        out
    }

    /// A syntactically valid EIP-1559 signed-tx ENVELOPE (`0x02 ++ rlp list`) with the given
    /// nonce and `to` (None = contract creation: empty RLP `to` field). Signature fields are
    /// dummies — the decoder walks fields, it does not verify.
    fn tx_1559_envelope(nonce: u64, to: Option<[u8; 20]>, data: &[u8]) -> Vec<u8> {
        let to_field = match to {
            Some(a) => rlp_enc_bytes(&a),
            None => rlp_enc_bytes(&[]),
        };
        let list = rlp_enc_list(&[
            rlp_enc_u64(42_161),      // chain_id
            rlp_enc_u64(nonce),       // nonce
            rlp_enc_u64(1),           // max_priority_fee
            rlp_enc_u64(1),           // max_fee
            rlp_enc_u64(21_000),      // gas_limit
            to_field,                 // to
            rlp_enc_u64(0),           // value
            rlp_enc_bytes(data),      // data
            rlp_enc_list(&[]),        // access_list
            rlp_enc_u64(0),           // y_parity
            rlp_enc_u64(1),           // r
            rlp_enc_u64(1),           // s
        ]);
        let mut env = vec![0x02u8];
        env.extend_from_slice(&list);
        env
    }

    /// SignedTx (kind 4) L2 frame wrapping a valid 1559 envelope.
    fn signed_1559_frame(nonce: u64, to: Option<[u8; 20]>, data: &[u8]) -> Vec<u8> {
        let mut out = vec![4u8];
        out.extend_from_slice(&tx_1559_envelope(nonce, to, data));
        out
    }

    /// Serializes the watermark-sensitive tests: the taint watermark is process-global and
    /// monotonic, so equality assertions (e.g. "ignorable kinds do NOT move it") would be
    /// flaky under the parallel test harness without this.
    static WATERMARK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn watermark_lock() -> std::sync::MutexGuard<'static, ()> {
        WATERMARK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn collect_normalized(sequence: u64, buf: &[u8]) -> Vec<(FeedLeafCursor, &'static str, u64)> {
        // (cursor, variant tag, nonce-or-0) — owned summary so assertions read clearly.
        let mut items = Vec::new();
        normalize_l2msg(sequence, buf, |item| {
            items.push(match item {
                NormalizedItem::Decoded { cursor, info, .. } => (cursor, "decoded", info.nonce),
                NormalizedItem::Unsupported { cursor, .. } => (cursor, "unsupported", 0),
                NormalizedItem::Malformed { cursor, .. } => (cursor, "malformed", 0),
                NormalizedItem::Tainted { cursor, .. } => (cursor, "tainted", 0),
            });
        });
        items
    }

    /// FL-03 (RED): official Nitro `parse_l2.go` appends batch children FIFO in a forward
    /// loop. The walkers pushed children forward onto a LIFO stack and popped — reversing
    /// every sibling pair.
    #[test]
    fn batch_two_children_emitted_in_input_order() {
        let a = signed_tx_frame(b"\xaa\x01");
        let b = signed_tx_frame(b"\xbb\x02");
        let batch = batch_frame(&[&a, &b]);
        let mut order: Vec<Vec<u8>> = Vec::new();
        decode_l2msg_recursive_raw(&batch, |env| order.push(env.to_vec()));
        assert_eq!(
            order,
            vec![b"\xaa\x01".to_vec(), b"\xbb\x02".to_vec()],
            "batch [A,B] must emit A then B (official Nitro FIFO input order)"
        );
    }

    /// FL-03 (RED): nested batches recurse depth-first, left-to-right. For [[A,B],C] the
    /// canonical leaf order is A,B,C; for [A,[B,C]] it is also A,B,C. Exercises BOTH walkers.
    #[test]
    fn nested_batch_leaf_order_is_depth_first_for_batch_ab_then_c_and_a_then_batch_bc() {
        let a = signed_1559_frame(0, Some([0x11; 20]), b"\x01\x02\x03\x04");
        let b = signed_1559_frame(1, Some([0x22; 20]), b"\x05\x06\x07\x08");
        let c = signed_1559_frame(2, Some([0x33; 20]), b"\x09\x0a\x0b\x0c");

        // Shape 1: [[A,B],C]
        let inner_ab = batch_frame(&[&a, &b]);
        let shape1 = batch_frame(&[&inner_ab, &c]);
        // Shape 2: [A,[B,C]]
        let inner_bc = batch_frame(&[&b, &c]);
        let shape2 = batch_frame(&[&a, &inner_bc]);

        for (label, frame) in [("[[A,B],C]", &shape1), ("[A,[B,C]]", &shape2)] {
            let mut raw_lens: Vec<usize> = Vec::new();
            decode_l2msg_recursive_raw(frame, |env| raw_lens.push(env.len()));
            let expect_lens: Vec<usize> = [&a, &b, &c].iter().map(|f| f.len() - 1).collect();
            assert_eq!(
                raw_lens.len(),
                3,
                "{label}: all three leaves must emit (raw walker)"
            );
            assert_eq!(
                raw_lens, expect_lens,
                "{label}: raw walker must emit A,B,C depth-first in input order"
            );

            let mut nonces: Vec<u64> = Vec::new();
            decode_l2msg_recursive(frame, |tx| nonces.push(tx.nonce));
            assert_eq!(
                nonces,
                vec![0, 1, 2],
                "{label}: decoded walker must emit A,B,C depth-first in input order"
            );
        }
    }

    /// FL-03 + FS-06 (RED): the canonical normalizer must emit leaves in input order AND
    /// assign `inner_ordinal` 0,1,2,… at canonical leaf emission.
    #[test]
    fn recursive_batch_walkers_preserve_canonical_sibling_order_and_ordinals() {
        let a = signed_1559_frame(0, Some([0x11; 20]), b"\x01\x02\x03\x04");
        let b = signed_1559_frame(1, Some([0x22; 20]), b"\x05\x06\x07\x08");
        let c = signed_1559_frame(2, Some([0x33; 20]), b"\x09\x0a\x0b\x0c");
        let d = signed_1559_frame(3, Some([0x44; 20]), b"\x0d\x0e\x0f\x10");
        let inner_bc = batch_frame(&[&b, &c]);
        let frame = batch_frame(&[&a, &inner_bc, &d]);

        let items = collect_normalized(9_000, &frame);
        assert_eq!(items.len(), 4, "four decodable leaves must all emit");
        let nonces: Vec<u64> = items.iter().map(|(_, _, nonce)| *nonce).collect();
        assert_eq!(
            nonces,
            vec![0, 1, 2, 3],
            "canonical depth-first input order is A,B,C,D"
        );
        for (i, (cursor, tag, _)) in items.iter().enumerate() {
            assert_eq!(*tag, "decoded");
            assert_eq!(cursor.sequence, 9_000);
            assert_eq!(
                cursor.inner_ordinal, i as u32,
                "inner_ordinal must be assigned at canonical leaf emission (0,1,2,3)"
            );
        }
    }

    /// FL-04 narrowed (RED): every recursive-path decode failure must surface as a TYPED item
    /// AND advance the taint watermark — no silent `continue`. The watermark has no clear
    /// API: only canonical reconcile (Phase 2) supersedes it, so it must survive later good
    /// frames.
    #[test]
    fn recursive_decode_failure_taints_authority_until_canonical_reconcile() {
        let _guard = watermark_lock();
        // (fixture, expected variant tag, label)
        let unknown = vec![99u8, 0xaa];
        let over_deep = {
            let mut f = signed_tx_frame(b"\xde\xad");
            for _ in 0..20 {
                f = wrap_batch(&f);
            }
            f
        };
        let truncated_batch = vec![3u8, 0, 0, 0]; // length prefix cut off mid-u64
        let decode_fail = vec![4u8, 0xff, 0xff, 0xff]; // kind 4, garbage envelope
        let cases: Vec<(&[u8], &str, &str)> = vec![
            (&[], "malformed", "empty payload"),
            (&unknown, "malformed", "unknown kind 99"),
            (&over_deep, "tainted", "batch nested past the depth cap"),
            (&truncated_batch, "tainted", "truncated batch length prefix"),
            (&decode_fail, "tainted", "kind-4 envelope decode failure"),
        ];
        for (fixture, expect_tag, label) in cases {
            let before = feed_taint_watermark();
            let items = collect_normalized(7_100, fixture);
            assert_eq!(
                items.len(),
                1,
                "{label}: exactly one typed item must surface (no silent drop)"
            );
            assert_eq!(
                items[0].1, expect_tag,
                "{label}: the item must be typed {expect_tag}"
            );
            let after = feed_taint_watermark();
            assert!(
                after.count > before.count,
                "{label}: the taint watermark must advance"
            );
        }
        // Stickiness: a subsequent GOOD frame must not clear/rewind the watermark.
        let marked = feed_taint_watermark();
        let good = signed_1559_frame(7, Some([0x55; 20]), b"\x01\x02\x03\x04");
        let items = collect_normalized(7_101, &good);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].1, "decoded");
        let after_good = feed_taint_watermark();
        assert!(
            after_good.count >= marked.count,
            "a good frame must never rewind the taint watermark (no self-clear)"
        );
    }

    /// FS-01 (RED): official Nitro kinds 0 (`UnsignedUserTx`) and 1 (`ContractTx`) are REAL
    /// state-bearing kinds this engine does not decode — they must taint, not vanish. Kinds
    /// 2/7 (unimplemented in Nitro) and 6 (deprecated heartbeat) are the documented Ignorable
    /// set and must NOT move the watermark.
    #[test]
    fn official_nitro_kinds_zero_and_one_decode_or_taint() {
        let _guard = watermark_lock();
        for kind_byte in [0u8, 1u8] {
            let frame = vec![kind_byte, 0x01, 0x02, 0x03];
            let before = feed_taint_watermark();
            let mut tainted_state_bearing = 0u32;
            let mut decoded = 0u32;
            normalize_l2msg(7_200, &frame, |item| match item {
                NormalizedItem::Tainted {
                    reason: TaintReason::StateBearingUndecoded(k),
                    ..
                } => {
                    assert_eq!(k, kind_byte);
                    tainted_state_bearing += 1;
                }
                NormalizedItem::Decoded { .. } => decoded += 1,
                _ => {}
            });
            assert_eq!(
                (tainted_state_bearing, decoded),
                (1, 0),
                "kind {kind_byte} must surface as exactly one StateBearingUndecoded taint"
            );
            let after = feed_taint_watermark();
            assert!(
                after.count > before.count,
                "kind {kind_byte} must advance the taint watermark"
            );
        }
        for kind_byte in [2u8, 6u8, 7u8] {
            let frame = vec![kind_byte, 0x01];
            let before = feed_taint_watermark();
            let mut unsupported = 0u32;
            let mut other = 0u32;
            normalize_l2msg(7_201, &frame, |item| match item {
                NormalizedItem::Unsupported { .. } => unsupported += 1,
                _ => other += 1,
            });
            assert_eq!(
                (unsupported, other),
                (1, 0),
                "kind {kind_byte} is the documented Ignorable set (Nitro-unimplemented/deprecated)"
            );
            let after = feed_taint_watermark();
            assert_eq!(
                after.count, before.count,
                "Ignorable kind {kind_byte} must NOT move the taint watermark"
            );
        }
    }

    /// FL-05 ingest half (RED): a decoded signed tx with an EMPTY `to` field is a CONTRACT
    /// CREATION. The legacy decode mapped it to the zero address and let it flow as an
    /// ordinary Decoded item — indistinguishable from a genuine `to == 0x0` call and invisible
    /// as the state-bearing event it is. It must surface as typed taint.
    #[test]
    fn contract_creation_frame_taints_active_family_authority() {
        let _guard = watermark_lock();
        let creation = signed_1559_frame(5, None, b"\x60\x80\x60\x40");
        let before = feed_taint_watermark();
        let items = collect_normalized(7_300, &creation);
        assert_eq!(items.len(), 1, "the creation leaf must surface");
        assert_eq!(
            items[0].1, "tainted",
            "contract creation must be TYPED TAINT, not a Decoded item with to=0x0"
        );
        let mut reason = None;
        normalize_l2msg(7_300, &creation, |item| {
            if let NormalizedItem::Tainted { reason: r, .. } = item {
                reason = Some(r);
            }
        });
        assert_eq!(
            reason,
            Some(TaintReason::ContractCreation),
            "the taint reason must name contract creation"
        );
        let after = feed_taint_watermark();
        assert!(
            after.count > before.count,
            "creation must advance the taint watermark"
        );
    }

    /// Merge-review §H.2 restored commitment (RED): one malformed signed child must taint —
    /// but its SIBLINGS must still emit, in order. (A taint-only test could pass while the
    /// walker wrongly truncated every later sibling.)
    #[test]
    fn failed_signed_child_taints_but_siblings_still_emit() {
        let good_a = signed_1559_frame(0, Some([0x11; 20]), b"\x01\x02\x03\x04");
        let bad = vec![4u8, 0xff, 0xee]; // kind 4, undecodable envelope
        let good_c = signed_1559_frame(2, Some([0x33; 20]), b"\x09\x0a\x0b\x0c");
        let frame = batch_frame(&[&good_a, &bad, &good_c]);

        let items = collect_normalized(7_400, &frame);
        let tags: Vec<&str> = items.iter().map(|(_, tag, _)| *tag).collect();
        assert_eq!(
            tags,
            vec!["decoded", "tainted", "decoded"],
            "the malformed middle child taints; BOTH siblings still emit, in input order"
        );
        let nonces: Vec<u64> = items
            .iter()
            .filter(|(_, tag, _)| *tag == "decoded")
            .map(|(_, _, nonce)| *nonce)
            .collect();
        assert_eq!(nonces, vec![0, 2], "sibling identity/order preserved");
        let ordinals: Vec<u32> = items.iter().map(|(c, _, _)| c.inner_ordinal).collect();
        assert_eq!(
            ordinals,
            vec![0, 1, 2],
            "the tainted leaf still consumes its canonical ordinal"
        );
    }

    /// WI-03 (RED): an undecoded state-bearing item whose payload happens to contain a known
    /// pool/destination address must NEVER authorize active pool state — it surfaces as taint
    /// (watermark moves) and yields ZERO Decoded items for the classifier to act on.
    #[test]
    fn destination_only_unknown_touch_never_authorizes_active_pool_state() {
        let _guard = watermark_lock();
        // Kind-0 (UnsignedUserTx) payload embedding a 20-byte pool-looking address.
        let mut frame = vec![0u8];
        frame.extend_from_slice(&[0xab; 20]);
        frame.extend_from_slice(b"\xde\xad\xbe\xef");
        let before = feed_taint_watermark();
        let items = collect_normalized(7_500, &frame);
        assert_eq!(items.len(), 1, "the touch must surface as exactly one typed item");
        assert_eq!(
            items[0].1, "tainted",
            "a destination-only unknown touch is TAINT, not silence and not authority"
        );
        let decoded = items.iter().filter(|(_, tag, _)| *tag == "decoded").count();
        assert_eq!(decoded, 0, "no Decoded item may reach the classifier from it");
        let after = feed_taint_watermark();
        assert!(after.count > before.count, "the watermark must move");
    }

    #[test]
    fn batch_walks_lengths() {
        let mut payload = Vec::new();
        let inner_a = b"AAA".to_vec();
        let inner_b = b"BB".to_vec();
        payload.extend_from_slice(&(inner_a.len() as u64).to_be_bytes());
        payload.extend_from_slice(&inner_a);
        payload.extend_from_slice(&(inner_b.len() as u64).to_be_bytes());
        payload.extend_from_slice(&inner_b);
        let mut count = 0u32;
        decode_l2msg_batch(&payload, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 2);
    }
}
