//! Per-pool state slots for Curve **CryptoSwap NG** (TwoCrypto-NG / Tricrypto-NG) pools.
//!
//! Mirror of [`crate::curve_state`] but storing [`engine_amm::curve_cryptoswap::CryptoSwapState`]
//! instead of the StableSwap kernel. The split keeps the two Curve families' state tables
//! type-distinct (no enum-tag branching on the hot path) and lets the state writer commit each
//! family through its native ABI without round-tripping through a shared adapter.
//!
//! Lock-free reads via `ArcSwap<CryptoCurveSlot>`, same single-writer pattern as the rest of the
//! state crate.

use alloy_primitives::U256;
use arc_swap::ArcSwap;
use engine_amm::curve_cryptoswap::CryptoSwapState;
use engine_amm::curve_twocrypto::TwoCryptoSwapState;
use std::sync::Arc;

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};
use crate::token::Address;

/// Maximum number of coins per CryptoSwap pool — Curve's NG factory ships only 2- and 3-coin
/// variants, so 3 is the fixed-width upper bound.
pub const MAX_CRYPTO_COINS: usize = 3;

/// A registered Curve CryptoSwap NG pool: the kernel math state + the token addresses aligned
/// with the kernel's coin indices.
///
/// A pool is either 3-coin (Tricrypto-NG, `kernel: CryptoSwapState`, `twocrypto_kernel: None`,
/// `n_coins=3`) or 2-coin (TwoCrypto-NG, `twocrypto_kernel: Some`, `kernel` a placeholder,
/// `n_coins=2`). [`Self::quote_amount_out`] dispatches to whichever is present — the two kernels are
/// integer-exact against their respective Vyper sources.
#[derive(Debug, Clone)]
pub struct CryptoCurveSlot {
    /// 3-coin math state (Tricrypto-NG). A placeholder when `twocrypto_kernel` is `Some`.
    pub kernel: CryptoSwapState,
    /// 2-coin math state (TwoCrypto-NG), `Some` iff this is a 2-coin pool.
    pub twocrypto_kernel: Option<TwoCryptoSwapState>,
    /// Token address at each kernel coin index `0..n_coins`.
    pub tokens: [Address; MAX_CRYPTO_COINS],
    /// Active coin count (2 = TwoCrypto-NG, 3 = Tricrypto-NG).
    pub n_coins: u8,
}

impl CryptoCurveSlot {
    /// Resolve a token address to its kernel coin index.
    #[inline]
    pub fn index_of(&self, token: &Address) -> Option<usize> {
        (0..self.n_coins as usize).find(|&k| &self.tokens[k] == token)
    }

    /// A zeroed 3-coin kernel, used as the `kernel` placeholder for a 2-coin (TwoCrypto) slot.
    pub fn placeholder_kernel() -> CryptoSwapState {
        CryptoSwapState {
            balances: [U256::ZERO; 3],
            precisions: [U256::ZERO; 3],
            price_scale: [U256::ZERO; 2],
            ann: U256::ZERO,
            gamma: U256::ZERO,
            mid_fee: U256::ZERO,
            out_fee: U256::ZERO,
            fee_gamma: U256::ZERO,
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        }
    }

    /// Coin balances (real token-decimals units), `0..n_coins`, from whichever kernel is active.
    pub fn balances(&self) -> [U256; MAX_CRYPTO_COINS] {
        match &self.twocrypto_kernel {
            Some(tk) => [tk.balances[0], tk.balances[1], U256::ZERO],
            None => self.kernel.balances,
        }
    }

