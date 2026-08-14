//! Brotli L1 size estimator (background-only, §2.3 / §12.3).
//!
//! The Arbitrum sequencer compresses calldata with Brotli before posting to L1. We
//! precompute `L1S` (the brotli-compressed byte count) for every `(template_id,
//! amount_bucket_id)` pair on the background core, then the hot path looks it up.
//!
//! Arbitrum's per-transaction L1 size estimate uses **brotli-zero** — the brotli
//! compressor at its lowest quality (0), the fastest setting — applied to the tx
//! data, then multiplied by 16 to get L1 gas. We match that quality exactly so
//! our estimate tracks what ArbOS actually charges (see
//! <https://docs.arbitrum.io/how-arbitrum-works/l1-gas-pricing>).

use std::io::Write;
use std::sync::OnceLock;

use brotli::CompressorWriter;

/// Compute the brotli-zero compressed byte count of `calldata`, matching the
/// Arbitrum L1-size estimator (quality 0 — the lowest/fastest level).
///
/// Returns the number of bytes the L1 transaction will charge for. This function is
/// allocation-heavy and is **never** called on the hot path — its output is
/// precomputed (see [`brotli_calldata_bytes`]) and then looked up.
pub fn brotli_size_estimate(calldata: &[u8]) -> usize {
    let mut sink: Vec<u8> = Vec::with_capacity(calldata.len() / 2 + 64);
    {
        // quality 0 (brotli-zero, matches ArbOS), lgwin 22.
        let mut compressor = CompressorWriter::new(&mut sink, 4096, 0, 22);
        compressor
            .write_all(calldata)
            .expect("brotli compress write");
        compressor.flush().expect("brotli compress flush");
    }
    sink.len()
}

/// Upper bound on the raw calldata byte budget we model (8-hop deep cycles stay
/// well under this). Sized to keep the lazily-filled cache small.
const MAX_RAW_BYTES: usize = 2048;

/// Brotli-zero compressed L1 byte count for an atomic-arb submission whose *raw*
/// calldata budget is `raw_bytes`. The hot-path EV gate calls this instead of
/// feeding the raw byte count straight into [`crate::l1_data_fee_wei`]: Arbitrum
/// bills the brotli-compressed size, and the posted calldata (a flash-loan
/// provider's ABI-encoded call, dominated by zero-padded address/amount words)
/// compresses substantially, so the raw count over-charges the L1 fee and
/// rejects otherwise-profitable arbs.
///
/// Compression is far too slow for the hot path (50-200µs), so each distinct
/// `raw_bytes` value is compressed once and memoized in a lock-free table; later
/// calls are a single atomic load. The byte budget per route shape is preserved
/// (we don't second-guess the caller's raw estimate) — we only apply the
/// compression step that was previously missing.
pub fn brotli_calldata_bytes(raw_bytes: u32) -> u32 {
    static CACHE: OnceLock<Vec<std::sync::atomic::AtomicU32>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        (0..MAX_RAW_BYTES)
            .map(|_| std::sync::atomic::AtomicU32::new(0))
            .collect()
    });
    let idx = (raw_bytes as usize).min(MAX_RAW_BYTES - 1);
    let cached = cache[idx].load(std::sync::atomic::Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    // Cap at the raw byte count: a single tx's L1 contribution can never exceed its uncompressed
    // size (brotli framing overhead is amortized across the batch, not charged per-tx). This
    // removes the small-input framing artifact while keeping the estimate conservative — for
    // compressible calldata the brotli size wins; for worst-case incompressible calldata we charge
    // the full raw count, never less.
    let v = (brotli_size_estimate(&representative_calldata_of_len(idx)) as u32)
        .min(idx as u32)
        .max(1);
    cache[idx].store(v, std::sync::atomic::Ordering::Relaxed);
    v
}

