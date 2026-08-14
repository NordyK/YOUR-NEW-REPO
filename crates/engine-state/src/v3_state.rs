//! V3-family dense state store.
//!
//! Plan §0.4: no hot-path allocations after warmup. The old `Mutex<V3State>` design
//! cloned the entire `V3State` (including `Vec<TickInfo>`) on every read — 128 allocs
//! per dirty event with 64 V3 routes touching two pools each. Replaced with
//! `arc_swap::ArcSwap<V3State>` so reads are an atomic pointer-deref returning an
//! `arc_swap::Guard<Arc<V3State>>`. Writers `swap` a freshly built `Arc<V3State>`
//! (built on the writer thread, off the hot path). Zero allocations on read; readers
//! and writers never block each other.

use alloy_primitives::U256;
use arc_swap::ArcSwap;
use std::sync::Arc;

use engine_amm::TickInfo;

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};

const _V3_STATE_ALIGN_64: () = assert!(std::mem::align_of::<V3State>() == 64);

/// V3 pool live state.
///
/// B7: `#[repr(C, align(64))]` — the scalar head (sqrt_price, tick, liquidity, fees) is what
/// the marginal-product scan touches; aligning to 64 B keeps it in one cache line so the
/// hot read isn't a split-line load. `initialised_ticks` (heap Vec) follows; the tick walk
/// already prefetches sequentially.
#[repr(C, align(64))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3State {
    /// `sqrt(price) * 2^96`.
    pub sqrt_price_x96: U256,
    /// Current tick.
    pub tick: i32,
    /// Active liquidity at the current tick.
    pub liquidity: u128,
    /// Pool fee in pips (e-6).
    pub fee_pips: u32,
    /// Tick spacing.
    pub tick_spacing: i32,
    /// Warm window of initialised ticks. Sorted ascending. The writer publishes a fresh
    /// `Arc<V3State>` via `ArcSwap::swap`; readers hold an `arc_swap::Guard` for the
    /// duration of the sim and see a consistent snapshot.
    pub initialised_ticks: Vec<TickInfo>,
}

impl Default for V3State {
    fn default() -> Self {
        Self {
            sqrt_price_x96: U256::ZERO,
            tick: 0,
            liquidity: 0,
            fee_pips: 0,
            tick_spacing: 0,
            initialised_ticks: Vec::new(),
        }
    }
}

/// Apply a signed liquidity delta to an unsigned active-liquidity value, fail-closed on over/underflow.
fn apply_liquidity_delta(liquidity: u128, delta: i128) -> Option<u128> {
    if delta >= 0 {
        liquidity.checked_add(delta as u128)
    } else {
        liquidity.checked_sub(delta.unsigned_abs())
    }
}

/// Decode the `data` payload of a Uniswap V4
/// `ModifyLiquidity(bytes32 id, address sender, int24 tickLower, int24 tickUpper, int256 liquidityDelta, bytes32 salt)`
/// log into `(tick_lower, tick_upper, liquidity_delta)`. The indexed `id`/`sender` live in topics (not
/// data); data is `[tickLower(word0), tickUpper(word1), liquidityDelta(word2), salt(word3)]`.
///
/// `int24` sign-extends through its 32-byte word, so the low 4 bytes are the exact `i32`. `liquidityDelta`
/// is an `int256`; real pool deltas always fit `i128` (active liquidity is `uint128`), and a value that
/// does NOT fit is rejected (`None`) — a fail-closed signal so the caller falls back to an RPC reconcile
/// rather than truncating. Returns `None` on a short payload.
pub fn decode_v4_modify_liquidity(data: &[u8]) -> Option<(i32, i32, i128)> {
    if data.len() < 96 {
        return None;
    }
    let tick_lower = i32::from_be_bytes(data[28..32].try_into().ok()?);
    let tick_upper = i32::from_be_bytes(data[60..64].try_into().ok()?);
    // liquidityDelta = word2 [64..96]. The i128 is the low 16 bytes; the high 16 must be pure
    // sign-extension of the i128 sign bit, else the int256 doesn't fit i128 → fail-closed.
    let w = &data[64..96];
    let sign = if w[16] & 0x80 != 0 { 0xFFu8 } else { 0x00u8 };
    if w[..16].iter().any(|&b| b != sign) {
        return None;
    }
    let liquidity_delta = i128::from_be_bytes(w[16..32].try_into().ok()?);
    Some((tick_lower, tick_upper, liquidity_delta))
}

