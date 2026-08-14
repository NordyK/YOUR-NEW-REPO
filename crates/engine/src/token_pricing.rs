//! On-chain token → ETH pricing, derived from the pools we snapshot at startup.
//!
//! Replaces the previous hardcoded `symbol => USD price` map in `startup.rs`. The
//! prior approach went stale (hand-guessed LINK/GMX prices, a fixed 2058 ETH anchor)
//! and silently mis-valued any pool whose token wasn't in the literal map.
//!
//! Instead we read the live reserves/sqrt-prices of the pools that just passed the
//! §5.4 snapshot gate and solve each registered token's price *in ETH* (full-unit
//! terms, i.e. how many ETH one whole token is worth):
//!
//! 1. WETH → identity (1.0).
//! 2. Direct WETH pair → spot from the **deepest** registered pool (max WETH-side
//!    reserve/liquidity).
//! 3. 2-hop via a hub already priced to WETH (USDC/USDt0/USDC.e/WBTC/…): the
//!    iterate-to-fixed-point loop lets multi-hop tokens (the LST cluster) resolve
//!    once their hub is priced.
//! 4. Plausibility guard + a small static anchor fallback for any token with no
//!    on-chain route — fail-open at init, never panic.
//!
//! The ETH/USD anchor (used only for the §5.5 USD liquidity floor) is read from the
//! WETH/USDC pool with the same plausibility bounds; it falls back to a numeric
//! default only when no WETH/USDC pool is available.

use ahash::{AHashMap, AHashSet};

/// Wrapped-ETH on Arbitrum One. The pricing root: 1 WETH == 1 ETH.
pub const WETH: [u8; 20] = [
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
];

/// `2^96` as f64, for V3 sqrtPriceX96 → price decoding.
const Q96_F64: f64 = 79_228_162_514_264_337_593_543_950_336.0;

/// Plausibility band for a token's ETH price (full-unit terms). A token worth more
/// than 1e6 ETH or less than 1e-12 ETH per whole unit is almost certainly a decimals
/// bug or a near-dead pool quoting garbage; reject it and let the fallback anchor (or
/// "unpriced") handle it. WBTC (~30–40 ETH) and dust tokens both fit comfortably.
const ETH_PRICE_MIN: f64 = 1e-12;
const ETH_PRICE_MAX: f64 = 1e6;

/// Plausibility band for the ETH/USD anchor. Catches a flipped/garbage WETH-USDC pool.
const ETH_USD_MIN: f64 = 100.0;
const ETH_USD_MAX: f64 = 1_000_000.0;

/// Price-relevant projection of a snapshotted pool. `startup.rs` builds these from its
/// local `PoolWithSnapshot` so this module stays independent of the (function-local)
/// snapshot enum. Reserves/decimals are carried raw; the math here normalizes.
#[derive(Clone, Copy, Debug)]
pub enum PricePool {
    /// Constant-product reserves in raw token units, with each token's decimals.
    V2 {
        /// Lower-address token.
        token0: [u8; 20],
        /// Higher-address token.
        token1: [u8; 20],
        /// `token0` ERC-20 decimals.
        dec0: u8,
        /// `token1` ERC-20 decimals.
        dec1: u8,
        /// `token0` reserve in raw units.
        reserve0: u128,
        /// `token1` reserve in raw units.
        reserve1: u128,
    },
    /// Concentrated liquidity: `sqrtPriceX96` (as f64), active liquidity, decimals.
    V3 {
        /// Lower-address token.
        token0: [u8; 20],
        /// Higher-address token.
        token1: [u8; 20],
        /// `token0` ERC-20 decimals.
        dec0: u8,
        /// `token1` ERC-20 decimals.
        dec1: u8,
        /// `sqrt(price) * 2^96`, already widened to f64 (lossy but fine for a spot rate).
        sqrt_price_x96: f64,
        /// Active liquidity at the current tick (depth proxy).
        liquidity: u128,
    },
    /// Curve StableSwap-NG pool — spot priced by probing `get_dy` at a small amount.
    /// Carries the kernel state plus per-coin addresses/decimals (BPT excluded).
    Curve {
        /// Coin address at each kernel index `0..kernel.n_coins`.
        tokens: [[u8; 20]; engine_amm::curve::MAX_COINS],
        /// Coin ERC-20 decimals at each kernel index.
        decimals: [u8; engine_amm::curve::MAX_COINS],
        /// StableSwap math state consumed by `get_dy`.
        kernel: engine_amm::curve::StableSwapState,
    },
    /// Balancer V2 stable pool — spot priced by probing `calc_out_given_in`.
    Balancer {
        /// Token address at each kernel index `0..kernel.n`.
        tokens: [[u8; 20]; engine_amm::balancer_stable::MAX_TOKENS],
        /// Token ERC-20 decimals at each kernel index.
        decimals: [u8; engine_amm::balancer_stable::MAX_TOKENS],
        /// StableMath state consumed by `calc_out_given_in`.
        kernel: engine_amm::balancer_stable::BalancerStableState,
    },
}

