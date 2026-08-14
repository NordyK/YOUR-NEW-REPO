use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

use alloy_primitives::U256 as AlloyU256;
use ruint::aliases::U256 as RuintU256;
use serde_json::Value;
use tungstenite::Message;

use engine_config::{EngineConfig, RouterEntry};
use engine_detect::{
    decode_aggregator_intents, predict_route, DecodedHop, PredictorContext, RouteIntent,
    SelectorClass, SelectorRegistry,
};
use engine_feed::{
    decode_envelope, decode_l2msg_recursive_raw, decode_signed_tx, Address, ARB_ONE_CHAIN_ID,
};
use engine_gas::{GasOracle, GasSnapshot};
use engine_metrics::{LatencyHistograms, ReasonRing};
use engine_routes::{enumerate_two_hop_routes, CsrBuilder, RouteTable};
use engine_state::{
    PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, V3State, VenueId,
};

#[derive(Debug)]
struct RealCorpusPipelineCase {
    frame_json: String,
    router_address: Address,
    selector: [u8; 4],
    token_in: Address,
    token_out: Address,
    fee_pips: u32,
    intent: RouteIntent,
}

#[derive(Debug, Clone, Copy)]
struct CachedV3Pool {
    address: Address,
    family: PoolKind,
    token0: Address,
    token1: Address,
    fee_pips: u32,
    tick_spacing: i32,
}

#[derive(Debug, Clone, Copy)]
struct CachedV2Pool {
    address: Address,
    family: PoolKind,
    token0: Address,
    token1: Address,
    fee_bps: u32,
}

#[derive(Debug)]
struct CacheBackedPipelineCase {
    case: RealCorpusPipelineCase,
    cached_pool: CachedV3Pool,
    corpus_name: String,
}

#[derive(Debug)]
struct CacheBackedV2PipelineCase {
    case: RealCorpusPipelineCase,
    cached_pool: CachedV2Pool,
    corpus_name: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn hex_addr(addr: &Address) -> String {
    format!("0x{}", faster_hex::hex_string(addr))
}

fn value_address(value: &Value, field: &str) -> Address {
    let array = value
        .get(field)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("discovery cache pool missing {field} array: {value}"));
    assert_eq!(array.len(), 20, "discovery cache {field} must be 20 bytes");
    let mut out = [0u8; 20];
    for (idx, byte) in array.iter().enumerate() {
        let raw = byte
            .as_u64()
            .unwrap_or_else(|| panic!("discovery cache {field}[{idx}] is not u64: {value}"));
        assert!(
            raw <= u8::MAX as u64,
            "discovery cache {field}[{idx}] is not a byte: {value}"
        );
        out[idx] = raw as u8;
    }
    out
}

fn value_u32(value: &Value, field: &str) -> u32 {
    let raw = value
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("discovery cache pool missing numeric {field}: {value}"));
    assert!(
        raw <= u32::MAX as u64,
        "discovery cache {field} exceeds u32: {value}"
    );
    raw as u32
}

fn value_i32(value: &Value, field: &str) -> i32 {
    let raw = value
        .get(field)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("discovery cache pool missing numeric {field}: {value}"));
    assert!(
        (i32::MIN as i64..=i32::MAX as i64).contains(&raw),
        "discovery cache {field} exceeds i32: {value}"
    );
    raw as i32
}

fn tiered_v3_family(value: &Value) -> Option<PoolKind> {
    match value.get("family").and_then(Value::as_str)? {
        "UniswapV3" => Some(PoolKind::UniswapV3),
        "SushiV3" => Some(PoolKind::SushiV3),
        "PancakeV3" => Some(PoolKind::PancakeV3),
        _ => None,
    }
}

fn v2_family(value: &Value) -> Option<PoolKind> {
    match value.get("family").and_then(Value::as_str)? {
        "UniswapV2" => Some(PoolKind::UniswapV2),
        "SushiV2" => Some(PoolKind::SushiV2),
        "CamelotV2" => Some(PoolKind::CamelotV2),
        _ => None,
    }
}

/// Worktree-portable discovery-cache source: the COMMITTED full identity-only cache snapshot
/// (every pool identity verbatim from the untracked engine/discovery_cache.json, same array
/// order). The family filters below yield the identical pool Vecs the live file would, so the
/// `.find()`-selected cache-backed pool is unchanged.
fn discovery_cache_fixture() -> PathBuf {
    repo_root().join("codex/campaign_baseline_wi01/discovery_cache_full_identity_golden.json")
}

fn load_cached_tiered_v3_pools() -> Vec<CachedV3Pool> {
    let bytes = fs::read(discovery_cache_fixture())
        .expect("read discovery cache full-identity golden fixture");
    let json: Value = serde_json::from_slice(&bytes).expect("parse discovery cache json");
    json.get("pools")
        .and_then(Value::as_array)
        .expect("discovery cache pools array")
        .iter()
        .filter_map(|pool| {
            let family = tiered_v3_family(pool)?;
            Some(CachedV3Pool {
                address: value_address(pool, "address"),
                family,
                token0: value_address(pool, "token0"),
                token1: value_address(pool, "token1"),
                fee_pips: value_u32(pool, "fee_pips"),
                tick_spacing: value_i32(pool, "tick_spacing"),
            })
        })
        .collect()
}

fn load_cached_v2_pools() -> Vec<CachedV2Pool> {
    let bytes = fs::read(discovery_cache_fixture())
        .expect("read discovery cache full-identity golden fixture");
    let json: Value = serde_json::from_slice(&bytes).expect("parse discovery cache json");
    json.get("pools")
        .and_then(Value::as_array)
        .expect("discovery cache pools array")
        .iter()
        .filter_map(|pool| {
            let family = v2_family(pool)?;
            Some(CachedV2Pool {
                address: value_address(pool, "address"),
                family,
                token0: value_address(pool, "token0"),
                token1: value_address(pool, "token1"),
                fee_bps: value_u32(pool, "fee_bps"),
            })
        })
        .collect()
}

