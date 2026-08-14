//! Uniswap V4 `Initialize` log scanner (hookless concentrated-liquidity pools).
//!
//! V4 is a singleton architecture: one `PoolManager` contract holds every pool's state
//! and emits every pool's events. A pool is identified by a `bytes32 poolId` (the keccak
//! of its `PoolKey`), not by a deployed address. Hookless V4 pools use the SAME
//! concentrated-liquidity math as Uniswap V3, so the engine reuses the V3 kernel and
//! `V3State` store. Only identity / discovery / snapshot differ.
//!
//! Event signature:
//! ```text
//! event Initialize(
//!     bytes32 indexed id,
//!     address indexed currency0,
//!     address indexed currency1,
//!     uint24           fee,
//!     int24            tickSpacing,
//!     address          hooks,
//!     uint160          sqrtPriceX96,
//!     int24            tick
//! );
//! ```
//! `id` = topics[1], `currency0` = topics[2][12:32], `currency1` = topics[3][12:32].
//! Data words = `[fee, tickSpacing, hooks, sqrtPriceX96, tick]`; `hooks` = data word2.
//!
//! Identity model: the engine keys everything on `[u8;20]` addresses, so each V4 pool is
//! registered under a **synthetic address = poolId[0..20]**. The full 32-byte poolId is
//! carried alongside so the StateView snapshot (`getSlot0`/`getLiquidity`/tick window) can
//! address the pool.
//!
//! Filters:
//! * only `hooks == address(0)` (hookless — hooked pools have custom curves we can't reuse
//!   the V3 kernel for);
//! * NO native-ETH pools (`currency0 == address(0)`): MantisExecutor rebuilds the PoolKey from
//!   the WETH-tokenized hop, so a native pool's stored poolId can never match the executor's
//!   recompute — the route would always revert `PoolId mismatch` (v4-native-eth fix 2026-07-01);
//! * both currencies must be in the allowlist.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveryError};

/// V4 native-ETH sentinel: `currency = address(0)` denotes native ETH.
pub const NATIVE_ETH: Address = [0u8; 20];

/// Topic hash for V4 `Initialize`. Computed lazily from the canonical signature so the
/// signature string is the single source of truth (no hard-coded magic number).
pub fn initialize_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// Public alias for ergonomic access by name.
pub const V4_INITIALIZE_TOPIC: fn() -> &'static [u8; 32] = initialize_topic;

/// One V4 PoolManager to scan.
#[derive(Debug, Clone)]
pub struct V4PoolManagerConfig {
    /// PoolManager singleton address (emits every pool's events).
    pub address: Address,
    /// Earliest block to scan from. Arbitrum One PoolManager ≈ block 298 076 243
    /// (early 2025); a safe lower bound is 290 000 000.
    pub deployment_block: u64,
    /// WETH address used to map V4's native-ETH sentinel (`address(0)`) into the engine's
    /// token universe.
    pub weth: Address,
}

/// A discovered hookless V4 pool. Mirrors `DiscoveredPool` but carries the full 32-byte
/// `poolId` (the StateView reads need it) plus the synthetic address (`poolId[0..20]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4DiscoveredPool {
    /// Synthetic registry address = `poolId[0..20]`.
    pub address: Address,
    /// Full 32-byte poolId for StateView reads.
    pub pool_id: [u8; 32],
    /// `currency0` — never native `address(0)` (native-ETH pools are excluded at admission).
    pub token0: Address,
    /// `currency1` — never native `address(0)`.
    pub token1: Address,
    /// Fee in pips (10⁻⁶).
    pub fee_pips: u32,
    /// Tick spacing.
    pub tick_spacing: i32,
    /// Block at which the pool was initialised.
    pub created_block: u64,
}

/// Map V4's native-ETH sentinel (`address(0)`) to the engine's WETH address; pass any
/// other address through unchanged.
#[inline]
fn map_native(addr: Address, weth: Address) -> Address {
    if addr == NATIVE_ETH {
        weth
    } else {
        addr
    }
}

