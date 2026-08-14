//! [Stage 5 · 5% receipt deep-compare] A low-rate, deterministic per-tx canonical-receipt
//! cross-check that FEEDS the Stage-1 reconcile under `MIRROR_RECONCILE=sampled`.
//!
//! Ported from Fulcrum's `validation/sampling.rs` (deterministic tx-hash sampling): a fixed ~5% of
//! feed txs — selected by the first byte of the tx hash, so the choice is reproducible and needs no
//! shared counter — additionally have their CANONICAL receipt fetched and compared against what the
//! advancer journaled. If the advancer replayed a tx as producing state changes but the canonical
//! chain reverted it or never included it, that is a divergence signal that flags the block for a
//! forced reconcile (the same honesty net Stage 1 already applies to gaps).
//!
//! This module is PURE (sampler decision + status enum + divergence rule) and laptop-tested. The
//! live `eth_getTransactionReceipt` fetch that supplies [`ReceiptStatus`] is soak/archive-gated (see
//! [`crate::revm_fork`]). Default reconcile mode is `logs`, so with the flag unchanged this whole
//! path is inert — byte-identical to Stage 1.

/// Deterministic per-tx sampler: samples a tx iff `tx_hash[0] < floor(rate * 256)`. A uniform tx
/// hash therefore samples at ~`rate`, with no modulo bias and no shared state (reproducible across
/// restarts / threads).
#[derive(Debug, Clone, Copy)]
pub struct ReceiptSampler {
    /// `floor(rate * 256)` — a tx-hash first byte strictly below this is sampled (0 ⇒ never,
    /// 256 ⇒ always).
    threshold: u16,
}

impl ReceiptSampler {
    /// Build a sampler for a percentage rate (clamped to 0..=100). `MIRROR_RECEIPT_SAMPLE_PCT=5`
    /// ⇒ threshold 12 ⇒ ~4.7% of uniform hashes.
    pub fn new(rate_pct: u8) -> Self {
        let rate = (rate_pct.min(100) as f64) / 100.0;
        let threshold = (rate * 256.0).floor() as u16;
        Self { threshold }
    }

    /// Whether `tx_hash` is selected for a receipt deep-compare this pass.
    pub fn should_sample(&self, tx_hash: &[u8; 32]) -> bool {
        (tx_hash[0] as u16) < self.threshold
    }

    /// The selection threshold (test/observability hook).
    pub fn threshold(&self) -> u16 {
        self.threshold
    }
}

/// The canonical status of a sampled tx, as read from its receipt (or its absence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptStatus {
    /// Receipt present, `status == 0x1`.
    Success,
    /// Receipt present, `status == 0x0` (the tx reverted on-chain).
    Reverted,
    /// No receipt for this hash at the canonical head (dropped / reorged / not yet mined).
    Absent,
}

/// Divergence rule: the advancer journaled `journaled_state_change` for this tx (it replayed
/// successfully and composed slots onto the mirror). If the canonical receipt disagrees — the tx
/// actually reverted or is absent — then the mirror holds state that never truly landed, which is a
/// divergence that must feed the reconcile. A journaled tx confirmed `Success` is consistent; a tx
/// we did NOT treat as state-changing can never diverge on this axis.
pub fn receipt_diverges(journaled_state_change: bool, status: ReceiptStatus) -> bool {
    journaled_state_change && !matches!(status, ReceiptStatus::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_with_first(b: u8) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0] = b;
        h
    }

    #[test]
    fn threshold_matches_fulcrum_five_percent() {
        // 5% → floor(0.05 * 256) = floor(12.8) = 12.
        assert_eq!(ReceiptSampler::new(5).threshold(), 12);
        assert_eq!(ReceiptSampler::new(0).threshold(), 0);
        assert_eq!(ReceiptSampler::new(100).threshold(), 256);
        assert_eq!(ReceiptSampler::new(200).threshold(), 256, "clamped to 100%");
    }

    #[test]
    fn sampling_is_deterministic_and_boundary_correct() {
        let s = ReceiptSampler::new(5); // threshold 12: bytes 0..=11 sample, 12.. don't
        assert!(s.should_sample(&hash_with_first(0)));
        assert!(s.should_sample(&hash_with_first(11)));
        assert!(!s.should_sample(&hash_with_first(12)));
        assert!(!s.should_sample(&hash_with_first(255)));
        // Deterministic: same hash → same decision.
        let h = hash_with_first(3);
        assert_eq!(s.should_sample(&h), s.should_sample(&h));
    }

    #[test]
    fn selected_fraction_matches_rate_over_all_first_bytes() {
        let s = ReceiptSampler::new(5);
        let selected = (0u16..256).filter(|&b| s.should_sample(&hash_with_first(b as u8))).count();
        assert_eq!(selected, 12, "exactly floor(rate*256) of the 256 first-byte classes sample");
    }

    #[test]
    fn never_and_always_sample_edges() {
        let never = ReceiptSampler::new(0);
        let always = ReceiptSampler::new(100);
        for b in 0u16..256 {
            assert!(!never.should_sample(&hash_with_first(b as u8)), "0% never samples");
            assert!(always.should_sample(&hash_with_first(b as u8)), "100% always samples");
        }
    }

    #[test]
    fn divergence_truth_table() {
        // Journaled a state change → only Success is consistent.
        assert!(!receipt_diverges(true, ReceiptStatus::Success));
        assert!(receipt_diverges(true, ReceiptStatus::Reverted));
        assert!(receipt_diverges(true, ReceiptStatus::Absent));
        // Not treated as state-changing → never diverges on this axis.
        assert!(!receipt_diverges(false, ReceiptStatus::Reverted));
        assert!(!receipt_diverges(false, ReceiptStatus::Absent));
        assert!(!receipt_diverges(false, ReceiptStatus::Success));
    }
}
