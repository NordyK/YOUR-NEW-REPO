//! 20-byte address type plus secp256k1 sender recovery (cold-path utility — never used on
//! the hot detector loop).

use secp256k1::{ecdsa::RecoverableSignature, Message, Secp256k1};
use thiserror::Error;
use tiny_keccak::{Hasher, Keccak};

/// 20-byte Ethereum address.
pub type Address = [u8; 20];

/// All-zero address (used for contract-creation `to == 0`).
pub const ZERO_ADDRESS: Address = [0u8; 20];

/// Sender recovery errors.
#[derive(Debug, Clone, Copy, Error)]
pub enum AddressError {
    /// secp256k1 reported an internal error during recovery.
    #[error("secp256k1 failed to recover")]
    Recovery,
    /// Signature has an invalid recovery id.
    #[error("invalid recovery id")]
    InvalidRecoveryId,
}

/// Recover the sender address from a signing hash and a 65-byte `(r, s, v)` signature.
///
/// `signing_hash` is the keccak256 of the canonical signing payload (caller's responsibility).
/// `v` is the byte after `r||s` and is reduced to 0/1 internally.
pub fn recover_sender(signing_hash: &[u8; 32], sig65: &[u8; 65]) -> Result<Address, AddressError> {
    let v = sig65[64];
    let recovery_id = match v {
        0 | 27 => 0i32,
        1 | 28 => 1i32,
        _ => return Err(AddressError::InvalidRecoveryId),
    };
    let mut rs = [0u8; 64];
    rs.copy_from_slice(&sig65[..64]);
    let rec_id = secp256k1::ecdsa::RecoveryId::try_from(recovery_id)
        .map_err(|_| AddressError::InvalidRecoveryId)?;
    let sig =
        RecoverableSignature::from_compact(&rs, rec_id).map_err(|_| AddressError::Recovery)?;
    let msg = Message::from_digest(*signing_hash);
    let secp = Secp256k1::verification_only();
    let pubkey = secp
        .recover_ecdsa(&msg, &sig)
        .map_err(|_| AddressError::Recovery)?;
    let uncompressed = pubkey.serialize_uncompressed();
    debug_assert_eq!(uncompressed[0], 0x04);
    let mut hasher = Keccak::v256();
    hasher.update(&uncompressed[1..]);
    let mut hash = [0u8; 32];
    hasher.finalize(&mut hash);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    Ok(addr)
}

/// Compute keccak256 of `input`.
pub fn keccak256(input: &[u8]) -> [u8; 32] {
    let mut h = Keccak::v256();
    h.update(input);
    let mut out = [0u8; 32];
    h.finalize(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak_empty_known() {
        let h = keccak256(b"");
        // keccak256("") == c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let expected: [u8; 32] = [
            0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7,
            0x03, 0xc0, 0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04,
            0x5d, 0x85, 0xa4, 0x70,
        ];
        assert_eq!(h, expected);
    }
}
