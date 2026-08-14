use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const STRICT_VERDICT_CSV: &str = "../../../Codex/STRICT_DETECTOR_VERDICT_2026-06-13.csv";
const TOKEN_AWARE_STRICT_VERDICT_CSV: &str =
    "../../../Codex/TOKEN_AWARE_STRICT_DETECTOR_VERDICT_2026-06-14.csv";
const TOKEN_AWARE_ROW_CLASSIFICATION_CSV: &str =
    "../../../Codex/TOKEN_AWARE_STRICT_DETECTOR_ROW_CLASSIFICATION_2026-06-14.csv";
const STATE_POINT_SENSITIVE_LOSS_CSV: &str =
    "../../../Codex/STATE_POINT_SENSITIVE_LOSS_ROWS_2026-06-14.csv";
const STATE_POINT_SENSITIVE_LOSS_JSON: &str =
    "../../contracts/test/fixtures/gate9_state_sensitive_loss_rows.json";
const STATE_POINT_SENSITIVE_LOSS_EXACT_STATUS_CSV: &str =
    "../../../Codex/STATE_POINT_SENSITIVE_LOSS_EXACT_REPLAY_STATUS_2026-06-14.csv";
const GATE9_REVM_DIR: &str = "../../../codex_tmp/gate9_revm_preflight/gate9";
const TOKEN_AWARE_STRICT_FIXTURE: &str =
    "../../contracts/test/fixtures/gate9_detect_routes_with_tokens_from_B.txt";
const DETECTOR_VERDICT_FORK_TEST: &str = "../../contracts/test/MantisDetectorVerdict.Fork.t.sol";
const MANTIS_EXECUTOR_SOL: &str = "../../contracts/src/MantisExecutor.sol";
const GATE9_REVM_SCRIPT: &str = "../../scripts/gate9_equivalence_revm.sh";
const GATE9_CORPUS: &str = "../../../anvil/corpus/blk_465686729.jsonl";
const STRICT_BLOCK_465686729_LOG: &str =
    "../../../codex_tmp/strict_detector/token_path_strict_detector_block465686729_2026-06-14.log";
const FOUNDRY_ANVIL_CATALOG_LOG: &str =
    "../../../codex_tmp/foundry/anvil_catalog_positive_2026-06-14.log";
const FOUNDRY_DETECT_EXECUTE_LOG: &str =
    "../../../codex_tmp/foundry/detect_execute_profit_2026-06-14.log";
const FOUNDRY_SLIPPAGE_CURVE_LOG: &str = "../../../codex_tmp/foundry/slippage_curve_2026-06-14.log";
const FOUNDRY_FAMILY_REALIZED_LOG: &str =
    "../../../codex_tmp/foundry/family_realized_pnl_2026-06-14.log";
const FOUNDRY_GATE5_REPLACEMENT_LOG: &str =
    "../../../codex_tmp/foundry/gate5_replacement_set_2026-06-14.log";
const FOUNDRY_C2_PREDICTOR_LOG: &str = "../../../codex_tmp/foundry/c2_predictor_2026-06-14.log";
const FOUNDRY_STRICT_METADATA_REPLAY_LOG: &str =
    "../../../codex_tmp/foundry/strict_metadata_replay_2026-06-14.log";
const WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";

#[derive(Debug)]
struct StrictVerdictRow {
    route_id: u64,
    block_number: u64,
    hop_count: u8,
    exchange_ids: String,
    input_amount: u128,
    gross_profit: u128,
    outcome: String,
    realized_net_wei: i128,
    revert_selector: String,
    reason: String,
}

#[derive(Debug)]
struct TokenAwareDetectRow {
    route_id: u64,
    block_number: u64,
    input_amount: u128,
    gross_profit: u128,
    hop_count: u8,
    exchanges: Vec<String>,
    pools: Vec<String>,
    tokens: Vec<String>,
}

#[derive(Debug)]
struct TokenAwareVerdictRow {
    route_id: u64,
    block_number: u64,
    hop_count: u8,
    exchange_ids: String,
    start_token: String,
    end_token: String,
    input_amount: u128,
    gross_profit: u128,
    outcome: String,
    realized_net_wei: Option<i128>,
    reason: String,
    token_path: Vec<String>,
    pool_path: Vec<String>,
}

#[derive(Debug)]
struct TokenAwareRowClassification {
    route_id: u64,
    detector_block_tag: u64,
    hop_count: u8,
    exchange_ids: String,
    family_path: String,
    start_is_weth: bool,
    input_amount: u128,
    gross_profit: u128,
    foundry_outcome: String,
    foundry_realized_net_wei: Option<i128>,
    first_revm_seq: u64,
    first_revm_result: String,
    first_revm_balance_delta_wei: Option<i128>,
    last_revm_seq: u64,
    last_revm_result: String,
    last_revm_balance_delta_wei: Option<i128>,
    last_revm_reject_reason: String,
    blocker_class: String,
}

fn read_required(path: impl AsRef<Path>, label: &str) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {label} at {}: {err}", path.display()))
}

fn collect_csv_files(root: &Path, out: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    for entry in fs::read_dir(root).unwrap_or_else(|err| {
        panic!(
            "failed to read CSV artifact directory {}: {err}",
            root.display()
        )
    }) {
        let path = entry
            .unwrap_or_else(|err| {
                panic!(
                    "failed to read CSV artifact entry under {}: {err}",
                    root.display()
                )
            })
            .path();
        if path.is_dir() {
            collect_csv_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "csv") {
            out.push(path);
        }
    }
}

fn csv_record_width(line: &str) -> usize {
    let mut width = 1;
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => width += 1,
            _ => {}
        }
    }
    assert!(!in_quotes, "unterminated quoted CSV field in `{line}`");
    width
}

#[derive(Clone, Copy, Debug)]
enum CsvArtifactDialect {
    StandardFixedWidth,
    HashCommentFixedWidth,
    StrictDetectorTrailingReason,
}

fn declared_csv_artifact_dialect(path: &Path) -> CsvArtifactDialect {
    let rendered = path.to_string_lossy();
    if rendered.ends_with("Codex/STRICT_DETECTOR_VERDICT_2026-06-13.csv") {
        return CsvArtifactDialect::StrictDetectorTrailingReason;
    }
    if rendered.ends_with("anvil/dune_arb_taxonomy.csv")
        || rendered.ends_with("contracts/test/fixtures/arb_taxonomy_catalog.csv")
        || rendered.ends_with("research/strategy_executor/index_pool_map.csv")
    {
        return CsvArtifactDialect::HashCommentFixedWidth;
    }
    CsvArtifactDialect::StandardFixedWidth
}

fn csv_width_failures_for_dialect(
    raw: &str,
    dialect: CsvArtifactDialect,
) -> Vec<(usize, usize, usize)> {
    match dialect {
        CsvArtifactDialect::StandardFixedWidth => csv_width_failures(
            raw.lines()
                .enumerate()
                .filter(|(_, line)| !line.trim().is_empty())
                .map(|(idx, line)| (idx, line.trim_end_matches('\r'))),
        ),
        CsvArtifactDialect::HashCommentFixedWidth => csv_width_failures(
            raw.lines()
                .enumerate()
                .filter(|(_, line)| {
                    let trimmed = line.trim();
                    !trimmed.is_empty() && !trimmed.starts_with('#')
                })
                .map(|(idx, line)| (idx, line.trim_end_matches('\r'))),
        ),
        CsvArtifactDialect::StrictDetectorTrailingReason => {
            let mut failures = Vec::new();
            for (idx, line) in raw.lines().enumerate() {
                let line = line.trim_end_matches('\r');
                if line.trim().is_empty() {
                    continue;
                }
                let width = line.splitn(10, ',').count();
                if width != 10 {
                    failures.push((idx + 1, width, 10));
                }
            }
            failures
        }
    }
}

fn csv_width_failures<'a>(
    rows: impl Iterator<Item = (usize, &'a str)>,
) -> Vec<(usize, usize, usize)> {
    let mut expected_width = None;
    let mut bad_rows = Vec::new();
    for (idx, line) in rows {
        let width = csv_record_width(line);
        match expected_width {
            Some(expected) if width != expected => bad_rows.push((idx + 1, width, expected)),
            Some(_) => {}
            None => expected_width = Some(width),
        }
    }
    bad_rows
}

fn parse_u64(value: &str, label: &str, line_no: usize) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {value}: {err}"))
}

fn parse_u128(value: &str, label: &str, line_no: usize) -> u128 {
    value
        .parse::<u128>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {value}: {err}"))
}

fn parse_i128(value: &str, label: &str, line_no: usize) -> i128 {
    value
        .parse::<i128>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {value}: {err}"))
}

fn load_strict_verdict() -> Vec<StrictVerdictRow> {
    let raw = read_required(STRICT_VERDICT_CSV, "strict detector verdict csv");
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(idx, line)| {
            if idx == 0 {
                assert_eq!(
                    line,
                    "route_id,block_number,hop_count,exchange_ids,input_amount,gross_profit,outcome,realized_net_wei,revert_selector,reason",
                    "unexpected strict verdict CSV header"
                );
                return None;
            }
            let line_no = idx + 1;
            let fields = line.splitn(10, ',').collect::<Vec<_>>();
            assert_eq!(fields.len(), 10, "line {line_no}: malformed CSV row");
            Some(StrictVerdictRow {
                route_id: parse_u64(fields[0], "route_id", line_no),
                block_number: parse_u64(fields[1], "block_number", line_no),
                hop_count: fields[2]
                    .parse::<u8>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}")),
                exchange_ids: fields[3].to_string(),
                input_amount: parse_u128(fields[4], "input_amount", line_no),
                gross_profit: parse_u128(fields[5], "gross_profit", line_no),
                outcome: fields[6].to_string(),
                realized_net_wei: parse_i128(fields[7], "realized_net_wei", line_no),
                revert_selector: fields[8].to_string(),
                reason: fields[9].to_string(),
            })
        })
        .collect()
}