fn cached_tiered_v3_pool_for_hop(
    cached_pools: &[CachedV3Pool],
    hop: &DecodedHop,
) -> Option<CachedV3Pool> {
    if hop.fee_pips == 0 {
        return None;
    }
    cached_pools.iter().copied().find(|pool| {
        pool.fee_pips == hop.fee_pips
            && ((pool.token0 == hop.token_in && pool.token1 == hop.token_out)
                || (pool.token0 == hop.token_out && pool.token1 == hop.token_in))
    })
}

fn cached_v2_pool_for_hop(cached_pools: &[CachedV2Pool], hop: &DecodedHop) -> Option<CachedV2Pool> {
    if hop.fee_pips != 0 {
        return None;
    }
    cached_pools.iter().copied().find(|pool| {
        (pool.token0 == hop.token_in && pool.token1 == hop.token_out)
            || (pool.token0 == hop.token_out && pool.token1 == hop.token_in)
    })
}

fn first_v3_compatible_real_router_frame(path: &Path) -> RealCorpusPipelineCase {
    let file =
        File::open(path).unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let registry = SelectorRegistry::canonical();
    let mut diagnostics = Vec::new();

    for line in BufReader::new(file).lines() {
        let line = line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if line.trim().is_empty() {
            continue;
        }

        let envelope = decode_envelope(line.as_bytes())
            .unwrap_or_else(|err| panic!("failed to decode envelope in {}: {err}", path.display()));
        let mut payload = envelope.l2msg_b64.to_vec();
        let decoded_len = base64_simd::forgiving_decode_inplace(&mut payload)
            .unwrap_or_else(|err| {
                panic!("failed to decode l2msg base64 in {}: {err}", path.display())
            })
            .len();
        let payload = &payload[..decoded_len];
        let mut found = None;

        decode_l2msg_recursive_raw(payload, |raw| {
            if found.is_some() {
                return;
            }
            let Some(tx) = decode_signed_tx(raw) else {
                return;
            };
            if tx.chain_id != ARB_ONE_CHAIN_ID {
                return;
            }
            let Some(selector_bytes) = tx.data.get(..4) else {
                return;
            };
            let mut selector = [0u8; 4];
            selector.copy_from_slice(selector_bytes);
            if registry.lookup(&selector) != Some(SelectorClass::Aggregator) {
                return;
            }

            for intent in decode_aggregator_intents(selector, tx.data) {
                if diagnostics.len() < 8 {
                    diagnostics.push(format!(
                        "selector=0x{} to={} exact_input={} amount_zero={} hops={}",
                        faster_hex::hex_string(&selector),
                        hex_addr(&tx.to),
                        intent.exact_input,
                        intent.amount_in.is_zero(),
                        intent
                            .hops
                            .iter()
                            .map(|hop| format!(
                                "pool={} fee={} in={} out={}",
                                hop.pool
                                    .map(|pool| hex_addr(&pool))
                                    .unwrap_or_else(|| "none".into()),
                                hop.fee_pips,
                                hex_addr(&hop.token_in),
                                hex_addr(&hop.token_out)
                            ))
                            .collect::<Vec<_>>()
                            .join(";")
                    ));
                }
                if !intent.exact_input || intent.amount_in.is_zero() {
                    continue;
                }
                let Some(hop) = intent.hops.iter().find(|hop| is_v3_compatible_hop(hop)) else {
                    continue;
                };
                found = Some(RealCorpusPipelineCase {
                    frame_json: line.clone(),
                    router_address: tx.to,
                    selector,
                    token_in: hop.token_in,
                    token_out: hop.token_out,
                    fee_pips: hop.fee_pips,
                    intent,
                });
                return;
            }
        });

        if let Some(case) = found {
            return case;
        }
    }

    panic!(
        "{} did not contain a real Arbitrum aggregator frame with an exact-input V3-compatible token-pair hop; decoded diagnostics: {:?}",
        path.display(),
        diagnostics
    );
}

fn first_bounded_v2_compatible_real_router_frame(path: &Path) -> RealCorpusPipelineCase {
    let file =
        File::open(path).unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let registry = SelectorRegistry::canonical();
    let mut diagnostics = Vec::new();

    for line in BufReader::new(file).lines() {
        let line = line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if line.trim().is_empty() || !line.contains("\"l2Msg\"") {
            continue;
        }

        let envelope = decode_envelope(line.as_bytes())
            .unwrap_or_else(|err| panic!("failed to decode envelope in {}: {err}", path.display()));
        let mut payload = envelope.l2msg_b64.to_vec();
        let decoded_len = base64_simd::forgiving_decode_inplace(&mut payload)
            .unwrap_or_else(|err| {
                panic!("failed to decode l2msg base64 in {}: {err}", path.display())
            })
            .len();
        let payload = &payload[..decoded_len];
        let mut found = None;

        decode_l2msg_recursive_raw(payload, |raw| {
            if found.is_some() {
                return;
            }
            let Some(tx) = decode_signed_tx(raw) else {
                return;
            };
            if tx.chain_id != ARB_ONE_CHAIN_ID {
                return;
            }
            let Some(selector_bytes) = tx.data.get(..4) else {
                return;
            };
            let mut selector = [0u8; 4];
            selector.copy_from_slice(selector_bytes);
            let Some(class) = registry.lookup(&selector) else {
                return;
            };

            let mut intents = Vec::new();
            match class {
                SelectorClass::Aggregator => {
                    intents.extend(decode_aggregator_intents(selector, tx.data));
                }
                SelectorClass::V2Router | SelectorClass::V3Router => {
                    if let Ok(intent) =
                        engine_detect::decode_router_intent(class, selector, tx.data)
                    {
                        intents.push(intent);
                    }
                }
            }

            for intent in intents {
                if diagnostics.len() < 8 {
                    diagnostics.push(format!(
                        "selector=0x{} to={} exact_input={} amount={} hops={}",
                        faster_hex::hex_string(&selector),
                        hex_addr(&tx.to),
                        intent.exact_input,
                        intent.amount_in,
                        intent
                            .hops
                            .iter()
                            .map(|hop| format!(
                                "pool={} fee={} in={} out={}",
                                hop.pool
                                    .map(|pool| hex_addr(&pool))
                                    .unwrap_or_else(|| "none".into()),
                                hop.fee_pips,
                                hex_addr(&hop.token_in),
                                hex_addr(&hop.token_out)
                            ))
                            .collect::<Vec<_>>()
                            .join(";")
                    ));
                }
                if !intent.exact_input
                    || intent.amount_in.is_zero()
                    || intent.amount_in > AlloyU256::from(u128::MAX)
                {
                    continue;
                }
                let Some(hop) = intent.hops.iter().find(|hop| is_v2_compatible_hop(hop)) else {
                    continue;
                };
                found = Some(RealCorpusPipelineCase {
                    frame_json: line.clone(),
                    router_address: tx.to,
                    selector,
                    token_in: hop.token_in,
                    token_out: hop.token_out,
                    fee_pips: hop.fee_pips,
                    intent,
                });
                return;
            }
        });

        if let Some(case) = found {
            return case;
        }
    }

    panic!(
        "{} did not contain a real Arbitrum frame with a bounded exact-input V2-compatible token-pair hop; decoded diagnostics: {:?}",
        path.display(),
        diagnostics
    );
}