impl PricePool {
    /// Coins held by this pool (length-bounded slice into a fixed buffer).
    #[inline]
    fn coins(&self) -> &[[u8; 20]] {
        match self {
            PricePool::V2 { .. } | PricePool::V3 { .. } => &[],
            PricePool::Curve { tokens, kernel, .. } => &tokens[..kernel.n_coins.min(tokens.len())],
            PricePool::Balancer { tokens, kernel, .. } => &tokens[..kernel.n.min(tokens.len())],
        }
    }

    /// Does this pool pair `a` with `b` (either order)? For multi-coin pools, true when
    /// both `a` and `b` are among the coins.
    #[inline]
    fn pairs(&self, a: &[u8; 20], b: &[u8; 20]) -> bool {
        match self {
            PricePool::V2 { token0, token1, .. } | PricePool::V3 { token0, token1, .. } => {
                (token0 == a && token1 == b) || (token0 == b && token1 == a)
            }
            _ => {
                let c = self.coins();
                c.contains(a) && c.contains(b)
            }
        }
    }

    /// All coins paired with `tok` inside this pool. For a 2-token pool that's the single
    /// other token; for a multi-coin stable pool it's every other coin. Empty if `tok` is
    /// not in the pool. Built once per token at boot, so a `Vec` is fine.
    #[inline]
    fn counterparties(&self, tok: &[u8; 20]) -> Vec<[u8; 20]> {
        let mut out = Vec::new();
        match self {
            PricePool::V2 { token0, token1, .. } | PricePool::V3 { token0, token1, .. } => {
                if token0 == tok {
                    out.push(*token1);
                } else if token1 == tok {
                    out.push(*token0);
                }
            }
            _ => {
                let c = self.coins();
                if c.contains(tok) {
                    out.extend(c.iter().filter(|t| *t != tok).copied());
                }
            }
        }
        out
    }
}

/// Spot price of `target` denominated in `other`, in **full-token** units: how many
/// `other` one whole `target` is worth. Returns `None` if the pool can't quote (zero
/// reserves / liquidity / degenerate sqrt price).
///
/// V2: `price = (reserve_other / 10^dec_other) / (reserve_target / 10^dec_target)`.
/// V3: `sqrtPriceX96` encodes `token1/token0` (raw units); we decode to a full-unit
/// `token1-per-token0` and invert if `target` is token1.
fn spot_in(pool: &PricePool, target: &[u8; 20], other: &[u8; 20]) -> Option<f64> {
    match *pool {
        PricePool::V2 {
            token0,
            token1,
            dec0,
            dec1,
            reserve0,
            reserve1,
        } => {
            if reserve0 == 0 || reserve1 == 0 {
                return None;
            }
            let r0 = reserve0 as f64 / 10f64.powi(dec0 as i32);
            let r1 = reserve1 as f64 / 10f64.powi(dec1 as i32);
            if &token0 == target && &token1 == other {
                Some(r1 / r0)
            } else if &token1 == target && &token0 == other {
                Some(r0 / r1)
            } else {
                None
            }
        }
        PricePool::V3 {
            token0,
            token1,
            dec0,
            dec1,
            sqrt_price_x96,
            liquidity,
        } => {
            if sqrt_price_x96 <= 0.0 || liquidity == 0 {
                return None;
            }
            let sp = sqrt_price_x96 / Q96_F64;
            // raw price = token1_raw per token0_raw
            let raw_price = sp * sp;
            // Convert to full-token price: token1_full per token0_full.
            // raw_price has units (token1_raw/token0_raw); multiply by
            // 10^dec0 / 10^dec1 to get full-token ratio.
            let price_1_per_0 = raw_price * 10f64.powi(dec0 as i32 - dec1 as i32);
            if !price_1_per_0.is_finite() || price_1_per_0 <= 0.0 {
                return None;
            }
            if &token0 == target && &token1 == other {
                // want other(token1) per target(token0) = price_1_per_0
                Some(price_1_per_0)
            } else if &token1 == target && &token0 == other {
                Some(1.0 / price_1_per_0)
            } else {
                None
            }
        }
        PricePool::Curve {
            tokens,
            decimals,
            kernel,
        } => {
            let n = kernel.n_coins;
            let i = (0..n).find(|&k| &tokens[k] == target)?;
            let j = (0..n).find(|&k| &tokens[k] == other)?;
            // Probe a small `target` amount so we read the marginal (spot) rate, not a
            // size-impacted one. Use balance/10000, floored at 1 raw unit.
            let dx = (kernel.balances[i] / 10_000).max(1);
            let dy = engine_amm::curve::get_dy(&kernel, i, j, dx).ok()?;
            stable_spot_full(dx, dy, decimals[i], decimals[j])
        }
        PricePool::Balancer {
            tokens,
            decimals,
            kernel,
        } => {
            let n = kernel.n;
            let i = (0..n).find(|&k| &tokens[k] == target)?;
            let j = (0..n).find(|&k| &tokens[k] == other)?;
            let dx = (kernel.balances[i] / 10_000).max(1);
            let dy = engine_amm::balancer_stable::calc_out_given_in(&kernel, i, j, dx).ok()?;
            stable_spot_full(dx, dy, decimals[i], decimals[j])
        }
    }
}