fn load_token_aware_detect_fixture() -> Vec<TokenAwareDetectRow> {
    let raw = read_required(
        TOKEN_AWARE_STRICT_FIXTURE,
        "token-aware strict detector fixture",
    );
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            let line_no = idx + 1;
            let fields = line.split(',').collect::<Vec<_>>();
            assert!(
                fields.len() >= 7,
                "line {line_no}: malformed TOKEN_DETECT row"
            );
            assert_eq!(
                fields[0], "TOKEN_DETECT",
                "line {line_no}: unexpected token-aware fixture prefix"
            );
            let hop_count = fields[5]
                .parse::<u8>()
                .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}"));
            assert_eq!(
                fields.len(),
                7 + usize::from(hop_count) * 3,
                "line {line_no}: malformed TOKEN_DETECT field count"
            );

            let mut exchanges = Vec::with_capacity(usize::from(hop_count));
            let mut pools = Vec::with_capacity(usize::from(hop_count));
            let mut tokens = Vec::with_capacity(usize::from(hop_count) + 1);
            tokens.push(fields[6].to_ascii_lowercase());
            for i in 0..usize::from(hop_count) {
                exchanges.push(fields[7 + i * 3].to_string());
                pools.push(fields[8 + i * 3].to_ascii_lowercase());
                tokens.push(fields[9 + i * 3].to_ascii_lowercase());
            }

            TokenAwareDetectRow {
                route_id: parse_u64(fields[1], "route_id", line_no),
                block_number: parse_u64(fields[2], "block_number", line_no),
                input_amount: parse_u128(fields[3], "input_amount", line_no),
                gross_profit: parse_u128(fields[4], "gross_profit", line_no),
                hop_count,
                exchanges,
                pools,
                tokens,
            }
        })
        .collect()
}

fn load_token_aware_strict_verdict() -> Vec<TokenAwareVerdictRow> {
    let raw = read_required(
        TOKEN_AWARE_STRICT_VERDICT_CSV,
        "token-aware strict detector verdict csv",
    );
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(idx, line)| {
            if idx == 0 {
                assert_eq!(
                    line,
                    "route_id,block_number,hop_count,exchange_ids,start_token,end_token,input_amount,gross_profit,outcome,realized_net_wei,reason,token_path,pool_path",
                    "unexpected token-aware strict verdict CSV header"
                );
                return None;
            }
            let line_no = idx + 1;
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                13,
                "line {line_no}: malformed token-aware verdict row"
            );
            Some(TokenAwareVerdictRow {
                route_id: parse_u64(fields[0], "route_id", line_no),
                block_number: parse_u64(fields[1], "block_number", line_no),
                hop_count: fields[2]
                    .parse::<u8>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}")),
                exchange_ids: fields[3].to_string(),
                start_token: fields[4].to_ascii_lowercase(),
                end_token: fields[5].to_ascii_lowercase(),
                input_amount: parse_u128(fields[6], "input_amount", line_no),
                gross_profit: parse_u128(fields[7], "gross_profit", line_no),
                outcome: fields[8].to_string(),
                realized_net_wei: if fields[9].is_empty() {
                    None
                } else {
                    Some(parse_i128(fields[9], "realized_net_wei", line_no))
                },
                reason: fields[10].to_string(),
                token_path: fields[11]
                    .split('>')
                    .map(str::to_ascii_lowercase)
                    .collect(),
                pool_path: fields[12]
                    .split('>')
                    .map(str::to_ascii_lowercase)
                    .collect(),
            })
        })
        .collect()
}

fn parse_optional_i128(value: &str, label: &str, line_no: usize) -> Option<i128> {
    if value.is_empty() {
        None
    } else {
        Some(parse_i128(value, label, line_no))
    }
}

fn load_token_aware_row_classification() -> Vec<TokenAwareRowClassification> {
    let raw = read_required(
        TOKEN_AWARE_ROW_CLASSIFICATION_CSV,
        "token-aware strict detector row classification csv",
    );
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(idx, line)| {
            if idx == 0 {
                assert_eq!(
                    line,
                    "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_outcome,foundry_realized_net_wei,first_revm_seq,first_revm_result,first_revm_balance_delta_wei,last_revm_seq,last_revm_result,last_revm_balance_delta_wei,last_revm_reject_reason,blocker_class",
                    "unexpected token-aware row classification CSV header"
                );
                return None;
            }
            let line_no = idx + 1;
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                18,
                "line {line_no}: malformed token-aware row classification"
            );
            Some(TokenAwareRowClassification {
                route_id: parse_u64(fields[0], "route_id", line_no),
                detector_block_tag: parse_u64(fields[1], "detector_block_tag", line_no),
                hop_count: fields[2]
                    .parse::<u8>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}")),
                exchange_ids: fields[3].to_string(),
                family_path: fields[4].to_string(),
                start_is_weth: fields[5]
                    .parse::<bool>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid start_is_weth: {err}")),
                input_amount: parse_u128(fields[6], "input_amount", line_no),
                gross_profit: parse_u128(fields[7], "gross_profit", line_no),
                foundry_outcome: fields[8].to_string(),
                foundry_realized_net_wei: parse_optional_i128(
                    fields[9],
                    "foundry_realized_net_wei",
                    line_no,
                ),
                first_revm_seq: parse_u64(fields[10], "first_revm_seq", line_no),
                first_revm_result: fields[11].to_string(),
                first_revm_balance_delta_wei: parse_optional_i128(
                    fields[12],
                    "first_revm_balance_delta_wei",
                    line_no,
                ),
                last_revm_seq: parse_u64(fields[13], "last_revm_seq", line_no),
                last_revm_result: fields[14].to_string(),
                last_revm_balance_delta_wei: parse_optional_i128(
                    fields[15],
                    "last_revm_balance_delta_wei",
                    line_no,
                ),
                last_revm_reject_reason: fields[16].to_string(),
                blocker_class: fields[17].to_string(),
            })
        })
        .collect()
}

fn gate9_path(name: &str) -> PathBuf {
    PathBuf::from(GATE9_REVM_DIR).join(name)
}

fn load_jsonl(path: impl AsRef<Path>, label: &str) -> Vec<Value> {
    let raw = read_required(path, label);
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|err| panic!("{label} line {} is not valid JSON: {err}", idx + 1))
        })
        .collect()
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key} in {value}"))
        .to_string()
}

fn candidate_seq(row: &Value) -> u64 {
    let candidate_id = string_field(row, "candidate_id");
    let rest = candidate_id
        .strip_prefix("seq:")
        .unwrap_or_else(|| panic!("candidate_id missing seq prefix: {candidate_id}"));
    let seq = rest
        .split(':')
        .next()
        .unwrap_or_else(|| panic!("candidate_id missing seq value: {candidate_id}"));
    seq.parse::<u64>()
        .unwrap_or_else(|err| panic!("invalid candidate seq in {candidate_id}: {err}"))
}

fn optional_string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing u64 field {key} in {value}"))
}

fn string_array_field(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field {key} in {value}"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("non-string item in field {key}: {value}"))
                .to_string()
        })
        .collect()
}

fn byte_array_field(value: &Value, key: &str) -> Vec<u8> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing byte-array field {key} in {value}"))
        .iter()
        .map(|item| {
            let raw = item
                .as_u64()
                .unwrap_or_else(|| panic!("non-u64 item in byte-array field {key}: {value}"));
            u8::try_from(raw)
                .unwrap_or_else(|err| panic!("out-of-range byte in field {key}: {raw}: {err}"))
        })
        .collect()
}

fn first_occurrence_map(
    rows: &[Value],
) -> BTreeMap<(u64, String), (String, String, String, String)> {
    let mut out = BTreeMap::new();
    for row in rows {
        let key = (
            u64_field(row, "route_id"),
            string_field(row, "input_amount"),
        );
        out.entry(key).or_insert_with(|| {
            (
                string_field(row, "gross_profit_wei"),
                string_field(row, "revm_result"),
                optional_string_field(row, "decision"),
                optional_string_field(row, "net_profit_wei"),
            )
        });
    }
    out
}

fn first_occurrence_token_path_map(rows: &[Value]) -> BTreeMap<(u64, String), Vec<String>> {
    let mut out = BTreeMap::new();
    for row in rows {
        let key = (
            u64_field(row, "route_id"),
            string_field(row, "input_amount"),
        );
        out.entry(key)
            .or_insert_with(|| string_array_field(row, "tokens"));
    }
    out
}

fn first_occurrence_route_path_map(
    rows: &[Value],
) -> BTreeMap<(u64, String), (Vec<String>, Vec<String>)> {
    let mut out = BTreeMap::new();
    for row in rows {
        let key = (
            u64_field(row, "route_id"),
            string_field(row, "input_amount"),
        );
        out.entry(key).or_insert_with(|| {
            (
                string_array_field(row, "tokens")
                    .into_iter()
                    .map(|value| value.to_ascii_lowercase())
                    .collect(),
                string_array_field(row, "pools")
                    .into_iter()
                    .map(|value| value.to_ascii_lowercase())
                    .collect(),
            )
        });
    }
    out
}

fn first_occurrence_revm_row_map(rows: &[Value]) -> BTreeMap<(u64, String), Value> {
    let mut out = BTreeMap::new();
    for row in rows {
        let key = (
            u64_field(row, "route_id"),
            string_field(row, "input_amount"),
        );
        out.entry(key).or_insert_with(|| row.clone());
    }
    out
}

