//! secp256k1 signing helpers.
//!
//! The trading key is held in memory only. `SigningKey` wraps a `SecretKey` with a
//! single-purpose interface and never serialises itself.

use secp256k1::{Message, Secp256k1, SecretKey, SignOnly};
use thiserror::Error;
use tiny_keccak::{Hasher, Keccak};

/// Signing errors.
#[derive(Debug, Clone, Copy, Error)]
pub enum SigningError {
    /// libsecp256k1 returned an error.
    #[error("signing failed")]
    Internal,
}

/// secp256k1 group order n/2 (big-endian) — the EIP-2 low-S upper bound. A canonical signature
/// has `s <= n/2`; the malleable twin is `n - s`. libsecp256k1 normalises to low-S, so this is
/// only used by a defense-in-depth `debug_assert!` that would trip if a future backend regressed.
/// (Not `#[cfg(debug_assertions)]`-gated: `debug_assert!` still name-resolves its expression in
/// release builds, so the const must exist there too; it is simply never read at runtime.)
const SECP256K1_HALF_N: [u8; 32] = [
    0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0x5D, 0x57, 0x6E, 0x73, 0x57, 0xA4, 0x50, 0x1D, 0xDF, 0xE9, 0x2F, 0x46, 0x68, 0x1B, 0x20, 0xA0,
];

/// 32-byte secret key wrapper.
///
/// The `Secp256k1<SignOnly>` context (with its precomputed multiplication tables) is built **once**
/// at construction and reused for every sign — the submit hot path no longer rebuilds it per
/// signature (TAIL_LATENCY_PLAN §2 D2; BENCHMARKS flagged ~28 µs sign as the dominant submit cost).
pub struct SigningKey {
    secret: SecretKey,
    secp: Secp256k1<SignOnly>,
}

impl SigningKey {
    /// Construct from 32 raw bytes. Returns `Err` if the bytes are not a valid scalar.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, SigningError> {
        let secret = SecretKey::from_slice(bytes).map_err(|_| SigningError::Internal)?;
        Ok(Self {
            secret,
            secp: Secp256k1::signing_only(),
        })
    }

    /// Recover the 20-byte address corresponding to this key.
    pub fn address(&self) -> [u8; 20] {
        let pubkey = self.secret.public_key(&self.secp);
        let uncompressed = pubkey.serialize_uncompressed();
        let mut h = Keccak::v256();
        h.update(&uncompressed[1..]);
        let mut hash = [0u8; 32];
        h.finalize(&mut hash);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&hash[12..]);
        addr
    }

    /// Sign a 32-byte digest with this key's cached context, returning `(r, s, y_parity)`.
    #[inline]
    pub fn sign_hash(&self, signing_hash: &[u8; 32]) -> ([u8; 32], [u8; 32], u8) {
        let msg = Message::from_digest(*signing_hash);
        let sig = self.secp.sign_ecdsa_recoverable(&msg, &self.secret);
        let (recid, compact) = sig.serialize_compact();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&compact[..32]);
        s.copy_from_slice(&compact[32..]);
        // Defense-in-depth (EIP-2): libsecp256k1 already returns the canonical low-S form. Assert
        // it so a future backend swap that emits high-S (malleable) signatures fails loudly in
        // debug/test rather than silently broadcasting a malleable tx.
        debug_assert!(s <= SECP256K1_HALF_N, "ECDSA s must be low-S (<= n/2)");
        (r, s, i32::from(recid) as u8)
    }
}

/// Sign an EIP-1559 transaction's *keccak256 hash of the type-prefixed RLP-encoded
/// unsigned payload* and return `(r, s, y_parity)`. Uses the key's cached signing context.
pub fn sign_eip1559(
    key: &SigningKey,
    signing_hash: &[u8; 32],
) -> Result<([u8; 32], [u8; 32], u8), SigningError> {
    Ok(key.sign_hash(signing_hash))
}

