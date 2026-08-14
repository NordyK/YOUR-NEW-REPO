use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const GATE4_DIR: &str = "certification_runs/20260614_213542/gate4";
const CHECKPOINTS_ALIGNED: &str =
    "certification_runs/20260614_213542/gate4/checkpoints_aligned.jsonl";

fn engine_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("engine/crates/engine has engine root ancestor")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    read(path)
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            if line.trim().is_empty() {
                return None;
            }
            Some(
                serde_json::from_str::<Value>(line).unwrap_or_else(|err| {
                    panic!("parse {} line {}: {err}", path.display(), idx + 1)
                }),
            )
        })
        .collect()
}

fn parse_stats(path: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for line in read(path).lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value
            .trim()
            .parse::<u64>()
            .unwrap_or_else(|err| panic!("parse {} {key}: {err}", path.display()));
        out.insert(key, value);
    }
    out
}

#[test]
fn saved_gate4_aligned_checkpoint_artifact_is_non_vacuous_and_equal() {
    let path = engine_root().join(CHECKPOINTS_ALIGNED);
    let rows = read_jsonl(&path);
    assert_eq!(
        rows.len(),
        9,
        "Gate 4 aligned checkpoint row count drifted; reclassify the artifact"
    );

    let mut families = BTreeMap::<String, usize>::new();
    for (idx, row) in rows.iter().enumerate() {
        let line = idx + 1;
        assert_eq!(
            row.get("source").and_then(Value::as_str),
            Some("final_replay_state"),
            "line {line}: checkpoint source should be final replay state"
        );
        assert_eq!(
            row.get("equal").and_then(Value::as_bool),
            Some(true),
            "line {line}: aligned local/fork hashes must match"
        );
        assert_eq!(
            row.get("differences")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0),
            "line {line}: aligned checkpoint row must have no differences"
        );
        for key in ["local_state_hash", "fork_state_hash"] {
            let hash = row
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("line {line}: missing {key}"));
            assert!(
                hash.starts_with("0x") && hash.len() == 66,
                "line {line}: {key} should be a 32-byte hex hash, got {hash}"
            );
        }
        let family = row
            .get("pool_type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("line {line}: missing pool_type"))
            .to_string();
        *families.entry(family).or_default() += 1;
    }

    assert_eq!(families.get("UniswapV3").copied(), Some(5));
    assert_eq!(families.get("PancakeV3").copied(), Some(1));
    assert_eq!(families.get("DodoV2").copied(), Some(2));
    assert_eq!(families.get("CurveStableSwapNg").copied(), Some(1));
    assert_eq!(
        families.len(),
        4,
        "unexpected Gate 4 checkpoint family set: {families:#?}"
    );
}

#[test]
fn saved_gate4_decode_taxonomy_has_no_relevant_parser_or_unknown_drops() {
    let root = engine_root().join(GATE4_DIR);
    for rel in [
        "blk_463159679.stats",
        "win_463159679_20.stats",
        "win_467233190_20.stats",
    ] {
        let path = root.join(rel);
        let stats = parse_stats(&path);
        assert_eq!(
            stats.get("frames_seen"),
            stats.get("frames_parsed"),
            "{rel}: every frame should be classified, even when unsupported envelopes are dropped"
        );
        for key in [
            "sequence_gaps",
            "parser_bug_drops",
            "relevant_swap_drops",
            "unknown_drops",
        ] {
            assert_eq!(
                stats.get(key).copied(),
                Some(0),
                "{rel}: {key} must stay zero"
            );
        }
    }
}
