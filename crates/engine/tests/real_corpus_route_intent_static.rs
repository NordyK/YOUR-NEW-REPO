use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use engine_detect::{
    decode_aggregator_intents, decode_router_intent, DecodedHop, RouteIntent, SelectorClass,
    SelectorRegistry,
};
use engine_feed::{
    decode_envelope, decode_l2msg_recursive_raw, decode_signed_tx, Address, ARB_ONE_CHAIN_ID,
};

#[derive(Debug, Default)]
struct CorpusRouteIntentStats {
    frames_seen: usize,
    decoded_signed_txs: usize,
    arbitrum_chain_id_txs: usize,
    calldata_txs: usize,
    selector_txs: usize,
    known_selector_txs: usize,
    decodable_intent_txs: usize,
    intents_total: usize,
    hops_total: usize,
    nonzero_amount_in_intents: usize,
    exact_input_intents: usize,
    token_pair_hops: usize,
    concrete_pool_hops: usize,
    distinct_tokens: BTreeSet<Address>,
    distinct_concrete_pools: BTreeSet<Address>,
    selector_hits: BTreeMap<[u8; 4], usize>,
    decoded_selector_hits: BTreeMap<[u8; 4], usize>,
    class_hits: BTreeMap<&'static str, usize>,
}

#[derive(Debug, Clone, Copy)]
struct CorpusExpectation {
    relative_path: &'static str,
    min_frames: usize,
    min_decoded_signed_txs: usize,
    min_known_selector_txs: usize,
    min_decodable_intent_txs: usize,
    min_intents_total: usize,
    min_hops_total: usize,
    min_distinct_tokens: usize,
    min_selector_kinds_decoded: usize,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn selector_hex(selector: &[u8; 4]) -> String {
    let mut out = String::with_capacity(10);
    out.push_str("0x");
    for byte in selector {
        write!(&mut out, "{byte:02x}").expect("write selector hex");
    }
    out
}

fn map_summary(map: &BTreeMap<[u8; 4], usize>) -> String {
    map.iter()
        .map(|(selector, count)| format!("{}={count}", selector_hex(selector)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn class_name(class: SelectorClass) -> &'static str {
    match class {
        SelectorClass::V2Router => "v2_router",
        SelectorClass::V3Router => "v3_router",
        SelectorClass::Aggregator => "aggregator",
    }
}

fn record_hop(stats: &mut CorpusRouteIntentStats, hop: &DecodedHop) {
    stats.hops_total += 1;
    if hop.token_in != [0u8; 20] && hop.token_out != [0u8; 20] {
        stats.token_pair_hops += 1;
        stats.distinct_tokens.insert(hop.token_in);
        stats.distinct_tokens.insert(hop.token_out);
    }
    if let Some(pool) = hop.pool {
        stats.concrete_pool_hops += 1;
        stats.distinct_concrete_pools.insert(pool);
    }
}

fn record_intent(stats: &mut CorpusRouteIntentStats, intent: &RouteIntent) {
    stats.intents_total += 1;
    if !intent.amount_in.is_zero() {
        stats.nonzero_amount_in_intents += 1;
    }
    if intent.exact_input {
        stats.exact_input_intents += 1;
    }
    for hop in &intent.hops {
        record_hop(stats, hop);
    }
}

fn decode_corpus(path: &Path) -> CorpusRouteIntentStats {
    let file =
        File::open(path).unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let registry = SelectorRegistry::canonical();
    let mut stats = CorpusRouteIntentStats::default();

    for line in BufReader::new(file).lines() {
        let line = line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if line.trim().is_empty() {
            continue;
        }

        stats.frames_seen += 1;
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
            if tx.chain_id == ARB_ONE_CHAIN_ID {
                stats.arbitrum_chain_id_txs += 1;
            }
            if !tx.data.is_empty() {
                stats.calldata_txs += 1;
            }
            let Some(selector_bytes) = tx.data.get(..4) else {
                return;
            };
            let mut selector = [0u8; 4];
            selector.copy_from_slice(selector_bytes);
            stats.selector_txs += 1;
            *stats.selector_hits.entry(selector).or_default() += 1;

            let Some(class) = registry.lookup(&selector) else {
                return;
            };
            stats.known_selector_txs += 1;
            *stats.class_hits.entry(class_name(class)).or_default() += 1;

            let before = stats.intents_total;
            match class {
                SelectorClass::Aggregator => {
                    for intent in decode_aggregator_intents(selector, tx.data) {
                        record_intent(&mut stats, &intent);
                    }
                }
                SelectorClass::V2Router | SelectorClass::V3Router => {
                    if let Ok(intent) = decode_router_intent(class, selector, tx.data) {
                        record_intent(&mut stats, &intent);
                    }
                }
            }
            if stats.intents_total > before {
                stats.decodable_intent_txs += 1;
                *stats.decoded_selector_hits.entry(selector).or_default() += 1;
            }
        });
    }

    stats
}

fn assert_route_intent_surface(expectation: CorpusExpectation) {
    let path = repo_root().join(expectation.relative_path);
    let stats = decode_corpus(&path);

    assert!(
        stats.frames_seen >= expectation.min_frames,
        "{}: expected at least {} frames, saw {}",
        expectation.relative_path,
        expectation.min_frames,
        stats.frames_seen
    );
    assert!(
        stats.decoded_signed_txs >= expectation.min_decoded_signed_txs,
        "{}: expected at least {} decoded signed txs, saw {}",
        expectation.relative_path,
        expectation.min_decoded_signed_txs,
        stats.decoded_signed_txs
    );
    assert_eq!(
        stats.decoded_signed_txs, stats.arbitrum_chain_id_txs,
        "{}: every decoded signed tx must carry Arbitrum One chain id {}",
        expectation.relative_path, ARB_ONE_CHAIN_ID
    );
    assert!(
        stats.known_selector_txs >= expectation.min_known_selector_txs,
        "{}: expected at least {} known selector txs, saw {}; selector_hits=[{}]",
        expectation.relative_path,
        expectation.min_known_selector_txs,
        stats.known_selector_txs,
        map_summary(&stats.selector_hits)
    );
    assert!(
        stats.decodable_intent_txs >= expectation.min_decodable_intent_txs,
        "{}: expected at least {} txs with decoded route intents, saw {}; decoded_selectors=[{}]; class_hits={:?}",
        expectation.relative_path,
        expectation.min_decodable_intent_txs,
        stats.decodable_intent_txs,
        map_summary(&stats.decoded_selector_hits),
        stats.class_hits
    );
    assert!(
        stats.intents_total >= expectation.min_intents_total,
        "{}: expected at least {} decoded intents, saw {}",
        expectation.relative_path,
        expectation.min_intents_total,
        stats.intents_total
    );
    assert!(
        stats.hops_total >= expectation.min_hops_total,
        "{}: expected at least {} decoded intent hops, saw {}",
        expectation.relative_path,
        expectation.min_hops_total,
        stats.hops_total
    );
    if expectation.min_decodable_intent_txs > 0 {
        assert!(
            stats.token_pair_hops > 0,
            "{}: decoded intents must expose token-pair hops",
            expectation.relative_path
        );
        assert!(
            stats.nonzero_amount_in_intents > 0,
            "{}: decoded intents must include nonzero amount_in evidence",
            expectation.relative_path
        );
        assert!(
            stats.exact_input_intents > 0,
            "{}: decoded intents must include exact-input flow",
            expectation.relative_path
        );
    }
    assert!(
        stats.distinct_tokens.len() >= expectation.min_distinct_tokens,
        "{}: expected at least {} distinct decoded route tokens, saw {}",
        expectation.relative_path,
        expectation.min_distinct_tokens,
        stats.distinct_tokens.len()
    );
    assert!(
        stats.decoded_selector_hits.len() >= expectation.min_selector_kinds_decoded,
        "{}: expected at least {} decoded selector kinds, saw {} ({})",
        expectation.relative_path,
        expectation.min_selector_kinds_decoded,
        stats.decoded_selector_hits.len(),
        map_summary(&stats.decoded_selector_hits)
    );
}

#[test]
fn saved_arbitrum_corpora_decode_real_route_intents() {
    let corpus_dir = repo_root().join("anvil/corpus");
    if !corpus_dir.exists() {
        eprintln!(
            "SKIP: artifact {} absent (out-of-branch)",
            corpus_dir.display()
        );
        return;
    }
    for expectation in [
        CorpusExpectation {
            relative_path: "anvil/corpus/blk_465686729.jsonl",
            min_frames: 29,
            min_decoded_signed_txs: 28,
            min_known_selector_txs: 0,
            min_decodable_intent_txs: 0,
            min_intents_total: 0,
            min_hops_total: 0,
            min_distinct_tokens: 0,
            min_selector_kinds_decoded: 0,
        },
        CorpusExpectation {
            relative_path: "anvil/corpus/win_467233190_20.jsonl",
            min_frames: 129,
            min_decoded_signed_txs: 100,
            min_known_selector_txs: 2,
            min_decodable_intent_txs: 1,
            min_intents_total: 1,
            min_hops_total: 1,
            min_distinct_tokens: 2,
            min_selector_kinds_decoded: 1,
        },
    ] {
        assert_route_intent_surface(expectation);
    }
}
