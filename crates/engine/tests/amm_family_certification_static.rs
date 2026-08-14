use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const DETECT_FIXTURE: &str = "../../contracts/test/fixtures/gate9_detect_routes_from_B.txt";
const FAMILY_REPLAY_CANDIDATES: &str =
    "../../contracts/test/fixtures/family_strict_replay_candidates.json";
const FAMILY_BLOCKERS_CSV: &str =
    "../../../Codex/AMM_FAMILY_STRICT_COVERAGE_BLOCKERS_2026-06-14.csv";
const DEFAULT_GATE9_RUN_DIR: &str = "../../../codex_tmp/gate9_revm_preflight/gate9";
const ANVIL_CATALOG: &str = "../../../anvil/anvil_catalog.csv";
const GATE9_B_CANDIDATES: &str =
    "../../../codex_tmp/gate9_revm_preflight/gate9/B_binary_candidates_revm.jsonl";
// Tier B source: the committed full-schema candidate-record sample sliced from the regenerated
// current-source artifact (carries exchange_ids + revm_result). Stable + committed.
const TIER_B_ECONOMICS_FIXTURE: &str =
    "../../contracts/test/fixtures/candidate_records_economics_sample.jsonl";
// Tier A corpus: the curated supported-family strict subset (Solidity verdict reconstructable).
const STRICT_DETECT_FIXTURE: &str = "../../contracts/test/fixtures/detect_routes_strict.txt";

/// Tier B: exchange ids the ENGINE's in-memory REVM actually executed to a money verdict
/// (revm_result success|loss) in the regenerated candidate records. Proves the engine can validate
/// the family even when the Solidity harness cannot reconstruct its decoded/derived state.
fn engine_revm_validated_exchange_ids() -> BTreeSet<u8> {
    let mut out = BTreeSet::new();
    let Ok(raw) = fs::read_to_string(TIER_B_ECONOMICS_FIXTURE) else {
        return out;
    };
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let result = row.get("revm_result").and_then(Value::as_str).unwrap_or("");
        if result != "success" && result != "loss" {
            continue;
        }
        if let Some(ids) = row.get("exchange_ids").and_then(Value::as_array) {
            for id in ids.iter().filter_map(Value::as_u64) {
                out.insert(id as u8);
            }
        }
    }
    out
}

/// Tier C: families with no realized-net opportunity in the certification corpus, proven instead by
/// mechanical executor coverage (flashLoanArbMixed across every family, Loss-rejected) + Gate-1
/// kernel byte-exactness. Each maps to a named fork test that MUST exist in the tree.
fn tier_c_mechanical_proofs() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("trader_joe_lb", "MantisExecutor.Fork.t.sol"),
        ("camelot_v2", "MantisRoutesAll.Fork.t.sol"),
        ("uniswap_v2", "MantisExecutor.Fork.t.sol"),
        ("balancer_v2", "MantisExecutor.Fork.t.sol"),
    ])
}

#[derive(Debug, Clone, Copy)]
struct FamilyContract {
    log_family: &'static str,
    mantis_exchange_ids: &'static [u8],
    strict_fork_verdict_reconstruction: bool,
}

#[derive(Debug)]
struct FamilyBlockerRow {
    registered_pool_count: u64,
    mantis_exchange_ids: Vec<u8>,
    detect_hop_hits: usize,
    strict_fork_verdict_reconstruction: bool,
    detector_corpus_scope: String,
    blocker: String,
}