#[test]
fn strict_detector_verdict_artifact_has_complete_blocker_taxonomy() {
    let rows = load_strict_verdict();
    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let revm_token_paths = first_occurrence_token_path_map(&b_rows);
    assert_eq!(rows.len(), 29, "strict detector verdict row count drifted");
    let hop_counts = rows
        .iter()
        .map(|row| row.hop_count)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        hop_counts,
        BTreeSet::from([2, 3, 4]),
        "strict detector verdict no longer covers 2/3/4-hop rows"
    );

    let positives = rows
        .iter()
        .filter(|row| row.outcome == "positive")
        .collect::<Vec<_>>();
    let other_reverts = rows
        .iter()
        .filter(|row| row.outcome == "other_revert")
        .collect::<Vec<_>>();
    let losses = rows
        .iter()
        .filter(|row| row.outcome == "loss")
        .collect::<Vec<_>>();

    assert_eq!(positives.len(), 4, "positive strict detector count drifted");
    assert_eq!(
        other_reverts.len(),
        17,
        "other-revert strict detector count drifted"
    );
    assert_eq!(losses.len(), 8, "strict detector Loss count drifted");

    for row in &rows {
        assert!(row.route_id > 0, "route id must be nonzero");
        assert!(row.block_number > 0, "block number must be nonzero");
        assert!(row.input_amount > 0, "input amount must be nonzero");
        assert!(row.gross_profit > 0, "gross profit must be nonzero");
        assert!(!row.exchange_ids.is_empty(), "exchange id path missing");
        assert!(
            !row.reason.is_empty(),
            "reason missing for route {}",
            row.route_id
        );
    }

    let positive_keys = positives
        .iter()
        .map(|row| (row.route_id, row.block_number))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        positive_keys,
        BTreeSet::from([
            (16752, 22_207_819),
            (3396, 22_207_819),
            (362, 22_207_820),
            (3396, 22_207_820),
        ]),
        "strict-positive route set drifted"
    );
    for row in positives {
        assert!(
            row.realized_net_wei > 0,
            "strict-positive row has non-positive realized net"
        );
        assert!(
            row.revert_selector.is_empty(),
            "strict-positive row must not have revert selector"
        );
    }

    for row in &other_reverts {
        assert_eq!(
            row.realized_net_wei, 0,
            "non-positive strict row unexpectedly has realized net"
        );
    }

    let solidity_error_reverts = other_reverts
        .iter()
        .filter(|row| row.revert_selector == "0x08c379a0")
        .count();
    let metadata_limited_reverts = other_reverts
        .iter()
        .filter(|row| row.reason.contains("metadata-limited family"))
        .count();
    let token_path = |row: &StrictVerdictRow| {
        revm_token_paths
            .get(&(row.route_id, row.input_amount.to_string()))
            .unwrap_or_else(|| panic!("missing REVM token path for strict row {row:?}"))
    };
    let non_weth_cycle_rows = rows
        .iter()
        .filter(|row| token_path(row).first().map(String::as_str) != Some(WETH))
        .count();
    let non_weth_error_rows = other_reverts
        .iter()
        .filter(|row| row.revert_selector == "0x08c379a0")
        .filter(|row| token_path(row).first().map(String::as_str) != Some(WETH))
        .count();
    let non_weth_loss_rows = losses
        .iter()
        .filter(|row| token_path(row).first().map(String::as_str) != Some(WETH))
        .count();
    let weth_loss_rows = losses.len() - non_weth_loss_rows;
    let non_weth_metadata_rows = other_reverts
        .iter()
        .filter(|row| row.reason.contains("metadata-limited family"))
        .filter(|row| token_path(row).first().map(String::as_str) != Some(WETH))
        .count();
    let weth_metadata_rows = metadata_limited_reverts - non_weth_metadata_rows;

    assert_eq!(
        solidity_error_reverts, 11,
        "Solidity Error(string) revert count drifted"
    );
    assert_eq!(
        metadata_limited_reverts, 6,
        "metadata-limited strict row count drifted"
    );
    assert_eq!(
        non_weth_cycle_rows, 18,
        "strict verdict non-WETH cycle count drifted"
    );
    assert_eq!(
        non_weth_error_rows, 11,
        "all Solidity Error(string) strict rows should currently be non-WETH token-path schema gaps"
    );
    assert_eq!(
        non_weth_loss_rows, 4,
        "non-WETH strict Loss rows count drifted"
    );
    assert_eq!(weth_loss_rows, 4, "WETH strict Loss rows count drifted");
    assert_eq!(
        non_weth_metadata_rows, 3,
        "non-WETH metadata-limited strict rows count drifted"
    );
    assert_eq!(
        weth_metadata_rows, 3,
        "WETH metadata-limited strict rows count drifted"
    );
    assert_eq!(
        solidity_error_reverts + metadata_limited_reverts,
        other_reverts.len(),
        "strict verdict blocker classes do not partition all non-positive rows"
    );

    let loss_keys = losses
        .iter()
        .map(|row| (row.route_id, row.block_number))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        loss_keys,
        BTreeSet::from([
            (425, 22_207_820),
            (417, 22_207_820),
            (423, 22_207_820),
            (421, 22_207_820),
            (16772, 22_207_821),
            (16836, 22_207_821),
            (16752, 22_207_821),
            (21919, 22_207_821),
        ]),
        "strict Loss route set drifted"
    );
    for row in losses {
        assert!(
            row.realized_net_wei < 0,
            "Loss row must carry negative shortfall as realized net"
        );
        assert!(
            row.reason.contains("Loss profit gate"),
            "Loss row reason must name the profit gate"
        );
    }

    for (route_id, expected_exchange_ids) in [(3258, "0+9+0"), (3240, "0+25+0"), (6252, "0+20+18")]
    {
        let row = rows
            .iter()
            .find(|row| row.route_id == route_id)
            .unwrap_or_else(|| panic!("missing strict row for route {route_id}"));
        assert_eq!(row.exchange_ids, expected_exchange_ids);
        assert_eq!(
            row.outcome, "other_revert",
            "route {route_id} should remain strict-red until the detector harness is fixed"
        );
    }
}

#[test]
fn token_aware_strict_detector_fixture_matches_revm_paths_and_strict_csv() {
    let detect_rows = load_token_aware_detect_fixture();
    let strict_rows = load_strict_verdict();
    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let revm_paths = first_occurrence_route_path_map(&b_rows);
    let strict_by_key = strict_rows
        .iter()
        .map(|row| ((row.route_id, row.block_number, row.input_amount), row))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        detect_rows.len(),
        29,
        "token-aware strict fixture row count drifted"
    );
    assert_eq!(
        detect_rows.len(),
        strict_rows.len(),
        "token-aware strict fixture and strict verdict row counts diverged"
    );
    assert_eq!(
        strict_by_key.len(),
        strict_rows.len(),
        "strict verdict keys must be unique by route/block/input"
    );

    let hop_counts = detect_rows
        .iter()
        .map(|row| row.hop_count)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        hop_counts,
        BTreeSet::from([2, 3, 4]),
        "token-aware strict fixture no longer covers 2/3/4-hop rows"
    );

    for row in &detect_rows {
        assert_eq!(
            row.tokens.len(),
            usize::from(row.hop_count) + 1,
            "route {} has wrong token path length",
            row.route_id
        );
        assert_eq!(
            row.pools.len(),
            usize::from(row.hop_count),
            "route {} has wrong pool path length",
            row.route_id
        );
        assert_eq!(
            row.exchanges.len(),
            usize::from(row.hop_count),
            "route {} has wrong exchange path length",
            row.route_id
        );
        assert_eq!(
            row.tokens.first(),
            row.tokens.last(),
            "route {} is not a token roundtrip",
            row.route_id
        );

        let strict = strict_by_key
            .get(&(row.route_id, row.block_number, row.input_amount))
            .unwrap_or_else(|| panic!("missing strict CSV row for token-aware row {row:?}"));
        assert_eq!(
            row.gross_profit, strict.gross_profit,
            "route {} gross profit diverged from strict CSV",
            row.route_id
        );
        assert_eq!(
            row.exchanges.join("+"),
            strict.exchange_ids,
            "route {} exchange path diverged from strict CSV",
            row.route_id
        );

        let (revm_tokens, revm_pools) = revm_paths
            .get(&(row.route_id, row.input_amount.to_string()))
            .unwrap_or_else(|| panic!("missing REVM path for token-aware row {row:?}"));
        assert_eq!(
            &row.tokens, revm_tokens,
            "route {} token path diverged from REVM artifact",
            row.route_id
        );
        assert_eq!(
            &row.pools, revm_pools,
            "route {} pool path diverged from REVM artifact",
            row.route_id
        );
    }

    let non_weth_cycle_rows = detect_rows
        .iter()
        .filter(|row| row.tokens.first().map(String::as_str) != Some(WETH))
        .count();
    assert_eq!(
        non_weth_cycle_rows, 18,
        "token-aware strict fixture non-WETH cycle count drifted"
    );
}

#[test]
fn token_aware_foundry_verdict_is_not_yet_revm_equivalent() {
    let verdict_rows = load_token_aware_strict_verdict();
    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let revm_rows = first_occurrence_revm_row_map(&b_rows);

    assert_eq!(
        verdict_rows.len(),
        29,
        "token-aware Foundry verdict row count drifted"
    );

    let mut foundry_positive_revm_success = 0usize;
    let mut foundry_loss_revm_success = 0usize;
    let mut foundry_other_revm_success = 0usize;
    let mut all_route_net = 0i128;

    for row in &verdict_rows {
        assert_eq!(
            row.start_token, row.end_token,
            "token-aware Foundry verdict row is not cyclic: {row:?}"
        );
        assert_eq!(
            row.token_path.len(),
            usize::from(row.hop_count) + 1,
            "token-aware Foundry verdict token path length drifted: {row:?}"
        );
        assert_eq!(
            row.pool_path.len(),
            usize::from(row.hop_count),
            "token-aware Foundry verdict pool path length drifted: {row:?}"
        );
        assert!(row.block_number > 0, "missing block number for {row:?}");
        assert!(row.gross_profit > 0, "missing gross profit for {row:?}");
        assert!(
            !row.exchange_ids.is_empty(),
            "missing exchange path for {row:?}"
        );
        if let Some(net) = row.realized_net_wei {
            all_route_net += net;
        }

        let revm = revm_rows
            .get(&(row.route_id, row.input_amount.to_string()))
            .unwrap_or_else(|| panic!("missing first-occurrence REVM row for {row:?}"));
        assert_eq!(
            string_field(revm, "revm_result"),
            "success",
            "token-aware strict row is not REVM-success at first occurrence: {row:?}"
        );
        assert_eq!(
            string_field(revm, "reject_reason"),
            "none",
            "token-aware strict row has REVM reject reason at first occurrence: {row:?}"
        );
        assert_eq!(
            string_array_field(revm, "tokens")
                .into_iter()
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>(),
            row.token_path,
            "token-aware strict row token path diverges from first-occurrence REVM row: {row:?}"
        );
        assert_eq!(
            string_array_field(revm, "pools")
                .into_iter()
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>(),
            row.pool_path,
            "token-aware strict row pool path diverges from first-occurrence REVM row: {row:?}"
        );

        match row.outcome.as_str() {
            "positive" => {
                foundry_positive_revm_success += 1;
                assert!(
                    row.realized_net_wei.unwrap_or_default() > 0,
                    "Foundry positive row lacks positive realized net: {row:?}"
                );
            }
            "loss" => {
                foundry_loss_revm_success += 1;
                assert!(
                    row.realized_net_wei.unwrap_or_default() < 0,
                    "Foundry Loss row lacks negative shortfall: {row:?}"
                );
                assert!(
                    row.reason.contains("Loss profit gate"),
                    "Foundry Loss row missing profit-gate reason: {row:?}"
                );
            }
            "other_revert" => {
                foundry_other_revm_success += 1;
                assert!(
                    row.reason.contains("metadata-limited"),
                    "Foundry other_revert row is not classified as metadata-limited: {row:?}"
                );
            }
            other => panic!("unknown token-aware Foundry outcome {other}: {row:?}"),
        }
    }

    assert_eq!(
        foundry_positive_revm_success, 12,
        "Foundry-positive/REVM-success count drifted"
    );
    assert_eq!(
        foundry_loss_revm_success, 11,
        "Foundry-Loss/REVM-success equivalence-gap count drifted"
    );
    assert_eq!(
        foundry_other_revm_success, 6,
        "Foundry-other/REVM-success metadata-gap count drifted"
    );
    assert_eq!(
        all_route_net, 9_842_237_309_711_971,
        "token-aware Foundry all-route net drifted"
    );
}