/// Build a structurally-realistic atomic-arb calldata blob of `len` bytes. The
/// exact field semantics don't matter for the compressed-size estimate; what
/// matters is the byte structure that ABI-encoded EVM calldata actually has —
/// 32-byte words where addresses carry 12 leading zero bytes, small ints carry
/// ~28-30 leading zeros, and token amounts carry several. Brotli-zero compresses
/// those zero runs the same way it will for the real submission. The blob cycles
/// address/small/amount word shapes and truncates to exactly `len`.
fn representative_calldata_of_len(len: usize) -> Vec<u8> {
    // Worst-case-realistic atomic-arb calldata: 32-byte ABI words where ONLY the structural ABI
    // zero-padding compresses and every significant byte is MAXIMALLY high-entropy (a splitmix64
    // stream — NOT a linear ramp, which brotli would model and under-count). Address-shaped words
    // (12 zero pad + 20 entropy) DOMINATE because they are the least-compressible real word, so the
    // brotli-zero size of this blob is a conservative UPPER bound on any real submission's L1 size
    // and never under-charges the L1 data fee. Deterministic per `len` (seeded by it) → cacheable.
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15 ^ (len as u64);
    let mut fill = |dst: &mut [u8]| {
        for chunk in dst.chunks_mut(8) {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let b = z.to_be_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
    };
    let mut cd: Vec<u8> = Vec::with_capacity(len + 32);
    cd.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); // selector
    let mut i = 0u8;
    while cd.len() < len {
        let mut w = [0u8; 32];
        match i % 4 {
            0 => fill(&mut w[28..32]), // small-int word (exchangeId/fee): 28 zero pad + 4 entropy
            1 => fill(&mut w[16..32]), // amount word: 16 zero pad + 16 entropy
            _ => fill(&mut w[12..32]), // address word (x2, dominant): 12 zero pad + 20 entropy
        }
        cd.extend_from_slice(&w);
        i = i.wrapping_add(1);
    }
    cd.truncate(len);
    cd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highly_compressible_input_shrinks() {
        let input = vec![0u8; 4_096];
        let n = brotli_size_estimate(&input);
        assert!(n < 100, "expected high compression, got {n}");
    }

    /// Build realistic atomic-arb calldata: 4-byte selector + per-hop words
    /// [exchangeId, tokenIn, tokenOut, fee, pool, amountIn]. Pools/amounts are DISTINCT and
    /// high-entropy per hop (real routes touch different pools at different sizes); only the ABI
    /// zero-padding (12-byte address pads, ~16-byte amount pads) compresses.
    fn realistic_arb_calldata(hops: usize) -> Vec<u8> {
        fn nxt(s: &mut u64) -> [u8; 8] {
            *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)).to_be_bytes()
        }
        let small = |v: u32| -> [u8; 32] {
            let mut w = [0u8; 32];
            w[28..].copy_from_slice(&v.to_be_bytes());
            w
        };
        let mut s: u64 = 0x1234_5678_9ABC_DEF0;
        let mut cd: Vec<u8> = vec![0xab, 0xcd, 0xef, 0x12];
        for h in 0..hops {
            cd.extend_from_slice(&small((h as u32) % 28)); // exchangeId
            for _ in 0..3 {
                // address word: 12 zero pad + 20 high-entropy bytes
                let mut w = [0u8; 32];
                w[12..20].copy_from_slice(&nxt(&mut s));
                w[20..28].copy_from_slice(&nxt(&mut s));
                w[28..32].copy_from_slice(&nxt(&mut s)[..4]);
                cd.extend_from_slice(&w);
            }
            cd.extend_from_slice(&small(500 + (h as u32) * 100)); // fee tier
                                                                  // amountIn: ~16 zero pad + 16 high-entropy bytes
            let mut w = [0u8; 32];
            w[16..24].copy_from_slice(&nxt(&mut s));
            w[24..32].copy_from_slice(&nxt(&mut s));
            cd.extend_from_slice(&w);
        }
        cd
    }

    #[test]
    fn estimator_is_conservative_vs_realistic_arb_calldata() {
        // SAFETY (L1 under-charge guard): the length-keyed estimate MUST be an upper bound on what
        // a REAL arb calldata of that length compresses to. Otherwise l1_data_fee_wei under-charges
        // and a loss-making trade passes the net-profit gate (REVM/anvil don't model L1). The prior
        // synthetic blob used repeated constants (small_word(500)/0-1) → over-compressible →
        // UNDER-estimate. This pins the conservative-upper-bound invariant.
        for hops in 2..=8usize {
            let real = realistic_arb_calldata(hops);
            if real.len() >= MAX_RAW_BYTES {
                continue;
            }
            let est = brotli_calldata_bytes(real.len() as u32) as usize;
            let actual = brotli_size_estimate(&real);
            assert!(
                est >= actual,
                "L1 estimate {est} < real compressed {actual} (hops={hops}, raw={}) — UNDER-charges L1",
                real.len()
            );
        }
    }

    #[test]
    fn random_input_does_not_shrink_much() {
        let mut input = vec![0u8; 1_024];
        for (i, b) in input.iter_mut().enumerate() {
            // pseudo-random
            *b = (i as u8).wrapping_mul(37).wrapping_add(13);
        }
        let n = brotli_size_estimate(&input);
        assert!(
            n > 256,
            "uniform pseudo-random should not compress aggressively, got {n}"
        );
    }

    #[test]
    fn calldata_budget_compresses_below_raw() {
        // The estimate is CONSERVATIVE: it tracks the worst-case (high-entropy) brotli-zero size
        // but is capped at the raw count (a tx's L1 contribution can't exceed its uncompressed
        // size). So it is always <= raw, and for larger budgets (more zero-padding) strictly below.
        for raw in [240u32, 260, 280, 320, 340, 360, 400] {
            let compressed = brotli_calldata_bytes(raw);
            println!("raw={raw} brotli0={compressed}");
            assert!(
                compressed <= raw,
                "raw={raw}: compressed {compressed} not <= raw"
            );
            assert!(
                compressed > raw / 2,
                "raw={raw}: compressed {compressed} implausibly small (under-charge)"
            );
        }
        // Larger budgets never compress to fewer bytes than smaller ones.
        assert!(brotli_calldata_bytes(400) >= brotli_calldata_bytes(240));
        // Repeated calls hit the memoized value.
        assert_eq!(brotli_calldata_bytes(280), brotli_calldata_bytes(280));
    }
}
