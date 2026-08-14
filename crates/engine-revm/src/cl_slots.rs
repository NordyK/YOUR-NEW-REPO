//! Concentrated-liquidity storage-slot derivation for the validator fork prewarm.
//!
//! A REVM candidate replay that touches a V3/V4 pool reads MORE than the `{slot0, liquidity}`
//! pair the boot prewarm covered: every swap also reads the fee-growth globals, the current
//! observation (V3 only), at least one `tickBitmap` word, and — when it crosses — the full
//! `ticks[t]` structs. Each such miss was a synchronous remote RPC on the validation hot path
//! (`VALIDATION_RPC_COLD`). This module derives those slots PURELY (keccak slot math) from
//! state the engine's mirror already holds (current tick, tick spacing, initialized tick set),
//! so the boot/idle prewarm can batch-fetch them off the hot path.
//!
//! Layout ground truth (verified against live Arbitrum contracts, 2026-07-01, by comparing
//! derived-slot `eth_getStorageAt`/`extsload` reads with the public getters):
//! * UniswapV3 (0xC6962004f452bE9203591991D15f6b388e09E8D0, WETH/USDC 500):
//!   `ticks` mapping at slot 5 — `keccak(int256(tick) ‖ 5)` holds
//!   `liquidityNet(int128) ‖ liquidityGross(uint128)` byte-identical to `ticks(int24)`;
//!   `tickBitmap` at slot 6 — `keccak(int256(word) ‖ 6)` (bit `compressed % 256` proven set).
//!   Base slots: 0 = slot0, 1/2 = feeGrowthGlobal0/1, 3 = protocolFees, 4 = liquidity,
//!   8 + i = observations[i]. `Tick.Info` spans 4 slots.
//! * UniswapV4 PoolManager (0x360E68faCcca8cA495c1B759Fd9EEe466db9FB32):
//!   `pools` mapping at slot 6 — state base = `keccak(poolId ‖ 6)`; base+0 = slot0 (packing
//!   proven == `StateView.getSlot0`), base+1/2 = feeGrowthGlobal0/1, base+3 = liquidity
//!   (proven == `getLiquidity`), base+4 = `ticks` mapping, base+5 = `tickBitmap`
//!   (`keccak(int256(word) ‖ base+5)` proven == `getTickBitmap`). `TickInfo` spans 3 slots
//!   (no oracle fields in V4).

use alloy_primitives::U256;
use tiny_keccak::{Hasher, Keccak};

use crate::erc20_slots::StorageProbe;

/// UniswapV3Pool `ticks` mapping base slot.
const V3_TICKS_SLOT: u64 = 5;
/// UniswapV3Pool `tickBitmap` mapping base slot.
const V3_TICK_BITMAP_SLOT: u64 = 6;
/// UniswapV3Pool `observations` array base slot.
const V3_OBSERVATIONS_SLOT: u64 = 8;
/// V4 PoolManager `pools` mapping base slot (`StateView`'s `POOLS_SLOT`).
const V4_POOLS_SLOT: u64 = 6;
/// V3 `Tick.Info` struct size in slots (liq, feeGrowthOutside0, feeGrowthOutside1, packed oracle).
const V3_TICK_INFO_SLOTS: u64 = 4;
/// V4 `TickInfo` struct size in slots (liq, feeGrowthOutside0, feeGrowthOutside1).
const V4_TICK_INFO_SLOTS: u64 = 3;

fn keccak_two_words(a: &[u8; 32], b: &[u8; 32]) -> U256 {
    let mut k = Keccak::v256();
    k.update(a);
    k.update(b);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    U256::from_be_bytes(out)
}

/// A signed value as a 32-byte two's-complement ABI word (mapping keys are `int256`-extended).
fn int_word(v: i64) -> [u8; 32] {
    let mut w = if v < 0 { [0xFFu8; 32] } else { [0u8; 32] };
    w[24..32].copy_from_slice(&v.to_be_bytes());
    w
}

/// Storage slot of UniswapV3 `ticks[tick]` (first slot of the 4-slot `Tick.Info`).
pub fn v3_ticks_mapping_slot(tick: i32) -> U256 {
    keccak_two_words(
        &int_word(tick as i64),
        &U256::from(V3_TICKS_SLOT).to_be_bytes::<32>(),
    )
}

/// Storage slot of UniswapV3 `tickBitmap[word]`.
pub fn v3_tick_bitmap_slot(word: i32) -> U256 {
    keccak_two_words(
        &int_word(word as i64),
        &U256::from(V3_TICK_BITMAP_SLOT).to_be_bytes::<32>(),
    )
}