/// Full-token spot from a raw (dx in → dy out) probe: `(dy/10^dec_out)/(dx/10^dec_in)`.
fn stable_spot_full(dx_raw: u128, dy_raw: u128, dec_in: u8, dec_out: u8) -> Option<f64> {
    if dx_raw == 0 || dy_raw == 0 {
        return None;
    }
    let in_full = dx_raw as f64 / 10f64.powi(dec_in as i32);
    let out_full = dy_raw as f64 / 10f64.powi(dec_out as i32);
    let px = out_full / in_full;
    if px.is_finite() && px > 0.0 {
        Some(px)
    } else {
        None
    }
}

/// Depth of a pool on `side`'s side, as a comparable f64 magnitude. Used to pick the
/// "deepest" pool when several quote the same pair. For V2 this is the reserve of
/// `side` (full-token); for V3 we use `liquidity` (no per-side reserve available),
/// which is monotonic in depth across that pair's pools.
fn depth_on_side(pool: &PricePool, side: &[u8; 20]) -> f64 {
    match *pool {
        PricePool::V2 {
            token0,
            dec0,
            dec1,
            reserve0,
            reserve1,
            ..
        } => {
            if &token0 == side {
                reserve0 as f64 / 10f64.powi(dec0 as i32)
            } else {
                reserve1 as f64 / 10f64.powi(dec1 as i32)
            }
        }
        PricePool::V3 { liquidity, .. } => liquidity as f64,
        PricePool::Curve {
            tokens,
            decimals,
            kernel,
        } => (0..kernel.n_coins)
            .find(|&k| &tokens[k] == side)
            .map(|k| kernel.balances[k] as f64 / 10f64.powi(decimals[k] as i32))
            .unwrap_or(0.0),
        PricePool::Balancer {
            tokens,
            decimals,
            kernel,
        } => (0..kernel.n)
            .find(|&k| &tokens[k] == side)
            .map(|k| kernel.balances[k] as f64 / 10f64.powi(decimals[k] as i32))
            .unwrap_or(0.0),
    }
}

