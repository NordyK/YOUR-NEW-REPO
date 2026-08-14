//! Storage helpers for hydrating Uniswap V3-compatible tick state.
//!
//! Canonical V3 pools store `ticks` as `mapping(int24 => Tick.Info)`. The first
//! packed storage word contains `liquidityGross` and `liquidityNet`; additional
//! words contain fee and time accumulator fields that the engine does not use
//! for quote simulation.

use tiny_keccak::{Hasher, Keccak};

/// Return the storage slot for the first packed `Tick.Info` word.
///
/// `mapping_slot` is the Solidity storage slot number for `ticks`. Canonical
/// Uniswap V3 and Pancake V3 pools use slot `5`.
pub fn v3_tick_info_word0_slot(tick: i32, mapping_slot: u64) -> [u8; 32] {
    let mut preimage = [0u8; 64];
    if tick < 0 {
        preimage[..32].fill(0xff);
    }
    let raw_tick = (tick as i64).to_be_bytes();
    preimage[29..32].copy_from_slice(&raw_tick[5..8]);
    preimage[56..64].copy_from_slice(&mapping_slot.to_be_bytes());

    let mut hasher = Keccak::v256();
    hasher.update(&preimage);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

/// Pack `Tick.Info` word 0 as Solidity stores it:
/// low 128 bits = `liquidityGross`, high 128 bits = `liquidityNet`.
pub fn pack_v3_tick_liquidity_word(liquidity_gross: u128, liquidity_net: i128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&liquidity_net.to_be_bytes());
    out[16..].copy_from_slice(&liquidity_gross.to_be_bytes());
    out
}
