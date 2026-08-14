use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde_json::Value;

const DETECT_FIXTURE: &str = "../../contracts/test/fixtures/gate9_detect_routes_from_B.txt";
const TOKEN_AWARE_STRICT_VERDICT_CSV: &str =
    "../../../Codex/TOKEN_AWARE_STRICT_DETECTOR_VERDICT_2026-06-14.csv";
const STATE_POINT_SENSITIVE_LOSS_CSV: &str =
    "../../../Codex/STATE_POINT_SENSITIVE_LOSS_ROWS_2026-06-14.csv";
const GATE9_B_CANDIDATES: &str =
    "../../../codex_tmp/gate9_revm_v3fix_20260618T2145Z/gate9/B_binary_candidates_revm.jsonl";
const FOUNDRY_STRICT_METADATA_REPLAY_LOG: &str =
    "../../../codex_tmp/foundry/strict_metadata_replay_2026-06-14.log";
const PIPELINE_RS: &str = "src/pipeline.rs";

const STRICT_DETECT_TOTAL: u64 = 29;
const STRICT_DETECT_REALIZED_POSITIVE: u64 = 12;
const STRICT_DETECT_LOSSES: u64 = 11;
const STRICT_DETECT_OTHER_REVERTS: u64 = 6;
const STRICT_DETECT_ALL_ROUTE_NET_WEI: i128 = 9_842_237_309_711_971;
const STRICT_METADATA_RECOVERED_POSITIVE: u64 = 6;
const DODO_PMM_MAX_MEASURED_GAS_USED: u64 = 933_529;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum Outcome {
    PositiveRealized,
    MechanicalOnly,
    Loss,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum RouteClass {
    SameVenue,
    CrossVenue,
    HistoricalTxReplay,
}

#[derive(Debug, Clone)]
struct Proof {
    name: &'static str,
    hop_count: u8,
    route_class: RouteClass,
    outcome: Outcome,
    realized_amount_raw: u128,
    families: &'static [&'static str],
}

