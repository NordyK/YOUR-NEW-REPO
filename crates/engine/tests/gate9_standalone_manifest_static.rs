use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const REQUIREMENTS: &str =
    "../../../Codex/GATE9_STANDALONE_REPLAY_MANIFEST_REQUIREMENTS_2026-06-14.json";
const HISTORICAL_ANVIL_MATRIX: &str =
    "../../../Codex/HISTORICAL_ANVIL_REPLAY_BLOCKER_MATRIX_2026-06-14.json";
const SAMPLE: &str = "../../contracts/test/fixtures/gate9_standalone_replay_manifest.sample.json";
const GATE9_B_CANDIDATES: &str =
    "../../../codex_tmp/gate9_b_binary_monitored_20260615T231853Z-205717/gate9/B_binary_candidates_revm.jsonl";
const STATE_SENSITIVE_LOSS_FIXTURE: &str =
    "../../contracts/test/fixtures/gate9_state_sensitive_loss_rows.json";
const GATE9_ANVIL_CORPUS: &str = "../../../anvil/corpus/blk_465686729.jsonl";
const GATE9_PARENT_BLOCK_CACHE: &str = "../../../anvil/block_cache/465686728";

fn read_json(path: &str) -> Value {
    let raw = fs::read_to_string(Path::new(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{path} is invalid JSON: {err}"))
}

fn current_gate9_fields() -> (usize, BTreeSet<String>, BTreeSet<String>) {
    let raw = fs::read_to_string(Path::new(GATE9_B_CANDIDATES))
        .unwrap_or_else(|err| panic!("failed to read Gate9 candidate JSONL: {err}"));
    let mut fields = BTreeSet::new();
    let mut null_fields = BTreeSet::new();
    let mut rows = 0usize;

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        rows += 1;
        let row: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("line {} invalid JSON: {err}", idx + 1));
        let object = row.as_object().expect("candidate row must be object");
        for (key, value) in object {
            fields.insert(key.clone());
            if value.is_null() {
                null_fields.insert(key.clone());
            }
        }
    }

    (rows, fields, null_fields)
}

fn current_gate9_null_counts() -> BTreeMap<String, usize> {
    let raw = fs::read_to_string(Path::new(GATE9_B_CANDIDATES))
        .unwrap_or_else(|err| panic!("failed to read Gate9 candidate JSONL: {err}"));
    let rows = raw
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|err| panic!("line {} invalid JSON: {err}", idx + 1))
        })
        .collect::<Vec<_>>();

    let mut fields = BTreeSet::<String>::new();
    for row in &rows {
        fields.extend(
            row.as_object()
                .expect("candidate row must be object")
                .keys()
                .cloned(),
        );
    }

    fields
        .into_iter()
        .map(|field| {
            let nulls = rows
                .iter()
                .filter(|row| row.get(&field).is_none_or(Value::is_null))
                .count();
            (field, nulls)
        })
        .collect()
}

fn string_set(value: &Value, field: &str) -> BTreeSet<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{field} entries must be strings"))
                .to_string()
        })
        .collect()
}

fn nested_string_set(value: &Value, first: &str, second: &str) -> BTreeSet<String> {
    value[first][second]
        .as_array()
        .unwrap_or_else(|| panic!("{first}.{second} must be array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{first}.{second} entries must be strings"))
                .to_string()
        })
        .collect()
}

fn gate9_candidate_rows() -> Vec<Value> {
    let raw = fs::read_to_string(Path::new(GATE9_B_CANDIDATES))
        .unwrap_or_else(|err| panic!("failed to read Gate9 candidate JSONL: {err}"));
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|err| panic!("line {} invalid JSON: {err}", idx + 1))
        })
        .collect()
}

