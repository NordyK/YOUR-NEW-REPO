//! Pins the laptop-only certification boundary.
//!
//! This suite intentionally scans only active runnable surfaces: Rust crates,
//! engine scripts/deploy helpers, and current gate JSON artifacts. Historical
//! reports/docs are not scanned because they are evidence records, not live gates.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const SELF: &str = "laptop_only_certification_boundary_static.rs";

const CURRENT_GATE_ARTIFACTS: &[&str] = &[
    "Codex/REMAINING_GATES_DETAILED_EXECUTION_PLAN_2026-06-20.json",
    "Codex/REMAINING_GATES_EDGE_CASE_CLOSURE_MATRIX_2026-06-20.json",
    "Codex/LIVE_CAPITAL_GO_NO_GO_2026-06-19.json",
];

const REMOVED_OR_LIVE_ONLY_REFERENCES: &[&str] = &[
    "--ignored",
    "#[ignore",
    "```ignore",
    "real_aws_dryrun_artifact_dir_satisfies_release_contract",
    "release_money_path_is_fully_certified",
    "pipeline::live_parity_gates",
    "l1_gas_nodeinterface_matrix_live",
    "gate9_equivalence_report_contract",
    "live_feed_event_age_probe",
    "live_feed_dissect",
    "live_arb_feed_receives_frames",
    "v3_mint_burn_live_parity",
    "curve_stableswap_live_parity",
    "v4_modify_liquidity_live_parity",
    "trader_joe_lb_live_parity",
    "curve_crypto_live_parity",
    "balancer_weighted_live_parity",
    "fluid_v1_live_parity",
    "deploy_readiness_static",
    "standalone_replay_metadata_gap_static",
    "remaining_gaps_deep_rerun_static",
    "current_blocker_fix_map_static",
    "current_remaining_gaps_certification_plan_json_static",
    "current_remaining_gaps_certification_report_static",
    "current_remaining_gaps_plan_static",
    "deep_remaining_gaps_certification_plan_static",
    "local_live_gate_attempt_static",
    "money_path_master_plan_static",
    "remaining_gaps_certification_plan_static",
    "revm_live_equivalence_boundary_static",
];

/// Frozen allowlist of `(repo-relative file, needle)` pairs that legitimately contain an ignore
/// marker on a NON-gate surface: `#[ignore]`d manual micro-benchmarks (perf gates that must never
/// run in the default `cargo test`) and RPC-gated tests/doc examples the LEADER runs with
/// `--ignored`. These are not removed live/AWS-only GATES — the guard below still catches any NEW
/// file that introduces an ignore marker, and still catches every live-only-gate needle (e.g. the
/// `*_live_parity` references) in ANY file, including these. Each entry must match an occurrence
/// that is actually present; an entry that stops matching is itself a drift the guard surfaces.
const ALLOWED_IGNORE_MARKERS: &[(&str, &str)] = &[
    ("engine/crates/engine-amm/src/curve.rs", "#[ignore"),
    ("engine/crates/engine-amm/src/curve.rs", "--ignored"),
    ("engine/crates/engine-amm/src/solidly_stable.rs", "#[ignore"),
    (
        "engine/crates/engine-amm/src/solidly_stable.rs",
        "--ignored",
    ),
    ("engine/crates/engine-amm/src/trader_joe_lb.rs", "#[ignore"),
    ("engine/crates/engine-amm/src/trader_joe_lb.rs", "--ignored"),
    ("engine/crates/engine-routes/src/compiled.rs", "#[ignore"),
    ("engine/crates/engine-routes/src/compiled.rs", "--ignored"),
    ("engine/crates/engine/src/latency_budgets.rs", "#[ignore"),
    ("engine/crates/engine/src/latency_budgets.rs", "--ignored"),
    ("engine/crates/engine/src/state_mirror.rs", "#[ignore"),
    ("engine/crates/engine/src/state_mirror.rs", "--ignored"),
    (
        "engine/crates/engine/tests/corpus_catalog_detection_static.rs",
        "#[ignore",
    ),
    (
        "engine/crates/engine/tests/cull_safety_upper_bound_static.rs",
        "```ignore",
    ),
    (
        "engine/crates/engine/tests/onchain_quote_parity.rs",
        "#[ignore",
    ),
    (
        "engine/crates/engine/tests/onchain_quote_parity.rs",
        "--ignored",
    ),
    // Docs-lint harnesses: these tests deliberately read repository Markdown (a NON-correctness
    // lint) and are therefore #[ignore]'d out of the correctness certification surface — exactly
    // what this guard is meant to permit via the allowlist. The campaign's own rule bans Markdown
    // as correctness evidence, so excluding them from the cert run is correct, not a hidden failure.
    (
        "engine/crates/engine-feed/tests/engine_build_blocker_static.rs",
        "#[ignore",
    ),
    (
        "engine/crates/engine/tests/active_mantis_executor_boundary_static.rs",
        "#[ignore",
    ),
    // certification_artifacts_static.rs contains the literal string "#[ignore = \"docs-lint" inside
    // an assertion that VERIFIES the above markers exist — a meta-check, not an ignored test.
    (
        "engine/crates/engine/tests/certification_artifacts_static.rs",
        "#[ignore",
    ),
];