fn family_contracts() -> Vec<FamilyContract> {
    vec![
        FamilyContract {
            log_family: "uniswap_v3",
            mantis_exchange_ids: &[0],
            strict_fork_verdict_reconstruction: true,
        },
        FamilyContract {
            log_family: "sushi_v3",
            mantis_exchange_ids: &[25],
            strict_fork_verdict_reconstruction: true,
        },
        FamilyContract {
            log_family: "pancake_v3",
            mantis_exchange_ids: &[6],
            strict_fork_verdict_reconstruction: true,
        },
        FamilyContract {
            log_family: "algebra_v19",
            mantis_exchange_ids: &[9],
            strict_fork_verdict_reconstruction: true,
        },
        FamilyContract {
            log_family: "dodo_v2",
            mantis_exchange_ids: &[23],
            strict_fork_verdict_reconstruction: true,
        },
        FamilyContract {
            log_family: "curve_stableswap_ng",
            mantis_exchange_ids: &[18],
            strict_fork_verdict_reconstruction: false,
        },
        FamilyContract {
            log_family: "uniswap_v4",
            mantis_exchange_ids: &[20],
            strict_fork_verdict_reconstruction: false,
        },
        FamilyContract {
            log_family: "trader_joe_lb",
            mantis_exchange_ids: &[15],
            strict_fork_verdict_reconstruction: false,
        },
        FamilyContract {
            log_family: "camelot_v2",
            mantis_exchange_ids: &[1],
            strict_fork_verdict_reconstruction: false,
        },
        FamilyContract {
            log_family: "uniswap_v2",
            mantis_exchange_ids: &[24],
            strict_fork_verdict_reconstruction: false,
        },
        FamilyContract {
            log_family: "balancer_v2",
            mantis_exchange_ids: &[26, 27],
            strict_fork_verdict_reconstruction: false,
        },
    ]
}

fn value_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    Some(rest.split_whitespace().next().unwrap_or(rest))
}

fn parse_registered_families(log: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for line in log.lines() {
        if !line.contains("registered pools by family") {
            continue;
        }
        let Some(family) = value_after(line, "family=") else {
            continue;
        };
        let Some(count_raw) = value_after(line, "count=") else {
            continue;
        };
        let count = count_raw.parse::<u64>().unwrap_or_else(|err| {
            panic!("invalid registered family count in line {line}: {err}");
        });
        out.insert(family.to_string(), count);
    }
    out
}

fn parse_detect_ids(path: &Path) -> BTreeMap<u8, usize> {
    let raw = fs::read_to_string(path).expect("read DETECT fixture");
    let mut out = BTreeMap::<u8, usize>::new();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.first(),
            Some(&"DETECT"),
            "line {}: not DETECT",
            idx + 1
        );
        let hop_count = fields[5]
            .parse::<usize>()
            .unwrap_or_else(|err| panic!("line {}: invalid hop_count: {err}", idx + 1));
        assert!(
            (2..=4).contains(&hop_count),
            "line {}: strict verdict harness currently expects 2/3/4-hop rows, got {hop_count}",
            idx + 1
        );
        assert_eq!(
            fields.len(),
            6 + hop_count * 2,
            "line {}: field count does not match hop_count",
            idx + 1
        );
        for hop_idx in 0..hop_count {
            let exchange_id = fields[6 + hop_idx * 2]
                .parse::<u8>()
                .unwrap_or_else(|err| panic!("line {}: invalid exchange id: {err}", idx + 1));
            *out.entry(exchange_id).or_insert(0) += 1;
        }
    }
    out
}

fn parse_exchange_ids(raw: &str, line_no: usize) -> Vec<u8> {
    raw.split('+')
        .map(|value| {
            value.parse::<u8>().unwrap_or_else(|err| {
                panic!("line {line_no}: invalid Mantis exchange id {value}: {err}")
            })
        })
        .collect()
}

fn parse_bool(raw: &str, line_no: usize, field: &str) -> bool {
    match raw {
        "true" => true,
        "false" => false,
        other => panic!("line {line_no}: invalid {field} bool {other}"),
    }
}

fn load_family_blockers(path: &Path) -> BTreeMap<String, FamilyBlockerRow> {
    let raw = fs::read_to_string(path).expect("read AMM family blocker artifact");
    let mut out = BTreeMap::new();

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(
                line,
                "family,registered_pool_count,mantis_exchange_ids,detect_hop_hits,strict_fork_verdict_reconstruction,detector_corpus_scope,blocker",
                "unexpected AMM family blocker CSV header"
            );
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            7,
            "line {line_no}: malformed AMM family blocker row"
        );
        let family = fields[0].to_string();
        let row = FamilyBlockerRow {
            registered_pool_count: fields[1].parse::<u64>().unwrap_or_else(|err| {
                panic!("line {line_no}: invalid registered_pool_count: {err}")
            }),
            mantis_exchange_ids: parse_exchange_ids(fields[2], line_no),
            detect_hop_hits: fields[3]
                .parse::<usize>()
                .unwrap_or_else(|err| panic!("line {line_no}: invalid detect_hop_hits: {err}")),
            strict_fork_verdict_reconstruction: parse_bool(
                fields[4],
                line_no,
                "strict_fork_verdict_reconstruction",
            ),
            detector_corpus_scope: fields[5].to_string(),
            blocker: fields[6].to_string(),
        };
        assert!(
            out.insert(family.clone(), row).is_none(),
            "duplicate AMM family blocker row for {family}"
        );
    }

    out
}

