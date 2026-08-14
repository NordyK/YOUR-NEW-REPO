//! Engine configuration types and loader.
//!
//! All configuration is read from a single JSON file (`engine.json`) at startup. The file
//! contains the token universe, registry of factory + router addresses with their
//! verified codehashes, EOA lane keys, gas/L1 settings, hot/warm tier caps, and the
//! confidence/EV thresholds.
//!
//! **Pool addresses are NOT in the config.** The engine §5.4 readiness pipeline calls
//! for dynamic pool discovery via factory `PoolCreated`/`PairCreated` log scans + Curve
//! registry walks. The static pool list ("No static pool whitelist" — plan §line 2116)
//! was removed and replaced by `engine-discovery`. The config supplies the *factory*
//! allowlist (with per-factory deployment blocks) and the discovery loop pulls every
//! pool from the chain.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// clippy: cosmetic doc-list formatting nits in the module/loader docs.
#![allow(clippy::doc_lazy_continuation)]

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Arbitrum chain id (must equal 42161).
    pub chain_id: u64,
    /// Local sequencer feed relay endpoint (Unix socket path or local TCP `host:port`).
    pub relay_endpoint: String,
    /// Sequencer submit endpoint (HTTPS RPC URL).
    pub sequencer_endpoint: String,
    /// Token universe.
    pub tokens: Vec<TokenEntry>,
    /// Venue (factory) registry.
    pub venues: Vec<VenueEntry>,
    /// Known router contracts (Uniswap SwapRouter, Universal Router, Pancake V3 router,
    /// etc.). The classifier dirties pools when one of these is the tx `to`.
    #[serde(default)]
    pub routers: Vec<RouterEntry>,
    /// Fluid DEX pool registry. Fluid uses a singleton + resolver architecture
    /// (not a per-pool factory deploy), so discovery happens through resolver
    /// calls (`DexReservesResolver.getAllPools()`, `getAllPermissionedDexes()`)
    /// rather than `PoolCreated` event scans. Operators can either:
    ///   * (today) list known Fluid pool addresses here, plus token0/token1
    ///     and fee_pips; snapshots are populated as default-paused until the
    ///     background resolver poller writes real state, OR
    ///   * (future) configure `fluid_v1_resolver` / `fluid_v2_resolver` and
    ///     let the engine call them at startup + on refresh.
    /// Both paths register the pool through `add_fluid_pool` and route
    /// enumeration emits cycles through it the same as V2/V3.
    #[serde(default)]
    pub fluid_pools: Vec<FluidPoolEntry>,
    /// §5.4 readiness pipeline / §6 scoring knobs. Discovery scan parameters, hot-tier
    /// caps, scoring cadence.
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    /// Per-lane EOA configuration.
    pub lanes: Vec<LaneConfig>,
    /// Per-route default amount grid edges (in base token wei).
    pub default_amount_grid_min: String,
    /// Per-route default amount grid max edges.
    pub default_amount_grid_max: String,
    /// Number of buckets in the default amount grid (6–12).
    pub default_amount_grid_buckets: usize,
    /// DEPRECATED / IGNORED. All submission thresholds now live in `.env` (the single source of
    /// truth) and are read directly from the environment — see `THRESHOLDS.md`. This field is kept
    /// only for backward-compatible parsing of old config files; the engine does not read it.
    /// `#[serde(default)]` lets new `engine.json` files omit the `thresholds` block entirely.
    #[serde(default)]
    pub thresholds: Thresholds,
    /// Executor contract address (`0x...`); empty when the executor is not deployed yet
    /// (detection-only mode).
    #[serde(default)]
    pub executor: String,
    /// Whether the submitter is allowed to actually write to the sequencer TLS socket.
    /// Defaults to false — the executor must be deployed and the operator must opt in
    /// explicitly before live submit is enabled.
    #[serde(default)]
    pub live_submit: bool,
    /// Per-tier hop-count topology knobs. 2-hop and triangle are on by default; deep
    /// tiers (4–8) are off by default per plan §3 ("Deep-hop tiers stay dry-run /
    /// replay-only by default"). Flip per tier to bench / dry-run a wider search.
    #[serde(default)]
    pub topology: TopologyConfig,
    /// Co-location regime for the §6.1 route-winnability re-weight. When false (default,
    /// e.g. a non-co-located sandbox with multi-second feed lag), contested hub pools
    /// (WETH/stable, stable/stable) are de-emphasized in the hot-tier election because
    /// we cannot win those FCFS races; the budget tilts to under-contested LST flow.
    /// Set true on production infra (AWS, sub-ms sequencer path) to lift that penalty so
    /// the engine contests hubs *and* LST flow. The live `realized_inclusion_rate` term
    /// self-corrects from there.
    #[serde(default)]
    pub colocated: bool,
    /// Chainlink aggregator feeds, snapshotted at startup. Empty = pure on-chain pool
    /// pricing (the default). When present, the WETH/USD feed anchors ETH/USD and each
    /// token feed provides a high-confidence mark that cross-checks / overrides the
    /// pool-derived value.
    #[serde(default)]
    pub chainlink_feeds: Vec<ChainlinkFeedEntry>,
}

