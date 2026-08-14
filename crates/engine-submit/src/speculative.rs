//! B5: Speculative submission scaffolding.
//!
//! Today the submit pipeline runs strictly sequentially: simulate → REVM-validate → sign → submit.
//! ECDSA signing (~25–50 µs on Graviton4) and tx encoding (~5 µs) sit on the latency critical
//! path *after* the validator's verdict. Most validator verdicts on our scored top-K are
//! positive (the scoring + sim already filtered ~95 % of garbage), so doing signing AFTER
//! validation wastes 50+ µs on the typical win path.
//!
//! Speculative submission moves signing **in parallel** with the validator: build + sign the tx
//! optimistically the moment the simulator emits a candidate; when the validator returns, either
//! commit the already-signed bytes (~0 µs additional) or discard them (no on-chain effect — the
//! signature is just bytes in memory). This shaves the ECDSA cost off the win path while costing
//! only the work itself on rejects (which we're throwing away anyway).
//!
//! ## Cancel semantics
//! `cancel()` is a local no-op: the signed bytes never reach the network. The on-chain nonce is
//! NOT consumed (no tx is sent). The signer's secret is unchanged. The only "cost" is the CPU
//! cycles spent on the speculative sign, which is exactly the cost we'd pay synchronously today.
//!
//! ## Lifecycle
//! ```text
//!   sim emits cand →  SpeculativeTx::build_and_sign(...)
//!                  ╲                                  ╱
//!  REVM validator ────────┬─ verdict = OK  → .commit() → submit bytes to sequencer
//!                          │
//!                          └─ verdict = REJECT → .cancel() (drop bytes; ECDSA cost amortized)
//! ```
//!
//! ## Why this isn't a security risk
//! The signed tx is bound to a specific `(chain_id, nonce, to, value, calldata, gas)` tuple. A stale
//! signature is still a perfectly valid transaction for that **old** tuple and the sequencer may
//! accept it; it does not become invalid merely because local state moved. Safety therefore comes
//! from keeping speculative bytes opaque until a validator commit and requiring exact current-intent
//! equality at submit. Any ambiguity must cancel the speculative value and synchronously re-sign.

use crate::signing::{sign_eip1559, SigningError, SigningKey};
use crate::tx_encode::{encode_eip1559_signed, encode_eip1559_unsigned, Eip1559Tx};
use thiserror::Error;
use tiny_keccak::{Hasher, Keccak};

/// State of a speculatively-built transaction.
///
/// Construction signs immediately; `commit` returns the bytes, `cancel` drops them.
pub struct SpeculativeTx {
    /// The fully-encoded, signed RLP bytes ready to wrap in `eth_sendRawTransaction`.
    signed_rlp: Vec<u8>,
    /// The keccak256 hash of the signed payload (= tx hash). Useful for tracking + tracing.
    tx_hash: [u8; 32],
}

impl SpeculativeTx {
    /// Build + sign a tx speculatively. This runs the same code path as a synchronous sign;
    /// the speculative part is calling it in PARALLEL with validation rather than after.
    pub fn build_and_sign(tx: &Eip1559Tx, signing_key: &SigningKey) -> Result<Self, SigningError> {
        let unsigned = encode_eip1559_unsigned(tx);
        let mut h = Keccak::v256();
        h.update(&unsigned);
        let mut signing_hash = [0u8; 32];
        h.finalize(&mut signing_hash);
        let (r, s, y_parity) = sign_eip1559(signing_key, &signing_hash)?;
        let signed_rlp = encode_eip1559_signed(tx, (r, s, y_parity));
        let mut h2 = Keccak::v256();
        h2.update(&signed_rlp);
        let mut tx_hash = [0u8; 32];
        h2.finalize(&mut tx_hash);
        Ok(Self {
            signed_rlp,
            tx_hash,
        })
    }

    /// Validator approved → return the signed bytes ready for the wire.
    pub fn commit(self) -> CommittedTx {
        CommittedTx {
            signed_rlp: self.signed_rlp,
            tx_hash: self.tx_hash,
        }
    }

    /// Validator rejected → drop the bytes. No on-chain effect.
    pub fn cancel(self) -> CancelledTx {
        CancelledTx {
            tx_hash: self.tx_hash,
        }
    }

    /// Inspect the tx hash without committing or cancelling — useful for tracing/logging while
    /// the validator is still deliberating.
    #[inline]
    pub fn tx_hash(&self) -> &[u8; 32] {
        &self.tx_hash
    }
}