/// Uniswap tick-bitmap position for `tick` at `spacing`: `(word, bit)` with the contract's
/// FLOOR semantics for negative ticks (`compressed--` when `tick < 0 && tick % spacing != 0`).
pub fn compress_word_bit(tick: i32, spacing: i32) -> (i32, u8) {
    let spacing = spacing.max(1);
    let mut compressed = tick / spacing;
    if tick < 0 && tick % spacing != 0 {
        compressed -= 1;
    }
    let word = compressed >> 8;
    let bit = (compressed - (word << 8)) as u8;
    (word, bit)
}

/// V4 `Pool.State` base slot for `pool_id` (= slot of packed slot0; liquidity at `+3`).
pub fn v4_pool_state_slot(pool_id: &[u8; 32]) -> U256 {
    keccak_two_words(pool_id, &U256::from(V4_POOLS_SLOT).to_be_bytes::<32>())
}

/// Storage slot of V4 `ticks[tick]` under `base` (= [`v4_pool_state_slot`]), 3-slot struct.
pub fn v4_ticks_mapping_slot(base: U256, tick: i32) -> U256 {
    keccak_two_words(
        &int_word(tick as i64),
        &(base + U256::from(4u8)).to_be_bytes::<32>(),
    )
}

/// Storage slot of V4 `tickBitmap[word]` under `base`.
pub fn v4_tick_bitmap_slot(base: U256, word: i32) -> U256 {
    keccak_two_words(
        &int_word(word as i64),
        &(base + U256::from(5u8)).to_be_bytes::<32>(),
    )
}

/// The nearest `per_side` initialized ticks at-or-below and above `current_tick`
/// (nearest-first within each side).
fn nearest_ticks(initialised: &[i32], current_tick: i32, per_side: usize) -> Vec<i32> {
    let mut below: Vec<i32> = initialised
        .iter()
        .copied()
        .filter(|t| *t <= current_tick)
        .collect();
    let mut above: Vec<i32> = initialised
        .iter()
        .copied()
        .filter(|t| *t > current_tick)
        .collect();
    below.sort_unstable_by(|a, b| b.cmp(a));
    above.sort_unstable();
    below.truncate(per_side);
    above.truncate(per_side);
    below.extend(above);
    below
}