/// Scan the V4 PoolManager for every `Initialize` event between `deployment_block` and
/// `head_block`, decode each PoolKey, keep pools where `hooks == address(0)` AND both
/// currencies (after `address(0)` → WETH) are in `token_allowlist`, and emit
/// `V4DiscoveredPool` records.
pub fn discover_v4_pools(
    client: &ArbitrumRpcClient,
    cfg: &V4PoolManagerConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<V4DiscoveredPool>, DiscoveryError> {
    let topic0 = initialize_topic();
    info!(
        pool_manager = ?cfg.address,
        from = cfg.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning V4 Initialize logs"
    );
    let logs = fetch_logs_chunked(
        client,
        &cfg.address,
        topic0,
        cfg.deployment_block,
        head_block,
        chunk_blocks,
        /* max_retries */ 4,
    )?;
    debug!(count = logs.len(), "raw V4 Initialize logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    let mut hooked_skipped = 0usize;
    let mut native_skipped = 0usize;
    for log in &logs {
        match decode_v4_initialize(log) {
            Ok(dec) => match admit_v4(&dec, log.block_number, cfg.weth, token_allowlist) {
                V4Admit::Admitted(p) => out.push(p),
                V4Admit::SkipHooked => hooked_skipped += 1,
                V4Admit::SkipNative => native_skipped += 1,
                V4Admit::SkipAllowlist => {}
            },
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed V4 Initialize log; skipping");
            }
        }
    }
    info!(
        pool_manager = ?cfg.address,
        discovered = out.len(),
        hooked_skipped,
        native_skipped,
        "V4 Initialize discovery complete"
    );
    Ok(out)
}

/// Admission verdict for one decoded V4 `Initialize` log.
enum V4Admit {
    Admitted(V4DiscoveredPool),
    SkipHooked,
    SkipNative,
    SkipAllowlist,
}

/// Decide whether a decoded V4 `Initialize` becomes a discovered pool. Pure (no RPC) so the
/// admission policy is unit-testable.
fn admit_v4(dec: &V4Init, block: u64, weth: Address, allowlist: &AHashSet<Address>) -> V4Admit {
    if !dec.hookless {
        return V4Admit::SkipHooked;
    }
    // [v4-native-eth fix 2026-07-01] Native-ETH pools (currency0 == address(0); currency1 can
    // never be — V4 sorts currencies and nothing sorts below zero) are EXCLUDED, not WETH-mapped:
    // MantisExecutor rebuilds the PoolKey from the WETH-tokenized hop tokens, so the stored
    // native poolId can never equal its recompute and the route always reverts "PoolId mismatch"
    // (fail-closed, but 40% of discovered V4 pools were detect-only). Re-admit only together
    // with executor-side native-currency support (native settle + sentinel through calldata).
    if dec.currency0 == NATIVE_ETH || dec.currency1 == NATIVE_ETH {
        return V4Admit::SkipNative;
    }
    let token0 = map_native(dec.currency0, weth);
    let token1 = map_native(dec.currency1, weth);
    if !allowlist.contains(&token0) || !allowlist.contains(&token1) {
        return V4Admit::SkipAllowlist;
    }
    let mut address = [0u8; 20];
    address.copy_from_slice(&dec.pool_id[0..20]);
    V4Admit::Admitted(V4DiscoveredPool {
        address,
        pool_id: dec.pool_id,
        token0,
        token1,
        fee_pips: dec.fee,
        tick_spacing: dec.tick_spacing,
        created_block: block,
    })
}

/// keccak256(abi.encode(PoolKey)) for a HOOKLESS key built from unordered ERC-20 currencies
/// `(a, b)` (sorted ascending, exactly like `MantisExecutor._v4PoolKey`), `fee_pips` (uint24)
/// and `tick_spacing` (int24). This is the id the EXECUTOR recomputes at swap time — a stored
/// poolId that does not equal it (e.g. a native-ETH pool, whose real key has
/// `currency0 = address(0)`) can never execute and must not be registered.
pub fn v4_pool_id_for(a: Address, b: Address, fee_pips: u32, tick_spacing: i32) -> [u8; 32] {
    let (c0, c1) = if a <= b { (a, b) } else { (b, a) };
    // abi.encode(PoolKey{currency0, currency1, uint24 fee, int24 tickSpacing, address hooks=0})
    // = 5 static words of 32 bytes.
    let mut enc = [0u8; 160];
    enc[12..32].copy_from_slice(&c0);
    enc[44..64].copy_from_slice(&c1);
    enc[92..96].copy_from_slice(&fee_pips.to_be_bytes());
    // int24 sign-extends to a full int256 word in canonical ABI.
    if tick_spacing < 0 {
        enc[96..124].fill(0xFF);
    }
    enc[124..128].copy_from_slice(&tick_spacing.to_be_bytes());
    // enc[128..160] = hooks = address(0), already zero.
    let mut k = Keccak::v256();
    k.update(&enc);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// Decoded V4 `Initialize` fields.
struct V4Init {
    pool_id: [u8; 32],
    currency0: Address,
    currency1: Address,
    fee: u32,
    tick_spacing: i32,
    hookless: bool,
}

/// Decode a single V4 `Initialize` log.
fn decode_v4_initialize(log: &LogEntry) -> Result<V4Init, DiscoveryError> {
    if log.topics.len() != 4 {
        return Err(DiscoveryError::MalformedLog("V4 Initialize topic count"));
    }
    // topics[1] = poolId (bytes32), topics[2] = currency0, topics[3] = currency1.
    let pool_id = log.topics[1];
    let currency0 = address_from_topic(&log.topics[2])?;
    let currency1 = address_from_topic(&log.topics[3])?;
    // data = [fee(u24), tickSpacing(i24), hooks(address), sqrtPriceX96(u160), tick(i24)]
    // = 5 words = 160 bytes. We only need fee, tickSpacing, hooks here.
    if log.data.len() < 96 {
        return Err(DiscoveryError::MalformedLog("V4 Initialize data len"));
    }
    let fee = u24_from_word(&log.data[0..32])?;
    let tick_spacing = i24_from_word(&log.data[32..64])?;
    let hooks = address_from_topic(&log.data[64..96].try_into().expect("32 bytes"))?;
    let hookless = hooks == NATIVE_ETH; // address(0) == no hooks
    Ok(V4Init {
        pool_id,
        currency0,
        currency1,
        fee,
        tick_spacing,
        hookless,
    })
}

/// Extract a 20-byte address from a 32-byte ABI word (low 20 bytes, high 12 zero).
fn address_from_topic(t: &[u8; 32]) -> Result<Address, DiscoveryError> {
    if t[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&t[12..32]);
    Ok(a)
}

/// Extract an unsigned `uint24` from a 32-byte ABI word (right-aligned, low 3 bytes).
fn u24_from_word(w: &[u8]) -> Result<u32, DiscoveryError> {
    if w.len() != 32 {
        return Err(DiscoveryError::MalformedLog("u24 word len"));
    }
    if w[0..29].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("u24 padding nonzero"));
    }
    Ok(u32::from_be_bytes([0, w[29], w[30], w[31]]))
}

