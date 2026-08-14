use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

const OTHER_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

struct TempFixtureDir(PathBuf);

impl TempFixtureDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mantis-az-screen-v3-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("fixture dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest(window_id: &str, start_epoch: u64) -> Value {
    json!({
        "schema": "mantis.az-screen-manifest.v3",
        "screen_mode": "az_deep_dive",
        "campaign_id": "campaign-fixture",
        "window_id": window_id,
        "probe_start_epoch": start_epoch,
        "probe_window_secs": 300,
        "source_head": "1111111111111111111111111111111111111111",
        "probe_sha256": "2222222222222222222222222222222222222222222222222222222222222222",
        "config_sha256": "3333333333333333333333333333333333333333333333333333333333333333",
        "source_manifest_sha256": "4444444444444444444444444444444444444444444444444444444444444444",
        "region_universe_sha256": "5555555555555555555555555555555555555555555555555555555555555555",
        "instance_type": "c8gn.2xlarge",
        "ami_release": "al2023-fixture",
        "kernel_release": "6.1.fixture",
        "egress_class": "direct_igw_public_ipv4",
        "attempt_schedule": "equal_interval_full_window_v1",
        "timing_scope": "blocking_connect_call_only_v1",
        "probe_interval_ms": 75000,
        "connect_timeout_ms": 2000,
        "start_tolerance_ms": 250,
        "finish_grace_secs": 5,
        "wall_raw_elapsed_tolerance_ns": 250_000_000u64,
        "failure_penalty_ns": 2_000_000_000u64,
        "targets": {
            "submit": {
                "label": "raw_submit_sequencer",
                "decision_role": "submit_deciding",
                "host": "arb1-sequencer.arbitrum.io",
                "port": 443,
                "ipv4": ["198.51.100.10", "198.51.100.11"]
            },
            "feed": {
                "label": "public_feed_front_door",
                "decision_role": "diagnostic_only",
                "host": "arb1-feed.arbitrum.io",
                "port": 443,
                "ipv4": ["203.0.113.10", "203.0.113.11"]
            }
        },
        "placements": [
            {"region":"us-east-2","az_name":"us-east-2a","az_id":"use2-az1","subnet_id":"subnet-az1"},
            {"region":"us-east-2","az_name":"us-east-2b","az_id":"use2-az2","subnet_id":"subnet-az2"},
            {"region":"us-east-2","az_name":"us-east-2c","az_id":"use2-az3","subnet_id":"subnet-az3"}
        ]
    })
}

fn attempt(target: &str, ip: &str, index: u64, duration_ns: u64) -> Value {
    let start_raw_ns = 1_000_000_000_000u64 + index * 75_000_000_000;
    json!({
        "target": target,
        "target_ip": ip,
        "attempt_index": index,
        "scheduled_raw_ns": start_raw_ns,
        "started_raw_ns": start_raw_ns,
        "finished_raw_ns": start_raw_ns + duration_ns,
        "status": "ok",
        "actual_peer_ip": ip,
        "failure_reason": null,
        "observed_duration_ns": duration_ns,
        "duration_ns": duration_ns
    })
}

fn canonical_manifest_sha256(manifest: &Value) -> String {
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(
            "import hashlib,json,sys; v=json.load(sys.stdin); \
             print(hashlib.sha256(json.dumps(v,sort_keys=True,separators=(',',':'),ensure_ascii=True).encode()).hexdigest())",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn canonical digest");
    child
        .stdin
        .take()
        .expect("digest stdin")
        .write_all(&serde_json::to_vec(manifest).unwrap())
        .expect("write manifest");
    let output = child.wait_with_output().expect("digest output");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn read_artifact(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).expect("artifact JSON")
}

fn write_artifact(path: &Path, artifact: &Value) {
    fs::write(path, serde_json::to_vec_pretty(artifact).unwrap()).expect("write artifact")
}

fn refresh_manifest_content_digest(artifact: &mut Value) {
    artifact["manifest_content_sha256"] = json!(canonical_manifest_sha256(&artifact["manifest"]));
}

fn artifact_paths(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .expect("fixture entries")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".json"))
        })
        .collect();
    paths.sort();
    paths
}

fn mark_attempt_failed(attempt: &mut Value, penalty_ns: u64, reason: &str) {
    attempt["status"] = json!("failed");
    attempt["actual_peer_ip"] = Value::Null;
    attempt["failure_reason"] = json!(reason);
    attempt["duration_ns"] = json!(penalty_ns);
}

fn set_success_duration(attempt: &mut Value, duration_ns: u64) {
    let started = attempt["started_raw_ns"].as_u64().expect("started raw");
    attempt["finished_raw_ns"] = json!(started + duration_ns);
    attempt["observed_duration_ns"] = json!(duration_ns);
    attempt["duration_ns"] = json!(duration_ns);
}

fn max_attempt_finish(artifact: &Value) -> u64 {
    artifact["raw_attempts"]
        .as_array()
        .expect("raw attempts")
        .iter()
        .map(|attempt| attempt["finished_raw_ns"].as_u64().expect("finished raw"))
        .max()
        .expect("at least one raw attempt")
}

fn add_raw_finish_evidence(artifact: &mut Value, raw_finish_ns: u64, wall_finish_ns: u64) {
    let wall_start_ns = artifact["probe_actual_start_wall_ns"]
        .as_u64()
        .expect("wall start");
    let raw_start_ns = artifact["window_raw_start_ns"].as_u64().expect("raw start");
    let wall_elapsed_ns = wall_finish_ns.abs_diff(wall_start_ns);
    let raw_elapsed_ns = raw_finish_ns.abs_diff(raw_start_ns);
    artifact["probe_actual_finish_raw_ns"] = json!(raw_finish_ns);
    artifact["probe_actual_finish_wall_ns"] = json!(wall_finish_ns);
    artifact["wall_raw_elapsed_delta_ns"] = json!(wall_elapsed_ns.abs_diff(raw_elapsed_ns));
}

#[allow(clippy::too_many_arguments)]
fn write_fixture(
    dir: &Path,
    window_id: &str,
    start_epoch: u64,
    az_name: &str,
    az_id: &str,
    subnet_id: &str,
    submit_first_ns: u64,
    submit_second_ns: u64,
    feed_first_ns: u64,
    feed_second_ns: u64,
) {
    let manifest = manifest(window_id, start_epoch);
    write_fixture_from_manifest(
        dir,
        manifest,
        az_name,
        az_id,
        subnet_id,
        submit_first_ns,
        submit_second_ns,
        feed_first_ns,
        feed_second_ns,
    );
}

