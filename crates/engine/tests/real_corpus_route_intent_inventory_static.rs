use std::{
    fmt::Write as _,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use alloy_primitives::U256;
use engine_detect::{
    decode_aggregator_intents, decode_router_intent, RouteIntent, SelectorClass, SelectorRegistry,
};
use engine_feed::{
    decode_envelope, decode_l2msg_recursive_raw, decode_signed_tx, ARB_ONE_CHAIN_ID,
};

#[derive(Debug, Default)]
struct InventoryStats {
    corpus_files: usize,
    frames_seen: usize,
    l2_frames_seen: usize,
    skipped_non_l2_frames: usize,
    decoded_signed_txs: usize,
    arbitrum_chain_id_txs: usize,
    non_arbitrum_chain_id_txs: usize,
    known_selector_txs: usize,
    decodable_intent_txs: usize,
    intents_total: usize,
    exact_input_intents: usize,
    bounded_exact_input_intents: usize,
    unbounded_exact_input_intents: usize,
    token_pair_intents: usize,
    bounded_token_pair_intents: usize,
    v2_token_pair_intents: usize,
    v3_token_pair_intents: usize,
    concrete_pool_intents: usize,
    corpus_summaries: Vec<String>,
    bounded_samples: Vec<String>,
    unbounded_samples: Vec<String>,
}

#[derive(Debug, Default)]
struct CorpusStats {
    frames_seen: usize,
    l2_frames_seen: usize,
    skipped_non_l2_frames: usize,
    decoded_signed_txs: usize,
    known_selector_txs: usize,
    decodable_intent_txs: usize,
    intents_total: usize,
    bounded_exact_input_intents: usize,
    unbounded_exact_input_intents: usize,
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

fn selector_hex(selector: &[u8; 4]) -> String {
    let mut out = String::with_capacity(8);
    for byte in selector {
        write!(&mut out, "{byte:02x}").expect("write selector");
    }
    out
}

fn address_hex(addr: &[u8; 20]) -> String {
    format!("0x{}", faster_hex::hex_string(addr))
}

fn corpus_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unknown>")
        .to_string()
}

fn has_token_pair_hop(intent: &RouteIntent) -> bool {
    intent.hops.iter().any(|hop| {
        hop.pool.is_none()
            && hop.token_in != [0u8; 20]
            && hop.token_out != [0u8; 20]
            && hop.token_in != hop.token_out
    })
}

fn has_concrete_pool_hop(intent: &RouteIntent) -> bool {
    intent.hops.iter().any(|hop| hop.pool.is_some())
}

fn first_hop_summary(intent: &RouteIntent) -> String {
    let Some(hop) = intent.hops.first() else {
        return "no_hops".to_string();
    };
    format!(
        "pool={} fee={} in={} out={}",
        hop.pool
            .map(|pool| address_hex(&pool))
            .unwrap_or_else(|| "none".into()),
        hop.fee_pips,
        address_hex(&hop.token_in),
        address_hex(&hop.token_out)
    )
}

fn record_intent(
    stats: &mut InventoryStats,
    corpus: &mut CorpusStats,
    corpus_name: &str,
    selector: [u8; 4],
    tx_to: [u8; 20],
    intent: &RouteIntent,
) {
    stats.intents_total += 1;
    corpus.intents_total += 1;

    let token_pair = has_token_pair_hop(intent);
    if token_pair {
        stats.token_pair_intents += 1;
        if intent
            .hops
            .iter()
            .any(|hop| hop.pool.is_none() && hop.fee_pips == 0)
        {
            stats.v2_token_pair_intents += 1;
        }
        if intent
            .hops
            .iter()
            .any(|hop| hop.pool.is_none() && hop.fee_pips > 0)
        {
            stats.v3_token_pair_intents += 1;
        }
    }
    if has_concrete_pool_hop(intent) {
        stats.concrete_pool_intents += 1;
    }

    if intent.exact_input {
        stats.exact_input_intents += 1;
        let bounded = !intent.amount_in.is_zero() && intent.amount_in <= U256::from(u128::MAX);
        if bounded {
            stats.bounded_exact_input_intents += 1;
            corpus.bounded_exact_input_intents += 1;
            if token_pair {
                stats.bounded_token_pair_intents += 1;
            }
            if stats.bounded_samples.len() < 8 {
                stats.bounded_samples.push(format!(
                    "{corpus_name}: selector=0x{} to={} amount={} {}",
                    selector_hex(&selector),
                    address_hex(&tx_to),
                    intent.amount_in,
                    first_hop_summary(intent)
                ));
            }
        } else if !intent.amount_in.is_zero() {
            stats.unbounded_exact_input_intents += 1;
            corpus.unbounded_exact_input_intents += 1;
            if stats.unbounded_samples.len() < 8 {
                stats.unbounded_samples.push(format!(
                    "{corpus_name}: selector=0x{} to={} amount={} {}",
                    selector_hex(&selector),
                    address_hex(&tx_to),
                    intent.amount_in,
                    first_hop_summary(intent)
                ));
            }
        }
    }
}

fn scan_corpus(path: &Path, stats: &mut InventoryStats) {
    let file =
        File::open(path).unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let registry = SelectorRegistry::canonical();
    let name = corpus_name(path);
    let mut corpus = CorpusStats::default();

    for line in BufReader::new(file).lines() {
        let line = line.unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if line.trim().is_empty() {
            continue;
        }

        stats.frames_seen += 1;
        corpus.frames_seen += 1;
        if !line.contains("\"l2Msg\"") {
            stats.skipped_non_l2_frames += 1;
            corpus.skipped_non_l2_frames += 1;
            continue;
        }
        stats.l2_frames_seen += 1;
        corpus.l2_frames_seen += 1;
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
            corpus.decoded_signed_txs += 1;
            if tx.chain_id == ARB_ONE_CHAIN_ID {
                stats.arbitrum_chain_id_txs += 1;
            } else {
                stats.non_arbitrum_chain_id_txs += 1;
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
            stats.known_selector_txs += 1;
            corpus.known_selector_txs += 1;

            let before = stats.intents_total;
            match class {
                SelectorClass::Aggregator => {
                    for intent in decode_aggregator_intents(selector, tx.data) {
                        record_intent(stats, &mut corpus, &name, selector, tx.to, &intent);
                    }
                }
                SelectorClass::V2Router | SelectorClass::V3Router => {
                    if let Ok(intent) = decode_router_intent(class, selector, tx.data) {
                        record_intent(stats, &mut corpus, &name, selector, tx.to, &intent);
                    }
                }
            }
            if stats.intents_total > before {
                stats.decodable_intent_txs += 1;
                corpus.decodable_intent_txs += 1;
            }
        });
    }

    stats.corpus_summaries.push(format!(
        "{name}: frames={} l2_frames={} skipped_non_l2={} txs={} known_selector_txs={} intent_txs={} intents={} bounded_exact={} unbounded_exact={}",
        corpus.frames_seen,
        corpus.l2_frames_seen,
        corpus.skipped_non_l2_frames,
        corpus.decoded_signed_txs,
        corpus.known_selector_txs,
        corpus.decodable_intent_txs,
        corpus.intents_total,
        corpus.bounded_exact_input_intents,
        corpus.unbounded_exact_input_intents,
    ));
}