fn first_cache_backed_bounded_v3_real_router_frame(
    paths: &[PathBuf],
    cached_pools: &[CachedV3Pool],
) -> Option<CacheBackedPipelineCase> {
    assert!(
        !cached_pools.is_empty(),
        "discovery cache must contain tiered V3 pools for cache-backed threaded proof"
    );
    let registry = SelectorRegistry::canonical();
    let mut diagnostics = Vec::new();

    for path in paths {
        let file = File::open(path)
            .unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
        let corpus_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        for line in BufReader::new(file).lines() {
            let line =
                line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            if line.trim().is_empty() || !line.contains("\"l2Msg\"") {
                continue;
            }

            let envelope = decode_envelope(line.as_bytes()).unwrap_or_else(|err| {
                panic!("failed to decode envelope in {}: {err}", path.display())
            });
            let mut payload = envelope.l2msg_b64.to_vec();
            let decoded_len = base64_simd::forgiving_decode_inplace(&mut payload)
                .unwrap_or_else(|err| {
                    panic!("failed to decode l2msg base64 in {}: {err}", path.display())
                })
                .len();
            let payload = &payload[..decoded_len];
            let mut found = None;

            decode_l2msg_recursive_raw(payload, |raw| {
                if found.is_some() {
                    return;
                }
                let Some(tx) = decode_signed_tx(raw) else {
                    return;
                };
                if tx.chain_id != ARB_ONE_CHAIN_ID {
                    return;
                }
                let Some(selector_bytes) = tx.data.get(..4) else {
                    return;
                };
                let mut selector = [0u8; 4];
                selector.copy_from_slice(selector_bytes);
                let Some(class) = registry.lookup(&selector) else {
                    return;
                };

                let mut intents = Vec::new();
                match class {
                    SelectorClass::Aggregator => {
                        intents.extend(decode_aggregator_intents(selector, tx.data));
                    }
                    SelectorClass::V2Router | SelectorClass::V3Router => {
                        if let Ok(intent) =
                            engine_detect::decode_router_intent(class, selector, tx.data)
                        {
                            intents.push(intent);
                        }
                    }
                }

                for intent in intents {
                    if !intent.exact_input
                        || intent.amount_in.is_zero()
                        || intent.amount_in > AlloyU256::from(u128::MAX)
                    {
                        continue;
                    }

                    let mut matched_hop = None;
                    let mut matched_pool = None;
                    for hop in &intent.hops {
                        if hop.pool.is_none()
                            && hop.fee_pips > 0
                            && hop.token_in != [0u8; 20]
                            && hop.token_out != [0u8; 20]
                            && hop.token_in != hop.token_out
                        {
                            if let Some(pool) = cached_tiered_v3_pool_for_hop(cached_pools, hop) {
                                matched_hop = Some(*hop);
                                matched_pool = Some(pool);
                                break;
                            }
                        }
                    }

                    if diagnostics.len() < 12 {
                        diagnostics.push(format!(
                            "{corpus_name}: selector=0x{} to={} exact_input={} amount={} matched_pool={} hops={}",
                            faster_hex::hex_string(&selector),
                            hex_addr(&tx.to),
                            intent.exact_input,
                            intent.amount_in,
                            matched_pool
                                .map(|pool| hex_addr(&pool.address))
                                .unwrap_or_else(|| "none".into()),
                            intent
                                .hops
                                .iter()
                                .map(|hop| format!(
                                    "pool={} fee={} in={} out={}",
                                    hop.pool
                                        .map(|pool| hex_addr(&pool))
                                        .unwrap_or_else(|| "none".into()),
                                    hop.fee_pips,
                                    hex_addr(&hop.token_in),
                                    hex_addr(&hop.token_out)
                                ))
                                .collect::<Vec<_>>()
                                .join(";")
                        ));
                    }

                    let (Some(hop), Some(cached_pool)) = (matched_hop, matched_pool) else {
                        continue;
                    };

                    found = Some(CacheBackedPipelineCase {
                        case: RealCorpusPipelineCase {
                            frame_json: line.clone(),
                            router_address: tx.to,
                            selector,
                            token_in: hop.token_in,
                            token_out: hop.token_out,
                            fee_pips: hop.fee_pips,
                            intent,
                        },
                        cached_pool,
                        corpus_name: corpus_name.clone(),
                    });
                    return;
                }
            });

            if let Some(case) = found {
                return Some(case);
            }
        }
    }

    // No cache-backed V3 frame in the PRESENT corpus. Return None so the caller can skip LOUDLY
    // when the gitignored fallback capture is absent (and fail if the full corpus was present).
    let _ = &diagnostics;
    None
}