#[allow(clippy::too_many_arguments)]
fn write_fixture_from_manifest(
    dir: &Path,
    manifest: Value,
    az_name: &str,
    az_id: &str,
    subnet_id: &str,
    submit_first_ns: u64,
    submit_second_ns: u64,
    feed_first_ns: u64,
    feed_second_ns: u64,
) {
    let window_id = manifest["window_id"]
        .as_str()
        .expect("window id")
        .to_owned();
    let start_epoch = manifest["probe_start_epoch"].as_u64().expect("start epoch");
    let region = manifest["placements"]
        .as_array()
        .expect("placements")
        .iter()
        .find(|placement| placement["az_id"] == az_id)
        .and_then(|placement| placement["region"].as_str())
        .expect("placement region")
        .to_owned();
    let submit_ips = manifest["targets"]["submit"]["ipv4"]
        .as_array()
        .expect("submit ips");
    let feed_ips = manifest["targets"]["feed"]["ipv4"]
        .as_array()
        .expect("feed ips");
    let manifest_content_sha256 = canonical_manifest_sha256(&manifest);
    let mut raw_attempts = Vec::new();
    for index in 0..4 {
        raw_attempts.push(attempt(
            "submit",
            submit_ips[0].as_str().unwrap(),
            index,
            submit_first_ns,
        ));
        raw_attempts.push(attempt(
            "submit",
            submit_ips[1].as_str().unwrap(),
            index,
            submit_second_ns,
        ));
        raw_attempts.push(attempt(
            "feed",
            feed_ips[0].as_str().unwrap(),
            index,
            feed_first_ns,
        ));
        raw_attempts.push(attempt(
            "feed",
            feed_ips[1].as_str().unwrap(),
            index,
            feed_second_ns,
        ));
    }
    let raw_finish_ns = raw_attempts
        .iter()
        .map(|attempt| attempt["finished_raw_ns"].as_u64().unwrap())
        .max()
        .unwrap()
        + 1;
    let raw_start_ns = 1_000_000_000_000u64;
    let wall_start_ns = start_epoch * 1_000_000_000;
    let wall_finish_ns = wall_start_ns + raw_finish_ns - raw_start_ns;
    let artifact = json!({
        "schema": "mantis.az-screen.v5",
        "producer": "az-rtt-probe.sh/python-v5",
        "manifest_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "manifest_content_sha256": manifest_content_sha256,
        "script_sha256": manifest["probe_sha256"],
        "manifest": manifest,
        "probe_actual_start_wall_ns": wall_start_ns,
        "probe_start_skew_ns": 0,
        "probe_actual_finish_raw_ns": raw_finish_ns,
        "probe_actual_finish_wall_ns": wall_finish_ns,
        "wall_raw_elapsed_delta_ns": 0,
        "window_raw_start_ns": raw_start_ns,
        "window_raw_end_ns": 1_300_000_000_000u64,
        "raw_clock": "CLOCK_MONOTONIC_RAW",
        "placement_verified": true,
        "host": {
            "region": region,
            "az_name": az_name,
            "az_id": az_id,
            "subnet_id": subnet_id,
            "instance_type": manifest["instance_type"],
            "instance_id": format!("i-{az_id}"),
            "ami_id": "ami-fixture",
            "kernel_release": "6.1.fixture",
            "egress_class": "direct_igw_public_ipv4"
        },
        "metric": "fixed_peer_tcp_connect_rtt",
        "timing_scope": "blocking_connect_call_only_v1",
        "directional_claim": "none_rtt_only",
        "raw_attempts": raw_attempts
    });
    fs::write(
        dir.join(format!("{window_id}-{az_id}.json")),
        serde_json::to_vec_pretty(&artifact).unwrap(),
    )
    .expect("write fixture");
}

fn region_manifest(window_id: &str, start_epoch: u64) -> Value {
    let mut value = manifest(window_id, start_epoch);
    value["screen_mode"] = json!("region_screen");
    value["instance_type"] = json!("c7g.large");
    value["region_az_sample_count"] = json!(2);
    value["region_aggregation"] = json!("median_peers_then_median_az_v1");
    value["placements"] = json!([
        {"region":"eu-west-1","az_name":"eu-west-1b","az_id":"euw1-az2","subnet_id":"subnet-euw1"},
        {"region":"eu-west-1","az_name":"eu-west-1c","az_id":"euw1-az3","subnet_id":"subnet-euw1-2"},
        {"region":"eu-central-1","az_name":"eu-central-1c","az_id":"euc1-az3","subnet_id":"subnet-euc1"},
        {"region":"eu-central-1","az_name":"eu-central-1b","az_id":"euc1-az2","subnet_id":"subnet-euc1-2"},
        {"region":"us-west-2","az_name":"us-west-2a","az_id":"usw2-az1","subnet_id":"subnet-usw2"},
        {"region":"us-west-2","az_name":"us-west-2b","az_id":"usw2-az2","subnet_id":"subnet-usw2-2"}
    ]);
    value
}

fn region_manifest_with_placements(
    window_id: &str,
    start_epoch: u64,
    placements: Value,
    region_az_sample_count: u64,
) -> Value {
    let mut value = manifest(window_id, start_epoch);
    value["screen_mode"] = json!("region_screen");
    value["instance_type"] = json!("c7g.medium");
    value["region_az_sample_count"] = json!(region_az_sample_count);
    value["region_aggregation"] = json!("median_peers_then_median_az_v1");
    value["placements"] = placements;
    value
}

#[allow(clippy::type_complexity)]
fn write_region_matrix_window(
    dir: &Path,
    window_id: &str,
    start_epoch: u64,
    rows: &[(&str, &str, &str, &str, u64, u64, u64, u64)],
    region_az_sample_count: u64,
) {
    let placements = Value::Array(
        rows.iter()
            .map(|(region, az_name, az_id, subnet_id, ..)| {
                json!({
                    "region": region,
                    "az_name": az_name,
                    "az_id": az_id,
                    "subnet_id": subnet_id
                })
            })
            .collect(),
    );
    let frozen =
        region_manifest_with_placements(window_id, start_epoch, placements, region_az_sample_count);
    for (
        _region,
        az_name,
        az_id,
        subnet_id,
        submit_first,
        submit_second,
        feed_first,
        feed_second,
    ) in rows
    {
        write_fixture_from_manifest(
            dir,
            frozen.clone(),
            az_name,
            az_id,
            subnet_id,
            *submit_first,
            *submit_second,
            *feed_first,
            *feed_second,
        );
    }
}