#[test]
fn token_aware_revm_revalidation_is_state_point_sensitive() {
    let verdict_rows = load_token_aware_strict_verdict();
    assert_eq!(
        verdict_rows.len(),
        29,
        "token-aware Foundry verdict row count drifted"
    );

    let unique_keys = verdict_rows
        .iter()
        .map(|row| (row.route_id, row.input_amount.to_string()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unique_keys.len(),
        26,
        "token-aware route/input unique-key count drifted"
    );

    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let mut grouped = BTreeMap::<(u64, String), Vec<&Value>>::new();
    for row in &b_rows {
        let key = (
            u64_field(row, "route_id"),
            string_field(row, "input_amount"),
        );
        if unique_keys.contains(&key) {
            grouped.entry(key).or_default().push(row);
        }
    }
    assert_eq!(
        grouped.len(),
        unique_keys.len(),
        "Gate 9 B artifact does not cover every token-aware route/input key"
    );

    for rows in grouped.values_mut() {
        rows.sort_by_key(|row| candidate_seq(row));
    }

    let mut transition_counts = BTreeMap::<(String, String), usize>::new();
    let mut occurrence_distribution = BTreeMap::<usize, usize>::new();
    let mut stable_success_keys = BTreeSet::<(u64, String)>::new();

    for (key, rows) in &grouped {
        assert!(!rows.is_empty(), "missing REVM rows for key {key:?}");
        *occurrence_distribution.entry(rows.len()).or_insert(0) += 1;

        let first = rows.first().unwrap();
        let last = rows.last().unwrap();
        assert_eq!(
            string_field(first, "revm_result"),
            "success",
            "first REVM occurrence should be success for key {key:?}"
        );
        assert_eq!(
            string_field(first, "reject_reason"),
            "none",
            "first REVM occurrence should not reject key {key:?}"
        );

        let first_result = string_field(first, "revm_result");
        let last_result = string_field(last, "revm_result");
        *transition_counts
            .entry((first_result.clone(), last_result.clone()))
            .or_insert(0) += 1;

        match last_result.as_str() {
            "success" => {
                assert_eq!(
                    string_field(last, "reject_reason"),
                    "none",
                    "stable success key should not have a final reject reason: {key:?}"
                );
                stable_success_keys.insert(key.clone());
            }
            "loss" => {
                assert!(
                    string_field(last, "reject_reason").contains("loss shortfall_wei="),
                    "final loss key should expose shortfall evidence: {key:?}"
                );
            }
            other => panic!("unexpected final REVM result {other} for key {key:?}"),
        }
    }

    assert_eq!(
        transition_counts,
        BTreeMap::from([
            (("success".to_string(), "loss".to_string()), 23),
            (("success".to_string(), "success".to_string()), 3),
        ]),
        "REVM first-to-last result transitions drifted"
    );
    assert_eq!(
        occurrence_distribution,
        BTreeMap::from([(1, 3), (4, 4), (6, 7), (7, 9), (8, 1), (10, 1), (11, 1)]),
        "REVM duplicate/revalidation distribution drifted"
    );
    assert_eq!(
        stable_success_keys,
        [
            (268u64, "73436371"),
            (6252, "33316121147"),
            (6674, "14936120")
        ]
        .into_iter()
        .map(|(route_id, input_amount)| (route_id, input_amount.to_string()))
        .collect::<BTreeSet<_>>(),
        "stable REVM-success route/input set drifted"
    );

    let foundry_loss_rows = verdict_rows
        .iter()
        .filter(|row| row.outcome == "loss")
        .collect::<Vec<_>>();
    assert_eq!(
        foundry_loss_rows.len(),
        11,
        "Foundry Loss row count drifted"
    );
    let mut foundry_loss_key_counts = BTreeMap::<(u64, String), usize>::new();
    for row in foundry_loss_rows {
        *foundry_loss_key_counts
            .entry((row.route_id, row.input_amount.to_string()))
            .or_insert(0) += 1;
    }
    assert_eq!(
        foundry_loss_key_counts.len(),
        9,
        "Foundry Loss unique-key count drifted"
    );
    assert_eq!(
        foundry_loss_key_counts
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(key, count)| (key.clone(), *count))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ((3185u64, "35688324".to_string()), 2),
            ((16752u64, "64195465398059420".to_string()), 2),
        ]),
        "Foundry Loss duplicate-key set drifted"
    );
    for key in foundry_loss_key_counts.keys() {
        let rows = grouped
            .get(key)
            .unwrap_or_else(|| panic!("missing grouped REVM rows for Foundry Loss key {key:?}"));
        assert_eq!(
            string_field(rows.last().unwrap(), "revm_result"),
            "loss",
            "Foundry Loss key should also end as a later REVM loss: {key:?}"
        );
    }
}

#[test]
fn token_aware_row_classification_artifact_matches_foundry_and_revm() {
    let classifications = load_token_aware_row_classification();
    let verdict_rows = load_token_aware_strict_verdict();
    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );

    assert_eq!(
        classifications.len(),
        29,
        "token-aware row classification count drifted"
    );

    let verdict_by_key = verdict_rows
        .iter()
        .map(|row| ((row.route_id, row.block_number, row.input_amount), row))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        verdict_by_key.len(),
        verdict_rows.len(),
        "token-aware strict verdict keys are not unique"
    );

    let mut grouped = BTreeMap::<(u64, String), Vec<&Value>>::new();
    for row in &b_rows {
        grouped
            .entry((
                u64_field(row, "route_id"),
                string_field(row, "input_amount"),
            ))
            .or_default()
            .push(row);
    }
    for rows in grouped.values_mut() {
        rows.sort_by_key(|row| candidate_seq(row));
    }

    let mut class_counts = BTreeMap::<String, usize>::new();
    let mut stable_metadata_success_keys = BTreeSet::<(u64, String)>::new();

    for row in &classifications {
        assert!(
            (2..=4).contains(&row.hop_count),
            "classification row has unsupported hop count: {row:?}"
        );
        assert!(row.gross_profit > 0, "gross profit missing: {row:?}");
        assert!(
            !row.exchange_ids.is_empty(),
            "exchange path missing: {row:?}"
        );
        assert!(!row.family_path.is_empty(), "family path missing: {row:?}");

        let verdict = verdict_by_key
            .get(&(row.route_id, row.detector_block_tag, row.input_amount))
            .unwrap_or_else(|| panic!("classification row missing strict verdict: {row:?}"));
        assert_eq!(
            row.hop_count, verdict.hop_count,
            "classification hop count diverges from strict verdict: {row:?}"
        );
        assert_eq!(
            row.exchange_ids, verdict.exchange_ids,
            "classification exchange path diverges from strict verdict: {row:?}"
        );
        assert_eq!(
            row.gross_profit, verdict.gross_profit,
            "classification gross profit diverges from strict verdict: {row:?}"
        );
        assert_eq!(
            row.foundry_outcome, verdict.outcome,
            "classification Foundry outcome diverges from strict verdict: {row:?}"
        );
        assert_eq!(
            row.foundry_realized_net_wei, verdict.realized_net_wei,
            "classification realized net diverges from strict verdict: {row:?}"
        );
        assert_eq!(
            row.start_is_weth,
            verdict.start_token == WETH,
            "classification start token flag diverges from strict verdict: {row:?}"
        );

        let revm_rows = grouped
            .get(&(row.route_id, row.input_amount.to_string()))
            .unwrap_or_else(|| panic!("classification row missing Gate 9 REVM rows: {row:?}"));
        let first = revm_rows.first().unwrap();
        let last = revm_rows.last().unwrap();

        assert_eq!(
            row.first_revm_seq,
            candidate_seq(first),
            "classification first REVM sequence drifted: {row:?}"
        );
        assert_eq!(
            row.first_revm_result,
            string_field(first, "revm_result"),
            "classification first REVM result drifted: {row:?}"
        );
        assert_eq!(
            row.first_revm_balance_delta_wei,
            parse_optional_i128(
                &optional_string_field(first, "balance_delta_wei"),
                "first_revm_balance_delta_wei",
                0
            ),
            "classification first REVM balance delta drifted: {row:?}"
        );
        assert_eq!(
            row.last_revm_seq,
            candidate_seq(last),
            "classification last REVM sequence drifted: {row:?}"
        );
        assert_eq!(
            row.last_revm_result,
            string_field(last, "revm_result"),
            "classification last REVM result drifted: {row:?}"
        );
        assert_eq!(
            row.last_revm_balance_delta_wei,
            parse_optional_i128(
                &optional_string_field(last, "balance_delta_wei"),
                "last_revm_balance_delta_wei",
                0
            ),
            "classification last REVM balance delta drifted: {row:?}"
        );
        assert_eq!(
            row.last_revm_reject_reason,
            string_field(last, "reject_reason"),
            "classification last REVM reject reason drifted: {row:?}"
        );

        *class_counts.entry(row.blocker_class.clone()).or_insert(0) += 1;
        match row.blocker_class.as_str() {
            "foundry_positive" => {
                assert_eq!(row.foundry_outcome, "positive", "{row:?}");
                assert!(
                    row.foundry_realized_net_wei.unwrap_or_default() > 0,
                    "{row:?}"
                );
                assert_eq!(row.first_revm_result, "success", "{row:?}");
            }
            "state_point_sensitive_loss" => {
                assert_eq!(row.foundry_outcome, "loss", "{row:?}");
                assert!(
                    row.foundry_realized_net_wei.unwrap_or_default() < 0,
                    "{row:?}"
                );
                assert_eq!(row.first_revm_result, "success", "{row:?}");
                assert_eq!(row.last_revm_result, "loss", "{row:?}");
                assert!(
                    row.last_revm_reject_reason.contains("loss shortfall_wei="),
                    "{row:?}"
                );
            }
            "strict_metadata_missing" => {
                assert_eq!(row.foundry_outcome, "other_revert", "{row:?}");
                assert_eq!(row.first_revm_result, "success", "{row:?}");
                assert!(
                    row.family_path.contains("UniswapV4")
                        || row.family_path.contains("CurveStableSwapNg"),
                    "metadata-missing row must identify V4 or Curve metadata need: {row:?}"
                );
                if row.last_revm_result == "success" {
                    stable_metadata_success_keys
                        .insert((row.route_id, row.input_amount.to_string()));
                }
            }
            other => panic!("unexpected blocker class {other}: {row:?}"),
        }
    }

    assert_eq!(
        class_counts,
        BTreeMap::from([
            ("foundry_positive".to_string(), 12),
            ("state_point_sensitive_loss".to_string(), 11),
            ("strict_metadata_missing".to_string(), 6),
        ]),
        "token-aware classification counts drifted"
    );
    assert_eq!(
        stable_metadata_success_keys,
        BTreeSet::from([
            (268u64, "73436371".to_string()),
            (6252u64, "33316121147".to_string()),
            (6674u64, "14936120".to_string()),
        ]),
        "stable metadata-success route/input set drifted"
    );
}