/// True iff `path` (an absolute repo path) hitting `needle` is on the frozen ignore-marker
/// allowlist. Only the three ignore-marker needles can ever be allowlisted; every other needle
/// (the removed live/AWS-only gate references) is denied on every file, no exceptions.
fn is_allowed_ignore_marker(path: &Path, needle: &str) -> bool {
    if !matches!(needle, "--ignored" | "#[ignore" | "```ignore") {
        return false;
    }
    ALLOWED_IGNORE_MARKERS
        .iter()
        .any(|(rel, allowed_needle)| *allowed_needle == needle && path.ends_with(rel))
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("engine/crates/engine has repo root ancestor")
        .to_path_buf()
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    let entries = fs::read_dir(root).unwrap_or_else(|e| panic!("read_dir {}: {e}", root.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry {}: {e}", root.display()));
        let path = entry.path();
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if file_name == "target" || file_name == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn active_text_files() -> Vec<PathBuf> {
    let repo = repo_root();
    let engine = repo.join("engine");
    let mut files = Vec::new();
    collect_files(&engine.join("crates"), &mut files);
    collect_files(&engine.join("scripts"), &mut files);
    collect_files(&engine.join("deploy"), &mut files);
    for artifact in CURRENT_GATE_ARTIFACTS {
        files.push(repo.join(artifact));
    }
    files
        .into_iter()
        .filter(|p| p.file_name().and_then(|s| s.to_str()) != Some(SELF))
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("rs" | "sh" | "json" | "toml")
            )
        })
        .collect()
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(raw) => out.push(raw.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

fn cargo_test_name(command: &str) -> Option<&str> {
    let mut parts = command.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--test" {
            return parts.next();
        }
    }
    None
}

fn active_test_exists(repo: &Path, test_name: &str) -> bool {
    let expected = format!("{test_name}.rs");
    let crates = repo.join("engine/crates");
    let mut files = Vec::new();
    collect_files(&crates, &mut files);
    files.iter().any(|path| {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            == Some("tests")
            && path.file_name().and_then(|s| s.to_str()) == Some(expected.as_str())
    })
}

