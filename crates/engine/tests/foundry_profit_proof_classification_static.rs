use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MATRIX: &str = "../../../Codex/FOUNDRY_PROFIT_PROOF_CLASSIFICATION_2026-06-15.json";
const CONTRACTS_DIR: &str = "../../contracts/test";
const LOSS_CSV: &str = "../../contracts/test/fixtures/loss_classification.csv";

#[derive(Debug, Clone, PartialEq, Eq)]
struct LossRow {
    classification: String,
    profit_gate: bool,
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new("../../..").join(path)
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn read_json(path: impl AsRef<Path>) -> Value {
    let raw = read(path);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid JSON: {err}"))
}

fn obj<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing object key {key} in {value}"))
}

fn str_field(value: &Value, key: &str) -> String {
    obj(value, key)
        .as_str()
        .unwrap_or_else(|| panic!("missing string field {key} in {value}"))
        .to_string()
}

fn bool_field(value: &Value, key: &str) -> bool {
    obj(value, key)
        .as_bool()
        .unwrap_or_else(|| panic!("missing bool field {key} in {value}"))
}

fn u64_field(value: &Value, key: &str) -> u64 {
    obj(value, key)
        .as_u64()
        .unwrap_or_else(|| panic!("missing u64 field {key} in {value}"))
}

fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    match obj(value, key) {
        Value::Null => None,
        Value::String(raw) => Some(raw.clone()),
        other => panic!("field {key} must be string or null, got {other}"),
    }
}

fn opt_bool_field(value: &Value, key: &str) -> Option<bool> {
    match obj(value, key) {
        Value::Null => None,
        Value::Bool(raw) => Some(*raw),
        other => panic!("field {key} must be bool or null, got {other}"),
    }
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    obj(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("field {key} must be array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("non-string item in {key}: {item}"))
                .to_string()
        })
        .collect()
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|err| panic!("failed to run sha256sum for {}: {err}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha256sum stdout must be utf8")
        .split_whitespace()
        .next()
        .expect("sha256sum output missing hash")
        .to_string()
}

fn matrix_rows(matrix: &Value) -> Vec<&Value> {
    obj(matrix, "rows")
        .as_array()
        .expect("rows must be array")
        .iter()
        .collect()
}

fn row_map<'a>(matrix: &'a Value) -> BTreeMap<String, &'a Value> {
    let mut out = BTreeMap::new();
    for row in matrix_rows(matrix) {
        let file = str_field(row, "file");
        let old = out.insert(file.clone(), row);
        assert!(old.is_none(), "duplicate matrix row for {file}");
    }
    out
}

fn current_fork_tests() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(CONTRACTS_DIR).expect("read contracts test dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|raw| raw.to_str()) else {
            continue;
        };
        if name.ends_with(".Fork.t.sol") {
            out.insert(name.to_string());
        }
    }
    out
}

fn parse_loss_csv() -> BTreeMap<String, LossRow> {
    let csv = read(LOSS_CSV);
    let mut out = BTreeMap::new();
    for (idx, raw_line) in csv.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(line, "file,classification,profit_gate", "bad CSV header");
            continue;
        }
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields.len(), 3, "bad CSV row: {line}");
        let profit_gate = match fields[2] {
            "yes" => true,
            "no" => false,
            other => panic!("bad profit_gate {other} in row {line}"),
        };
        let old = out.insert(
            fields[0].to_string(),
            LossRow {
                classification: fields[1].to_string(),
                profit_gate,
            },
        );
        assert!(old.is_none(), "duplicate CSV row for {}", fields[0]);
    }
    out
}

fn inc(map: &mut BTreeMap<String, u64>, key: impl Into<String>) {
    *map.entry(key.into()).or_insert(0) += 1;
}

fn matrix_count_map(matrix: &Value, key: &str) -> BTreeMap<String, u64> {
    obj(obj(matrix, "summary"), key)
        .as_object()
        .unwrap_or_else(|| panic!("summary.{key} must be object"))
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_u64()
                    .unwrap_or_else(|| panic!("summary.{key} value must be u64")),
            )
        })
        .collect()
}

