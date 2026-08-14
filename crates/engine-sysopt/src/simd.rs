//! B3: SIMD primitives for the scoring scan.
//!
//! The hot loop in `pipeline.rs` over `seen` routes computes a scalar `cycle_marginal_product`,
//! then gates with `if !(score > 1.0) { continue }`, then pushes survivors into a bounded
//! `ArrayVec`. The scoring math itself is hard to vectorize across routes (each route touches
//! different pool slots with different families), but the filter+collect tail IS vectorizable:
//! once we have an array of 4 scores in registers, the threshold compare + survivor count is
//! a 4-wide reduction.
//!
//! This module exposes a SIMD-friendly `mask_above_threshold_f64x4` plus a `count_survivors`
//! reduction. Aarch64 NEON (Graviton4 production) + x86_64 SSE2 + scalar fallback. All paths
//! are verified to produce bit-identical results via reference tests so SIMD is never a
//! correctness divergence vs. scalar.

/// Return a 4-lane mask where lane `i` is `true` iff `scores[i] > threshold` (NaN-safe — NaN
/// rejects, matching `!(p > 1.0)` semantics used in the scoring scan).
#[inline]
pub fn mask_above_threshold_f64x4(scores: [f64; 4], threshold: f64) -> [bool; 4] {
    // Aarch64 NEON has fcmgt.2d (compares 2 f64 lanes); we use two 2-lane vectors. NEON
    // mask result is all-1s/all-0s u64 per lane. We collapse to [bool; 4].
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let lo = vld1q_f64([scores[0], scores[1]].as_ptr());
        let hi = vld1q_f64([scores[2], scores[3]].as_ptr());
        let t = vdupq_n_f64(threshold);
        let m_lo = vcgtq_f64(lo, t); // uint64x2_t mask
        let m_hi = vcgtq_f64(hi, t);
        [
            vgetq_lane_u64(m_lo, 0) != 0,
            vgetq_lane_u64(m_lo, 1) != 0,
            vgetq_lane_u64(m_hi, 0) != 0,
            vgetq_lane_u64(m_hi, 1) != 0,
        ]
    }
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    unsafe {
        use std::arch::x86_64::*;
        let lo = _mm_loadu_pd([scores[0], scores[1]].as_ptr());
        let hi = _mm_loadu_pd([scores[2], scores[3]].as_ptr());
        let t = _mm_set1_pd(threshold);
        let m_lo = _mm_cmpgt_pd(lo, t);
        let m_hi = _mm_cmpgt_pd(hi, t);
        let mask_lo = _mm_movemask_pd(m_lo);
        let mask_hi = _mm_movemask_pd(m_hi);
        [
            (mask_lo & 0b01) != 0,
            (mask_lo & 0b10) != 0,
            (mask_hi & 0b01) != 0,
            (mask_hi & 0b10) != 0,
        ]
    }
    #[cfg(not(any(
        target_arch = "aarch64",
        all(target_arch = "x86_64", target_feature = "sse2")
    )))]
    {
        // Scalar fallback — same NaN semantics.
        [
            scores[0] > threshold,
            scores[1] > threshold,
            scores[2] > threshold,
            scores[3] > threshold,
        ]
    }
}

/// Count survivors in a 4-lane mask (0..=4). Used as a quick check to short-circuit the
/// `for_each(|surviving|)` loop on a fully-rejected batch.
#[inline]
pub fn count_survivors(mask: [bool; 4]) -> u32 {
    (mask[0] as u32) + (mask[1] as u32) + (mask[2] as u32) + (mask[3] as u32)
}

