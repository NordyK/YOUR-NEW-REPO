//! Arbitrum sequencer feed decoder.
//!
//! Reads `BroadcastFeedMessage` frames from the local relay socket, extracts
//! `sequenceNumber`, `header.timestamp`, and `l2Msg` via `memchr`-based byte scans, decodes
//! the base64 `l2Msg` payload in place using `base64-simd`, dispatches on the first byte
//! (`L2MsgKind`), and RLP-decodes signed transactions to recover `(to, data, value, from)`.
//!
//! No allocator usage on the hot path: all transient buffers are owned by the caller via the
//! `FeedDecoder` struct, and the parser writes its outputs into a fixed-size record.
//!
//! References:
//! * Arbitrum docs: `https://docs.arbitrum.io/run-arbitrum-node/sequencer/read-sequencer-feed`
//! * Nitro `L2MessageKind` constants: `arbos/parse_l2.go`
//! * Public Arbitrum One feed (relay upstream): `wss://arb1-feed.arbitrum.io/feed`
//! * Public Arbitrum One sequencer endpoint: `https://arb1-sequencer.arbitrum.io/rpc`

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// clippy: DecodeError is returned large-by-value on the COLD decode-error path (perf-irrelevant);
// boxing it would churn every decoder signature for no hot-path gain. Suppress on this crate.
#![allow(clippy::result_large_err)]

pub mod address;
pub mod constants;
pub mod envelope;
pub mod feed_frame;
pub mod l2msg;
pub mod relay_client;
pub mod rlp;

/// Parse a default-off enabled-mode flag using the engine-wide accepted vocabulary.
///
/// Only `1`, `true`, `on`, and `yes` (ASCII case-insensitive, surrounding whitespace ignored)
/// enable a feature. Missing values and every other spelling remain disabled.
pub fn parse_enabled_flag(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1") | Some("true") | Some("on") | Some("yes")
    )
}

pub use address::{recover_sender, Address, ZERO_ADDRESS};
pub use constants::{
    ARB_ONE_CHAIN_ID, ARB_ONE_FEED_WSS, ARB_ONE_SEQUENCER_RPC, BALANCER_VAULT_ARBITRUM,
    NITRO_GENESIS_BLOCK_NUMBER,
};
pub use envelope::{decode_envelope, iter_envelopes, EnvelopeFields, EnvelopeIter};
pub use feed_frame::{
    decode_frame, encode_frame, encode_frame_v2, encode_frame_v3, encode_frame_v4,
    FeedFrameAuthority, FeedFrameMarker, FeedFrameView, FEED_FRAME_FLAG_IGNORABLE_ORDINAL,
    FEED_FRAME_FLAG_TAINT, FEED_FRAME_HEADER_LEN,
    FEED_FRAME_HEADER_LEN_V2, FEED_FRAME_HEADER_LEN_V3, FEED_FRAME_HEADER_LEN_V4,
    FEED_FRAME_MAGIC, FEED_FRAME_MAGIC_V2, FEED_FRAME_MAGIC_V3, FEED_FRAME_MAGIC_V4,
    is_feed_payload_conflict_sentinel, FEED_PAYLOAD_CONFLICT_SENTINEL,
    FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC,
};
pub use l2msg::{
    decode_l2msg, decode_l2msg_batch, decode_l2msg_recursive, decode_l2msg_recursive_raw,
    decode_signed_tx, feed_taint_watermark, normalize_l2msg, record_feed_taint, FeedLeafCursor,
    FeedTaintWatermark, L2MsgKind, MalformedReason, NormalizedItem, TaintReason, TransactionInfo,
    TAINT_CODE_NONAUTHORITATIVE_FRAME, TAINT_CODE_UNDECODABLE_PAYLOAD,
};
pub use relay_client::{
    FeedDegradationReason, RelayCallback, RelayError, WsRelayClient, DEFAULT_LOCAL_RELAY_WS,
    PUBLIC_ARB_ONE_FEED_WS,
};
pub use rlp::{
    access_list_addresses, decode_signed_tx_1559, decode_signed_tx_legacy, recover_tx_sender,
    AccessListAddresses, RlpError,
};
