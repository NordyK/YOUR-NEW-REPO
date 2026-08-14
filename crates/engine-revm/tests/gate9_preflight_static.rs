use std::{fs, path::Path};

const SCOPE_REQUIREMENTS: &str = "../../../Codex/GATE9_REVM_EQUIVALENCE_SCOPE_2026-06-14.json";

fn read_required(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    })
}

fn assert_file_nonempty(path: &Path, label: &str) {
    let meta = fs::metadata(path).unwrap_or_else(|err| {
        panic!("{label} missing at {}: {err}", path.display());
    });
    assert!(meta.len() > 0, "{label} is empty at {}", path.display());
}

fn assert_contains_all(haystack: &str, needles: &[&str], label: &str) {
    for needle in needles {
        assert!(haystack.contains(needle), "{label} missing: {needle}");
    }
}

#[test]
fn gate9_revm_harness_keeps_non_vacuous_safeguards() {
    let script = read_required(Path::new("../../scripts/gate9_equivalence_revm.sh"));

    for required in [
        "REVM_VALIDATE=1",
        "FORCE_ISOLATED_REPLAY=1",
        "ACTIVE_MARK=\"REVM route validation ACTIVE\"",
        "INVALID: Run A produced 0 candidates",
        "INVALID: Run B produced 0 candidates",
        "field-mismatches",
        "EQUIVALENCE:",
        "empty candidate set(s) — empty==empty is not equivalence",
    ] {
        assert!(
            script.contains(required),
            "gate9 REVM harness missing non-vacuous safeguard: {required}"
        );
    }
}

#[test]
fn gate9_revm_equivalence_scope_is_transport_only_not_standalone_replay() {
    let scope = read_required(Path::new(SCOPE_REQUIREMENTS));
    let script = read_required(Path::new("../../scripts/gate9_equivalence_revm.sh"));
    let report = read_required(Path::new(
        "../../../codex_tmp/gate9_revm_preflight/gate9/equivalence_report_revm.txt",
    ));

    assert_contains_all(
        &scope,
        &[
            "same_corpus_file_transport_vs_binunix_transport_candidate_set_equivalence",
            "nonempty_candidate_sets",
            "zero_candidate_key_differences",
            "zero_compared_field_mismatches",
            "REVM_VALIDATE_enabled",
            "FORCE_ISOLATED_REPLAY_enabled",
            "same_snapshot_block",
            "same_block_env",
        ],
        "Gate9 equivalence scope current proof list",
    );
    assert_contains_all(
        &scope,
        &[
            "standalone_anvil_foundry_replay",
            "foundry_rollFork_txHash_prefix_state",
            "foundry_vm_transact_historical_transaction_replay",
            "profit_after_arbitrum_gas",
            "candidate_calldata_reconstructability",
            "changed_slot_reconstructability",
            "live_public_feed_freshness",
            "all_registered_amm_family_realized_net_coverage",
        ],
        "Gate9 equivalence scope non-proof list",
    );
    assert_contains_all(
        &scope,
        &[
            "tx_hash",
            "transaction_index",
            "mantis_calldata",
            "executor_calldata",
            "changed_slots",
            "exchange_ids",
            "exchange_metadata",
            "gas_limit",
            "min_profit",
            "mantis_executor",
            "payee",
        ],
        "Gate9 standalone/live-fork missing field list",
    );
    assert_contains_all(
        &scope,
        &[
            "use_absolute_path",
            "passes_with_absolute_GATE9_RUN_DIR",
            "transport_equivalence_only",
            "rolls fork to transaction block and replays prior transactions",
            "fetches and executes a transaction from a fork",
        ],
        "Gate9 equivalence path/documentation policy",
    );

    for required_fragment in [
        "REVM_VALIDATE=1",
        "FORCE_ISOLATED_REPLAY=1",
        "ACTIVE_MARK=\"REVM route validation ACTIVE\"",
        "INVALID: Run A produced 0 candidates",
        "INVALID: Run B produced 0 candidates",
        "empty candidate set(s) — empty==empty is not equivalence",
        "field-mismatches",
        "EQUIVALENCE:",
    ] {
        assert!(
            script.contains(required_fragment),
            "Gate9 harness missing scope-required fragment: {required_fragment}"
        );
    }
    for required_fragment in [
        "A(file) candidates=49  B(binary) candidates=49",
        "keys only in A: 0  only in B: 0",
        "common keys: 49  field-mismatches: 0",
        "EQUIVALENCE: PASS",
    ] {
        assert!(
            report.contains(required_fragment),
            "Gate9 equivalence report missing scope-required fragment: {required_fragment}"
        );
    }
}
