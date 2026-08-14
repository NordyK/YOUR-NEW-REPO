//! Static guardrails for the current AZ-screening contract.
//!
//! Functional ranking/provenance fixtures live in `az_screen_v3.rs`.  These
//! checks deliberately prevent the retired application-probe and directional
//! terminology from returning through operator-facing entry points.

use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn read(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative)).unwrap_or_else(|error| {
        panic!("read {relative}: {error}");
    })
}

#[test]
fn producer_is_fixed_peer_dual_target_and_failure_inclusive() {
    let source = read("engine/deploy/aws/az-rtt-probe.sh");
    for token in [
        "PROBE_MANIFEST_FILE",
        "mantis.az-screen-manifest.v3",
        "mantis.az-screen.v5",
        "az-rtt-probe.sh/python-v5",
        "raw_submit_sequencer",
        "public_feed_front_door",
        "CLOCK_MONOTONIC_RAW",
        "fixed_peer_tcp_connect_rtt",
        "blocking_connect_call_only_v1",
        "failure_penalty_ns",
        "raw_attempts",
        "probe_actual_finish_raw_ns",
        "wall_raw_elapsed_delta_ns",
        "socket.AF_INET",
        "sock.connect((peer_ip, port))",
        "sock.getpeername()",
        "none_rtt_only",
    ] {
        assert!(source.contains(token), "producer contract lost {token}");
    }
    for retired in [
        "eth_chainId",
        "eth_sendRawTransaction",
        "warm_app_rtt",
        "half_rtt_proxy",
    ] {
        assert!(
            !source.contains(retired),
            "retired application/directional contract returned: {retired}"
        );
    }
}

#[test]
fn producer_never_resolves_or_sends_application_bytes_to_targets() {
    let source = read("engine/deploy/aws/az-rtt-probe.sh");
    for forbidden in [
        "getaddrinfo",
        "gethostbyname",
        "socket.create_connection",
        ".send(",
        ".sendall(",
        "ssl.wrap_socket",
    ] {
        assert!(
            !source.contains(forbidden),
            "target path must remain fixed-peer connect-only: {forbidden}"
        );
    }
}

#[test]
fn aggregator_recomputes_raw_failure_inclusive_percentiles() {
    let source = read("engine/deploy/aws/probe_aggregate.py");
    for token in [
        "raw_attempts",
        "failure_penalty_ns",
        "nearest_rank(values, 0.50)",
        "nearest_rank(values, 0.99)",
        "failures / len(samples)",
        "failed attempt must retain failure penalty",
        "attempt indices are not contiguous from zero",
        "timing_scope must be",
        "blocking_connect_call_only_v1",
    ] {
        assert!(
            source.contains(token),
            "aggregator raw-proof contract lost {token}"
        );
    }
}

#[test]
fn aggregator_selects_one_cross_window_peer_per_az_before_distinct_az_ranking() {
    let source = read("engine/deploy/aws/probe_aggregate.py");
    let per_az = source
        .find("selected_submit_peers")
        .expect("cross-window per-AZ submit peer selection");
    let ranking = source[per_az..]
        .find("fixed_rows: list[PeerRow]")
        .map(|offset| offset + per_az)
        .expect("materialize fixed-peer AZ rows");
    let compare = source[ranking..]
        .find("winner, runner = ranking[:2]")
        .map(|offset| offset + ranking)
        .expect("distinct-AZ comparison");
    assert!(per_az < ranking && ranking < compare);
    assert!(source.contains("runner is not a distinct AZ"));
}

#[test]
fn pin_is_submit_only_and_feed_is_diagnostic_non_vetoing() {
    let source = read("engine/deploy/aws/probe_aggregate.py");
    for token in [
        "\"feed\": \"diagnostic_only\"",
        "unstable_submit_az_peer_winner",
        "submit_p50_gap_below_50us_",
        "submit_p99_gap_below_100us_",
        "SUBMIT_AZ_EVIDENCE",
        "decision_scope=submit_counterpart",
        "NO_SUBMIT_PIN",
        "SUBMIT_PIN_READY",
        "submit_peer=",
        "directional_claim=none_rtt_only",
    ] {
        assert!(source.contains(token), "submit-only pin gate lost {token}");
    }
    assert!(!source.contains("submit/feed AZ winners differ"));
}

#[test]
fn region_screen_uses_equal_az_hierarchical_medians_and_typed_verdicts() {
    let source = read("engine/deploy/aws/probe_aggregate.py");
    for token in [
        "region_az_sample_count",
        "median_peers_then_median_az_v1",
        "aggregate_summaries([row.summary for row in peer_rows])",
        "SUBMIT_REGION_READY",
        "NO_SUBMIT_REGION",
        "region_universe_sha256",
    ] {
        assert!(
            source.contains(token),
            "region-screen contract lost {token}"
        );
    }
    assert!(!source.contains("NO_REGION"));
}

#[test]
fn controllers_use_account_stage_locks_bounded_aws_time_and_exhaustive_hashes() {
    for path in [
        "engine/deploy/aws/global-region-screen.sh",
        "engine/deploy/aws/az-deep-dive-campaign.sh",
    ] {
        let source = read(path);
        assert!(source.contains("$EXPECTED_ACCOUNT-$STAGE.lock"), "{path}");
        assert!(
            !source.contains("$EXPECTED_ACCOUNT-$CAMPAIGN_ID-$STAGE.lock"),
            "{path}"
        );
        for token in [
            "169.254.169.123",
            "MAX_WALL_OFFSET_NS",
            "FINAL.sha256",
            "region-offering-universe.py",
        ] {
            assert!(source.contains(token), "{path} lost {token}");
        }
    }

    let global = read("engine/deploy/aws/global-region-screen.sh");
    assert_eq!(
        global
            .matches("printf '%s\\n' \"$IDENTITY_JSON\" >\"$STATE_DIR/aws/identity.json\"")
            .count(),
        1,
        "identity evidence must be written once"
    );
    assert_eq!(
        global.matches("for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do").count(),
        1,
        "SSH readiness must have exactly one bounded 30-attempt loop"
    );
    assert!(global.contains("TARGET_REGIONS=(us-east-1 us-east-2)"));
    assert!(global.contains("EXPECTED_PLACEMENTS=4"));
    assert!(global.contains("LOCATION_CAMPAIGN_BUDGET_CAP_MILLIS=8000"));
    assert!(!global.contains("EXPECTED_PLACEMENTS=30"));
    assert!(!global.contains("EXPECTED_PLACEMENTS=48"));
}

#[test]
fn certification_window_floor_is_three_and_cannot_be_lowered() {
    let source = read("engine/deploy/aws/probe_aggregate.py");
    assert!(
        source.contains("policy_int(\"PROBE_MIN_WINDOWS\", 3, 3, 100)"),
        "three independent windows must remain the non-lowerable default"
    );
}

#[test]
fn legacy_launchers_delegate_before_any_mutation() {
    for path in [
        "engine/deploy/aws/probe_fleet.sh",
        "engine/deploy/aws/az_hunt_final.sh",
    ] {
        let source = read(path);
        let delegate = source
            .find("exec \"$SCRIPT_DIR/az-id-hunt.sh\"")
            .unwrap_or_else(|| panic!("{path} must delegate"));
        let first_mutation = [
            "aws ec2 run-instances",
            "aws ec2 terminate-instances",
            "aws ec2 authorize-security-group-ingress",
        ]
        .into_iter()
        .filter_map(|needle| source.find(needle))
        .min()
        .expect("historical mutation remains visible after delegation");
        assert!(
            delegate < first_mutation,
            "{path} mutates before delegation"
        );
    }
}