    /// Quote `i → j` for `dx` real-units via the active kernel (re-solving `D` on a working copy if
    /// it's cold). Returns `None` on kernel error or zero output. Centralizes the 2-vs-3-coin
    /// dispatch so call sites don't branch.
    pub fn quote_amount_out(&self, i: usize, j: usize, dx: U256) -> Option<U256> {
        let dy = match &self.twocrypto_kernel {
            Some(tk) => {
                if tk.d.is_zero() {
                    let mut k = tk.clone();
                    k.recompute_d().ok()?;
                    k.get_amount_out_exact(i, j, dx).ok()?.0
                } else {
                    tk.get_amount_out_exact(i, j, dx).ok()?.0
                }
            }
            None => {
                if self.kernel.d.is_zero() {
                    let mut k = self.kernel.clone();
                    k.recompute_d().ok()?;
                    k.get_amount_out_exact(i, j, dx).ok()?.0
                } else {
                    self.kernel.get_amount_out_exact(i, j, dx).ok()?.0
                }
            }
        };
        (!dy.is_zero()).then_some(dy)
    }

    /// Forward-apply a swap `i → j` of `dx` real-units through the active kernel, returning
    /// `(dy_after_fee, new_slot)` with the kernel's balances moved and `D` re-solved (cold `D` is
    /// solved first, matching [`Self::quote_amount_out`]). `tokens`/`n_coins` are preserved. Returns
    /// `None` on kernel error or zero output. Centralizes the 2-vs-3-coin dispatch for the predictor.
    pub fn apply_exchange(&self, i: usize, j: usize, dx: U256) -> Option<(U256, CryptoCurveSlot)> {
        match &self.twocrypto_kernel {
            Some(tk) => {
                let mut k = tk.clone();
                if k.d.is_zero() {
                    k.recompute_d().ok()?;
                }
                let (dy, new_k) = k.apply_exchange(i, j, dx).ok()?;
                if dy.is_zero() {
                    return None;
                }
                let mut new_slot = self.clone();
                new_slot.twocrypto_kernel = Some(new_k);
                Some((dy, new_slot))
            }
            None => {
                let mut k = self.kernel.clone();
                if k.d.is_zero() {
                    k.recompute_d().ok()?;
                }
                let (dy, new_k) = k.apply_exchange(i, j, dx).ok()?;
                if dy.is_zero() {
                    return None;
                }
                let mut new_slot = self.clone();
                new_slot.kernel = new_k;
                Some((dy, new_slot))
            }
        }
    }
}

fn placeholder() -> CryptoCurveSlot {
    CryptoCurveSlot {
        kernel: CryptoSwapState {
            balances: [U256::ZERO; 3],
            precisions: [U256::ZERO; 3],
            price_scale: [U256::ZERO; 2],
            ann: U256::ZERO,
            gamma: U256::ZERO,
            mid_fee: U256::ZERO,
            out_fee: U256::ZERO,
            fee_gamma: U256::ZERO,
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        },
        twocrypto_kernel: None,
        tokens: [[0u8; 20]; MAX_CRYPTO_COINS],
        n_coins: 0,
    }
}

/// Dense CryptoSwap state store. One slot per registered pool address.
pub struct CryptoCurveStateStore {
    slots: Vec<ArcSwap<CryptoCurveSlot>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl CryptoCurveStateStore {
    /// Production constructor: the store is born BRANDED to `universe` (WI-06 F1 totality, codex
    /// item 1). `UniverseBuilder::new` is the SOLE caller — a resident store can never be
    /// constructed WILDCARD in a production build, so a foreign permit can never witness it.
    pub(crate) fn new_branded(universe: UniverseId) -> Self {
        Self {
            slots: Vec::new(),
            universe_id: universe,
        }
    }

    /// Empty WILDCARD store — TEST ONLY (compiled out of every production build via the same cfg
    /// as `StateWriterPermit::test_mint`). Substrate/loom/downstream tests witness it with a
    /// WILDCARD `test_mint` permit.
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn new() -> Self {
        Self::new_branded(UniverseId::WILDCARD)
    }

    /// Allocate `n` empty slots — TEST ONLY (WILDCARD constructor path; gated with `new`).
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn with_capacity(n: usize) -> Self {
        let mut s = Self::new();
        s.slots.reserve_exact(n);
        for _ in 0..n {
            s.slots.push(ArcSwap::from_pointee(placeholder()));
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(placeholder()));
        StateSlot(i)
    }

    /// Single-writer commit.
    pub fn set(&self, slot: StateSlot, value: CryptoCurveSlot, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// Snapshot read.
    pub fn snapshot(&self, slot: StateSlot) -> Arc<CryptoCurveSlot> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&CryptoCurveSlot) -> R) -> R {
        let guard = self.slots[slot.0 as usize].load();
        f(&guard)
    }