#[test]
fn az_deep_dive_rejects_non_c8gn_deciding_shape() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    for entry in fs::read_dir(fixtures.path()).expect("fixtures") {
        let path = entry.expect("entry").path();
        let mut artifact: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        artifact["manifest"]["instance_type"] = json!("c7g.large");
        artifact["host"]["instance_type"] = json!("c7g.large");
        fs::write(&path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    }
    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("c8gn.2xlarge"), "stderr={stderr}");
}

fn run_aggregate_with_env(dir: &Path, env: &[(&str, &str)]) -> std::process::Output {
    let mut command = Command::new("python3");
    command
        .arg(repo_root().join("engine/deploy/aws/probe_aggregate.py"))
        .arg(dir)
        .env("PROBE_MIN_ATTEMPTS", "1")
        .env("PROBE_MAX_FAILURE_RATE", "0");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run probe aggregator")
}

fn run_aggregate(dir: &Path) -> std::process::Output {
    run_aggregate_with_env(dir, &[])
}

fn write_window(
    dir: &Path,
    window_id: &str,
    start: u64,
    submit_winner: &str,
    feed_winner: &str,
    close_second_peer_in_winner: bool,
) {
    for (az_name, az_id, subnet_id) in [
        ("us-east-2a", "use2-az1", "subnet-az1"),
        ("us-east-2b", "use2-az2", "subnet-az2"),
        ("us-east-2c", "use2-az3", "subnet-az3"),
    ] {
        let submit_first = if az_id == submit_winner {
            300_000
        } else if az_id == "use2-az2" {
            500_000
        } else {
            650_000
        };
        let feed_first = if az_id == feed_winner {
            310_000
        } else if az_id == "use2-az2" {
            520_000
        } else {
            670_000
        };
        let submit_second = if az_id == submit_winner && close_second_peer_in_winner {
            submit_first + 10_000
        } else {
            submit_first + 200_000
        };
        let feed_second = if az_id == feed_winner && close_second_peer_in_winner {
            feed_first + 10_000
        } else {
            feed_first + 200_000
        };
        write_fixture(
            dir,
            window_id,
            start,
            az_name,
            az_id,
            subnet_id,
            submit_first,
            submit_second,
            feed_first,
            feed_second,
        );
    }
}

fn write_three_windows(
    dir: &Path,
    submit_winner: &str,
    feed_winner: &str,
    close_second_peer_in_winner: bool,
) {
    for (window_id, start) in [
        ("w1", 1_700_000_000),
        ("w2", 1_700_000_900),
        ("w3", 1_700_001_800),
    ] {
        write_window(
            dir,
            window_id,
            start,
            submit_winner,
            feed_winner,
            close_second_peer_in_winner,
        );
    }
}

fn write_two_windows_only(dir: &Path, submit_winner: &str, feed_winner: &str) {
    for (window_id, start) in [("w1", 1_700_000_000), ("w2", 1_700_000_900)] {
        write_window(dir, window_id, start, submit_winner, feed_winner, false);
    }
}

#[test]
fn producer_contract_is_dual_target_raw_tcp_only() {
    let source = fs::read_to_string(repo_root().join("engine/deploy/aws/az-rtt-probe.sh"))
        .expect("read az-rtt-probe");
    for required in [
        "PROBE_MANIFEST_FILE",
        "mantis.az-screen.v5",
        "raw_submit_sequencer",
        "public_feed_front_door",
        "fixed_peer_tcp_connect_rtt",
        "CLOCK_MONOTONIC_RAW",
        "blocking_connect_call_only_v1",
        "raw_attempts",
        "c8gn.2xlarge",
        "none_rtt_only",
    ] {
        assert!(
            source.contains(required),
            "missing producer contract {required}"
        );
    }
    for forbidden in ["eth_chainId", "eth_sendRawTransaction", "warm_app_rtt"] {
        assert!(
            !source.contains(forbidden),
            "AZ screening must not use RPC method {forbidden}"
        );
    }
}

#[test]
fn legacy_v4_artifact_and_v2_manifest_are_rejected() {
    for legacy_part in ["artifact", "manifest"] {
        let fixtures = TempFixtureDir::new();
        write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
        let path = fixtures.path().join("w1-use2-az1.json");
        let mut artifact = read_artifact(&path);
        if legacy_part == "artifact" {
            artifact["schema"] = json!("mantis.az-screen.v4");
            artifact["producer"] = json!("az-rtt-probe.sh/python-v4");
        } else {
            artifact["manifest"]["schema"] = json!("mantis.az-screen-manifest.v2");
            refresh_manifest_content_digest(&mut artifact);
        }
        write_artifact(&path, &artifact);
        let output = run_aggregate(fixtures.path());
        assert!(
            !output.status.success(),
            "legacy {legacy_part} compatibility was accepted"
        );
    }
}

#[test]
fn missing_or_wrong_timing_scope_is_rejected() {
    for replacement in [None, Some("socket_lifecycle_inclusive_v0")] {
        let fixtures = TempFixtureDir::new();
        write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
        let path = fixtures.path().join("w1-use2-az1.json");
        let mut artifact = read_artifact(&path);
        match replacement {
            Some(value) => artifact["timing_scope"] = json!(value),
            None => {
                artifact
                    .as_object_mut()
                    .expect("artifact object")
                    .remove("timing_scope");
            }
        }
        write_artifact(&path, &artifact);

        let output = run_aggregate(fixtures.path());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "timing scope {replacement:?} was accepted; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(stderr.contains("timing_scope"), "stderr={stderr}");
    }
}

#[test]
fn distinct_az_comparison_ignores_second_peer_in_winning_az() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", true);
    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_PIN_READY region=us-east-2 az_id=use2-az1")
            && stdout.contains("submit_peer=198.51.100.10")
            && stdout.contains("feed_role=diagnostic_only"),
        "stdout={stdout}"
    );
    for line in stdout.lines().filter(|line| line.starts_with("COMPARE")) {
        assert!(
            line.contains("winner_az=use2-az1") && !line.contains("runner_az=use2-az1"),
            "comparison must use a distinct-AZ runner: {line}"
        );
    }
}

#[test]
fn diagnostic_feed_winner_cannot_veto_submit_pin() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az2", false);
    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_PIN_READY region=us-east-2 az_id=use2-az1")
            && stdout.contains("feed_role=diagnostic_only"),
        "stdout={stdout}"
    );
}