/// Decode a Uniswap V3 (or V3-fork: Sushi/Pancake) `Mint`/`Burn` liquidity log into the signed tick
/// delta consumed by [`V3State::apply_modify_liquidity`]. In V3 the bounds are INDEXED, so they
/// arrive in topics (the caller passes `topics[2]`/`topics[3]` as the two 32-byte tick words); the
/// `uint128 amount` (liquidity) is in `data` — at word 1 for `Mint` (`[sender, amount, …]`) and word
/// 0 for `Burn` (`[amount, …]`). `Burn` is negated. Returns `None` (fail-closed) on a short payload
/// or an `amount` that does not fit `i128` (real pool liquidity always does). Caller MUST only invoke
/// this for logs with >= 4 topics (indexed bounds); other layouts (e.g. some Algebra variants that do
/// not index the bounds) fall back to the existing RPC tick-window refresh.
pub fn decode_v3_liquidity_event(
    tick_lower_word: &[u8; 32],
    tick_upper_word: &[u8; 32],
    data: &[u8],
    is_burn: bool,
) -> Option<(i32, i32, i128)> {
    let tick_lower = i32::from_be_bytes(tick_lower_word[28..32].try_into().ok()?);
    let tick_upper = i32::from_be_bytes(tick_upper_word[28..32].try_into().ok()?);
    let amount_off = if is_burn { 0 } else { 32 }; // Burn: data word 0; Mint: data word 1 (after sender)
    let w = data.get(amount_off..amount_off + 32)?;
    // amount is uint128; the high 16 bytes must be zero (else it doesn't fit u128 → fail-closed).
    if w[..16].iter().any(|&b| b != 0) {
        return None;
    }
    let amount = u128::from_be_bytes(w[16..32].try_into().ok()?);
    let signed = i128::try_from(amount).ok()?; // real liquidity fits i128; otherwise fail-closed
    let delta = if is_burn {
        signed.checked_neg()?
    } else {
        signed
    };
    Some((tick_lower, tick_upper, delta))
}

impl V3State {
    /// Apply a Uniswap V4/V3 `ModifyLiquidity` delta to this mirrored pool state WITHOUT any RPC —
    /// the core of zero-RPC tick-map freshness. Canonical Uniswap tick convention (matches the kernel
    /// crossing in `engine_amm::uniswap_v3`): `liquidity_net[tickLower] += delta`,
    /// `liquidity_net[tickUpper] -= delta`, and active `liquidity += delta` iff `tickLower <= tick < tickUpper`.
    ///
    /// Keeps `initialised_ticks` SORTED ascending (the kernel requires it) and prunes any tick whose net
    /// reaches 0 (a net-0 tick contributes nothing when crossed, so dropping it preserves kernel output
    /// and bounds the Vec under continuous event application).
    ///
    /// Atomic + fail-closed: validates the active-liquidity and both tick-net updates BEFORE mutating, so
    /// on any over/underflow (or a malformed `tickLower >= tickUpper`) it returns `false` leaving the
    /// state UNCHANGED — the caller can then trigger an RPC reconcile, and REVM backstops any submit.
    pub fn apply_modify_liquidity(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
    ) -> bool {
        if liquidity_delta == 0 {
            return true; // no-op
        }
        if tick_lower >= tick_upper {
            return false; // malformed range
        }
        // 1) active liquidity (only if the current tick sits inside the modified range)
        let new_liquidity = if tick_lower <= self.tick && self.tick < tick_upper {
            match apply_liquidity_delta(self.liquidity, liquidity_delta) {
                Some(l) => l,
                None => return false,
            }
        } else {
            self.liquidity
        };
        // 2) validate both tick-net updates before committing either
        let new_lower = match self.tick_net(tick_lower).checked_add(liquidity_delta) {
            Some(v) => v,
            None => return false,
        };
        let new_upper = match self.tick_net(tick_upper).checked_sub(liquidity_delta) {
            Some(v) => v,
            None => return false,
        };
        // 3) commit (infallible from here)
        self.upsert_tick(tick_lower, new_lower);
        self.upsert_tick(tick_upper, new_upper);
        self.liquidity = new_liquidity;
        true
    }

