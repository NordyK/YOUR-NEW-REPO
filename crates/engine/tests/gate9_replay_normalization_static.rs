use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const MONITORED_B: &str =
    "../../../codex_tmp/gate9_b_binary_monitored_20260615T231853Z-205717/gate9/B_binary_candidates_revm.jsonl";
const CLEAN_SUCCESS_FIXTURE: &str =
    "../../contracts/test/fixtures/gate9_clean_success_corpus_replay.json";
const REPORT: &str = "../../../Codex/REMAINING_GAPS_DEEP_TEST_RERUN_2026-06-16.json";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn read_json(path: &str) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|err| panic!("{path} invalid JSON: {err}"))
}

fn candidate_rows() -> Vec<Value> {
    read(MONITORED_B)
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|err| panic!("candidate line {} invalid JSON: {err}", idx + 1))
        })
        .collect()
}

fn string_array<'a>(value: &'a Value, key: &str) -> Vec<&'a str> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be array"))
        .iter()
        .map(|item| item.as_str().unwrap_or_else(|| panic!("{key} item string")))
        .collect()
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} must be u64 in {value}"))
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} must be string in {value}"))
}

fn positive_string_wei(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse::<u128>().ok())
        .is_some_and(|parsed| parsed > 0)
}

fn hex_field(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|raw| raw.starts_with("0x") && raw.len() > 10)
}

fn tx_hash_field(value: &Value) -> bool {
    value
        .get("tx_hash")
        .and_then(Value::as_str)
        .is_some_and(|raw| raw.starts_with("0x") && raw.len() == 66)
}

fn string_set(value: &Value, key: &str) -> BTreeSet<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} must be array"))
        .iter()
        .map(|item| item.as_str().expect("array item string").to_string())
        .collect()
}

#[test]
fn monitored_b_has_exact_executor_replay_anchors_except_manifest_aliases() {
    let rows = candidate_rows();
    assert_eq!(rows.len(), 246);

    let fields = rows
        .iter()
        .flat_map(|row| row.as_object().expect("candidate object").keys().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(fields.len(), 34);

    let missing = ["transaction_index", "mantis_calldata", "executor_calldata"]
        .into_iter()
        .filter(|field| !fields.contains(*field))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        missing,
        ["executor_calldata", "mantis_calldata", "transaction_index"]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );

    for required in [
        "tx_hash",
        "calldata",
        "changed_slots",
        "exchange_ids",
        "exchange_metadata",
        "payee",
        "mantis_executor",
        "snapshot_block",
        "block_env",
        "block_timestamp",
        "gas_limit",
        "min_profit",
        "estimated_gas_wei",
        "total_gas_wei",
        "net_profit_wei",
        "balance_delta_wei",
    ] {
        assert!(
            fields.contains(required),
            "monitored-B artifact missing core replay/economics field {required}"
        );
    }

    let accepted = rows
        .iter()
        .filter(|row| {
            row["revm_result"].as_str() == Some("success")
                && row["reject_reason"].as_str() == Some("none")
        })
        .collect::<Vec<_>>();
    assert_eq!(accepted.len(), 27);
    for row in accepted {
        assert!(
            tx_hash_field(row),
            "{} missing tx_hash",
            row["candidate_id"]
        );
        assert!(
            hex_field(row, "calldata"),
            "{} missing calldata",
            row["candidate_id"]
        );
        assert!(
            positive_string_wei(row, "net_profit_wei"),
            "{} missing positive net_profit_wei",
            row["candidate_id"]
        );
        assert!(
            positive_string_wei(row, "balance_delta_wei"),
            "{} missing positive balance_delta_wei",
            row["candidate_id"]
        );
    }
}

#[test]
fn clean_success_fixture_calldata_is_source_matched_to_monitored_b() {
    let rows = candidate_rows()
        .into_iter()
        .map(|row| (str_field(&row, "candidate_id").to_string(), row))
        .collect::<BTreeMap<_, _>>();
    let fixture = read_json(CLEAN_SUCCESS_FIXTURE);
    assert_eq!(
        str_field(&fixture, "source_candidate_file"),
        MONITORED_B.trim_start_matches("../../../")
    );
    // 22 = the byte-exact-reproducible clean-success rows. The 5 revm=success rows that do NOT
    // reproduce on the trigger-positioned fork (route268 = DODO maintainer-fee, fixed; 3408/6252/
    // 6674/15603 = V4 PoolManager singleton-state reconstruction, AWS-gated) are excluded here and
    // isolated/diagnosed in V4CurveStateDivergenceAll.Fork.t.sol (all capital-safe, zero kernel bugs).
    assert_eq!(u64_field(&fixture, "row_count"), 22);

    let ids = string_array(&fixture, "candidate_ids");
    let tx_hashes = string_array(&fixture, "tx_hashes");
    let calldatas = string_array(&fixture, "calldatas");
    let expected_delta = string_array(&fixture, "expected_balance_delta_wei");
    let expected_net = string_array(&fixture, "expected_net_profit_wei");
    let selectors = string_array(&fixture, "selectors");
    assert_eq!(ids.len(), 22);
    assert_eq!(tx_hashes.len(), ids.len());
    assert_eq!(calldatas.len(), ids.len());
    assert_eq!(expected_delta.len(), ids.len());
    assert_eq!(expected_net.len(), ids.len());
    assert_eq!(selectors.len(), ids.len());

    for i in 0..ids.len() {
        let row = rows
            .get(ids[i])
            .unwrap_or_else(|| panic!("fixture id {} missing from monitored-B", ids[i]));
        assert_eq!(row["revm_result"].as_str(), Some("success"));
        assert_eq!(row["reject_reason"].as_str(), Some("none"));
        assert_eq!(row["tx_hash"].as_str(), Some(tx_hashes[i]));
        assert_eq!(row["calldata"].as_str(), Some(calldatas[i]));
        assert_eq!(row["balance_delta_wei"].as_str(), Some(expected_delta[i]));
        assert_eq!(row["net_profit_wei"].as_str(), Some(expected_net[i]));
        assert!(
            calldatas[i].starts_with(selectors[i]),
            "{} calldata does not start with expected selector",
            ids[i]
        );
    }
}

#[test]
fn report_classifies_remaining_fields_as_replay_normalization_not_kernel_bug() {
    let report = read_json(REPORT);
    let section = &report["fresh_gate9_replay_normalization_current_20260618"];
    assert_eq!(
        str_field(section, "status"),
        "current_monitored_b_replay_anchors_green_manifest_aliases_and_tx_index_missing"
    );
    assert_eq!(u64_field(section, "candidate_rows"), 246);
    assert_eq!(u64_field(section, "field_count"), 34);
    assert_eq!(u64_field(section, "accepted_success_rows"), 27);
    assert!(section["all_rows_have_tx_hash"].as_bool().unwrap_or(false));
    assert!(section["all_rows_have_exact_calldata"]
        .as_bool()
        .unwrap_or(false));
    assert!(section["accepted_rows_have_positive_net_and_delta"]
        .as_bool()
        .unwrap_or(false));
    assert_eq!(
        string_set(section, "remaining_missing_fields"),
        ["executor_calldata", "mantis_calldata", "transaction_index"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert!(
        str_field(section, "release_implication")
            .contains("not an AMM kernel or executor-call correctness failure"),
        "report must classify this as replay normalization, not kernel logic"
    );
}