#[test]
fn matrix_covers_every_current_foundry_fork_test_and_hash_pins_sources() {
    let matrix = read_json(MATRIX);
    assert_eq!(
        matrix["schema"].as_str(),
        Some("foundry_profit_proof_classification.v1")
    );
    assert_eq!(
        matrix["classification"].as_str(),
        Some("current_foundry_fork_tests_do_not_all_prove_detector_money")
    );
    assert_eq!(
        matrix["scope"]["production_logic_modified"].as_bool(),
        Some(false)
    );

    let rows = row_map(&matrix);
    let fork_tests = current_fork_tests();
    let classified = rows.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        classified, fork_tests,
        "classification matrix must exactly cover current *.Fork.t.sol files"
    );
    assert_eq!(
        rows.len() as u64,
        u64_field(obj(&matrix, "summary"), "fork_tests_total")
    );

    for (file, row) in rows {
        let path = workspace_path(&str_field(row, "path"));
        assert!(
            path.ends_with(Path::new("engine/contracts/test").join(&file)),
            "{file} path does not point at matching fork test: {}",
            path.display()
        );
        assert!(
            path.is_file(),
            "classified fork test does not exist: {}",
            path.display()
        );
        assert_eq!(
            sha256_file(&path),
            str_field(row, "sha256"),
            "{file} source hash drifted"
        );
        assert!(
            !string_array(row, "required_snippets").is_empty(),
            "{file} must include source evidence snippets"
        );
    }

    let source = &matrix["sources"]["loss_classification_csv"];
    let source_path = workspace_path(&str_field(source, "path"));
    assert_eq!(
        fs::metadata(&source_path)
            .unwrap_or_else(|err| panic!("metadata for {}: {err}", source_path.display()))
            .len(),
        u64_field(source, "bytes"),
        "loss classification CSV byte size drifted"
    );
    assert_eq!(
        sha256_file(&source_path),
        str_field(source, "sha256"),
        "loss classification CSV hash drifted"
    );
}

#[test]
fn classification_counts_and_release_credit_rules_hold() {
    let matrix = read_json(MATRIX);
    let rows = matrix_rows(&matrix);
    let allowed_classes = BTreeSet::from([
        "diagnostic_only",
        "direct_realized_profit_proof",
        "gas_accounting_mechanical",
        "historical_tx_profit_existence",
        "historical_tx_reproduction_no_pnl",
        "infrastructure_or_security",
        "mechanical_only",
        "predictor_kernel_parity",
        "slippage_shape_gate",
        "strict_detector_profit_gate",
    ]);
    let expected_counts = BTreeMap::from([
        ("diagnostic_only".to_string(), 8),
        ("direct_realized_profit_proof".to_string(), 6),
        ("gas_accounting_mechanical".to_string(), 1),
        ("historical_tx_profit_existence".to_string(), 1),
        ("historical_tx_reproduction_no_pnl".to_string(), 1),
        ("infrastructure_or_security".to_string(), 3),
        ("mechanical_only".to_string(), 9),
        ("predictor_kernel_parity".to_string(), 1),
        ("slippage_shape_gate".to_string(), 1),
        ("strict_detector_profit_gate".to_string(), 3),
    ]);

    let mut observed_counts = BTreeMap::new();
    let mut detector_money_files = BTreeSet::new();
    let mut limited_profit_examples = 0u64;
    for row in rows {
        let file = str_field(row, "file");
        let class = str_field(row, "class");
        assert!(
            allowed_classes.contains(class.as_str()),
            "{file} has unsupported class {class}"
        );
        inc(&mut observed_counts, class.clone());

        if bool_field(row, "counts_as_release_detector_money_proof") {
            detector_money_files.insert(file.clone());
        }
        if bool_field(row, "counts_as_limited_realized_profit_example") {
            limited_profit_examples += 1;
        }

        let loss_role = str_field(row, "loss_acceptance_role");
        if class == "mechanical_only"
            || class == "diagnostic_only"
            || class == "gas_accounting_mechanical"
            || class == "infrastructure_or_security"
            || class == "predictor_kernel_parity"
            || class == "historical_tx_reproduction_no_pnl"
        {
            assert!(
                !bool_field(row, "counts_as_release_detector_money_proof"),
                "{file} is {class} but is counted as release detector money proof"
            );
        }
        if loss_role == "loss_as_mechanical_success"
            || loss_role == "loss_as_diagnostic_classification"
            || loss_role == "infra_reverts_allowed_not_money"
        {
            assert!(
                !bool_field(row, "counts_as_release_detector_money_proof"),
                "{file} lets Loss/reverts satisfy a non-money check but is counted as detector money"
            );
        }
    }

    assert_eq!(observed_counts, expected_counts, "unexpected class counts");
    assert_eq!(
        matrix_count_map(&matrix, "counts_by_class"),
        expected_counts,
        "summary class counts are stale"
    );
    assert_eq!(
        detector_money_files,
        BTreeSet::from([
            "MantisDetectExecuteProfit.Fork.t.sol".to_string(),
            "MantisDetectorVerdict.Fork.t.sol".to_string(),
            "MantisGate9CalldataReplay.Fork.t.sol".to_string(),
            "MantisGate9CleanSuccessCorpusReplay.Fork.t.sol".to_string()
        ]),
        "only strict or detect/execute money gates may count as detector-money proof"
    );
    assert_eq!(
        detector_money_files.len() as u64,
        u64_field(
            obj(&matrix, "summary"),
            "counts_as_release_detector_money_proof"
        )
    );
    assert_eq!(
        limited_profit_examples,
        u64_field(
            obj(&matrix, "summary"),
            "counts_as_limited_realized_profit_example"
        )
    );
}

