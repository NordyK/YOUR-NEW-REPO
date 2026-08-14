//! Nonce lane state (§8.9, §10.11).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// Lane class.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneClass {
    /// WETH/USDC + WBTC/WETH highest-EV lane.
    HighEvTwoHop = 0,
    /// Stables basket.
    Stables = 1,
    /// CLMM + triangle.
    Triangles = 2,
    /// Warm / lower-frequency.
    Warm = 3,
    /// Variant overflow / self-backrun second leg.
    VariantOverflow = 4,
}

/// Lane status snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceLaneStatus {
    /// Enabled and within in-flight cap.
    Live,
    /// Temporarily disabled (e.g. nonce mismatch).
    Disabled,
    /// At in-flight cap.
    Saturated,
}

/// One nonce lane.
pub struct NonceLane {
    /// 8-bit lane id.
    pub eoa_id: u8,
    /// 20-byte address.
    pub eoa: [u8; 20],
    /// Next nonce to use.
    pub next_nonce: AtomicU64,
    /// Warm TLS connection id (engine-owned).
    pub connection_id: u16,
    /// In-flight counter.
    pub in_flight: AtomicU32,
    /// In-flight cap.
    pub max_in_flight: u32,
    /// Class.
    pub lane_class: LaneClass,
    /// Enabled flag.
    pub enabled: AtomicBool,
    /// Reclaimed (claimed-but-never-sent) nonces, min-heap so the LOWEST is re-issued first to
    /// fill the would-be gap before `next_nonce` advances. Sole caller is the single submit
    /// thread (+ the rare background reconciler), so this `Mutex` is effectively uncontended.
    /// Without this, a claim that fails to reach the wire (sign error, zero-executor guard,
    /// send failure, or a discarded speculative pre-sign) would leave a permanent nonce gap —
    /// fatal on Arbitrum, where a missing nonce stalls every higher nonce on the EOA.
    reclaimed: Mutex<BinaryHeap<Reverse<u64>>>,
}

impl NonceLane {
    /// Construct a lane.
    pub fn new(
        eoa_id: u8,
        eoa: [u8; 20],
        initial_nonce: u64,
        connection_id: u16,
        lane_class: LaneClass,
        max_in_flight: u32,
    ) -> Self {
        Self {
            eoa_id,
            eoa,
            next_nonce: AtomicU64::new(initial_nonce),
            connection_id,
            in_flight: AtomicU32::new(0),
            max_in_flight,
            lane_class,
            enabled: AtomicBool::new(true),
            reclaimed: Mutex::new(BinaryHeap::new()),
        }
    }

    /// Claim a nonce and increment the in-flight counter. Returns `None` if disabled or
    /// saturated. Re-issues the lowest **reclaimed** nonce first (filling a would-be gap)
    /// before advancing `next_nonce`.
    pub fn claim_nonce(&self) -> Option<u64> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        let inflight = self.in_flight.load(Ordering::Acquire);
        if inflight >= self.max_in_flight {
            return None;
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        // Prefer a reclaimed nonce so a prior unsent claim doesn't leave a permanent gap.
        if let Ok(mut heap) = self.reclaimed.lock() {
            if let Some(Reverse(n)) = heap.pop() {
                return Some(n);
            }
        }
        let n = self.next_nonce.fetch_add(1, Ordering::AcqRel);
        Some(n)
    }

    /// Mark an in-flight submission as completed (the tx reached the wire). Decrements in-flight
    /// only — the nonce is consumed on-chain, NOT reclaimed.
    pub fn release(&self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }

    /// Return a claimed-but-NEVER-SENT nonce to the lane so it can be re-issued without leaving
    /// a sequential-nonce gap. Use on every path where `claim_nonce` succeeded but the tx did
    /// not reach the sequencer (sign error, zero-executor guard, send failure, or a discarded
    /// speculative pre-sign). Also decrements the in-flight counter (the slot is freed).
    ///
    /// Fast path: if `nonce == next_nonce - 1` (the just-issued nonce, the overwhelmingly common
    /// case), simply roll `next_nonce` back via CAS — no heap entry. Otherwise stash it in the
    /// reclaim heap for `claim_nonce` to re-issue.
    pub fn reclaim(&self, nonce: u64) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        // Try the CAS roll-back fast path: if no other claim has advanced next_nonce past
        // `nonce + 1`, just decrement it (zero heap churn).
        if self
            .next_nonce
            .compare_exchange(nonce + 1, nonce, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return;
        }
        // Out-of-order reclaim (a later nonce was claimed before this one was reclaimed) →
        // stash for re-issue. Lowest re-issues first.
        if let Ok(mut heap) = self.reclaimed.lock() {
            heap.push(Reverse(nonce));
        }
    }

    /// Snapshot status.
    pub fn status(&self) -> NonceLaneStatus {
        if !self.enabled.load(Ordering::Acquire) {
            return NonceLaneStatus::Disabled;
        }
        if self.in_flight.load(Ordering::Acquire) >= self.max_in_flight {
            return NonceLaneStatus::Saturated;
        }
        NonceLaneStatus::Live
    }

    /// Disable this lane.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// Re-enable this lane.
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    /// Read the next nonce without claiming it.
    pub fn peek_nonce(&self) -> u64 {
        self.next_nonce.load(Ordering::Acquire)
    }

    /// Force-set the next nonce (background reconciler) and DISCARD the reclaim heap.
    ///
    /// The reconciler calls this after querying the EOA's true on-chain nonce to repair drift.
    /// Resetting `next_nonce` invalidates every entry in the reclaim heap, so the heap MUST be
    /// cleared or `claim_nonce` (which pops the heap before reading `next_nonce`) would re-issue
    /// a stale nonce and wedge the lane:
    ///
    /// - Forward jump (`new_nonce >= old next_nonce`): every reclaimed entry is `< old next_nonce
    ///   <= new_nonce`, i.e. now below the floor. Re-issuing one yields a nonce-too-low tx the
    ///   sequencer rejects — a permanent stall on that lane.
    /// - Backward reset (`new_nonce < old next_nonce`): `next_nonce` will itself re-traverse and
    ///   re-issue `[new_nonce ..)`, so any heap entry `>= new_nonce` would be handed out TWICE
    ///   (once from the heap, once from the counter) — a duplicate nonce. Entries `< new_nonce`
    ///   are stale as above. Either way the entire heap is meaningless after the reset.
    ///
    /// Clearing is therefore correct in both directions and skips no live nonce (the counter
    /// re-covers `[new_nonce ..)`). The in-flight counter is left untouched — it tracks tx that
    /// actually reached the wire, which the reconciler does not adjudicate here.
    pub fn force_nonce(&self, new_nonce: u64) {
        self.next_nonce.store(new_nonce, Ordering::Release);
        if let Ok(mut heap) = self.reclaimed.lock() {
            heap.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_increments_nonce_and_inflight() {
        let lane = NonceLane::new(0, [0u8; 20], 5, 0, LaneClass::HighEvTwoHop, 2);
        assert_eq!(lane.claim_nonce(), Some(5));
        assert_eq!(lane.claim_nonce(), Some(6));
        assert_eq!(lane.claim_nonce(), None); // saturated
        lane.release();
        assert_eq!(lane.claim_nonce(), Some(7));
    }

    #[test]
    fn reclaim_fast_path_rolls_back_just_issued_nonce_no_gap() {
        // The dominant case: claim N, immediately reclaim N (sign failed) → next claim is N
        // again, NOT N+1. No gap.
        let lane = NonceLane::new(0, [0u8; 20], 10, 0, LaneClass::HighEvTwoHop, 4);
        assert_eq!(lane.claim_nonce(), Some(10));
        lane.reclaim(10);
        assert_eq!(
            lane.peek_nonce(),
            10,
            "next_nonce rolled back to the reclaimed value"
        );
        assert_eq!(
            lane.claim_nonce(),
            Some(10),
            "reclaimed nonce re-issued — no gap"
        );
        assert_eq!(lane.claim_nonce(), Some(11));
    }

    #[test]
    fn reclaim_out_of_order_refills_lowest_first() {
        // Claim 10,11,12; reclaim 10 (the lowest, out of order since 11/12 came after). The
        // next claim must re-issue 10 to fill the gap before issuing 13.
        let lane = NonceLane::new(0, [0u8; 20], 10, 0, LaneClass::HighEvTwoHop, 8);
        assert_eq!(lane.claim_nonce(), Some(10));
        assert_eq!(lane.claim_nonce(), Some(11));
        assert_eq!(lane.claim_nonce(), Some(12));
        lane.reclaim(10); // out-of-order: next_nonce is 13, not 11
        assert_eq!(
            lane.claim_nonce(),
            Some(10),
            "lowest reclaimed re-issued first (fills gap)"
        );
        assert_eq!(
            lane.claim_nonce(),
            Some(13),
            "then continue from next_nonce"
        );
    }

    #[test]
    fn reclaim_frees_inflight_slot() {
        // Reclaim must free the in-flight slot (the submission never happened).
        let lane = NonceLane::new(0, [0u8; 20], 0, 0, LaneClass::Stables, 1);
        assert_eq!(lane.claim_nonce(), Some(0));
        assert_eq!(lane.claim_nonce(), None, "saturated at cap 1");
        lane.reclaim(0);
        assert_eq!(lane.claim_nonce(), Some(0), "slot freed + nonce re-issued");
    }

    #[test]
    fn no_gaps_over_random_claim_reclaim_sequence() {
        // Property: after any interleaving of claim/reclaim, the set of OUTSTANDING (claimed,
        // not reclaimed) nonces ∪ reclaimed-pending ∪ [next_nonce..) must be gap-free starting
        // from the initial nonce. We model it: every claimed nonce that is reclaimed must be
        // re-issuable, and the union of issued+pending must cover [start, max_issued] with no hole.
        let lane = NonceLane::new(0, [0u8; 20], 100, 0, LaneClass::Triangles, 16);
        let mut live: Vec<u64> = Vec::new();
        // Claim 6, reclaim the 2nd and 4th (out of order), then drain.
        for _ in 0..6 {
            live.push(lane.claim_nonce().unwrap());
        }
        lane.reclaim(live[1]);
        lane.reclaim(live[3]);
        // The two reclaimed must come back (lowest first) before any brand-new nonce.
        let a = lane.claim_nonce().unwrap();
        let b = lane.claim_nonce().unwrap();
        let mut reissued = [a, b];
        reissued.sort_unstable();
        let mut expected = [live[1], live[3]];
        expected.sort_unstable();
        assert_eq!(
            reissued, expected,
            "both reclaimed nonces re-issued — no permanent gap"
        );
    }

    #[test]
    fn force_nonce_forward_purges_stale_reclaimed() {
        // E2: reconciler learns the on-chain nonce jumped forward (e.g. the lane lost track of
        // txs the EOA already landed). Any reclaimed nonce is now BELOW the new floor and stale;
        // re-issuing it would produce a nonce-too-low tx the sequencer rejects → lane stall.
        let lane = NonceLane::new(0, [0u8; 20], 10, 0, LaneClass::HighEvTwoHop, 16);
        assert_eq!(lane.claim_nonce(), Some(10));
        assert_eq!(lane.claim_nonce(), Some(11));
        assert_eq!(lane.claim_nonce(), Some(12));
        lane.reclaim(10); // out-of-order → heap {10}
        lane.reclaim(11); // heap {10, 11}
        lane.force_nonce(20); // forward jump; 10 & 11 are now stale
        assert_eq!(
            lane.claim_nonce(),
            Some(20),
            "stale reclaimed (< new floor) purged — issue from the forced nonce"
        );
        assert_eq!(lane.claim_nonce(), Some(21));
    }

    #[test]
    fn force_nonce_backward_reset_does_not_duplicate_reclaimed() {
        // E2: reconciler resets the floor BACKWARD (on-chain next is lower than the lane thought
        // because higher nonces never landed). `next_nonce` will re-traverse [14..), so a heap
        // entry >= the new floor must NOT also be re-issued from the heap — that is a DUPLICATE
        // nonce (two txs claim 14), which collides on-chain.
        let lane = NonceLane::new(0, [0u8; 20], 10, 0, LaneClass::HighEvTwoHop, 16);
        for _ in 0..6 {
            lane.claim_nonce().unwrap(); // 10..=15, next_nonce = 16
        }
        lane.reclaim(14); // heap {14}
        lane.force_nonce(14); // reset floor to 14; heap entry 14 must not double-issue
        let a = lane.claim_nonce().unwrap();
        let b = lane.claim_nonce().unwrap();
        assert_ne!(a, b, "no duplicate nonce after a backward reset");
        assert_eq!(a, 14, "issue from the forced floor");
        assert_eq!(b, 15);
    }

    #[test]
    fn disabled_returns_none() {
        let lane = NonceLane::new(0, [0u8; 20], 0, 0, LaneClass::Stables, 4);
        lane.disable();
        assert_eq!(lane.claim_nonce(), None);
        assert_eq!(lane.status(), NonceLaneStatus::Disabled);
    }
}