#[derive(Debug)]
struct DetectRow {
    hop_count: u8,
    exchange_ids: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct StrictDetectorSummary {
    total: u64,
    positive: u64,
    losses: u64,
    other_reverts: u64,
    all_route_net_wei: i128,
}

fn proof_manifest() -> Vec<Proof> {
    vec![
        Proof {
            name: "historical_2hop_pancakev3_uniswapv3_weth_wbtc",
            hop_count: 2,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 49_388_179_306_558_003,
            families: &["PancakeV3", "UniswapV3"],
        },
        Proof {
            name: "engine_emitted_route685_fluid_uniswapv4",
            hop_count: 2,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 2_376_750,
            families: &["FluidDex", "UniswapV4"],
        },
        Proof {
            name: "historical_3hop_pancakev3_uniswapv3_weth_usdc_pendle",
            hop_count: 3,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 9_319_862_452_660,
            families: &["PancakeV3", "UniswapV3"],
        },
        Proof {
            name: "historical_4hop_uniswapv3_weth_wbtc_usdt0_usdc",
            hop_count: 4,
            route_class: RouteClass::SameVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 529_827_481_023_789,
            families: &["UniswapV3"],
        },
        Proof {
            name: "release_revm_route3258_algebrav19",
            hop_count: 3,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 10_827,
            families: &["AlgebraV19"],
        },
        Proof {
            name: "release_revm_route3240_sushiv3",
            hop_count: 3,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 11_491,
            families: &["SushiV3"],
        },
        Proof {
            name: "release_revm_route6252_curve_stableswap_ng",
            hop_count: 3,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 48_817_481,
            families: &["CurveStableSwapNg", "UniswapV3", "UniswapV4"],
        },
        Proof {
            name: "historical_real_tx_balancer",
            hop_count: 2,
            route_class: RouteClass::HistoricalTxReplay,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 226_969,
            families: &["Balancer"],
        },
        Proof {
            name: "historical_real_tx_camelot",
            hop_count: 2,
            route_class: RouteClass::HistoricalTxReplay,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 21_326_003_351_037_000_000,
            families: &["Camelot"],
        },
        Proof {
            name: "historical_real_tx_dodo",
            hop_count: 2,
            route_class: RouteClass::HistoricalTxReplay,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 377_223_577_784_265_933_345,
            families: &["DodoV2"],
        },
        Proof {
            name: "historical_real_tx_trader_joe",
            hop_count: 2,
            route_class: RouteClass::HistoricalTxReplay,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 110_446_809,
            families: &["TraderJoeLb"],
        },
        Proof {
            name: "historical_real_tx_maverick",
            hop_count: 2,
            route_class: RouteClass::HistoricalTxReplay,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 24_077_621_950,
            families: &["MaverickV2"],
        },
        Proof {
            name: "historical_real_tx_kyber",
            hop_count: 2,
            route_class: RouteClass::HistoricalTxReplay,
            outcome: Outcome::PositiveRealized,
            realized_amount_raw: 3_992_796_398_037,
            families: &["KyberSwapElastic"],
        },
        Proof {
            name: "historical_4hop_executor_path",
            hop_count: 4,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::Loss,
            realized_amount_raw: 0,
            families: &["UniswapV3", "DodoV2"],
        },
        Proof {
            name: "dodo_pmm_3hop_gas_oracle",
            hop_count: 3,
            route_class: RouteClass::CrossVenue,
            outcome: Outcome::MechanicalOnly,
            realized_amount_raw: 0,
            families: &["DodoV2", "UniswapV3", "PancakeV3", "AlgebraV19"],
        },
    ]
}

fn mantis_exchange_names() -> BTreeMap<u8, &'static str> {
    BTreeMap::from([
        (0, "UniswapV3"),
        (1, "CamelotV2"),
        (6, "PancakeV3"),
        (9, "AlgebraV19"),
        (15, "TraderJoeLb"),
        (18, "CurveStableSwapNg"),
        (20, "UniswapV4"),
        (23, "DodoV2"),
        (25, "SushiV3"),
    ])
}

fn parse_detect_fixture() -> Vec<DetectRow> {
    let raw = fs::read_to_string(Path::new(DETECT_FIXTURE)).expect("read DETECT fixture");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(fields.first(), Some(&"DETECT"), "non-DETECT fixture row");
            let hop_count = fields[5].parse::<u8>().expect("hop_count");
            assert_eq!(
                fields.len(),
                6 + hop_count as usize * 2,
                "DETECT row field count does not match hop_count"
            );
            let exchange_ids = (0..hop_count as usize)
                .map(|i| fields[6 + i * 2].parse::<u8>().expect("exchange id"))
                .collect::<Vec<_>>();
            DetectRow {
                hop_count,
                exchange_ids,
            }
        })
        .collect()
}

fn token_aware_strict_detector_summary() -> StrictDetectorSummary {
    let raw = fs::read_to_string(Path::new(TOKEN_AWARE_STRICT_VERDICT_CSV))
        .expect("read token-aware strict detector verdict CSV");
    let mut summary = StrictDetectorSummary {
        total: 0,
        positive: 0,
        losses: 0,
        other_reverts: 0,
        all_route_net_wei: 0,
    };

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(
                line,
                "route_id,block_number,hop_count,exchange_ids,start_token,end_token,input_amount,gross_profit,outcome,realized_net_wei,reason,token_path,pool_path",
                "unexpected token-aware strict detector CSV header"
            );
            continue;
        }

        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            13,
            "line {line_no}: malformed token-aware strict detector row"
        );
        assert_eq!(
            fields[4], fields[5],
            "line {line_no}: token-aware strict row is not a cycle"
        );
        summary.total += 1;
        match fields[8] {
            "positive" => summary.positive += 1,
            "loss" => summary.losses += 1,
            "other_revert" => summary.other_reverts += 1,
            other => panic!("line {line_no}: unknown strict detector outcome {other}"),
        }
        if !fields[9].is_empty() {
            summary.all_route_net_wei += fields[9]
                .parse::<i128>()
                .unwrap_or_else(|err| panic!("line {line_no}: invalid realized_net_wei: {err}"));
        }
    }

    summary
}