/// Extract a signed `int24` from a 32-byte ABI word (sign-extended in canonical ABI).
fn i24_from_word(w: &[u8]) -> Result<i32, DiscoveryError> {
    if w.len() != 32 {
        return Err(DiscoveryError::MalformedLog("int24 word len"));
    }
    let high = w[0];
    let mut bytes = [0u8; 4];
    bytes[1] = w[29];
    bytes[2] = w[30];
    bytes[3] = w[31];
    if high == 0xFF {
        bytes[0] = 0xFF;
        for &b in &w[1..29] {
            if b != 0xFF {
                return Err(DiscoveryError::MalformedLog("int24 padding mixed"));
            }
        }
        if w[29] & 0x80 == 0 {
            return Err(DiscoveryError::MalformedLog(
                "int24 sign byte without high bit",
            ));
        }
    } else if high == 0 {
        for &b in &w[1..29] {
            if b != 0 {
                return Err(DiscoveryError::MalformedLog("int24 padding mixed"));
            }
        }
        if w[29] & 0x80 != 0 {
            return Err(DiscoveryError::MalformedLog(
                "int24 positive but high bit set",
            ));
        }
    } else {
        return Err(DiscoveryError::MalformedLog("int24 padding not 00/FF"));
    }
    Ok(i32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_topic_matches_known_hash() {
        // Verified live against the Arbitrum PoolManager (2026-05): topic0 of the
        // Initialize event is dd466e67…838d6438.
        let computed = initialize_topic();
        let mut got = [0u8; 64];
        faster_hex::hex_encode(computed, &mut got).unwrap();
        assert_eq!(
            std::str::from_utf8(&got).unwrap(),
            "dd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438"
        );
    }

    #[test]
    fn native_eth_maps_to_weth() {
        let weth = [0x82u8; 20];
        assert_eq!(map_native(NATIVE_ETH, weth), weth);
        let other = [0xABu8; 20];
        assert_eq!(map_native(other, weth), other);
    }

    fn word(v: &[u8]) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[32 - v.len()..].copy_from_slice(v);
        w
    }

    #[test]
    fn u24_decodes() {
        assert_eq!(u24_from_word(&word(&[0x0B, 0xB8])).unwrap(), 3000);
        assert_eq!(u24_from_word(&word(&[0x01, 0xF4])).unwrap(), 500);
    }

    #[test]
    fn i24_positive_and_negative() {
        assert_eq!(i24_from_word(&word(&[60])).unwrap(), 60);
        // -1 int24 = 0xFFFFFF sign-extended.
        let mut w = [0xFFu8; 32];
        w[31] = 0xFF;
        assert_eq!(i24_from_word(&w).unwrap(), -1);
    }

    #[test]
    fn decode_full_initialize_log_hookless() {
        // Synthesised log: poolId in topics[1], currency0/1 in topics[2..4],
        // data = fee=500, tickSpacing=10, hooks=0, sqrtPriceX96=X, tick=-199857.
        let pid = [0xAAu8; 32];
        let mut c0 = [0u8; 32];
        c0[12..32].copy_from_slice(&[0u8; 20]); // native ETH
        let mut c1 = [0u8; 32];
        c1[12..32].copy_from_slice(&[0xCDu8; 20]);
        let mut data = Vec::new();
        data.extend_from_slice(&word(&[0x01, 0xF4])); // fee 500
        data.extend_from_slice(&word(&[10])); // tickSpacing 10
        data.extend_from_slice(&[0u8; 32]); // hooks = 0
        data.extend_from_slice(&[0u8; 32]); // sqrtPriceX96 (ignored)
        data.extend_from_slice(&[0u8; 32]); // tick (ignored)
        let log = LogEntry {
            block_number: 123,
            address: [0x36u8; 20],
            topics: vec![*initialize_topic(), pid, c0, c1],
            data,
        };
        let dec = decode_v4_initialize(&log).unwrap();
        assert!(dec.hookless);
        assert_eq!(dec.fee, 500);
        assert_eq!(dec.tick_spacing, 10);
        assert_eq!(dec.currency0, NATIVE_ETH);
        assert_eq!(dec.currency1, [0xCDu8; 20]);
        assert_eq!(dec.pool_id, pid);
    }

    #[test]
    fn decode_hooked_pool_flagged_not_hookless() {
        let pid = [0x11u8; 32];
        let c0 = word(&[0x82u8; 20]);
        let c1 = word(&[0xCDu8; 20]);
        let mut data = Vec::new();
        data.extend_from_slice(&word(&[0x01, 0xF4]));
        data.extend_from_slice(&word(&[10]));
        let mut hooks = [0u8; 32];
        hooks[12..32].copy_from_slice(&[0x99u8; 20]); // non-zero hooks
        data.extend_from_slice(&hooks);
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);
        let log = LogEntry {
            block_number: 1,
            address: [0x36u8; 20],
            topics: vec![*initialize_topic(), pid, c0, c1],
            data,
        };
        let dec = decode_v4_initialize(&log).unwrap();
        assert!(!dec.hookless);
    }

    fn haddr(s: &str) -> Address {
        let mut a = [0u8; 20];
        faster_hex::hex_decode(s.as_bytes(), &mut a).unwrap();
        a
    }

    fn h32(s: &str) -> [u8; 32] {
        let mut a = [0u8; 32];
        faster_hex::hex_decode(s.as_bytes(), &mut a).unwrap();
        a
    }

    #[test]
    fn v4_pool_id_for_matches_executor_recompute_real_vector() {
        // Real Arbitrum vector (verified with `cast keccak` over abi.encode(PoolKey), session-2
        // pass 2026-07-01): the WETH/USDC 500/10 hookless key hashes to 0xfc7b…8653. Must be
        // order-insensitive — MantisExecutor._v4PoolKey sorts the currencies.
        let weth = haddr("82af49447d8a07e3bd95bd0d56f35241523fbab1");
        let usdc = haddr("af88d065e77c8cc2239327c5edb3a432268e5831");
        let expect = h32("fc7b3ad139daaf1e9c3637ed921c154d1b04286f8a82b805a6c352da57028653");
        assert_eq!(v4_pool_id_for(weth, usdc, 500, 10), expect);
        assert_eq!(v4_pool_id_for(usdc, weth, 500, 10), expect);
        // The REAL Arbitrum pool at these parameters is native-ETH/USDC: its stored id (below)
        // is the keccak of the currency0=address(0) key, which the executor's WETH-tokenized
        // recompute can NEVER equal — that is the native-ETH unexecutability bug.
        let stored_native = h32("864abca0a6202dba5b8868772308da953ff125b0f95015adbf89aaf579e903a8");
        assert_ne!(v4_pool_id_for(weth, usdc, 500, 10), stored_native);
    }

    fn native_init(pid: [u8; 32], c1_byte: u8) -> V4Init {
        V4Init {
            pool_id: pid,
            currency0: NATIVE_ETH,
            currency1: [c1_byte; 20],
            fee: 500,
            tick_spacing: 10,
            hookless: true,
        }
    }

    #[test]
    fn native_eth_v4_pool_is_rejected_at_admission() {
        // currency0 == address(0): MantisExecutor rebuilds the PoolKey from the WETH-tokenized
        // hop, so the stored native poolId can never match its recompute → the route would
        // always revert "PoolId mismatch". Admission must SKIP the pool, not WETH-map it.
        let weth = [0x82u8; 20];
        let other = [0xCDu8; 20];
        let mut allow = AHashSet::default();
        allow.insert(weth);
        allow.insert(other);
        let dec = native_init([0xAAu8; 32], 0xCD);
        assert!(
            matches!(admit_v4(&dec, 123, weth, &allow), V4Admit::SkipNative),
            "native-ETH V4 pool must be excluded from discovery (unexecutable by MantisExecutor)"
        );
    }

    #[test]
    fn non_native_v4_pool_is_admitted_with_raw_currencies() {
        let weth = [0x82u8; 20];
        let t0 = [0x11u8; 20];
        let t1 = [0xCDu8; 20];
        let mut allow = AHashSet::default();
        allow.insert(t0);
        allow.insert(t1);
        let dec = V4Init {
            pool_id: [0xBBu8; 32],
            currency0: t0,
            currency1: t1,
            fee: 3000,
            tick_spacing: 60,
            hookless: true,
        };
        match admit_v4(&dec, 77, weth, &allow) {
            V4Admit::Admitted(p) => {
                assert_eq!(p.token0, t0);
                assert_eq!(p.token1, t1);
                assert_eq!(p.pool_id, [0xBBu8; 32]);
                assert_eq!(p.address, [0xBBu8; 20]);
                assert_eq!(
                    (p.fee_pips, p.tick_spacing, p.created_block),
                    (3000, 60, 77)
                );
            }
            _ => panic!("plain ERC-20 hookless allowlisted pool must be admitted"),
        }
    }
}