#[test]
fn mixed_frozen_peer_manifest_is_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az3.json");
    let mut artifact: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    artifact["manifest"]["targets"]["feed"]["ipv4"][0] = json!("203.0.113.99");
    fs::write(&path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("mixed manifest") || stderr.contains("peer set"),
        "stderr={stderr}"
    );
}

#[test]
fn cross_candidate_late_start_is_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az3.json");
    let mut artifact: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    artifact["probe_actual_start_wall_ns"] = json!(1_700_000_001_000_000_000u64);
    artifact["probe_start_skew_ns"] = json!(1_000_000_000u64);
    fs::write(&path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("start") && (stderr.contains("tolerance") || stderr.contains("spread")),
        "stderr={stderr}"
    );
}

#[test]
fn scheduled_cadence_tamper_is_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az2.json");
    let mut artifact: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let scheduled = artifact["raw_attempts"][0]["scheduled_raw_ns"]
        .as_u64()
        .unwrap();
    artifact["raw_attempts"][0]["scheduled_raw_ns"] = json!(scheduled + 1);
    fs::write(&path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("scheduled")
            || stderr.contains("cadence")
            || stderr.contains("invalid raw attempt timing"),
        "stderr={stderr}"
    );
}

#[test]
fn attempt_after_window_and_finish_overrun_are_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az2.json");
    let mut artifact: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    artifact["raw_attempts"][0]["started_raw_ns"] = json!(1_300_000_000_001u64);
    artifact["raw_attempts"][0]["finished_raw_ns"] = json!(1_300_000_300_001u64);
    artifact["raw_attempts"][0]["observed_duration_ns"] = json!(300_000u64);
    artifact["raw_attempts"][0]["duration_ns"] = json!(300_000u64);
    let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
    add_raw_finish_evidence(
        &mut artifact,
        1_300_000_300_002,
        wall_start_ns + 300_000_300_002,
    );
    fs::write(&path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("window") || stderr.contains("finish grace"),
        "stderr={stderr}"
    );
}

#[test]
fn wall_finish_195562ns_before_old_bound_is_accepted_when_raw_coverage_is_valid() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az3.json");
    let mut artifact = read_artifact(&path);
    let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
    let wall_finish_ns = wall_start_ns + 225_000_000_000 - 195_562;
    let raw_finish_ns = max_attempt_finish(&artifact) + 1;
    add_raw_finish_evidence(&mut artifact, raw_finish_ns, wall_finish_ns);
    write_artifact(&path, &artifact);

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "valid raw coverage rejected: {stderr}"
    );
}

#[test]
fn raw_finish_before_last_attempt_is_rejected_even_when_wall_finish_looks_valid() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az2.json");
    let mut artifact = read_artifact(&path);
    let raw_finish_ns = max_attempt_finish(&artifact) - 1;
    let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
    let raw_start_ns = artifact["window_raw_start_ns"].as_u64().unwrap();
    let wall_finish_ns = wall_start_ns + raw_finish_ns - raw_start_ns;
    add_raw_finish_evidence(&mut artifact, raw_finish_ns, wall_finish_ns);
    write_artifact(&path, &artifact);

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "raw finish before join coverage was accepted"
    );
    assert!(
        stderr.contains("raw finish") || stderr.contains("last attempt"),
        "stderr={stderr}"
    );
}

#[test]
fn raw_finish_beyond_grace_is_rejected_even_when_wall_metadata_is_within_deadline() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az2.json");
    let mut artifact = read_artifact(&path);
    let raw_end_ns = artifact["window_raw_end_ns"].as_u64().unwrap();
    let raw_finish_ns = raw_end_ns + 5_000_000_001;
    let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
    let wall_finish_ns = wall_start_ns + 305_000_000_000;
    add_raw_finish_evidence(&mut artifact, raw_finish_ns, wall_finish_ns);
    write_artifact(&path, &artifact);

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "raw finish grace overrun was accepted"
    );
    assert!(
        stderr.contains("raw finish") && stderr.contains("grace"),
        "stderr={stderr}"
    );
}

#[test]
fn wall_clock_step_cannot_fake_missing_final_raw_cadence_coverage() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az2.json");
    let mut artifact = read_artifact(&path);
    let raw_start_ns = artifact["window_raw_start_ns"].as_u64().unwrap();
    let raw_finish_ns = raw_start_ns + 225_000_000_000 - 100_000;
    let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
    let wall_finish_ns = wall_start_ns + 225_000_000_000 + 95_562;
    add_raw_finish_evidence(&mut artifact, raw_finish_ns, wall_finish_ns);
    write_artifact(&path, &artifact);

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "wall clock metadata faked raw coverage"
    );
    assert!(
        stderr.contains("raw finish") || stderr.contains("last attempt"),
        "stderr={stderr}"
    );
}

#[test]
fn final_attempt_schedule_tamper_is_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az2.json");
    let mut artifact = read_artifact(&path);
    let sample = artifact["raw_attempts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|sample| {
            sample["target"] == "submit"
                && sample["target_ip"] == "198.51.100.10"
                && sample["attempt_index"] == 3
        })
        .expect("last scheduled sample");
    let scheduled = sample["scheduled_raw_ns"].as_u64().unwrap();
    sample["scheduled_raw_ns"] = json!(scheduled - 1);
    write_artifact(&path, &artifact);

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("scheduled") || stderr.contains("cadence"),
        "stderr={stderr}"
    );
}

#[test]
fn arbitrary_regions_are_screened_before_any_az_pin() {
    let fixtures = TempFixtureDir::new();
    for (window_id, start) in [
        ("w1", 1_700_000_000),
        ("w2", 1_700_000_900),
        ("w3", 1_700_001_800),
    ] {
        let frozen = region_manifest(window_id, start);
        for (region, az_name, az_id, subnet_id, base) in [
            (
                "eu-west-1",
                "eu-west-1b",
                "euw1-az2",
                "subnet-euw1",
                300_000,
            ),
            (
                "eu-west-1",
                "eu-west-1c",
                "euw1-az3",
                "subnet-euw1-2",
                320_000,
            ),
            (
                "eu-central-1",
                "eu-central-1c",
                "euc1-az3",
                "subnet-euc1",
                800_000,
            ),
            (
                "eu-central-1",
                "eu-central-1b",
                "euc1-az2",
                "subnet-euc1-2",
                820_000,
            ),
            (
                "us-west-2",
                "us-west-2a",
                "usw2-az1",
                "subnet-usw2",
                1_200_000,
            ),
            (
                "us-west-2",
                "us-west-2b",
                "usw2-az2",
                "subnet-usw2-2",
                1_220_000,
            ),
        ] {
            let mut candidate = frozen.clone();
            candidate["placements"] = frozen["placements"].clone();
            let _ = region;
            write_fixture_from_manifest(
                fixtures.path(),
                candidate,
                az_name,
                az_id,
                subnet_id,
                base,
                base + 200_000,
                base + 10_000,
                base + 210_000,
            );
        }
    }
    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_REGION_READY region=eu-west-1")
            && stdout.contains("REGION_RANK")
            && stdout.contains("REGION_COMPARE")
            && !stdout.contains("SUBMIT_PIN_READY"),
        "stdout={stdout}"
    );
}

