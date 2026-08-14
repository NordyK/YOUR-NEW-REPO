//! Startup builder that wires tokens, pools, and the address registry together. Single
//! entry point so the engine binary doesn't repeat boilerplate.

use crate::dodo_state::DodoStateStore;
use crate::fluid_state::{FluidPoolSnapshot, FluidStateStore};
use crate::ids::{PairId, PoolId, StateSlot, TokenId, VenueId};
use crate::maverick_state::MaverickStateStore;
use crate::pool::{PoolKind, PoolMeta, PoolTable, PoolTier};
use crate::registry::{AddressRegistry, RegistryBuilder, RegistryError};
use crate::token::{Address, RiskClass, TokenMeta, TokenTable};
use crate::v2_state::{V2State, V2StateStore};
use crate::v3_state::{V3State, V3StateStore};
use engine_amm::dodo_v2::DodoState;
use engine_amm::maverick_v2::MaverickState;

/// Convenience builder that combines token + pool + state + registry.
pub struct UniverseBuilder {
    pub(crate) tokens: TokenTable,
    pub(crate) pools: PoolTable,
    pub(crate) v2_state: V2StateStore,
    pub(crate) v3_state: V3StateStore,
    pub(crate) fluid_state: FluidStateStore,
    pub(crate) dodo_state: DodoStateStore,
    pub(crate) balancer_state: crate::balancer_state::BalancerStateStore,
    pub(crate) balancer_weighted_state: crate::balancer_weighted_state::BalancerWeightedStateStore,
    pub(crate) curve_state: crate::curve_state::CurveStateStore,
    pub(crate) curve_crypto_state: crate::curve_crypto_state::CryptoCurveStateStore,
    pub(crate) lb_state: crate::lb_state::LbStateStore,
    pub(crate) maverick_state: MaverickStateStore,
    pub(crate) registry: RegistryBuilder,
    pub(crate) v4_pool_ids: ahash::AHashMap<PoolId, [u8; 32]>,
    pair_index: ahash::AHashMap<(TokenId, TokenId), PairId>,
    /// WI-06 §3.3 sole-writer cradle: the single [`StateWriterPermit`] is born here (empty
    /// builder), used to witness every PRE-SEAL family-store init, and handed to the state
    /// thread by [`seal`](Self::seal). [`build`](Self::build) drops it (no post-seal writer).
    seal_permit: Option<crate::publication::StateWriterPermit>,
}

impl UniverseBuilder {
    /// New empty builder.
    pub fn new() -> Self {
        // WI-06 F1 (codex item 1): allocate this universe's process-unique brand and stamp it
        // into the sole-writer permit AND every family store + the pool table. A permit minted by
        // a DIFFERENT builder carries a different brand, so its txn is refused at every witnessed
        // write — the forgeable-permit class is closed structurally. Branding happens HERE, before
        // any pre-seal `add_*_pool` init write, so the cradle permit and the stores already agree.
        let universe_id = crate::publication::next_universe_id();
        // Every resident store/table is BORN branded (`new_branded`) — there is no WILDCARD
        // window and no separate `brand()` step. Combined with the cradle permit minted on the
        // same brand, the store set and the sole-writer permit already agree before any pre-seal
        // `add_*_pool` init write.
        Self {
            tokens: TokenTable::new(),
            pools: PoolTable::new_branded(universe_id),
            v2_state: V2StateStore::new_branded(universe_id),
            v3_state: V3StateStore::new_branded(universe_id),
            fluid_state: FluidStateStore::new_branded(universe_id),
            dodo_state: DodoStateStore::new_branded(universe_id),
            balancer_state: crate::balancer_state::BalancerStateStore::new_branded(universe_id),
            balancer_weighted_state:
                crate::balancer_weighted_state::BalancerWeightedStateStore::new_branded(
                    universe_id,
                ),
            curve_state: crate::curve_state::CurveStateStore::new_branded(universe_id),
            curve_crypto_state:
                crate::curve_crypto_state::CryptoCurveStateStore::new_branded(universe_id),
            lb_state: crate::lb_state::LbStateStore::new_branded(universe_id),
            maverick_state: MaverickStateStore::new_branded(universe_id),
            registry: RegistryBuilder::new(),
            v4_pool_ids: ahash::AHashMap::default(),
            pair_index: ahash::AHashMap::default(),
            seal_permit: Some(crate::publication::StateWriterPermit::mint(universe_id)),
        }
    }

    /// Insert a token. Returns the dense id. Also registers the address → id mapping
    /// in the address registry so router decoders can resolve hop endpoints.
    pub fn add_token(&mut self, address: Address, decimals: u8, risk: RiskClass) -> TokenId {
        let id = self.tokens.insert(TokenMeta::new(address, decimals, risk));
        let _ = self.registry.register_token(address, id);
        id
    }

    /// Set the Q96 ETH-per-smallest-unit factor for a token (startup price wiring).
    /// Profit accounting is in ETH-wei; this lets a cycle funded in any base token
    /// convert its gross via `(amount * q96) >> 96`.
    pub fn set_token_eth_price_q96(&mut self, id: TokenId, q96: alloy_primitives::U256) {
        self.tokens.set_eth_wei_per_unit_q96(id, q96);
    }