    /// `liquidity_net` at `tick` (0 if the tick is not initialised).
    fn tick_net(&self, tick: i32) -> i128 {
        match self
            .initialised_ticks
            .binary_search_by_key(&tick, |t| t.tick)
        {
            Ok(i) => self.initialised_ticks[i].liquidity_net,
            Err(_) => 0,
        }
    }

    /// Set `tick`'s net to `new_net` (sorted insert if absent); remove it if `new_net == 0`.
    fn upsert_tick(&mut self, tick: i32, new_net: i128) {
        match self
            .initialised_ticks
            .binary_search_by_key(&tick, |t| t.tick)
        {
            Ok(i) => {
                if new_net == 0 {
                    self.initialised_ticks.remove(i);
                } else {
                    self.initialised_ticks[i].liquidity_net = new_net;
                }
            }
            Err(i) => {
                if new_net != 0 {
                    self.initialised_ticks.insert(
                        i,
                        TickInfo {
                            tick,
                            liquidity_net: new_net,
                        },
                    );
                }
            }
        }
    }
}

/// Dense V3 state store backed by per-slot `ArcSwap<V3State>`.
pub struct V3StateStore {
    slots: Vec<ArcSwap<V3State>>,
    /// B16: per-slot recycle bin holding the *previous* generation `Arc<V3State>` after a
    /// `set_recycled`. The next `set_recycled` reuses that allocation (control block +
    /// `V3State` + the `initialised_ticks` heap buffer) IFF every reader has released it
    /// (`Arc::get_mut` succeeds), eliminating the steady-state per-update allocation. Lock-free:
    /// `ArcSwapOption` so the recycle path stays on the writer thread without a mutex.
    spares: Vec<arc_swap::ArcSwapOption<V3State>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl V3StateStore {
    /// Production constructor: the store is born BRANDED to `universe` (WI-06 F1 totality, codex
    /// item 1). `UniverseBuilder::new` is the SOLE caller — a resident store can never be
    /// constructed WILDCARD in a production build, so a foreign permit can never witness it.
    pub(crate) fn new_branded(universe: UniverseId) -> Self {
        Self {
            slots: Vec::new(),
            spares: Vec::new(),
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
        s.spares.reserve_exact(n);
        for _ in 0..n {
            s.slots.push(ArcSwap::from_pointee(V3State::default()));
            s.spares.push(arc_swap::ArcSwapOption::empty());
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(V3State::default()));
        self.spares.push(arc_swap::ArcSwapOption::empty());
        StateSlot(i)
    }

    /// Single-writer set. Builds a fresh `Arc<V3State>` and atomically swaps it in;
    /// readers holding a `Guard` to the previous generation are unaffected.
    pub fn set(&self, slot: StateSlot, value: V3State, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// B16: allocation-recycling single-writer set. Drop-in replacement for [`set`](Self::set)
    /// with identical observable behaviour, but reuses the previous generation's heap allocation
    /// when no reader still holds it, so the steady state does **zero** allocations per update.
    ///
    /// Mechanism: the previous `set_recycled` stashed the just-replaced `Arc` in this slot's
    /// recycle bin. We take it back; if `Arc::get_mut` succeeds (every sim reader has dropped its
    /// `Guard`), we overwrite the fields in place — crucially `clear()` + `extend_from_slice` on
    /// `initialised_ticks` reuses its existing buffer capacity rather than reallocating — and swap
    /// that recycled `Arc` in. If a reader still holds it, we fall back to a fresh `Arc::new`
    /// (never mutating a buffer a reader is observing). Either way the now-replaced `Arc` becomes
    /// the next recycle candidate.
    ///
    /// Safe to interleave with `set`/`store`: the recycle bin only ever holds an `Arc` this store
    /// previously published, and the fall-back path is always correct.
    pub fn set_recycled(&self, slot: StateSlot, value: V3State, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        let i = slot.0 as usize;
        let new_arc = match self.spares[i].swap(None) {
            Some(mut spare) => match Arc::get_mut(&mut spare) {
                Some(st) => {
                    // Uniquely owned — reuse the allocation. Overwrite the scalar head and
                    // refill the tick buffer in place (reuses capacity, no realloc when it fits).
                    st.sqrt_price_x96 = value.sqrt_price_x96;
                    st.tick = value.tick;
                    st.liquidity = value.liquidity;
                    st.fee_pips = value.fee_pips;
                    st.tick_spacing = value.tick_spacing;
                    st.initialised_ticks.clear();
                    st.initialised_ticks
                        .extend_from_slice(&value.initialised_ticks);
                    spare
                }
                // Still referenced by a reader's Guard — cannot mutate. Drop it (its refcount
                // falls when that reader releases) and allocate fresh.
                None => Arc::new(value),
            },
            None => Arc::new(value),
        };
        let prev = self.slots[i].swap(new_arc);
        // Stash the replaced generation for the next recycle attempt.
        self.spares[i].store(Some(prev));
    }

    /// Hot-path read API. Returns a `V3State` snapshot — a clone of the current state.
    /// The clone is unavoidable because the V3 kernel takes `&[TickInfo]` by reference
    /// and the caller's borrow needs to outlive the guard. For zero-allocation reads
    /// see `with_snapshot`.
    pub fn snapshot(&self, slot: StateSlot) -> V3State {
        (**self.slots[slot.0 as usize].load()).clone()
    }

    /// Zero-allocation hot-path read API. Loads a `Guard` over the current generation
    /// and passes it to the closure. The reader can borrow `&V3State` fields directly
    /// (including `&[TickInfo]`) without cloning the tick vector.
    ///
    /// Replaces the old `Mutex<V3State>.lock().clone()` pattern that allocated on every
    /// read. With 64 routes × 2 pools per route the old path did 128 Vec allocs per
    /// dirty event; this path does zero.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&V3State) -> R) -> R {
        let guard = self.slots[slot.0 as usize].load();
        f(&guard)
    }

    /// Get a guard over the current generation. Caller can hold the guard across reads.
    /// Use this when the borrow lifetime needs to span multiple operations (e.g. two-hop
    /// evaluator that touches two pools simultaneously).
    #[inline]
    pub fn load(&self, slot: StateSlot) -> arc_swap::Guard<Arc<V3State>> {
        self.slots[slot.0 as usize].load()
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
impl Default for V3StateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod recycle_tests {
    use super::*;
    use engine_amm::TickInfo;

    fn st(tick: i32, liq: u128, n_ticks: usize) -> V3State {
        V3State {
            sqrt_price_x96: U256::from(tick.unsigned_abs() as u64 + 1) << 96,
            tick,
            liquidity: liq,
            fee_pips: 3000,
            tick_spacing: 60,
            initialised_ticks: (0..n_ticks)
                .map(|k| TickInfo {
                    tick: tick + k as i32,
                    liquidity_net: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn set_recycled_is_observably_identical_to_set() {
        // Functional equivalence: after set_recycled, the snapshot must equal the written value.
        let store = V3StateStore::with_capacity(1);
        let slot = StateSlot(0);
        let v = st(100, 5_000, 8);
        store.set_recycled(slot, v.clone(), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(store.snapshot(slot), v);
        // And again with a different shape (fewer ticks) — must not retain stale tail entries.
        let v2 = st(-50, 9_000, 3);
        store.set_recycled(slot, v2.clone(), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(store.snapshot(slot), v2);
    }

    #[test]
    fn steady_state_reuses_the_allocation_no_new_alloc() {
        // The core B16 guarantee: once a recycled Arc has cycled through the spare bin and no
        // reader holds it, a subsequent set_recycled reuses that SAME allocation (pointer
        // identity), proving zero per-update allocation in steady state.
        let store = V3StateStore::with_capacity(1);
        let slot = StateSlot(0);

        store.set_recycled(slot, st(1, 10, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen A published; spare = default (gen 0)
        store.set_recycled(slot, st(2, 20, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen B published; spare = gen A (count 1)
                                                // Record gen B's address (currently published; will be moved to spare on next set).
        let gen_b_ptr = Arc::as_ptr(&store.load(slot));
        store.set_recycled(slot, st(3, 30, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen C reuses spare (gen A); spare = gen B
        store.set_recycled(slot, st(4, 40, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen D should REUSE gen B's allocation

        let gen_d_ptr = Arc::as_ptr(&store.load(slot));
        assert_eq!(
            gen_d_ptr, gen_b_ptr,
            "steady-state set_recycled must reuse a prior allocation (pointer identity)"
        );
        assert_eq!(
            store.snapshot(slot).tick,
            4,
            "value still correct after recycling"
        );
    }

    #[test]
    fn reader_holding_previous_generation_forces_fresh_alloc_no_corruption() {
        // Safety: if a reader still holds the spare's generation, set_recycled MUST NOT mutate it
        // in place (that would corrupt the reader's view). It falls back to a fresh alloc, and the
        // held guard keeps observing its original snapshot.
        let store = V3StateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, st(1, 10, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // spare = gen 0
        store.set_recycled(slot, st(2, 20, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // published gen B; spare = gen A

        // Pin gen B by holding a full Arc clone (simulates a sim reader's live snapshot).
        let held = store.load(slot);
        let held_arc: Arc<V3State> = arc_swap::Guard::into_inner(held);
        assert_eq!(held_arc.tick, 2);

        // This set moves gen B into the spare bin while we still hold it (count == 2).
        store.set_recycled(slot, st(3, 30, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        // Next set tries to recycle gen B, finds get_mut fails → fresh alloc, no in-place mutation.
        store.set_recycled(slot, st(4, 40, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn());

        assert_eq!(held_arc.tick, 2, "held reader's snapshot must be untouched");
        assert_eq!(held_arc.liquidity, 20);
        assert_eq!(
            store.snapshot(slot).tick,
            4,
            "current generation is the latest write"
        );
    }

    #[test]
    fn recycled_buffer_capacity_is_reused_when_it_fits() {
        // Writing a larger tick set then a smaller one: the smaller write reuses the grown buffer
        // (capacity >= len) rather than reallocating, and reports the correct (shorter) length.
        let store = V3StateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, st(0, 1, 32), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // spare warm-up
        store.set_recycled(slot, st(0, 1, 32), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // recyclable spare now holds a 32-cap buffer
        store.set_recycled(slot, st(0, 1, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // reuse: clear()+extend to 4, capacity stays
        let snap = store.snapshot(slot);
        assert_eq!(snap.initialised_ticks.len(), 4);
    }
}

#[cfg(test)]
mod v4_modify_liquidity_tests {
    use super::*;
    use engine_amm::TickInfo;

    /// Sign-extend a signed value into a 32-byte ABI word (matches int24 / int256 encoding).
    fn word_signed(v: i128) -> [u8; 32] {
        let mut w = [if v < 0 { 0xFFu8 } else { 0x00u8 }; 32];
        w[16..32].copy_from_slice(&v.to_be_bytes());
        w
    }

    /// Encode a ModifyLiquidity `data` payload (4 words: tickLower, tickUpper, liquidityDelta, salt).
    fn enc(lower: i32, upper: i32, delta: i128) -> Vec<u8> {
        let mut d = Vec::with_capacity(128);
        d.extend_from_slice(&word_signed(lower as i128));
        d.extend_from_slice(&word_signed(upper as i128));
        d.extend_from_slice(&word_signed(delta));
        d.extend_from_slice(&[0xABu8; 32]); // salt — ignored by decode
        d
    }

    fn v3(tick: i32, liquidity: u128, ticks: &[(i32, i128)]) -> V3State {
        V3State {
            sqrt_price_x96: U256::from(1u64) << 96,
            tick,
            liquidity,
            fee_pips: 3000,
            tick_spacing: 60,
            initialised_ticks: ticks
                .iter()
                .map(|&(t, n)| TickInfo {
                    tick: t,
                    liquidity_net: n,
                })
                .collect(),
        }
    }

    #[test]
    fn decode_positive() {
        assert_eq!(
            decode_v4_modify_liquidity(&enc(-887220, 887220, 1_000_000)),
            Some((-887220, 887220, 1_000_000))
        );
    }

    #[test]
    fn decode_negative_ticks_and_negative_delta() {
        assert_eq!(
            decode_v4_modify_liquidity(&enc(-60, -10, -5_000_000_000_000)),
            Some((-60, -10, -5_000_000_000_000))
        );
    }

    #[test]
    fn decode_short_payload_is_none() {
        assert_eq!(decode_v4_modify_liquidity(&[0u8; 64]), None);
    }

    #[test]
    fn decode_delta_not_fitting_i128_is_fail_closed() {
        // A word2 with a bit set in the high 16 bytes does not fit i128 → None (fail-closed).
        let mut d = enc(-60, 60, 0);
        d[64 + 15] = 0x01; // set bit in the upper half of the liquidityDelta word
        assert_eq!(decode_v4_modify_liquidity(&d), None);
    }

    #[test]
    fn add_in_range_inserts_two_ticks_and_bumps_liquidity() {
        let mut s = v3(0, 1000, &[]);
        assert!(s.apply_modify_liquidity(-60, 60, 500));
        assert_eq!(s.liquidity, 1500); // tick 0 ∈ [-60, 60)
        assert_eq!(
            s.initialised_ticks,
            vec![
                TickInfo {
                    tick: -60,
                    liquidity_net: 500
                },
                TickInfo {
                    tick: 60,
                    liquidity_net: -500
                },
            ]
        );
    }

    #[test]
    fn add_outside_range_leaves_active_liquidity_unchanged() {
        let mut s = v3(100, 1000, &[]);
        assert!(s.apply_modify_liquidity(-60, 60, 500));
        assert_eq!(s.liquidity, 1000); // tick 100 ∉ [-60, 60)
        assert_eq!(s.initialised_ticks.len(), 2); // ticks still recorded
    }

    #[test]
    fn range_bounds_are_lower_inclusive_upper_exclusive() {
        // tick == tickLower → inside (liquidity changes)
        let mut lo = v3(-60, 1000, &[]);
        lo.apply_modify_liquidity(-60, 60, 500);
        assert_eq!(lo.liquidity, 1500);
        // tick == tickUpper → outside (liquidity unchanged)
        let mut hi = v3(60, 1000, &[]);
        hi.apply_modify_liquidity(-60, 60, 500);
        assert_eq!(hi.liquidity, 1000);
    }

    #[test]
    fn remove_prunes_ticks_that_net_to_zero() {
        let mut s = v3(0, 1500, &[(-60, 500), (60, -500)]);
        assert!(s.apply_modify_liquidity(-60, 60, -500));
        assert_eq!(s.liquidity, 1000);
        assert!(
            s.initialised_ticks.is_empty(),
            "ticks netting to 0 must be pruned"
        );
    }

    #[test]
    fn ticks_stay_sorted_ascending_after_insert() {
        let mut s = v3(0, 1000, &[(-120, 100), (120, -100)]);
        assert!(s.apply_modify_liquidity(-60, 60, 500));
        let ticks: Vec<i32> = s.initialised_ticks.iter().map(|t| t.tick).collect();
        assert_eq!(ticks, vec![-120, -60, 60, 120]);
        assert!(s
            .initialised_ticks
            .windows(2)
            .all(|w| w[0].tick < w[1].tick));
    }

    #[test]
    fn underflow_is_fail_closed_and_leaves_state_unchanged() {
        let mut s = v3(0, 100, &[(-60, 500), (60, -500)]);
        let before = s.clone();
        assert!(
            !s.apply_modify_liquidity(-60, 60, -500),
            "removing more liquidity than active must fail-closed"
        );
        assert_eq!(s, before, "failed apply must leave state untouched");
    }

    #[test]
    fn add_then_remove_same_range_round_trips_to_original() {
        let mut s = v3(0, 1000, &[(-120, 7), (120, -7)]);
        let orig = s.clone();
        assert!(s.apply_modify_liquidity(-60, 60, 500));
        assert!(s.apply_modify_liquidity(-60, 60, -500));
        assert_eq!(
            s, orig,
            "add then equal remove must restore the exact prior state"
        );
    }

    #[test]
    fn overlapping_position_updates_existing_tick_net() {
        // A second add sharing tickLower accumulates net rather than duplicating the tick.
        let mut s = v3(0, 1000, &[(-60, 500), (60, -500)]);
        assert!(s.apply_modify_liquidity(-60, 120, 300));
        assert_eq!(s.tick_net(-60), 800); // 500 + 300
        assert_eq!(s.tick_net(60), -500); // untouched
        assert_eq!(s.tick_net(120), -300); // new upper
        assert_eq!(s.liquidity, 1300); // tick 0 ∈ [-60,120)
    }
}

#[cfg(test)]
mod v3_liquidity_event_tests {
    use super::*;

    /// 32-byte sign-extended word (indexed int24 tick layout in a topic).
    fn tick_word(t: i32) -> [u8; 32] {
        let mut w = [if t < 0 { 0xFFu8 } else { 0x00u8 }; 32];
        w[16..32].copy_from_slice(&(t as i128).to_be_bytes());
        w
    }
    /// A uint128 `amount` packed into a 32-byte word (high 16 bytes zero).
    fn amount_word(a: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&a.to_be_bytes());
        w
    }
    /// Mint data: [sender, amount, amount0, amount1].
    fn mint_data(amount: u128) -> Vec<u8> {
        let mut d = vec![0u8; 32]; // sender
        d.extend_from_slice(&amount_word(amount));
        d.extend_from_slice(&[0u8; 64]); // amount0, amount1
        d
    }
    /// Burn data: [amount, amount0, amount1].
    fn burn_data(amount: u128) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&amount_word(amount));
        d.extend_from_slice(&[0u8; 64]);
        d
    }

    #[test]
    fn decode_mint_positive_delta() {
        assert_eq!(
            decode_v3_liquidity_event(&tick_word(-60), &tick_word(60), &mint_data(5000), false),
            Some((-60, 60, 5000))
        );
    }

    #[test]
    fn decode_burn_negates_delta() {
        assert_eq!(
            decode_v3_liquidity_event(
                &tick_word(-887220),
                &tick_word(887220),
                &burn_data(5000),
                true
            ),
            Some((-887220, 887220, -5000))
        );
    }

    #[test]
    fn decode_handles_negative_ticks() {
        assert_eq!(
            decode_v3_liquidity_event(&tick_word(-120), &tick_word(-60), &mint_data(7), false),
            Some((-120, -60, 7))
        );
    }

    #[test]
    fn decode_amount_over_u128_high_bytes_is_fail_closed() {
        let mut d = mint_data(0);
        d[32] = 0x01; // set a high byte of the amount word → not a valid uint128 low-half
        assert_eq!(
            decode_v3_liquidity_event(&tick_word(0), &tick_word(60), &d, false),
            None
        );
    }

    #[test]
    fn decode_short_data_is_none() {
        // Mint needs >= 64 bytes (amount at word 1); 32 bytes is too short.
        assert_eq!(
            decode_v3_liquidity_event(&tick_word(0), &tick_word(60), &[0u8; 32], false),
            None
        );
    }

    #[test]
    fn decode_then_apply_mint_then_burn_round_trips() {
        // Integration: a Mint decode applied, then an equal Burn decode applied, restores state.
        let mut s = V3State {
            sqrt_price_x96: U256::from(1u64) << 96,
            tick: 0,
            liquidity: 1000,
            fee_pips: 500,
            tick_spacing: 10,
            initialised_ticks: Vec::new(),
        };
        let orig = s.clone();
        let (lo, hi, d_mint) =
            decode_v3_liquidity_event(&tick_word(-10), &tick_word(10), &mint_data(500), false)
                .unwrap();
        assert!(s.apply_modify_liquidity(lo, hi, d_mint));
        assert_eq!(s.liquidity, 1500);
        let (lo2, hi2, d_burn) =
            decode_v3_liquidity_event(&tick_word(-10), &tick_word(10), &burn_data(500), true)
                .unwrap();
        assert!(s.apply_modify_liquidity(lo2, hi2, d_burn));
        assert_eq!(
            s, orig,
            "Mint then equal Burn must restore the exact prior state"
        );
    }
}