#[test]
fn state_point_sensitive_loss_rows_have_first_success_then_later_loss_artifact() {
    let classifications = load_token_aware_row_classification();
    let expected_loss_keys = classifications
        .iter()
        .filter(|row| row.blocker_class == "state_point_sensitive_loss")
        .map(|row| {
            (
                row.route_id,
                row.detector_block_tag,
                row.input_amount.to_string(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_loss_keys.len(),
        11,
        "state-point-sensitive loss classification count drifted"
    );

    let raw = read_required(
        STATE_POINT_SENSITIVE_LOSS_CSV,
        "state-point-sensitive Loss row artifact",
    );
    let mut rows = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(
                line,
                "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_loss_wei,occurrences,success_occurrences,loss_occurrences,first_seq,first_balance_delta_wei,last_seq,last_loss_shortfall_wei,last_gas_used,seq_results,token_path,pool_path",
                "unexpected state-point-sensitive Loss CSV header"
            );
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            20,
            "line {line_no}: malformed state-point-sensitive Loss row"
        );
        rows.push((line_no, fields));
    }

    assert_eq!(
        rows.len(),
        11,
        "state-point-sensitive Loss row artifact count drifted"
    );

    let mut seen_loss_keys = BTreeSet::new();
    let mut unique_route_inputs = BTreeSet::new();
    let mut hop_counts = BTreeMap::<u8, usize>::new();
    let mut total_occurrences = 0u64;
    let mut total_success_occurrences = 0u64;
    let mut total_loss_occurrences = 0u64;

    for (line_no, fields) in &rows {
        let route_id = parse_u64(fields[0], "route_id", *line_no);
        let detector_block_tag = parse_u64(fields[1], "detector_block_tag", *line_no);
        let hop_count = fields[2]
            .parse::<u8>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}"));
        let input_amount = fields[6].to_string();
        let gross_profit = parse_u128(fields[7], "gross_profit", *line_no);
        let foundry_loss = parse_i128(fields[8], "foundry_loss_wei", *line_no);
        let occurrences = parse_u64(fields[9], "occurrences", *line_no);
        let success_occurrences = parse_u64(fields[10], "success_occurrences", *line_no);
        let loss_occurrences = parse_u64(fields[11], "loss_occurrences", *line_no);
        let first_seq = parse_u64(fields[12], "first_seq", *line_no);
        let first_balance_delta = parse_i128(fields[13], "first_balance_delta_wei", *line_no);
        let last_seq = parse_u64(fields[14], "last_seq", *line_no);
        let last_loss_shortfall = parse_u128(fields[15], "last_loss_shortfall_wei", *line_no);
        let last_gas_used = parse_u64(fields[16], "last_gas_used", *line_no);
        let seq_results = fields[17];
        let token_path = fields[18].split('>').collect::<Vec<_>>();
        let pool_path = fields[19].split('>').collect::<Vec<_>>();

        assert!(
            expected_loss_keys.contains(&(route_id, detector_block_tag, input_amount.clone())),
            "line {line_no}: Loss artifact row is not in row-classification artifact"
        );
        assert!(
            seen_loss_keys.insert((route_id, detector_block_tag, input_amount.clone())),
            "line {line_no}: duplicate state-point-sensitive Loss row"
        );
        unique_route_inputs.insert((route_id, input_amount));
        *hop_counts.entry(hop_count).or_insert(0) += 1;
        total_occurrences += occurrences;
        total_success_occurrences += success_occurrences;
        total_loss_occurrences += loss_occurrences;

        assert!(gross_profit > 0, "line {line_no}: gross profit missing");
        assert!(
            foundry_loss < 0,
            "line {line_no}: Foundry Loss row must carry negative realized net"
        );
        assert_eq!(
            occurrences,
            success_occurrences + loss_occurrences,
            "line {line_no}: occurrence split does not add up"
        );
        assert!(
            success_occurrences >= 1,
            "line {line_no}: row must have at least one successful REVM occurrence"
        );
        assert!(
            loss_occurrences >= 1,
            "line {line_no}: row must have at least one later REVM loss"
        );
        assert!(
            first_seq < last_seq,
            "line {line_no}: first success must precede last loss"
        );
        assert!(
            first_balance_delta > 0,
            "line {line_no}: first REVM success must have positive balance delta"
        );
        assert!(
            last_loss_shortfall > 0,
            "line {line_no}: final REVM loss shortfall missing"
        );
        assert!(last_gas_used > 0, "line {line_no}: final gas_used missing");
        assert_eq!(
            token_path.len(),
            usize::from(hop_count) + 1,
            "line {line_no}: token path length drifted"
        );
        assert_eq!(
            pool_path.len(),
            usize::from(hop_count),
            "line {line_no}: pool path length drifted"
        );
        assert!(
            seq_results.contains(&format!("{first_seq}:success:{first_balance_delta}")),
            "line {line_no}: seq_results missing first success evidence"
        );
        assert!(
            seq_results.contains(&format!(
                "{last_seq}:loss:loss shortfall_wei={last_loss_shortfall} gas_used={last_gas_used}"
            )),
            "line {line_no}: seq_results missing final loss evidence"
        );
    }

    assert_eq!(
        seen_loss_keys, expected_loss_keys,
        "state-point-sensitive Loss artifact does not cover every classified Loss row"
    );
    assert_eq!(
        unique_route_inputs.len(),
        9,
        "state-point-sensitive Loss unique route/input count drifted"
    );
    assert_eq!(
        hop_counts,
        BTreeMap::from([(2u8, 1usize), (3u8, 6usize), (4u8, 4usize)]),
        "state-point-sensitive Loss hop distribution drifted"
    );
    assert_eq!(
        (
            total_occurrences,
            total_success_occurrences,
            total_loss_occurrences
        ),
        (85, 15, 70),
        "state-point-sensitive Loss occurrence totals drifted"
    );
}

#[test]
fn state_point_sensitive_loss_rows_have_exact_replay_status_artifacts() {
    #[derive(Debug)]
    struct ExpectedLossRow {
        hop_count: u8,
        exchange_ids: Vec<u8>,
        family_path: Vec<String>,
        start_is_weth: bool,
        gross_profit: String,
        foundry_loss_wei: String,
        occurrences: u64,
        success_occurrences: u64,
        loss_occurrences: u64,
        first_seq: u64,
        first_balance_delta_wei: String,
        last_seq: u64,
        last_loss_shortfall_wei: String,
        last_gas_used: u64,
        token_path: Vec<String>,
        pool_path: Vec<String>,
    }

    let raw = read_required(
        STATE_POINT_SENSITIVE_LOSS_CSV,
        "state-point-sensitive Loss row artifact",
    );
    let mut expected = BTreeMap::<(u64, u64, String), ExpectedLossRow>::new();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            20,
            "line {line_no}: malformed state-point-sensitive Loss row"
        );

        let route_id = parse_u64(fields[0], "route_id", line_no);
        let detector_block_tag = parse_u64(fields[1], "detector_block_tag", line_no);
        let input_amount = fields[6].to_string();
        let previous = expected.insert(
            (route_id, detector_block_tag, input_amount),
            ExpectedLossRow {
                hop_count: fields[2]
                    .parse::<u8>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}")),
                exchange_ids: fields[3]
                    .split('+')
                    .map(|value| {
                        value.parse::<u8>().unwrap_or_else(|err| {
                            panic!("line {line_no}: invalid exchange id {value}: {err}")
                        })
                    })
                    .collect(),
                family_path: fields[4].split('>').map(str::to_string).collect(),
                start_is_weth: fields[5]
                    .parse::<bool>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid start_is_weth: {err}")),
                gross_profit: fields[7].to_string(),
                foundry_loss_wei: fields[8].to_string(),
                occurrences: parse_u64(fields[9], "occurrences", line_no),
                success_occurrences: parse_u64(fields[10], "success_occurrences", line_no),
                loss_occurrences: parse_u64(fields[11], "loss_occurrences", line_no),
                first_seq: parse_u64(fields[12], "first_seq", line_no),
                first_balance_delta_wei: fields[13].to_string(),
                last_seq: parse_u64(fields[14], "last_seq", line_no),
                last_loss_shortfall_wei: fields[15].to_string(),
                last_gas_used: parse_u64(fields[16], "last_gas_used", line_no),
                token_path: fields[18].split('>').map(str::to_string).collect(),
                pool_path: fields[19].split('>').map(str::to_string).collect(),
            },
        );
        assert!(
            previous.is_none(),
            "duplicate state-sensitive Loss key in source CSV"
        );
    }
    assert_eq!(expected.len(), 11, "Loss source CSV row count drifted");

    let fixture = serde_json::from_str::<Value>(&read_required(
        STATE_POINT_SENSITIVE_LOSS_JSON,
        "state-point-sensitive Loss JSON fixture",
    ))
    .expect("state-sensitive Loss JSON fixture");
    assert_eq!(
        string_field(&fixture, "artifact"),
        "gate9_state_sensitive_loss_rows"
    );
    assert_eq!(u64_field(&fixture, "snapshot_block"), 465_686_728);
    assert_eq!(u64_field(&fixture, "block_env"), 465_686_729);
    assert_eq!(
        string_field(&fixture, "status"),
        "exact_sequence_replay_blocked_by_missing_standalone_inputs"
    );

    let rows = fixture["rows"]
        .as_array()
        .expect("state-sensitive Loss fixture rows must be array");
    assert_eq!(rows.len(), expected.len(), "Loss JSON row count drifted");
    let mut json_keys = BTreeSet::new();
    for row in rows {
        let route_id = u64_field(row, "route_id");
        let detector_block_tag = u64_field(row, "detector_block_tag");
        let input_amount = string_field(row, "input_amount");
        let key = (route_id, detector_block_tag, input_amount.clone());
        assert!(json_keys.insert(key.clone()), "duplicate JSON Loss row");
        let expected_row = expected
            .get(&key)
            .unwrap_or_else(|| panic!("JSON Loss row not found in source CSV: {key:?}"));

        assert_eq!(u64_field(row, "hop_count") as u8, expected_row.hop_count);
        assert_eq!(
            byte_array_field(row, "exchange_ids"),
            expected_row.exchange_ids
        );
        assert_eq!(
            string_array_field(row, "family_path"),
            expected_row.family_path
        );
        assert_eq!(
            row.get("start_is_weth").and_then(Value::as_bool),
            Some(expected_row.start_is_weth)
        );
        assert_eq!(string_field(row, "gross_profit"), expected_row.gross_profit);
        assert_eq!(
            string_field(row, "foundry_loss_wei"),
            expected_row.foundry_loss_wei
        );
        assert_eq!(u64_field(row, "occurrences"), expected_row.occurrences);
        assert_eq!(
            u64_field(row, "success_occurrences"),
            expected_row.success_occurrences
        );
        assert_eq!(
            u64_field(row, "loss_occurrences"),
            expected_row.loss_occurrences
        );
        assert_eq!(u64_field(row, "first_success_seq"), expected_row.first_seq);
        assert_eq!(
            string_field(row, "first_balance_delta_wei"),
            expected_row.first_balance_delta_wei
        );
        assert_eq!(u64_field(row, "last_loss_seq"), expected_row.last_seq);
        assert_eq!(
            string_field(row, "last_loss_shortfall_wei"),
            expected_row.last_loss_shortfall_wei
        );
        assert_eq!(u64_field(row, "last_gas_used"), expected_row.last_gas_used);
        assert_eq!(
            string_array_field(row, "token_path"),
            expected_row.token_path
        );
        assert_eq!(string_array_field(row, "pool_path"), expected_row.pool_path);
        assert!(
            parse_i128(&expected_row.foundry_loss_wei, "foundry_loss_wei", 0) < 0,
            "fixture row should remain a Foundry Loss row until exact-state replay is proven"
        );
    }
    assert_eq!(
        json_keys,
        expected.keys().cloned().collect::<BTreeSet<_>>(),
        "Loss JSON fixture does not cover the source CSV key set"
    );

    let status_raw = read_required(
        STATE_POINT_SENSITIVE_LOSS_EXACT_STATUS_CSV,
        "state-point-sensitive exact replay status CSV",
    );
    let mut status_keys = BTreeSet::new();
    for (idx, line) in status_raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(
                line,
                "route_id,detector_block_tag,input_amount,first_seq_status,last_seq_status,foundry_exact_state_status,revm_status_match,blocker",
                "unexpected exact replay status CSV header"
            );
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields.len(), 8, "line {line_no}: malformed status row");
        let key = (
            parse_u64(fields[0], "route_id", line_no),
            parse_u64(fields[1], "detector_block_tag", line_no),
            fields[2].to_string(),
        );
        assert!(
            expected.contains_key(&key),
            "exact replay status row does not match source CSV: {key:?}"
        );
        assert!(status_keys.insert(key), "duplicate exact replay status row");
        assert_eq!(
            fields[3], "revm_success_positive_not_foundry_exact_replayed",
            "line {line_no}: first seq status must remain unresolved"
        );
        assert_eq!(
            fields[4], "revm_loss_not_foundry_exact_replayed",
            "line {line_no}: last seq status must remain unresolved"
        );
        assert_eq!(
            fields[5], "missing_standalone_replay_inputs",
            "line {line_no}: exact-state status should remain blocked"
        );
        assert_eq!(
            fields[6], "unverified",
            "line {line_no}: REVM status match must not be claimed yet"
        );
        assert!(
            fields[7].contains("missing_calldata")
                && fields[7].contains("block_env")
                && fields[7].contains("min_profit"),
            "line {line_no}: blocker should name missing replay-critical inputs"
        );
    }
    assert_eq!(
        status_keys,
        expected.keys().cloned().collect::<BTreeSet<_>>(),
        "exact replay status CSV does not cover every state-sensitive Loss row"
    );
}