fn read_json(path: &Path) -> Value {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read JSON {}: {err}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("invalid JSON {}: {err}", path.display()))
}

fn required_string(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("{field} must be string in {value:?}"))
        .to_string()
}

fn optional_u64(value: &Value, field: &str) -> Option<u64> {
    if value[field].is_null() {
        return None;
    }
    Some(
        value[field]
            .as_u64()
            .unwrap_or_else(|| panic!("{field} must be number or null in {value:?}")),
    )
}

fn string_array(value: &Value, field: &str) -> Vec<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be array in {value:?}"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{field} entry must be string in {value:?}"))
                .to_string()
        })
        .collect()
}

fn u8_array(value: &Value, field: &str) -> Vec<u8> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be array in {value:?}"))
        .iter()
        .map(|item| {
            item.as_u64()
                .unwrap_or_else(|| panic!("{field} entry must be number in {value:?}"))
                as u8
        })
        .collect()
}

fn catalog_contains(block: u64, tx_hash: &str, family_alias: &str) -> bool {
    let raw = fs::read_to_string(Path::new(ANVIL_CATALOG)).expect("read Anvil catalog");
    raw.lines().skip(1).any(|line| {
        let fields = line.split(',').collect::<Vec<_>>();
        fields.len() == 8
            && fields[0] == block.to_string()
            && fields[1].eq_ignore_ascii_case(tx_hash)
            && fields[3] == family_alias
    })
}

fn gate9_success_row_exists(
    route_id: u64,
    block: u64,
    token_path: &[String],
    pool_path: &[String],
) -> bool {
    let raw =
        fs::read_to_string(Path::new(GATE9_B_CANDIDATES)).expect("read Gate9 candidate JSONL");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            let row: Value = serde_json::from_str(line).expect("Gate9 candidate row JSON");
            row["route_id"].as_u64() == Some(route_id)
                && row["block"].as_u64() == Some(block)
                && row["revm_result"].as_str() == Some("success")
                && row["reject_reason"].as_str() == Some("none")
                && row["balance_delta_wei"].as_str().is_some()
                && string_array(&row, "tokens") == token_path
                && string_array(&row, "pools") == pool_path
                && row["estimated_gas_wei"].is_null()
                && row["net_profit_wei"].is_null()
        })
}

