//! Calldata builders and route kinds.
//!
//! The live engine targets the on-chain `MantisExecutor` (ground truth:
//! `engine/contracts/src/MantisExecutor.sol`). Its mixed-family entrypoints take the full
//! per-call route inline as a `TradeHopV2[3]` / `TradeHopV2[4]` payload plus `amountIn` and
//! `minNetProfit`:
//!   - `flashLoanArbMixed` / `flashLoanArbMixed4` — Balancer V2 flash loan (0%) or DEX flash-swap.
//!   - `flashLoanArbAaveMixed` / `flashLoanArbAaveMixed4` — Aave V3 funding (required when the route
//!     trades a Balancer pool, since the Balancer Vault is non-reentrant).
//!
//! Built by [`encode_mantis_mixed`] / [`encode_mantis_mixed4`]; selectors are pinned against keccak
//! in the unit tests. [`RouteKind`] remains as a topology classifier for route enumeration.

use alloy_primitives::{Address, U256};

// ============================================================================
// Mantis executor ABI (per-call TradeHopV2[3] payload, two funding entrypoints).
// Ground truth: engine/contracts/src/MantisExecutor.sol. Selectors verified by
// `mantis_selectors_match_keccak` below (and cross-checked with `cast sig`).
// ============================================================================

/// `flashLoanArbMixed(uint128,(uint8,address,address,uint24,address,uint8,uint8,int24,address,address,bytes32)[3],uint128)`.
/// Balancer V2 flash loan (0% fee) or DEX flash-swap funding. Use when the route does NOT
/// trade a Balancer pool. Selector `0xfe95fbbd`.
pub const FLASH_LOAN_ARB_MIXED_SELECTOR: [u8; 4] = [0xfe, 0x95, 0xfb, 0xbd];

/// `flashLoanArbAaveMixed(uint128,(uint8,address,address,uint24,address,uint8,uint8,int24,address,address,bytes32)[3],uint128)`.
/// Aave V3 flashLoanSimple funding (0.05% premium). REQUIRED when the route trades a Balancer
/// pool (the Balancer Vault is non-reentrant). Selector `0x28747b75`.
pub const FLASH_LOAN_ARB_AAVE_MIXED_SELECTOR: [u8; 4] = [0x28, 0x74, 0x7b, 0x75];

/// Full Solidity signature strings (used by the keccak selector test — the single source
/// of truth that the byte constants above are correct).
pub const FLASH_LOAN_ARB_MIXED_SIG: &str =
    "flashLoanArbMixed(uint128,(uint8,address,address,uint24,address,uint8,uint8,int24,address,address,bytes32)[3],uint128)";
/// See [`FLASH_LOAN_ARB_MIXED_SIG`].
pub const FLASH_LOAN_ARB_AAVE_MIXED_SIG: &str =
    "flashLoanArbAaveMixed(uint128,(uint8,address,address,uint24,address,uint8,uint8,int24,address,address,bytes32)[3],uint128)";

/// Encoded calldata length for a Mantis mixed call: selector(4) + amountIn(32) +
/// 3 * TradeHopV2(11 words * 32) + minNetProfit(32) = 4 + 32 + 1056 + 32 = 1124.
pub const MANTIS_MIXED_CALLDATA_LEN: usize = 4 + 32 + 3 * (11 * 32) + 32;

/// Smallest hop count the Mantis executor has a calldata entrypoint for (a 2-hop cycle).
pub const MANTIS_MIN_HOPS: usize = 2;
/// Largest hop count the Mantis executor has a calldata entrypoint for. The mixed entrypoints
/// carry a `TradeHopV2[3]` (2/3-hop) or `TradeHopV2[4]` (4-hop) — there is NO `[5..8]` variant,
/// so routes longer than this are catalog/provenance-only and have no submittable calldata.
/// Submission MUST fail-closed (return None) outside `MANTIS_MIN_HOPS..=MANTIS_MAX_HOPS`.
pub const MANTIS_MAX_HOPS: usize = 4;