#[test]
fn loss_fixture_alignment_prevents_loss_pass_from_being_counted_as_money() {
    let matrix = read_json(MATRIX);
    let rows = row_map(&matrix);
    let loss_rows = parse_loss_csv();
    assert_eq!(
        loss_rows.len() as u64,
        u64_field(obj(&matrix, "summary"), "loss_fixture_rows")
    );

    for (file, loss_row) in &loss_rows {
        let row = rows
            .get(file)
            .unwrap_or_else(|| panic!("CSV row {file} has no matrix row"));
        assert_eq!(
            opt_str_field(row, "loss_fixture_classification"),
            Some(loss_row.classification.clone()),
            "{file} loss fixture classification drifted"
        );
        assert_eq!(
            opt_bool_field(row, "loss_fixture_profit_gate"),
            Some(loss_row.profit_gate),
            "{file} loss fixture profit_gate drifted"
        );

        if loss_row.classification == "mechanical_only"
            || loss_row.classification == "diagnostic_loss"
        {
            assert!(
                !bool_field(row, "counts_as_release_detector_money_proof"),
                "{file} is {} in loss fixture but counted as detector money",
                loss_row.classification
            );
        }
    }

    for (file, row) in rows {
        if !loss_rows.contains_key(&file) {
            assert!(
                opt_str_field(row, "loss_fixture_classification").is_none(),
                "{file} is absent from loss fixture but has a matrix classification"
            );
            assert!(
                opt_bool_field(row, "loss_fixture_profit_gate").is_none(),
                "{file} is absent from loss fixture but has matrix profit_gate"
            );
        }
    }
}

#[test]
fn required_source_snippets_are_present() {
    let matrix = read_json(MATRIX);
    for row in matrix_rows(&matrix) {
        let file = str_field(row, "file");
        let path = workspace_path(&str_field(row, "path"));
        let source = read(&path);
        for snippet in string_array(row, "required_snippets") {
            assert!(
                source.contains(&snippet),
                "{file} is missing required evidence snippet {snippet:?}"
            );
        }
    }
}

#[test]
fn release_detector_money_rows_cannot_credit_loss_or_revert_as_success() {
    let matrix = read_json(MATRIX);
    let allowed_policies = BTreeSet::from([
        "size_sweep_may_catch_loss_but_final_execution_requires_positive_realized_weth",
        "loss_counted_negative_and_bounded_not_success",
        "strict_replay_revert_fails_test",
        "strict_all_rows_revert_fails_test_diagnostics_not_release_green",
    ]);

    for row in matrix_rows(&matrix) {
        if !bool_field(row, "counts_as_release_detector_money_proof") {
            continue;
        }

        let file = str_field(row, "file");
        let source = read(workspace_path(&str_field(row, "path")));
        assert!(
            !bool_field(row, "loss_can_satisfy_release_gate"),
            "{file} is release money proof but allows Loss to satisfy the release gate"
        );

        let policy = str_field(row, "release_revert_policy");
        assert!(
            allowed_policies.contains(policy.as_str()),
            "{file} has unsupported release revert policy {policy}"
        );

        let final_assertions = string_array(row, "final_positive_assertion_snippets");
        assert!(
            !final_assertions.is_empty(),
            "{file} must pin final positive-money assertions"
        );
        for snippet in &final_assertions {
            assert!(
                source.contains(snippet),
                "{file} final positive assertion drifted or is missing: {snippet:?}"
            );
        }

        let has_catch_or_expect_revert = source.contains("catch {")
            || source.contains("catch (bytes memory")
            || source.contains("vm.expectRevert()");
        if has_catch_or_expect_revert {
            assert!(
                policy.contains("strict")
                    || policy.contains("loss_counted_negative")
                    || policy.contains("size_sweep"),
                "{file} has catch/expectRevert surface without an explicit release-safe policy"
            );
        }
    }
}