    /// Number of slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(any(test, loom, feature = "test-permit"))]
impl Default for CryptoCurveStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_slot() -> CryptoCurveSlot {
        let mut tokens = [[0u8; 20]; MAX_CRYPTO_COINS];
        tokens[0] = [0xaa; 20];
        tokens[1] = [0xbb; 20];
        tokens[2] = [0xcc; 20];
        CryptoCurveSlot {
            twocrypto_kernel: None,
            kernel: CryptoSwapState {
                balances: [
                    U256::from(5_000_000_000_000u64),
                    U256::from(8_000_000_000u64),
                    U256::from(1_600_000_000_000_000_000_000u128),
                ],
                precisions: [
                    U256::from(1_000_000_000_000u64),
                    U256::from(10_000_000_000u64),
                    U256::ONE,
                ],
                price_scale: [
                    U256::from(60_000_000_000_000_000_000_000u128),
                    U256::from(3_000_000_000_000_000_000_000u128),
                ],
                ann: U256::from(1_707_629u64),
                gamma: U256::from(11_809_167_828_997u64),
                mid_fee: U256::from(3_000_000u64),
                out_fee: U256::from(30_000_000u64),
                fee_gamma: U256::from(5_000_000_000_000_000u64),
                d: U256::ZERO,
                last_k0_prev: U256::ZERO,
            },
            tokens,
            n_coins: 3,
        }
    }

    #[test]
    fn store_allocates_and_commits_atomically() {
        let mut store = CryptoCurveStateStore::new();
        let slot = store.allocate();
        let read_before = store.snapshot(slot);
        assert_eq!(read_before.n_coins, 0, "placeholder is empty");

        let s = sample_slot();
        store.set(slot, s.clone(), &crate::publication::StateWriterPermit::test_mint().begin_txn());

        let read_after = store.snapshot(slot);
        assert_eq!(read_after.n_coins, 3);
        assert_eq!(read_after.tokens[0], [0xaa; 20]);
        assert_eq!(read_after.tokens[1], [0xbb; 20]);
        assert_eq!(read_after.tokens[2], [0xcc; 20]);
        assert_eq!(
            read_after.kernel.balances[0],
            U256::from(5_000_000_000_000u64)
        );
    }

    #[test]
    fn index_of_resolves_token_to_kernel_coin_index() {
        let s = sample_slot();
        assert_eq!(s.index_of(&[0xaa; 20]), Some(0));
        assert_eq!(s.index_of(&[0xbb; 20]), Some(1));
        assert_eq!(s.index_of(&[0xcc; 20]), Some(2));
        assert_eq!(s.index_of(&[0xff; 20]), None);
    }

    #[test]
    fn slot_exposes_recomputed_d_via_kernel() {
        // End-to-end gate: a committed slot's kernel can compute D and quote a swap —
        // proves the storage layer doesn't lose precision against the engine-amm kernel.
        let mut s = sample_slot();
        let d = s
            .kernel
            .recompute_d()
            .expect("balanced reference pool converges");
        assert_eq!(d, U256::from(14_597_972_047_721_800_479_164_646u128));
        let (out, _fee, _) = s
            .kernel
            .get_amount_out_exact(0, 1, U256::from(10_000_000_000u64))
            .expect("balanced swap quotes");
        assert_eq!(out, U256::from(16_027_909u64));
    }

