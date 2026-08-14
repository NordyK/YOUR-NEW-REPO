use std::{collections::BTreeSet, fs, path::PathBuf};

const DRIVER: &str = include_str!("../../../deploy/aws/run-all-harnesses.sh");

const EXPECTED_SUITES: &[(&str, &str)] = &[
    ("MantisAaveFundedProfit.Fork.t.sol", "465686728"),
    ("MantisAnvilCatalogPositiveReplay.Fork.t.sol", "465686728"),
    ("MantisBalancerWeighted.Fork.t.sol", "465686728"),
    ("MantisC2Predictor.Fork.t.sol", "self"),
    ("MantisCombosReplay.Fork.t.sol", "self"),
    ("MantisCurveMixedId17.Fork.t.sol", "465686728"),
    ("MantisDetectExecuteProfit.Fork.t.sol", "465686728"),
    ("MantisDetectedFreeRunReplay.Fork.t.sol", "465686728"),
    ("MantisDetectorVerdict.Fork.t.sol", "465686728"),
    ("MantisEngineCalldata.Fork.t.sol", "465686728"),
    ("MantisExecutor.Fork.t.sol", "465686728"),
    ("MantisExecutor4Hop.Fork.t.sol", "467233189"),
    ("MantisFamilyRealizedPnL.Fork.t.sol", "self"),
    ("MantisFlashMatrix.Fork.t.sol", "465686728"),
    ("MantisFlashOriginGuard.Fork.t.sol", "465686728"),
    ("MantisFunding.Fork.t.sol", "465686728"),
    ("MantisGasOracle.Fork.t.sol", "self"),
    ("MantisGate3PreState.Fork.t.sol", "465686728"),
    ("MantisGate5DodoV3ExactPrefix.Fork.t.sol", "464791929"),
    ("MantisGate5FluidV4Parent.Fork.t.sol", "461623755"),
    ("MantisGate5ReplacementSet.Fork.t.sol", "465686728"),
    ("MantisGate6ExactAmountDiagnostic.Fork.t.sol", "465686728"),
    ("MantisGate9CalldataReplay.Fork.t.sol", "465686729"),
    (
        "MantisGate9CleanSuccessCorpusReplay.Fork.t.sol",
        "465686729",
    ),
    ("MantisRealizedPnL.Fork.t.sol", "460179231"),
    ("MantisRoutes.Fork.t.sol", "460179231"),
    ("MantisRoutesAll.Fork.t.sol", "460179231"),
    ("MantisSlippageCurve.Fork.t.sol", "465686728"),
    ("MantisStaleness.Fork.t.sol", "465686728"),
    ("MantisStrictMetadataReplay.Fork.t.sol", "465686728"),
    ("MantisUniV2MixedId24.Fork.t.sol", "465686728"),
    ("SushiV3Probe.Fork.t.sol", "465686728"),
    ("V4CurveStateDivergenceAll.Fork.t.sol", "465686728"),
    ("V4StateDivergenceDiag.Fork.t.sol", "465686728"),
];

fn mapped_suites() -> BTreeSet<String> {
    DRIVER
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("forge_suite \"")?;
            Some(rest.split('"').next()?.to_owned())
        })
        .collect()
}