/// One Mantis `TradeHopV2` leg. Field order MIRRORS the Solidity struct exactly
/// (see `MantisExecutor.sol` `struct TradeHopV2`). All fields are static, so the
/// struct (and the `[3]` array) ABI-encode inline with no offset words.
#[derive(Debug, Clone, Copy)]
pub struct MantisHop {
    /// Mantis exchange id (UniswapV3=0, Camelot=1, Sushi=2, … DODO=23, UniV2=24, SushiV3=25, Balancer=26).
    pub exchange_id: u8,
    /// ERC-20 address sent into this hop.
    pub token_in: Address,
    /// ERC-20 address received from this hop.
    pub token_out: Address,
    /// V3-family fee tier (uint24). 0 for venues whose router resolves by token pair.
    pub fee: u32,
    /// Explicit pool address (DODO/Fluid/Maverick/Curve/Balancer); zero for router-resolved venues.
    pub pool: Address,
    /// Curve `i` coin index.
    pub coin_in_idx: u8,
    /// Curve `j` coin index.
    pub coin_out_idx: u8,
    /// Uniswap V4 tick spacing (int24); 0 otherwise.
    pub tick_spacing: i32,
    /// Uniswap V4 hooks; zero otherwise.
    pub hooks: Address,
    /// Uniswap V4 PoolManager; zero otherwise.
    pub pool_manager: Address,
    /// Balancer poolId / Uniswap V4 poolId; zero otherwise.
    pub pool_id: [u8; 32],
}

impl Default for MantisHop {
    fn default() -> Self {
        Self {
            exchange_id: 0,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            fee: 0,
            pool: Address::ZERO,
            coin_in_idx: 0,
            coin_out_idx: 0,
            tick_spacing: 0,
            hooks: Address::ZERO,
            pool_manager: Address::ZERO,
            pool_id: [0u8; 32],
        }
    }
}

/// Largest value the Solidity `TradeHopV2.fee` field (`uint24`) can hold. A Rust `fee: u32`
/// above this would silently wrap when ABI-encoded into the 24-bit slot (only the low 3 bytes
/// survive), so the calldata builder MUST reject it rather than emit a truncated — and thus
/// wrong-pool / wrong-fee-tier — hop. Fail-closed: skip the route, never broadcast a mangled fee.
pub const MANTIS_HOP_FEE_MAX: u32 = 0x00FF_FFFF;

/// Whether `fee` fits the on-chain `uint24` field without truncation. Callers building hops
/// MUST gate on this before encoding (see [`MANTIS_HOP_FEE_MAX`]).
#[inline]
pub fn hop_fee_fits_uint24(fee: u32) -> bool {
    fee <= MANTIS_HOP_FEE_MAX
}

#[inline]
fn put_word_u128(out: &mut Vec<u8>, v: u128) {
    out.extend_from_slice(&U256::from(v).to_be_bytes::<32>());
}

#[inline]
fn put_word_addr(out: &mut Vec<u8>, a: Address) {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    out.extend_from_slice(&w);
}

#[inline]
fn put_word_i24(out: &mut Vec<u8>, v: i32) {
    // Sign-extend int24 to a 256-bit two's-complement word.
    let mut w = if v < 0 { [0xffu8; 32] } else { [0u8; 32] };
    let b = (v as i64).to_be_bytes(); // 8 bytes, two's complement
    w[24..].copy_from_slice(&b);
    out.extend_from_slice(&w);
}

fn encode_hop(out: &mut Vec<u8>, h: &MantisHop) {
    put_word_u128(out, h.exchange_id as u128);
    put_word_addr(out, h.token_in);
    put_word_addr(out, h.token_out);
    put_word_u128(out, h.fee as u128);
    put_word_addr(out, h.pool);
    put_word_u128(out, h.coin_in_idx as u128);
    put_word_u128(out, h.coin_out_idx as u128);
    put_word_i24(out, h.tick_spacing);
    put_word_addr(out, h.hooks);
    put_word_addr(out, h.pool_manager);
    out.extend_from_slice(&h.pool_id);
}

/// Funding source for a Mantis call — mirrors the engine's `touches_balancer` rule
/// (`pipeline.rs::route_flash_fee_wei`): a route trading a Balancer pool MUST fund via Aave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MantisFunding {
    /// Balancer V2 flash loan (0%) / DEX flash-swap. `flashLoanArbMixed`.
    BalancerOrFlashSwap,
    /// Aave V3 flashLoanSimple (0.05%). `flashLoanArbAaveMixed`. Required for Balancer-leg routes.
    Aave,
}