/// Per-tier enable flags for hop-count topology. Plan §0.4 mandates that 4–8 hop tiers
/// remain replay-only by default; the operator flips them on per tier during benching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyConfig {
    /// Two-hop cross-venue cycles (highest priority per plan §0.4 rule 1).
    #[serde(default = "default_true")]
    pub enable_two_hop: bool,
    /// Triangular three-hop cycles (plan §0.4 rule 2).
    #[serde(default = "default_true")]
    pub enable_triangle: bool,
    /// Four-hop cycles. Off by default; flip on for benching only.
    #[serde(default)]
    pub enable_4_hop: bool,
    /// Five-hop cycles. Off by default.
    #[serde(default)]
    pub enable_5_hop: bool,
    /// Six-hop cycles. Off by default.
    #[serde(default)]
    pub enable_6_hop: bool,
    /// Seven-hop cycles. Off by default.
    #[serde(default)]
    pub enable_7_hop: bool,
    /// Eight-hop cycles. Off by default (the design upper bound).
    #[serde(default)]
    pub enable_8_hop: bool,
    /// Cap on enumerated deep-hop (4–8) routes *per tier*. Plan §5 sizing puts deep
    /// tiers in the low thousands; the engine truncates further enumeration once the
    /// cap is hit and records the truncation in the startup log.
    #[serde(default = "default_deep_cap")]
    pub max_routes_per_deep_tier: usize,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            enable_two_hop: true,
            enable_triangle: true,
            // [Codex 5–8-hop] OUR DECISION / PLAN FOR NOW: cap live topology at 4 hops. The Mantis
            // submit calldata encoder only supports 2–4 hops (`MANTIS_MIN_HOPS..=MANTIS_MAX_HOPS`), so
            // 5–8-hop cycles would be enumerated/scored but are NON-submittable (they fail-close to
            // empty calldata) — pure wasted compute. 4-hop stays ON: mixed-family 4-hop cycles ARE
            // submittable and are now admitted to the CSR (see fast_path.rs [Codex-F4]). The 5–8-hop
            // flags remain (default OFF) so we can re-enable deeper cycles later once the encoder + gas
            // model support them. Per-tier enumeration is still bounded by `max_routes_per_deep_tier`.
            enable_4_hop: true,
            enable_5_hop: false,
            enable_6_hop: false,
            enable_7_hop: false,
            enable_8_hop: false,
            max_routes_per_deep_tier: default_deep_cap(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_deep_cap() -> usize {
    8192
}

impl TopologyConfig {
    /// Whether the given hop-count is enabled by this config.
    pub fn enabled(&self, hop_count: u8) -> bool {
        match hop_count {
            2 => self.enable_two_hop,
            3 => self.enable_triangle,
            4 => self.enable_4_hop,
            5 => self.enable_5_hop,
            6 => self.enable_6_hop,
            7 => self.enable_7_hop,
            8 => self.enable_8_hop,
            _ => false,
        }
    }

    /// Structural upper bound on hop count (matches `engine_routes::MAX_HOPS`, the
    /// `ArrayVec` capacity of a `Route`). A value above this cannot be enumerated.
    pub const MAX_HOPS: u8 = 8;

    /// Apply a single operator `MAX_HOPS` cap: enable every tier `2..=max` and disable
    /// every tier above `max`. This is the one-knob override (env `MAX_HOPS`) the operator
    /// uses to "swap to 8 or any number" at runtime without editing per-tier flags.
    /// `max` is clamped to `[2, MAX_HOPS]` (2-hop is the minimum cycle; 8 is the structural
    /// capacity), so out-of-range values are coerced rather than silently disabling routing.
    pub fn apply_max_hops(&mut self, max: u8) {
        let max = max.clamp(2, Self::MAX_HOPS);
        self.enable_two_hop = max >= 2;
        self.enable_triangle = max >= 3;
        self.enable_4_hop = max >= 4;
        self.enable_5_hop = max >= 5;
        self.enable_6_hop = max >= 6;
        self.enable_7_hop = max >= 7;
        self.enable_8_hop = max >= 8;
    }
}

/// Token entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenEntry {
    /// 20-byte address.
    pub address: String,
    /// Decimals.
    pub decimals: u8,
    /// Symbol (informational).
    pub symbol: String,
    /// Risk class: `permissionless` or `centralised_stable`.
    pub risk_class: String,
}

