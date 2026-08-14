use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use serde_json::Value;

const MONITOR_CANDIDATES: &str = "logs/monitor/candidates.jsonl";
const GATE9_A_FILE: &str = "certification_runs/20260614_213542/gate9/A_file_candidates_revm.jsonl";

const REPLAY_SUBMIT_FIELDS: &[&str] = &[
    "calldata",
    "block_env",
    "changed_slots",
    "mantis_executor",
    "payee",
    "gas_limit",
    "min_profit",
    "exchange_ids",
    "exchange_metadata",
    "l1_fee_wei",
    "l2_gas_price_wei",
    "submit_min_profit_wei",
    "submit_calldata_bytes",
];

const GATE9_REPLAY_FIELDS: &[&str] = &[
    "calldata",
    "block_env",
    "changed_slots",
    "mantis_executor",
    "payee",
    "gas_limit",
    "min_profit",
    "exchange_ids",
    "exchange_metadata",
    "l1_fee_wei",
    "l2_gas_price_wei",
];

const RELEASE_RUN_FILES: &[&str] = &[
    "env.redacted",
    "binary.sha256",
    "run_metadata.json",
    "artifact_manifest.json",
    "raw_feed.jsonl",
    "raw_feed.jsonl.freshness.jsonl",
    "feed_candidate_freshness_report.json",
    "candidates.jsonl",
    "paper.jsonl",
    "nodeinterface_gas_matrix.json",
    "live_parity.json",
];

#[derive(Default)]
struct CandidateSummary {
    rows: usize,
    result_counts: BTreeMap<String, usize>,
    positive_net_rows: usize,
    accepted_success_rows: usize,
    accepted_positive_success_rows: usize,
    singleton_offline_rejections: usize,
    rows_with_submit_min_profit: usize,
    rows_with_submit_calldata_bytes: usize,
}

fn engine_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("engine/crates/engine has engine root ancestor")
        .to_path_buf()
}

fn parse_jsonl(path: &Path) -> impl Iterator<Item = (usize, Value)> {
    let label = path.display().to_string();
    let file = File::open(path).unwrap_or_else(|err| panic!("open {}: {err}", path.display()));
    BufReader::new(file)
        .lines()
        .enumerate()
        .filter_map(move |(idx, line)| {
            let line = line.unwrap_or_else(|err| panic!("read {label} line {}: {err}", idx + 1));
            if line.trim().is_empty() {
                return None;
            }
            let value = serde_json::from_str::<Value>(&line)
                .unwrap_or_else(|err| panic!("parse {label} line {}: {err}", idx + 1));
            Some((idx + 1, value))
        })
}

fn has_non_null(row: &Value, key: &str) -> bool {
    row.get(key).is_some_and(|value| !value.is_null())
}

fn decimal_i128(row: &Value, key: &str) -> Option<i128> {
    match row.get(key)? {
        Value::String(raw) => raw.parse().ok(),
        Value::Number(number) => number.as_i64().map(i128::from),
        _ => None,
    }
}

fn summarize_candidates(path: &Path, required_fields: &[&str]) -> CandidateSummary {
    let mut summary = CandidateSummary::default();
    for (line_no, row) in parse_jsonl(path) {
        summary.rows += 1;
        for field in required_fields {
            assert!(
                row.get(field).is_some(),
                "{} line {line_no} missing required field {field}",
                path.display()
            );
        }

        if has_non_null(&row, "submit_min_profit_wei") {
            summary.rows_with_submit_min_profit += 1;
        }
        if has_non_null(&row, "submit_calldata_bytes") {
            summary.rows_with_submit_calldata_bytes += 1;
        }

        let result = row
            .get("revm_result")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        *summary.result_counts.entry(result.to_string()).or_default() += 1;

        let net = decimal_i128(&row, "net_profit_wei");
        if net.is_some_and(|value| value > 0) {
            summary.positive_net_rows += 1;
        }

        let reject = row
            .get("reject_reason")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        if reject.starts_with("singleton_offline_nonauthoritative") {
            summary.singleton_offline_rejections += 1;
        }
        if result == "success" && reject == "none" {
            summary.accepted_success_rows += 1;
            if net.is_some_and(|value| value > 0) {
                summary.accepted_positive_success_rows += 1;
            }
        }
    }
    assert!(summary.rows > 0, "{} is empty", path.display());
    summary
}

fn collect_dirs(root: &Path, out: &mut BTreeSet<PathBuf>) {
    if !root.exists() {
        return;
    }
    let entries =
        fs::read_dir(root).unwrap_or_else(|err| panic!("read_dir {}: {err}", root.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("read_dir entry {}: {err}", root.display()));
        let path = entry.path();
        if path.is_dir() {
            out.insert(path.clone());
            collect_dirs(&path, out);
        }
    }
}

fn non_empty_file(path: &Path) -> bool {
    path.is_file()
        && fs::metadata(path)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
}

#[test]
fn monitor_candidates_are_field_complete_but_have_no_accepted_money_path() {
    let path = engine_root().join(MONITOR_CANDIDATES);
    let summary = summarize_candidates(&path, REPLAY_SUBMIT_FIELDS);

    assert!(
        summary.rows >= 18_000,
        "monitor candidates unexpectedly small: {} rows",
        summary.rows
    );
    for result in ["loss", "revert", "success"] {
        assert!(
            summary.result_counts.get(result).copied().unwrap_or(0) > 0,
            "monitor candidates have no {result} rows: {:?}",
            summary.result_counts
        );
    }
    assert!(
        summary.positive_net_rows > 0,
        "monitor candidates must preserve positive net rows for analysis"
    );
    assert!(
        summary.singleton_offline_rejections > 0,
        "monitor candidates should expose singleton offline rejection boundary"
    );
    assert_eq!(
        summary.accepted_success_rows, 0,
        "monitor candidates are not a release money-path artifact if any success is accepted"
    );
    assert_eq!(
        summary.accepted_positive_success_rows, 0,
        "monitor candidates currently prove no accepted positive money path"
    );
}

#[test]
fn gate9_a_file_has_positive_replay_rows_but_lacks_submit_economics() {
    let path = engine_root().join(GATE9_A_FILE);
    let summary = summarize_candidates(&path, GATE9_REPLAY_FIELDS);

    assert_eq!(
        summary.rows, 246,
        "Gate9 A-file row count drifted; reclassify this artifact before using it as evidence"
    );
    assert!(
        summary.accepted_positive_success_rows > 0,
        "Gate9 A-file should retain accepted positive replay rows"
    );
    assert_eq!(
        summary.rows_with_submit_min_profit, 0,
        "Gate9 A-file now has submit_min_profit_wei; update the blocker classification"
    );
    assert_eq!(
        summary.rows_with_submit_calldata_bytes, 0,
        "Gate9 A-file now has submit_calldata_bytes; update the blocker classification"
    );
}

#[test]
fn no_current_directory_is_a_complete_single_run_release_artifact() {
    let engine = engine_root();
    let mut dirs = BTreeSet::new();
    collect_dirs(&engine.join("logs"), &mut dirs);
    collect_dirs(&engine.join("certification_runs"), &mut dirs);

    let complete_dirs = dirs
        .into_iter()
        .filter(|dir| {
            RELEASE_RUN_FILES
                .iter()
                .all(|name| non_empty_file(&dir.join(name)))
        })
        .collect::<Vec<_>>();

    assert!(
        complete_dirs.is_empty(),
        "found complete release artifact directories; re-run aws_dryrun_artifact_contract on {:?}",
        complete_dirs
    );
}