impl MantisFunding {
    /// The 4-byte selector for this funding choice.
    #[inline]
    pub fn selector(self) -> [u8; 4] {
        match self {
            MantisFunding::BalancerOrFlashSwap => FLASH_LOAN_ARB_MIXED_SELECTOR,
            MantisFunding::Aave => FLASH_LOAN_ARB_AAVE_MIXED_SELECTOR,
        }
    }

    /// Select the funding entrypoint from the route's `touches_balancer` flag.
    /// MUST mirror `route_flash_fee_wei` (Aave 0.05% iff touches_balancer, else 0%).
    #[inline]
    pub fn from_touches_balancer(touches_balancer: bool) -> Self {
        if touches_balancer {
            MantisFunding::Aave
        } else {
            MantisFunding::BalancerOrFlashSwap
        }
    }
}

/// Encode a Mantis `flashLoanArb{,Aave}Mixed(uint128 amountIn, TradeHopV2[3] hops, uint128 minNetProfit)`
/// call. Hand-rolled ABI encoding (all args static → head-only, no offsets), matching
/// Solidity `abi.encodeWithSelector` exactly. Verified on-fork against the Solidity struct path.
pub fn encode_mantis_mixed(
    funding: MantisFunding,
    amount_in: u128,
    hops: &[MantisHop; 3],
    min_net_profit: u128,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(MANTIS_MIXED_CALLDATA_LEN);
    out.extend_from_slice(&funding.selector());
    put_word_u128(&mut out, amount_in);
    for h in hops.iter() {
        encode_hop(&mut out, h);
    }
    put_word_u128(&mut out, min_net_profit);
    debug_assert_eq!(out.len(), MANTIS_MIXED_CALLDATA_LEN);
    out
}

/// `flashLoanArbMixed4(uint128,TradeHopV2[4],uint128)` selector (verified via `cast sig`).
pub const FLASH_LOAN_ARB_MIXED4_SELECTOR: [u8; 4] = [0x29, 0x64, 0x9a, 0xbb];
/// `flashLoanArbAaveMixed4(uint128,TradeHopV2[4],uint128)` selector (verified via `cast sig`).
pub const FLASH_LOAN_ARB_AAVE_MIXED4_SELECTOR: [u8; 4] = [0x0f, 0x4c, 0xcc, 0xb9];
/// Encoded 4-hop calldata length: `selector(4) + amountIn(32) + 4 * TradeHopV2(11*32) + minNetProfit(32)`.
pub const MANTIS_MIXED4_CALLDATA_LEN: usize = 4 + 32 + 4 * (11 * 32) + 32;

/// EXACT ABI-encoded Mantis submit calldata length for a `hop_count`-hop route — the single source
/// of truth the submit-side L1 (Brotli) data-fee floor must use. 2- and 3-hop ride the `[3]` encoder
/// (`encode_mantis_mixed`, [`MANTIS_MIXED_CALLDATA_LEN`]); 4-hop uses the `[4]` encoder
/// (`encode_mantis_mixed4`, [`MANTIS_MIXED4_CALLDATA_LEN`], +352 bytes). Feeding the 3-hop length to
/// a 4-hop route undercounts the L1 data fee in `minNetProfit`, so a 4-hop can clear the floor while
/// being net-negative after the true fee. `mantis_calldata_len_matches_built_encoders` pins this
/// against the real encoders so the constants can't silently drift.
#[inline]
pub const fn mantis_calldata_len(hop_count: usize) -> usize {
    if hop_count >= 4 {
        MANTIS_MIXED4_CALLDATA_LEN
    } else {
        MANTIS_MIXED_CALLDATA_LEN
    }
}