/// Fluid DEX pool entry. Manual operator registration of a known Fluid v1 or v2
/// pool, used until automated resolver-based discovery lands. The engine will
/// register the pool with the default paused snapshot; a future background
/// resolver-poll task fills in real state via `DexReservesResolver`
/// (v1) / `DexV2Resolver` (v2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluidPoolEntry {
    /// 20-byte pool address (`0x...`). For v1/T1 this is the DEX pool address;
    /// for v2 D3/D4 it is the singleton-scoped pool address (the operator must
    /// have resolved the `DexKey` to a concrete pool address before configuring).
    pub address: String,
    /// Family: `"fluid_v1"` (collateral/debt split adjusted-reserve math) or
    /// `"fluid_v2"` (D3/D4 concentrated-liquidity segment math).
    pub family: String,
    /// token0 address (lexicographically smaller).
    pub token0: String,
    /// token1 address (lexicographically larger).
    pub token1: String,
    /// Fee in 1e6-precision (`FEE_DENOMINATOR_1E6` per the kernel). 3000 = 30 bps.
    /// For v2 dynamic-fee pools, use `16777215` (`0xFFFFFF`) to mark dynamic; the
    /// live fee is consulted by the kernel at quote time once the resolver poller
    /// populates real state.
    pub fee_pips: u32,
}

/// Router contract entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterEntry {
    /// 20-byte router address (`0x...`).
    pub address: String,
    /// Informational label (e.g. `uniswap_v3_swap_router`, `universal_router_v2`).
    #[serde(default)]
    pub label: String,
}

/// Chainlink aggregator feed entry, snapshotted once at startup to anchor ETH/USD and
/// cross-check / override the pool-derived token marks. Operator-supplied so a wrong or
/// missing feed simply falls through to the pool oracle (the plausibility band catches
/// garbage). Verify every address against the official Chainlink Arbitrum feed list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainlinkFeedEntry {
    /// Token whose mark this feed provides (`0x...`). Use the WETH address for the
    /// authoritative ETH/USD anchor feed.
    pub token: String,
    /// Chainlink aggregator *proxy* address (`0x...`).
    pub feed: String,
    /// Quote denomination: `usd` (answer is token/USD) or `eth` (answer is token/ETH).
    /// Defaults to `usd`.
    #[serde(default = "default_quote_usd")]
    pub quote: String,
}

fn default_quote_usd() -> String {
    "usd".into()
}

/// Venue (factory) entry. Drives §5.4 gate 1 (Discovered).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueEntry {
    /// Family name (`uniswap_v2`, `uniswap_v3`, `sushi_v2`, `sushi_v3`,
    /// `pancake_v3`, `camelot_v2`).
    pub family: String,
    /// Factory address (hex).
    pub factory: String,
    /// Block at which this factory was deployed on Arbitrum One. The log scanner starts
    /// here — passing 0 works but is slow.
    #[serde(default)]
    pub deployment_block: u64,
    /// V2-family default fee in bps (e.g. 30 for Uniswap V2 fork). Ignored for V3.
    #[serde(default)]
    pub default_fee_bps: u32,
    /// Runtime bytecode keccak hash (hex, no `0x`). Multiple values allowed for forks
    /// that share the same family but differ in deployed bytecode; the discovery gate
    /// accepts any listed hash.
    #[serde(default)]
    pub runtime_hash: String,
    /// Init code hash for V3-family factories (hex, no `0x`); empty for V2.
    #[serde(default)]
    pub init_code_hash: String,
}

/// §5.4 / §5.5 / §6 knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    /// Block-range chunk for `eth_getLogs`. Alchemy free tier caps at ~10 000;
    /// the chunker halves on the "range too large" signal so 50 000 is safe.
    #[serde(default = "default_chunk")]
    pub block_range_chunk: u64,
    /// Hot tier cap per §5.5 (target 48–64, hard cap 80).
    #[serde(default = "default_hot_cap")]
    pub hot_pool_cap: usize,
    /// Snapshot-ready cap (target 160–220).
    #[serde(default = "default_warm_cap")]
    pub warm_pool_cap: usize,
    /// Score re-rank cadence in seconds (plan §6: 5–30 min; defaults to 5 min).
    #[serde(default = "default_score_interval")]
    pub score_interval_secs: u64,
    /// Whether to enforce the §5.6 runtime codehash gate. If false, only token-pair
    /// gate runs — used during initial research-mode discovery to *record* the hash
    /// artifacts. Production must be true.
    #[serde(default)]
    pub enforce_codehash: bool,
    /// Optional explicit head block for testing (else `eth_blockNumber`).
    #[serde(default)]
    pub head_block_override: Option<u64>,
    /// Fluid v1/T1 reserves resolver address (`DexReservesResolver`). When set,
    /// the discovery loop calls `getAllPoolAddresses()` / `getAllPools()` to
    /// enumerate every v1 pool on-chain and registers them automatically. When
    /// empty, Fluid v1 discovery is skipped (manual `fluid_pools` registration
    /// is still honored).
    ///
    /// Arbitrum One default (resolved from Fluid's official deployment
    /// manifest): `0x05Bd8269A20C472b148246De20E6852091BF16Ff`.
    #[serde(default)]
    pub fluid_v1_resolver: String,
    /// Fluid v2 D3/D4 dex resolver address (`DexResolver`). When set, the
    /// discovery loop calls `getAllPermissionedDexes()` to enumerate v2 dex
    /// keys. Permissionless v2 pools are not yet GA on Arbitrum so this path
    /// reports 0 pools today; once Fluid governance enables permissionless v2
    /// the engine begins registering them automatically.
    ///
    /// Arbitrum One default: `0x11D80CfF056Cef4F9E6d23da8672fE9873e5cC07`.
    #[serde(default)]
    pub fluid_v2_resolver: String,
    /// Fluid Liquidity Layer address for `LogOperate` event subscription.
    /// When the snapshot poller lands, it listens here to dirty Fluid pool
    /// snapshots on any liquidity event referencing a tracked pool.
    ///
    /// Arbitrum One default: `0x52Aa899454998Be5b000Ad077a46Bbe360F4e497`.
    #[serde(default)]
    pub fluid_liquidity_layer: String,

    /// Maverick V2 `PoolLens` address — used to fetch the tick window around
    /// the active tick. If empty, Maverick pools register with an empty tick
    /// window and the kernel returns `TickWindowExhausted` on any swap quote.
    ///
    /// Arbitrum One: `0x6A9EB38DE5D349Fe751E0aDb4c0D9D391f94cc8D` (canonical lens).
    #[serde(default)]
    pub maverick_lens: String,

    /// Uniswap V4 `StateView` lens address — reads per-poolId state (slot0, liquidity,
    /// tick bitmap/info) from the singleton PoolManager. Required to snapshot V4 pools.
    ///
    /// Arbitrum One default: `0x76fd297e2d437cd7f76d50f01afe6160f86e9990`.
    #[serde(default = "default_v4_state_view")]
    pub v4_state_view: String,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            block_range_chunk: default_chunk(),
            hot_pool_cap: default_hot_cap(),
            warm_pool_cap: default_warm_cap(),
            score_interval_secs: default_score_interval(),
            enforce_codehash: false,
            head_block_override: None,
            fluid_v1_resolver: String::new(),
            fluid_v2_resolver: String::new(),
            fluid_liquidity_layer: String::new(),
            maverick_lens: String::new(),
            v4_state_view: default_v4_state_view(),
        }
    }
}