fn recovered_metadata_replay_is_all_positive() -> bool {
    let raw = fs::read_to_string(Path::new(FOUNDRY_STRICT_METADATA_REPLAY_LOG))
        .expect("read strict recovered-metadata Foundry replay log");
    for required in [
        "[PASS] test_StrictMetadataRows_WithRecoveredMetadata_Diagnostic()",
        "metadata rows tested: 6",
        "metadata rows positive: 6",
        "metadata rows loss: 0",
        "metadata rows other: 0",
        "metadata rows sum net: 11042381592624219",
        "Suite result: ok. 1 passed; 0 failed",
    ] {
        assert!(
            raw.contains(required),
            "strict recovered-metadata replay log missing: {required}"
        );
    }
    assert!(
        !raw.contains("https://") && !raw.contains("arb-mainnet"),
        "strict recovered-metadata replay log should keep provider URLs redacted"
    );
    true
}

fn state_point_loss_replay_note() -> String {
    let raw = fs::read_to_string(Path::new(STATE_POINT_SENSITIVE_LOSS_CSV))
        .expect("read state-point-sensitive Loss row artifact");
    let mut rows = 0u64;
    let mut total_occurrences = 0u64;
    let mut total_success_occurrences = 0u64;
    let mut total_loss_occurrences = 0u64;
    let mut hop_counts = BTreeMap::<u8, u64>::new();

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
        rows += 1;
        let hop_count = fields[2]
            .parse::<u8>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid hop_count: {err}"));
        let foundry_loss = fields[8]
            .parse::<i128>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid foundry_loss_wei: {err}"));
        let occurrences = fields[9]
            .parse::<u64>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid occurrences: {err}"));
        let success_occurrences = fields[10]
            .parse::<u64>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid success_occurrences: {err}"));
        let loss_occurrences = fields[11]
            .parse::<u64>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid loss_occurrences: {err}"));
        let first_delta = fields[13]
            .parse::<i128>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid first_balance_delta_wei: {err}"));
        let last_shortfall = fields[15]
            .parse::<u128>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid last_loss_shortfall_wei: {err}"));

        assert!(
            foundry_loss < 0,
            "line {line_no}: Foundry Loss must be negative"
        );
        assert_eq!(
            occurrences,
            success_occurrences + loss_occurrences,
            "line {line_no}: occurrence split drifted"
        );
        assert!(
            first_delta > 0,
            "line {line_no}: first REVM occurrence must be positive"
        );
        assert!(
            last_shortfall > 0,
            "line {line_no}: last REVM loss shortfall missing"
        );

        total_occurrences += occurrences;
        total_success_occurrences += success_occurrences;
        total_loss_occurrences += loss_occurrences;
        *hop_counts.entry(hop_count).or_insert(0) += 1;
    }

    assert_eq!(
        rows, STRICT_DETECT_LOSSES,
        "Loss artifact row count drifted"
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
    assert_eq!(
        hop_counts,
        BTreeMap::from([(2u8, 1u64), (3u8, 6u64), (4u8, 4u64)]),
        "state-point-sensitive Loss hop distribution drifted"
    );

    format!(
        "; {rows} Loss rows are state-point-sensitive and classified as offline/parent-pinned state artifacts, not standalone kernel failures: Gate9 replay has {total_success_occurrences} positive first/early success occurrences and {total_loss_occurrences} later loss occurrences across {total_occurrences} total occurrences; hop split={hop_counts:?}"
    )
}