fn source_suites() -> BTreeSet<String> {
    let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/test");
    fs::read_dir(tests)
        .expect("read contracts/test")
        .map(|entry| entry.expect("read test entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".Fork.t.sol"))
        .collect()
}

#[test]
fn phase1_forge_scope_is_explicit_serial_and_fail_closed() {
    assert!(
        DRIVER.contains("HARNESS_SCOPE") && DRIVER.contains("forge)"),
        "run-all-harnesses.sh has no explicit HARNESS_SCOPE=forge mode"
    );
    assert!(
        DRIVER.contains("FORGE_BIN=\"${HOME}/.foundry/bin/forge\"")
            && DRIVER.contains("\"$FORGE_BIN\" test")
            && DRIVER.contains("--color never -j 1"),
        "Forge scope must resolve the requested binary and force every invocation through -j 1"
    );
    assert!(
        DRIVER.contains("command+=(--fork-url arbitrum --fork-block-number \"$state_point\")")
            && !DRIVER.contains("--fork-url \"$ARBITRUM_RPC_URL\"")
            && DRIVER.contains("| sanitize_forge_log >\"$OUT/$logfile\""),
        "Forge must resolve the configured RPC alias without putting a credential-bearing URL in argv, and sanitize provider errors before parsing or reporting logs"
    );
    assert!(
        DRIVER.contains("FORGE_EXPECTED_TESTS=161")
            && DRIVER.contains("FORGE_EXPECTED_HARNESS_EXCLUSIONS=0")
            && DRIVER.contains("FORGE_TOTAL_PASSED")
            && DRIVER.contains("FORGE_TOTAL_FAILED")
            && DRIVER.contains("FORGE_TOTAL_SKIPPED"),
        "Forge scope must aggregate exact counts and document that corrected setup has no expected reds"
    );
    for setting in [
        "DETECT_FILE=test/fixtures/detect_routes_current.txt",
        "DETECTOR_MIN_TOTAL=29",
        "DETECTOR_MIN_PROFIT=4",
        "DETECTOR_MAX_LOSS=8",
        "DETECTOR_MAX_OTHER_REVERTS=17",
        "DETECTOR_MIN_NET_SUM_WEI=1",
    ] {
        assert_eq!(
            DRIVER.matches(setting).count(),
            1,
            "{setting} must be present exactly once and scoped only to MantisDetectorVerdict"
        );
    }

    let begin = DRIVER
        .find("# BEGIN PHASE1_FORGE_SCOPE")
        .expect("missing Forge-scope safety marker");
    let end = DRIVER
        .find("# END PHASE1_FORGE_SCOPE")
        .expect("missing Forge-scope safety marker");
    let scope = &DRIVER[begin..end];
    assert!(
        !scope.contains("latest"),
        "deterministic Forge gate must not contain an unpinned latest-state branch"
    );
    // The safety scanner must name forbidden Solidity primitives in its grep pattern, and the
    // WI-00 campaign firewall must name forbidden env-var shapes in its declarative refusal
    // patterns. Exclude only those declarative guard lines when checking that the driver
    // itself never performs such actions.
    let mut in_firewall_patterns = false;
    let action_scope = scope
        .lines()
        .filter(|line| {
            if line.contains("BEGIN CAMPAIGN_FIREWALL_PATTERNS") {
                in_firewall_patterns = true;
                return false;
            }
            if line.contains("END CAMPAIGN_FIREWALL_PATTERNS") {
                in_firewall_patterns = false;
                return false;
            }
            !in_firewall_patterns && !line.contains("grep -hEq")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        scope.contains("(cd contracts && \"${launch[@]}\" \"${command[@]}\") 2>&1")
            && scope.contains("pipe_status=(\"${PIPESTATUS[@]}\")")
            && scope.contains("rc=\"${pipe_status[0]}\"")
            && !scope.contains("if ! (cd contracts"),
        "Forge command exit status must be captured from the first pipeline element before a sanitizer can mask it"
    );
    // WI-00b gaps 1.4/3.3: campaign children run under an env -i allowlist with an isolated
    // HOME and DOTENV_PATH=/dev/null (never the operator environment or ambient Foundry cache).
    assert!(
        scope.contains("env -i")
            && scope.contains("DOTENV_PATH=/dev/null")
            && scope.contains("child_home"),
        "campaign Forge children must be confined (env -i allowlist, isolated HOME, DOTENV_PATH=/dev/null)"
    );
    assert!(
        !scope.contains("--allow-failure"),
        "Forge scope must never turn failing tests into a zero exit"
    );
    for forbidden in [
        "eval ",
        "terraform ",
        "aws ",
        "ssh ",
        "scp ",
        "forge script",
        "--broadcast",
        "cast send",
        "eth_sendRawTransaction",
        "PRIVATE_KEY",
        "SIGNER_KEY",
        "writeLine",
        "writeToml",
        "copyFile",
        "createDir",
        "removeDir",
    ] {
        assert!(
            !action_scope.contains(forbidden),
            "Forge scope contains forbidden action marker {forbidden:?}"
        );
    }
}

#[test]
fn forge_manifest_has_exact_parent_pin_for_every_suite() {
    // WI-00: the closed manifest itself must carry every suite's parent pin so the offline
    // dispatcher can select the frozen Anvil state per suite without re-deriving it from the
    // invocation lines. Every entry is `<file>:<pin>` with pin numeric or `self`.
    let begin = DRIVER
        .find("FORGE_SUITES=(")
        .expect("driver lost its FORGE_SUITES manifest");
    let end = begin
        + DRIVER[begin..]
            .find(')')
            .expect("FORGE_SUITES manifest is unterminated");
    let manifest = &DRIVER[begin..end];

    let entries: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('"') || line.ends_with(".Fork.t.sol"))
        .collect();
    assert_eq!(
        entries.len(),
        EXPECTED_SUITES.len(),
        "manifest entry count drifted from the closed suite inventory"
    );
    for (suite, pin) in EXPECTED_SUITES {
        assert!(
            manifest.contains(&format!("\"{suite}:{pin}\"")),
            "manifest entry for {suite} must carry its exact parent pin {pin}"
        );
    }
    for entry in &entries {
        let trimmed = entry.trim_matches('"');
        let (_, pin) = trimmed
            .split_once(':')
            .unwrap_or_else(|| panic!("manifest entry {trimmed:?} has no parent-pin field"));
        assert!(
            pin == "self" || (!pin.is_empty() && pin.chars().all(|c| c.is_ascii_digit())),
            "manifest entry {trimmed:?} pin must be numeric or self"
        );
    }
}

#[test]
fn phase1_forge_manifest_covers_every_current_suite_with_the_pinned_mapping() {
    let expected = EXPECTED_SUITES
        .iter()
        .map(|(suite, _)| (*suite).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        source_suites(),
        expected,
        "test-source suite inventory drifted"
    );
    assert_eq!(
        mapped_suites(),
        expected,
        "Forge driver suite inventory drifted"
    );

    for (suite, block) in EXPECTED_SUITES {
        let invocation = format!("forge_suite \"{suite}\" \"{block}\"");
        assert!(
            DRIVER.contains(&invocation),
            "missing explicit block mapping: {invocation}"
        );
    }
}
