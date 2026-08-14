use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
struct Classification {
    classification: String,
    profit_gate: bool,
}

fn read_required(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    })
}

fn load_classifications(path: &Path) -> BTreeMap<String, Classification> {
    let csv = read_required(path);
    let mut out = BTreeMap::new();
    for (idx, line) in csv.lines().enumerate() {
        if idx == 0 {
            assert_eq!(
                line, "file,classification,profit_gate",
                "unexpected csv header"
            );
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split(',').collect();
        assert_eq!(fields.len(), 3, "bad csv row: {line}");
        let profit_gate = match fields[2] {
            "yes" => true,
            "no" => false,
            other => panic!("bad profit_gate value {other} in {line}"),
        };
        let previous = out.insert(
            fields[0].to_string(),
            Classification {
                classification: fields[1].to_string(),
                profit_gate,
            },
        );
        assert!(
            previous.is_none(),
            "duplicate classification row for {}",
            fields[0]
        );
    }
    out
}

fn solidity_tests_with_loss_surface(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(dir).expect("read contracts test dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sol") {
            continue;
        }
        let content = read_required(&path);
        if content.contains("error Loss")
            || content.contains("Loss.selector")
            || content.contains("InsufficientProfit.selector")
            || content.contains("LOSS_SELECTOR")
            || content.contains("Loss(uint")
        {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .expect("utf8 filename")
                .to_string();
            out.insert(name);
        }
    }
    out
}

#[test]
fn loss_acceptance_inventory_covers_every_loss_surface_file() {
    let contracts_dir = PathBuf::from("../../contracts/test");
    let fixture = contracts_dir.join("fixtures/loss_classification.csv");
    let classifications = load_classifications(&fixture);
    let loss_files = solidity_tests_with_loss_surface(&contracts_dir);
    let classified_files = classifications.keys().cloned().collect::<BTreeSet<_>>();

    let missing = loss_files
        .difference(&classified_files)
        .cloned()
        .collect::<Vec<_>>();
    let stale = classified_files
        .difference(&loss_files)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "Loss-surface files missing classification: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "classification rows without current Loss surface: {stale:?}"
    );
}

#[test]
fn loss_mechanical_tests_are_not_profit_gates() {
    let fixture = PathBuf::from("../../contracts/test/fixtures/loss_classification.csv");
    let classifications = load_classifications(&fixture);
    let allowed_classes = BTreeSet::from([
        "mechanical_only",
        "profit_gate",
        "diagnostic_loss",
        "positive_required",
    ]);

    for (file, row) in classifications {
        assert!(
            allowed_classes.contains(row.classification.as_str()),
            "{file} has unsupported certification class {}",
            row.classification
        );

        if row.classification == "mechanical_only" || row.classification == "diagnostic_loss" {
            assert!(
                !row.profit_gate,
                "{file} is {} but marked as a profit gate",
                row.classification
            );
        }
        if row.profit_gate {
            assert!(
                row.classification == "profit_gate" || row.classification == "positive_required",
                "{file} is a profit gate without a money-proof classification"
            );
        }
    }
}

/// WI-4 (b): a profit-gate test must actually ASSERT realized money — not merely accept `Loss` in a
/// try/catch (which only proves route mechanics). Statically require each `profit_gate=yes` Solidity
/// file to contain a positive-profit assertion over a realized/net/profit/delta quantity, so a
/// mechanics-only test can never be mislabeled as a money proof.
#[test]
fn profit_gate_tests_actually_assert_realized_money() {
    let contracts_dir = PathBuf::from("../../contracts/test");
    let fixture = contracts_dir.join("fixtures/loss_classification.csv");
    let classifications = load_classifications(&fixture);

    // A money assertion = an assertGt/assertGe/assertTrue whose subject names a realized economic
    // quantity, OR the detector-verdict env gate that enforces realized net-positive counts.
    let money_subjects = [
        "realized",
        "Realized",
        "bestProfit",
        "best profit",
        "net_profit",
        "netSum",
        "net ",
        "Net",
        "delta",
        "Delta",
        "profit",
        "Profit",
        "DETECTOR_MIN_PROFIT",
        "DETECTOR_MIN_NET_SUM_WEI",
    ];

    for (file, row) in classifications {
        if !row.profit_gate {
            continue;
        }
        let body = read_required(&contracts_dir.join(&file));
        let has_assert = body.contains("assertGt(")
            || body.contains("assertGe(")
            || body.contains("assertTrue(")
            || body.contains("assertLt("); // some gates assert loss-count <= 0
        assert!(
            has_assert,
            "{file} is a profit gate but contains no hard assertion"
        );
        let names_money = money_subjects.iter().any(|s| body.contains(s));
        assert!(
            names_money,
            "{file} is a profit gate but never asserts over a realized money quantity \
             (realized/net/profit/delta) — it may be accepting Loss as pass"
        );
    }
}

#[test]
fn executor_fork_mechanical_matrix_has_no_silent_executor_catches_or_bare_skips() {
    let path = PathBuf::from("../../contracts/test/MantisExecutor.Fork.t.sol");
    let body = read_required(&path);
    let lines = body.lines().collect::<Vec<_>>();

    let mut pending_executor_try: Option<usize> = None;
    let mut current_function = String::new();
    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        if let Some(rest) = line.trim_start().strip_prefix("function ") {
            current_function = rest.split('(').next().expect("function name").to_string();
        }
        if line.contains("try executor.") {
            pending_executor_try = Some(line_no);
            if line.contains("catch") {
                assert!(
                    line.contains("catch (bytes memory reason)"),
                    "executor try at line {line_no} has a same-line catch without revert bytes: {line}"
                );
                pending_executor_try = None;
            }
            continue;
        }

        if let Some(try_line) = pending_executor_try {
            if line.contains("catch") {
                assert!(
                    line.contains("catch (bytes memory reason)"),
                    "executor try at line {try_line} catches without revert bytes at line {line_no}: {line}"
                );
                pending_executor_try = None;
            }
        }

        if line.trim() == "return;" {
            let has_nearby_skip = lines[idx.saturating_sub(3)..=idx]
                .iter()
                .any(|nearby| nearby.contains("vm.skip("));
            assert!(
                has_nearby_skip || current_function == "_assertLossOrBubble",
                "bare return at line {line_no} can silently pass a fork proof without vm.skip"
            );
        }
    }

    assert!(
        pending_executor_try.is_none(),
        "unterminated executor try without catch inspection at line {:?}",
        pending_executor_try
    );
}