#[test]
fn region_stability_ignores_coarse_representative_az_flips() {
    let fixtures = TempFixtureDir::new();
    for (window_id, start, flip) in [
        ("w1", 1_700_000_000, false),
        ("w2", 1_700_000_900, true),
        ("w3", 1_700_001_800, false),
    ] {
        let (a1, a2) = if flip {
            (140_000, 100_000)
        } else {
            (100_000, 140_000)
        };
        write_region_matrix_window(
            fixtures.path(),
            window_id,
            start,
            &[
                (
                    "region-a",
                    "region-a-1",
                    "ra-az1",
                    "subnet-ra1",
                    a1,
                    a1 + 100_000,
                    a1 + 10_000,
                    a1 + 110_000,
                ),
                (
                    "region-a",
                    "region-a-2",
                    "ra-az2",
                    "subnet-ra2",
                    a2,
                    a2 + 100_000,
                    a2 + 10_000,
                    a2 + 110_000,
                ),
                (
                    "region-b",
                    "region-b-1",
                    "rb-az1",
                    "subnet-rb1",
                    800_000,
                    900_000,
                    810_000,
                    910_000,
                ),
                (
                    "region-b",
                    "region-b-2",
                    "rb-az2",
                    "subnet-rb2",
                    840_000,
                    940_000,
                    850_000,
                    950_000,
                ),
            ],
            2,
        );
    }

    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_REGION_READY region=region-a")
            && stdout.contains("decision_scope=submit_only")
            && stdout.contains("feed_role=diagnostic_only"),
        "stdout={stdout}"
    );
}

#[test]
fn region_stability_ignores_opportunistic_peer_flips() {
    let fixtures = TempFixtureDir::new();
    for (window_id, start, flip) in [
        ("w1", 1_700_000_000, false),
        ("w2", 1_700_000_900, false),
        ("w3", 1_700_001_800, true),
    ] {
        let (first, second) = if flip {
            (300_000, 100_000)
        } else {
            (100_000, 300_000)
        };
        write_region_matrix_window(
            fixtures.path(),
            window_id,
            start,
            &[
                (
                    "region-a",
                    "region-a-1",
                    "ra-az1",
                    "subnet-ra1",
                    first,
                    second,
                    first + 10_000,
                    second + 10_000,
                ),
                (
                    "region-a",
                    "region-a-2",
                    "ra-az2",
                    "subnet-ra2",
                    first + 20_000,
                    second + 20_000,
                    first + 30_000,
                    second + 30_000,
                ),
                (
                    "region-b",
                    "region-b-1",
                    "rb-az1",
                    "subnet-rb1",
                    800_000,
                    1_000_000,
                    810_000,
                    1_010_000,
                ),
                (
                    "region-b",
                    "region-b-2",
                    "rb-az2",
                    "subnet-rb2",
                    820_000,
                    1_020_000,
                    830_000,
                    1_030_000,
                ),
            ],
            2,
        );
    }

    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_REGION_READY region=region-a"),
        "stdout={stdout}"
    );
}

#[test]
fn region_ranking_uses_median_across_azs_instead_of_fastest_az() {
    let fixtures = TempFixtureDir::new();
    for (window_id, start) in [
        ("w1", 1_700_000_000),
        ("w2", 1_700_000_900),
        ("w3", 1_700_001_800),
    ] {
        write_region_matrix_window(
            fixtures.path(),
            window_id,
            start,
            &[
                (
                    "region-a",
                    "region-a-1",
                    "ra-az1",
                    "subnet-ra1",
                    50_000,
                    60_000,
                    55_000,
                    65_000,
                ),
                (
                    "region-a",
                    "region-a-2",
                    "ra-az2",
                    "subnet-ra2",
                    900_000,
                    910_000,
                    905_000,
                    915_000,
                ),
                (
                    "region-a",
                    "region-a-3",
                    "ra-az3",
                    "subnet-ra3",
                    920_000,
                    930_000,
                    925_000,
                    935_000,
                ),
                (
                    "region-b",
                    "region-b-1",
                    "rb-az1",
                    "subnet-rb1",
                    400_000,
                    410_000,
                    405_000,
                    415_000,
                ),
                (
                    "region-b",
                    "region-b-2",
                    "rb-az2",
                    "subnet-rb2",
                    420_000,
                    430_000,
                    425_000,
                    435_000,
                ),
                (
                    "region-b",
                    "region-b-3",
                    "rb-az3",
                    "subnet-rb3",
                    440_000,
                    450_000,
                    445_000,
                    455_000,
                ),
            ],
            3,
        );
    }

    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_REGION_READY region=region-b"),
        "stdout={stdout}"
    );
}

#[test]
fn region_screen_rejects_unequal_az_sample_counts() {
    let fixtures = TempFixtureDir::new();
    for (window_id, start) in [
        ("w1", 1_700_000_000),
        ("w2", 1_700_000_900),
        ("w3", 1_700_001_800),
    ] {
        write_region_matrix_window(
            fixtures.path(),
            window_id,
            start,
            &[
                (
                    "region-a",
                    "region-a-1",
                    "ra-az1",
                    "subnet-ra1",
                    100_000,
                    200_000,
                    110_000,
                    210_000,
                ),
                (
                    "region-a",
                    "region-a-2",
                    "ra-az2",
                    "subnet-ra2",
                    120_000,
                    220_000,
                    130_000,
                    230_000,
                ),
                (
                    "region-a",
                    "region-a-3",
                    "ra-az3",
                    "subnet-ra3",
                    140_000,
                    240_000,
                    150_000,
                    250_000,
                ),
                (
                    "region-b",
                    "region-b-1",
                    "rb-az1",
                    "subnet-rb1",
                    800_000,
                    900_000,
                    810_000,
                    910_000,
                ),
                (
                    "region-b",
                    "region-b-2",
                    "rb-az2",
                    "subnet-rb2",
                    820_000,
                    920_000,
                    830_000,
                    930_000,
                ),
            ],
            2,
        );
    }

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "unequal Stage-1 AZ sampling was accepted"
    );
    assert!(
        stderr.contains("equal-AZ") || stderr.contains("sample count"),
        "stderr={stderr}"
    );
}