#[test]
fn state_point_sensitive_loss_sequence_results_match_gate9_candidate_jsonl() {
    #[derive(Debug)]
    struct SeqResult {
        seq: u64,
        result: String,
        detail: String,
    }

    fn parse_seq_results(raw: &str, line_no: usize) -> Vec<SeqResult> {
        raw.split(';')
            .filter(|entry| !entry.trim().is_empty())
            .map(|entry| {
                let parts = entry.splitn(3, ':').collect::<Vec<_>>();
                assert_eq!(
                    parts.len(),
                    3,
                    "line {line_no}: malformed seq result entry {entry}"
                );
                SeqResult {
                    seq: parse_u64(parts[0], "seq", line_no),
                    result: parts[1].to_string(),
                    detail: parts[2].to_string(),
                }
            })
            .collect()
    }

    let gate9_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let mut gate9_by_route_input_seq = BTreeMap::<(u64, String, u64), Value>::new();
    for row in gate9_rows {
        let key = (
            u64_field(&row, "route_id"),
            string_field(&row, "input_amount"),
            candidate_seq(&row),
        );
        assert!(
            gate9_by_route_input_seq.insert(key.clone(), row).is_none(),
            "duplicate Gate9 candidate key: {key:?}"
        );
    }

    let raw = read_required(
        STATE_POINT_SENSITIVE_LOSS_CSV,
        "state-point-sensitive Loss row artifact",
    );
    let mut total_occurrences = 0usize;
    let mut total_success_occurrences = 0usize;
    let mut total_loss_occurrences = 0usize;
    let mut rows = 0usize;

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            20,
            "line {line_no}: malformed state-point-sensitive Loss row"
        );

        rows += 1;
        let route_id = parse_u64(fields[0], "route_id", line_no);
        let input_amount = fields[6].to_string();
        let occurrences = parse_u64(fields[9], "occurrences", line_no) as usize;
        let success_occurrences = parse_u64(fields[10], "success_occurrences", line_no) as usize;
        let loss_occurrences = parse_u64(fields[11], "loss_occurrences", line_no) as usize;
        let first_seq = parse_u64(fields[12], "first_seq", line_no);
        let first_balance_delta_wei = fields[13];
        let last_seq = parse_u64(fields[14], "last_seq", line_no);
        let last_loss_shortfall_wei = fields[15];
        let last_gas_used = fields[16];
        let seq_results = parse_seq_results(fields[17], line_no);
        let token_path = fields[18]
            .split('>')
            .map(str::to_string)
            .collect::<Vec<_>>();
        let pool_path = fields[19]
            .split('>')
            .map(str::to_string)
            .collect::<Vec<_>>();

        assert_eq!(
            seq_results.len(),
            occurrences,
            "line {line_no}: seq_results does not match occurrences"
        );
        assert_eq!(
            success_occurrences + loss_occurrences,
            occurrences,
            "line {line_no}: occurrence split does not add up"
        );
        assert!(
            seq_results.iter().any(|result| result.seq == first_seq),
            "line {line_no}: first_seq is not present in seq_results"
        );
        assert!(
            seq_results.iter().any(|result| result.seq == last_seq),
            "line {line_no}: last_seq is not present in seq_results"
        );

        let success_count = seq_results
            .iter()
            .filter(|result| result.result == "success")
            .count();
        let loss_count = seq_results
            .iter()
            .filter(|result| result.result == "loss")
            .count();
        assert_eq!(
            success_count, success_occurrences,
            "line {line_no}: success seq count drifted"
        );
        assert_eq!(
            loss_count, loss_occurrences,
            "line {line_no}: loss seq count drifted"
        );

        for seq_result in seq_results {
            let key = (route_id, input_amount.clone(), seq_result.seq);
            let gate9_row = gate9_by_route_input_seq
                .get(&key)
                .unwrap_or_else(|| panic!("line {line_no}: missing Gate9 candidate {key:?}"));
            assert_eq!(
                u64_field(gate9_row, "block"),
                465_686_729,
                "line {line_no}: Gate9 block drifted for {key:?}"
            );
            assert_eq!(
                string_array_field(gate9_row, "tokens"),
                token_path,
                "line {line_no}: token path drifted for {key:?}"
            );
            assert_eq!(
                string_array_field(gate9_row, "pools"),
                pool_path,
                "line {line_no}: pool path drifted for {key:?}"
            );
            assert_eq!(
                string_field(gate9_row, "revm_result"),
                seq_result.result,
                "line {line_no}: REVM result drifted for {key:?}"
            );

            match seq_result.result.as_str() {
                "success" => {
                    assert_eq!(
                        string_field(gate9_row, "balance_delta_wei"),
                        seq_result.detail,
                        "line {line_no}: success balance delta drifted for {key:?}"
                    );
                    assert_eq!(
                        string_field(gate9_row, "reject_reason"),
                        "none",
                        "line {line_no}: success row should not have reject reason for {key:?}"
                    );
                    if seq_result.seq == first_seq {
                        assert_eq!(
                            seq_result.detail, first_balance_delta_wei,
                            "line {line_no}: first success delta drifted"
                        );
                    }
                }
                "loss" => {
                    assert!(
                        gate9_row
                            .get("balance_delta_wei")
                            .is_some_and(Value::is_null),
                        "line {line_no}: loss row should not carry balance_delta_wei for {key:?}"
                    );
                    assert_eq!(
                        string_field(gate9_row, "reject_reason"),
                        seq_result.detail,
                        "line {line_no}: loss reject reason drifted for {key:?}"
                    );
                    if seq_result.seq == last_seq {
                        assert!(
                            seq_result
                                .detail
                                .contains(&format!("shortfall_wei={last_loss_shortfall_wei}")),
                            "line {line_no}: last loss shortfall drifted"
                        );
                        assert!(
                            seq_result
                                .detail
                                .contains(&format!("gas_used={last_gas_used}")),
                            "line {line_no}: last loss gas used drifted"
                        );
                    }
                }
                other => panic!("line {line_no}: unsupported seq result {other}"),
            }
        }

        total_occurrences += occurrences;
        total_success_occurrences += success_occurrences;
        total_loss_occurrences += loss_occurrences;
    }

    assert_eq!(rows, 11, "state-sensitive Loss row count drifted");
    assert_eq!(
        (
            total_occurrences,
            total_success_occurrences,
            total_loss_occurrences
        ),
        (85, 15, 70),
        "state-sensitive Loss sequence totals drifted"
    );
}