/// Full prewarm slot list for one UniswapV3-layout pool: base slots (slot0, fee growths,
/// protocolFees, liquidity), the current + adjacent observation, tickBitmap words around the
/// current tick and of the selected ticks, and the nearest `per_side` initialized ticks'
/// complete `Tick.Info` structs. Deduplicated.
pub fn v3_pool_prewarm_slots(
    current_tick: i32,
    tick_spacing: i32,
    observation_index: u16,
    initialised_ticks: &[i32],
    per_side: usize,
) -> Vec<U256> {
    let mut out: Vec<U256> = Vec::with_capacity(10 + per_side * 2 * 5);
    for s in 0u64..=4 {
        out.push(U256::from(s));
    }
    // Current observation + the adjacent one a same-block write would advance into (cardinality
    // wrap warms at worst one unused slot — harmless).
    out.push(U256::from(V3_OBSERVATIONS_SLOT + observation_index as u64));
    out.push(U256::from(
        V3_OBSERVATIONS_SLOT + observation_index as u64 + 1,
    ));
    let (w, _) = compress_word_bit(current_tick, tick_spacing);
    for word in [w - 1, w, w + 1] {
        out.push(v3_tick_bitmap_slot(word));
    }
    for t in nearest_ticks(initialised_ticks, current_tick, per_side) {
        let first = v3_ticks_mapping_slot(t);
        for off in 0..V3_TICK_INFO_SLOTS {
            out.push(first + U256::from(off));
        }
        let (tw, _) = compress_word_bit(t, tick_spacing);
        out.push(v3_tick_bitmap_slot(tw));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Full prewarm slot list for one V4 pool on the PoolManager: state base slots (slot0, fee
/// growths, liquidity), tickBitmap words around the current tick and of the selected ticks,
/// and the nearest `per_side` initialized ticks' 3-slot `TickInfo` structs. Deduplicated.
pub fn v4_pool_prewarm_slots(
    pool_id: &[u8; 32],
    current_tick: i32,
    tick_spacing: i32,
    initialised_ticks: &[i32],
    per_side: usize,
) -> Vec<U256> {
    let base = v4_pool_state_slot(pool_id);
    let mut out: Vec<U256> = Vec::with_capacity(8 + per_side * 2 * 4);
    for off in 0u64..=3 {
        out.push(base + U256::from(off));
    }
    let (w, _) = compress_word_bit(current_tick, tick_spacing);
    for word in [w - 1, w, w + 1] {
        out.push(v4_tick_bitmap_slot(base, word));
    }
    for t in nearest_ticks(initialised_ticks, current_tick, per_side) {
        let first = v4_ticks_mapping_slot(base, t);
        for off in 0..V4_TICK_INFO_SLOTS {
            out.push(first + U256::from(off));
        }
        let (tw, _) = compress_word_bit(t, tick_spacing);
        out.push(v4_tick_bitmap_slot(base, tw));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Warm every slot in `slots` for `contract` through the probe. Returns the count warmed.
pub fn prewarm_slots<P: StorageProbe>(probe: &mut P, contract: [u8; 20], slots: &[U256]) -> usize {
    for s in slots {
        probe.warm_storage_slot(contract, *s);
    }
    slots.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(s: &str) -> [u8; 32] {
        let mut a = [0u8; 32];
        for (i, b) in a.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        a
    }

    fn u(s: &str) -> U256 {
        U256::from_be_bytes(h32(s))
    }

    // ---- Chain-proven vectors (Arbitrum One, 2026-07-01; see module doc) ----

    #[test]
    fn v3_ticks_slot_matches_onchain_vector() {
        // Pool 0xC696…8D0: storage at this derived slot held liquidityNet‖liquidityGross
        // byte-identical to ticks(-202510).
        assert_eq!(
            v3_ticks_mapping_slot(-202510),
            u("74ee24aed0d0652a25c415545f02af7dc66606db23ce109dc29357997cd2d8ec")
        );
        // Independent cast abi-encode+keccak vector for a positive tick.
        assert_eq!(
            v3_ticks_mapping_slot(100),
            u("ad66b8e7ab72f450ddfdaf1c5bc10e3a3fabf9f63ad8aa07b8743b93722f0a45")
        );
    }

    #[test]
    fn v3_bitmap_slot_matches_onchain_vector() {
        // Word -80 (current-tick word of the live pool): derived slot's word had bit 229 set.
        assert_eq!(
            v3_tick_bitmap_slot(-80),
            u("32d80bb380a6153715a380340a591610b4f5a62ca0809dce09eefe0e7c37ea7f")
        );
    }

    #[test]
    fn compress_word_bit_floor_semantics() {
        // Live vector: tick -202510 spacing 10 → compressed -20251 → word -80, bit 229 (bit
        // proven set on-chain in the word above).
        assert_eq!(compress_word_bit(-202510, 10), (-80, 229));
        // Negative non-multiple must FLOOR (contract: `if (tick < 0 && tick % spacing != 0) compressed--`).
        assert_eq!(compress_word_bit(-202515, 10), (-80, 228));
        // Positive: 63900/60 = 1065 → word 4, bit 41.
        assert_eq!(compress_word_bit(63900, 60), (4, 41));
        // Zero tick.
        assert_eq!(compress_word_bit(0, 10), (0, 0));
    }

    #[test]
    fn v4_state_base_slot_matches_onchain_vector() {
        // PoolManager 0x360E…FB32, poolId 0x1e5b…ca64: extsload(base) == getSlot0 packing and
        // extsload(base+3) == getLiquidity, both verified live.
        let pid = h32("1e5bc06befed2b29a7b1074cfabe8fe59da21779ad2dcd7bb62b5bc45ba9ca64");
        assert_eq!(
            v4_pool_state_slot(&pid),
            u("1558253c237d2aa5518a4bcf33b66305a16ac7dcc2ae5ad774080d40e9f38531")
        );
    }

    #[test]
    fn v4_bitmap_and_ticks_slots_match_vectors() {
        let base = u("1558253c237d2aa5518a4bcf33b66305a16ac7dcc2ae5ad774080d40e9f38531");
        // extsload(keccak(int256(4) ‖ base+5)) == StateView.getTickBitmap(pid, 4), proven live.
        assert_eq!(
            v4_tick_bitmap_slot(base, 4),
            u("96c35ddba2bfeae2987e2ac1db733ec10a56ffe096c05f178d70617cb04bb26a")
        );
        // cast abi-encode+keccak vector (int256(60) ‖ base+4).
        assert_eq!(
            v4_ticks_mapping_slot(base, 60),
            u("8c4e494ab81926b119b16b98a70c6b5d79a106c06d71ec3ed68257e5e4e4187f")
        );
    }

    // ---- Prewarm plan builders ----

    #[test]
    fn v3_prewarm_plan_covers_swap_read_set() {
        // current tick 10, spacing 10, obs index 7, ticks [-100,-50,0,50,100], 2 per side:
        // nearest ≤ current = {0, -50}; nearest > current = {50, 100}; -100 excluded.
        let slots = v3_pool_prewarm_slots(10, 10, 7, &[-100, -50, 0, 50, 100], 2);
        // Base slots 0..=4 (slot0, feeGrowth0/1, protocolFees, liquidity).
        for base in 0u64..=4 {
            assert!(
                slots.contains(&U256::from(base)),
                "missing base slot {base}"
            );
        }
        // Observations: 8+7 and 8+8.
        assert!(slots.contains(&U256::from(15u64)));
        assert!(slots.contains(&U256::from(16u64)));
        // Selected ticks: full 4-slot Tick.Info each.
        for t in [-50, 0, 50, 100] {
            let first = v3_ticks_mapping_slot(t);
            for off in 0u64..V3_TICK_INFO_SLOTS {
                assert!(
                    slots.contains(&(first + U256::from(off))),
                    "missing tick {t} slot +{off}"
                );
            }
        }
        // Excluded tick -100 must NOT be warmed.
        assert!(!slots.contains(&v3_ticks_mapping_slot(-100)));
        // Bitmap: current word (0) ± 1, plus every selected tick's word (all 0/-1 here).
        let (w, _) = compress_word_bit(10, 10);
        for word in [w - 1, w, w + 1] {
            assert!(slots.contains(&v3_tick_bitmap_slot(word)));
        }
        // Deduplicated.
        let mut sorted = slots.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), slots.len(), "plan must be deduplicated");
    }

    #[test]
    fn v4_prewarm_plan_covers_swap_read_set() {
        let pid = h32("1e5bc06befed2b29a7b1074cfabe8fe59da21779ad2dcd7bb62b5bc45ba9ca64");
        let base = v4_pool_state_slot(&pid);
        let slots = v4_pool_prewarm_slots(&pid, 63950, 60, &[63900, 64020], 2);
        // Base slots base..=base+3 (slot0, feeGrowth0/1, liquidity).
        for off in 0u64..=3 {
            assert!(
                slots.contains(&(base + U256::from(off))),
                "missing base+{off}"
            );
        }
        // Ticks: 3-slot TickInfo each (V4 has no oracle fields).
        for t in [63900, 64020] {
            let first = v4_ticks_mapping_slot(base, t);
            for off in 0u64..V4_TICK_INFO_SLOTS {
                assert!(slots.contains(&(first + U256::from(off))));
            }
            assert!(
                !slots.contains(&(first + U256::from(V4_TICK_INFO_SLOTS))),
                "V4 TickInfo is 3 slots — must not warm a 4th"
            );
        }
        // Bitmap around the current tick.
        let (w, _) = compress_word_bit(63950, 60);
        for word in [w - 1, w, w + 1] {
            assert!(slots.contains(&v4_tick_bitmap_slot(base, word)));
        }
        let mut sorted = slots.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), slots.len());
    }

    #[test]
    fn nearest_ticks_selects_per_side_and_handles_edges() {
        // Empty set → empty. One-sided sets → only that side, capped by availability.
        let slots = v3_pool_prewarm_slots(0, 10, 0, &[], 2);
        // Still warms base + observation + current bitmap words even with no ticks.
        assert!(slots.contains(&U256::ZERO));
        let above_only = v3_pool_prewarm_slots(0, 10, 0, &[20, 40, 60], 2);
        assert!(above_only.contains(&v3_ticks_mapping_slot(20)));
        assert!(above_only.contains(&v3_ticks_mapping_slot(40)));
        assert!(!above_only.contains(&v3_ticks_mapping_slot(60)));
    }

    #[test]
    fn prewarm_slots_warms_each_via_probe() {
        struct Rec(Vec<([u8; 20], U256)>);
        impl StorageProbe for Rec {
            fn token_balance_of(&mut self, _t: [u8; 20], _h: [u8; 20]) -> Option<U256> {
                None
            }
            fn read_storage_slot(&mut self, _c: [u8; 20], _s: U256) -> U256 {
                U256::ZERO
            }
            fn warm_storage_slot(&mut self, c: [u8; 20], s: U256) {
                self.0.push((c, s));
            }
        }
        let mut rec = Rec(Vec::new());
        let n = prewarm_slots(&mut rec, [0xAB; 20], &[U256::from(1u8), U256::from(9u8)]);
        assert_eq!(n, 2);
        assert_eq!(
            rec.0,
            vec![([0xAB; 20], U256::from(1u8)), ([0xAB; 20], U256::from(9u8))]
        );
    }
}