/// K3: batched 4-wide V2 marginal spot-ratio for the scoring scan.
///
/// The infinitesimal-size marginal price of a V2/constant-product hop is
/// `(reserve_out / reserve_in) * (fee_factor / 10_000)`; a route's marginal product is the product
/// of its hops' ratios, and the scan rejects any route whose product is `≤ 1.0` (can't even cover
/// fees at zero size). This computes four hops' ratios at once. f64 `div`/`mul` are IEEE-754
/// deterministic, so the NEON/SSE2 paths are **bit-identical** to the scalar fallback (asserted in
/// tests) — SIMD is purely a throughput win on the approximate PRE-FILTER, never a correctness
/// divergence. The exact integer kernels (V3 tick-walk, Curve/Balancer Newton) stay scalar: a
/// vectorized 256-bit divide does not exist and their data-dependent control flow would break the
/// byte-exactness invariant.
///
/// `reserve_in` lanes must be non-zero (a zeroed lane yields `inf`/`NaN`, which the downstream
/// `mask_above_threshold_f64x4` rejects — fail-closed).
#[inline]
pub fn batch_v2_marginal_ratio_f64x4(
    reserve_in: [f64; 4],
    reserve_out: [f64; 4],
    fee_factor: [f64; 4],
) -> [f64; 4] {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let inv_denom = vdupq_n_f64(10_000.0);
        let mut out = [0.0f64; 4];
        for half in 0..2 {
            let i = half * 2;
            let ri = vld1q_f64([reserve_in[i], reserve_in[i + 1]].as_ptr());
            let ro = vld1q_f64([reserve_out[i], reserve_out[i + 1]].as_ptr());
            let ff = vld1q_f64([fee_factor[i], fee_factor[i + 1]].as_ptr());
            // (ro / ri) * (ff / 10000)
            let ratio = vmulq_f64(vdivq_f64(ro, ri), vdivq_f64(ff, inv_denom));
            vst1q_f64(out.as_mut_ptr().add(i), ratio);
        }
        out
    }
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    unsafe {
        use std::arch::x86_64::*;
        let inv_denom = _mm_set1_pd(10_000.0);
        let mut out = [0.0f64; 4];
        for half in 0..2 {
            let i = half * 2;
            let ri = _mm_loadu_pd([reserve_in[i], reserve_in[i + 1]].as_ptr());
            let ro = _mm_loadu_pd([reserve_out[i], reserve_out[i + 1]].as_ptr());
            let ff = _mm_loadu_pd([fee_factor[i], fee_factor[i + 1]].as_ptr());
            let ratio = _mm_mul_pd(_mm_div_pd(ro, ri), _mm_div_pd(ff, inv_denom));
            _mm_storeu_pd(out.as_mut_ptr().add(i), ratio);
        }
        out
    }
    #[cfg(not(any(
        target_arch = "aarch64",
        all(target_arch = "x86_64", target_feature = "sse2")
    )))]
    {
        let mut out = [0.0f64; 4];
        for k in 0..4 {
            out[k] = (reserve_out[k] / reserve_in[k]) * (fee_factor[k] / 10_000.0);
        }
        out
    }
}

