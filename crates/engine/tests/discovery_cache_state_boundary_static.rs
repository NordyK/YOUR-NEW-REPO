use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::PathBuf,
};

use serde_json::Value;

const PHASE_18_V3_POOL: [u8; 20] = [
    0xbe, 0x3a, 0xd6, 0xa5, 0x66, 0x9d, 0xc0, 0xb8, 0xb1, 0x2f, 0xeb, 0xc0, 0x36, 0x08, 0x86, 0x0c,
    0x31, 0xe2, 0xee, 0xf6,
];
const PHASE_19_V2_POOL: [u8; 20] = [
    0x97, 0xb1, 0x92, 0x19, 0x8d, 0x16, 0x4c, 0x2a, 0x18, 0x34, 0x29, 0x5e, 0x30, 0x2b, 0x71, 0x3b,
    0xc3, 0x2c, 0x8f, 0x1d,
];

const IDENTITY_ONLY_POOL_KEYS: &[&str] = &[
    "address",
    "created_block",
    "family",
    "fee_bps",
    "fee_pips",
    "tick_spacing",
    "token0",
    "token1",
];

const EXECUTABLE_STATE_KEYS: &[&str] = &[
    "amp",
    "balance_delta_wei",
    "balances",
    "block_env",
    "block_number",
    "current_tick",
    "exchange_price",
    "fee_growth",
    "gas_snapshot",
    "gas_used",
    "initialised_ticks",
    "initialized_ticks",
    "liquidity",
    "net_profit_wei",
    "price_scale",
    "reserve0",
    "reserve1",
    "reserves",
    "snapshot_block",
    "sqrtPriceX96",
    "sqrt_price",
    "sqrt_price_x96",
    "slot0",
    "state_block",
    "state_root",
    "tick",
    "tick_window",
    "ticks",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolSummary {
    section: &'static str,
    index: usize,
    family: String,
    fee_pips: u64,
    fee_bps: u64,
    tick_spacing: u64,
    created_block: u64,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn read_discovery_cache() -> Value {
    // Worktree-portable: read the COMMITTED full identity-only cache snapshot (every pool identity
    // verbatim from the untracked engine/discovery_cache.json — pools/fluid_pools with only the
    // eight identity keys this boundary test admits, plus head_block). Identity fields and counts
    // are byte-identical to the live-file baseline, so the pinned pool identities and the
    // breadth/identity-only assertions below are preserved exactly.
    let bytes = fs::read(
        repo_root().join("codex/campaign_baseline_wi01/discovery_cache_full_identity_golden.json"),
    )
    .expect("read discovery cache full-identity golden fixture");
    serde_json::from_slice(&bytes).expect("parse discovery cache json")
}

fn value_address(value: &Value, field: &str) -> [u8; 20] {
    let array = value
        .get(field)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("pool missing {field} address array: {value}"));
    assert_eq!(array.len(), 20, "{field} must be exactly 20 bytes");

    let mut out = [0u8; 20];
    for (idx, byte) in array.iter().enumerate() {
        let raw = byte
            .as_u64()
            .unwrap_or_else(|| panic!("{field}[{idx}] must be a byte integer: {value}"));
        assert!(raw <= u8::MAX as u64, "{field}[{idx}] exceeds u8");
        out[idx] = raw as u8;
    }
    out
}

fn hex_address(address: &[u8; 20]) -> String {
    let mut out = String::from("0x");
    for byte in address {
        write!(&mut out, "{byte:02x}").expect("write address hex");
    }
    out
}

fn object_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("pool must be a JSON object: {value}"))
        .keys()
        .cloned()
        .collect()
}

fn numeric_field(value: &Value, field: &str) -> u64 {
    value
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("pool missing numeric {field}: {value}"))
}

fn pool_summary(section: &'static str, index: usize, pool: &Value) -> PoolSummary {
    PoolSummary {
        section,
        index,
        family: pool
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("pool missing family string: {pool}"))
            .to_string(),
        fee_pips: numeric_field(pool, "fee_pips"),
        fee_bps: numeric_field(pool, "fee_bps"),
        tick_spacing: numeric_field(pool, "tick_spacing"),
        created_block: numeric_field(pool, "created_block"),
    }
}