#[test]
fn cloudflare_feed_diagnostic_cannot_veto_submit_region_ready() {
    let fixtures = TempFixtureDir::new();
    for (window_id, start) in [
        ("w1", 1_700_000_000),
        ("w2", 1_700_000_900),
        ("w3", 1_700_001_800),
    ] {
        write_region_matrix_window(
            fixtures.path(),
            window_id,
            start,
            &[
                (
                    "region-a",
                    "region-a-1",
                    "ra-az1",
                    "subnet-ra1",
                    100_000,
                    200_000,
                    900_000,
                    1_000_000,
                ),
                (
                    "region-a",
                    "region-a-2",
                    "ra-az2",
                    "subnet-ra2",
                    120_000,
                    220_000,
                    920_000,
                    1_020_000,
                ),
                (
                    "region-b",
                    "region-b-1",
                    "rb-az1",
                    "subnet-rb1",
                    800_000,
                    900_000,
                    110_000,
                    210_000,
                ),
                (
                    "region-b",
                    "region-b-2",
                    "rb-az2",
                    "subnet-rb2",
                    820_000,
                    920_000,
                    130_000,
                    230_000,
                ),
            ],
            2,
        );
    }

    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_REGION_READY region=region-a")
            && stdout.contains("decision_scope=submit_only")
            && stdout.contains("feed_role=diagnostic_only")
            && !stdout.contains("submit/feed region winners differ"),
        "stdout={stdout}"
    );
}

#[test]
fn unverified_placement_is_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["placement_verified"] = json!(false);
    write_artifact(&path, &artifact);

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("placement_verified"), "stderr={stderr}");
}

#[test]
fn missing_or_duplicate_placement_artifact_is_rejected() {
    let missing = TempFixtureDir::new();
    write_three_windows(missing.path(), "use2-az1", "use2-az1", false);
    fs::remove_file(missing.path().join("w1-use2-az3.json")).expect("remove fixture artifact");
    let output = run_aggregate(missing.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("placement coverage"), "stderr={stderr}");

    let duplicate = TempFixtureDir::new();
    write_three_windows(duplicate.path(), "use2-az1", "use2-az1", false);
    fs::copy(
        duplicate.path().join("w1-use2-az3.json"),
        duplicate.path().join("w1-use2-az3-duplicate.json"),
    )
    .expect("duplicate fixture artifact");
    let output = run_aggregate(duplicate.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("placement coverage"), "stderr={stderr}");
}

#[test]
fn mixed_campaign_configuration_across_windows_is_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    for path in artifact_paths(fixtures.path(), "w3-") {
        let mut artifact = read_artifact(&path);
        artifact["manifest"]["config_sha256"] = json!(OTHER_SHA256);
        refresh_manifest_content_digest(&mut artifact);
        write_artifact(&path, &artifact);
    }

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("mixed manifest") || stderr.contains("campaign provenance"),
        "stderr={stderr}"
    );
}

#[test]
fn missing_malformed_or_mixed_source_and_region_universe_provenance_is_rejected() {
    let missing = TempFixtureDir::new();
    write_three_windows(missing.path(), "use2-az1", "use2-az1", false);
    let path = missing.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["manifest"]
        .as_object_mut()
        .expect("manifest object")
        .remove("source_manifest_sha256");
    refresh_manifest_content_digest(&mut artifact);
    write_artifact(&path, &artifact);
    let output = run_aggregate(missing.path());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("source_manifest_sha256"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let malformed = TempFixtureDir::new();
    write_three_windows(malformed.path(), "use2-az1", "use2-az1", false);
    let path = malformed.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["manifest"]["region_universe_sha256"] = json!("not-a-sha");
    refresh_manifest_content_digest(&mut artifact);
    write_artifact(&path, &artifact);
    let output = run_aggregate(malformed.path());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("region_universe_sha256"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    for field in ["source_manifest_sha256", "region_universe_sha256"] {
        let mixed = TempFixtureDir::new();
        write_three_windows(mixed.path(), "use2-az1", "use2-az1", false);
        for path in artifact_paths(mixed.path(), "w3-") {
            let mut artifact = read_artifact(&path);
            artifact["manifest"][field] = json!(OTHER_SHA256);
            refresh_manifest_content_digest(&mut artifact);
            write_artifact(&path, &artifact);
        }
        let output = run_aggregate(mixed.path());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(
            stderr.contains("mixed manifest") || stderr.contains("campaign provenance"),
            "field={field} stderr={stderr}"
        );
    }
}

#[test]
fn overlapping_windows_are_rejected() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let overlapping_start = 1_700_000_100u64;
    for path in artifact_paths(fixtures.path(), "w2-") {
        let mut artifact = read_artifact(&path);
        artifact["manifest"]["probe_start_epoch"] = json!(overlapping_start);
        let wall_start_ns = overlapping_start * 1_000_000_000;
        artifact["probe_actual_start_wall_ns"] = json!(wall_start_ns);
        let raw_start_ns = artifact["window_raw_start_ns"].as_u64().unwrap();
        let raw_finish_ns = artifact["probe_actual_finish_raw_ns"].as_u64().unwrap();
        artifact["probe_actual_finish_wall_ns"] =
            json!(wall_start_ns + raw_finish_ns - raw_start_ns);
        artifact["wall_raw_elapsed_delta_ns"] = json!(0);
        refresh_manifest_content_digest(&mut artifact);
        write_artifact(&path, &artifact);
    }

    let output = run_aggregate(fixtures.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("overlap"), "stderr={stderr}");
}

#[test]
fn wrong_or_noncontiguous_attempt_counts_and_numeric_booleans_are_rejected() {
    let wrong_count = TempFixtureDir::new();
    write_three_windows(wrong_count.path(), "use2-az1", "use2-az1", false);
    let path = wrong_count.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["raw_attempts"].as_array_mut().unwrap().remove(0);
    write_artifact(&path, &artifact);
    let output = run_aggregate(wrong_count.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("attempt count") || stderr.contains("exact frozen peer set"),
        "stderr={stderr}"
    );

    let noncontiguous = TempFixtureDir::new();
    write_three_windows(noncontiguous.path(), "use2-az1", "use2-az1", false);
    let path = noncontiguous.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    let sample = artifact["raw_attempts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|sample| {
            sample["target"] == "submit"
                && sample["target_ip"] == "198.51.100.10"
                && sample["attempt_index"] == 3
        })
        .expect("sample to make noncontiguous");
    sample["attempt_index"] = json!(4);
    write_artifact(&path, &artifact);
    let output = run_aggregate(noncontiguous.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("contiguous"), "stderr={stderr}");

    let boolean = TempFixtureDir::new();
    write_three_windows(boolean.path(), "use2-az1", "use2-az1", false);
    let path = boolean.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["raw_attempts"][0]["duration_ns"] = json!(true);
    write_artifact(&path, &artifact);
    let output = run_aggregate(boolean.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("duration_ns must be an integer"),
        "stderr={stderr}"
    );
}