/// Scalar reference for [`batch_v2_marginal_ratio_f64x4`] — the exact same f64 op sequence, lane by
/// lane. The SIMD paths must equal this bit-for-bit (IEEE-754 determinism).
#[inline]
pub fn batch_v2_marginal_ratio_scalar(
    reserve_in: [f64; 4],
    reserve_out: [f64; 4],
    fee_factor: [f64; 4],
) -> [f64; 4] {
    let mut out = [0.0f64; 4];
    for k in 0..4 {
        out[k] = (reserve_out[k] / reserve_in[k]) * (fee_factor[k] / 10_000.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_matches_scalar_reference_on_easy_cases() {
        let scores = [0.5_f64, 1.0_f64, 1.5_f64, 2.0_f64];
        let m = mask_above_threshold_f64x4(scores, 1.0);
        assert_eq!(
            m,
            [false, false, true, true],
            "x > 1.0 must reject 1.0 itself (strict GT) and pass 1.5/2.0"
        );
    }

    #[test]
    fn mask_rejects_nan_and_neg_infinity() {
        let scores = [f64::NAN, f64::NEG_INFINITY, 0.0, f64::INFINITY];
        let m = mask_above_threshold_f64x4(scores, 1.0);
        // NaN compared with > is always false → must reject. NEG_INFINITY rejects. 0 rejects.
        // +INFINITY passes (it's > any finite threshold).
        assert_eq!(m, [false, false, false, true]);
    }

    #[test]
    fn mask_matches_scalar_on_dense_random_inputs() {
        // Parity gate: SIMD path must be bit-identical to the scalar reference for every
        // input. If the SIMD path ever drifts (e.g. wrong NEON intrinsic), this fires.
        let threshold = 1.0_f64;
        let cases = [
            [0.999_999_999_9, 1.0, 1.000_000_000_1, 1.5],
            [-1.0, -0.0, 0.0, 1e-300],
            [1e308, -1e308, 1e-308, -1e-308],
            [2.0, 2.0, 2.0, 2.0],
            [f64::MIN_POSITIVE, f64::MAX, -f64::MAX, 1.0],
        ];
        for &c in &cases {
            let m = mask_above_threshold_f64x4(c, threshold);
            let scalar = [
                c[0] > threshold,
                c[1] > threshold,
                c[2] > threshold,
                c[3] > threshold,
            ];
            assert_eq!(
                m, scalar,
                "SIMD drift on {c:?}: simd={m:?} scalar={scalar:?}"
            );
        }
    }

    #[test]
    fn count_survivors_returns_popcount() {
        assert_eq!(count_survivors([true, true, true, true]), 4);
        assert_eq!(count_survivors([true, false, true, false]), 2);
        assert_eq!(count_survivors([false, false, false, false]), 0);
    }

    #[test]
    fn batch_v2_ratio_simd_is_bit_identical_to_scalar() {
        // Parity gate: the 4-wide SIMD marginal-ratio must equal the scalar reference BIT-for-bit
        // (IEEE-754 f64 div/mul are deterministic). Any NEON/SSE2 drift fires here. The scan only
        // uses this as an approximate pre-filter, but the SIMD/scalar paths must never disagree.
        let cases: [([f64; 4], [f64; 4], [f64; 4]); 5] = [
            (
                [1.0, 2.0, 3.0, 4.0],
                [2.0, 2.0, 9.0, 1.0],
                [9970.0, 9999.0, 9000.0, 9970.0],
            ),
            (
                [1e18, 5e20, 1.0, 7.5e12],
                [2e18, 1e21, 100.0, 5e12],
                [9970.0, 9970.0, 9970.0, 9990.0],
            ),
            (
                [1.5, 1e-6, 1e9, 123.0],
                [1.5, 1e6, 1e9, 456.0],
                [10000.0, 5000.0, 1.0, 9970.0],
            ),
            (
                [1e300, 1e-300, 1e150, 2.0],
                [1e-300, 1e300, 1e150, 2.0],
                [9970.0; 4],
            ),
            (
                [3.0, 3.0, 3.0, 3.0],
                [1.0, 2.0, 3.0, 4.0],
                [9970.0, 9970.0, 9970.0, 9970.0],
            ),
        ];
        for (ri, ro, ff) in cases {
            let simd = batch_v2_marginal_ratio_f64x4(ri, ro, ff);
            let scalar = batch_v2_marginal_ratio_scalar(ri, ro, ff);
            // bit-for-bit equality (compare raw bits so NaN lanes also match exactly)
            for k in 0..4 {
                assert_eq!(
                    simd[k].to_bits(),
                    scalar[k].to_bits(),
                    "SIMD drift lane {k} on ri={ri:?} ro={ro:?} ff={ff:?}: simd={} scalar={}",
                    simd[k],
                    scalar[k]
                );
            }
        }
    }

    #[test]
    fn batch_v2_ratio_orders_with_price_and_fee() {
        // Economic sanity: a higher reserve_out/reserve_in ⇒ higher ratio; a higher fee (lower
        // fee_factor) ⇒ lower ratio. A ratio > 1.0 is the scan's "worth simulating" gate.
        let r = batch_v2_marginal_ratio_f64x4(
            [100.0, 100.0, 100.0, 100.0],
            [200.0, 300.0, 200.0, 200.0],
            [9970.0, 9970.0, 9000.0, 9970.0],
        );
        assert!(r[1] > r[0], "higher out/in ⇒ higher ratio");
        assert!(r[2] < r[0], "higher fee (lower factor) ⇒ lower ratio");
        assert!(r[0] > 1.0, "2x price at 0.3% fee clears the >1.0 gate");
    }
}
