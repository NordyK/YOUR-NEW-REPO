use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use engine_feed::{
    decode_envelope, decode_l2msg_recursive_raw, decode_signed_tx, Address, ARB_ONE_CHAIN_ID,
};

const CLASSIFIER_SURFACE_SELECTORS: [[u8; 4]; 6] = [
    [0x09, 0x5e, 0xa7, 0xb3],
    [0x24, 0x85, 0x6b, 0xc3],
    [0x35, 0x93, 0x56, 0x4c],
    [0x76, 0x5e, 0x82, 0x7f],
    [0xa9, 0x05, 0x9c, 0xbb],
    [0xdc, 0x25, 0x01, 0x11],
];

#[derive(Debug, Default)]
struct CorpusSelectorStats {
    frames_seen: usize,
    raw_txs_seen: usize,
    decoded_signed_txs: usize,
    undecodable_raw_txs: usize,
    arbitrum_chain_id_txs: usize,
    calldata_txs: usize,
    selector_txs: usize,
    large_calldata_txs: usize,
    max_calldata_len: usize,
    distinct_to: BTreeSet<Address>,
    distinct_selectors: BTreeSet<[u8; 4]>,
    classifier_surface_hits: BTreeMap<[u8; 4], usize>,
}

#[derive(Debug, Clone, Copy)]
struct CorpusExpectation {
    relative_path: &'static str,
    min_frames: usize,
    min_decoded_signed_txs: usize,
    max_undecodable_raw_txs: usize,
    min_selector_txs: usize,
    min_distinct_to: usize,
    min_distinct_selectors: usize,
    min_large_calldata_txs: usize,
    min_classifier_surface_hits: usize,
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

fn summarize_classifier_hits(hits: &BTreeMap<[u8; 4], usize>) -> String {
    hits.iter()
        .map(|(selector, count)| format!("{}={count}", selector_hex(selector)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decode_corpus(path: &Path) -> CorpusSelectorStats {
    let file =
        File::open(path).unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));
    let mut stats = CorpusSelectorStats::default();

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
            stats.raw_txs_seen += 1;
            let Some(tx) = decode_signed_tx(raw) else {
                stats.undecodable_raw_txs += 1;
                return;
            };

            stats.decoded_signed_txs += 1;
            if tx.chain_id == ARB_ONE_CHAIN_ID {
                stats.arbitrum_chain_id_txs += 1;
            }
            stats.distinct_to.insert(tx.to);
            if !tx.data.is_empty() {
                stats.calldata_txs += 1;
            }
            if tx.data.len() >= 1024 {
                stats.large_calldata_txs += 1;
            }
            stats.max_calldata_len = stats.max_calldata_len.max(tx.data.len());
            if let Some(bytes) = tx.data.get(..4) {
                let mut selector = [0u8; 4];
                selector.copy_from_slice(bytes);
                stats.selector_txs += 1;
                stats.distinct_selectors.insert(selector);
                if CLASSIFIER_SURFACE_SELECTORS.contains(&selector) {
                    *stats.classifier_surface_hits.entry(selector).or_default() += 1;
                }
            }
        });
    }

    stats
}

fn assert_corpus_surface(expectation: CorpusExpectation) {
    let path = repo_root().join(expectation.relative_path);
    let stats = decode_corpus(&path);
    let classifier_surface_hits: usize = stats.classifier_surface_hits.values().sum();

    assert!(
        stats.frames_seen >= expectation.min_frames,
        "{}: expected at least {} feed frames, saw {}",
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
        stats.undecodable_raw_txs <= expectation.max_undecodable_raw_txs,
        "{}: expected at most {} undecodable raw txs, saw {}",
        expectation.relative_path,
        expectation.max_undecodable_raw_txs,
        stats.undecodable_raw_txs
    );
    assert!(
        stats.selector_txs >= expectation.min_selector_txs,
        "{}: expected at least {} txs with 4-byte selectors, saw {}",
        expectation.relative_path,
        expectation.min_selector_txs,
        stats.selector_txs
    );
    assert!(
        stats.distinct_to.len() >= expectation.min_distinct_to,
        "{}: expected at least {} distinct destinations, saw {}",
        expectation.relative_path,
        expectation.min_distinct_to,
        stats.distinct_to.len()
    );
    assert!(
        stats.distinct_selectors.len() >= expectation.min_distinct_selectors,
        "{}: expected at least {} distinct selectors, saw {}",
        expectation.relative_path,
        expectation.min_distinct_selectors,
        stats.distinct_selectors.len()
    );
    assert!(
        stats.large_calldata_txs >= expectation.min_large_calldata_txs,
        "{}: expected at least {} large-calldata txs, saw {} (max_calldata_len={})",
        expectation.relative_path,
        expectation.min_large_calldata_txs,
        stats.large_calldata_txs,
        stats.max_calldata_len
    );
    assert!(
        classifier_surface_hits >= expectation.min_classifier_surface_hits,
        "{}: expected at least {} classifier-surface selector hits, saw {} ({})",
        expectation.relative_path,
        expectation.min_classifier_surface_hits,
        classifier_surface_hits,
        summarize_classifier_hits(&stats.classifier_surface_hits)
    );
}

#[test]
fn saved_arbitrum_corpora_expose_selector_and_calldata_surfaces() {
    for expectation in [
        CorpusExpectation {
            relative_path: "anvil/corpus/blk_465686729.jsonl",
            min_frames: 29,
            min_decoded_signed_txs: 28,
            max_undecodable_raw_txs: 1,
            min_selector_txs: 28,
            min_distinct_to: 8,
            min_distinct_selectors: 8,
            min_large_calldata_txs: 2,
            min_classifier_surface_hits: 2,
        },
        CorpusExpectation {
            relative_path: "anvil/corpus/win_467233190_20.jsonl",
            min_frames: 129,
            min_decoded_signed_txs: 100,
            max_undecodable_raw_txs: 25,
            min_selector_txs: 80,
            min_distinct_to: 30,
            min_distinct_selectors: 25,
            min_large_calldata_txs: 10,
            min_classifier_surface_hits: 10,
        },
    ] {
        assert_corpus_surface(expectation);
    }
}