fn scan_all_corpora() -> InventoryStats {
    let mut stats = InventoryStats::default();
    let paths = corpus_paths();
    stats.corpus_files = paths.len();
    for path in paths {
        scan_corpus(&path, &mut stats);
    }
    stats
}

#[test]
fn saved_corpus_route_intent_inventory_exposes_bounded_amount_surface() {
    let corpus_dir = repo_root().join("anvil/corpus");
    if !corpus_dir.exists() {
        eprintln!(
            "SKIP: artifact {} absent (out-of-branch)",
            corpus_dir.display()
        );
        return;
    }
    let stats = scan_all_corpora();

    assert!(
        stats.corpus_files >= 12,
        "expected current saved corpus inventory to include every jsonl file; stats={stats:?}"
    );
    assert!(
        stats.arbitrum_chain_id_txs > 900_000,
        "inventory must scan Arbitrum One txs from the large saved capture corpora; stats={stats:?}"
    );
    assert!(
        stats.decoded_signed_txs > 100_000,
        "inventory must scan the large saved capture corpora, not only tiny windows; stats={stats:?}"
    );
    assert!(
        stats.intents_total > 0,
        "inventory must find at least one decoded route intent; stats={stats:?}"
    );
    assert!(
        stats.non_arbitrum_chain_id_txs > 0,
        "large captures should keep non-Arbitrum/legacy-chain-id decoder boundary visible; stats={stats:?}"
    );
    assert!(
        stats.bounded_exact_input_intents >= 9_000,
        "saved corpus should expose bounded exact-input route intents for exact-frame laptop tests; stats={stats:?}"
    );
    assert!(
        stats.bounded_token_pair_intents >= 9_000,
        "saved corpus should expose bounded token-pair route intents for resident-pool prediction tests; stats={stats:?}"
    );
    assert!(
        stats.v2_token_pair_intents >= 6_000,
        "saved corpus should include V2 token-pair route intents; stats={stats:?}"
    );
    assert!(
        stats.v3_token_pair_intents >= 3_000,
        "saved corpus should include V3 token-pair route intents; stats={stats:?}"
    );
}