#[test]
fn failure_penalty_and_peer_evidence_contradictions_are_rejected() {
    let bad_penalty = TempFixtureDir::new();
    write_three_windows(bad_penalty.path(), "use2-az1", "use2-az1", false);
    let path = bad_penalty.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    let sample = &mut artifact["raw_attempts"][0];
    sample["status"] = json!("failed");
    sample["actual_peer_ip"] = Value::Null;
    sample["failure_reason"] = json!("TimeoutError");
    write_artifact(&path, &artifact);
    let output = run_aggregate(bad_penalty.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("failure penalty"), "stderr={stderr}");

    let bad_peer = TempFixtureDir::new();
    write_three_windows(bad_peer.path(), "use2-az1", "use2-az1", false);
    let path = bad_peer.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["raw_attempts"][0]["actual_peer_ip"] = json!("192.0.2.99");
    write_artifact(&path, &artifact);
    let output = run_aggregate(bad_peer.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("invalid peer"), "stderr={stderr}");
}

#[test]
fn failure_rate_above_policy_makes_the_az_target_ineligible() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    let path = fixtures.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    let penalty = artifact["manifest"]["failure_penalty_ns"].as_u64().unwrap();
    for sample in artifact["raw_attempts"].as_array_mut().unwrap() {
        if sample["target"] == "submit" && sample["attempt_index"] == 0 {
            mark_attempt_failed(sample, penalty, "TimeoutError");
        }
    }
    write_artifact(&path, &artifact);

    let output = run_aggregate_with_env(fixtures.path(), &[("PROBE_MAX_FAILURE_RATE", "0.249")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        (stderr.contains("no eligible submit peer")
            || stderr.contains("no submit peer is eligible"))
            && stderr.contains("use2-az1"),
        "stderr={stderr}"
    );
}

#[test]
fn missing_target_or_peer_samples_are_rejected() {
    let missing_target = TempFixtureDir::new();
    write_three_windows(missing_target.path(), "use2-az1", "use2-az1", false);
    let path = missing_target.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["raw_attempts"]
        .as_array_mut()
        .unwrap()
        .retain(|sample| sample["target"] != "feed");
    write_artifact(&path, &artifact);
    let output = run_aggregate(missing_target.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("exact frozen peer set"), "stderr={stderr}");

    let missing_peer = TempFixtureDir::new();
    write_three_windows(missing_peer.path(), "use2-az1", "use2-az1", false);
    let path = missing_peer.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["raw_attempts"]
        .as_array_mut()
        .unwrap()
        .retain(|sample| !(sample["target"] == "submit" && sample["target_ip"] == "198.51.100.11"));
    write_artifact(&path, &artifact);
    let output = run_aggregate(missing_peer.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("exact frozen peer set"), "stderr={stderr}");
}

#[test]
fn manifest_content_and_script_digest_mismatches_are_rejected() {
    let raw_manifest = TempFixtureDir::new();
    write_three_windows(raw_manifest.path(), "use2-az1", "use2-az1", false);
    let path = raw_manifest.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["manifest_sha256"] = json!(OTHER_SHA256);
    write_artifact(&path, &artifact);
    let output = run_aggregate(raw_manifest.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("mixed manifest"), "stderr={stderr}");

    let content = TempFixtureDir::new();
    write_three_windows(content.path(), "use2-az1", "use2-az1", false);
    let path = content.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["manifest_content_sha256"] = json!(OTHER_SHA256);
    write_artifact(&path, &artifact);
    let output = run_aggregate(content.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("manifest_content_sha256 mismatch"),
        "stderr={stderr}"
    );

    let script = TempFixtureDir::new();
    write_three_windows(script.path(), "use2-az1", "use2-az1", false);
    let path = script.path().join("w1-use2-az1.json");
    let mut artifact = read_artifact(&path);
    artifact["script_sha256"] = json!(OTHER_SHA256);
    write_artifact(&path, &artifact);
    let output = run_aggregate(script.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("script_sha256"), "stderr={stderr}");
}

#[test]
fn unstable_az_is_no_submit_pin_but_peer_flip_uses_cross_window_selection() {
    let unstable_az = TempFixtureDir::new();
    write_window(
        unstable_az.path(),
        "w1",
        1_700_000_000,
        "use2-az1",
        "use2-az1",
        false,
    );
    write_window(
        unstable_az.path(),
        "w2",
        1_700_000_900,
        "use2-az1",
        "use2-az1",
        false,
    );
    write_window(
        unstable_az.path(),
        "w3",
        1_700_001_800,
        "use2-az2",
        "use2-az2",
        false,
    );
    let output = run_aggregate(unstable_az.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("NO_SUBMIT_PIN")
            && stdout.contains("reasons=unstable_submit_az_peer_winner"),
        "stdout={stdout}"
    );

    let unstable_peer = TempFixtureDir::new();
    write_three_windows(unstable_peer.path(), "use2-az1", "use2-az1", false);
    let path = unstable_peer.path().join("w3-use2-az1.json");
    let mut artifact = read_artifact(&path);
    for sample in artifact["raw_attempts"].as_array_mut().unwrap() {
        let duration = match (sample["target"].as_str(), sample["target_ip"].as_str()) {
            (Some("submit"), Some("198.51.100.10")) => Some(400_000),
            (Some("submit"), Some("198.51.100.11")) => Some(300_000),
            (Some("feed"), Some("203.0.113.10")) => Some(510_000),
            (Some("feed"), Some("203.0.113.11")) => Some(310_000),
            _ => None,
        };
        if let Some(duration) = duration {
            set_success_duration(sample, duration);
        }
    }
    write_artifact(&path, &artifact);
    let output = run_aggregate(unstable_peer.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("SUBMIT_PIN_READY region=us-east-2 az_id=use2-az1")
            && stdout.contains("submit_peer=198.51.100.10"),
        "stdout={stdout}"
    );
}