fn first_cache_backed_bounded_v2_real_router_frame(
    paths: &[PathBuf],
    cached_pools: &[CachedV2Pool],
) -> Option<CacheBackedV2PipelineCase> {
    assert!(
        !cached_pools.is_empty(),
        "discovery cache must contain V2-family pools for cache-backed threaded proof"
    );
    let registry = SelectorRegistry::canonical();
    let mut diagnostics = Vec::new();

    for path in paths {
        let file = File::open(path)
            .unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
        let corpus_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        for line in BufReader::new(file).lines() {
            let line =
                line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            if line.trim().is_empty() || !line.contains("\"l2Msg\"") {
                continue;
            }

            let envelope = decode_envelope(line.as_bytes()).unwrap_or_else(|err| {
                panic!("failed to decode envelope in {}: {err}", path.display())
            });
            let mut payload = envelope.l2msg_b64.to_vec();
            let decoded_len = base64_simd::forgiving_decode_inplace(&mut payload)
                .unwrap_or_else(|err| {
                    panic!("failed to decode l2msg base64 in {}: {err}", path.display())
                })
                .len();
            let payload = &payload[..decoded_len];
            let mut found = None;

            decode_l2msg_recursive_raw(payload, |raw| {
                if found.is_some() {
                    return;
                }
                let Some(tx) = decode_signed_tx(raw) else {
                    return;
                };
                if tx.chain_id != ARB_ONE_CHAIN_ID {
                    return;
                }
                let Some(selector_bytes) = tx.data.get(..4) else {
                    return;
                };
                let mut selector = [0u8; 4];
                selector.copy_from_slice(selector_bytes);
                let Some(class) = registry.lookup(&selector) else {
                    return;
                };

                let mut intents = Vec::new();
                match class {
                    SelectorClass::Aggregator => {
                        intents.extend(decode_aggregator_intents(selector, tx.data));
                    }
                    SelectorClass::V2Router | SelectorClass::V3Router => {
                        if let Ok(intent) =
                            engine_detect::decode_router_intent(class, selector, tx.data)
                        {
                            intents.push(intent);
                        }
                    }
                }

                for intent in intents {
                    if !intent.exact_input
                        || intent.amount_in.is_zero()
                        || intent.amount_in > AlloyU256::from(u128::MAX)
                    {
                        continue;
                    }

                    let mut matched_hop = None;
                    let mut matched_pool = None;
                    for hop in &intent.hops {
                        if hop.pool.is_none()
                            && hop.fee_pips == 0
                            && hop.token_in != [0u8; 20]
                            && hop.token_out != [0u8; 20]
                            && hop.token_in != hop.token_out
                        {
                            if let Some(pool) = cached_v2_pool_for_hop(cached_pools, hop) {
                                matched_hop = Some(*hop);
                                matched_pool = Some(pool);
                                break;
                            }
                        }
                    }

                    if diagnostics.len() < 12 {
                        diagnostics.push(format!(
                            "{corpus_name}: selector=0x{} to={} exact_input={} amount={} matched_pool={} hops={}",
                            faster_hex::hex_string(&selector),
                            hex_addr(&tx.to),
                            intent.exact_input,
                            intent.amount_in,
                            matched_pool
                                .map(|pool| hex_addr(&pool.address))
                                .unwrap_or_else(|| "none".into()),
                            intent
                                .hops
                                .iter()
                                .map(|hop| format!(
                                    "pool={} fee={} in={} out={}",
                                    hop.pool
                                        .map(|pool| hex_addr(&pool))
                                        .unwrap_or_else(|| "none".into()),
                                    hop.fee_pips,
                                    hex_addr(&hop.token_in),
                                    hex_addr(&hop.token_out)
                                ))
                                .collect::<Vec<_>>()
                                .join(";")
                        ));
                    }

                    let (Some(hop), Some(cached_pool)) = (matched_hop, matched_pool) else {
                        continue;
                    };

                    found = Some(CacheBackedV2PipelineCase {
                        case: RealCorpusPipelineCase {
                            frame_json: line.clone(),
                            router_address: tx.to,
                            selector,
                            token_in: hop.token_in,
                            token_out: hop.token_out,
                            fee_pips: hop.fee_pips,
                            intent,
                        },
                        cached_pool,
                        corpus_name: corpus_name.clone(),
                    });
                    return;
                }
            });

            if let Some(case) = found {
                return Some(case);
            }
        }
    }

    // No cache-backed V2 frame in the PRESENT corpus. Return None so the caller can skip LOUDLY
    // when the gitignored fallback capture is absent (and fail if the full corpus was present).
    let _ = &diagnostics;
    None
}

fn is_v2_compatible_hop(hop: &&DecodedHop) -> bool {
    hop.pool.is_none()
        && hop.fee_pips == 0
        && hop.token_in != [0u8; 20]
        && hop.token_out != [0u8; 20]
        && hop.token_in != hop.token_out
}

fn is_v3_compatible_hop(hop: &&DecodedHop) -> bool {
    hop.pool.is_none()
        && hop.fee_pips > 0
        && hop.token_in != [0u8; 20]
        && hop.token_out != [0u8; 20]
        && hop.token_in != hop.token_out
}

