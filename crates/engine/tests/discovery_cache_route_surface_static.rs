use std::{
    collections::HashSet,
    fmt::Write as _,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use alloy_primitives::U256;
use engine_detect::{
    decode_aggregator_intents, decode_router_intent, DecodedHop, RouteIntent, SelectorClass,
    SelectorRegistry,
};
use engine_feed::{
    decode_envelope, decode_l2msg_recursive_raw, decode_signed_tx, Address, ARB_ONE_CHAIN_ID,
};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PairKey {
    lo: Address,
    hi: Address,
}

impl PairKey {
    fn new(a: Address, b: Address) -> Self {
        if a <= b {
            Self { lo: a, hi: b }
        } else {
            Self { lo: b, hi: a }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct V3Key {
    pair: PairKey,
    fee_pips: u32,
}

#[derive(Debug, Default)]
struct CacheSurface {
    total_pools: usize,
    fluid_pools: usize,
    any_pairs: HashSet<PairKey>,
    v2_pairs: HashSet<PairKey>,
    tiered_v3_pairs: HashSet<V3Key>,
    algebra_pairs: HashSet<PairKey>,
}

#[derive(Debug, Default)]
struct CoverageStats {
    corpus_files: usize,
    frames_seen: usize,
    l2_frames_seen: usize,
    skipped_non_l2_frames: usize,
    decoded_signed_txs: usize,
    arbitrum_chain_id_txs: usize,
    bounded_leading_token_pair_intents: usize,
    bounded_leading_v2_intents: usize,
    bounded_leading_v3_intents: usize,
    any_cache_pair_present: usize,
    predictor_resolvable: usize,
    v2_predictor_resolvable: usize,
    v3_predictor_resolvable: usize,
    concrete_pool_intents: usize,
    threaded_bounded_sample_seen: bool,
    threaded_bounded_sample_resolvable: bool,
    samples_resolved: Vec<String>,
    samples_missing: Vec<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn corpus_paths() -> Vec<PathBuf> {
    let mut paths = fs::read_dir(repo_root().join("anvil/corpus"))
        .expect("read corpus dir")
        .map(|entry| entry.expect("read corpus dir entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn address_hex(addr: &Address) -> String {
    format!("0x{}", faster_hex::hex_string(addr))
}

fn selector_hex(selector: &[u8; 4]) -> String {
    let mut out = String::with_capacity(8);
    for byte in selector {
        write!(&mut out, "{byte:02x}").expect("write selector");
    }
    out
}

fn value_address(value: &Value, field: &str) -> Address {
    let array = value
        .get(field)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("discovery cache pool missing {field} array: {value}"));
    assert_eq!(array.len(), 20, "discovery cache {field} must be 20 bytes");
    let mut out = [0u8; 20];
    for (idx, byte) in array.iter().enumerate() {
        out[idx] = byte
            .as_u64()
            .unwrap_or_else(|| panic!("discovery cache {field}[{idx}] is not u64: {value}"))
            as u8;
    }
    out
}

fn value_u32(value: &Value, field: &str) -> u32 {
    value
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("discovery cache pool missing numeric {field}: {value}"))
        as u32
}

fn ingest_cache_pool(cache: &mut CacheSurface, pool: &Value) {
    let token0 = value_address(pool, "token0");
    let token1 = value_address(pool, "token1");
    let pair = PairKey::new(token0, token1);
    let family = pool
        .get("family")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("discovery cache pool missing family string: {pool}"));
    cache.total_pools += 1;
    cache.any_pairs.insert(pair);

    match family {
        "UniswapV2" | "SushiV2" | "CamelotV2" => {
            cache.v2_pairs.insert(pair);
        }
        "UniswapV3" | "SushiV3" | "PancakeV3" => {
            cache.tiered_v3_pairs.insert(V3Key {
                pair,
                fee_pips: value_u32(pool, "fee_pips"),
            });
        }
        "AlgebraV19" => {
            cache.algebra_pairs.insert(pair);
        }
        _ => {}
    }
}

fn load_cache_surface() -> CacheSurface {
    // Worktree-portable: read the COMMITTED full identity-only cache snapshot (every pool identity
    // verbatim from the untracked engine/discovery_cache.json). The pair/family surface built
    // below is byte-identical to the live-file baseline, so the cache-side coverage floors hold.
    let bytes = fs::read(
        repo_root().join("codex/campaign_baseline_wi01/discovery_cache_full_identity_golden.json"),
    )
    .expect("read discovery cache full-identity golden fixture");
    let json: Value = serde_json::from_slice(&bytes).expect("parse discovery cache json");
    let mut surface = CacheSurface::default();
    for pool in json
        .get("pools")
        .and_then(Value::as_array)
        .expect("discovery cache pools array")
    {
        ingest_cache_pool(&mut surface, pool);
    }
    let fluid = json
        .get("fluid_pools")
        .and_then(Value::as_array)
        .expect("discovery cache fluid_pools array");
    surface.fluid_pools = fluid.len();
    for pool in fluid {
        ingest_cache_pool(&mut surface, pool);
    }
    surface
}

fn leading_token_pair_hop(intent: &RouteIntent) -> Option<&DecodedHop> {
    let hop = intent.hops.first()?;
    if hop.pool.is_none()
        && hop.token_in != [0u8; 20]
        && hop.token_out != [0u8; 20]
        && hop.token_in != hop.token_out
    {
        Some(hop)
    } else {
        None
    }
}

fn is_bounded_exact_input(intent: &RouteIntent) -> bool {
    intent.exact_input && !intent.amount_in.is_zero() && intent.amount_in <= U256::from(u128::MAX)
}

fn predictor_resolves(cache: &CacheSurface, hop: &DecodedHop) -> bool {
    let pair = PairKey::new(hop.token_in, hop.token_out);
    if hop.fee_pips == 0 {
        cache.v2_pairs.contains(&pair)
    } else {
        cache.tiered_v3_pairs.contains(&V3Key {
            pair,
            fee_pips: hop.fee_pips,
        }) || cache.algebra_pairs.contains(&pair)
    }
}

fn is_threaded_bounded_sample(selector: [u8; 4], intent: &RouteIntent, hop: &DecodedHop) -> bool {
    selector == [0x38, 0xed, 0x17, 0x39]
        && intent.amount_in == U256::from(21_633_030u64)
        && hop.fee_pips == 0
        && address_hex(&hop.token_in) == "0x6efa9b8883dfb78fd75cd89d8474c44c3cbda469"
        && address_hex(&hop.token_out) == "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9"
}

fn record_intent(
    stats: &mut CoverageStats,
    cache: &CacheSurface,
    corpus_name: &str,
    selector: [u8; 4],
    intent: &RouteIntent,
) {
    if intent.hops.iter().any(|hop| hop.pool.is_some()) {
        stats.concrete_pool_intents += 1;
    }
    if !is_bounded_exact_input(intent) {
        return;
    }
    let Some(hop) = leading_token_pair_hop(intent) else {
        return;
    };

    stats.bounded_leading_token_pair_intents += 1;
    if hop.fee_pips == 0 {
        stats.bounded_leading_v2_intents += 1;
    } else {
        stats.bounded_leading_v3_intents += 1;
    }

    let pair = PairKey::new(hop.token_in, hop.token_out);
    let any_pair = cache.any_pairs.contains(&pair);
    if any_pair {
        stats.any_cache_pair_present += 1;
    }
    let resolved = predictor_resolves(cache, hop);
    if resolved {
        stats.predictor_resolvable += 1;
        if hop.fee_pips == 0 {
            stats.v2_predictor_resolvable += 1;
        } else {
            stats.v3_predictor_resolvable += 1;
        }
    }
    if is_threaded_bounded_sample(selector, intent, hop) {
        stats.threaded_bounded_sample_seen = true;
        stats.threaded_bounded_sample_resolvable = resolved;
    }

    let sample = format!(
        "{corpus_name}: selector=0x{} fee={} in={} out={} amount={}",
        selector_hex(&selector),
        hop.fee_pips,
        address_hex(&hop.token_in),
        address_hex(&hop.token_out),
        intent.amount_in
    );
    if resolved && stats.samples_resolved.len() < 8 {
        stats.samples_resolved.push(sample);
    } else if !resolved && stats.samples_missing.len() < 8 {
        stats.samples_missing.push(sample);
    }
}

fn scan_corpus(path: &Path, cache: &CacheSurface, stats: &mut CoverageStats) {
    let file =
        File::open(path).unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let registry = SelectorRegistry::canonical();
    let corpus_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>");

    for line in BufReader::new(file).lines() {
        let line = line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if line.trim().is_empty() {
            continue;
        }

        stats.frames_seen += 1;
        if !line.contains("\"l2Msg\"") {
            stats.skipped_non_l2_frames += 1;
            continue;
        }
        stats.l2_frames_seen += 1;

        let envelope = decode_envelope(line.as_bytes())
            .unwrap_or_else(|err| panic!("failed to decode envelope in {}: {err}", path.display()));
        let mut payload = envelope.l2msg_b64.to_vec();
        let decoded_len = base64_simd::forgiving_decode_inplace(&mut payload)
            .unwrap_or_else(|err| {
                panic!("failed to decode l2msg base64 in {}: {err}", path.display())
            })
            .len();
        let payload = &payload[..decoded_len];

        decode_l2msg_recursive_raw(payload, |raw| {
            let Some(tx) = decode_signed_tx(raw) else {
                return;
            };
            stats.decoded_signed_txs += 1;
            if tx.chain_id != ARB_ONE_CHAIN_ID {
                return;
            }
            stats.arbitrum_chain_id_txs += 1;
            let Some(selector_bytes) = tx.data.get(..4) else {
                return;
            };
            let mut selector = [0u8; 4];
            selector.copy_from_slice(selector_bytes);
            let Some(class) = registry.lookup(&selector) else {
                return;
            };

            match class {
                SelectorClass::Aggregator => {
                    for intent in decode_aggregator_intents(selector, tx.data) {
                        record_intent(stats, cache, corpus_name, selector, &intent);
                    }
                }
                SelectorClass::V2Router | SelectorClass::V3Router => {
                    if let Ok(intent) = decode_router_intent(class, selector, tx.data) {
                        record_intent(stats, cache, corpus_name, selector, &intent);
                    }
                }
            }
        });
    }
}

#[test]
fn saved_corpus_bounded_route_intents_intersect_discovery_cache_pool_identities() {
    let corpus_dir = repo_root().join("anvil/corpus");
    if !corpus_dir.exists() {
        eprintln!(
            "SKIPPED: artifact absent: {} (out-of-branch corpus directory)",
            corpus_dir.display()
        );
        return;
    }
    // The bounded-intent coverage floors below (>=9000 leading-hop intents, >=6000 V2, >=2700 V3)
    // are calibrated to the FULL recorded corpus, which includes the gitignored, regenerable
    // capture_*.jsonl. In a clean checkout only the small tracked per-block corpora are present
    // (a few hundred frames), so those floors are unreachable. Skip LOUDLY when the full capture
    // is absent so a clean-environment gate can detect it — never a silent pass.
    let full_capture = repo_root().join("anvil/corpus/capture_2026-06-11_79555frames.jsonl");
    if !full_capture.exists() {
        eprintln!(
            "SKIPPED: artifact absent: {} (gitignored regenerable full sequencer capture) — the \
             saved-corpus coverage floors require the complete recorded corpus",
            full_capture.display()
        );
        return;
    }
    let cache = load_cache_surface();
    let mut stats = CoverageStats::default();
    let paths = corpus_paths();
    stats.corpus_files = paths.len();
    for path in paths {
        scan_corpus(&path, &cache, &mut stats);
    }

    assert!(
        cache.total_pools >= 3_000,
        "discovery cache should expose the local real pool identity surface; total_pools={} fluid_pools={} any_pairs={} v2_pairs={} tiered_v3_pairs={} algebra_pairs={}",
        cache.total_pools,
        cache.fluid_pools,
        cache.any_pairs.len(),
        cache.v2_pairs.len(),
        cache.tiered_v3_pairs.len(),
        cache.algebra_pairs.len()
    );
    assert!(
        stats.bounded_leading_token_pair_intents >= 9_000,
        "saved corpus should expose a broad bounded leading-hop token-pair surface; stats={stats:?}"
    );
    assert!(
        stats.bounded_leading_v2_intents >= 6_000,
        "saved corpus should expose broad bounded V2 leading-hop route intents; stats={stats:?}"
    );
    assert!(
        stats.bounded_leading_v3_intents >= 2_700,
        "saved corpus should expose broad bounded V3 leading-hop route intents; stats={stats:?}"
    );
    assert!(
        stats.any_cache_pair_present >= 1_600,
        "local discovery cache should contain real pool identities for a material subset of bounded saved route intents; stats={stats:?}"
    );
    assert!(
        stats.predictor_resolvable >= 1_600,
        "local discovery cache should make a material subset of bounded saved route intents predictor-resolvable by current token/fee rules; stats={stats:?}"
    );
    assert!(
        stats.v2_predictor_resolvable >= 150,
        "local discovery cache should cover some bounded V2-router leading-hop intents; stats={stats:?}"
    );
    assert!(
        stats.v3_predictor_resolvable >= 1_470,
        "local discovery cache should cover a broad bounded V3/Algebra leading-hop surface; stats={stats:?}"
    );
    assert!(
        stats.threaded_bounded_sample_seen,
        "the exact bounded V2 frame used by the threaded guardrail must remain visible in the saved-corpus scan; stats={stats:?}"
    );
    assert!(
        !stats.threaded_bounded_sample_resolvable,
        "the current exact bounded V2 threaded sample is intentionally not cache-backed, so that guardrail must remain classified as synthetic-resident-state proof; stats={stats:?}"
    );
}