/// Sign a legacy transaction (EIP-155 v) — convenience for the canary self-transfer.
pub fn sign_legacy_keccak(
    key: &SigningKey,
    signing_hash: &[u8; 32],
    chain_id: u64,
) -> Result<([u8; 32], [u8; 32], u64), SigningError> {
    let (r, s, y) = sign_eip1559(key, signing_hash)?;
    // v = chain_id * 2 + 35 + y_parity
    let v = chain_id * 2 + 35 + y as u64;
    Ok((r, s, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_recovers() {
        let key_bytes = [1u8; 32];
        let key = SigningKey::from_bytes(&key_bytes).unwrap();
        let addr = key.address();
        let hash = [2u8; 32];
        let (r, s, y) = sign_eip1559(&key, &hash).unwrap();
        // Recover via engine_feed and compare addresses.
        let mut sig65 = [0u8; 65];
        sig65[..32].copy_from_slice(&r);
        sig65[32..64].copy_from_slice(&s);
        sig65[64] = y;
        let recovered = engine_feed::recover_sender(&hash, &sig65).unwrap();
        assert_eq!(recovered, addr);
    }

    // D2: the cached signing context must produce byte-identical signatures across repeated calls
    // (RFC-6979 deterministic ECDSA) — proving context reuse changed nothing but the cost.
    #[test]
    fn cached_context_signs_deterministically() {
        let key = SigningKey::from_bytes(&[7u8; 32]).unwrap();
        let hash = [9u8; 32];
        let a = sign_eip1559(&key, &hash).unwrap();
        let b = sign_eip1559(&key, &hash).unwrap();
        assert_eq!(
            a, b,
            "same key+hash must sign identically (deterministic + reused ctx)"
        );
        // The address is also stable across calls on the cached context.
        assert_eq!(key.address(), key.address());
        // A different digest yields a different signature (sanity: not a constant).
        let c = sign_eip1559(&key, &[10u8; 32]).unwrap();
        assert_ne!(a, c);
    }

    /// secp256k1 group order n/2 (EIP-2 low-S bound), big-endian. A canonical ECDSA signature
    /// MUST have `s <= n/2`; the other root `n - s` is the malleable twin. libsecp256k1 already
    /// normalises to low-S, so this pins that guarantee on a real signature (and the matching
    /// `debug_assert!` in `sign_hash` trips if a future backend swap regresses it).
    const SECP256K1_HALF_N: [u8; 32] = [
        0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0x5D, 0x57, 0x6E, 0x73, 0x57, 0xA4, 0x50, 0x1D, 0xDF, 0xE9, 0x2F, 0x46, 0x68, 0x1B,
        0x20, 0xA0,
    ];

    #[test]
    fn signature_s_is_low_half() {
        let key = SigningKey::from_bytes(&[0x42u8; 32]).unwrap();
        // Several distinct digests so we don't depend on one lucky `s`.
        for seed in [1u8, 2, 7, 13, 99, 200] {
            let (_r, s, _y) = sign_eip1559(&key, &[seed; 32]).unwrap();
            assert!(
                s <= SECP256K1_HALF_N,
                "signature s must be in the low half (<= n/2): s = {s:02x?}"
            );
        }
    }

    // Two distinct keys recover to two distinct addresses, each matching its own signature.
    #[test]
    fn distinct_keys_recover_distinctly() {
        let k1 = SigningKey::from_bytes(&[1u8; 32]).unwrap();
        let k2 = SigningKey::from_bytes(&[2u8; 32]).unwrap();
        assert_ne!(k1.address(), k2.address());
        let hash = [3u8; 32];
        for (k, a) in [(&k1, k1.address()), (&k2, k2.address())] {
            let (r, s, y) = sign_eip1559(k, &hash).unwrap();
            let mut sig65 = [0u8; 65];
            sig65[..32].copy_from_slice(&r);
            sig65[32..64].copy_from_slice(&s);
            sig65[64] = y;
            assert_eq!(engine_feed::recover_sender(&hash, &sig65).unwrap(), a);
        }
    }
}