fn build_v2_detection_context(
    token_in: Address,
    token_out: Address,
) -> Arc<engine::pipeline::DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let token_a = ub.add_token(token_in, 18, RiskClass::Permissionless);
    let token_b = ub.add_token(token_out, 6, RiskClass::CentralisedStable);
    ub.set_token_eth_price_q96(token_a, RuintU256::from(1u128) << 96);
    ub.set_token_eth_price_q96(token_b, RuintU256::from(250_000_000u128) << 96);

    ub.add_v2_pool(
        [0xe1; 20],
        PoolKind::UniswapV2,
        token_a,
        token_b,
        VenueId(0),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 4_000_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add high-price V2 pool");
    ub.add_v2_pool(
        [0xe2; 20],
        PoolKind::UniswapV2,
        token_a,
        token_b,
        VenueId(1),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 3_880_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add low-price V2 pool");

    build_detection_context_from_universe(ub)
}

fn v3_state(tick: i32, fee_pips: u32) -> V3State {
    V3State {
        sqrt_price_x96: engine_amm::sqrt_price_at_tick(tick).expect("synthetic V3 tick"),
        tick,
        liquidity: 50_000_000_000_000_000u128,
        fee_pips,
        tick_spacing: 10,
        initialised_ticks: vec![
            engine_amm::TickInfo {
                tick: tick - 100_000,
                liquidity_net: 1_000_000_000_000,
            },
            engine_amm::TickInfo {
                tick: tick + 100_000,
                liquidity_net: -1_000_000_000_000,
            },
        ],
    }
}

fn build_detection_context(
    token_in: Address,
    token_out: Address,
    fee_pips: u32,
) -> Arc<engine::pipeline::DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let token_a = ub.add_token(token_in, 18, RiskClass::Permissionless);
    let token_b = ub.add_token(token_out, 18, RiskClass::CentralisedStable);
    ub.set_token_eth_price_q96(token_a, RuintU256::from(1u128) << 96);
    ub.set_token_eth_price_q96(token_b, RuintU256::from(250_000_000u128) << 96);

    ub.add_v3_pool(
        [0xf1; 20],
        PoolKind::UniswapV3,
        token_a,
        token_b,
        VenueId(0),
        fee_pips,
        10,
        v3_state(0, fee_pips),
    )
    .expect("add high-price pool");
    ub.add_v3_pool(
        [0xf2; 20],
        PoolKind::UniswapV3,
        token_a,
        token_b,
        VenueId(1),
        fee_pips,
        10,
        v3_state(-1_000, fee_pips),
    )
    .expect("add low-price pool");

    build_detection_context_from_universe(ub)
}

fn build_cached_v3_detection_context(
    cached_pool: CachedV3Pool,
) -> Arc<engine::pipeline::DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let token0 = ub.add_token(cached_pool.token0, 18, RiskClass::Permissionless);
    let token1 = ub.add_token(cached_pool.token1, 18, RiskClass::CentralisedStable);
    ub.set_token_eth_price_q96(token0, RuintU256::from(1u128) << 96);
    ub.set_token_eth_price_q96(token1, RuintU256::from(250_000_000u128) << 96);

    ub.add_v3_pool(
        cached_pool.address,
        cached_pool.family,
        token0,
        token1,
        VenueId(0),
        cached_pool.fee_pips,
        cached_pool.tick_spacing,
        v3_state(0, cached_pool.fee_pips),
    )
    .expect("add real cache-backed V3 pool identity");
    ub.add_v3_pool(
        [0xf3; 20],
        cached_pool.family,
        token0,
        token1,
        VenueId(1),
        cached_pool.fee_pips,
        cached_pool.tick_spacing,
        v3_state(-1_000, cached_pool.fee_pips),
    )
    .expect("add synthetic comparison V3 pool");

    build_detection_context_from_universe(ub)
}

fn build_cached_v2_detection_context(
    cached_pool: CachedV2Pool,
) -> Arc<engine::pipeline::DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let token0 = ub.add_token(cached_pool.token0, 18, RiskClass::Permissionless);
    let token1 = ub.add_token(cached_pool.token1, 6, RiskClass::CentralisedStable);
    ub.set_token_eth_price_q96(token0, RuintU256::from(1u128) << 96);
    ub.set_token_eth_price_q96(token1, RuintU256::from(250_000_000u128) << 96);

    ub.add_v2_pool(
        cached_pool.address,
        cached_pool.family,
        token0,
        token1,
        VenueId(0),
        cached_pool.fee_bps,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 4_000_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add real cache-backed V2 pool identity");
    ub.add_v2_pool(
        [0xe3; 20],
        cached_pool.family,
        token0,
        token1,
        VenueId(1),
        cached_pool.fee_bps,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 3_880_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add synthetic comparison V2 pool");

    build_detection_context_from_universe(ub)
}

fn build_detection_context_from_universe(
    ub: UniverseBuilder,
) -> Arc<engine::pipeline::DetectionContext> {
    let universe = ub.build();
    let mut route_table = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut route_table);
    assert!(
        route_table.len() >= 2,
        "real-token synthetic universe must enumerate both route directions"
    );
    for route_id in 0..route_table.len() {
        route_table
            .get_mut(engine_state::RouteId(route_id as u32))
            .hot = true;
    }

    let mut csr = CsrBuilder::new(universe.pools.len() as u32);
    for route in route_table.all() {
        for hop in &route.hops {
            csr.push(hop.pool.0, route.id);
        }
    }

    let n_pools = universe.pools.len();
    Arc::new(engine::pipeline::DetectionContext {
        universe: Arc::new(universe),
        route_table,
        pool_to_hot_routes: csr.build(),
        selectors: engine_detect::SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![
            1u64 << 32;
            n_pools
        ])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    })
}

fn counter_snapshot(pipeline: &engine::pipeline::Pipeline) -> (u64, u64, u64, u64, u64) {
    (
        pipeline.frames_processed.load(Ordering::Relaxed),
        pipeline.dirty_events.load(Ordering::Relaxed),
        pipeline.candidates.load(Ordering::Relaxed),
        pipeline.validated.load(Ordering::Relaxed),
        pipeline.submitted.load(Ordering::Relaxed),
    )
}

fn direct_prediction_count(
    detection: &engine::pipeline::DetectionContext,
    intent: &RouteIntent,
) -> Result<usize, engine_detect::PredictionError> {
    let ctx = PredictorContext {
        registry: &detection.universe.registry,
        pool_table: &detection.universe.pools,
        tokens: &detection.universe.tokens,
        v2_store: &detection.universe.v2_state,
        v3_store: &detection.universe.v3_state,
        curve_store: &detection.universe.curve_state,
        curve_crypto_store: &detection.universe.curve_crypto_state,
        dodo_store: &detection.universe.dodo_state,
        balancer_store: &detection.universe.balancer_state,
        lb_store: &detection.universe.lb_state,
        balancer_weighted_store: &detection.universe.balancer_weighted_state,
        fluid_store: &detection.universe.fluid_state,
        maverick_store: &detection.universe.maverick_state,
    };
    predict_route(&ctx, intent).map(|preds| preds.len())
}