fn nonempty_line_count(path: &str) -> usize {
    fs::read_to_string(Path::new(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

#[test]
fn gate9_standalone_replay_requirements_cover_current_missing_fields() {
    let requirements = read_json(REQUIREMENTS);
    let required = string_set(&requirements, "required_candidate_fields");
    let recorded_present = string_set(&requirements, "current_gate9_present_fields");
    let recorded_missing = string_set(&requirements, "current_gate9_missing_fields");
    let (row_count, current_present, current_null_fields) = current_gate9_fields();

    assert_eq!(row_count, 246, "Gate9 candidate count drifted");
    assert_eq!(
        requirements["current_gate9_candidate_rows"].as_u64(),
        Some(row_count as u64),
        "requirements artifact row count does not match current Gate9 JSONL"
    );
    assert_eq!(
        recorded_present, current_present,
        "requirements artifact current_present fields drifted"
    );

    let current_missing = required
        .difference(&current_present)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        recorded_missing, current_missing,
        "requirements artifact current missing fields drifted"
    );

    assert_eq!(
        current_missing,
        BTreeSet::from([
            "executor_calldata".to_string(),
            "mantis_calldata".to_string(),
            "transaction_index".to_string(),
        ]),
        "current monitored B rows should only miss standalone-manifest normalization fields"
    );

    for now_present in [
        "tx_hash",
        "exchange_ids",
        "exchange_metadata",
        "calldata",
        "changed_slots",
        "snapshot_block",
        "block_env",
        "block_timestamp",
        "gas_limit",
        "min_profit",
        "mantis_executor",
        "payee",
        "estimated_gas_wei",
        "net_profit_wei",
    ] {
        assert!(
            current_present.contains(now_present),
            "monitored Gate9 candidate rows should now contain {now_present}"
        );
    }

    assert!(
        current_null_fields.contains("net_profit_wei"),
        "net_profit_wei is present but still null on non-success rows"
    );
    assert!(
        current_null_fields.contains("balance_delta_wei"),
        "balance_delta_wei is present but still null on non-success rows"
    );
    assert!(
        !current_null_fields.contains("estimated_gas_wei"),
        "estimated_gas_wei should no longer be null in the monitored B artifact"
    );
}

#[test]
fn historical_anvil_replay_blocker_matrix_matches_gate9_and_local_corpus() {
    if !Path::new(GATE9_ANVIL_CORPUS).exists() || !Path::new(GATE9_PARENT_BLOCK_CACHE).exists() {
        eprintln!("SKIP: anvil corpus/block-cache artifact absent (out-of-branch)");
        return;
    }
    let matrix = read_json(HISTORICAL_ANVIL_MATRIX);
    let requirements = read_json(REQUIREMENTS);
    let rows = gate9_candidate_rows();

    assert_eq!(
        rows.len(),
        matrix["current_gate9"]["candidate_rows"]
            .as_u64()
            .expect("current_gate9.candidate_rows must be number") as usize,
        "historical Anvil matrix candidate row count drifted"
    );

    let mut candidate_fields = BTreeSet::<String>::new();
    let mut unique_blocks = BTreeSet::<u64>::new();
    let mut state_points = BTreeSet::<String>::new();
    let mut route_ids = BTreeSet::<u64>::new();
    for row in &rows {
        let object = row.as_object().expect("Gate9 candidate row must be object");
        candidate_fields.extend(object.keys().cloned());
        unique_blocks.insert(
            row["block"]
                .as_u64()
                .expect("Gate9 candidate block must be number"),
        );
        state_points.insert(
            row["state_point"]
                .as_str()
                .expect("Gate9 candidate state_point must be string")
                .to_string(),
        );
        route_ids.insert(
            row["route_id"]
                .as_u64()
                .expect("Gate9 candidate route_id must be number"),
        );
    }

    let matrix_blocks = matrix["current_gate9"]["unique_blocks"]
        .as_array()
        .expect("current_gate9.unique_blocks must be array")
        .iter()
        .map(|value| value.as_u64().expect("unique block must be number"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unique_blocks, matrix_blocks,
        "historical Anvil matrix unique blocks drifted"
    );
    assert_eq!(
        state_points,
        nested_string_set(&matrix, "current_gate9", "state_points"),
        "historical Anvil matrix state points drifted"
    );
    assert_eq!(
        candidate_fields,
        nested_string_set(&matrix, "current_gate9", "candidate_fields_present"),
        "historical Anvil matrix present fields drifted"
    );
    let null_counts = current_gate9_null_counts();
    for (field, value) in matrix["current_gate9"]["partially_null_fields"]
        .as_object()
        .expect("partially_null_fields must be object")
    {
        assert_eq!(
            null_counts.get(field).copied().unwrap_or_default(),
            value.as_u64().expect("partial null count must be u64") as usize,
            "partial null count drifted for {field}"
        );
    }
    for field in string_set(&matrix["current_gate9"], "null_for_all_rows") {
        assert_eq!(
            null_counts.get(&field).copied().unwrap_or_default(),
            rows.len(),
            "all-null field {field} no longer null for every row"
        );
    }
    assert_eq!(
        null_counts
            .get("estimated_gas_wei")
            .copied()
            .unwrap_or_default(),
        0,
        "estimated_gas_wei should be present on every monitored B row"
    );

    let corpus_rows = nonempty_line_count(GATE9_ANVIL_CORPUS);
    assert!(
        corpus_rows
            >= matrix["local_saved_material"]["saved_corpus_min_rows"]
                .as_u64()
                .expect("saved_corpus_min_rows must be number") as usize,
        "Gate9 saved Anvil corpus is too small: {corpus_rows}"
    );
    assert_eq!(
        corpus_rows,
        matrix["local_saved_material"]["saved_corpus_actual_rows"]
            .as_u64()
            .expect("saved_corpus_actual_rows must be number") as usize,
        "saved corpus actual row count drifted"
    );

    let parent_cache_bytes = fs::metadata(Path::new(GATE9_PARENT_BLOCK_CACHE))
        .unwrap_or_else(|err| panic!("failed to stat parent block cache: {err}"))
        .len();
    assert!(
        parent_cache_bytes
            >= matrix["local_saved_material"]["parent_block_cache_min_bytes"]
                .as_u64()
                .expect("parent_block_cache_min_bytes must be number"),
        "Gate9 parent block cache is too small: {parent_cache_bytes}"
    );
    // MINIMUM baseline, not an exact freeze. EVERY fork test forks from this parent block, so its
    // cache legitimately grows as more real state is touched. >= certified-actual is the meaningful
    // invariant (offline replay needs at least this much state); == yesterday's byte count is
    // perpetually red on any machine actually used to test and proves nothing. The min-bytes check
    // above already guarantees sufficiency.
    assert!(
        parent_cache_bytes
            >= matrix["local_saved_material"]["parent_block_cache_actual_bytes"]
                .as_u64()
                .expect("parent_block_cache_actual_bytes must be number"),
        "parent block cache below certified baseline: {parent_cache_bytes}"
    );

    let fixture = read_json(STATE_SENSITIVE_LOSS_FIXTURE);
    let fixture_rows = fixture["rows"]
        .as_array()
        .expect("state-sensitive loss rows must be array");
    assert_eq!(
        fixture_rows.len(),
        matrix["state_sensitive_loss_fixture"]["rows"]
            .as_u64()
            .expect("state_sensitive_loss_fixture.rows must be number") as usize,
        "state-sensitive loss fixture row count drifted"
    );

    let mut hop_split = BTreeMap::<String, usize>::new();
    for row in fixture_rows {
        let route_id = row["route_id"]
            .as_u64()
            .expect("state-sensitive row route_id must be number");
        assert!(
            route_ids.contains(&route_id),
            "state-sensitive route {route_id} missing from Gate9 candidate JSONL"
        );
        let hop_count = row["hop_count"]
            .as_u64()
            .expect("state-sensitive row hop_count must be number")
            .to_string();
        *hop_split.entry(hop_count).or_insert(0) += 1;
    }
    for (hop_count, count) in hop_split {
        assert_eq!(
            Some(count as u64),
            matrix["state_sensitive_loss_fixture"]["hop_split"][&hop_count].as_u64(),
            "state-sensitive hop split drifted for hop_count={hop_count}"
        );
    }

    let missing_fields = nested_string_set(&matrix, "current_blockers", "missing_candidate_fields");
    for field in &missing_fields {
        assert!(
            !candidate_fields.contains(field),
            "historical Anvil matrix says {field} is missing, but Gate9 candidate JSONL now contains it"
        );
    }

    let requirements_missing = string_set(&requirements, "current_gate9_missing_fields");
    for field in ["transaction_index", "mantis_calldata", "executor_calldata"] {
        assert!(
            requirements_missing.contains(field) && missing_fields.contains(field),
            "{field} must stay classified as missing until candidate artifacts are normalized into a standalone manifest"
        );
    }
    for field in [
        "tx_hash",
        "exchange_ids",
        "exchange_metadata",
        "calldata",
        "changed_slots",
        "snapshot_block",
        "block_env",
        "block_timestamp",
        "gas_limit",
        "min_profit",
        "mantis_executor",
        "payee",
    ] {
        assert!(
            !requirements_missing.contains(field) && !missing_fields.contains(field),
            "{field} is present in the monitored B artifact and should not remain in the missing-field blocker list"
        );
    }
}

#[test]
fn standalone_replay_sample_manifest_has_all_required_fields() {
    let requirements = read_json(REQUIREMENTS);
    let required = string_set(&requirements, "required_candidate_fields");
    let sample = read_json(SAMPLE);
    let candidates = sample["candidates"]
        .as_array()
        .expect("sample candidates must be array");
    assert!(
        !candidates.is_empty(),
        "sample standalone replay manifest must contain at least one row"
    );

    for (idx, candidate) in candidates.iter().enumerate() {
        let object = candidate
            .as_object()
            .unwrap_or_else(|| panic!("candidate {idx} must be object"));
        let fields = object.keys().cloned().collect::<BTreeSet<_>>();
        let missing = required
            .difference(&fields)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(
            missing.is_empty(),
            "sample candidate {idx} missing standalone fields: {missing:?}"
        );
    }
}