#[test]
fn submit_peer_is_selected_by_cross_window_median_not_per_window_minimum() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);
    for (window, first, second) in [
        ("w1", 100_000, 350_000),
        ("w2", 900_000, 350_000),
        ("w3", 900_000, 350_000),
    ] {
        let path = fixtures.path().join(format!("{window}-use2-az1.json"));
        let mut artifact = read_artifact(&path);
        for sample in artifact["raw_attempts"].as_array_mut().unwrap() {
            let duration = match (sample["target"].as_str(), sample["target_ip"].as_str()) {
                (Some("submit"), Some("198.51.100.10")) => Some(first),
                (Some("submit"), Some("198.51.100.11")) => Some(second),
                _ => None,
            };
            if let Some(duration) = duration {
                set_success_duration(sample, duration);
            }
        }
        let raw_finish_ns = max_attempt_finish(&artifact) + 1;
        let raw_start_ns = artifact["window_raw_start_ns"].as_u64().unwrap();
        let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
        add_raw_finish_evidence(
            &mut artifact,
            raw_finish_ns,
            wall_start_ns + raw_finish_ns - raw_start_ns,
        );
        write_artifact(&path, &artifact);
    }

    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout
            .contains("SUBMIT_PIN_READY region=us-east-2 az_id=use2-az1 submit_peer=198.51.100.11"),
        "stdout={stdout}"
    );
    let evidence: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("SUBMIT_AZ_EVIDENCE "))
        .collect();
    assert_eq!(
        evidence.len(),
        3,
        "one evidence line is required per tested AZ"
    );
    assert!(
        evidence.iter().any(|line| {
            line.contains("az_id=use2-az1")
                && line.contains("submit_peer=198.51.100.11")
                && line.contains("windows=3")
                && line.contains("decision_scope=submit_counterpart")
        }),
        "evidence={evidence:?}"
    );
}

#[test]
fn three_windows_is_the_non_lowerable_certification_floor() {
    let two = TempFixtureDir::new();
    write_two_windows_only(two.path(), "use2-az1", "use2-az1");
    let output = run_aggregate(two.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout}");
    assert!(
        stderr.contains("at least 3 independent windows"),
        "stderr={stderr}"
    );

    let cannot_lower = TempFixtureDir::new();
    write_three_windows(cannot_lower.path(), "use2-az1", "use2-az1", false);
    let output = run_aggregate_with_env(cannot_lower.path(), &[("PROBE_MIN_WINDOWS", "2")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("PROBE_MIN_WINDOWS") && stderr.contains("3..100"),
        "stderr={stderr}"
    );

    let raised = TempFixtureDir::new();
    write_three_windows(raised.path(), "use2-az1", "use2-az1", false);
    let output = run_aggregate_with_env(raised.path(), &[("PROBE_MIN_WINDOWS", "4")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("at least 4 independent windows"),
        "stderr={stderr}"
    );
}

#[test]
fn ohio_scout_is_non_authoritative_when_canonical_replay_is_no_submit_pin() {
    let fixtures = TempFixtureDir::new();
    write_three_windows(fixtures.path(), "use2-az1", "use2-az1", false);

    // Replay the measured Ohio campaign shape. Peer selection is frozen once per
    // AZ across all windows; with that canonical rule the per-window AZ winner is
    // az2, az1, az1, so the result must not be promoted to submission authority.
    for (window, az_id, submit_first_ns, submit_second_ns) in [
        ("w1", "use2-az1", 1_365_825, 2_000_000),
        ("w2", "use2-az1", 941_273, 2_000_000),
        ("w3", "use2-az1", 1_103_305, 2_000_000),
        ("w1", "use2-az2", 893_672, 1_406_532),
        ("w2", "use2-az2", 1_323_929, 1_723_592),
        ("w3", "use2-az2", 1_342_007, 1_396_683),
        ("w1", "use2-az3", 1_514_329, 1_149_380),
        ("w2", "use2-az3", 1_512_765, 1_664_844),
        ("w3", "use2-az3", 1_478_742, 1_624_829),
    ] {
        let path = fixtures.path().join(format!("{window}-{az_id}.json"));
        let mut artifact = read_artifact(&path);
        for sample in artifact["raw_attempts"].as_array_mut().unwrap() {
            let duration_ns = match (sample["target"].as_str(), sample["target_ip"].as_str()) {
                (Some("submit"), Some("198.51.100.10")) => Some(submit_first_ns),
                (Some("submit"), Some("198.51.100.11")) => Some(submit_second_ns),
                _ => None,
            };
            if let Some(duration_ns) = duration_ns {
                set_success_duration(sample, duration_ns);
            }
        }
        let raw_finish_ns = max_attempt_finish(&artifact) + 1;
        let raw_start_ns = artifact["window_raw_start_ns"].as_u64().unwrap();
        let wall_start_ns = artifact["probe_actual_start_wall_ns"].as_u64().unwrap();
        add_raw_finish_evidence(
            &mut artifact,
            raw_finish_ns,
            wall_start_ns + raw_finish_ns - raw_start_ns,
        );
        write_artifact(&path, &artifact);
    }

    let output = run_aggregate(fixtures.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    assert!(
        stdout.contains("NO_SUBMIT_PIN")
            && stdout.contains("reasons=unstable_submit_az_peer_winner")
            && !stdout.contains("SUBMIT_PIN_READY"),
        "stdout={stdout}"
    );

    let scout =
        fs::read_to_string(repo_root().join("engine/deploy/aws/us-east-2-c8gn-console-scout.sh"))
            .expect("read Ohio console scout");
    assert!(
        scout.contains("feed_role") && scout.contains("diagnostic_only"),
        "the cheap Ohio scout must retain its diagnostic measurement label"
    );
    for required_boundary in [
        "mantis.ohio-console-scout.result.v2",
        "\"decision_role\":\"diagnostic_only\"",
        "\"placement_authority\":\"none\"",
        "\"canonical_follow_up_required\":True",
    ] {
        assert!(
            scout.contains(required_boundary),
            "Ohio diagnostic boundary lost {required_boundary}"
        );
    }
    for forbidden_authority in ["submit_authority", "submit_winner_az_id", "winner_az_id"] {
        assert!(
            !scout.contains(forbidden_authority),
            "the cheap Ohio scout cannot self-promote measured data to submission authority: {forbidden_authority}"
        );
    }
}