fn wait_for_frame_settle(pipeline: &engine::pipeline::Pipeline) -> (u64, u64, u64, u64, u64) {
    let start = Instant::now();
    let mut first_frame_at = None;
    loop {
        let snapshot = counter_snapshot(pipeline);
        if snapshot.4 > 0 || start.elapsed() >= Duration::from_secs(3) {
            return snapshot;
        }
        if snapshot.0 > 0 {
            let frame_at = *first_frame_at.get_or_insert_with(Instant::now);
            if frame_at.elapsed() >= Duration::from_millis(500) {
                return snapshot;
            }
        }
        if start.elapsed() >= Duration::from_secs(3) {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn bounded_real_saved_corpus_frame_reaches_threaded_dirty_and_candidate_counters() {
    // Worktree-portable: this proof consumes the gitignored, regenerable full sequencer capture
    // (anvil/corpus/capture_*.jsonl). When it is absent (fresh checkout / clean-environment gate)
    // skip LOUDLY so a gate can detect it — never a silent pass.
    let corpus = repo_root().join("anvil/corpus/capture_2026-06-11_79555frames.jsonl");
    if !corpus.exists() {
        eprintln!(
            "SKIPPED: artifact absent: {} (gitignored regenerable full sequencer capture) — \
             bounded threaded-surface proof requires the recorded live capture",
            corpus.display()
        );
        return;
    }
    let case = first_bounded_v2_compatible_real_router_frame(&corpus);
    let detection = build_v2_detection_context(case.token_in, case.token_out);
    let direct_preds = direct_prediction_count(&detection, &case.intent)
        .expect("bounded decoded route intent should predict against synthetic V2 universe");
    assert!(
        direct_preds >= 1,
        "bounded exact saved route intent should dirty at least one resident pool before threaded replay; amount_in={} selector=0x{} router={} token_in={} token_out={}",
        case.intent.amount_in,
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub relay");
    let endpoint = format!(
        "ws://127.0.0.1:{}/feed",
        listener.local_addr().unwrap().port()
    );
    let frame_json = case.frame_json.clone();
    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept relay client");
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).expect("accept websocket");
        ws.send(Message::Text(frame_json.into())).ok();
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
    cfg.routers.push(RouterEntry {
        address: hex_addr(&case.router_address),
        label: "bounded_real_corpus_threaded_pipeline_router".into(),
    });
    assert!(
        !cfg.live_submit,
        "bounded real-corpus laptop guardrail must not enable live sequencer writes"
    );

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let latencies = Arc::new(LatencyHistograms::new());
    let reasons = Arc::new(ReasonRing::new());
    // WI-06 §3.3: attach the sole publisher (over the shared universe) + a write permit before
    // start_threads moves them into the state-writer thread. The permit is a type-level write
    // witness — F1 brands it, so this harness uses the WILDCARD `test_mint` permit to witness a
    // universe it did not itself seal (unreachable in production; dev-only `test-permit` feature).
    let state_writer_permit = engine_state::publication::StateWriterPermit::test_mint();
    let state_publisher = engine_state::publication::StatePublisher::new(
        std::sync::Arc::clone(&detection.universe),
        detection.route_table.len(),
        8192,
    );
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, latencies, reasons, detection);
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    pipeline.start_threads().expect("start pipeline");

    let (frames, dirty, candidates, validated, submitted) = wait_for_frame_settle(&pipeline);

    pipeline.signal_shutdown();
    pipeline.join();
    let _ = server_thread.join();

    assert!(
        frames >= 1,
        "bounded exact saved frame should traverse threaded parser; selector=0x{} router={} token_in={} token_out={} amount_in={} frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}",
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
        case.intent.amount_in,
    );
    assert!(
        dirty >= 1,
        "bounded exact saved frame should create a dirty-pool event; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        candidates >= 1,
        "bounded exact saved frame should reach sim and emit a candidate; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        validated >= 1,
        "bounded exact saved frame candidate should reach validation; frames={frames} dirty={dirty} candidates={candidates} validated={validated}"
    );
    assert!(
        submitted >= 1,
        "bounded exact saved frame should reach dry-run would-submit; frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}"
    );
}

#[test]
fn cache_backed_bounded_saved_v2_frame_reaches_threaded_pipeline_with_real_pool_identity() {
    let cached_pools = load_cached_v2_pools();
    // The tracked blk_464734243 corpus is the primary source; the gitignored regenerable capture
    // is a fallback. Scan only PRESENT files so a clean checkout (capture absent) still runs off
    // the tracked block; skip LOUDLY if no present corpus carries a cache-backed V2 frame.
    let paths: Vec<PathBuf> = [
        repo_root().join("anvil/corpus/blk_464734243.jsonl"),
        repo_root().join("anvil/corpus/capture_2026-06-11_79555frames.jsonl"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    let Some(cache_case) = first_cache_backed_bounded_v2_real_router_frame(&paths, &cached_pools)
    else {
        assert!(
            !repo_root()
                .join("anvil/corpus/capture_2026-06-11_79555frames.jsonl")
                .exists(),
            "no cache-backed V2 frame found even though the full recorded corpus is present"
        );
        eprintln!(
            "SKIPPED: artifact absent: {} (gitignored regenerable full sequencer capture) — no \
             cache-backed V2 frame in the PRESENT corpus; this real-pool-identity threaded proof \
             requires the recorded live capture",
            repo_root()
                .join("anvil/corpus/capture_2026-06-11_79555frames.jsonl")
                .display()
        );
        return;
    };
    let case = &cache_case.case;
    let detection = build_cached_v2_detection_context(cache_case.cached_pool);
    let direct_preds = direct_prediction_count(&detection, &case.intent)
        .expect("cache-backed bounded saved V2 route intent should predict");
    assert!(
        direct_preds >= 1,
        "cache-backed bounded saved V2 route intent should dirty the real cached pool identity before threaded replay; corpus={} selector=0x{} router={} pool={} token_in={} token_out={} fee_bps={} amount_in={}",
        cache_case.corpus_name,
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&cache_case.cached_pool.address),
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
        cache_case.cached_pool.fee_bps,
        case.intent.amount_in,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub relay");
    let endpoint = format!(
        "ws://127.0.0.1:{}/feed",
        listener.local_addr().unwrap().port()
    );
    let frame_json = case.frame_json.clone();
    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept relay client");
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).expect("accept websocket");
        ws.send(Message::Text(frame_json.into())).ok();
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
    cfg.routers.push(RouterEntry {
        address: hex_addr(&case.router_address),
        label: "cache_backed_bounded_v2_real_corpus_threaded_pipeline_router".into(),
    });
    assert!(
        !cfg.live_submit,
        "cache-backed V2 real-corpus laptop guardrail must not enable live sequencer writes"
    );

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let latencies = Arc::new(LatencyHistograms::new());
    let reasons = Arc::new(ReasonRing::new());
    // WI-06 §3.3: attach the sole publisher (over the shared universe) + a write permit before
    // start_threads moves them into the state-writer thread. The permit is a type-level write
    // witness — F1 brands it, so this harness uses the WILDCARD `test_mint` permit to witness a
    // universe it did not itself seal (unreachable in production; dev-only `test-permit` feature).
    let state_writer_permit = engine_state::publication::StateWriterPermit::test_mint();
    let state_publisher = engine_state::publication::StatePublisher::new(
        std::sync::Arc::clone(&detection.universe),
        detection.route_table.len(),
        8192,
    );
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, latencies, reasons, detection);
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    pipeline.start_threads().expect("start pipeline");

    let (frames, dirty, candidates, validated, submitted) = wait_for_frame_settle(&pipeline);

    pipeline.signal_shutdown();
    pipeline.join();
    let _ = server_thread.join();

    assert!(
        frames >= 1,
        "cache-backed bounded saved V2 frame should traverse threaded parser with real cached pool identity; corpus={} selector=0x{} router={} pool={} family={:?} token_in={} token_out={} fee_bps={} amount_in={} frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}",
        cache_case.corpus_name,
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&cache_case.cached_pool.address),
        cache_case.cached_pool.family,
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
        cache_case.cached_pool.fee_bps,
        case.intent.amount_in,
    );
    assert!(
        dirty >= 1,
        "cache-backed bounded saved V2 frame should create a dirty-pool event; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        candidates >= 1,
        "cache-backed bounded saved V2 frame should reach sim and emit a candidate; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        validated >= 1,
        "cache-backed bounded saved V2 frame candidate should reach validation; frames={frames} dirty={dirty} candidates={candidates} validated={validated}"
    );
    assert!(
        submitted >= 1,
        "cache-backed bounded saved V2 frame should reach dry-run would-submit; frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}"
    );
}

#[test]
fn cache_backed_bounded_saved_v3_frame_reaches_threaded_pipeline_with_real_pool_identity() {
    let cached_pools = load_cached_tiered_v3_pools();
    // Scan only PRESENT corpus files (see the V2 test): the tracked blk_464734243 is primary, the
    // gitignored regenerable capture_*.jsonl is fallback. Skip LOUDLY when no present corpus
    // carries a cache-backed V3 frame (and only then — panic if the full corpus is present).
    let paths: Vec<PathBuf> = [
        repo_root().join("anvil/corpus/blk_464734243.jsonl"),
        repo_root().join("anvil/corpus/capture_2026-06-11_79555frames.jsonl"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    let Some(cache_case) = first_cache_backed_bounded_v3_real_router_frame(&paths, &cached_pools)
    else {
        assert!(
            !repo_root()
                .join("anvil/corpus/capture_2026-06-11_79555frames.jsonl")
                .exists(),
            "no cache-backed V3 frame found even though the full recorded corpus is present"
        );
        eprintln!(
            "SKIPPED: artifact absent: {} (gitignored regenerable full sequencer capture) — no \
             cache-backed V3 frame in the PRESENT corpus; this real-pool-identity threaded proof \
             requires the recorded live capture",
            repo_root()
                .join("anvil/corpus/capture_2026-06-11_79555frames.jsonl")
                .display()
        );
        return;
    };
    let case = &cache_case.case;
    let detection = build_cached_v3_detection_context(cache_case.cached_pool);
    let direct_preds = direct_prediction_count(&detection, &case.intent)
        .expect("cache-backed bounded saved V3 route intent should predict");
    assert!(
        direct_preds >= 1,
        "cache-backed bounded saved V3 route intent should dirty the real cached pool identity before threaded replay; corpus={} selector=0x{} router={} pool={} token_in={} token_out={} fee_pips={} amount_in={}",
        cache_case.corpus_name,
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&cache_case.cached_pool.address),
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
        case.fee_pips,
        case.intent.amount_in,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub relay");
    let endpoint = format!(
        "ws://127.0.0.1:{}/feed",
        listener.local_addr().unwrap().port()
    );
    let frame_json = case.frame_json.clone();
    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept relay client");
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).expect("accept websocket");
        ws.send(Message::Text(frame_json.into())).ok();
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
    cfg.routers.push(RouterEntry {
        address: hex_addr(&case.router_address),
        label: "cache_backed_bounded_real_corpus_threaded_pipeline_router".into(),
    });
    assert!(
        !cfg.live_submit,
        "cache-backed real-corpus laptop guardrail must not enable live sequencer writes"
    );

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let latencies = Arc::new(LatencyHistograms::new());
    let reasons = Arc::new(ReasonRing::new());
    // WI-06 §3.3: attach the sole publisher (over the shared universe) + a write permit before
    // start_threads moves them into the state-writer thread. The permit is a type-level write
    // witness — F1 brands it, so this harness uses the WILDCARD `test_mint` permit to witness a
    // universe it did not itself seal (unreachable in production; dev-only `test-permit` feature).
    let state_writer_permit = engine_state::publication::StateWriterPermit::test_mint();
    let state_publisher = engine_state::publication::StatePublisher::new(
        std::sync::Arc::clone(&detection.universe),
        detection.route_table.len(),
        8192,
    );
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, latencies, reasons, detection);
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    pipeline.start_threads().expect("start pipeline");

    let (frames, dirty, candidates, validated, submitted) = wait_for_frame_settle(&pipeline);

    pipeline.signal_shutdown();
    pipeline.join();
    let _ = server_thread.join();

    assert!(
        frames >= 1,
        "cache-backed bounded saved V3 frame should traverse threaded parser with real cached pool identity; corpus={} selector=0x{} router={} pool={} family={:?} token_in={} token_out={} fee_pips={} amount_in={} frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}",
        cache_case.corpus_name,
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&cache_case.cached_pool.address),
        cache_case.cached_pool.family,
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
        case.fee_pips,
        case.intent.amount_in,
    );
    assert!(
        dirty >= 1,
        "cache-backed bounded saved V3 frame should create a dirty-pool event; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        candidates >= 1,
        "cache-backed bounded saved V3 frame should reach sim and emit a candidate; frames={frames} dirty={dirty} candidates={candidates}"
    );
    assert!(
        validated >= 1,
        "cache-backed bounded saved V3 frame candidate should reach validation; frames={frames} dirty={dirty} candidates={candidates} validated={validated}"
    );
    assert!(
        submitted >= 1,
        "cache-backed bounded saved V3 frame should reach dry-run would-submit; frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}"
    );
}

