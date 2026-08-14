//! RPC client for snapshotting pool state and refreshing gas/nonce data.
//!
//! Uses `ureq` (blocking, rustls-enabled) for HTTPS JSON-RPC because every RPC call is
//! background-only — the hot path never makes RPC calls (plan §0.4 rule 1). The client is
//! single-purpose: read-only `eth_call`, `eth_getStorageAt`, `eth_blockNumber`,
//! `eth_gasPrice`, `eth_getTransactionCount`, plus Arbitrum-specific
//! `NodeInterface.gasEstimateComponents`.
//!
//! Snapshot helpers:
//! * `fetch_v2_reserves(pool)` — calls `getReserves()` returning `(uint112,uint112,uint32)`.
//! * `fetch_v3_slot0(pool)` — calls `slot0()` returning the V3 slot.
//! * `fetch_v3_liquidity(pool)` — calls `liquidity()`.
//! * `fetch_v3_tick(pool, tick)` — calls `ticks(int24)` for the tick window.
//! * `fetch_erc20_decimals(token)` — calls `decimals()`.
//! * `fetch_pool_code_hash(pool)` — calls `eth_getCode` then keccak256 for the §5.6 gate.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod anvil_load_state;
pub mod balancer_snapshot;
pub mod balancer_weighted_snapshot;
pub mod chainlink;
pub mod client;
pub mod codehash;
pub mod cryptoswap_snapshot;
pub mod curve_snapshot;
pub mod dodo_snapshot;
pub mod erc20;
pub mod kyber_snapshot;
pub mod lb_snapshot;
pub mod maverick_lens;
pub mod maverick_snapshot;
pub mod multicall;
pub mod paper_reconcile;
pub mod twocrypto_snapshot;
pub mod v2_snapshot;
pub mod v3_snapshot;
pub mod v3_storage;
pub mod v4_snapshot;

pub use anvil_load_state::{
    erc20_balance_slot, hydrate_v3_ticks_in_load_state, write_storage_word_in_load_state,
};
pub use balancer_snapshot::{
    fetch_balancer_stable, fetch_balancer_states_batched, BalancerStableView,
    BALANCER_VAULT_ARBITRUM,
};
pub use balancer_weighted_snapshot::fetch_balancer_weighted;
pub use chainlink::{fetch_chainlink_marks, ChainlinkMark, CHAINLINK_MAX_AGE_SECS};
pub use client::{
    archive_rpc_url, hot_rpc_url, resolve_archive_rpc_url, resolve_hot_rpc_url, ArbitrumRpcClient,
    BlockTag, RpcError,
};
pub use codehash::{fetch_pool_code_hash, verify_pool_code_hash};
pub use cryptoswap_snapshot::fetch_cryptoswap_state;
pub use curve_snapshot::{fetch_curve_state, CurvePoolView, CURVE_STABLESWAP_NG_FACTORY_ARBITRUM};
pub use dodo_snapshot::{
    decode_dodo_state, dodo_shadow_getters, fetch_dodo_state, fetch_dodo_states_batched,
};
pub use erc20::fetch_erc20_decimals;
pub use kyber_snapshot::{
    fetch_kyber_initialized_ticks, fetch_kyber_liquidity, fetch_kyber_liquidity_total,
    fetch_kyber_pool_state, kyber_fee_units_to_ppm,
};
pub use lb_snapshot::fetch_lb_state;
pub use maverick_lens::{fetch_maverick_lens, MaverickLensResult};
pub use maverick_snapshot::fetch_maverick_state_basic;
pub use multicall::{
    aggregate3, fetch_pool_states_batched, Call3, CallResult, PoolStateUpdate, PriceFetchKind,
    MULTICALL3_ARBITRUM,
};
pub use twocrypto_snapshot::fetch_twocrypto_state;
pub use v2_snapshot::{
    fetch_camelot_is_stable, fetch_token0, fetch_token1, fetch_v2_reserves, V2Snapshot,
};
pub use v3_snapshot::{
    fetch_algebra_global_state, fetch_v3_liquidity, fetch_v3_slot0, fetch_v3_tick,
    fetch_v3_tick_window, fetch_v3_tick_window_multicall, fetch_v3_windows_batched,
    AlgebraGlobalStateSnapshot, V3Slot0Snapshot, V3TickSnapshot, fetch_algebra_tick_spacing};
pub use v3_storage::{pack_v3_tick_liquidity_word, v3_tick_info_word0_slot};
pub use v4_snapshot::{
    fetch_v4_liquidity, fetch_v4_slot0, fetch_v4_states_batched, fetch_v4_tick,
    fetch_v4_tick_bitmap, fetch_v4_tick_window, V4CheapState, V4Slot0Snapshot,
};