fn collect_local_verification_commands<'a>(
    value: &'a Value,
    artifact: &str,
    out: &mut Vec<(String, &'a str)>,
) {
    match value {
        Value::Object(map) => {
            if let Some(commands) = map
                .get("local_verification_commands")
                .and_then(Value::as_array)
            {
                for command in commands {
                    let command = command.as_str().unwrap_or_else(|| {
                        panic!("{artifact} local_verification_commands item is not a string")
                    });
                    out.push((artifact.to_string(), command));
                }
            }
            for item in map.values() {
                collect_local_verification_commands(item, artifact, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_local_verification_commands(item, artifact, out);
            }
        }
        _ => {}
    }
}

fn collect_named_command_arrays<'a>(
    value: &'a Value,
    artifact: &str,
    path: &mut Vec<String>,
    out: &mut Vec<(String, String, &'a str)>,
) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                path.push(key.clone());
                if matches!(
                    key.as_str(),
                    "commands"
                        | "local_verification_commands"
                        | "external_verification_commands"
                        | "aws_verification_commands"
                ) {
                    let commands = item
                        .as_array()
                        .unwrap_or_else(|| panic!("{artifact} {} is not an array", path.join(".")));
                    for command in commands {
                        let command = command.as_str().unwrap_or_else(|| {
                            panic!("{artifact} {} item is not a string", path.join("."))
                        });
                        out.push((artifact.to_string(), path.join("."), command));
                    }
                }
                collect_named_command_arrays(item, artifact, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                collect_named_command_arrays(item, artifact, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn active_surfaces_do_not_reintroduce_ignored_or_live_only_gates() {
    let mut hits = Vec::new();
    for path in active_text_files() {
        let text = read(&path);
        for needle in REMOVED_OR_LIVE_ONLY_REFERENCES {
            if text.contains(needle) && !is_allowed_ignore_marker(&path, needle) {
                hits.push(format!("{} contains {needle}", path.display()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "active laptop certification surfaces must not reference removed live/AWS-only gates:\n{}",
        hits.join("\n")
    );
}

#[test]
fn removed_live_only_test_files_stay_removed() {
    let repo = repo_root();
    for rel in [
        "engine/crates/engine-feed/tests/live_feed_event_age_probe.rs",
        "engine/crates/engine-feed/tests/live_feed.rs",
        "engine/crates/engine-revm/tests/gate9_equivalence_report_contract.rs",
        "engine/crates/engine-rpc/tests/live_arbitrum.rs",
        "engine/crates/engine/tests/l1_gas_nodeinterface_matrix_live.rs",
        "engine/crates/engine/tests/live_feed_dissect.rs",
        "engine/crates/engine/tests/v3_mint_burn_live_parity.rs",
        "engine/crates/engine/tests/v4_modify_liquidity_live_parity.rs",
        "engine/crates/engine/tests/trader_joe_lb_live_parity.rs",
        "engine/crates/engine/tests/curve_crypto_live_parity.rs",
        "engine/crates/engine/tests/balancer_weighted_live_parity.rs",
        "engine/crates/engine/tests/fluid_v1_live_parity.rs",
        "engine/crates/engine/tests/deploy_readiness_static.rs",
    ] {
        assert!(
            !repo.join(rel).exists(),
            "{rel} is AWS/live-only and must not return as a laptop gate"
        );
    }
}

#[test]
fn current_json_cargo_test_commands_reference_existing_active_tests() {
    let repo = repo_root();
    let mut failures = Vec::new();
    for artifact in CURRENT_GATE_ARTIFACTS {
        let path = repo.join(artifact);
        let mut strings = Vec::new();
        collect_strings(&json(&path), &mut strings);
        for command in strings {
            if !command.contains("cargo test") {
                continue;
            }
            if let Some(test_name) = cargo_test_name(&command) {
                if !active_test_exists(&repo, test_name) {
                    failures.push(format!(
                        "{artifact}: missing --test {test_name} in `{command}`"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "current gate artifacts must not point local cargo commands at deleted tests:\n{}",
        failures.join("\n")
    );
}

#[test]
fn local_verification_commands_are_laptop_only() {
    let repo = repo_root();
    let forbidden = [
        "forge ",
        "cast ",
        "source ",
        "--fork-url",
        "ARBITRUM_RPC_URL",
        "AWS_DRYRUN",
        "scripts/assemble_aws_dryrun_artifact_dir.sh",
        "http://",
        "https://",
        "curl ",
    ];
    let mut failures = Vec::new();
    for artifact in CURRENT_GATE_ARTIFACTS {
        let path = repo.join(artifact);
        let parsed = json(&path);
        let mut commands = Vec::new();
        collect_local_verification_commands(&parsed, artifact, &mut commands);
        for (artifact, command) in commands {
            if !command.starts_with("cargo test ") {
                failures.push(format!(
                    "{artifact}: local verification command is not a cargo test: `{command}`"
                ));
            }
            for needle in forbidden {
                if command.contains(needle) {
                    failures.push(format!(
                        "{artifact}: local verification command contains external marker {needle}: `{command}`"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "local_verification_commands must stay laptop-only:\n{}",
        failures.join("\n")
    );
}

#[test]
fn command_arrays_do_not_hide_executable_live_or_aws_steps_as_laptop_work() {
    let repo = repo_root();
    let executable_external_markers = [
        "forge ",
        "cast ",
        "source ",
        "--fork-url",
        "ARBITRUM_RPC_URL",
        "WARMUP_TIMEOUT_MS",
        "AWS_DRYRUN_SOURCE_DIR=",
        "AWS_DRYRUN_ARTIFACT_DIR=",
        "scripts/assemble_aws_dryrun_artifact_dir.sh",
        "http://",
        "https://",
        "curl ",
    ];
    let mut failures = Vec::new();
    for artifact in CURRENT_GATE_ARTIFACTS {
        let path = repo.join(artifact);
        let parsed = json(&path);
        let mut commands = Vec::new();
        collect_named_command_arrays(&parsed, artifact, &mut Vec::new(), &mut commands);
        for (artifact, json_path, command) in commands {
            let is_external_marker = command.starts_with("external-only ");
            if is_external_marker {
                if !command.contains("no laptop command") {
                    failures.push(format!(
                        "{artifact} {json_path}: external marker must explain there is no laptop command: `{command}`"
                    ));
                }
                if json_path.contains("local_verification_commands") {
                    failures.push(format!(
                        "{artifact} {json_path}: local verification must not contain external marker: `{command}`"
                    ));
                }
                continue;
            }
            for needle in executable_external_markers {
                if command.contains(needle) {
                    failures.push(format!(
                        "{artifact} {json_path}: executable external marker {needle} is not laptop-provable: `{command}`"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "command arrays must not hide AWS/live/RPC commands in laptop certification surfaces:\n{}",
        failures.join("\n")
    );
}