#[test]
fn release_detector_money_revert_policies_match_current_source_behavior() {
    let matrix = read_json(MATRIX);
    let rows = row_map(&matrix);

    let detect = rows
        .get("MantisDetectExecuteProfit.Fork.t.sol")
        .expect("detect execute profit row");
    assert_eq!(
        str_field(detect, "release_revert_policy"),
        "size_sweep_may_catch_loss_but_final_execution_requires_positive_realized_weth"
    );
    let detect_src = read(workspace_path(&str_field(detect, "path")));
    for snippet in [
        "} catch {",
        "Loss / not profitable at this size",
        "assertGt(bestProfit, 0",
        "assertGt(realized, 0",
    ] {
        assert!(
            detect_src.contains(snippet),
            "MantisDetectExecuteProfit policy evidence missing {snippet:?}"
        );
    }

    let detector = rows
        .get("MantisDetectorVerdict.Fork.t.sol")
        .expect("detector verdict row");
    assert_eq!(
        str_field(detector, "release_revert_policy"),
        "loss_counted_negative_and_bounded_not_success"
    );
    let detector_src = read(workspace_path(&str_field(detector, "path")));
    for snippet in [
        "outcome = 1;",
        "net = -int256(sf);",
        "assertLe(nLoss, maxLoss",
        "assertTrue(netSum >= int256(minNetSumWei)",
    ] {
        assert!(
            detector_src.contains(snippet),
            "MantisDetectorVerdict policy evidence missing {snippet:?}"
        );
    }

    let gate9 = rows
        .get("MantisGate9CalldataReplay.Fork.t.sol")
        .expect("Gate9 calldata replay row");
    assert_eq!(
        str_field(gate9, "release_revert_policy"),
        "strict_replay_revert_fails_test"
    );
    let gate9_src = read(workspace_path(&str_field(gate9, "path")));
    for snippet in [
        "Loss/InsufficientProfit is not accepted here.",
        "assertTrue(false, string.concat(id, \": Gate9 bot calldata reverted instead of realizing profit\"))",
        "assertGt(realizedDelta, 0",
        "assertGe(realizedDelta, expectedDelta",
    ] {
        assert!(
            gate9_src.contains(snippet),
            "MantisGate9CalldataReplay policy evidence missing {snippet:?}"
        );
    }

    let clean = rows
        .get("MantisGate9CleanSuccessCorpusReplay.Fork.t.sol")
        .expect("Gate9 clean corpus row");
    assert_eq!(
        str_field(clean, "release_revert_policy"),
        "strict_all_rows_revert_fails_test_diagnostics_not_release_green"
    );
    let clean_src = read(workspace_path(&str_field(clean, "path")));
    for snippet in [
        "assertTrue(false, string.concat(id, \": exact clean-success calldata reverted\"))",
        "assertGt(realizedDelta, 0",
        "assertEq(realizedDelta, expectedDelta",
        "LoadedState_ClassifyRangeFromEnv_Diagnostic",
    ] {
        assert!(
            clean_src.contains(snippet),
            "MantisGate9CleanSuccessCorpusReplay policy evidence missing {snippet:?}"
        );
    }
}

#[test]
fn current_verdict_stays_red_until_strict_detector_artifacts_are_green() {
    let matrix = read_json(MATRIX);
    assert_eq!(
        matrix["summary"]["current_release_conclusion"].as_str(),
        Some("red: broader strict detector corpus is now proven red on the 27-row clean-success exact-calldata gate; selected Gate9 2/3/4-hop replay is green, but V4/Curve rows disagree with REVM and mechanical tests must not be counted as money proof")
    );
    let rows = row_map(&matrix);
    let detector = rows
        .get("MantisDetectorVerdict.Fork.t.sol")
        .expect("detector verdict row");
    assert_eq!(
        str_field(detector, "current_status"),
        "red_current_artifact_has_losses_and_metadata_limited_rows"
    );

    let mechanical_or_diagnostic = matrix_rows(&matrix)
        .into_iter()
        .filter(|row| {
            matches!(
                str_field(row, "class").as_str(),
                "mechanical_only" | "diagnostic_only" | "gas_accounting_mechanical"
            )
        })
        .count() as u64;
    assert_eq!(
        mechanical_or_diagnostic,
        u64_field(
            obj(&matrix, "summary"),
            "mechanical_or_diagnostic_not_detector_money"
        )
    );
    assert!(
        mechanical_or_diagnostic > 0,
        "the matrix must keep mechanical/diagnostic tests visible as non-money proof"
    );
}