fn current_dodo_detector_gas_units() -> u64 {
    let source = fs::read_to_string(PIPELINE_RS).expect("read pipeline.rs");
    let marker = "DODO/PMM 3-hop cycles measured";
    let marker_pos = source
        .find(marker)
        .expect("DODO/PMM measured-gas marker in pipeline.rs");
    let after_marker = &source[marker_pos..];
    let needle = "let gas_units: u64 =";
    let line = after_marker
        .lines()
        .find(|line| line.contains(needle))
        .expect("DODO detector gas_units line after marker");
    line.split(needle)
        .nth(1)
        .expect("gas_units rhs")
        .trim()
        .trim_end_matches(';')
        .replace('_', "")
        .parse::<u64>()
        .expect("parse DODO detector gas_units")
}

fn candidate_record_schema_blockers() -> Vec<String> {
    let raw = fs::read_to_string(Path::new(GATE9_B_CANDIDATES))
        .expect("read Gate 9 B candidate record JSONL");
    let mut fields = BTreeSet::<String>::new();
    let mut row_count = 0usize;

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        row_count += 1;
        let row = serde_json::from_str::<Value>(line).unwrap_or_else(|err| {
            panic!("candidate record line {} is invalid JSON: {err}", idx + 1)
        });
        let object = row
            .as_object()
            .unwrap_or_else(|| panic!("candidate record line {} is not an object", idx + 1));
        fields.extend(object.keys().cloned());
    }

    assert_eq!(
        row_count, 246,
        "Gate 9 B candidate record row count drifted"
    );

    let missing_standalone_replay_fields = [
        "exchange_ids",
        "transaction_index",
        "mantis_calldata",
        "executor_calldata",
        "calldata",
        "changed_slots",
        "payee",
        "mantis_executor",
        "snapshot_block",
        "block_env",
        "block_timestamp",
        "gas_limit",
        "min_profit",
        "exchange_metadata",
    ]
    .into_iter()
    .filter(|field| !fields.contains(*field))
    .collect::<Vec<_>>();

    if missing_standalone_replay_fields.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Gate 9 candidate records are not standalone Anvil/Foundry replay inputs; missing persisted fields: {missing_standalone_replay_fields:?}"
        )]
    }
}

#[test]
fn money_path_proof_manifest_is_well_formed() {
    let proofs = proof_manifest();
    assert!(!proofs.is_empty(), "money-path proof manifest is empty");

    let mut names = BTreeSet::new();
    for proof in &proofs {
        assert!(names.insert(proof.name), "duplicate proof {}", proof.name);
        assert!(proof.hop_count >= 2, "{} has invalid hop count", proof.name);
        assert!(!proof.families.is_empty(), "{} has no families", proof.name);
        if proof.outcome == Outcome::PositiveRealized {
            assert!(
                proof.realized_amount_raw > 0,
                "{} is positive but has zero realized amount",
                proof.name
            );
        }
    }

    let rows = parse_detect_fixture();
    assert!(!rows.is_empty(), "DETECT fixture is empty");

    let strict_detector = token_aware_strict_detector_summary();
    assert_eq!(
        strict_detector,
        StrictDetectorSummary {
            total: STRICT_DETECT_TOTAL,
            positive: STRICT_DETECT_REALIZED_POSITIVE,
            losses: STRICT_DETECT_LOSSES,
            other_reverts: STRICT_DETECT_OTHER_REVERTS,
            all_route_net_wei: STRICT_DETECT_ALL_ROUTE_NET_WEI,
        },
        "token-aware strict detector verdict artifact drifted"
    );
    assert!(
        recovered_metadata_replay_is_all_positive(),
        "strict metadata rows should be fork-positive when recovered metadata is supplied"
    );
    assert!(
        state_point_loss_replay_note().contains("11 Loss rows are state-point-sensitive"),
        "state-point-sensitive Loss note missing expected summary"
    );
}