fn default_v4_state_view() -> String {
    "0x76fd297e2d437cd7f76d50f01afe6160f86e9990".to_string()
}

fn default_chunk() -> u64 {
    50_000
}
fn default_hot_cap() -> usize {
    64
}
fn default_warm_cap() -> usize {
    200
}
fn default_score_interval() -> u64 {
    300
}

/// Lane config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneConfig {
    /// 0–4.
    pub eoa_id: u8,
    /// EOA 20-byte address.
    pub eoa: String,
    /// Class string: `high_ev_two_hop`, `stables`, `triangles`, `warm`, `variant_overflow`.
    pub lane_class: String,
    /// Max in-flight submissions.
    pub max_in_flight: u32,
    /// Private key (hex, never logged). Loaded into memory only.
    pub key_hex: String,
}

/// Submission thresholds.
///
/// **DEPRECATED / IGNORED for gating.** These config-file fields are parsed for backward
/// compatibility but do NOT gate submission — every live floor (min net profit, min EV, ROI bps,
/// gas-spike ratio, flash-fee cap, etc.) is read from `.env` at boot (the single source of truth;
/// see `EFFECTIVE THRESHOLDS` at startup). Editing these fields has no effect on the live gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thresholds {
    /// Per-route minimum net profit (in base-token wei). DEPRECATED — see the struct-level note.
    pub min_net_profit_wei: String,
    /// Minimum absolute EV (in base-token wei).
    pub min_ev_absolute_wei: i128,
    /// Minimum ROI in bps.
    pub min_roi_bps: u32,
    /// Confidence floor (0.95 baseline).
    pub confidence_floor: f32,
    /// Gas snapshot maximum age (ms).
    pub gas_max_age_ms: u64,
    /// Maximum Balancer flash-loan fee (18-dec fixed point).
    pub max_flashloan_fee_1e18: String,
}

impl Default for Thresholds {
    /// Vestigial defaults — the engine reads thresholds from `.env`, not this struct. Present so
    /// `#[serde(default)]` lets an `engine.json` omit the block. Values mirror the historical
    /// engine.json so any legacy reader sees sane numbers; the live gate ignores them.
    fn default() -> Self {
        Self {
            min_net_profit_wei: "2500000000000".into(),
            min_ev_absolute_wei: 500_000_000_000,
            min_roi_bps: 0,
            confidence_floor: 0.95,
            gas_max_age_ms: 500,
            max_flashloan_fee_1e18: "10000000000000000".into(),
        }
    }
}