/// ETH-notional depth of `pool` on the `hub` side: the ETH value of the hub-side
/// inventory. Used as a robust, cross-AMM-comparable weight when reconciling many
/// candidate marks for one token (a raw V2 reserve and a V3 `liquidity` are otherwise
/// not comparable). V2 uses the hub reserve directly; V3 derives the hub-side virtual
/// reserve from `L` and `sqrtP` (`x0 = L/sqrtP`, `x1 = L*sqrtP`). `hub_eth` is ETH per
/// full hub token.
fn eth_notional_depth(pool: &PricePool, hub: &[u8; 20], hub_eth: f64) -> f64 {
    let hub_full = match *pool {
        PricePool::V2 { .. } | PricePool::Curve { .. } | PricePool::Balancer { .. } => {
            depth_on_side(pool, hub)
        }
        PricePool::V3 {
            token0,
            token1,
            dec0,
            dec1,
            sqrt_price_x96,
            liquidity,
        } => {
            if sqrt_price_x96 <= 0.0 || liquidity == 0 {
                return 0.0;
            }
            let sp = sqrt_price_x96 / Q96_F64; // sqrt(price): raw token1 per raw token0
            let l = liquidity as f64;
            if &token0 == hub {
                (l / sp) / 10f64.powi(dec0 as i32)
            } else if &token1 == hub {
                (l * sp) / 10f64.powi(dec1 as i32)
            } else {
                return 0.0;
            }
        }
    };
    let v = hub_full * hub_eth;
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

/// Liquidity-weighted median of `(price, weight)` candidates. Robust to a single thin
/// pool quoting garbage: the deep pools dominate the cumulative-weight crossing point,
/// so one near-dead pool can't drag the mark. Returns `None` if no positive-weight
/// candidate survives.
fn weighted_median(mut cands: Vec<(f64, f64)>) -> Option<f64> {
    cands.retain(|&(p, w)| p.is_finite() && p > 0.0 && w.is_finite() && w > 0.0);
    if cands.is_empty() {
        return None;
    }
    cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = cands.iter().map(|&(_, w)| w).sum();
    let mut cum = 0.0;
    for &(p, w) in &cands {
        cum += w;
        if cum * 2.0 >= total {
            return Some(p);
        }
    }
    cands.last().map(|&(p, _)| p)
}

/// Derive each registered token's price in ETH (full-token units) from the snapshotted
/// pools. `decimals_by_addr` must cover every token referenced by `tokens`.
///
/// `hub_anchors` lists the non-WETH hub tokens whose ETH price we seed before the
/// 2-hop pass (typically the USD stables + WBTC). A hub still prefers an on-chain
/// direct-WETH route when one exists; the anchor is only the bootstrap value so a
/// token→hub→WETH chain can resolve when the hub itself has no direct WETH pool.
///
/// Returns `addr → eth_per_full_token`. Tokens with no plausible route are simply
/// absent from the map; `startup.rs` treats absence as "unpriced" (q96 = 0 → the
/// evaluator fails closed for that base, and the floor counts it as $0).
pub fn derive_token_eth_prices(
    pools: &[PricePool],
    tokens: &[[u8; 20]],
    decimals_by_addr: &AHashMap<[u8; 20], u8>,
    hub_anchors: &[([u8; 20], f64)],
) -> AHashMap<[u8; 20], f64> {
    let weth = WETH;
    let mut price: AHashMap<[u8; 20], f64> = AHashMap::default();

    // 1. WETH identity.
    price.insert(weth, 1.0);

    let token_set: AHashSet<[u8; 20]> = tokens.iter().copied().collect();
    let _ = decimals_by_addr; // decimals travel inside PricePool; kept for API symmetry.

    // Seed hub anchors as bootstrap estimates so token→hub→WETH chains can resolve in
    // the first round. Derived on-chain marks overwrite these as soon as a usable route
    // exists; an anchor only survives for a hub with no usable pool at all.
    for &(hub, anchor_eth) in hub_anchors {
        if token_set.contains(&hub) {
            price.entry(hub).or_insert(anchor_eth);
        }
    }

    // Robust mark derivation. Each round, re-mark every non-WETH token as the
    // liquidity-weighted median of ALL routes pairing it with an already-priced
    // counterparty — direct-WETH and 2-hop-via-priced-hub candidates compete in one
    // median, weighted by ETH-notional depth. This is robust to a thin pool quoting
    // garbage: a near-dead crvUSD/WETH pool can't drag the mark off the deep
    // crvUSD/USDC route. Iterated to a fixed point (bounded; the token graph diameter
    // is tiny). No peg assumption — a depegged or redenominated token (e.g. legacy
    // FRAX) marks at its true on-chain value, not $1.
    const DIRECT_WETH_BONUS: f64 = 2.0; // prefer direct quotes over compounded 2-hops
    const CONVERGE_EPS: f64 = 0.002; // 0.2% — stop once marks stabilize
    let max_rounds = 8usize;
    for _ in 0..max_rounds {
        let mut next: AHashMap<[u8; 20], f64> = AHashMap::default();
        for &tok in tokens.iter() {
            if tok == weth {
                continue;
            }
            let mut cands: Vec<(f64, f64)> = Vec::new();
            for p in pools {
                for hub in p.counterparties(&tok) {
                    let Some(&hub_eth) = price.get(&hub) else {
                        continue;
                    };
                    let Some(spot_hub_per_tok) = spot_in(p, &tok, &hub) else {
                        continue;
                    };
                    if !spot_hub_per_tok.is_finite() || spot_hub_per_tok <= 0.0 {
                        continue;
                    }
                    let eth = spot_hub_per_tok * hub_eth;
                    if !(ETH_PRICE_MIN..=ETH_PRICE_MAX).contains(&eth) {
                        continue;
                    }
                    let mut w = eth_notional_depth(p, &hub, hub_eth);
                    if hub == weth {
                        w *= DIRECT_WETH_BONUS;
                    }
                    if w > 0.0 {
                        cands.push((eth, w));
                    }
                }
            }
            if let Some(m) = weighted_median(cands) {
                next.insert(tok, m);
            }
        }
        // Commit and detect convergence via max relative change.
        let mut changed = false;
        for (tok, m) in next {
            match price.get(&tok) {
                Some(&old) if old > 0.0 => {
                    if ((m - old).abs() / old) > CONVERGE_EPS {
                        changed = true;
                    }
                }
                _ => changed = true,
            }
            price.insert(tok, m);
        }
        if !changed {
            break;
        }
    }

    price
}

/// Derive ETH/USD from the deepest WETH/USDC-class pool, with plausibility bounds.
/// `usd_anchors` are the addresses we treat as ≈ $1 (USDC, USDt0, USDC.e, …); the
/// first one that has a deep WETH pool wins. Fails open to `fallback` (the old 2058).
pub fn derive_eth_usd(pools: &[PricePool], usd_anchors: &[[u8; 20]], fallback: f64) -> f64 {
    let weth = WETH;
    let mut best: Option<(f64, f64)> = None; // (depth_on_weth_side, eth_usd)
    for &usd in usd_anchors {
        for p in pools {
            if !p.pairs(&weth, &usd) {
                continue;
            }
            // price of WETH in USD = how many USD per 1 WETH
            if let Some(usd_per_weth) = spot_in(p, &weth, &usd) {
                if !(ETH_USD_MIN..=ETH_USD_MAX).contains(&usd_per_weth) {
                    continue;
                }
                let depth = depth_on_side(p, &weth);
                if best.map(|(d, _)| depth > d).unwrap_or(true) {
                    best = Some((depth, usd_per_weth));
                }
            }
        }
    }
    best.map(|(_, v)| v).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(byte: u8) -> [u8; 20] {
        let mut x = [0u8; 20];
        x[19] = byte;
        x
    }

    /// WETH/USDC V2 pool → USDC priced at ~1/ethusd; a TOKEN/USDC pool resolves 2-hop.
    #[test]
    fn weth_usdc_v2_then_two_hop_token() {
        let weth = WETH;
        let usdc = a(0x11);
        let token = a(0x22); // some token only paired with USDC

        // WETH/USDC pool: 1000 WETH, 2,500,000 USDC → ETH/USD = 2500.
        // WETH 18 dec, USDC 6 dec.
        let weth_usdc = PricePool::V2 {
            token0: weth,
            token1: usdc,
            dec0: 18,
            dec1: 6,
            reserve0: 1_000u128 * 10u128.pow(18),
            reserve1: 2_500_000u128 * 10u128.pow(6),
        };
        // TOKEN/USDC pool: 10,000 TOKEN, 50,000 USDC → TOKEN = $5 → 5/2500 = 0.002 ETH.
        // TOKEN 18 dec.
        let token_usdc = PricePool::V2 {
            token0: token,
            token1: usdc,
            dec0: 18,
            dec1: 6,
            reserve0: 10_000u128 * 10u128.pow(18),
            reserve1: 50_000u128 * 10u128.pow(6),
        };

        let mut dec = AHashMap::default();
        dec.insert(weth, 18u8);
        dec.insert(usdc, 6u8);
        dec.insert(token, 18u8);

        let pools = vec![weth_usdc, token_usdc];
        let tokens = vec![weth, usdc, token];

        let prices = derive_token_eth_prices(&pools, &tokens, &dec, &[]);

        // USDC priced via direct WETH pair: 1 USDC = 1/2500 ETH = 0.0004.
        let usdc_eth = prices.get(&usdc).copied().expect("USDC priced");
        assert!(
            (usdc_eth - 0.0004).abs() < 1e-9,
            "USDC ETH price {usdc_eth} != 0.0004 (= 1/ethusd)"
        );

        // TOKEN resolves 2-hop: $5 / $2500 = 0.002 ETH.
        let token_eth = prices.get(&token).copied().expect("TOKEN priced 2-hop");
        assert!(
            (token_eth - 0.002).abs() < 1e-9,
            "TOKEN ETH price {token_eth} != 0.002"
        );

        // ETH/USD anchor from the WETH/USDC pool.
        let ethusd = derive_eth_usd(&pools, &[usdc], 2058.0);
        assert!((ethusd - 2500.0).abs() < 1e-6, "ETH/USD {ethusd} != 2500");
    }

    /// V3 direct WETH pool prices a token; decimals are handled.
    #[test]
    fn v3_direct_weth_pair() {
        let weth = WETH;
        // token0 < ... ordering doesn't matter for our address-keyed math.
        let token = a(0x33);
        // Build a V3 pool where token0=token (18 dec), token1=WETH (18 dec),
        // and price token1/token0 = 0.5 → token = 0.5 ETH.
        // sqrtPriceX96 = sqrt(0.5) * 2^96.
        let sp = (0.5f64).sqrt() * Q96_F64;
        let pool = PricePool::V3 {
            token0: token,
            token1: weth,
            dec0: 18,
            dec1: 18,
            sqrt_price_x96: sp,
            liquidity: 1_000_000_000_000u128,
        };
        let mut dec = AHashMap::default();
        dec.insert(weth, 18u8);
        dec.insert(token, 18u8);
        let prices = derive_token_eth_prices(&[pool], &[weth, token], &dec, &[]);
        let token_eth = prices.get(&token).copied().expect("token priced via V3");
        assert!(
            (token_eth - 0.5).abs() < 1e-6,
            "V3 token ETH price {token_eth} != 0.5"
        );
    }

    /// A token with no route is simply absent (fail-open: unpriced).
    #[test]
    fn unrouted_token_absent() {
        let weth = WETH;
        let orphan = a(0x44);
        let mut dec = AHashMap::default();
        dec.insert(weth, 18u8);
        dec.insert(orphan, 18u8);
        let prices = derive_token_eth_prices(&[], &[weth, orphan], &dec, &[]);
        assert!(prices.contains_key(&weth));
        assert!(!prices.contains_key(&orphan), "orphan must stay unpriced");
    }

    /// Deepest pool wins when two pools quote the same direct-WETH pair differently.
    #[test]
    fn deepest_direct_pool_wins() {
        let weth = WETH;
        let token = a(0x55);
        // Shallow pool quotes token = 1.0 ETH; deep pool quotes 0.1 ETH. Deep wins.
        let shallow = PricePool::V2 {
            token0: token,
            token1: weth,
            dec0: 18,
            dec1: 18,
            reserve0: 10u128.pow(18),
            reserve1: 10u128.pow(18),
        };
        let deep = PricePool::V2 {
            token0: token,
            token1: weth,
            dec0: 18,
            dec1: 18,
            reserve0: 10_000u128 * 10u128.pow(18),
            reserve1: 1_000u128 * 10u128.pow(18),
        };
        let mut dec = AHashMap::default();
        dec.insert(weth, 18u8);
        dec.insert(token, 18u8);
        let prices = derive_token_eth_prices(&[shallow, deep], &[weth, token], &dec, &[]);
        let px = prices.get(&token).copied().unwrap();
        assert!(
            (px - 0.1).abs() < 1e-9,
            "deep pool price 0.1 should win, got {px}"
        );
    }

    /// A thin direct-WETH pool mis-quotes a stablecoin; the deep stable 2-hop dominates
    /// the liquidity-weighted median and recovers the true mark. Regression for crvUSD /
    /// GHO marking at ~$0.5 off a near-dead WETH pool.
    #[test]
    fn thin_direct_pool_loses_to_deep_stable_route() {
        let weth = WETH;
        let usdc = a(0x11);
        let crvusd = a(0x66);

        // WETH/USDC deep → ETH/USD = 2500, so USDC = 0.0004 ETH.
        let weth_usdc = PricePool::V2 {
            token0: weth,
            token1: usdc,
            dec0: 18,
            dec1: 6,
            reserve0: 1_000u128 * 10u128.pow(18),
            reserve1: 2_500_000u128 * 10u128.pow(6),
        };
        // crvUSD/WETH THIN, mis-quotes crvUSD ≈ $0.49 (5/25510 = 0.000196 ETH).
        let crvusd_weth_thin = PricePool::V2 {
            token0: crvusd,
            token1: weth,
            dec0: 18,
            dec1: 18,
            reserve0: 25_510u128 * 10u128.pow(18),
            reserve1: 5u128 * 10u128.pow(18),
        };
        // crvUSD/USDC DEEP, true peg ≈ $1 (1 USDC per crvUSD → 0.0004 ETH).
        let crvusd_usdc_deep = PricePool::V2 {
            token0: crvusd,
            token1: usdc,
            dec0: 18,
            dec1: 6,
            reserve0: 1_000_000u128 * 10u128.pow(18),
            reserve1: 1_000_000u128 * 10u128.pow(6),
        };

        let mut dec = AHashMap::default();
        dec.insert(weth, 18u8);
        dec.insert(usdc, 6u8);
        dec.insert(crvusd, 18u8);

        let pools = vec![weth_usdc, crvusd_weth_thin, crvusd_usdc_deep];
        let tokens = vec![weth, usdc, crvusd];
        let prices = derive_token_eth_prices(&pools, &tokens, &dec, &[]);

        let crv = prices.get(&crvusd).copied().expect("crvUSD priced");
        // True ≈ 0.0004 ETH ($1); the thin pool's 0.000196 must NOT win.
        assert!(
            (crv - 0.0004).abs() < 1e-5,
            "crvUSD should mark ~0.0004 ETH via the deep stable route, got {crv}"
        );
    }

    /// crvUSD has NO V2/V3 pool — only a Curve StableSwap pool. It must still mark ~$1
    /// via the kernel-probed Curve spot. Regression for the Curve-only-liquidity gap.
    #[test]
    fn curve_only_token_marks_via_kernel() {
        let weth = WETH;
        let usdc = a(0x11);
        let crvusd = a(0x66);

        // WETH/USDC V2 → ETH/USD = 2500, USDC = 0.0004 ETH.
        let weth_usdc = PricePool::V2 {
            token0: weth,
            token1: usdc,
            dec0: 18,
            dec1: 6,
            reserve0: 1_000u128 * 10u128.pow(18),
            reserve1: 2_500_000u128 * 10u128.pow(6),
        };
        // Balanced 2-coin Curve pool crvUSD(18)/USDC(6). rates: 18-dec coin = 1e18,
        // 6-dec coin = 1e30 (see StableSwapState docs).
        let mut tokens = [[0u8; 20]; engine_amm::curve::MAX_COINS];
        tokens[0] = crvusd;
        tokens[1] = usdc;
        let mut balances = [0u128; engine_amm::curve::MAX_COINS];
        balances[0] = 1_000_000u128 * 10u128.pow(18);
        balances[1] = 1_000_000u128 * 10u128.pow(6);
        let mut rates = [0u128; engine_amm::curve::MAX_COINS];
        rates[0] = 10u128.pow(18);
        rates[1] = 10u128.pow(30);
        let mut decimals = [0u8; engine_amm::curve::MAX_COINS];
        decimals[0] = 18;
        decimals[1] = 6;
        let curve = PricePool::Curve {
            tokens,
            decimals,
            kernel: engine_amm::curve::StableSwapState {
                n_coins: 2,
                balances,
                rates,
                a_param: 200 * engine_amm::curve::A_PRECISION,
                fee: 1_000_000, // 1 bps
                offpeg_fee_multiplier: 0,
            },
        };

        let mut dec = AHashMap::default();
        dec.insert(weth, 18u8);
        dec.insert(usdc, 6u8);
        dec.insert(crvusd, 18u8);

        let prices = derive_token_eth_prices(&[weth_usdc, curve], &[weth, usdc, crvusd], &dec, &[]);
        let crv = prices
            .get(&crvusd)
            .copied()
            .expect("crvUSD priced via Curve");
        // Balanced stable pool → ~1:1 with USDC → ~0.0004 ETH.
        assert!(
            (crv - 0.0004).abs() < 2e-5,
            "crvUSD via Curve should mark ~0.0004 ETH, got {crv}"
        );
    }
}
