//! Real saved-corpus replay smoke for laptop certification.
//!
//! This is intentionally narrower than a full arbitrage compute benchmark: it proves that
//! saved live-feed frames from `anvil/corpus` are present and decode through the replay
//! driver with non-vacuous signed-transaction counters. It must not be cited as route
//! profitability, exact-state replay, or live-feed freshness proof.

use std::{fs, path::Path};

use engine_replay::{FeedFrameSource, ReplayDriver, ReplayStats};

#[derive(Debug)]
struct CorpusExpectation {
    rel_path: &'static str,
    min_bytes: u64,
    exact_frames: u64,
    min_signed_txs: u64,
}

fn repo_path(rel_path: &str) -> String {
    format!("{}/../../../{rel_path}", env!("CARGO_MANIFEST_DIR"))
}

fn replay_corpus(path: &Path) -> ReplayStats {
    let frames: Vec<_> = FeedFrameSource::open(path)
        .unwrap_or_else(|err| panic!("open saved corpus {}: {err}", path.display()))
        .collect();
    assert!(
        !frames.is_empty(),
        "saved corpus {} must contain replay frames",
        path.display()
    );

    let mut driver = ReplayDriver::new();
    for frame in &frames {
        let _ = driver.process(frame);
    }
    driver.stats().clone()
}

#[test]
fn saved_real_corpus_frames_decode_through_replay_driver_with_nonzero_signed_txs() {
    for expected in [
        CorpusExpectation {
            rel_path: "anvil/corpus/blk_465686729.jsonl",
            min_bytes: 18_000,
            exact_frames: 29,
            min_signed_txs: 28,
        },
        CorpusExpectation {
            rel_path: "anvil/corpus/win_467233190_20.jsonl",
            min_bytes: 80_000,
            exact_frames: 129,
            min_signed_txs: 100,
        },
    ] {
        let path = repo_path(expected.rel_path);
        let path = Path::new(&path);
        let metadata = fs::metadata(path)
            .unwrap_or_else(|err| panic!("missing saved corpus {}: {err}", path.display()));
        assert!(
            metadata.len() >= expected.min_bytes,
            "{} is too small for the pinned laptop corpus: {} < {} bytes",
            expected.rel_path,
            metadata.len(),
            expected.min_bytes
        );

        let stats = replay_corpus(path);
        assert_eq!(
            stats.frames_seen, expected.exact_frames,
            "{} frame-count drift",
            expected.rel_path
        );
        assert_eq!(
            stats.frames_parsed, expected.exact_frames,
            "{} parsed-frame drift",
            expected.rel_path
        );
        assert!(
            stats.signed_tx_count >= expected.min_signed_txs,
            "{} decoded too few signed transactions: {} < {}; stats={stats:?}",
            expected.rel_path,
            stats.signed_tx_count,
            expected.min_signed_txs
        );
        assert_eq!(
            stats.sequence_gaps, 0,
            "{} must be a contiguous saved feed window",
            expected.rel_path
        );
        assert_eq!(
            stats.parser_bug_drops, 0,
            "{} must not hit malformed-frame/parser-bug drops; stats={stats:?}",
            expected.rel_path
        );
        assert!(
            stats.unsupported_envelope_kind_drops + stats.relevant_swap_drops
                < stats.frames_seen / 5,
            "{} has too many undecoded feed envelopes for certification smoke: stats={stats:?}",
            expected.rel_path
        );
    }
}