    fn sample_twocrypto_slot() -> CryptoCurveSlot {
        let mut tokens = [[0u8; 20]; MAX_CRYPTO_COINS];
        tokens[0] = [0xaa; 20];
        tokens[1] = [0xbb; 20];
        CryptoCurveSlot {
            kernel: CryptoCurveSlot::placeholder_kernel(),
            twocrypto_kernel: Some(TwoCryptoSwapState {
                balances: [
                    U256::from(5_000_000_000_000_000_000_000u128),
                    U256::from(18_000_000_000u64),
                ],
                precisions: [U256::ONE, U256::from(10_000_000_000u64)],
                price_scale: U256::from(28_000_000_000_000_000_000u128),
                ann: U256::from(200_000u64),
                gamma: U256::from(100_000_000_000_000u64),
                mid_fee: U256::from(5_000_000u64),
                out_fee: U256::from(45_000_000u64),
                fee_gamma: U256::from(3_000_000_000_000_000u64),
                d: U256::ZERO,
                last_k0_prev: U256::ZERO,
            }),
            tokens,
            n_coins: 2,
        }
    }

    // TwoCrypto (2-coin) dispatch: a 2-coin slot quotes via the TwoCrypto kernel through the
    // unified `quote_amount_out` (re-solving cold D), and `balances()` returns the 2-coin balances.
    #[test]
    fn two_coin_slot_quotes_via_twocrypto_kernel() {
        let s = sample_twocrypto_slot();
        assert_eq!(s.index_of(&[0xaa; 20]), Some(0));
        assert_eq!(s.index_of(&[0xbb; 20]), Some(1));
        assert_eq!(
            s.balances()[0],
            U256::from(5_000_000_000_000_000_000_000u128)
        );
        assert_eq!(s.balances()[1], U256::from(18_000_000_000u64));
        // Quote coin0 → coin1 for 1 unit of coin0 — must produce a positive output via the 2-coin
        // kernel (the 3-coin `kernel` is a zero placeholder here, so this proves the dispatch).
        let out = s
            .quote_amount_out(0, 1, U256::from(1_000_000_000_000_000_000u128))
            .expect("twocrypto slot quotes > 0");
        assert!(out > U256::ZERO);
        // Cross-check it equals the kernel computed directly (with D solved).
        let mut k = s.twocrypto_kernel.clone().unwrap();
        k.recompute_d().unwrap();
        let (direct, _, _) = k
            .get_amount_out_exact(0, 1, U256::from(1_000_000_000_000_000_000u128))
            .unwrap();
        assert_eq!(out, direct, "slot dispatch == direct kernel quote");
    }

    #[test]
    fn apply_exchange_dispatches_and_moves_active_kernel_balances() {
        // 3-coin: cold D is solved, balances move, token map preserved.
        let s = sample_slot();
        let b0 = s.kernel.balances[0];
        let b1 = s.kernel.balances[1];
        let dx = U256::from(10_000_000_000u64);
        let (dy, new) = s.apply_exchange(0, 1, dx).expect("3-coin apply_exchange");
        assert_eq!(
            dy,
            U256::from(16_027_909u64),
            "matches the direct kernel quote"
        );
        assert_eq!(new.kernel.balances[0], b0 + dx, "coin0 (in) up by dx");
        assert_eq!(new.kernel.balances[1], b1 - dy, "coin1 (out) down by dy");
        assert_eq!(new.tokens, s.tokens, "token map preserved");
        assert_eq!(new.n_coins, 3);
        assert!(!new.kernel.d.is_zero(), "D solved on the predicted slot");

        // 2-coin: dispatch routes to the TwoCrypto kernel, leaving the 3-coin placeholder untouched.
        let s2 = sample_twocrypto_slot();
        let tb0 = s2.twocrypto_kernel.as_ref().unwrap().balances[0];
        let dx2 = U256::from(1_000_000_000_000_000_000u128);
        let (dy2, new2) = s2.apply_exchange(0, 1, dx2).expect("2-coin apply_exchange");
        assert!(dy2 > U256::ZERO);
        let ntk = new2
            .twocrypto_kernel
            .as_ref()
            .expect("2-coin kernel present");
        assert_eq!(ntk.balances[0], tb0 + dx2, "coin0 (in) up by dx");
        assert!(!ntk.d.is_zero());
    }
}