fn collect_executable_state_keys(path: &str, value: &Value, hits: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let child_path = format!("{path}.{key}");
                if EXECUTABLE_STATE_KEYS.contains(&key.as_str()) {
                    hits.push(child_path.clone());
                }
                collect_executable_state_keys(&child_path, nested, hits);
            }
        }
        Value::Array(items) => {
            for (idx, nested) in items.iter().enumerate() {
                collect_executable_state_keys(&format!("{path}[{idx}]"), nested, hits);
            }
        }
        _ => {}
    }
}

#[test]
fn discovery_cache_is_identity_only_not_executable_state_snapshot() {
    let cache = read_discovery_cache();
    let head_block = cache
        .get("head_block")
        .and_then(Value::as_u64)
        .expect("discovery cache head_block");
    let pools = cache
        .get("pools")
        .and_then(Value::as_array)
        .expect("discovery cache pools array");
    let fluid_pools = cache
        .get("fluid_pools")
        .and_then(Value::as_array)
        .expect("discovery cache fluid_pools array");

    assert!(head_block > 0, "head_block must anchor the identity cache");
    // AUD-DISCOVERY-BREADTH re-pin (2026-07-04): floor lowered to current authoritative breadth
    // (3142 pools at head 479515032; see discovery_cache_current_boundary_static for the family
    // histogram + the native-ETH SkipNative justification). Still non-vacuous, still trips on real loss.
    assert!(
        pools.len() >= 3_000,
        "cache must remain non-vacuous over discovered pool identities (below re-pinned 3000 floor)"
    );
    assert!(
        fluid_pools.len() >= 8,
        "fluid resolver-discovered identities must remain represented"
    );

    let allowed = IDENTITY_ONLY_POOL_KEYS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut state_key_hits = Vec::new();
    let mut phase_pools = BTreeMap::new();

    for (section, pool_set) in [("pools", pools), ("fluid_pools", fluid_pools)] {
        for (index, pool) in pool_set.iter().enumerate() {
            let keys = object_keys(pool);
            let unexpected = keys
                .iter()
                .filter(|key| !allowed.contains(key.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            assert!(
                unexpected.is_empty(),
                "{section}[{index}] contains unclassified cache keys {unexpected:?}; update the state-boundary test before treating them as proof"
            );

            value_address(pool, "address");
            value_address(pool, "token0");
            value_address(pool, "token1");
            let summary = pool_summary(section, index, pool);
            assert!(
                summary.created_block <= head_block,
                "{section}[{index}] created after cache head: {summary:?}, head_block={head_block}"
            );

            collect_executable_state_keys(
                &format!("{section}[{index}]"),
                pool,
                &mut state_key_hits,
            );
            let address = value_address(pool, "address");
            if address == PHASE_18_V3_POOL || address == PHASE_19_V2_POOL {
                phase_pools.insert(hex_address(&address), summary);
            }
        }
    }

    assert_eq!(
        phase_pools
            .get("0xbe3ad6a5669dc0b8b12febc03608860c31e2eef6")
            .map(|pool| (pool.family.as_str(), pool.fee_pips, pool.tick_spacing)),
        Some(("UniswapV3", 100, 1)),
        "Phase 18 cache-backed V3 pool identity drifted"
    );
    assert_eq!(
        phase_pools
            .get("0x97b192198d164c2a1834295e302b713bc32c8f1d")
            .map(|pool| (pool.family.as_str(), pool.fee_bps)),
        Some(("CamelotV2", 30)),
        "Phase 19 cache-backed V2 pool identity drifted"
    );

    assert!(
        state_key_hits.is_empty(),
        "discovery cache is no longer identity-only; executable AMM state keys appeared: {state_key_hits:?}. Do not treat cache-backed threaded guardrails as exact-state profit proof without a block-aligned state snapshot review."
    );
}