    /// Insert a V2 pool with starting reserves. Returns the dense pool id.
    ///
    /// `kind` must be one of the V2-family variants (`UniswapV2`, `SushiV2`, `CamelotV2`).
    /// The family is preserved verbatim in `PoolMeta` so the dispatcher and the runtime
    /// state writer can route each pool through its family-specific kernel — important
    /// for Camelot V2's directional dynamic fees.
    pub fn add_v2_pool(
        &mut self,
        address: Address,
        kind: PoolKind,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_bps: u32,
        reserves: V2State,
    ) -> Result<PoolId, RegistryError> {
        debug_assert!(
            matches!(
                kind,
                PoolKind::UniswapV2
                    | PoolKind::SushiV2
                    | PoolKind::CamelotV2
                    // SolidlyV3 (Solidly/Velodrome-style stable) is V2-reserves-backed: it stores
                    // token0/token1 reserves in v2_state and is refreshed via the V2 Sync-log path.
                    // Only the price *curve* differs (x³y+xy³=k), dispatched by PoolKind downstream.
                    | PoolKind::SolidlyV3
            ),
            "add_v2_pool requires a V2-reserves-backed kind, got {:?}",
            kind
        );
        let pair = self.pair_for(token0, token1);
        let state_slot = self.v2_state.allocate();
        let meta = PoolMeta {
            address,
            kind,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_bps,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.v2_state.set(state_slot, reserves, &init_txn);
        self.registry.register_pool(address, id)?;
        // Register the address-keyed pair lookup so router decoders can find the pool
        // from (token_in, token_out).
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a V3-family pool with starting state. Returns the dense pool id.
    ///
    /// `kind` must be one of the V3-family variants (`UniswapV3`, `SushiV3`, `PancakeV3`,
    /// `AlgebraV19`). Preserving the family in `PoolMeta` keeps observability accurate
    /// and lets the snapshot writer / dispatcher route each pool through its
    /// family-specific kernel.
    pub fn add_v3_pool(
        &mut self,
        address: Address,
        kind: PoolKind,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        tick_spacing: i32,
        state: V3State,
    ) -> Result<PoolId, RegistryError> {
        debug_assert!(
            matches!(
                kind,
                PoolKind::UniswapV3
                    | PoolKind::SushiV3
                    | PoolKind::PancakeV3
                    | PoolKind::AlgebraV19
                    | PoolKind::UniswapV4
                    // KyberSwapElastic is concentrated-liquidity (V3-shaped): it stores a V3State in
                    // v3_state and quotes via the Elastic kernel (dispatched by PoolKind). Same
                    // store, different curve — so add_v3_pool is its correct constructor too.
                    | PoolKind::KyberSwapElastic
            ),
            "add_v3_pool requires a V3-state-backed kind, got {:?}",
            kind
        );
        let pair = self.pair_for(token0, token1);
        let state_slot = self.v3_state.allocate();
        let meta = PoolMeta {
            address,
            kind,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_pips,
            tick_spacing,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.v3_state.set(state_slot, state, &init_txn);
        self.registry.register_pool(address, id)?;
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a hookless Uniswap V4 pool with its canonical 32-byte PoolManager poolId.
    ///
    /// V4 state is stored in the V3-family state store, but executor calldata needs the
    /// full `poolId` in addition to the synthetic 20-byte address used for resident-pool
    /// indexing and log lookup.
    pub fn add_v4_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        tick_spacing: i32,
        pool_id: [u8; 32],
        state: V3State,
    ) -> Result<PoolId, RegistryError> {
        let id = self.add_v3_pool(
            address,
            PoolKind::UniswapV4,
            token0,
            token1,
            venue,
            fee_pips,
            tick_spacing,
            state,
        )?;
        self.v4_pool_ids.insert(id, pool_id);
        Ok(id)
    }

    /// Insert a Fluid DEX pool (v1/T1 or v2 D3/D4) with starting snapshot. Same
    /// shape as `add_v2_pool` / `add_v3_pool` so the rest of the engine can
    /// treat Fluid pools uniformly through PoolId.
    pub fn add_fluid_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        snapshot: FluidPoolSnapshot,
    ) -> Result<PoolId, RegistryError> {
        let pair = self.pair_for(token0, token1);
        let state_slot = self.fluid_state.allocate();
        let meta = PoolMeta {
            address,
            kind: PoolKind::FluidDex,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_pips,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.fluid_state.set(state_slot, snapshot, &init_txn);
        self.registry.register_pool(address, id)?;
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a DODO V2 pool (DVM / DPP / DSP) with starting snapshot.
    pub fn add_dodo_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        snapshot: DodoState,
    ) -> Result<PoolId, RegistryError> {
        let pair = self.pair_for(token0, token1);
        let state_slot = self.dodo_state.allocate();
        let meta = PoolMeta {
            address,
            kind: PoolKind::DodoV2,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_pips,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.dodo_state.set(state_slot, snapshot, &init_txn);
        self.registry.register_pool(address, id)?;
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert (or extend) a Balancer V2 stable pool for the tradable pair `(token0, token1)`.
    ///
    /// A Balancer stable pool is ONE Vault contract (one `address`) holding the FULL multi-token
    /// state in a SINGLE state slot; the route layer is pairwise. The `slot` carries that full state
    /// (`getPoolTokens` balances, scaling factors, amp, fee) plus the token list, so `index_of`
    /// resolves ANY pair to kernel indices against the one slot.
    ///
    /// [Codex AUD-016] If `address` is already registered (a sibling pair of the same multi-token
    /// pool), this REUSES the existing pool id + slot and only adds the new `(token0, token1)` pair
    /// index — it does NOT allocate a second slot. That is required for correctness: the address→id
    /// map is 1:1 and a Vault-Swap log resolves by address to exactly one id, so a second pair-
    /// specific slot would never be woken and would serve stale reserves. One slot per address means
    /// a single Vault-Swap refreshes every pair at once.
    pub fn add_balancer_stable_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        slot: crate::balancer_state::BalancerStableSlot,
    ) -> Result<PoolId, RegistryError> {
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        // Sibling pair of an already-registered multi-token pool ⇒ reuse the existing id + full-state
        // slot; just index the new pair. (The passed `slot` is redundant here — the first
        // registration's full multi-token state already serves every pair via `index_of`.)
        if let Some(existing) = self.registry.pool_of(&address) {
            self.registry
                .register_pair(token0_addr, token1_addr, existing);
            return Ok(existing);
        }
        let pair = self.pair_for(token0, token1);
        let state_slot = self.balancer_state.allocate();
        let meta = PoolMeta {
            address,
            kind: PoolKind::BalancerStable,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_pips,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.balancer_state.set(state_slot, slot, &init_txn);
        // First registration for this address — establishes the 1:1 address→id mapping.
        self.registry.register_pool(address, id)?;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a Balancer V2 **Weighted** pool for the tradable pair `(token0, token1)`. Mirrors
    /// [`Self::add_balancer_stable_pool`] but stores into the dedicated
    /// `BalancerWeightedStateStore` and tags the pool `PoolKind::BalancerWeighted` so the engine
    /// picks the weighted quote kernel + emits Mantis exchangeId 27.
    pub fn add_balancer_weighted_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        slot: crate::balancer_weighted_state::BalancerWeightedSlot,
    ) -> Result<PoolId, RegistryError> {
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        // [AUD-BAL-WEIGHTED-REUSE] Sibling pair of an already-registered multi-token WEIGHTED pool
        // ⇒ reuse the existing id + full-state slot; just index the new pair. A second slot would be
        // an orphan the Vault-Swap (resolved 1:1 by address) never refreshes → startup-stale
        // reserves. Mirrors `add_balancer_stable_pool` (AUD-016). The passed `slot` is redundant
        // here — the first registration's full multi-token state serves every pair via `index_of`.
        if let Some(existing) = self.registry.pool_of(&address) {
            self.registry
                .register_pair(token0_addr, token1_addr, existing);
            return Ok(existing);
        }
        let pair = self.pair_for(token0, token1);
        let state_slot = self.balancer_weighted_state.allocate();
        let meta = PoolMeta {
            address,
            kind: PoolKind::BalancerWeighted,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_pips,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.balancer_weighted_state.set(state_slot, slot, &init_txn);
        let _ = self.registry.register_pool(address, id);
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a Curve StableSwap-NG plain pool, registered for one tradable coin
    /// pair `(token0, token1)`. The `slot` carries the full multi-coin kernel state
    /// (balances, per-coin rates, A, fee) plus the coin token list so the hot path
    /// resolves the pair to kernel coin indices `i`/`j`. Mirrors
    /// `add_balancer_stable_pool`: one `PoolMeta` per pool address; `fee_bps` here is
    /// the Curve fee in its native 1e10 units (stored verbatim in `fee_pips_or_bps`
    /// and converted to bps by the §10.6 fee-floor pre-filter).
    pub fn add_curve_pool(
        &mut self,
        address: Address,
        kind: PoolKind,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_bps: u32,
        slot: crate::curve_state::CurveSlot,
    ) -> Result<PoolId, RegistryError> {
        // `add_curve_pool` covers Curve **StableSwap NG** pools only — these route through the
        // `engine-amm::curve::get_dy` StableSwap kernel. **CryptoSwap NG** (TwoCrypto-NG /
        // Tricrypto-NG) is now supported via the separate [`Self::add_curve_crypto_pool`]
        // registrar (its math kernel lives in `engine-amm::curve_cryptoswap`, with its own
        // dense state store). Any other kind fails closed — registering would silently
        // mis-route at quote time.
        if !matches!(kind, PoolKind::CurveStableSwapNg) {
            return Err(RegistryError::UnsupportedKind(kind));
        }
        let pair = self.pair_for(token0, token1);
        let state_slot = self.curve_state.allocate();
        let meta = PoolMeta {
            address,
            kind,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_bps,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.curve_state.set(state_slot, slot, &init_txn);
        // Address→id is 1:1; only register the first pair for a given pool address.
        let _ = self.registry.register_pool(address, id);
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Register a Curve **CryptoSwap NG** pool (TwoCrypto-NG / Tricrypto-NG).
    ///
    /// Mirrors [`Self::add_curve_pool`] but stores into the dedicated
    /// [`crate::curve_crypto_state::CryptoCurveStateStore`]. The kernel math is
    /// `engine-amm::curve_cryptoswap::CryptoSwapState` — Newton-`D` + `_newton_y` integer-exact
    /// against the Vyper source, validated by reference-vector tests in `engine-amm`.
    pub fn add_curve_crypto_pool(
        &mut self,
        address: Address,
        kind: PoolKind,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_bps: u32,
        slot: crate::curve_crypto_state::CryptoCurveSlot,
    ) -> Result<PoolId, RegistryError> {
        if !matches!(kind, PoolKind::CurveCryptoSwap) {
            return Err(RegistryError::UnsupportedKind(kind));
        }
        let pair = self.pair_for(token0, token1);
        let state_slot = self.curve_crypto_state.allocate();
        let meta = PoolMeta {
            address,
            kind,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_bps,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.curve_crypto_state.set(state_slot, slot, &init_txn);
        let _ = self.registry.register_pool(address, id);
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a Trader Joe Liquidity Book (LB v2.1/v2.2) pool with starting
    /// snapshot. The `slot` carries the kernel `LBState` (active bin + bin window
    /// + fee params) plus the token0/token1 addresses so the hot path can resolve
    /// each hop's direction to the kernel's `zero_for_one` flag. `fee_bps` is the
    /// pool's total fee (base + variable, clamped) in basis points, stored in
    /// `fee_pips_or_bps` for the §10.6 fee-floor pre-filter.
    pub fn add_lb_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_bps: u32,
        slot: crate::lb_state::LbSlot,
    ) -> Result<PoolId, RegistryError> {
        let pair = self.pair_for(token0, token1);
        let state_slot = self.lb_state.allocate();
        let meta = PoolMeta {
            address,
            kind: PoolKind::TraderJoeLb,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_bps,
            tick_spacing: 0,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.lb_state.set(state_slot, slot, &init_txn);
        self.registry.register_pool(address, id)?;
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    /// Insert a Maverick V2 pool with starting snapshot.
    pub fn add_maverick_pool(
        &mut self,
        address: Address,
        token0: TokenId,
        token1: TokenId,
        venue: VenueId,
        fee_pips: u32,
        snapshot: MaverickState,
    ) -> Result<PoolId, RegistryError> {
        let pair = self.pair_for(token0, token1);
        let state_slot = self.maverick_state.allocate();
        let meta = PoolMeta {
            address,
            kind: PoolKind::MaverickV2,
            token0,
            token1,
            pair,
            venue,
            fee_pips_or_bps: fee_pips,
            tick_spacing: snapshot.tick_spacing as i32,
            state_slot,
        };
        let id = self.pools.insert(meta);
        self.pools.set_tier(id, PoolTier::Validated);
        let init_txn = self
            .seal_permit
            .as_mut()
            .expect("builder seal permit present before seal")
            .begin_txn();
        self.maverick_state.set(state_slot, snapshot, &init_txn);
        self.registry.register_pool(address, id)?;
        let token0_addr = self.tokens.get(token0).address;
        let token1_addr = self.tokens.get(token1).address;
        self.registry.register_pair(token0_addr, token1_addr, id);
        Ok(id)
    }

    fn pair_for(&mut self, a: TokenId, b: TokenId) -> PairId {
        let key = if a.0 <= b.0 { (a, b) } else { (b, a) };
        let next = self.pair_index.len() as u16;
        *self.pair_index.entry(key).or_insert(PairId(next))
    }

    /// Promote a pool's tier (control-plane only).
    pub fn set_pool_tier(&mut self, id: PoolId, tier: PoolTier) {
        self.pools.set_tier(id, tier);
    }

    /// Snapshot the V3 pool's current state.
    pub fn v3_snapshot(&self, slot: StateSlot) -> V3State {
        self.v3_state.snapshot(slot)
    }

    /// Snapshot the V2 pool's current state.
    pub fn v2_snapshot(&self, slot: StateSlot) -> V2State {
        self.v2_state.snapshot(slot)
    }

    /// Finalise into the immutable resident artifacts. In production this is `pub(crate)`: an
    /// owned `Universe` has PUBLIC resident store fields, so exposing it would let a release-build
    /// caller replace a sealed store BEFORE any `Arc` wrapping. External callers must go through
    /// [`seal`](Self::seal), which hands the universe out already `Arc`-wrapped and anchors a
    /// retained clone in the permit (WI-06 F1 totality, codex item 1). Cross-crate
    /// tests/benches/examples reach `build` publicly ONLY through the `test-permit` dev-dependency
    /// feature — the same gate that exposes `test_mint` — so a production `cargo build` keeps it
    /// crate-internal.
    #[cfg(not(any(test, loom, feature = "test-permit")))]
    pub(crate) fn build(self) -> Universe {
        self.build_universe()
    }

    /// See [`build`](Self::build). PUBLIC only under the test cfg (`cargo test`/`loom`/downstream
    /// `test-permit`) so cross-crate tests/benches/examples can construct a bare `Universe`.
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn build(self) -> Universe {
        self.build_universe()
    }

    /// The shared finaliser body (behind both `build` visibilities and `seal`).
    fn build_universe(self) -> Universe {
        Universe {
            tokens: self.tokens,
            pools: self.pools,
            v2_state: self.v2_state,
            v3_state: self.v3_state,
            fluid_state: self.fluid_state,
            dodo_state: self.dodo_state,
            balancer_state: self.balancer_state,
            balancer_weighted_state: self.balancer_weighted_state,
            curve_state: self.curve_state,
            curve_crypto_state: self.curve_crypto_state,
            lb_state: self.lb_state,
            maverick_state: self.maverick_state,
            registry: self.registry.build(),
            v4_pool_ids: self.v4_pool_ids,
        }
    }

    /// WI-06 §3.3 sole-writer SEAL: build the universe AND mint the single
    /// [`StateWriterPermit`]. This is the ONE production mint of the permit — after sealing, the
    /// only way to mutate a family store is through a [`StateWriteTxn`] derived from this permit
    /// (held by the state thread) or through the sole [`StatePublisher`] draining off-thread
    /// [`StateMutationBatch`] commands. Consumes the builder (like [`build`](Self::build)), so a
    /// universe is sealed exactly once and cannot yield a second permit.
    pub fn seal(mut self) -> SealedUniverse {
        // Hand out the SAME cradle permit minted at `new()` (used to witness pre-seal init).
        // `take()` leaves `None`; `build()` then drops that `None` — the permit exists exactly
        // once and is now owned by the caller (the state thread).
        let mut permit = self
            .seal_permit
            .take()
            .expect("builder sealed exactly once (cradle permit present)");
        // WI-06 F1 totality (codex item 1): the universe leaves the crate ALREADY `Arc`-wrapped.
        // No owned `&mut Universe` ever escapes `seal`, so a resident store field cannot be
        // replaced (with a swapped-in WILDCARD store, or any other store) before Arc wrapping.
        // The permit + shared universe are handed over together via `into_parts`.
        let universe = std::sync::Arc::new(self.build_universe());
        // WI-06 round 3 (codex item 1 residual): retain a strong clone of the universe INSIDE the
        // permit for the universe's whole lifetime. The shared `Arc<Universe>` handed out by
        // `into_parts` therefore keeps strong_count > 1 forever, so `Arc::get_mut`/`Arc::try_unwrap`
        // can NEVER recover an owned `&mut Universe` (nor pop a store field) out of band — the
        // owned-`&mut`-recovery escape is structurally dead.
        permit.attach_universe_anchor(std::sync::Arc::clone(&universe));
        SealedUniverse { universe, permit }
    }

    /// Borrow the in-progress token table.
    pub fn tokens(&self) -> &TokenTable {
        &self.tokens
    }

    /// Borrow the in-progress pool table.
    pub fn pools(&self) -> &PoolTable {
        &self.pools
    }
}

impl Default for UniverseBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// The output of [`UniverseBuilder::seal`]: the built [`Universe`] plus the single
/// [`StateWriterPermit`]. The engine Arc-wraps the universe (shared read-only by every worker)
/// and hands the permit to the sole state thread; off-thread writers receive only a send-only
/// [`StateCommandSender`] cloned from the [`StatePublisher`]. Because `seal` consumes the
/// builder and `mint` is the crate-private sole production constructor, exactly one permit
/// exists per sealed universe.
pub struct SealedUniverse {
    /// The built universe, ALREADY `Arc`-wrapped (shared read-only by every worker). PRIVATE by
    /// design (WI-06 F1 totality, codex item 1): with no public field and no owned-`Universe`
    /// accessor, foreign code can never obtain an owned `&mut Universe` to replace a sealed store
    /// before publication.
    universe: std::sync::Arc<Universe>,
    /// The single writer permit — proof of sole-writer authority; only its owner can
    /// [`begin_txn`](crate::publication::StateWriterPermit::begin_txn). PRIVATE; released exactly
    /// once, together with the shared universe, via [`into_parts`](Self::into_parts).
    permit: crate::publication::StateWriterPermit,
}

impl SealedUniverse {
    /// Consume the seal, yielding the shared `Arc<Universe>` (held read-only by every reader) and
    /// the sole [`StateWriterPermit`](crate::publication::StateWriterPermit) (handed to the state
    /// thread). This is the ONLY way to extract either half, and the universe leaves already
    /// `Arc`-wrapped — no caller can obtain an owned, mutable `Universe` whose resident store
    /// fields could be swapped out of band before publication.
    pub fn into_parts(
        self,
    ) -> (
        std::sync::Arc<Universe>,
        crate::publication::StateWriterPermit,
    ) {
        (self.universe, self.permit)
    }
}

/// Built universe.
pub struct Universe {
    /// Token table.
    pub tokens: TokenTable,
    /// Pool table.
    pub pools: PoolTable,
    /// V2 state store.
    pub v2_state: V2StateStore,
    /// V3 state store.
    pub v3_state: V3StateStore,
    /// Fluid DEX v1/v2 state store.
    pub fluid_state: FluidStateStore,
    /// DODO V2 state store.
    pub dodo_state: DodoStateStore,
    /// Balancer V2 stable-pool state store.
    pub balancer_state: crate::balancer_state::BalancerStateStore,
    /// Balancer V2 Weighted-pool state store.
    pub balancer_weighted_state: crate::balancer_weighted_state::BalancerWeightedStateStore,
    /// Curve StableSwap-NG plain-pool state store.
    pub curve_state: crate::curve_state::CurveStateStore,
    /// Curve CryptoSwap-NG (TwoCrypto-NG / Tricrypto-NG) state store.
    pub curve_crypto_state: crate::curve_crypto_state::CryptoCurveStateStore,
    /// Trader Joe Liquidity Book state store.
    pub lb_state: crate::lb_state::LbStateStore,
    /// Maverick V2 state store.
    pub maverick_state: MaverickStateStore,
    /// Address registry.
    pub registry: AddressRegistry,
    /// Uniswap V4 pool ids keyed by dense pool id.
    pub v4_pool_ids: ahash::AHashMap<PoolId, [u8; 32]>,
}

impl Universe {
    /// Return the canonical Uniswap V4 PoolManager poolId for a resident V4 pool.
    pub fn v4_pool_id(&self, pool: PoolId) -> Option<[u8; 32]> {
        self.v4_pool_ids.get(&pool).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_balancer_weighted_pool_registers_with_weighted_kind() {
        use crate::balancer_weighted_state::{BalancerWeightedSlot, MAX_BALANCER_TOKENS};
        let mut b = UniverseBuilder::new();
        let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let mut tokens = [[0u8; 20]; MAX_BALANCER_TOKENS];
        tokens[0] = [0x82; 20];
        tokens[1] = [0xaf; 20];
        let mut balances = [alloy_primitives::U256::ZERO; MAX_BALANCER_TOKENS];
        balances[0] = alloy_primitives::U256::from(1_000_000_000_000_000_000_000u128);
        balances[1] = alloy_primitives::U256::from(100_000_000_000_000_000_000u128);
        let mut weights = [alloy_primitives::U256::ZERO; MAX_BALANCER_TOKENS];
        weights[0] = alloy_primitives::U256::from(800_000_000_000_000_000u128);
        weights[1] = alloy_primitives::U256::from(200_000_000_000_000_000u128);
        let slot = BalancerWeightedSlot {
            balances_d18: balances,
            scaling_d18: [alloy_primitives::U256::from(1_000_000_000_000_000_000u128);
                crate::balancer_weighted_state::MAX_BALANCER_TOKENS],
            weights_d18: weights,
            tokens,
            n_tokens: 2,
            swap_fee_d18: alloy_primitives::U256::from(1_000_000_000_000_000u128),
            pool_id: [0x7c; 32],
        };
        let pool = b
            .add_balancer_weighted_pool([0x99; 20], weth, usdc, VenueId(0), 1000, slot)
            .expect("weighted pool registers");
        let u = b.build();
        assert_eq!(u.registry.pool_of(&[0x99; 20]), Some(pool));
        // The registered pool must carry the weighted kind → exchangeId 27, and the poolId must
        // be readable from the weighted store for the Vault swap.
        assert_eq!(u.pools.meta(pool).kind, PoolKind::BalancerWeighted);
        assert_eq!(u.pools.meta(pool).kind.to_mantis_exchange_id(), Some(27));
        let stored_pid = u
            .balancer_weighted_state
            .with_snapshot(u.pools.meta(pool).state_slot, |s| s.pool_id);
        assert_eq!(stored_pid, [0x7c; 32]);
    }

    #[test]
    fn balancer_weighted_same_address_reuses_one_pool_for_every_pair() {
        // [AUD-BAL-WEIGHTED-REUSE] A Balancer WEIGHTED pool is ONE Vault contract (one address)
        // holding the FULL multi-token state in a single slot; the route layer is pairwise. A 2nd
        // tradable pair of the SAME pool address must REUSE the existing pool id + slot — otherwise
        // the sibling gets an orphan slot that the Vault-Swap (resolved 1:1 by address) never
        // refreshes → permanently STARTUP-STALE reserves. Mirrors the stable path (AUD-016).
        use crate::balancer_weighted_state::{BalancerWeightedSlot, MAX_BALANCER_TOKENS};
        use alloy_primitives::U256;

        let mut b = UniverseBuilder::new();
        let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let dai = b.add_token([0xda; 20], 18, RiskClass::CentralisedStable);
        let pool_addr = [0x99u8; 20];
        let mk_slot = || {
            let mut tokens = [[0u8; 20]; MAX_BALANCER_TOKENS];
            tokens[0] = [0x82; 20];
            tokens[1] = [0xaf; 20];
            tokens[2] = [0xda; 20];
            let mut balances = [U256::ZERO; MAX_BALANCER_TOKENS];
            balances[0] = U256::from(1_000_000_000_000_000_000_000u128);
            balances[1] = U256::from(100_000_000_000_000_000_000u128);
            balances[2] = U256::from(100_000_000_000_000_000_000u128);
            let mut weights = [U256::ZERO; MAX_BALANCER_TOKENS];
            weights[0] = U256::from(600_000_000_000_000_000u128);
            weights[1] = U256::from(200_000_000_000_000_000u128);
            weights[2] = U256::from(200_000_000_000_000_000u128);
            BalancerWeightedSlot {
                balances_d18: balances,
                scaling_d18: [U256::from(1_000_000_000_000_000_000u128); MAX_BALANCER_TOKENS],
                weights_d18: weights,
                tokens,
                n_tokens: 3,
                swap_fee_d18: U256::from(1_000_000_000_000_000u128),
                pool_id: [0x7c; 32],
            }
        };
        let id_ab = b
            .add_balancer_weighted_pool(pool_addr, weth, usdc, VenueId(0), 1000, mk_slot())
            .expect("first pair registers");
        let id_ac = b
            .add_balancer_weighted_pool(pool_addr, weth, dai, VenueId(0), 1000, mk_slot())
            .expect("second pair of the same pool address must succeed");
        assert_eq!(
            id_ab, id_ac,
            "both pairs of one Balancer weighted pool address must map to the SAME pool id"
        );
        let u = b.build();
        assert_eq!(
            u.pools.len(),
            1,
            "exactly ONE Balancer weighted pool/slot for one Vault address (no orphan sibling slot)"
        );
        assert_eq!(
            u.registry.pool_of(&pool_addr),
            Some(id_ab),
            "the address→id map must point at the single shared weighted pool"
        );
    }

    #[test]
    fn balancer_stable_same_address_reuses_one_pool_for_every_pair() {
        // [Codex AUD-016] A Balancer stable pool is ONE Vault contract (one address) holding the
        // FULL multi-token state in a single slot; the route layer is pairwise. Registering a second
        // tradable pair of the SAME pool address must REUSE the existing pool id + slot (so a
        // Vault-Swap on the address — resolved 1:1 by address — refreshes the shared slot for ALL
        // pairs). Allocating a second slot/id for the sibling pair would leave it STALE (only one id
        // wakes), and would corrupt the address→id map. So: same id for both pairs, exactly one pool.
        use crate::balancer_state::BalancerStableSlot;
        use alloy_primitives::U256;
        use engine_amm::balancer_stable::{BalancerStableState, MAX_TOKENS};

        let mut b = UniverseBuilder::new();
        let dai = b.add_token([0xda; 20], 18, RiskClass::CentralisedStable);
        let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let usdt = b.add_token([0xfd; 20], 6, RiskClass::CentralisedStable);
        let pool_addr = [0xb5u8; 20];
        let mk_slot = || {
            let mut balances = [0u128; MAX_TOKENS];
            balances[0] = 1_000_000_000_000_000_000_000;
            balances[1] = 1_000_000_000_000;
            balances[2] = 1_000_000_000_000;
            let mut sf = [U256::ZERO; MAX_TOKENS];
            sf[0] = U256::from(1_000_000_000_000_000_000u128);
            sf[1] = U256::from(1_000_000_000_000_000_000u128);
            sf[2] = U256::from(1_000_000_000_000_000_000u128);
            let kernel = BalancerStableState {
                n: 3,
                balances,
                scaling_factors: sf,
                amp: 1000 * 1000,
                swap_fee_1e18: 1_000_000_000_000_000,
            };
            let mut tokens = [[0u8; 20]; MAX_TOKENS];
            tokens[0] = [0xda; 20];
            tokens[1] = [0xaf; 20];
            tokens[2] = [0xfd; 20];
            BalancerStableSlot {
                kernel,
                tokens,
                pool_id: [0x07u8; 32],
            }
        };
        // First tradable pair (DAI/USDC) registers the pool.
        let id_ab = b
            .add_balancer_stable_pool(pool_addr, dai, usdc, VenueId(0), 1_000, mk_slot())
            .expect("first pair registers");
        // Second tradable pair (DAI/USDT) of the SAME pool address must reuse the same pool.
        let id_ac = b
            .add_balancer_stable_pool(pool_addr, dai, usdt, VenueId(0), 1_000, mk_slot())
            .expect("second pair of the same pool address must succeed");
        assert_eq!(
            id_ab, id_ac,
            "both pairs of one Balancer pool address must map to the SAME pool id"
        );
        let u = b.build();
        assert_eq!(
            u.pools.len(),
            1,
            "exactly ONE Balancer pool/slot for one Vault address (no stale sibling slot)"
        );
        assert_eq!(
            u.registry.pool_of(&pool_addr),
            Some(id_ab),
            "the address→id map must point at the single shared pool"
        );
    }

    #[test]
    fn build_minimal_universe() {
        let mut b = UniverseBuilder::new();
        let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let pool = b
            .add_v2_pool(
                [0x01u8; 20],
                PoolKind::UniswapV2,
                weth,
                usdc,
                VenueId(0),
                30,
                V2State::default(),
            )
            .unwrap();
        let u = b.build();
        assert_eq!(u.tokens.len(), 2);
        assert_eq!(u.pools.len(), 1);
        assert_eq!(u.registry.pool_of(&[0x01u8; 20]), Some(pool));
    }

    #[test]
    fn seal_yields_the_built_universe_and_the_sole_writer_permit() {
        // WI-06 T3: `seal()` must produce a universe byte-equivalent to `build()` for the same
        // inputs AND the single writer permit (the sole-writer authority proof). Building the
        // same universe twice — once via build(), once via seal() — must agree on the resident
        // set, and the sealed permit must be usable to open a write transaction.
        let make = || {
            let mut b = UniverseBuilder::new();
            let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
            let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
            b.add_v2_pool(
                [0x01u8; 20],
                PoolKind::UniswapV2,
                weth,
                usdc,
                VenueId(0),
                30,
                V2State::default(),
            )
            .unwrap();
            b
        };
        let built = make().build();
        let (universe, mut permit) = make().seal().into_parts();
        // Same resident universe as the plain build.
        assert_eq!(universe.tokens.len(), built.tokens.len());
        assert_eq!(universe.pools.len(), built.pools.len());
        assert_eq!(
            universe.registry.pool_of(&[0x01u8; 20]),
            built.registry.pool_of(&[0x01u8; 20])
        );
        // The permit is real: it opens a write-transaction witness (the ONLY post-seal path to a
        // family-store write). Holding this is sole-writer proof.
        let _txn = permit.begin_txn();
    }

    #[test]
    fn sealed_universe_arc_has_no_recoverable_mut_because_the_permit_anchors_it() {
        // WI-06 round 3 (codex item 1 residual): `into_parts` hands out an `Arc<Universe>` whose
        // strong-count must stay > 1 for the universe's whole lifetime, so a production caller can
        // NEVER `Arc::get_mut`/`Arc::try_unwrap` to recover an owned `&mut Universe` and replace or
        // directly mutate a sealed resident store out of band. The retained clone is held by the
        // sole-writer permit (dropping it — which also ends all write authority — is the only way
        // the universe becomes uniquely owned again).
        let make = || {
            let mut b = UniverseBuilder::new();
            let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
            let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
            b.add_v2_pool(
                [0x01u8; 20],
                PoolKind::UniswapV2,
                weth,
                usdc,
                VenueId(0),
                30,
                V2State::default(),
            )
            .unwrap();
            b
        };
        let (mut universe, permit) = make().seal().into_parts();
        assert!(
            std::sync::Arc::get_mut(&mut universe).is_none(),
            "post-seal Arc::get_mut MUST fail — the permit holds a second strong ref so no owned \
             &mut Universe can be recovered to swap a sealed store"
        );
        assert!(
            std::sync::Arc::strong_count(&universe) >= 2,
            "the sealed universe must carry >= 2 strong refs (the caller's + the permit's anchor)"
        );
        // Prove the PERMIT is the anchor holder: once it drops (write authority gone), the
        // universe is uniquely owned again.
        drop(permit);
        assert!(
            std::sync::Arc::get_mut(&mut universe).is_some(),
            "after the permit (anchor holder) drops, the universe is uniquely owned again"
        );
    }

    #[test]
    fn pair_index_dedup() {
        let mut b = UniverseBuilder::new();
        let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let p1 = b
            .add_v2_pool(
                [0x01u8; 20],
                PoolKind::UniswapV2,
                weth,
                usdc,
                VenueId(0),
                30,
                V2State::default(),
            )
            .unwrap();
        let p2 = b
            .add_v2_pool(
                [0x02u8; 20],
                PoolKind::UniswapV2,
                usdc,
                weth,
                VenueId(0),
                30,
                V2State::default(),
            )
            .unwrap();
        assert_eq!(b.pools.meta(p1).pair, b.pools.meta(p2).pair);
    }

    // Hand-built CurveSlot for tests — `CurveSlot` doesn't `impl Default` (no real default for
    // a kernel that hasn't been snapshotted), so tests build one with the zero stableswap state.
    fn empty_curve_slot() -> crate::curve_state::CurveSlot {
        use engine_amm::curve::MAX_COINS;
        crate::curve_state::CurveSlot::new(
            engine_amm::curve::StableSwapState {
                n_coins: 0,
                balances: [0u128; MAX_COINS],
                rates: [0u128; MAX_COINS],
                a_param: 0,
                fee: 0,
                offpeg_fee_multiplier: 0,
            },
            [[0u8; 20]; MAX_COINS],
        )
    }

    #[test]
    fn add_curve_pool_rejects_cryptoswap_via_stableswap_path() {
        // CryptoSwap NG must NOT register through the StableSwap path — the two kernels are
        // type-distinct (different state stores, different Newton iters). Fail-closed here
        // protects against an operator accidentally routing a CryptoSwap pool's snapshot into
        // the StableSwap kernel, which would silently quote with the wrong math.
        let mut b = UniverseBuilder::new();
        let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let usdc = b.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
        let err = b.add_curve_pool(
            [0x77u8; 20],
            PoolKind::CurveCryptoSwap,
            weth,
            usdc,
            VenueId(0),
            10_000_000,
            empty_curve_slot(),
        );
        assert!(
            matches!(
                err,
                Err(RegistryError::UnsupportedKind(PoolKind::CurveCryptoSwap))
            ),
            "expected UnsupportedKind(CurveCryptoSwap), got {err:?}"
        );
    }

    #[test]
    fn add_curve_crypto_pool_accepts_cryptoswap() {
        // The CryptoSwap NG registrar accepts what the StableSwap path rejects — and rejects
        // a mis-kind (StableSwapNG) symmetrically so the two registrars stay type-safe.
        use crate::curve_crypto_state::{CryptoCurveSlot, MAX_CRYPTO_COINS};
        let mut b = UniverseBuilder::new();
        let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
        let wbtc = b.add_token([0x2f; 20], 8, RiskClass::Permissionless);
        let mut tokens = [[0u8; 20]; MAX_CRYPTO_COINS];
        tokens[0] = [0x82; 20];
        tokens[1] = [0x2f; 20];
        let slot = CryptoCurveSlot {
            twocrypto_kernel: None,
            kernel: engine_amm::curve_cryptoswap::CryptoSwapState {
                balances: [
                    alloy_primitives::U256::from(5_000_000_000_000u64),
                    alloy_primitives::U256::from(8_000_000_000u64),
                    alloy_primitives::U256::from(1_600_000_000_000_000_000_000u128),
                ],
                precisions: [
                    alloy_primitives::U256::from(1_000_000_000_000u64),
                    alloy_primitives::U256::from(10_000_000_000u64),
                    alloy_primitives::U256::ONE,
                ],
                price_scale: [
                    alloy_primitives::U256::from(60_000_000_000_000_000_000_000u128),
                    alloy_primitives::U256::from(3_000_000_000_000_000_000_000u128),
                ],
                ann: alloy_primitives::U256::from(1_707_629u64),
                gamma: alloy_primitives::U256::from(11_809_167_828_997u64),
                mid_fee: alloy_primitives::U256::from(3_000_000u64),
                out_fee: alloy_primitives::U256::from(30_000_000u64),
                fee_gamma: alloy_primitives::U256::from(5_000_000_000_000_000u64),
                d: alloy_primitives::U256::ZERO,
                last_k0_prev: alloy_primitives::U256::ZERO,
            },
            tokens,
            n_coins: 3,
        };
        let pool = b
            .add_curve_crypto_pool(
                [0x88; 20],
                PoolKind::CurveCryptoSwap,
                weth,
                wbtc,
                VenueId(0),
                10_000_000,
                slot,
            )
            .expect("CryptoSwap NG must register through the crypto registrar");
        // The registrar rejects a StableSwap kind passed by mistake.
        let err = b.add_curve_crypto_pool(
            [0x99; 20],
            PoolKind::CurveStableSwapNg,
            weth,
            wbtc,
            VenueId(0),
            10_000_000,
            CryptoCurveSlot {
                twocrypto_kernel: None,
                kernel: engine_amm::curve_cryptoswap::CryptoSwapState {
                    balances: [alloy_primitives::U256::ZERO; 3],
                    precisions: [alloy_primitives::U256::ZERO; 3],
                    price_scale: [alloy_primitives::U256::ZERO; 2],
                    ann: alloy_primitives::U256::ZERO,
                    gamma: alloy_primitives::U256::ZERO,
                    mid_fee: alloy_primitives::U256::ZERO,
                    out_fee: alloy_primitives::U256::ZERO,
                    fee_gamma: alloy_primitives::U256::ZERO,
                    d: alloy_primitives::U256::ZERO,
                    last_k0_prev: alloy_primitives::U256::ZERO,
                },
                tokens: [[0u8; 20]; MAX_CRYPTO_COINS],
                n_coins: 0,
            },
        );
        assert!(
            matches!(
                err,
                Err(RegistryError::UnsupportedKind(PoolKind::CurveStableSwapNg))
            ),
            "expected UnsupportedKind(CurveStableSwapNg), got {err:?}"
        );
        let u = b.build();
        assert_eq!(u.registry.pool_of(&[0x88; 20]), Some(pool));
    }

    #[test]
    fn add_curve_pool_accepts_stableswap_ng() {
        // Positive control: the supported kind still registers successfully.
        let mut b = UniverseBuilder::new();
        let usdc = b.add_token([0xaa; 20], 6, RiskClass::CentralisedStable);
        let usdt = b.add_token([0xbb; 20], 6, RiskClass::CentralisedStable);
        let pool = b
            .add_curve_pool(
                [0x66u8; 20],
                PoolKind::CurveStableSwapNg,
                usdc,
                usdt,
                VenueId(0),
                1_000_000,
                empty_curve_slot(),
            )
            .expect("StableSwapNG must register");
        let u = b.build();
        assert_eq!(u.registry.pool_of(&[0x66u8; 20]), Some(pool));
    }
}