fn gate9_run_dir() -> PathBuf {
    env::var_os("GATE9_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GATE9_RUN_DIR))
}

#[test]
fn family_strict_replay_fixture_covers_blocked_families() {
    // The fixture seeds some candidates whose `source` is the out-of-branch Anvil catalog;
    // when that artifact is absent (bare checkout) `catalog_contains` would panic, so skip.
    if !Path::new(ANVIL_CATALOG).exists() {
        eprintln!("SKIP: artifact {ANVIL_CATALOG} absent (out-of-branch)");
        return;
    }
    let fixture = read_json(Path::new(FAMILY_REPLAY_CANDIDATES));
    assert_eq!(
        fixture["purpose"].as_str().is_some(),
        true,
        "family strict replay fixture needs a purpose"
    );

    let required = string_array(&fixture, "families_required")
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        required,
        BTreeSet::from([
            "BalancerStable".to_string(),
            "BalancerWeighted".to_string(),
            "CamelotV2".to_string(),
            "TraderJoeLb".to_string(),
            "UniswapV2".to_string(),
            "UniswapV4".to_string(),
            "CurveStableSwapNg".to_string(),
        ]),
        "required strict-family fixture set drifted"
    );

    let blockers = load_family_blockers(Path::new(FAMILY_BLOCKERS_CSV));
    let candidates = fixture["candidates"]
        .as_array()
        .expect("fixture candidates must be array");
    assert_eq!(
        candidates.len(),
        required.len(),
        "fixture must contain exactly one seed per required strict family"
    );

    let mut covered = BTreeSet::<String>::new();
    for candidate in candidates {
        let family = required_string(candidate, "family");
        assert!(
            required.contains(&family),
            "candidate family {family} not listed in families_required"
        );
        assert!(covered.insert(family.clone()), "duplicate family {family}");

        let registered_family = required_string(candidate, "registered_family");
        assert!(
            blockers.contains_key(&registered_family),
            "candidate {family} must map to a currently blocked registered family, got {registered_family}"
        );
        let expected_ids = blockers
            .get(&registered_family)
            .expect("blocked registered family")
            .mantis_exchange_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let candidate_ids = u8_array(candidate, "exchange_ids")
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert!(
            !candidate_ids.is_disjoint(&expected_ids),
            "candidate {family} exchange ids {candidate_ids:?} do not overlap blocked-family ids {expected_ids:?}"
        );

        let hop_count = candidate["hop_count"]
            .as_u64()
            .unwrap_or_else(|| panic!("candidate {family} hop_count must be number"));
        assert!(
            (2..=4).contains(&hop_count),
            "candidate {family} must be 2/3/4-hop seed, got {hop_count}"
        );

        let source = required_string(candidate, "source");
        let expected_replay_class = required_string(candidate, "expected_replay_class");
        let strict_replay_status = required_string(candidate, "strict_replay_status");
        assert_eq!(
            strict_replay_status, "not_strict_replayable",
            "fixture must not overclaim strict realized-net replay for {family}"
        );

        if source == "anvil/anvil_catalog.csv" {
            let block = candidate["block_number"]
                .as_u64()
                .unwrap_or_else(|| panic!("candidate {family} block_number must be number"));
            let tx_hash = candidate["tx_hash"]
                .as_str()
                .unwrap_or_else(|| panic!("candidate {family} tx_hash must be string"));
            let alias = required_string(candidate, "source_family_alias");
            assert!(
                catalog_contains(block, tx_hash, &alias),
                "candidate {family} source row not found in current Anvil catalog"
            );
            assert!(
                string_array(candidate, "pool_path").is_empty(),
                "Anvil catalog seed {family} should stay classified as missing pool_path"
            );
            assert_eq!(
                expected_replay_class, "seed_only_missing_pool_path_or_family_metadata",
                "Anvil catalog seed {family} must not be classified as replay proof"
            );
        } else if source == "codex_tmp/gate9_revm_preflight/gate9/B_binary_candidates_revm.jsonl" {
            let route_id = optional_u64(candidate, "route_id")
                .unwrap_or_else(|| panic!("Gate9 candidate {family} missing route_id"));
            let block = candidate["block_number"]
                .as_u64()
                .unwrap_or_else(|| panic!("candidate {family} block_number must be number"));
            let token_path = string_array(candidate, "token_path");
            let pool_path = string_array(candidate, "pool_path");
            assert!(
                candidate["tx_hash"].is_null(),
                "Gate9-sourced fixture {family} must expose missing tx_hash"
            );
            assert!(
                gate9_success_row_exists(route_id, block, &token_path, &pool_path),
                "Gate9 positive replay row not found for fixture candidate {family} route {route_id}"
            );
            assert_eq!(
                expected_replay_class,
                "gate9_replay_positive_but_missing_strict_metadata_or_gas_net",
                "Gate9 seed {family} must not be classified as strict realized-net proof"
            );
        } else {
            panic!("candidate {family} has unsupported source {source}");
        }

        let blocker = required_string(candidate, "blocker");
        for required_fragment in ["missing", "gas"] {
            assert!(
                blocker.to_ascii_lowercase().contains(required_fragment),
                "candidate {family} blocker should name missing evidence and gas/net gap: {blocker}"
            );
        }
    }

    assert_eq!(
        covered, required,
        "not every required strict family is covered"
    );
}