#[test]
fn strict_metadata_missing_rows_are_executor_metadata_requirements_not_profit_proof() {
    let classifications = load_token_aware_row_classification();
    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let harness_source = read_required(
        DETECTOR_VERDICT_FORK_TEST,
        "Mantis detector verdict Foundry harness",
    );
    let executor_source = read_required(MANTIS_EXECUTOR_SOL, "Mantis executor");

    let metadata_rows = classifications
        .iter()
        .filter(|row| row.blocker_class == "strict_metadata_missing")
        .collect::<Vec<_>>();
    assert_eq!(
        metadata_rows.len(),
        6,
        "strict metadata-missing row count drifted"
    );

    let metadata_route_ids = metadata_rows
        .iter()
        .map(|row| row.route_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        metadata_route_ids,
        BTreeSet::from([268u64, 3408, 6252, 6674, 15603, 16803]),
        "strict metadata-missing route set drifted"
    );

    let mut stable_revm_successes = BTreeSet::<(u64, String)>::new();
    for row in &metadata_rows {
        assert_eq!(
            row.foundry_outcome, "other_revert",
            "metadata row should be a strict Foundry mechanical blocker: {row:?}"
        );
        assert_eq!(
            row.first_revm_result, "success",
            "metadata row should be first-occurrence REVM-successful: {row:?}"
        );
        assert!(
            row.family_path.contains("UniswapV4") || row.family_path.contains("CurveStableSwapNg"),
            "metadata row must involve a family whose executor path needs fields absent from DETECT: {row:?}"
        );
        if row.last_revm_result == "success" {
            stable_revm_successes.insert((row.route_id, row.input_amount.to_string()));
        }
    }
    assert_eq!(
        stable_revm_successes,
        BTreeSet::from([
            (268u64, "73436371".to_string()),
            (6252u64, "33316121147".to_string()),
            (6674u64, "14936120".to_string()),
        ]),
        "stable REVM-success metadata route/input set drifted"
    );

    for required_harness_marker in [
        "function _needsMetadataNotPresentInDetectLine",
        "ex == UNISWAP_V4 || _isCurve(ex)",
        "function _hasMetadataLimitedHop",
        "unsupported metadata-limited family in current DETECT row",
    ] {
        assert!(
            harness_source.contains(required_harness_marker),
            "strict Foundry harness no longer documents metadata-limited classification: {required_harness_marker}"
        );
    }
    for absent_assignment in [
        "h.poolManager =",
        "h.poolId =",
        "h.tickSpacing =",
        "h.coinInIdx =",
        "h.coinOutIdx =",
    ] {
        assert!(
            !harness_source.contains(absent_assignment),
            "strict Foundry harness now assigns metadata field {absent_assignment}; update this blocker gate"
        );
    }

    for required_executor_marker in [
        "require(hops[i].coinInIdx != hops[i].coinOutIdx, \"Bad Curve coin idx\")",
        "require(hops[i].poolManager == UNISWAP_V4_POOL_MANAGER, \"Bad pool manager\")",
        "require(hops[i].poolId != bytes32(0), \"PoolId required\")",
        "require(hops[i].tickSpacing != 0, \"Tick spacing required\")",
        "require(_v4PoolId(key) == hop.poolId, \"PoolId mismatch\")",
        ".exchange(int128(uint128(hop.coinInIdx)), int128(uint128(hop.coinOutIdx)), amountIn, 0)",
    ] {
        assert!(
            executor_source.contains(required_executor_marker),
            "executor no longer exposes expected metadata requirement: {required_executor_marker}"
        );
    }

    let all_candidate_fields = b_rows
        .iter()
        .flat_map(|row| {
            row.as_object()
                .unwrap_or_else(|| panic!("candidate row is not an object: {row}"))
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    for present in [
        "route_id",
        "tokens",
        "pools",
        "input_amount",
        "gross_profit_wei",
    ] {
        assert!(
            all_candidate_fields.contains(present),
            "Gate 9 candidate artifact no longer exposes expected reconstruction field {present}"
        );
    }
    for missing_metadata_field in [
        "exchange_ids",
        "pool_manager",
        "poolManager",
        "pool_id",
        "poolId",
        "tick_spacing",
        "tickSpacing",
        "coin_in_idx",
        "coinInIdx",
        "coin_out_idx",
        "coinOutIdx",
        "exchange_metadata",
    ] {
        assert!(
            !all_candidate_fields.contains(missing_metadata_field),
            "Gate 9 candidate artifact now persists {missing_metadata_field}; update strict metadata blocker verdict"
        );
    }
}

#[test]
fn strict_metadata_replay_with_recovered_cache_metadata_is_positive_but_not_standalone() {
    let metadata_replay = read_required(
        FOUNDRY_STRICT_METADATA_REPLAY_LOG,
        "strict metadata recovered Foundry replay log",
    );

    for required in [
        "Ran 1 test for test/MantisStrictMetadataReplay.Fork.t.sol:MantisStrictMetadataReplayForkTest",
        "[PASS] test_StrictMetadataRows_WithRecoveredMetadata_Diagnostic()",
        "route3408 positive",
        "route3408 net: 10329170166289467",
        "route15603 positive",
        "route15603 net: 653517753508126",
        "route16803 positive",
        "route16803 net: 59693623844046",
        "route268 positive",
        "route268 net: 134971",
        "route6252 positive",
        "route6252 net: 48817481",
        "route6674 positive",
        "route6674 net: 30128",
        "metadata rows tested: 6",
        "metadata rows positive: 6",
        "metadata rows loss: 0",
        "metadata rows other: 0",
        "metadata rows sum net: 11042381592624219",
        "Suite result: ok. 1 passed; 0 failed",
    ] {
        assert!(
            metadata_replay.contains(required),
            "strict metadata replay log missing: {required}"
        );
    }
    assert!(
        !metadata_replay.contains("https://") && !metadata_replay.contains("arb-mainnet"),
        "strict metadata replay log should keep provider URLs redacted"
    );
}

#[test]
fn token_aware_foundry_harness_is_not_replay_sequence_state() {
    let harness_source = read_required(
        DETECTOR_VERDICT_FORK_TEST,
        "Mantis detector verdict Foundry harness",
    );
    assert!(
        harness_source.contains("--fork-block-number <head>"),
        "strict detector harness no longer documents a single fork-block run"
    );
    assert!(
        harness_source.contains("vm.envString(\"DETECT_FILE\")"),
        "strict detector harness no longer consumes DETECT_FILE"
    );
    assert!(
        !harness_source.contains("rollFork("),
        "strict detector harness now contains per-row rollFork; update the state-equivalence report"
    );

    let detect_rows = load_token_aware_detect_fixture();
    let detect_block_tags = detect_rows
        .iter()
        .map(|row| row.block_number)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        detect_block_tags,
        BTreeSet::from([22_207_819, 22_207_820, 22_207_821, 22_207_822, 22_207_844]),
        "token-aware DETECT row block tags drifted"
    );

    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let revm_blocks = b_rows
        .iter()
        .map(|row| u64_field(row, "block"))
        .collect::<BTreeSet<_>>();
    let revm_state_points = b_rows
        .iter()
        .map(|row| string_field(row, "state_point"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        revm_blocks,
        BTreeSet::from([465_686_729]),
        "Gate 9 REVM chain block drifted"
    );
    assert_eq!(
        revm_state_points,
        BTreeSet::from(["replay_sequence".to_string()]),
        "Gate 9 REVM state point is no longer replay_sequence"
    );
    assert!(
        detect_block_tags.is_disjoint(&revm_blocks),
        "DETECT row block tags unexpectedly equal Gate 9 chain block"
    );
}

#[test]
fn gate9_revm_replay_sequence_inputs_are_machine_checkable() {
    if !std::path::Path::new(GATE9_CORPUS).exists() {
        eprintln!("SKIP: artifact {GATE9_CORPUS} absent (out-of-branch)");
        return;
    }
    let script = read_required(GATE9_REVM_SCRIPT, "Gate 9 REVM equivalence script");
    for required in [
        "CORPUS=\"${CORPUS:-../anvil/corpus/blk_465686729.jsonl}\"",
        "SNAP=\"${SNAP:-465686728}\"",
        "BLOCKENV=\"${BLOCKENV:-465686729}\"",
        "REVM_VALIDATE=1",
        "FORCE_ISOLATED_REPLAY=1",
        "ACTIVE_MARK=\"REVM route validation ACTIVE\"",
        "feed_after_active",
        "CANDIDATE_RECORD_JSONL",
        "relay_sidecar --sock",
    ] {
        assert!(
            script.contains(required),
            "Gate 9 script no longer pins required exact-state input: {required}"
        );
    }

    let corpus_rows = load_jsonl(GATE9_CORPUS, "Gate 9 raw sequencer corpus");
    assert_eq!(
        corpus_rows.len(),
        29,
        "Gate 9 raw sequencer corpus row count drifted"
    );
    let corpus_sequences = corpus_rows
        .iter()
        .map(|row| {
            assert!(
                !string_field(row, "l2Msg").is_empty(),
                "raw sequencer corpus row has empty l2Msg: {row}"
            );
            u64_field(row, "sequenceNumber")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        corpus_sequences,
        (0u64..=28).collect::<BTreeSet<_>>(),
        "Gate 9 raw sequencer corpus sequence range drifted"
    );

    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    let candidate_sequences = b_rows.iter().map(candidate_seq).collect::<BTreeSet<_>>();
    assert!(
        candidate_sequences.is_subset(&corpus_sequences),
        "candidate sequence ids are not all sourced from the saved raw corpus"
    );
    assert_eq!(
        candidate_sequences,
        BTreeSet::from([
            2u64, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 27, 28,
        ]),
        "Gate 9 candidate-producing sequence set drifted"
    );

    let stdout = read_required(
        gate9_path("B_binary_stdout_revm.log"),
        "Gate 9 B binary stdout log",
    );
    for required in [
        "REVM route validation ACTIVE",
        "block=Number(465686728) block_env=Some(465686729)",
        "candidate record JSONL enabled",
        "buffered trigger-state replay queue (idle)",
        "CANDIDATE detected route_id=3146",
        "CANDIDATE detected route_id=16752",
    ] {
        assert!(
            stdout.contains(required),
            "Gate 9 B stdout log no longer proves replay-sequence boundary: {required}"
        );
    }
}

#[test]
fn plain_foundry_fork_at_gate9_block_is_not_replay_sequence_equivalent() {
    let log = read_required(
        STRICT_BLOCK_465686729_LOG,
        "plain Foundry fork block 465686729 strict detector log",
    );
    assert!(
        !log.contains("https://") && !log.contains("arb-mainnet"),
        "plain fork log should keep provider URLs redacted"
    );
    for required in [
        "[FAIL: realized net sum is not positive enough]",
        "detections tested: 29",
        "ACTUALLY net-positive (realized profit): 0",
        "net-negative / Loss: 23",
        "other reverts (mechanical): 6",
        "sum realized net (wei, all routes): -73141967294924632",
    ] {
        assert!(
            log.contains(required),
            "plain fork block 465686729 log no longer proves the expected verdict: {required}"
        );
    }
}

#[test]
fn gate9_revm_artifacts_are_non_vacuous_equivalent_and_cover_missing_families() {
    let report = read_required(
        gate9_path("equivalence_report_revm.txt"),
        "Gate 9 REVM report",
    );
    assert!(
        report.contains("EQUIVALENCE: PASS"),
        "Gate 9 REVM did not pass"
    );
    assert!(
        report.contains("field-mismatches: 0"),
        "Gate 9 REVM report has field mismatches"
    );
    assert!(
        !report.contains("candidates=0"),
        "empty candidate sets are not equivalence"
    );

    let a_rows = load_jsonl(
        gate9_path("A_file_candidates_revm.jsonl"),
        "Gate 9 A candidates",
    );
    let b_rows = load_jsonl(
        gate9_path("B_binary_candidates_revm.jsonl"),
        "Gate 9 B candidates",
    );
    assert_eq!(a_rows.len(), 246, "A raw candidate row count drifted");
    assert_eq!(b_rows.len(), 246, "B raw candidate row count drifted");

    for row in a_rows.iter().chain(b_rows.iter()) {
        assert_eq!(
            row.get("revm_enabled").and_then(Value::as_bool),
            Some(true),
            "candidate was not REVM-validated: {row}"
        );
        assert_eq!(
            string_field(row, "state_point"),
            "replay_sequence",
            "candidate is not tied to replay_sequence state"
        );
        assert_eq!(
            u64_field(row, "block"),
            465_686_729,
            "candidate block drifted"
        );
    }

    let a_first = first_occurrence_map(&a_rows);
    let b_first = first_occurrence_map(&b_rows);
    assert_eq!(a_first.len(), 49, "A unique comparison-key count drifted");
    assert_eq!(b_first.len(), 49, "B unique comparison-key count drifted");
    assert_eq!(
        a_first, b_first,
        "file and binary REVM candidate fields diverged"
    );

    let mut result_counts = BTreeMap::<String, usize>::new();
    for (_, result, _, _) in b_first.values() {
        *result_counts.entry(result.clone()).or_insert(0) += 1;
    }
    assert_eq!(result_counts.get("success"), Some(&26));
    assert_eq!(result_counts.get("loss"), Some(&18));
    assert_eq!(result_counts.get("revert"), Some(&5));

    let missing_family_routes = BTreeSet::from([3258u64, 3240, 6252]);
    for route_id in missing_family_routes {
        let row = b_rows
            .iter()
            .find(|row| u64_field(row, "route_id") == route_id)
            .unwrap_or_else(|| panic!("missing REVM row for route {route_id}"));
        assert_eq!(
            string_field(row, "revm_result"),
            "success",
            "route {route_id} is not REVM-successful"
        );
        assert_eq!(
            string_field(row, "reject_reason"),
            "none",
            "route {route_id} has reject reason"
        );
        let delta = string_field(row, "balance_delta_wei")
            .parse::<u128>()
            .unwrap_or_else(|err| panic!("route {route_id} invalid balance_delta_wei: {err}"));
        assert!(delta > 0, "route {route_id} has non-positive REVM delta");
    }
}

#[test]
fn fresh_foundry_profit_and_kernel_logs_are_non_vacuous() {
    let catalog = read_required(
        FOUNDRY_ANVIL_CATALOG_LOG,
        "fresh Foundry Anvil catalog positive replay log",
    );
    for required in [
        "Ran 2 tests for test/MantisAnvilCatalogPositiveReplay.Fork.t.sol",
        "[PASS] test_3hop_pancake_uniswap_catalog_seed_positive_realized()",
        "[PASS] test_4hop_uniswap_v3_catalog_seed_positive_realized()",
        "3hop exact-prefix realized WETH profit wei: 9319862452660",
        "4hop exact-prefix realized WETH profit wei: 529827481023789",
        "Suite result: ok. 2 passed; 0 failed",
    ] {
        assert!(
            catalog.contains(required),
            "Anvil catalog positive replay log missing: {required}"
        );
    }
    assert!(
        !catalog.contains("https://") && !catalog.contains("arb-mainnet"),
        "Anvil catalog log should keep provider URLs redacted"
    );

    let detect_execute = read_required(
        FOUNDRY_DETECT_EXECUTE_LOG,
        "fresh Foundry detect-execute profit log",
    );
    for required in [
        "[PASS] test_detect_then_execute_profit()",
        "DETECTED best size (wei): 16000000000000000000",
        "DETECTED+EXECUTED realized profit (wei): 49388179306558003",
        "FINAL realized profit at detected size (wei): 49388179306558003",
        "Suite result: ok. 1 passed; 0 failed",
    ] {
        assert!(
            detect_execute.contains(required),
            "detect-execute profit log missing: {required}"
        );
    }
    assert!(
        !detect_execute.contains("https://") && !detect_execute.contains("arb-mainnet"),
        "detect-execute log should keep provider URLs redacted"
    );

    let slippage = read_required(
        FOUNDRY_SLIPPAGE_CURVE_LOG,
        "fresh Foundry slippage curve log",
    );
    for required in [
        "[PASS] test_SlippageCurve_Isolated()",
        "profit@16000000000000000000: 49388179306558003",
        "profit@32000000000000000000: 0",
        "interior maxima detected (expect exactly 1): 1",
        "Suite result: ok. 1 passed; 0 failed",
    ] {
        assert!(
            slippage.contains(required),
            "slippage curve log missing: {required}"
        );
    }
    assert!(
        !slippage.contains("https://") && !slippage.contains("arb-mainnet"),
        "slippage log should keep provider URLs redacted"
    );

    let family = read_required(
        FOUNDRY_FAMILY_REALIZED_LOG,
        "fresh Foundry per-family realized PnL log",
    );
    for required in [
        "[PASS] test_PerFamilyRealizedPnL()",
        "FAMILIES WITH POSITIVE REALIZED TOKEN DELTA: 13",
        "FAMILIES TESTED: 16",
        "family: balancer",
        "realized USDt0 delta (raw, +=profit): 226969",
        "family: dodo",
        "realized ARB delta (raw, +=profit): 377223577784265933345",
        "family: trader_joe",
        "realized USDt0 delta (raw, +=profit): 110446809",
        "family: solidly",
        "realized none delta (raw, +=profit): 0",
        "family: sushiswap",
        "realized none delta (raw, +=profit): 0",
        "family: fluid",
        "realized none delta (raw, +=profit): 0",
        "Suite result: ok. 1 passed; 0 failed",
    ] {
        assert!(
            family.contains(required),
            "per-family realized PnL log missing: {required}"
        );
    }
    assert!(
        !family.contains("https://") && !family.contains("arb-mainnet"),
        "per-family log should keep provider URLs redacted"
    );

    let gate5 = read_required(
        FOUNDRY_GATE5_REPLACEMENT_LOG,
        "fresh Foundry Gate 5 replacement-set log",
    );
    for required in [
        "Ran 6 tests for test/MantisGate5ReplacementSet.Fork.t.sol",
        "[PASS] test_gate6_engine_emitted_route685_balance_delta()",
        "engine-emitted route685 balance-delta USDT0 raw: 2376750",
        "[PASS] test_replacement_464734243()",
        "RS 464734243 rev best profit USDT0 raw: 4221545",
        "[PASS] test_replacement_469953963()",
        "RS 469953963 rev best profit USDT0 raw: 3649437",
        "Suite result: ok. 6 passed; 0 failed",
    ] {
        assert!(
            gate5.contains(required),
            "Gate 5 replacement-set log missing: {required}"
        );
    }
    assert!(
        !gate5.contains("https://") && !gate5.contains("arb-mainnet"),
        "Gate 5 log should keep provider URLs redacted"
    );

    let c2 = read_required(FOUNDRY_C2_PREDICTOR_LOG, "fresh Foundry C2 predictor log");
    for required in [
        "[PASS] test_T2_BalancerStable_predictor_matches_executed_vault_swap()",
        "[PASS] test_T2_Curve_predictor_matches_executed_exchange()",
        "[PASS] test_T2_Dodo_predictor_matches_executed_sellBase()",
        "[PASS] test_T2_LB_predictor_matches_executed_swap()",
        "[PASS] test_T2_V2_predictor_matches_executed_swap()",
        "[PASS] test_T2_V3_predictor_matches_executed_swap()",
        "Suite result: ok. 6 passed; 0 failed",
    ] {
        assert!(
            c2.contains(required),
            "C2 predictor fork log missing: {required}"
        );
    }
    assert!(
        !c2.contains("https://") && !c2.contains("arb-mainnet"),
        "C2 predictor log should keep provider URLs redacted"
    );
}

#[test]
fn certification_correctness_tests_do_not_use_repo_markdown_as_evidence() {
    let checked = [
        "tests/active_mantis_executor_boundary_static.rs",
        "tests/aws_spot_interruption_durability_boundary_static.rs",
        "tests/certification_positive_replay_execution_boundary_static.rs",
        "tests/deployment_atomicity_dirty_tree_static.rs",
        "tests/pipeline_drift_certification_boundary_static.rs",
        "../engine-feed/tests/engine_build_blocker_static.rs",
    ];

    for rel in checked {
        let source = read_required(rel, rel);
        if !source.contains(".md") {
            continue;
        }
        assert!(
            source.contains("#[ignore = \"docs-lint"),
            "{rel} contains repository Markdown references but is not explicitly ignored as docs-lint"
        );
    }
}

#[test]
fn csv_artifacts_are_standard_width_or_declared_custom_dialects() {
    let roots = [
        Path::new("../../../Codex"),
        Path::new("../../../anvil"),
        Path::new("../../contracts/test/fixtures"),
        Path::new("../../research/strategy_executor"),
    ];
    let mut files = Vec::new();
    for root in roots {
        collect_csv_files(root, &mut files);
    }
    files.sort();

    let mut failures = Vec::new();
    for path in files {
        let raw = read_required(&path, "CSV artifact");
        let dialect = declared_csv_artifact_dialect(&path);
        let bad_rows = csv_width_failures_for_dialect(&raw, dialect);
        if !bad_rows.is_empty() {
            failures.push(format!("{} ({dialect:?}): {:?}", path.display(), bad_rows));
        }
    }

    assert!(
        failures.is_empty(),
        "CSV artifacts must be standard fixed-width CSV or explicitly declared custom dialects:\n{}",
        failures.join("\n")
    );
}