/// Encode a Mantis 4-hop `flashLoanArbMixed4 / flashLoanArbAaveMixed4(uint128, TradeHopV2[4],
/// uint128)`. Same static head-only layout as the 3-hop encoder (a `TradeHopV2[4]` is a fixed
/// static array — 4 inline hops, no offsets). 3-hop routes can ride this with `hops[3]` empty.
pub fn encode_mantis_mixed4(
    funding: MantisFunding,
    amount_in: u128,
    hops: &[MantisHop; 4],
    min_net_profit: u128,
) -> Vec<u8> {
    let selector = match funding {
        MantisFunding::BalancerOrFlashSwap => FLASH_LOAN_ARB_MIXED4_SELECTOR,
        MantisFunding::Aave => FLASH_LOAN_ARB_AAVE_MIXED4_SELECTOR,
    };
    let mut out = Vec::with_capacity(MANTIS_MIXED4_CALLDATA_LEN);
    out.extend_from_slice(&selector);
    put_word_u128(&mut out, amount_in);
    for h in hops.iter() {
        encode_hop(&mut out, h);
    }
    put_word_u128(&mut out, min_net_profit);
    debug_assert_eq!(out.len(), MANTIS_MIXED4_CALLDATA_LEN);
    out
}

/// Executor route kinds. Append-only.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// Two-hop V2/V2 with Balancer V2 flash loan.
    V2V2FlashBalancer = 0,
    /// Two-hop V2/V3.
    V2V3FlashBalancer = 1,
    /// Two-hop V3/V2.
    V3V2FlashBalancer = 2,
    /// Two-hop V3/V3.
    V3V3FlashBalancer = 3,
    /// Three-hop V2/V3/V2 triangle.
    V2V3V2TriangleFlashBalancer = 4,
    /// Three-hop V3/V2/V3 triangle.
    V3V2V3TriangleFlashBalancer = 5,
    /// Self-extend V3/V3 → V3/V3 (§24).
    V3V3SelfExtendV3V3 = 6,
    /// Self-extend V2/V3 → V2/V3.
    V2V3SelfExtendV2V3 = 7,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keccak_selector(sig: &str) -> [u8; 4] {
        use tiny_keccak::{Hasher, Keccak};
        let mut k = Keccak::v256();
        k.update(sig.as_bytes());
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        [out[0], out[1], out[2], out[3]]
    }

    #[test]
    fn hop_fee_uint24_bound() {
        // uint24 max is 16_777_215 (0x00FF_FFFF). Normal V3 fee tiers fit; 2^24 does not.
        assert_eq!(MANTIS_HOP_FEE_MAX, 16_777_215);
        assert!(hop_fee_fits_uint24(3000)); // 0.30% tier
        assert!(hop_fee_fits_uint24(MANTIS_HOP_FEE_MAX)); // boundary fits
        assert!(!hop_fee_fits_uint24(0x0100_0000)); // 2^24 truncates → rejected
        assert!(!hop_fee_fits_uint24(u32::MAX));
    }

    #[test]
    fn mantis_mixed_selector_matches_keccak() {
        assert_eq!(
            FLASH_LOAN_ARB_MIXED_SELECTOR,
            keccak_selector(FLASH_LOAN_ARB_MIXED_SIG)
        );
    }

    #[test]
    fn mantis_aave_mixed_selector_matches_keccak() {
        assert_eq!(
            FLASH_LOAN_ARB_AAVE_MIXED_SELECTOR,
            keccak_selector(FLASH_LOAN_ARB_AAVE_MIXED_SIG)
        );
    }

    #[test]
    fn mantis_calldata_len_matches_built_encoders() {
        // The submit-side L1-fee floor uses mantis_calldata_len(hop_count); it MUST equal the byte
        // length the real encoders produce, or 4-hop routes undercount the Arbitrum L1 data fee.
        let hops3 = [MantisHop::default(); 3];
        let hops4 = [MantisHop::default(); 4];
        let cd3 = encode_mantis_mixed(MantisFunding::BalancerOrFlashSwap, 1, &hops3, 1);
        let cd4 = encode_mantis_mixed4(MantisFunding::BalancerOrFlashSwap, 1, &hops4, 1);

        // 2- and 3-hop both ride the [3] encoder (same length); 4-hop is the longer [4] encoder.
        assert_eq!(mantis_calldata_len(2), cd3.len());
        assert_eq!(mantis_calldata_len(3), cd3.len());
        assert_eq!(mantis_calldata_len(4), cd4.len());
        // The 4-hop calldata is exactly one extra TradeHopV2 word-group (11 * 32 = 352 bytes) longer
        // — the bytes whose L1 fee the old fixed-3-hop floor silently dropped.
        assert_eq!(mantis_calldata_len(4) - mantis_calldata_len(3), 11 * 32);
    }

    #[test]
    fn funding_selection_mirrors_touches_balancer() {
        // Aave iff the route trades a Balancer pool; else Balancer/DEX-flash. Mirrors route_flash_fee_wei.
        assert_eq!(
            MantisFunding::from_touches_balancer(true),
            MantisFunding::Aave
        );
        assert_eq!(
            MantisFunding::from_touches_balancer(false),
            MantisFunding::BalancerOrFlashSwap
        );
        assert_eq!(
            MantisFunding::Aave.selector(),
            FLASH_LOAN_ARB_AAVE_MIXED_SELECTOR
        );
        assert_eq!(
            MantisFunding::BalancerOrFlashSwap.selector(),
            FLASH_LOAN_ARB_MIXED_SELECTOR
        );
    }

    #[test]
    fn encode_mantis_mixed_abi_shape() {
        // A 2-hop route (hop[2] empty): WETH -> USDC (UniV3 fee 500) -> WETH (DODO pool).
        let weth = Address::from([
            0x82u8, 0xaF, 0x49, 0x44, 0x7D, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xBD, 0x0d, 0x56, 0xf3,
            0x52, 0x41, 0x52, 0x3f, 0xBa, 0xb1,
        ]);
        let usdc = Address::from([
            0xafu8, 0x88, 0xd0, 0x65, 0xe7, 0x7c, 0x8c, 0xC2, 0x23, 0x93, 0x27, 0xC5, 0xED, 0xb3,
            0xA4, 0x32, 0x26, 0x8e, 0x58, 0x31,
        ]);
        let pool = Address::from([
            0x0Du8, 0x32, 0xA3, 0xbB, 0x04, 0xe8, 0xfF, 0x29, 0xf0, 0x97, 0x4C, 0x9D, 0x3F, 0x3d,
            0x85, 0x01, 0x2A, 0x0b, 0x5D, 0x14,
        ]);
        let mut hops = [MantisHop::default(); 3];
        hops[0] = MantisHop {
            exchange_id: 0,
            token_in: weth,
            token_out: usdc,
            fee: 500,
            ..Default::default()
        };
        hops[1] = MantisHop {
            exchange_id: 23,
            token_in: usdc,
            token_out: weth,
            pool,
            ..Default::default()
        };

        let cd = encode_mantis_mixed(
            MantisFunding::BalancerOrFlashSwap,
            50_000_000_000_000_000u128,
            &hops,
            7u128,
        );
        assert_eq!(cd.len(), MANTIS_MIXED_CALLDATA_LEN);
        assert_eq!(&cd[0..4], &FLASH_LOAN_ARB_MIXED_SELECTOR);

        let word = |i: usize| {
            let s = 4 + i * 32;
            let mut b = [0u8; 32];
            b.copy_from_slice(&cd[s..s + 32]);
            b
        };
        // word 0 = amountIn
        assert_eq!(
            U256::from_be_bytes(word(0)),
            U256::from(50_000_000_000_000_000u128)
        );
        // hop[0]: word1=exchangeId(0), word2=tokenIn(WETH), word3=tokenOut(USDC), word4=fee(500)
        assert_eq!(U256::from_be_bytes(word(1)), U256::ZERO);
        assert_eq!(&word(2)[12..], weth.as_slice());
        assert_eq!(&word(3)[12..], usdc.as_slice());
        assert_eq!(U256::from_be_bytes(word(4)), U256::from(500u64));
        // hop[1] starts at word 1+11=12: exchangeId(23) at word12, pool at word16 (12+4)
        assert_eq!(U256::from_be_bytes(word(12)), U256::from(23u64));
        assert_eq!(&word(16)[12..], pool.as_slice());
        // Aave variant flips only the selector.
        let cd_aave = encode_mantis_mixed(MantisFunding::Aave, 1, &hops, 0);
        assert_eq!(&cd_aave[0..4], &FLASH_LOAN_ARB_AAVE_MIXED_SELECTOR);
    }
}