#[test]
fn amm_family_contract_table_is_unique_and_mapped() {
    let contracts = family_contracts();
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();

    for contract in contracts {
        assert!(
            names.insert(contract.log_family),
            "duplicate family contract for {}",
            contract.log_family
        );
        assert!(
            !contract.mantis_exchange_ids.is_empty(),
            "{} needs at least one Mantis exchange id",
            contract.log_family
        );
        for id in contract.mantis_exchange_ids {
            assert!(ids.insert(*id), "duplicate Mantis exchange id {id}");
        }
    }
}

#[test]
fn amm_family_blocker_artifact_matches_current_gate9_run_and_detector_fixture() {
    let contracts = family_contracts()
        .into_iter()
        .map(|contract| (contract.log_family.to_string(), contract))
        .collect::<BTreeMap<_, _>>();
    let run_dir = gate9_run_dir();
    let registered_log = run_dir.join("B_binary_stdout_revm.log");
    let registered_raw = fs::read_to_string(&registered_log).unwrap_or_else(|err| {
        panic!(
            "read registered-family log {}: {err}",
            registered_log.display()
        )
    });
    let registered = parse_registered_families(&registered_raw);
    let strict_hits = parse_detect_ids(Path::new(DETECT_FIXTURE));
    let blockers = load_family_blockers(Path::new(FAMILY_BLOCKERS_CSV));

    let mut expected_blocked_families = BTreeSet::new();
    for (family, count) in registered.iter().filter(|(_, count)| **count > 0) {
        let contract = contracts
            .get(family)
            .unwrap_or_else(|| panic!("missing family contract for registered family {family}"));
        let family_hit_count = contract
            .mantis_exchange_ids
            .iter()
            .map(|id| strict_hits.get(id).copied().unwrap_or_default())
            .sum::<usize>();
        let is_blocked = family_hit_count == 0 || !contract.strict_fork_verdict_reconstruction;

        if !is_blocked {
            continue;
        }

        expected_blocked_families.insert(family.clone());
        let row = blockers
            .get(family)
            .unwrap_or_else(|| panic!("missing AMM family blocker artifact row for {family}"));
        assert_eq!(
            row.registered_pool_count, *count,
            "registered pool count drifted for {family}"
        );
        assert_eq!(
            row.mantis_exchange_ids, contract.mantis_exchange_ids,
            "Mantis exchange id mapping drifted for {family}"
        );
        assert_eq!(
            row.detect_hop_hits, family_hit_count,
            "DETECT hit count drifted for {family}"
        );
        assert_eq!(
            row.strict_fork_verdict_reconstruction, contract.strict_fork_verdict_reconstruction,
            "strict reconstruction flag drifted for {family}"
        );

        if family_hit_count == 0 {
            assert_eq!(
                row.detector_corpus_scope, "registered_not_detected",
                "family {family} should be classified as registered but absent from strict DETECT fixture"
            );
            assert!(
                row.blocker.contains("no_strict_detect_coverage"),
                "family {family} blocker should name missing DETECT coverage"
            );
        } else {
            assert_eq!(
                row.detector_corpus_scope, "detected_unsupported",
                "family {family} should be classified as detected but unsupported by strict reconstruction"
            );
            assert!(
                !row.blocker.contains("no_strict_detect_coverage"),
                "family {family} has DETECT hits and should not be labeled missing DETECT coverage"
            );
        }

        if !contract.strict_fork_verdict_reconstruction {
            assert!(
                row.blocker.contains("strict_reconstruction_missing"),
                "family {family} blocker should name strict reconstruction gap"
            );
        }
    }

    assert_eq!(
        blockers.keys().cloned().collect::<BTreeSet<_>>(),
        expected_blocked_families,
        "AMM family blocker artifact must contain exactly the currently blocked registered families"
    );
    assert_eq!(
        expected_blocked_families,
        BTreeSet::from([
            "balancer_v2".to_string(),
            "camelot_v2".to_string(),
            "curve_stableswap_ng".to_string(),
            "trader_joe_lb".to_string(),
            "uniswap_v2".to_string(),
            "uniswap_v4".to_string(),
        ]),
        "blocked AMM family set drifted"
    );
}