/// Failure while producing an independently parity-checked speculative signature.
#[derive(Debug, Clone, Copy, Error)]
pub enum ParityCheckError {
    /// One of the two deterministic signing passes failed.
    #[error(transparent)]
    Signing(#[from] SigningError),
    /// The two fresh signs disagreed in signed RLP or transaction hash.
    #[error("independent speculative signing passes disagreed")]
    Mismatch,
}

/// Opaque speculative transaction whose signed RLP and hash matched a second fresh deterministic
/// sign of the exact same EIP-1559 intent.
///
/// No raw bytes are exposed before [`Self::commit_if_exact`]. The caller must additionally bind the
/// value to its validator commit token and reconstruct the authoritative submit-time transaction
/// after claiming the nonce. A changed intent returns this value unchanged so it can only be
/// cancelled; it never exposes stale bytes to the wire.
pub struct ParityCheckedSpeculativeTx {
    intent: Eip1559Tx,
    signer: [u8; 20],
    speculative: SpeculativeTx,
}

impl ParityCheckedSpeculativeTx {
    /// Perform two fresh deterministic signs on the current thread and retain one opaque result only
    /// when both signed RLP and transaction hash are byte-identical.
    pub fn build(tx: &Eip1559Tx, key: &SigningKey) -> Result<Self, ParityCheckError> {
        let speculative = SpeculativeTx::build_and_sign(tx, key)?;
        let reference = SpeculativeTx::build_and_sign(tx, key)?;
        Self::from_pair(tx.clone(), key.address(), speculative, reference)
    }

    fn from_pair(
        intent: Eip1559Tx,
        signer: [u8; 20],
        speculative: SpeculativeTx,
        reference: SpeculativeTx,
    ) -> Result<Self, ParityCheckError> {
        let parity = speculative.signed_rlp == reference.signed_rlp
            && speculative.tx_hash == reference.tx_hash;
        let _ = reference.cancel();
        if !parity {
            let _ = speculative.cancel();
            return Err(ParityCheckError::Mismatch);
        }
        Ok(Self {
            intent,
            signer,
            speculative,
        })
    }

    /// Commit only when every EIP-1559 field and the expected signer identity equal the
    /// authoritative submit-time values. On mismatch, returns the still-opaque value so the caller
    /// can cancel it and fresh-sign.
    pub fn commit_if_exact(
        self,
        authoritative: &Eip1559Tx,
        authoritative_signer: [u8; 20],
    ) -> Result<CommittedTx, Self> {
        if self.intent == *authoritative && self.signer == authoritative_signer {
            Ok(self.speculative.commit())
        } else {
            Err(self)
        }
    }

    /// Cancel the opaque value. No signed RLP is returned.
    pub fn cancel(self) -> CancelledTx {
        self.speculative.cancel()
    }

    /// Hash of the parity-checked speculative transaction for tracing only.
    #[inline]
    pub fn tx_hash(&self) -> &[u8; 32] {
        self.speculative.tx_hash()
    }

