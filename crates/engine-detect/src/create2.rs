//! Deterministic (CREATE2) pool-address derivation for V2/V3-style factories.
//!
//! Lets the classifier wake SYNCHRONOUSLY on a router/aggregator swap that names a pool only by its
//! `(tokenA, tokenB[, fee])` arguments rather than a literal address — derive the address offline and
//! match it against the resident registry (TAIL_LATENCY_PLAN §6 E, lifting the ~0.9 % synchronous
//! wakeup ceiling). Pure + offline: `address = keccak256(0xff ++ factory ++ salt ++ init_code_hash)`,
//! where the salt encodes the (sorted) tokens (+ fee for V3). Init-code hashes come from per-venue
//! config (`init_code_hash`).

use alloy_primitives::{keccak256, U256};

/// Sort two token addresses ascending (Uniswap orders `token0 < token1`).
#[inline]
pub fn sort_tokens(a: [u8; 20], b: [u8; 20]) -> ([u8; 20], [u8; 20]) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Final CREATE2 step: `keccak256(0xff ++ factory ++ salt ++ init_code_hash)[12..]`.
fn create2_address(factory: [u8; 20], salt: [u8; 32], init_code_hash: [u8; 32]) -> [u8; 20] {
    let mut buf = [0u8; 1 + 20 + 32 + 32];
    buf[0] = 0xff;
    buf[1..21].copy_from_slice(&factory);
    buf[21..53].copy_from_slice(&salt);
    buf[53..85].copy_from_slice(&init_code_hash);
    let h = keccak256(buf);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&h.0[12..]);
    addr
}

/// Uniswap-V2-style pair address: `salt = keccak256(token0 ++ token1)` over the sorted tokens.
pub fn v2_pair_address(
    factory: [u8; 20],
    token_a: [u8; 20],
    token_b: [u8; 20],
    init_code_hash: [u8; 32],
) -> [u8; 20] {
    let (t0, t1) = sort_tokens(token_a, token_b);
    let mut salt_in = [0u8; 40];
    salt_in[..20].copy_from_slice(&t0);
    salt_in[20..].copy_from_slice(&t1);
    create2_address(factory, keccak256(salt_in).0, init_code_hash)
}

/// Uniswap-V3-style pool address: `salt = keccak256(abi.encode(token0, token1, fee))` — each value
/// padded to a 32-byte word, tokens sorted, `fee` a uint24.
pub fn v3_pool_address(
    factory: [u8; 20],
    token_a: [u8; 20],
    token_b: [u8; 20],
    fee: u32,
    init_code_hash: [u8; 32],
) -> [u8; 20] {
    let (t0, t1) = sort_tokens(token_a, token_b);
    let mut enc = [0u8; 96];
    enc[12..32].copy_from_slice(&t0);
    enc[44..64].copy_from_slice(&t1);
    enc[64..96].copy_from_slice(&U256::from(fee).to_be_bytes::<32>());
    create2_address(factory, keccak256(enc).0, init_code_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn h32(bytes: [u8; 32]) -> [u8; 32] {
        bytes
    }

    #[test]
    fn v2_pair_matches_canonical_mainnet_usdc_weth() {
        // Canonical Uniswap V2 (mainnet): factory 0x5C69bEe7…aA6f, init hash 96e8ac…845f.
        let factory: [u8; 20] = [
            0x5C, 0x69, 0xbE, 0xe7, 0x01, 0xef, 0x81, 0x4a, 0x2B, 0x6a, 0x3E, 0xDD, 0x4B, 0x16,
            0x52, 0xCB, 0x9c, 0xc5, 0xaA, 0x6f,
        ];
        let init_hash = h32([
            0x96, 0xe8, 0xac, 0x42, 0x77, 0x19, 0x8f, 0xf8, 0xb6, 0xf7, 0x85, 0x47, 0x8a, 0xa9,
            0xa3, 0x9f, 0x40, 0x3c, 0xb7, 0x68, 0xdd, 0x02, 0xcb, 0xee, 0x32, 0x6c, 0x3e, 0x7d,
            0xa3, 0x48, 0x84, 0x5f,
        ]);
        let usdc: [u8; 20] = [
            0xA0, 0xb8, 0x69, 0x91, 0xc6, 0x21, 0x8b, 0x36, 0xc1, 0xd1, 0x9D, 0x4a, 0x2e, 0x9E,
            0xb0, 0xcE, 0x36, 0x06, 0xeB, 0x48,
        ];
        let weth: [u8; 20] = [
            0xC0, 0x2a, 0xaA, 0x39, 0xb2, 0x23, 0xFE, 0x8D, 0x0A, 0x0e, 0x5C, 0x4F, 0x27, 0xeA,
            0xD9, 0x08, 0x3C, 0x75, 0x6C, 0xc2,
        ];
        // Canonical USDC/WETH V2 pair.
        let want: [u8; 20] = [
            0xB4, 0xe1, 0x6d, 0x01, 0x68, 0xe5, 0x2d, 0x35, 0xCa, 0xCD, 0x2c, 0x61, 0x85, 0xb4,
            0x42, 0x81, 0xEc, 0x28, 0xC9, 0xDc,
        ];
        assert_eq!(v2_pair_address(factory, usdc, weth, init_hash), want);
        // Token order must not matter (function sorts internally).
        assert_eq!(v2_pair_address(factory, weth, usdc, init_hash), want);
    }

    #[test]
    fn v3_pool_address_is_deterministic_and_fee_sensitive() {
        let factory = [0x11u8; 20];
        let hash = [0x22u8; 32];
        let a = [0xa0u8; 20];
        let b = [0xb0u8; 20];
        let p500 = v3_pool_address(factory, a, b, 500, hash);
        let p3000 = v3_pool_address(factory, a, b, 3000, hash);
        // Different fee tiers → different pools.
        assert_ne!(p500, p3000);
        // Order-insensitive.
        assert_eq!(p500, v3_pool_address(factory, b, a, 500, hash));
        // Different factory → different address.
        assert_ne!(p500, v3_pool_address([0x33u8; 20], a, b, 500, hash));
    }

    #[test]
    fn sort_tokens_orders_ascending() {
        let lo = [0x01u8; 20];
        let hi = [0x99u8; 20];
        assert_eq!(sort_tokens(hi, lo), (lo, hi));
        assert_eq!(sort_tokens(lo, hi), (lo, hi));
    }
}