#[test]
fn real_saved_corpus_frame_reaches_threaded_parser_and_exposes_sentinel_amount_boundary() {
    let case = first_v3_compatible_real_router_frame(
        &repo_root().join("anvil/corpus/win_467233190_20.jsonl"),
    );
    let detection = build_detection_context(case.token_in, case.token_out, case.fee_pips);
    assert!(
        case.intent.amount_in > AlloyU256::from(u128::MAX),
        "saved real Universal Router amount must expose the current sentinel/balance boundary"
    );
    let real_direct_preds = direct_prediction_count(&detection, &case.intent)
        .expect("decoded real route intent should predict against synthetic V3 universe");
    assert_eq!(
        real_direct_preds, 0,
        "current predictor must not forward-apply an unbounded real sentinel amount"
    );
    let mut bounded_intent = case.intent.clone();
    bounded_intent.amount_in = AlloyU256::from(1_000_000_000_000_000u128);
    let bounded_direct_preds = direct_prediction_count(&detection, &bounded_intent)
        .expect("bounded decoded route intent should predict against synthetic V3 universe");
    assert!(
        bounded_direct_preds >= 1,
        "decoded real token/fee path should dirty a resident pool once amount is bounded; amount_in={} fee_pips={} token_in={} token_out={}",
        case.intent.amount_in,
        case.fee_pips,
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub relay");
    let endpoint = format!(
        "ws://127.0.0.1:{}/feed",
        listener.local_addr().unwrap().port()
    );
    let frame_json = case.frame_json.clone();
    let server_thread = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept relay client");
        stream.set_nodelay(true).ok();
        let mut ws = tungstenite::accept(stream).expect("accept websocket");
        ws.send(Message::Text(frame_json.into())).ok();
        thread::sleep(Duration::from_millis(200));
        ws.close(None).ok();
    });

    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = endpoint;
    cfg.routers.push(RouterEntry {
        address: hex_addr(&case.router_address),
        label: "real_corpus_threaded_pipeline_router".into(),
    });
    assert!(
        !cfg.live_submit,
        "real-corpus laptop guardrail must not enable live sequencer writes"
    );

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let latencies = Arc::new(LatencyHistograms::new());
    let reasons = Arc::new(ReasonRing::new());
    // WI-06 §3.3: attach the sole publisher (over the shared universe) + a write permit before
    // start_threads moves them into the state-writer thread. The permit is a type-level write
    // witness — F1 brands it, so this harness uses the WILDCARD `test_mint` permit to witness a
    // universe it did not itself seal (unreachable in production; dev-only `test-permit` feature).
    let state_writer_permit = engine_state::publication::StateWriterPermit::test_mint();
    let state_publisher = engine_state::publication::StatePublisher::new(
        std::sync::Arc::clone(&detection.universe),
        detection.route_table.len(),
        8192,
    );
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, latencies, reasons, detection);
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    pipeline.start_threads().expect("start pipeline");

    let (frames, dirty, candidates, validated, submitted) = wait_for_frame_settle(&pipeline);

    pipeline.signal_shutdown();
    pipeline.join();
    let _ = server_thread.join();

    assert!(
        frames >= 1,
        "real saved frame should traverse threaded parser; selector=0x{} router={} token_in={} token_out={} fee_pips={} frames={frames} dirty={dirty} candidates={candidates} validated={validated} submitted={submitted}",
        faster_hex::hex_string(&case.selector),
        hex_addr(&case.router_address),
        hex_addr(&case.token_in),
        hex_addr(&case.token_out),
        case.fee_pips,
    );
    assert_eq!(
        dirty, 0,
        "current laptop boundary: real sentinel amount must not be overclaimed as a dirty prediction"
    );
    assert_eq!(
        candidates, 0,
        "without a dirty event, the real saved frame must not be overclaimed as compute-E2E"
    );
    assert_eq!(
        validated, 0,
        "without a candidate, the real saved frame must not be overclaimed as validator-E2E"
    );
    assert_eq!(
        submitted, 0,
        "without validation, the real saved frame must not reach dry-run would-submit"
    );
}