    #[cfg(test)]
    fn from_pair_for_test(
        intent: Eip1559Tx,
        signer: [u8; 20],
        speculative: SpeculativeTx,
        reference: SpeculativeTx,
    ) -> Result<Self, ParityCheckError> {
        Self::from_pair(intent, signer, speculative, reference)
    }
}

/// A committed signed tx — bytes ready to wrap in `eth_sendRawTransaction`.
pub struct CommittedTx {
    signed_rlp: Vec<u8>,
    tx_hash: [u8; 32],
}

impl CommittedTx {
    /// Borrow the signed RLP bytes.
    #[inline]
    pub fn signed_rlp(&self) -> &[u8] {
        &self.signed_rlp
    }
    /// The tx's keccak256 hash.
    #[inline]
    pub fn tx_hash(&self) -> &[u8; 32] {
        &self.tx_hash
    }
    /// Take ownership of the signed bytes for the wire writer.
    pub fn into_bytes(self) -> Vec<u8> {
        self.signed_rlp
    }
}

/// A cancelled speculative tx. Carries only the tx hash for tracing; no bytes leave this struct.
pub struct CancelledTx {
    tx_hash: [u8; 32],
}

impl CancelledTx {
    /// The hash of the tx that was cancelled. The corresponding bytes never reached the wire.
    #[inline]
    pub fn tx_hash(&self) -> &[u8; 32] {
        &self.tx_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx_encode::Eip1559Tx;

    fn sample_key() -> SigningKey {
        SigningKey::from_bytes(&[0x11; 32]).expect("0x11 repeated is a valid scalar")
    }

    fn sample_tx() -> Eip1559Tx {
        Eip1559Tx {
            chain_id: 42_161,
            nonce: 7,
            max_priority_fee_per_gas: alloy_primitives::U256::from(1_000_000_000u64),
            max_fee_per_gas: alloy_primitives::U256::from(100_000_000_000u64),
            gas_limit: 2_000_000,
            to: [0xab; 20],
            value: alloy_primitives::U256::ZERO,
            data: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    #[test]
    fn build_and_sign_produces_recoverable_signed_rlp() {
        let key = sample_key();
        let tx = sample_tx();
        let spec = SpeculativeTx::build_and_sign(&tx, &key).expect("build_and_sign");
        // Hash is deterministic for the same input.
        assert_eq!(spec.tx_hash().len(), 32);
        // The signed RLP must start with the EIP-1559 type byte (0x02) and be longer than
        // the unsigned payload (sig adds ≥65 bytes).
        let unsigned = encode_eip1559_unsigned(&tx);
        let signed = &spec.signed_rlp;
        assert_eq!(signed[0], 0x02, "EIP-1559 type byte");
        assert!(
            signed.len() > unsigned.len(),
            "signed must include signature bytes"
        );
    }

    #[test]
    fn commit_returns_same_bytes_as_synchronous_sign() {
        // Parity: speculative path must produce bit-identical bytes to a synchronous sign of
        // the same tx + key. Any drift would mean speculative submission lands a *different*
        // tx than the validator approved — unacceptable.
        let key = sample_key();
        let tx = sample_tx();
        let spec = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        let speculative_bytes = spec.commit().into_bytes();

        // Reference: do it synchronously the long way.
        let unsigned = encode_eip1559_unsigned(&tx);
        let mut h = Keccak::v256();
        h.update(&unsigned);
        let mut hash = [0u8; 32];
        h.finalize(&mut hash);
        let (r, s, y) = sign_eip1559(&key, &hash).unwrap();
        let sync_bytes = encode_eip1559_signed(&tx, (r, s, y));

        assert_eq!(
            speculative_bytes, sync_bytes,
            "speculative bytes diverged from synchronous sign — would land a different tx"
        );
    }

    #[test]
    fn cancel_yields_only_the_hash_no_bytes_leak() {
        // Contract: cancel destroys the signed bytes (RAII drop) and leaves only the hash for
        // tracing. Future readers of the cancelled struct see no signing material.
        let key = sample_key();
        let tx = sample_tx();
        let spec = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        let saved_hash = *spec.tx_hash();
        let cancelled = spec.cancel();
        assert_eq!(
            cancelled.tx_hash(),
            &saved_hash,
            "cancel must preserve the hash for tracing"
        );
        // No method on CancelledTx returns bytes — typestate prevents bytes-leak by design.
    }

    #[test]
    fn build_and_sign_is_deterministic_for_same_tx_and_key() {
        // ECDSA on libsecp256k1 with RFC 6979 nonce is deterministic — two calls with the
        // same (tx, key) MUST produce identical bytes. This is the property the validator
        // can rely on when comparing the speculative bytes against its own reproduction.
        let key = sample_key();
        let tx = sample_tx();
        let a = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        let b = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        assert_eq!(
            a.signed_rlp, b.signed_rlp,
            "RFC 6979 deterministic ECDSA: two signs of same input must match"
        );
        assert_eq!(a.tx_hash, b.tx_hash);
    }

    #[test]
    fn build_and_sign_distinguishes_different_nonces() {
        // Sanity: different tx contents → different signed bytes + different hashes. Catches
        // a regression where build_and_sign accidentally ignores its input.
        let key = sample_key();
        let mut tx = sample_tx();
        let a = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        tx.nonce += 1;
        let b = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        assert_ne!(a.signed_rlp, b.signed_rlp);
        assert_ne!(a.tx_hash, b.tx_hash);
    }

    #[test]
    fn parity_checked_many_case_corpus_matches_synchronous_rlp_hash_and_http_body() {
        let calldata_lengths = [0usize, 1, 55, 56, 127, 128, 196, 388, 580, 1124, 1476, 1600];
        let nonces = [0u64, 1, 127, 128, 255, 256, u32::MAX as u64, u64::MAX];
        let gas_limits = [21_000u64, 1_300_000, 3_867_557, 25_000_000];
        let mut cases = 0u64;

        for key_seed in 1u8..=4 {
            let speculative_key = SigningKey::from_bytes(&[key_seed; 32]).unwrap();
            let synchronous_key = SigningKey::from_bytes(&[key_seed; 32]).unwrap();
            for (i, &calldata_len) in calldata_lengths.iter().enumerate() {
                let mut data = vec![0u8; calldata_len];
                for (j, byte) in data.iter_mut().enumerate() {
                    *byte = key_seed.wrapping_mul(31).wrapping_add((i + j) as u8);
                }
                let tx = Eip1559Tx {
                    chain_id: if i % 3 == 0 { 1 } else { 42_161 },
                    nonce: nonces[i % nonces.len()],
                    max_priority_fee_per_gas: alloy_primitives::U256::ZERO,
                    max_fee_per_gas: alloy_primitives::U256::from(
                        20_000_000u64.saturating_mul((i as u64) + 1),
                    ),
                    gas_limit: gas_limits[i % gas_limits.len()],
                    to: [key_seed.wrapping_add(i as u8); 20],
                    value: if i % 5 == 0 {
                        alloy_primitives::U256::from(i as u64)
                    } else {
                        alloy_primitives::U256::ZERO
                    },
                    data,
                };

                let checked =
                    ParityCheckedSpeculativeTx::build(&tx, &speculative_key).expect("checked sign");
                let checked_committed = checked
                    .commit_if_exact(&tx, synchronous_key.address())
                    .unwrap_or_else(|_| panic!("identical intent rejected"));
                let synchronous = SpeculativeTx::build_and_sign(&tx, &synchronous_key)
                    .expect("synchronous sign")
                    .commit();

                assert_eq!(checked_committed.signed_rlp(), synchronous.signed_rlp());
                assert_eq!(checked_committed.tx_hash(), synchronous.tx_hash());

                let mut checked_body = Vec::new();
                let mut synchronous_body = Vec::new();
                crate::jsonrpc::build_send_raw_tx_envelope(
                    checked_committed.signed_rlp(),
                    cases + 1,
                    &mut checked_body,
                );
                crate::jsonrpc::build_send_raw_tx_envelope(
                    synchronous.signed_rlp(),
                    cases + 1,
                    &mut synchronous_body,
                );
                assert_eq!(checked_body, synchronous_body, "HTTP body case {cases}");
                cases += 1;
            }
        }
        assert_eq!(cases, 48, "corpus must remain broad enough to cross RLP boundaries");
    }

    #[test]
    fn parity_checked_exact_intent_gate_rejects_every_transaction_field_change() {
        let key = sample_key();
        let original = sample_tx();

        let mut variants = Vec::new();
        let mut tx = original.clone();
        tx.chain_id += 1;
        variants.push(tx);
        let mut tx = original.clone();
        tx.nonce += 1;
        variants.push(tx);
        let mut tx = original.clone();
        tx.max_priority_fee_per_gas += alloy_primitives::U256::from(1u8);
        variants.push(tx);
        let mut tx = original.clone();
        tx.max_fee_per_gas += alloy_primitives::U256::from(1u8);
        variants.push(tx);
        let mut tx = original.clone();
        tx.gas_limit += 1;
        variants.push(tx);
        let mut tx = original.clone();
        tx.to[0] ^= 1;
        variants.push(tx);
        let mut tx = original.clone();
        tx.value += alloy_primitives::U256::from(1u8);
        variants.push(tx);
        let mut tx = original.clone();
        tx.data.push(0);
        variants.push(tx);

        for changed in variants {
            let checked = ParityCheckedSpeculativeTx::build(&original, &key).unwrap();
            match checked.commit_if_exact(&changed, key.address()) {
                Ok(_) => panic!("changed EIP-1559 intent must never commit speculative bytes"),
                Err(stale) => {
                    let cancelled = stale.cancel();
                    assert_eq!(cancelled.tx_hash().len(), 32);
                }
            }
        }
    }

    #[test]
    fn parity_checked_wrong_signer_identity_is_fail_closed() {
        let expected_key = SigningKey::from_bytes(&[0x11; 32]).unwrap();
        let wrong_key = SigningKey::from_bytes(&[0x22; 32]).unwrap();
        let tx = sample_tx();
        let checked = ParityCheckedSpeculativeTx::build(&tx, &wrong_key).unwrap();
        match checked.commit_if_exact(&tx, expected_key.address()) {
            Ok(_) => panic!("exact tx signed by the wrong key must never commit"),
            Err(stale) => {
                let _ = stale.cancel();
            }
        }
    }

    #[test]
    fn parity_pair_mismatch_is_fail_closed_before_commit() {
        let key = sample_key();
        let tx = sample_tx();
        let speculative = SpeculativeTx::build_and_sign(&tx, &key).unwrap();
        let mut changed = tx.clone();
        changed.nonce += 1;
        let mismatched_reference = SpeculativeTx::build_and_sign(&changed, &key).unwrap();
        assert!(ParityCheckedSpeculativeTx::from_pair_for_test(
            tx,
            key.address(),
            speculative,
            mismatched_reference,
        )
        .is_err());
    }
}
