//! Submit pipeline: signer, nonce lanes, transaction encoding, JSON-RPC envelope.
//!
//! The hot path signs and serialises locally; warm TLS connections to the sequencer are
//! the responsibility of `engine` (it owns the runtime). This crate provides the pure-Rust
//! pieces every submission needs.

// `unsafe_code` is allowed ONLY in the io_uring_send module (which gates it behind a target
// + feature cfg). The rest of the crate stays safe-by-default; the unsafe is limited to the
// io_uring SQE push (pointer + length must be sized correctly per the kernel ABI).
#![deny(unsafe_code, missing_docs)]

pub mod hot_spare;
pub mod jsonrpc;
pub mod nonce_lane;
pub mod signing;
pub mod speculative;
pub mod tls;
pub mod tx_encode;
pub mod variants;

#[cfg(all(target_os = "linux", feature = "io-uring-submit"))]
#[allow(unsafe_code)]
pub mod io_uring_send;

pub use hot_spare::{HotSpare, SubmitBoundary, SubmitBoundaryStamp, SubmitConn};
pub use jsonrpc::{
    build_send_raw_tx_conditional_envelope, build_send_raw_tx_envelope, classify_submit_response,
    ConditionalPreconditions, KnownStorage, SubmitOutcome,
};
pub use nonce_lane::{LaneClass, NonceLane, NonceLaneStatus};
pub use signing::{sign_eip1559, sign_legacy_keccak, SigningKey};
pub use tls::{
    tls_handshakes_at_connect, DeliveryState, SequencerTlsConnection, SubmitError, SubmitFailure,
    MAX_SUBMIT_BODY, MAX_SUBMIT_REQUEST, TLS_DEFAULT_PORT,
};
pub use tx_encode::{encode_eip1559_signed, encode_eip1559_unsigned, Eip1559Tx};
pub use variants::{decide_variants, VariantPolicy};