impl EngineConfig {
    /// Load from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref())
            .with_context(|| format!("failed to read engine config at {:?}", path.as_ref()))?;
        let cfg: Self = serde_json::from_slice(&bytes).context("failed to parse engine config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate basic invariants for any boot mode.
    pub fn validate(&self) -> Result<()> {
        if self.chain_id != 42_161 {
            anyhow::bail!(
                "engine is Arbitrum One only (chain_id 42161), got {}",
                self.chain_id
            );
        }
        if self.tokens.is_empty() {
            anyhow::bail!("token universe must not be empty");
        }
        if !(0.0..=1.0).contains(&self.thresholds.confidence_floor) {
            anyhow::bail!("confidence_floor must be in [0, 1]");
        }
        Ok(())
    }

    /// Additional invariants the engine enforces before entering live submit mode.
    pub fn validate_for_live(&self) -> Result<()> {
        self.validate()?;
        if self.lanes.is_empty() {
            anyhow::bail!("at least one nonce lane is required for live submit");
        }
        if self.venues.is_empty() {
            anyhow::bail!("venue (factory) allowlist must not be empty for live submit");
        }
        // [Codex-F3] The executor + each lane's EOA/key must be present and well-formed BEFORE live
        // boot, so a malformed config can never silently fall back to a zero executor or a [1u8;32]
        // dummy signing key (SubmitterCtx::from_config) and broadcast from an unrelated address.
        parse_hex_fixed("executor", &self.executor, 20)?;
        for (i, lane) in self.lanes.iter().enumerate() {
            parse_hex_fixed(&format!("lanes[{i}].eoa"), &lane.eoa, 20)?;
            parse_hex_fixed(&format!("lanes[{i}].key_hex"), &lane.key_hex, 32)?;
        }
        Ok(())
    }

    /// Default Arbitrum One configuration scaffolding.
    pub fn default_arbitrum_one() -> Self {
        Self {
            chain_id: 42_161,
            relay_endpoint: "/var/run/arb-feed.sock".to_string(),
            sequencer_endpoint: "https://arb1-sequencer.arbitrum.io/rpc".to_string(),
            tokens: vec![
                TokenEntry {
                    address: "0x82AF49447D8A07E3BD95BD0D56F35241523FBAB1".into(),
                    decimals: 18,
                    symbol: "WETH".into(),
                    risk_class: "permissionless".into(),
                },
                TokenEntry {
                    address: "0xAF88D065E77C8CC2239327C5EDB3A432268E5831".into(),
                    decimals: 6,
                    symbol: "USDC".into(),
                    risk_class: "centralised_stable".into(),
                },
                TokenEntry {
                    address: "0xFD086BC7CD5C481DCC9C85EBE478A1C0B69FCBB9".into(),
                    decimals: 6,
                    symbol: "USDt0".into(),
                    risk_class: "centralised_stable".into(),
                },
                TokenEntry {
                    address: "0x2F2A2543B76A4166549F7AAB2E75BEF0AEFC5B0F".into(),
                    decimals: 8,
                    symbol: "WBTC".into(),
                    risk_class: "permissionless".into(),
                },
            ],
            // Plan §5.6 verified Arbitrum One factories. Empty venues meant a missing
            // engine.json would silently boot a no-op engine with zero discovered
            // pools. Populating with real venues + deployment blocks so the fallback
            // is actually exercisable.
            venues: vec![
                VenueEntry {
                    family: "uniswap_v3".into(),
                    factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".into(),
                    deployment_block: 165,
                    default_fee_bps: 0,
                    runtime_hash: String::new(),
                    init_code_hash: String::new(),
                },
                VenueEntry {
                    family: "sushi_v3".into(),
                    factory: "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e".into(),
                    deployment_block: 50_000_000,
                    default_fee_bps: 0,
                    runtime_hash: String::new(),
                    init_code_hash: String::new(),
                },
                VenueEntry {
                    family: "pancake_v3".into(),
                    factory: "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865".into(),
                    deployment_block: 90_000_000,
                    default_fee_bps: 0,
                    runtime_hash: String::new(),
                    init_code_hash: String::new(),
                },
                VenueEntry {
                    family: "camelot_v2".into(),
                    factory: "0x6EcCab422D763aC031210895C81787E87B43A652".into(),
                    deployment_block: 30_000_000,
                    default_fee_bps: 30,
                    runtime_hash: String::new(),
                    init_code_hash: String::new(),
                },
                VenueEntry {
                    // Uniswap V4 singleton PoolManager (emits every pool's events).
                    // `factory` carries the PoolManager address; hookless pools are
                    // discovered from its Initialize logs and quoted with the V3 kernel.
                    family: "uniswap_v4".into(),
                    factory: "0x360e68faccca8ca495c1b759fd9eee466db9fb32".into(),
                    deployment_block: 290_000_000,
                    default_fee_bps: 0,
                    runtime_hash: String::new(),
                    init_code_hash: String::new(),
                },
            ],
            // Routers feed the §10.5 classifier with the (to, selector) pairs that
            // dirty pools on observation. Without these the engine would see
            // ~95 % of Arbitrum traffic as "unknown target" and never dirty.
            routers: vec![
                RouterEntry {
                    address: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                    label: "uniswap_v3_swap_router".into(),
                },
                RouterEntry {
                    address: "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45".into(),
                    label: "uniswap_v3_swap_router_02".into(),
                },
                RouterEntry {
                    address: "0x5E325eDA8064b456f4781070C0738d849c824258".into(),
                    label: "universal_router_v1".into(),
                },
                RouterEntry {
                    address: "0x4Dae2f939ACf50408e13d58534Ff8c2776d45265".into(),
                    label: "universal_router_v2".into(),
                },
                RouterEntry {
                    address: "0x1b81D678ffb9C0263b24A97847620C99d213eB14".into(),
                    label: "pancake_v3_swap_router".into(),
                },
                RouterEntry {
                    address: "0x32226588378236Fd0c7c4053999F88aC0e5cAc77".into(),
                    label: "camelot_v2_router".into(),
                },
                RouterEntry {
                    address: "0x8A21F6768C1f8075791D08546Dadf6daA0bE820c".into(),
                    label: "camelot_v3_router".into(),
                },
                RouterEntry {
                    address: "0x111111125421cA6dc452d289314280a0f8842A65".into(),
                    label: "oneinch_v6_aggregation_router".into(),
                },
                RouterEntry {
                    address: "0x6A000F20005980200259B80c5102003040001068".into(),
                    label: "paraswap_v5_augustus".into(),
                },
                RouterEntry {
                    address: "0xCa423977156BB05b13A2BA3b76Bc5419E2fE9680".into(),
                    label: "openocean_v2".into(),
                },
            ],
            fluid_pools: vec![],
            lanes: vec![],
            default_amount_grid_min: "1000000000000000".into(), // 1e15 wei = 0.001 ETH
            default_amount_grid_max: "100000000000000000000".into(), // 100 ETH wei
            default_amount_grid_buckets: 8,
            thresholds: Thresholds {
                // Tuned 2026-05 from Dune analysis of 2-leg atomic arbs on
                // Arbitrum: median realized profit $0.014, p75 $0.031, p90
                // $0.065. The old 0.001 ETH floor (~$4) filtered out 99.5%
                // of real arbs; 2.5e12 wei = 0.0000025 ETH ≈ $0.01 catches
                // ~p70 of the distribution while staying above the median
                // 2-leg gas cost (~$0.03) only for the larger plays.
                min_net_profit_wei: "2500000000000".into(), // 0.0000025 ETH ≈ $0.01
                min_ev_absolute_wei: 500_000_000_000,       // 0.0000005 ETH ≈ $0.002
                min_roi_bps: 0,                             // allow any positive ROI
                confidence_floor: 0.95,
                gas_max_age_ms: 2_000, // Arbitrum gas pinned at 20Mwei floor 99% of time
                max_flashloan_fee_1e18: "10000000000000000".into(), // 1%
            },
            executor: String::new(),
            live_submit: false,
            topology: TopologyConfig::default(),
            colocated: false,
            chainlink_feeds: Vec::new(),
            discovery: DiscoveryConfig {
                // Fluid resolver addresses on Arbitrum One, resolved from
                // Fluid's official deployment manifest at
                // https://github.com/Instadapp/fluid-contracts-public/blob/main/deployments/deployments.md
                fluid_v1_resolver: "0x05Bd8269A20C472b148246De20E6852091BF16Ff".into(),
                fluid_v2_resolver: "0x11D80CfF056Cef4F9E6d23da8672fE9873e5cC07".into(),
                fluid_liquidity_layer: "0x52Aa899454998Be5b000Ad077a46Bbe360F4e497".into(),
                maverick_lens: "0x6A9EB38DE5D349Fe751E0aDb4c0D9D391f94cc8D".into(),
                ..DiscoveryConfig::default()
            },
        }
    }
}

/// Validate that `s` is a `bytes`-byte hex value (optional `0x`), all hex digits, and not all-zero;
/// returns the decoded bytes. Fail-closed SHAPE check for the live-submit executor/EOA/key so a
/// malformed config can never silently fall back to a zero executor or dummy signing key. [Codex-F3]
fn parse_hex_fixed(field: &str, s: &str, bytes: usize) -> Result<Vec<u8>> {
    let h = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if h.len() != bytes * 2 {
        anyhow::bail!(
            "live submit: `{field}` must be a {bytes}-byte hex value ({} hex chars), got {}",
            bytes * 2,
            h.len()
        );
    }
    if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("live submit: `{field}` contains non-hex characters");
    }
    let v: Vec<u8> = (0..bytes)
        .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).expect("validated hex"))
        .collect();
    if v.iter().all(|&b| b == 0) {
        anyhow::bail!("live submit: `{field}` must not be all-zero");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_fixed_validates_shape_and_nonzero() {
        assert!(parse_hex_fixed("x", "0x1111111111111111111111111111111111111111", 20).is_ok());
        assert!(parse_hex_fixed("x", "1111111111111111111111111111111111111111", 20).is_ok());
        assert!(parse_hex_fixed("x", "0x1234", 20).is_err()); // wrong length
        assert!(parse_hex_fixed("x", "0xZZ11111111111111111111111111111111111111", 20).is_err());
        assert!(parse_hex_fixed("x", "0x0000000000000000000000000000000000000000", 20).is_err());
        assert!(parse_hex_fixed("x", &format!("0x{}", "11".repeat(32)), 32).is_ok());
        assert!(parse_hex_fixed("x", &format!("0x{}", "00".repeat(32)), 32).is_err());
    }

    fn live_valid_config() -> EngineConfig {
        let mut c = EngineConfig::default_arbitrum_one();
        c.live_submit = true;
        c.executor = format!("0x{}", "11".repeat(20));
        c.lanes = vec![LaneConfig {
            eoa_id: 0,
            eoa: format!("0x{}", "22".repeat(20)),
            lane_class: "warm".into(),
            max_in_flight: 1,
            key_hex: format!("0x{}", "33".repeat(32)),
        }];
        c
    }

    #[test]
    fn validate_for_live_rejects_malformed_executor_eoa_or_key() {
        // A well-formed live config validates.
        assert!(live_valid_config().validate_for_live().is_ok());
        // Zero / short executor → reject (else from_config falls back to the zero executor).
        let mut c = live_valid_config();
        c.executor = format!("0x{}", "00".repeat(20));
        assert!(
            c.validate_for_live().is_err(),
            "zero executor must be rejected"
        );
        let mut c = live_valid_config();
        c.executor = "0xdead".into();
        assert!(
            c.validate_for_live().is_err(),
            "short executor must be rejected"
        );
        // Malformed lane key → reject (else from_config falls back to the [1u8;32] dummy key).
        let mut c = live_valid_config();
        c.lanes[0].key_hex = "0xnothex".into();
        assert!(
            c.validate_for_live().is_err(),
            "non-hex key must be rejected"
        );
        // Zero EOA → reject.
        let mut c = live_valid_config();
        c.lanes[0].eoa = format!("0x{}", "00".repeat(20));
        assert!(c.validate_for_live().is_err(), "zero EOA must be rejected");
    }

    #[test]
    fn default_validates() {
        EngineConfig::default_arbitrum_one().validate().unwrap();
    }

    #[test]
    fn default_topology_caps_at_four_hops() {
        // [Codex 5–8-hop] OUR DECISION / PLAN FOR NOW: live topology is capped at 4 hops. The submit
        // calldata encoder only supports 2–4 hops, so enumerating/scoring 5–8-hop cycles is wasted
        // compute (they fail-close to empty calldata). 4-hop stays ON (mixed-family 4-hop cycles are
        // submittable). Reversible per-flag for benching / future re-enable.
        let t = TopologyConfig::default();
        assert!(
            t.enable_two_hop && t.enable_triangle && t.enable_4_hop,
            "2/3/4-hop must stay enabled"
        );
        assert!(
            !t.enable_5_hop && !t.enable_6_hop && !t.enable_7_hop && !t.enable_8_hop,
            "5–8-hop tiers must be OFF by default (for now)"
        );
        assert!(t.enabled(4));
        assert!(!t.enabled(5) && !t.enabled(8));
    }

    #[test]
    fn wrong_chain_id_rejected() {
        let mut c = EngineConfig::default_arbitrum_one();
        c.chain_id = 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn roundtrip_json() {
        let c = EngineConfig::default_arbitrum_one();
        let s = serde_json::to_string(&c).unwrap();
        let d: EngineConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(d.chain_id, c.chain_id);
    }

    #[test]
    fn topology_defaults_match_plan() {
        // [Codex 5–8-hop] OUR DECISION / PLAN FOR NOW: default topology is capped at 4 hops. The
        // Mantis submit calldata encoder supports only 2–4 hops, so 5–8-hop cycles are non-submittable
        // (wasted compute) and disabled by default. 4-hop stays on (mixed-family 4-hop cycles are
        // submittable). Re-enable deeper tiers per-flag once the encoder/gas model support them.
        let t = TopologyConfig::default();
        assert!(t.enabled(2));
        assert!(t.enabled(3));
        assert!(t.enabled(4));
        for k in 5..=8 {
            assert!(!t.enabled(k), "{k}-hop should be OFF by default (for now)");
        }
        assert!(!t.enabled(9));
    }

    #[test]
    fn apply_max_hops_caps_tiers() {
        // MAX_HOPS=2 → only 2-hop; 3..=8 disabled.
        let mut t = TopologyConfig::default();
        t.apply_max_hops(2);
        assert!(t.enabled(2));
        for k in 3..=8 {
            assert!(!t.enabled(k), "{k}-hop must be off at MAX_HOPS=2");
        }
        // MAX_HOPS=5 → 2..=5 on, 6..=8 off.
        let mut t = TopologyConfig::default();
        t.apply_max_hops(5);
        for k in 2..=5 {
            assert!(t.enabled(k), "{k}-hop must be on at MAX_HOPS=5");
        }
        for k in 6..=8 {
            assert!(!t.enabled(k), "{k}-hop must be off at MAX_HOPS=5");
        }
        // MAX_HOPS=8 → all tiers on; out-of-range is clamped (0→2, 99→8).
        let mut t = TopologyConfig {
            enable_8_hop: false,
            ..TopologyConfig::default()
        };
        t.apply_max_hops(8);
        for k in 2..=8 {
            assert!(t.enabled(k), "{k}-hop must be on at MAX_HOPS=8");
        }
        let mut lo = TopologyConfig::default();
        lo.apply_max_hops(0);
        assert!(lo.enabled(2) && !lo.enabled(3), "MAX_HOPS=0 clamps to 2");
        let mut hi = TopologyConfig::default();
        hi.apply_max_hops(99);
        assert!(hi.enabled(8) && !hi.enabled(9), "MAX_HOPS=99 clamps to 8");
    }

    /// DELIVERABLE C: token-enum alignment. The engine assigns `TokenId = config index`
    /// (`startup.rs` iterates `cfg.tokens` in order via `add_token`). The Mantis Mixed
    /// entrypoints (`flashLoanArbMixed`/`flashLoanArbAaveMixed`) encode token **addresses**
    /// in `TradeHopV2`, so wrong-token swaps are prevented iff this TokenId→address map is
    /// correct — which this test pins.
    ///
    /// NOTE (verified): `contracts/src/generated/TokenLookupData.sol` is a SEPARATE 41-token
    /// universe (Tigger's), NOT index-aligned with this 20-token config, and serves only the
    /// contract's packed-payload path + constructor router approvals. The engine uses the
    /// address-based Mixed path, so the index mismatch cannot silently swap tokens. (All of
    /// the engine's routed catalog tokens — WETH/USDC/USDt0/WBTC/ARB/PENDLE — ARE present in
    /// TokenLookupData, so the contract pre-approved them.)
    #[test]
    fn token_enum_alignment_config_order_is_correct() {
        let cfg: EngineConfig = serde_json::from_str(include_str!("../../../engine.example.json"))
            .expect("parse engine.example.json");
        // (TokenId, symbol, canonical Arbitrum One address) — the source of truth the
        // address-based calldata encoder relies on.
        let expect: &[(&str, &str)] = &[
            ("WETH", "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"),
            ("USDC", "0xaf88d065e77c8cc2239327c5edb3a432268e5831"),
            ("USDt0", "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9"),
            ("WBTC", "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f"),
            ("ARB", "0x912ce59144191c1204e64559fe8253a0e49e6548"),
            ("wstETH", "0x5979d7b546e38e414f7e9822514be443a4800529"),
            ("DAI", "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1"),
            ("weETH", "0x35751007a407ca6feffe80b3cb397736d2cf4dbe"),
            ("rETH", "0xec70dcb4a1efa46b8f2d97c310c9c4790ba5ffa8"),
            ("ezETH", "0x2416092f143378750bb29b79ed961ab195cceea5"),
            ("sfrxETH", "0x95ab45875cffdba1e5f451b950bc2e42c0053f39"),
            ("GHO", "0x7dff72693f6a4149b17e7c6314655f6a9f7c8b33"),
            ("axlUSDC", "0xeb466342c4d449bc9f53a865d5cb90586f405215"),
            ("USDC.e", "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8"),
            ("USDe", "0x5d3a1ff2b6bab83b63cd9ad0787074081a52ef34"),
            ("LINK", "0xf97f4df75117a78c1a5a0dbb814af92458539fb4"),
            ("GMX", "0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a"),
            ("crvUSD", "0x498bf2b1e120fed3ad3d42ea2165e9b73f99c1e5"),
            ("PENDLE", "0x0c880f6761f1af8d9aa9c466984b80dab9a8c9e8"),
            ("frxUSD", "0x80eede496655fb9047dd39d9f418d5483ed600df"),
        ];
        assert_eq!(
            cfg.tokens.len(),
            expect.len(),
            "engine config must define exactly 20 tokens"
        );
        for (i, (sym, addr)) in expect.iter().enumerate() {
            assert_eq!(
                cfg.tokens[i].symbol.to_ascii_uppercase(),
                sym.to_ascii_uppercase(),
                "TokenId {i} symbol mismatch"
            );
            assert_eq!(
                cfg.tokens[i].address.to_ascii_lowercase(),
                *addr,
                "TokenId {i} ({sym}) address mismatch — would silently swap wrong token"
            );
        }
    }

    #[test]
    fn topology_serde_default_when_absent() {
        let json = r#"{
            "chain_id": 42161,
            "relay_endpoint": "ws://x",
            "sequencer_endpoint": "https://x",
            "tokens": [{"address":"0x00","decimals":18,"symbol":"X","risk_class":"permissionless"}],
            "venues": [], "lanes": [],
            "default_amount_grid_min": "1",
            "default_amount_grid_max": "2",
            "default_amount_grid_buckets": 1,
            "thresholds": {
                "min_net_profit_wei":"0","min_ev_absolute_wei":0,"min_roi_bps":0,
                "confidence_floor":0.5,"gas_max_age_ms":1000,
                "max_flashloan_fee_1e18":"0"
            }
        }"#;
        let cfg: EngineConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.topology.enable_two_hop);
        assert!(cfg.topology.enable_4_hop);
        assert_eq!(cfg.topology.max_routes_per_deep_tier, 8192);
    }
}
