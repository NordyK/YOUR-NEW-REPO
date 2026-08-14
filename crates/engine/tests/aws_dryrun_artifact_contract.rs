use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use alloy_primitives::keccak256;
use serde_json::Value;

const ENV_REDACTED: &str = "env.redacted";
const BINARY_SHA256: &str = "binary.sha256";
const RUN_METADATA: &str = "run_metadata.json";
const ARTIFACT_MANIFEST: &str = "artifact_manifest.json";
const RAW_FEED: &str = "raw_feed.jsonl";
const FRESHNESS: &str = "raw_feed.jsonl.freshness.jsonl";
const FRESHNESS_REPORT: &str = "feed_candidate_freshness_report.json";
const CANDIDATES: &str = "candidates.jsonl";
const PAPER: &str = "paper.jsonl";
const GAS_MATRIX: &str = "nodeinterface_gas_matrix.json";
const LIVE_PARITY: &str = "live_parity.json";
const RUN_IDENTITY: &str = "run_identity.json";
const DEPLOYMENT_MANIFEST: &str = "deployment_manifest.json";
const PLACEMENT_PROOF: &str = "placement_proof.json";
const PARITY_STATE_PIN: &str = "fastlane_parity_state_pin.json";
const PARITY_SOURCE_REPORT: &str = "parity_source_report.json";
const FASTLANE_PARITY_CORPUS: &str = "fastlane_parity_corpus.jsonl";
const FASTLANE_PARITY_TRANSCRIPT: &str = "fastlane_parity_transcript.jsonl";
const FASTLANE_PARITY_REPORT: &str = "fastlane_parity_report.json";
const FASTLANE_PARITY_MANIFEST: &str = "fastlane_parity_manifest.json";
const MONITORING_SUMMARY: &str = "monitoring_summary.json";
const ENGINE_LOG: &str = "engine.log";
const HOST_TELEMETRY: &str = "host_telemetry.jsonl";
const HOST_TELEMETRY_SUMMARY: &str = "host_telemetry_summary.json";
const SIDECAR_STDOUT: &str = "sidecar.stdout.log";
const SIDECAR_STDERR: &str = "sidecar.stderr.log";

fn read(path: &Path, label: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {label} at {}: {err}", path.display()))
}

fn read_json(path: &Path, label: &str) -> Value {
    serde_json::from_str(&read(path, label))
        .unwrap_or_else(|err| panic!("{label} is invalid JSON at {}: {err}", path.display()))
}

fn read_jsonl(path: &Path, label: &str) -> Vec<Value> {
    let raw = read(path, label);
    let rows = raw
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line).unwrap_or_else(|err| {
                panic!(
                    "{label} line {} is invalid JSON at {}: {err}",
                    idx + 1,
                    path.display()
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(!rows.is_empty(), "{label} must not be empty");
    rows
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|err| panic!("failed to run sha256sum for {}: {err}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha256sum stdout must be utf8")
        .split_whitespace()
        .next()
        .expect("sha256sum output missing hash")
        .to_string()
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn sha256sum");
    child
        .stdin
        .take()
        .expect("sha256sum stdin")
        .write_all(bytes)
        .expect("write sha256sum stdin");
    let output = child.wait_with_output().expect("wait for sha256sum");
    assert!(output.status.success(), "sha256sum over bytes failed");
    String::from_utf8(output.stdout)
        .expect("sha256sum stdout utf8")
        .split_whitespace()
        .next()
        .expect("sha256sum output missing hash")
        .to_string()
}

fn keccak_file(path: &Path) -> String {
    format!(
        "{:#x}",
        keccak256(fs::read(path).unwrap_or_else(|err| {
            panic!("failed to read Keccak input {}: {err}", path.display())
        }))
    )
}

fn unprefixed_keccak_file(path: &Path) -> String {
    keccak_file(path)
        .strip_prefix("0x")
        .expect("keccak_file always returns a 0x-prefixed digest")
        .to_string()
}

fn assert_sha256_digest(hash: &str, label: &str) {
    assert!(
        hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit()),
        "{label} must contain one 64-hex SHA256 digest"
    );
}

fn assert_keccak_digest(hash: &str, label: &str) {
    assert!(
        hash.len() == 66
            && hash.starts_with("0x")
            && hash[2..].chars().all(|ch| ch.is_ascii_hexdigit()),
        "{label} must contain one 32-byte 0x Keccak digest"
    );
}

fn assert_unprefixed_keccak_digest(hash: &str, label: &str) {
    assert!(
        hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit()),
        "{label} must contain one unprefixed 32-byte Keccak digest"
    );
}

fn assert_address(value: &str, label: &str) {
    assert!(
        value.len() == 42
            && value.starts_with("0x")
            && value[2..].chars().all(|ch| ch.is_ascii_hexdigit()),
        "{label} must contain one 20-byte 0x address"
    );
}

fn required_value_str<'a>(value: &'a Value, key: &str, label: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|item| !item.trim().is_empty())
        .unwrap_or_else(|| panic!("{label} missing non-empty {key}"))
}

fn required_nested_str<'a>(value: &'a Value, path: &[&str]) -> &'a str {
    let pointer = format!("/{}", path.join("/"));
    value
        .pointer(&pointer)
        .and_then(Value::as_str)
        .filter(|item| !item.trim().is_empty())
        .unwrap_or_else(|| panic!("JSON missing non-empty {pointer}"))
}

fn artifact_payload_files() -> [&'static str; 25] {
    [
        ENV_REDACTED,
        BINARY_SHA256,
        RUN_METADATA,
        RAW_FEED,
        FRESHNESS,
        FRESHNESS_REPORT,
        CANDIDATES,
        PAPER,
        GAS_MATRIX,
        LIVE_PARITY,
        RUN_IDENTITY,
        DEPLOYMENT_MANIFEST,
        PLACEMENT_PROOF,
        MONITORING_SUMMARY,
        ENGINE_LOG,
        HOST_TELEMETRY,
        HOST_TELEMETRY_SUMMARY,
        SIDECAR_STDOUT,
        SIDECAR_STDERR,
        PARITY_STATE_PIN,
        PARITY_SOURCE_REPORT,
        FASTLANE_PARITY_CORPUS,
        FASTLANE_PARITY_TRANSCRIPT,
        FASTLANE_PARITY_REPORT,
        FASTLANE_PARITY_MANIFEST,
    ]
}

fn validate_artifact_manifest(dir: &Path) {
    let manifest = read_json(&dir.join(ARTIFACT_MANIFEST), "artifact manifest JSON");
    assert_eq!(
        manifest.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_artifact_manifest_v2"),
        "artifact_manifest schema must be aws_dryrun_artifact_manifest_v2"
    );
    let files = manifest
        .get("files")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("artifact_manifest missing files object"));
    for file in artifact_payload_files() {
        let expected = files
            .get(file)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("artifact_manifest missing hash for {file}"));
        assert_sha256_digest(expected, &format!("artifact_manifest hash for {file}"));
        let actual = sha256_file(&dir.join(file));
        assert_eq!(
            actual, expected,
            "artifact_manifest hash mismatch for {file}"
        );
    }
}

fn required_metadata_str<'a>(metadata: &'a Value, key: &str) -> &'a str {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("run_metadata missing non-empty {key}"))
}

fn assert_utc_timestampish(value: &str, label: &str) {
    assert!(
        value.len() >= 20 && value.contains('T') && value.ends_with('Z'),
        "{label} must be a non-empty UTC timestamp ending in Z"
    );
}

fn required_metadata_u64(metadata: &Value, key: &str) -> u64 {
    metadata
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("run_metadata missing numeric {key}"))
}

fn assert_metadata_command_line_safe(command_line: &str) {
    for forbidden in ["SUBMIT_TX=true", "DRY_RUN=false", "REVM_VALIDATE=0"] {
        assert!(
            !command_line.contains(forbidden),
            "run_metadata command_line must not include {forbidden}"
        );
    }
    for required in ["DRY_RUN=true", "SUBMIT_TX=false", "REVM_VALIDATE=1"] {
        assert!(
            command_line.contains(required),
            "run_metadata command_line must include {required}"
        );
    }
}

fn active_setting(contents: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    contents.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        trimmed
            .strip_prefix(&prefix)
            .map(|value| value.split('#').next().unwrap_or("").trim().to_string())
    })
}

fn decimal_i128(row: &Value, key: &str) -> Option<i128> {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse::<i128>().ok())
}

fn decimal_u128(row: &Value, key: &str) -> Option<u128> {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse::<u128>().ok())
}

fn u64_field(row: &Value, key: &str) -> Option<u64> {
    row.get(key).and_then(Value::as_u64)
}

fn engine_heartbeat_counter_maxima(path: &Path) -> (u64, BTreeMap<&'static str, u64>) {
    const COUNTERS: [&str; 3] = ["submitted", "submit_wire_attempts", "submit_wire_acks"];
    let raw = read(path, "engine log");
    let mut heartbeat_rows = 0u64;
    let mut maxima = COUNTERS
        .into_iter()
        .map(|counter| (counter, 0u64))
        .collect::<BTreeMap<_, _>>();

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row.pointer("/fields/message").and_then(Value::as_str) != Some("pipeline heartbeat") {
            continue;
        }

        heartbeat_rows += 1;
        for counter in COUNTERS {
            let value = row
                .pointer(&format!("/fields/{counter}"))
                .and_then(Value::as_u64)
                .unwrap_or_else(|| {
                    panic!(
                        "engine log heartbeat line {} missing numeric fields.{counter}",
                        idx + 1
                    )
                });
            maxima
                .entry(counter)
                .and_modify(|maximum| *maximum = (*maximum).max(value));
        }
    }

    assert!(
        heartbeat_rows > 0,
        "engine log must contain at least one JSON pipeline heartbeat row"
    );
    (heartbeat_rows, maxima)
}

fn host_telemetry_required_domains() -> [&'static str; 15] {
    [
        "cpu",
        "scheduler",
        "process",
        "processes",
        "irq",
        "softirq",
        "ena",
        "softnet",
        "sockets",
        "network",
        "queue",
        "memory",
        "psi",
        "disk",
        "clock",
    ]
}

fn validate_host_telemetry(dir: &Path, run_identity: &Value) {
    let path = dir.join(HOST_TELEMETRY);
    let rows = read_jsonl(&path, "host telemetry JSONL");
    let summary = read_json(
        &dir.join(HOST_TELEMETRY_SUMMARY),
        "host telemetry summary JSON",
    );
    assert_eq!(
        summary.get("schema").and_then(Value::as_str),
        Some("mantis_host_telemetry_summary_v1"),
        "host telemetry summary schema is invalid"
    );
    assert_eq!(
        summary.get("status").and_then(Value::as_str),
        Some("pass"),
        "host telemetry summary status must be pass"
    );
    assert_eq!(
        summary.get("artifact").and_then(Value::as_str),
        Some(HOST_TELEMETRY),
        "host telemetry summary must name the portable raw JSONL"
    );
    let declared_sha = required_value_str(&summary, "sha256", "host telemetry summary");
    assert_sha256_digest(declared_sha, "host telemetry summary sha256");
    assert_eq!(
        declared_sha,
        sha256_file(&path),
        "host telemetry summary sha256 must match raw JSONL bytes"
    );
    assert_sha256_digest(
        required_value_str(&summary, "sampler_sha256", "host telemetry summary"),
        "host telemetry sampler_sha256",
    );
    assert_eq!(
        u64_field(&summary, "monitor_exit_status"),
        Some(0),
        "host telemetry monitor must exit successfully"
    );
    assert_eq!(
        summary
            .get("configured_interval_seconds")
            .and_then(Value::as_f64),
        Some(1.0),
        "AWS clean/evidence host telemetry cadence must be the explicit common 1s interval"
    );
    assert_eq!(
        u64_field(&summary, "row_count"),
        Some(rows.len() as u64),
        "host telemetry summary row_count must match raw JSONL"
    );
    let declared_relay_pid = u64_field(&summary, "relay_pid")
        .filter(|pid| *pid > 0)
        .unwrap_or_else(|| panic!("host telemetry summary missing positive relay_pid"));
    let declared_domains = summary
        .get("required_domains")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("host telemetry summary missing required_domains"));
    assert_eq!(
        declared_domains,
        &host_telemetry_required_domains()
            .into_iter()
            .map(Value::from)
            .collect::<Vec<_>>(),
        "host telemetry summary required_domains is incomplete or reordered"
    );

    let mut stamps = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let row_no = index + 1;
        assert_eq!(
            row.get("schema").and_then(Value::as_str),
            Some("mantis_host_telemetry_v1"),
            "host telemetry row {row_no} schema is invalid"
        );
        let stamp = u64_field(row, "monotonic_ns")
            .filter(|value| *value > 0)
            .unwrap_or_else(|| panic!("host telemetry row {row_no} has invalid monotonic_ns"));
        if let Some(previous) = stamps.last() {
            assert!(
                stamp > *previous,
                "host telemetry monotonic_ns must be strictly increasing"
            );
        }
        stamps.push(stamp);
        for domain in host_telemetry_required_domains() {
            let value = row
                .get(domain)
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("host telemetry row {row_no} missing domain {domain}"));
            let is_available = value
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| {
                    panic!("host telemetry row {row_no} domain {domain} lacks boolean available")
                });
            if !is_available {
                for key in ["source", "reason"] {
                    assert!(
                        value
                            .get(key)
                            .and_then(Value::as_str)
                            .is_some_and(|item| !item.is_empty()),
                        "host telemetry row {row_no} domain {domain} has untyped unavailability"
                    );
                }
            }
            assert!(
                is_available,
                "AWS host telemetry row {row_no} domain {domain} is unavailable"
            );
        }
        for process_name in ["engine", "sidecar", "relay"] {
            let process = row
                .pointer(&format!("/processes/{process_name}"))
                .and_then(Value::as_object)
                .unwrap_or_else(|| {
                    panic!("host telemetry row {row_no} missing {process_name} liveness")
                });
            assert_eq!(
                process.get("available").and_then(Value::as_bool),
                Some(true),
                "host telemetry row {row_no} {process_name} process is unavailable"
            );
            assert_eq!(
                process.get("alive").and_then(Value::as_bool),
                Some(true),
                "host telemetry row {row_no} {process_name} process is not alive"
            );
        }
        assert_eq!(
            row.pointer("/processes/relay/pid").and_then(Value::as_u64),
            Some(declared_relay_pid),
            "host telemetry row {row_no} relay PID must match canonical summary MainPID"
        );
        let sampler = row
            .get("sampler")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("host telemetry row {row_no} missing sampler affinity"));
        assert_eq!(
            sampler.get("available").and_then(Value::as_bool),
            Some(true),
            "host telemetry row {row_no} sampler affinity is unavailable"
        );
        assert_eq!(
            sampler.get("housekeeping_core").and_then(Value::as_u64),
            Some(0),
            "host telemetry row {row_no} sampler housekeeping core must be CPU 0"
        );
        assert_eq!(
            sampler.get("affinity_valid").and_then(Value::as_bool),
            Some(true),
            "host telemetry row {row_no} sampler affinity must be validated"
        );
        assert_eq!(
            sampler.get("affinity_cpus").and_then(Value::as_array),
            Some(&vec![Value::from(0)]),
            "host telemetry row {row_no} sampler must be pinned only to CPU 0"
        );
        let chrony = row
            .pointer("/clock/chrony")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("host telemetry row {row_no} missing chrony discipline"));
        assert_eq!(
            chrony.get("available").and_then(Value::as_bool),
            Some(true),
            "host telemetry row {row_no} chrony discipline is unavailable"
        );
        assert!(
            chrony
                .get("stratum")
                .and_then(Value::as_u64)
                .is_some_and(|value| value > 0),
            "host telemetry row {row_no} chrony stratum is invalid"
        );
        for field in [
            "system_time_offset_seconds",
            "last_offset_seconds",
            "rms_offset_seconds",
            "frequency_ppm",
            "residual_frequency_ppm",
            "skew_ppm",
        ] {
            assert!(
                chrony.get(field).and_then(Value::as_f64).is_some(),
                "host telemetry row {row_no} chrony {field} is not numeric"
            );
        }
        for resource in ["cpu", "memory", "io"] {
            assert_eq!(
                row.pointer(&format!("/psi/{resource}/available"))
                    .and_then(Value::as_bool),
                Some(true),
                "host telemetry row {row_no} {resource} PSI is unavailable"
            );
        }
        let ena_interfaces = row
            .pointer("/ena/interfaces")
            .and_then(Value::as_object)
            .filter(|interfaces| !interfaces.is_empty())
            .unwrap_or_else(|| panic!("host telemetry row {row_no} has no ENA interfaces"));
        for (interface, evidence) in ena_interfaces {
            assert_eq!(
                evidence.get("available").and_then(Value::as_bool),
                Some(true),
                "host telemetry row {row_no} ENA {interface} is unavailable"
            );
            assert_eq!(
                evidence.get("driver").and_then(Value::as_str),
                Some("ena"),
                "host telemetry row {row_no} interface {interface} is not ENA"
            );
            for facility in ["rss", "coalescing"] {
                assert_eq!(
                    evidence
                        .pointer(&format!("/{facility}/available"))
                        .and_then(Value::as_bool),
                    Some(true),
                    "host telemetry row {row_no} ENA {interface} {facility} is unavailable"
                );
            }
            for direction in ["rx", "tx"] {
                assert!(
                    evidence
                        .pointer(&format!("/queues/{direction}"))
                        .and_then(Value::as_u64)
                        .is_some_and(|count| count > 0),
                    "host telemetry row {row_no} ENA {interface} has no {direction} queues"
                );
            }
            let allowance = evidence
                .get("allowance_counters")
                .and_then(Value::as_object)
                .filter(|values| !values.is_empty())
                .unwrap_or_else(|| {
                    panic!("host telemetry row {row_no} ENA {interface} lacks allowance counters")
                });
            let deltas = evidence
                .get("allowance_deltas")
                .and_then(Value::as_object)
                .unwrap_or_else(|| {
                    panic!("host telemetry row {row_no} ENA {interface} lacks allowance deltas")
                });
            assert_eq!(
                allowance.keys().collect::<BTreeSet<_>>(),
                deltas.keys().collect::<BTreeSet<_>>(),
                "host telemetry row {row_no} ENA {interface} allowance delta keys mismatch"
            );
            for (kind, values) in [("counter", allowance), ("delta", deltas)] {
                for (name, value) in values {
                    assert!(
                        value.as_u64().is_some(),
                        "host telemetry row {row_no} ENA {interface} allowance {kind} {name} must be a nonnegative integer"
                    );
                }
            }
        }
    }
    let coverage_start = *stamps.first().expect("host telemetry rows nonempty");
    let coverage_end = *stamps.last().expect("host telemetry rows nonempty");
    assert_eq!(
        u64_field(&summary, "coverage_start_monotonic_ns"),
        Some(coverage_start),
        "host telemetry summary coverage start must match raw JSONL"
    );
    assert_eq!(
        u64_field(&summary, "coverage_end_monotonic_ns"),
        Some(coverage_end),
        "host telemetry summary coverage end must match raw JSONL"
    );
    let max_gap = stamps
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or(0);
    assert_eq!(
        u64_field(&summary, "max_gap_ns"),
        Some(max_gap),
        "host telemetry summary max_gap_ns must match raw JSONL"
    );
    assert!(
        max_gap
            <= u64_field(&summary, "max_allowed_gap_ns")
                .unwrap_or_else(|| panic!("host telemetry summary missing max_allowed_gap_ns")),
        "host telemetry sampling gap exceeds its declared bound"
    );
    assert_eq!(
        run_identity.get("host_telemetry"),
        Some(&summary),
        "run_identity must bind the exact portable host telemetry summary"
    );
}

fn route_id(row: &Value) -> Option<u64> {
    row.get("route_id").and_then(Value::as_u64)
}

fn required_str<'a>(row: &'a Value, key: &str, row_no: usize) -> &'a str {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("accepted candidate row {row_no} missing {key}"))
}

fn assert_hex_payload(row: &Value, key: &str, row_no: usize) {
    let value = required_str(row, key, row_no);
    assert!(
        value.starts_with("0x") && value.len() > 2,
        "accepted candidate row {row_no} {key} must be non-empty 0x hex"
    );
}

fn assert_tx_hash(row: &Value, key: &str, row_no: usize) {
    let value = required_str(row, key, row_no);
    assert!(
        value.starts_with("0x")
            && value.len() == 66
            && value[2..].chars().all(|ch| ch.is_ascii_hexdigit()),
        "accepted candidate row {row_no} {key} must be a 32-byte 0x tx hash"
    );
}

fn assert_non_empty_array(row: &Value, key: &str, row_no: usize) {
    assert!(
        row.get(key)
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        "accepted candidate row {row_no} missing non-empty {key}"
    );
}

fn required_array<'a>(row: &'a Value, key: &str, row_no: usize) -> &'a Vec<Value> {
    row.get(key)
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| panic!("accepted candidate row {row_no} missing non-empty {key}"))
}

fn assert_accepted_candidate_replay_anchors(row: &Value, row_no: usize) {
    required_str(row, "candidate_id", row_no);
    let feed_seq = u64_field(row, "feed_seq")
        .unwrap_or_else(|| panic!("accepted candidate row {row_no} missing feed_seq"));
    let recv_ns = u64_field(row, "recv_ns")
        .unwrap_or_else(|| panic!("accepted candidate row {row_no} missing recv_ns"));
    let trigger_seq = u64_field(row, "trigger_seq")
        .unwrap_or_else(|| panic!("accepted candidate row {row_no} missing trigger_seq"));
    let trigger_recv_ns = u64_field(row, "trigger_recv_ns")
        .unwrap_or_else(|| panic!("accepted candidate row {row_no} missing trigger_recv_ns"));
    assert_eq!(
        trigger_seq, feed_seq,
        "accepted candidate row {row_no} trigger_seq must match feed_seq"
    );
    assert_eq!(
        trigger_recv_ns, recv_ns,
        "accepted candidate row {row_no} trigger_recv_ns must match recv_ns"
    );
    assert_tx_hash(row, "tx_hash", row_no);
    assert_tx_hash(row, "trigger_tx_hash", row_no);
    assert_eq!(
        row.get("tx_hash"),
        row.get("trigger_tx_hash"),
        "accepted candidate row {row_no} tx_hash and trigger_tx_hash must match"
    );
    required_str(row, "state_point", row_no);
    required_str(row, "state_point_mode", row_no);
    for key in ["calldata", "mantis_calldata", "executor_calldata"] {
        assert_hex_payload(row, key, row_no);
    }
    assert_eq!(
        row.get("calldata"),
        row.get("mantis_calldata"),
        "accepted candidate row {row_no} calldata and mantis_calldata must match"
    );
    assert_eq!(
        row.get("calldata"),
        row.get("executor_calldata"),
        "accepted candidate row {row_no} calldata and executor_calldata must match"
    );
    for key in ["tokens", "pools", "exchange_ids", "exchange_metadata"] {
        assert_non_empty_array(row, key, row_no);
    }

    let tokens = required_array(row, "tokens", row_no);
    let pools = required_array(row, "pools", row_no);
    let exchange_ids = required_array(row, "exchange_ids", row_no);
    let exchange_metadata = required_array(row, "exchange_metadata", row_no);
    assert_eq!(
        tokens.len(),
        pools.len() + 1,
        "accepted candidate row {row_no} tokens length must equal pools length + 1"
    );
    assert_eq!(
        exchange_ids.len(),
        pools.len(),
        "accepted candidate row {row_no} exchange_ids length must match pools length"
    );
    assert_eq!(
        exchange_metadata.len(),
        pools.len(),
        "accepted candidate row {row_no} exchange_metadata length must match pools length"
    );

    for (hop_idx, hop) in exchange_metadata.iter().enumerate() {
        let hop_no = hop_idx + 1;
        required_str(hop, "family", row_no);
        let metadata_pool = required_str(hop, "pool", row_no);
        let route_pool = pools[hop_idx].as_str().unwrap_or_else(|| {
            panic!("accepted candidate row {row_no} pools hop {hop_no} not string")
        });
        assert_eq!(
            metadata_pool, route_pool,
            "accepted candidate row {row_no} exchange_metadata hop {hop_no} pool must match pools[{hop_idx}]"
        );
        assert!(
            hop.get("fee_pips_or_bps").is_some(),
            "accepted candidate row {row_no} exchange_metadata hop {hop_no} missing fee_pips_or_bps"
        );
        assert!(
            exchange_ids[hop_idx].as_u64().is_some(),
            "accepted candidate row {row_no} exchange_ids hop {hop_no} must be numeric"
        );
    }
}

fn nested_u64<'a>(row: &'a Value, path: &[&str]) -> Option<u64> {
    let mut cursor = row;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor.as_u64()
}

fn raw_feed_message_timestamp(row: &Value) -> Option<u64> {
    nested_u64(row, &["message", "message", "header", "timestamp"])
        .or_else(|| nested_u64(row, &["message", "header", "timestamp"]))
        .or_else(|| u64_field(row, "timestamp"))
}

fn collect_raw_feed_timestamps(raw_feed: &[Value]) -> BTreeMap<u64, u64> {
    let mut by_seq = BTreeMap::<u64, u64>::new();
    for (row_idx, row) in raw_feed.iter().enumerate() {
        let row_no = row_idx + 1;
        let messages = row
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(row));
        for (msg_idx, message) in messages.iter().enumerate() {
            let msg_no = msg_idx + 1;
            let seq = u64_field(message, "sequenceNumber").unwrap_or_else(|| {
                panic!("raw feed row {row_no} message {msg_no} missing numeric sequenceNumber")
            });
            let timestamp = raw_feed_message_timestamp(message).unwrap_or_else(|| {
                panic!("raw feed row {row_no} message {msg_no} missing header timestamp")
            });
            if let Some(previous) = by_seq.insert(seq, timestamp) {
                assert_eq!(
                    previous, timestamp,
                    "raw feed sequenceNumber {seq} has conflicting header timestamps"
                );
            }
        }
    }
    by_seq
}

fn percentile_floor(sorted: &[u64], percentile: usize) -> u64 {
    assert!(!sorted.is_empty(), "percentile input must not be empty");
    let idx = ((sorted.len() * percentile) / 100).min(sorted.len() - 1);
    sorted[idx]
}

fn validate_aws_dryrun_artifacts_with_expected_binary(
    dir: &Path,
    expected_binary_sha256: Option<&str>,
) {
    for file in artifact_payload_files()
        .into_iter()
        .chain(std::iter::once(ARTIFACT_MANIFEST))
    {
        let path = dir.join(file);
        assert!(
            path.is_file(),
            "AWS dry-run artifact missing required file {}",
            path.display()
        );
        if !matches!(file, SIDECAR_STDOUT | SIDECAR_STDERR) {
            assert!(
                path.metadata().expect("artifact metadata").len() > 0,
                "AWS dry-run artifact is empty: {}",
                path.display()
            );
        }
    }

    let env = read(&dir.join(ENV_REDACTED), "redacted env");
    for (key, expected) in [
        ("DRY_RUN", "true"),
        ("SUBMIT_TX", "false"),
        ("REVM_VALIDATE", "1"),
    ] {
        assert_eq!(
            active_setting(&env, key).as_deref(),
            Some(expected),
            "redacted AWS env must contain {key}={expected}"
        );
    }
    for forbidden in [
        "PRIVATE_KEY",
        "CANARY_PRIVATE_KEY",
        "TRADING_PRIVATE_KEY",
        "KEY_HEX",
        "EXECUTOR_PRIVATE_KEY",
    ] {
        assert!(
            active_setting(&env, forbidden).is_none(),
            "redacted AWS dry-run env must not load {forbidden}"
        );
    }
    for forbidden in ["ARBITRUM_RPC_URL"] {
        assert!(
            active_setting(&env, forbidden).is_none(),
            "redacted AWS dry-run env must not expose {forbidden}"
        );
    }
    for forbidden in [
        "EXECUTOR_ADDRESS",
        "MANTIS_EXECUTOR_ADDRESS",
        "ARBITRAGE_EXECUTOR_ADDRESS",
        "TRADE_EXECUTOR_ADDRESS",
    ] {
        assert!(
            active_setting(&env, forbidden).is_none(),
            "redacted AWS dry-run env must not depend on {forbidden}"
        );
    }

    let binary_hash = read(&dir.join(BINARY_SHA256), "binary sha256");
    let hash = binary_hash.trim();
    assert_sha256_digest(hash, "binary.sha256");
    if let Some(expected) = expected_binary_sha256 {
        let expected = expected.trim();
        assert_sha256_digest(expected, "expected release binary SHA256");
        assert_eq!(
            hash, expected,
            "binary.sha256 must match expected release binary"
        );
    }

    let run_metadata = read_json(&dir.join(RUN_METADATA), "run metadata JSON");
    assert_eq!(
        run_metadata.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_run_metadata_v2"),
        "run_metadata schema must be aws_dryrun_run_metadata_v2"
    );
    let metadata_binary_hash = required_metadata_str(&run_metadata, "binary_sha256").trim();
    assert_sha256_digest(metadata_binary_hash, "run_metadata binary_sha256");
    assert_eq!(
        metadata_binary_hash, hash,
        "run_metadata binary_sha256 must match binary.sha256"
    );
    for key in ["source_dir", "artifact_dir", "engine_bin", "command_line"] {
        required_metadata_str(&run_metadata, key);
    }
    assert_metadata_command_line_safe(required_metadata_str(&run_metadata, "command_line"));
    let assembled_at = required_metadata_str(&run_metadata, "assembled_at_utc");
    let run_started = required_metadata_str(&run_metadata, "run_started_utc");
    let run_finished = required_metadata_str(&run_metadata, "run_finished_utc");
    assert_utc_timestampish(assembled_at, "run_metadata assembled_at_utc");
    assert_utc_timestampish(run_started, "run_metadata run_started_utc");
    assert_utc_timestampish(run_finished, "run_metadata run_finished_utc");
    assert!(
        run_finished >= run_started,
        "run_metadata run_finished_utc must be greater than or equal to run_started_utc"
    );
    let run_duration_seconds = required_metadata_u64(&run_metadata, "run_duration_seconds");
    assert!(
        run_duration_seconds >= 300,
        "run_metadata run_duration_seconds must be at least 300"
    );

    let metadata_run_id = required_metadata_str(&run_metadata, "run_id");
    let metadata_profile = required_metadata_str(&run_metadata, "profile");
    assert_eq!(
        metadata_profile, "aws_evidence_fine",
        "run_metadata profile must be aws_evidence_fine"
    );
    let metadata_source_sha = required_metadata_str(&run_metadata, "source_manifest_sha256");
    let metadata_run_identity_sha = required_metadata_str(&run_metadata, "run_identity_sha256");
    let metadata_deployment_sha =
        required_metadata_str(&run_metadata, "deployment_manifest_sha256");
    let metadata_placement_sha = required_metadata_str(&run_metadata, "placement_proof_sha256");
    let metadata_monitoring_sha = required_metadata_str(&run_metadata, "monitoring_summary_sha256");
    let metadata_host_telemetry_sha = required_metadata_str(&run_metadata, "host_telemetry_sha256");
    let metadata_host_telemetry_summary_sha =
        required_metadata_str(&run_metadata, "host_telemetry_summary_sha256");
    let metadata_sidecar_stdout_sha = required_metadata_str(&run_metadata, "sidecar_stdout_sha256");
    let metadata_sidecar_stderr_sha = required_metadata_str(&run_metadata, "sidecar_stderr_sha256");
    for (digest, label) in [
        (metadata_source_sha, "run_metadata source_manifest_sha256"),
        (
            metadata_run_identity_sha,
            "run_metadata run_identity_sha256",
        ),
        (
            metadata_deployment_sha,
            "run_metadata deployment_manifest_sha256",
        ),
        (
            metadata_placement_sha,
            "run_metadata placement_proof_sha256",
        ),
        (
            metadata_monitoring_sha,
            "run_metadata monitoring_summary_sha256",
        ),
        (
            metadata_host_telemetry_sha,
            "run_metadata host_telemetry_sha256",
        ),
        (
            metadata_host_telemetry_summary_sha,
            "run_metadata host_telemetry_summary_sha256",
        ),
        (
            metadata_sidecar_stdout_sha,
            "run_metadata sidecar_stdout_sha256",
        ),
        (
            metadata_sidecar_stderr_sha,
            "run_metadata sidecar_stderr_sha256",
        ),
    ] {
        assert_sha256_digest(digest, label);
    }

    let run_identity = read_json(&dir.join(RUN_IDENTITY), "run identity JSON");
    assert_eq!(
        run_identity.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_harness_run_v1"),
        "run_identity schema must be aws_dryrun_harness_run_v1"
    );
    assert_eq!(
        required_value_str(&run_identity, "run_id", "run_identity"),
        metadata_run_id,
        "run_identity run_id must match run_metadata"
    );
    assert_eq!(
        required_value_str(&run_identity, "profile", "run_identity"),
        "aws_evidence_fine",
        "run_identity must be the instrumented AWS evidence profile"
    );
    assert_eq!(
        required_value_str(&run_identity, "status", "run_identity"),
        "pass",
        "run_identity status must be pass"
    );
    assert_eq!(
        run_identity.get("instrumented").and_then(Value::as_bool),
        Some(true),
        "run_identity must prove the evidence pass was instrumented"
    );
    assert_eq!(
        required_value_str(&run_identity, "source_manifest_sha256", "run_identity"),
        metadata_source_sha,
        "run_identity source manifest must match run_metadata"
    );
    assert_eq!(
        required_value_str(&run_identity, "engine_sha256", "run_identity"),
        hash,
        "run_identity engine hash must match binary.sha256"
    );
    assert_eq!(
        required_value_str(&run_identity, "deployment_manifest_sha256", "run_identity",),
        metadata_deployment_sha,
        "run_identity deployment manifest hash must match run_metadata"
    );
    assert_eq!(
        required_value_str(&run_identity, "placement_proof_sha256", "run_identity"),
        metadata_placement_sha,
        "run_identity placement proof hash must match run_metadata"
    );
    assert_eq!(
        required_value_str(&run_identity, "started_utc", "run_identity"),
        run_started,
        "run_identity started_utc must match run_metadata"
    );
    assert_eq!(
        required_value_str(&run_identity, "finished_utc", "run_identity"),
        run_finished,
        "run_identity finished_utc must match run_metadata"
    );
    assert_eq!(
        u64_field(&run_identity, "live_window_seconds"),
        Some(run_duration_seconds),
        "run_identity live_window_seconds must match run_metadata duration"
    );
    assert_eq!(
        required_value_str(&run_identity, "effective_command", "run_identity"),
        required_metadata_str(&run_metadata, "command_line"),
        "run_identity effective_command must match run_metadata command_line"
    );

    validate_host_telemetry(dir, &run_identity);

    let monitoring = read_json(&dir.join(MONITORING_SUMMARY), "monitoring summary JSON");
    assert_eq!(
        monitoring.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_monitoring_summary_v1"),
        "monitoring summary schema must be aws_dryrun_monitoring_summary_v1"
    );
    assert_eq!(
        required_value_str(&monitoring, "run_id", "monitoring summary"),
        metadata_run_id,
        "monitoring summary run_id must match run_metadata"
    );
    assert_eq!(
        required_value_str(&monitoring, "source_manifest_sha256", "monitoring summary",),
        metadata_source_sha,
        "monitoring summary source manifest must match run_metadata"
    );
    assert_eq!(
        required_value_str(&monitoring, "engine_sha256", "monitoring summary"),
        hash,
        "monitoring summary engine hash must match binary.sha256"
    );
    assert_eq!(
        u64_field(&monitoring, "live_window_seconds"),
        Some(run_duration_seconds),
        "monitoring summary live window must match run_metadata duration"
    );
    assert_eq!(
        required_value_str(&monitoring, "engine_log_artifact", "monitoring summary"),
        ENGINE_LOG,
        "monitoring summary must name the bundled engine.log artifact"
    );
    let declared_engine_log_sha =
        required_value_str(&monitoring, "engine_log_sha256", "monitoring summary");
    assert_sha256_digest(
        declared_engine_log_sha,
        "monitoring summary engine_log_sha256",
    );
    assert_eq!(
        declared_engine_log_sha,
        sha256_file(&dir.join(ENGINE_LOG)),
        "monitoring summary engine_log_sha256 must match bundled engine.log bytes"
    );

    let declared_heartbeat_rows = u64_field(&monitoring, "heartbeat_rows")
        .unwrap_or_else(|| panic!("monitoring summary missing numeric heartbeat_rows"));
    assert!(
        declared_heartbeat_rows > 0,
        "monitoring summary heartbeat_rows must be greater than zero"
    );
    let (observed_heartbeat_rows, observed_maxima) =
        engine_heartbeat_counter_maxima(&dir.join(ENGINE_LOG));
    assert_eq!(
        declared_heartbeat_rows, observed_heartbeat_rows,
        "monitoring summary heartbeat_rows must equal recomputed engine.log heartbeat count"
    );
    let declared_maxima = monitoring
        .get("counter_maxima")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("monitoring summary missing counter_maxima object"));
    for counter in ["submitted", "submit_wire_attempts", "submit_wire_acks"] {
        let declared = declared_maxima
            .get(counter)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                panic!("monitoring summary counter_maxima.{counter} must be numeric")
            });
        let observed = observed_maxima[counter];
        assert_eq!(
            declared, observed,
            "monitoring summary counter_maxima.{counter} must equal recomputed engine.log maximum"
        );
        if counter != "submitted" {
            assert_eq!(
                declared, 0,
                "monitoring summary counter_maxima.{counter} must be zero for a no-wire dry run"
            );
        }
    }

    let deployment = read_json(&dir.join(DEPLOYMENT_MANIFEST), "deployment manifest JSON");
    assert_eq!(
        deployment.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_deployment_bundle_v1"),
        "deployment manifest schema must be aws_dryrun_deployment_bundle_v1"
    );
    assert_eq!(
        required_value_str(&deployment, "source_manifest_sha256", "deployment manifest"),
        metadata_source_sha,
        "deployment source manifest must match run_metadata"
    );
    assert_eq!(
        required_value_str(&deployment, "engine_sha256", "deployment manifest"),
        hash,
        "deployment engine hash must match binary.sha256"
    );
    let deployment_placement_sha =
        required_value_str(&deployment, "placement_proof_sha256", "deployment manifest");
    assert_eq!(
        deployment_placement_sha, metadata_placement_sha,
        "deployment placement proof hash must match run_metadata"
    );

    let placement = read_json(&dir.join(PLACEMENT_PROOF), "placement proof JSON");
    assert_eq!(
        placement.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_placement_proof_v2"),
        "placement proof schema must be aws_dryrun_placement_proof_v2"
    );
    assert_eq!(
        placement.get("verdict").and_then(Value::as_str),
        Some("PLACEMENT_READY")
    );
    assert!(matches!(
        placement.get("objective").and_then(Value::as_str),
        Some("submission_priority" | "feed_priority")
    ));
    assert_eq!(
        required_value_str(&placement, "instance_type", "placement proof"),
        "c8gn.2xlarge",
        "placement proof must bind the certified c8gn.2xlarge shape"
    );
    for key in [
        "region",
        "az_name",
        "az_id",
        "subnet_id",
        "submit_pin_ip",
        "feed_pin_ip",
    ] {
        required_value_str(&placement, key, "placement proof");
    }
    assert_eq!(
        required_nested_str(&placement, &["source", "source_manifest_sha256"]),
        metadata_source_sha,
        "placement source digest must match run metadata"
    );
    for lane in ["submit", "feed"] {
        for key in [
            "report_sha256",
            "campaign_config_sha256",
            "bundle_manifest_sha256",
        ] {
            assert_sha256_digest(
                required_nested_str(&placement, &[lane, key]),
                &format!("placement proof {lane}.{key}"),
            );
        }
    }
    assert_eq!(
        placement
            .pointer("/counterpart_evidence/submit_colocated")
            .and_then(Value::as_bool),
        Some(true),
        "placement must contain colocated submit evidence"
    );
    assert_eq!(
        placement
            .pointer("/counterpart_evidence/feed_colocated")
            .and_then(Value::as_bool),
        Some(true),
        "placement must contain colocated feed evidence"
    );

    let raw_feed = read_jsonl(&dir.join(RAW_FEED), "raw feed JSONL");
    assert!(
        raw_feed.iter().any(|row| row.get("messages").is_some()),
        "raw feed capture must contain Arbitrum relay message envelopes"
    );
    let raw_feed_timestamps = collect_raw_feed_timestamps(&raw_feed);
    assert!(
        !raw_feed_timestamps.is_empty(),
        "raw feed capture must expose at least one sequenceNumber/header timestamp pair"
    );

    let freshness = read_jsonl(&dir.join(FRESHNESS), "freshness sidecar JSONL");
    let mut freshness_by_seq = BTreeMap::<u64, (u64, u64)>::new();
    for (idx, row) in freshness.iter().enumerate() {
        let n = idx + 1;
        let seq = u64_field(row, "seq")
            .unwrap_or_else(|| panic!("freshness row {n} missing numeric seq"));
        let sequencer_timestamp = u64_field(row, "sequencer_timestamp")
            .unwrap_or_else(|| panic!("freshness row {n} missing sequencer_timestamp"));
        let raw_timestamp = raw_feed_timestamps.get(&seq).unwrap_or_else(|| {
            panic!("freshness row {n} seq {seq} does not exist in raw feed capture")
        });
        assert!(
            sequencer_timestamp == *raw_timestamp,
            "freshness row {n} sequencer_timestamp does not match raw feed header timestamp"
        );
        assert!(
            u64_field(row, "capture_recv_ns").is_some(),
            "freshness row {n} missing capture_recv_ns"
        );
        let capture_recv_ns =
            u64_field(row, "capture_recv_ns").expect("freshness capture_recv_ns checked above");
        let age = u64_field(row, "event_age_ms")
            .unwrap_or_else(|| panic!("freshness row {n} missing event_age_ms"));
        freshness_by_seq.insert(seq, (age, capture_recv_ns));
    }
    assert!(
        !freshness_by_seq.is_empty(),
        "freshness sidecar must expose at least one sequenceNumber join key"
    );

    let freshness_report = read_json(
        &dir.join(FRESHNESS_REPORT),
        "feed/candidate freshness report",
    );
    let stale_threshold_ms = u64_field(&freshness_report, "stale_threshold_ms")
        .expect("freshness report missing stale_threshold_ms");
    assert!(
        stale_threshold_ms > 0,
        "freshness report stale_threshold_ms must be positive"
    );
    assert_eq!(
        freshness_report
            .get("local_laptop_over_1s_allowed_only_if_rejected")
            .and_then(Value::as_bool),
        Some(true),
        "freshness report must preserve local-laptop >1s latency boundary"
    );

    let candidates = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    let mut candidate_routes = BTreeSet::new();
    let mut accepted_positive = 0usize;
    let mut accepted_candidate_rows = Vec::<(usize, &Value)>::new();
    let mut accepted_candidate_routes = Vec::<(usize, u64)>::new();
    let mut accepted_candidate_paper_keys = Vec::<(usize, u64, u64, u64, u128, i128)>::new();
    let mut joined_event_ages = Vec::<u64>::new();
    let mut stale_candidates = 0usize;
    let mut accepted_stale_candidates = 0usize;
    let mut candidate_submit_calldata_bytes = BTreeSet::<u64>::new();
    let mut candidate_gas_keys = BTreeSet::<(u64, u128, u128)>::new();
    let mut candidate_direct_gas =
        BTreeMap::<String, (String, u64, String, u64, u128, u128)>::new();
    for (idx, row) in candidates.iter().enumerate() {
        let n = idx + 1;
        let feed_seq = u64_field(row, "feed_seq")
            .unwrap_or_else(|| panic!("candidate row {n} missing feed_seq"));
        let recv_ns = u64_field(row, "recv_ns")
            .unwrap_or_else(|| panic!("candidate row {n} missing recv_ns"));
        assert!(
            freshness_by_seq.contains_key(&feed_seq),
            "candidate row {n} feed_seq {feed_seq} does not join to freshness sidecar"
        );
        let (event_age_ms, freshness_recv_ns) = *freshness_by_seq
            .get(&feed_seq)
            .expect("freshness join checked above");
        assert_eq!(
            recv_ns, freshness_recv_ns,
            "candidate row {n} recv_ns does not match freshness capture_recv_ns"
        );
        joined_event_ages.push(event_age_ms);
        assert_eq!(
            row.get("revm_enabled").and_then(Value::as_bool),
            Some(true),
            "candidate row {n} must prove REVM validation was active"
        );
        let revm_result = row
            .get("revm_result")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("candidate row {n} missing revm_result"));
        assert!(
            matches!(revm_result, "success" | "loss" | "revert" | "halt"),
            "candidate row {n} unexpected revm_result {revm_result}"
        );
        let gross_profit = decimal_u128(row, "gross_profit_wei")
            .unwrap_or_else(|| panic!("candidate row {n} missing decimal gross_profit_wei"));
        let estimated_gas = decimal_u128(row, "estimated_gas_wei")
            .unwrap_or_else(|| panic!("candidate row {n} missing decimal estimated_gas_wei"));
        let total_gas = decimal_u128(row, "total_gas_wei")
            .unwrap_or_else(|| panic!("candidate row {n} missing decimal total_gas_wei"));
        let l1_fee = decimal_u128(row, "l1_fee_wei")
            .unwrap_or_else(|| panic!("candidate row {n} missing decimal l1_fee_wei"));
        let l2_gas_price = decimal_u128(row, "l2_gas_price_wei")
            .unwrap_or_else(|| panic!("candidate row {n} missing decimal l2_gas_price_wei"));
        let submit_min_profit = decimal_u128(row, "submit_min_profit_wei")
            .unwrap_or_else(|| panic!("candidate row {n} missing decimal submit_min_profit_wei"));
        assert!(
            gross_profit > 0,
            "candidate row {n} gross_profit_wei must be positive"
        );
        assert_eq!(
            total_gas, estimated_gas,
            "candidate row {n} total_gas_wei must match estimated_gas_wei"
        );
        assert!(
            estimated_gas >= l1_fee,
            "candidate row {n} estimated_gas_wei must include l1_fee_wei"
        );
        assert!(
            l2_gas_price > 0,
            "candidate row {n} l2_gas_price_wei must be positive"
        );
        assert!(
            submit_min_profit > 0,
            "candidate row {n} submit_min_profit_wei must be positive"
        );

        let gas_used = u64_field(row, "gas_used_units")
            .unwrap_or_else(|| panic!("candidate row {n} missing numeric gas_used_units"));
        let measured_gas = u64_field(row, "measured_execution_gas_used").unwrap_or_else(|| {
            panic!("candidate row {n} missing numeric measured_execution_gas_used")
        });
        let raw_calldata_bytes = u64_field(row, "raw_calldata_bytes")
            .unwrap_or_else(|| panic!("candidate row {n} missing numeric raw_calldata_bytes"));
        let submit_calldata_bytes = u64_field(row, "submit_calldata_bytes")
            .unwrap_or_else(|| panic!("candidate row {n} missing numeric submit_calldata_bytes"));
        let submit_gas_limit = u64_field(row, "submit_gas_limit")
            .unwrap_or_else(|| panic!("candidate row {n} missing numeric submit_gas_limit"));
        for (key, value) in [
            ("gas_used_units", gas_used),
            ("measured_execution_gas_used", measured_gas),
            ("raw_calldata_bytes", raw_calldata_bytes),
            ("submit_calldata_bytes", submit_calldata_bytes),
            ("submit_gas_limit", submit_gas_limit),
        ] {
            assert!(value > 0, "candidate row {n} {key} must be positive");
        }
        assert!(
            submit_gas_limit >= measured_gas,
            "candidate row {n} submit_gas_limit must cover measured_execution_gas_used"
        );
        candidate_submit_calldata_bytes.insert(submit_calldata_bytes);
        candidate_gas_keys.insert((submit_calldata_bytes, l1_fee, l2_gas_price));
        if let (Some(candidate_id), Some(target), Some(calldata), Some(block_number)) = (
            row.get("candidate_id").and_then(Value::as_str),
            row.get("mantis_executor").and_then(Value::as_str),
            row.get("executor_calldata").and_then(Value::as_str),
            u64_field(row, "snapshot_block").or_else(|| u64_field(row, "block_env")),
        ) {
            assert!(
                candidate_direct_gas
                    .insert(
                        candidate_id.to_string(),
                        (
                            target.to_string(),
                            block_number,
                            calldata.to_string(),
                            submit_calldata_bytes,
                            l1_fee,
                            l2_gas_price,
                        ),
                    )
                    .is_none(),
                "candidate_id {candidate_id} is duplicated in candidate JSONL"
            );
        }

        let reject = row
            .get("reject_reason")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("candidate row {n} missing reject_reason"));
        assert!(
            !reject.trim().is_empty(),
            "candidate row {n} reject_reason must not be empty"
        );
        if event_age_ms > stale_threshold_ms {
            stale_candidates += 1;
            if reject == "none" {
                accepted_stale_candidates += 1;
            }
            assert!(
                reject != "none",
                "stale candidate row {n} was accepted before would-submit"
            );
            let stale_reason = reject.to_ascii_lowercase();
            assert!(
                stale_reason.contains("stale") || stale_reason.contains("freshness"),
                "stale candidate row {n} must carry stale/freshness reject_reason, got {reject}"
            );
        }
        if revm_result == "success" && reject == "none" {
            let net = decimal_i128(row, "net_profit_wei")
                .unwrap_or_else(|| panic!("accepted row {n} missing net_profit_wei"));
            assert!(
                net > 0,
                "accepted success candidate row {n} is not net-positive after gas: {net}"
            );
            let candidate_route_id =
                route_id(row).expect("accepted candidate row missing route_id");
            let candidate_block = u64_field(row, "block")
                .unwrap_or_else(|| panic!("accepted candidate row {n} missing block"));
            accepted_positive += 1;
            accepted_candidate_rows.push((n, row));
            accepted_candidate_routes.push((n, candidate_route_id));
            accepted_candidate_paper_keys.push((
                n,
                candidate_route_id,
                recv_ns,
                candidate_block,
                gross_profit,
                net,
            ));
        }
        candidate_routes.insert(route_id(row).expect("candidate row missing route_id"));
    }
    assert!(
        accepted_positive > 0,
        "AWS dry-run artifact must contain at least one accepted net-positive success, \
         or the run is non-vacuous only for safety but not money-path measurement"
    );
    for (row_no, row) in accepted_candidate_rows {
        assert_accepted_candidate_replay_anchors(row, row_no);
    }

    joined_event_ages.sort_unstable();
    let candidate_count = candidates.len() as u64;
    assert_eq!(
        u64_field(&freshness_report, "candidate_count"),
        Some(candidate_count),
        "freshness report candidate_count must match candidate JSONL"
    );
    assert_eq!(
        u64_field(&freshness_report, "joined_candidate_count"),
        Some(candidate_count),
        "freshness report joined_candidate_count must match candidate JSONL"
    );
    assert_eq!(
        u64_field(&freshness_report, "missing_join_count"),
        Some(0),
        "freshness report must have zero unjoinable candidates"
    );
    assert_eq!(
        u64_field(&freshness_report, "p95_event_age_ms"),
        Some(percentile_floor(&joined_event_ages, 95)),
        "freshness report p95_event_age_ms must match joined candidates"
    );
    assert_eq!(
        u64_field(&freshness_report, "p99_event_age_ms"),
        Some(percentile_floor(&joined_event_ages, 99)),
        "freshness report p99_event_age_ms must match joined candidates"
    );
    assert_eq!(
        u64_field(&freshness_report, "max_event_age_ms"),
        joined_event_ages.last().copied(),
        "freshness report max_event_age_ms must match joined candidates"
    );
    assert_eq!(
        u64_field(&freshness_report, "stale_candidate_count"),
        Some(stale_candidates as u64),
        "freshness report stale_candidate_count must match threshold calculation"
    );
    assert_eq!(
        u64_field(&freshness_report, "accepted_stale_candidate_count"),
        Some(accepted_stale_candidates as u64),
        "freshness report accepted_stale_candidate_count must match candidate rejects"
    );
    assert_eq!(
        accepted_stale_candidates, 0,
        "AWS dry-run must reject every stale candidate before would-submit"
    );

    let paper = read_jsonl(&dir.join(PAPER), "paper JSONL");
    let mut paper_routes = BTreeSet::<u64>::new();
    let mut paper_keys = BTreeSet::<(u64, u64, u64, u128, i128)>::new();
    for row in paper.iter() {
        let timestamp_ns = u64_field(row, "timestamp_ns").expect("paper row missing timestamp_ns");
        let block_number_seen =
            u64_field(row, "block_number_seen").expect("paper row missing block_number_seen");
        let gross_profit =
            decimal_u128(row, "gross_profit_wei").expect("paper row missing gross_profit_wei");
        let net_profit =
            decimal_i128(row, "net_profit_wei").expect("paper row missing net_profit_wei");
        let route_id = route_id(row).expect("paper row missing route_id");
        paper_routes.insert(route_id);
        paper_keys.insert((
            route_id,
            timestamp_ns,
            block_number_seen,
            gross_profit,
            net_profit,
        ));
    }
    assert!(
        candidate_routes
            .intersection(&paper_routes)
            .next()
            .is_some(),
        "paper JSONL and candidate JSONL have no shared route_id"
    );
    for (row_no, route_id) in accepted_candidate_routes {
        assert!(
            paper_routes.contains(&route_id),
            "accepted candidate row {row_no} route_id {route_id} missing matching paper JSONL route_id"
        );
    }
    for (row_no, route_id, recv_ns, block, gross_profit, net_profit) in
        accepted_candidate_paper_keys
    {
        assert!(
            paper_keys.contains(&(route_id, recv_ns, block, gross_profit, net_profit)),
            "accepted candidate row {row_no} route_id {route_id} missing matching paper JSONL record at same timestamp/block/profit"
        );
    }

    let gas = read_json(&dir.join(GAS_MATRIX), "NodeInterface gas matrix");
    assert_eq!(
        gas.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_nodeinterface_gas_matrix_v2"),
        "gas matrix schema must be aws_dryrun_nodeinterface_gas_matrix_v2"
    );
    assert_eq!(
        required_value_str(&gas, "source", "gas matrix"),
        "Arbitrum NodeInterface direct eth_call gasEstimateL1Component",
        "gas matrix must identify the direct historical NodeInterface method"
    );
    assert_eq!(
        required_value_str(&gas, "run_id", "gas matrix"),
        metadata_run_id,
        "gas matrix run_id must match run identity"
    );
    assert_eq!(
        required_value_str(&gas, "source_manifest_sha256", "gas matrix"),
        metadata_source_sha,
        "gas matrix source manifest must match run identity"
    );
    assert_eq!(
        required_value_str(&gas, "node_interface_address", "gas matrix").to_ascii_lowercase(),
        "0x00000000000000000000000000000000000000c8",
        "gas matrix must call Arbitrum NodeInterface at 0xC8"
    );
    assert_eq!(
        required_value_str(&gas, "method", "gas matrix"),
        "gasEstimateL1Component(address,bool,bytes)",
        "gas matrix must use gasEstimateL1Component"
    );
    assert_eq!(
        required_value_str(&gas, "selector", "gas matrix").to_ascii_lowercase(),
        "0x77d488a2",
        "gas matrix selector must be 0x77d488a2"
    );
    for key in [
        "run_identity_sha256",
        "candidate_sha256",
        "rpc_endpoint_sha256",
    ] {
        assert_sha256_digest(
            required_value_str(&gas, key, "gas matrix"),
            &format!("gas matrix {key}"),
        );
    }
    assert!(
        gas.get("samples")
            .and_then(Value::as_array)
            .is_some_and(|samples| !samples.is_empty()),
        "gas matrix must contain non-empty submit calldata samples"
    );
    let gas_samples = gas
        .get("samples")
        .and_then(Value::as_array)
        .expect("gas matrix samples checked above");
    let mut gas_sample_calldata_bytes = BTreeSet::<u64>::new();
    let mut gas_sample_keys = BTreeSet::<(u64, u128, u128)>::new();
    let mut sampled_candidate_ids = BTreeSet::<String>::new();
    for (idx, sample) in gas_samples.iter().enumerate() {
        let n = idx + 1;
        let candidate_id = required_value_str(sample, "candidate_id", "gas sample");
        assert!(
            sampled_candidate_ids.insert(candidate_id.to_string()),
            "gas matrix repeats candidate_id {candidate_id}"
        );
        let (
            candidate_target,
            candidate_block,
            candidate_calldata,
            candidate_bytes,
            candidate_l1_fee,
            candidate_l2_price,
        ) = candidate_direct_gas.get(candidate_id).unwrap_or_else(|| {
            panic!("gas sample {n} candidate_id {candidate_id} has no direct candidate tuple")
        });
        let target = required_value_str(sample, "target", "gas sample");
        assert_address(target, &format!("gas sample {n} target"));
        assert_eq!(
            target.to_ascii_lowercase(),
            candidate_target.to_ascii_lowercase(),
            "gas sample {n} target must match candidate mantis_executor"
        );
        let block_number = u64_field(sample, "block_number")
            .unwrap_or_else(|| panic!("gas sample {n} missing numeric block_number"));
        assert_eq!(
            block_number, *candidate_block,
            "gas sample {n} block_number must match candidate snapshot block"
        );
        let block_tag = required_value_str(sample, "block_tag", "gas sample");
        let parsed_block_tag = block_tag
            .strip_prefix("0x")
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .unwrap_or_else(|| panic!("gas sample {n} block_tag must be 0x hex"));
        assert_eq!(
            parsed_block_tag, block_number,
            "gas sample {n} block_tag must encode block_number"
        );
        let block_hash = required_value_str(sample, "block_hash", "gas sample");
        assert!(
            block_hash.len() == 66
                && block_hash.starts_with("0x")
                && block_hash[2..].chars().all(|ch| ch.is_ascii_hexdigit()),
            "gas sample {n} block_hash must be a 32-byte 0x hash"
        );
        let calldata_bytes = u64_field(sample, "calldata_bytes")
            .unwrap_or_else(|| panic!("gas sample {n} missing numeric calldata_bytes"));
        assert!(
            calldata_bytes > 0,
            "gas sample {n} calldata_bytes must be positive"
        );
        assert_eq!(
            calldata_bytes, *candidate_bytes,
            "gas sample {n} calldata_bytes must match candidate submit_calldata_bytes"
        );
        assert!(
            candidate_calldata.starts_with("0x")
                && candidate_calldata.len() > 2
                && (candidate_calldata.len() - 2) % 2 == 0
                && candidate_calldata[2..]
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit()),
            "gas sample {n} candidate executor_calldata must be non-empty byte hex"
        );
        assert_eq!(
            calldata_bytes as usize,
            (candidate_calldata.len() - 2) / 2,
            "gas sample {n} calldata_bytes must match executor_calldata bytes"
        );
        assert_eq!(
            required_value_str(sample, "calldata_sha256", "gas sample"),
            sha256_bytes(candidate_calldata.as_bytes()),
            "gas sample {n} calldata_sha256 must bind exact candidate executor_calldata"
        );
        for key in ["nodeinterface_calldata_sha256", "raw_result_sha256"] {
            assert_sha256_digest(
                required_value_str(sample, key, "gas sample"),
                &format!("gas sample {n} {key}"),
            );
        }
        let gas_estimate_for_l1 = decimal_u128(sample, "gas_estimate_for_l1")
            .unwrap_or_else(|| panic!("gas sample {n} missing decimal gas_estimate_for_l1"));
        let base_fee = decimal_u128(sample, "base_fee_wei")
            .unwrap_or_else(|| panic!("gas sample {n} missing decimal base_fee_wei"));
        let l1_base_fee_estimate = decimal_u128(sample, "l1_base_fee_estimate_wei")
            .unwrap_or_else(|| panic!("gas sample {n} missing decimal l1_base_fee_estimate_wei"));
        assert!(
            gas_estimate_for_l1 > 0 && base_fee > 0 && l1_base_fee_estimate > 0,
            "gas sample {n} decoded NodeInterface values must be positive"
        );
        let l1_fee = decimal_u128(sample, "l1_fee_wei")
            .unwrap_or_else(|| panic!("gas sample {n} missing decimal l1_fee_wei"));
        assert_eq!(
            gas_estimate_for_l1
                .checked_mul(base_fee)
                .expect("NodeInterface L1 fee multiplication overflow"),
            l1_fee,
            "gas sample {n} l1_fee_wei must equal gas_estimate_for_l1 * base_fee_wei"
        );
        let candidate_sample_l1 = decimal_u128(sample, "candidate_l1_fee_wei")
            .unwrap_or_else(|| panic!("gas sample {n} missing candidate_l1_fee_wei"));
        let l2_gas_price = decimal_u128(sample, "candidate_l2_gas_price_wei")
            .unwrap_or_else(|| panic!("gas sample {n} missing candidate_l2_gas_price_wei"));
        assert_eq!(
            candidate_sample_l1, *candidate_l1_fee,
            "gas sample {n} candidate_l1_fee_wei must match candidate JSONL"
        );
        assert_eq!(
            l2_gas_price, *candidate_l2_price,
            "gas sample {n} candidate_l2_gas_price_wei must match candidate JSONL"
        );
        assert_eq!(
            l1_fee, *candidate_l1_fee,
            "gas sample {n} direct NodeInterface L1 fee must match candidate"
        );
        assert_eq!(
            base_fee, *candidate_l2_price,
            "gas sample {n} direct NodeInterface base fee must match candidate"
        );
        gas_sample_calldata_bytes.insert(calldata_bytes);
        gas_sample_keys.insert((calldata_bytes, l1_fee, l2_gas_price));
    }
    for calldata_bytes in candidate_submit_calldata_bytes {
        assert!(
            gas_sample_calldata_bytes.contains(&calldata_bytes),
            "gas matrix does not cover candidate submit_calldata_bytes {calldata_bytes}"
        );
    }
    for (calldata_bytes, l1_fee, l2_gas_price) in candidate_gas_keys {
        assert!(
            gas_sample_keys
                .iter()
                .any(|(sample_bytes, sample_l1_fee, _)| {
                    *sample_bytes == calldata_bytes && *sample_l1_fee == l1_fee
                }),
            "gas matrix l1_fee_wei does not match candidate submit_calldata_bytes {calldata_bytes}"
        );
        assert!(
            gas_sample_keys
                .iter()
                .any(|(sample_bytes, _, sample_l2_gas_price)| {
                    *sample_bytes == calldata_bytes && *sample_l2_gas_price == l2_gas_price
                }),
            "gas matrix l2_gas_price_wei does not match candidate submit_calldata_bytes {calldata_bytes}"
        );
    }

    let parity = read_json(&dir.join(LIVE_PARITY), "live parity JSON");
    assert_eq!(
        parity.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_live_parity_v2"),
        "live parity schema must be aws_dryrun_live_parity_v2"
    );
    assert_eq!(
        required_value_str(&parity, "run_id", "live parity"),
        metadata_run_id,
        "live parity run_id must match run identity"
    );
    assert_eq!(
        required_value_str(&parity, "source_manifest_sha256", "live parity"),
        metadata_source_sha,
        "live parity source manifest must match run identity"
    );
    assert_eq!(
        parity.get("strict_byte_exact").and_then(Value::as_bool),
        Some(true),
        "live parity must be strict byte-exact"
    );
    assert_eq!(
        parity.get("gate_pass").and_then(Value::as_bool),
        Some(true),
        "live parity gate_pass must be true"
    );
    assert_eq!(
        parity.get("unsafe_overquotes").and_then(Value::as_u64),
        Some(0),
        "enabled/promoted venue parity must have zero unsafe over-quotes"
    );

    let required_families = parity
        .get("required_families")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .expect("live parity required_families must be non-empty");
    let promoted_families = parity
        .get("promotion_eligible_families")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .expect("live parity promotion_eligible_families must be non-empty");
    assert_eq!(
        u64_field(&parity, "required_family_count"),
        Some(required_families.len() as u64),
        "live parity required_family_count must match its list"
    );
    assert_eq!(
        u64_field(&parity, "promotion_eligible_family_count"),
        Some(promoted_families.len() as u64),
        "live parity promotion_eligible_family_count must match its list"
    );
    let required_names = required_families
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|name| !name.is_empty())
                .expect("required parity family must be a non-empty string")
        })
        .collect::<BTreeSet<_>>();
    let promoted_names = promoted_families
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|name| !name.is_empty())
                .expect("promoted parity family must be a non-empty string")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        required_names.len(),
        required_families.len(),
        "live parity required_families must be unique"
    );
    assert_eq!(
        promoted_names.len(),
        promoted_families.len(),
        "live parity promotion_eligible_families must be unique"
    );
    assert!(
        promoted_names.is_subset(&required_names),
        "promoted parity families must be a subset of required families"
    );
    let families = parity
        .get("families")
        .and_then(Value::as_object)
        .expect("live parity missing families object");
    for family in required_names {
        let evidence = families
            .get(family)
            .unwrap_or_else(|| panic!("live parity missing evidence for required family {family}"));
        assert_eq!(
            evidence.get("state_pinned").and_then(Value::as_bool),
            Some(true),
            "required parity family {family} must be state-pinned"
        );
        assert!(
            u64_field(evidence, "comparisons").is_some_and(|count| count > 0),
            "required parity family {family} must have nonzero comparisons"
        );
        assert_eq!(
            u64_field(evidence, "unresolved_errors"),
            Some(0),
            "required parity family {family} must have zero unresolved errors"
        );
        if promoted_names.contains(family) {
            assert_eq!(
                evidence.get("byte_exact").and_then(Value::as_bool),
                Some(true),
                "promoted parity family {family} must be byte-exact"
            );
            assert_eq!(
                u64_field(evidence, "disagree"),
                Some(0),
                "promoted parity family {family} must have zero disagreements"
            );
        }
    }
    assert_keccak_digest(
        required_value_str(&parity, "corpus_keccak256", "live parity"),
        "live parity corpus_keccak256",
    );
    assert_keccak_digest(
        required_value_str(&parity, "state_pin_source_keccak256", "live parity"),
        "live parity state_pin_source_keccak256",
    );
    assert_sha256_digest(
        required_value_str(&parity, "source_report_sha256", "live parity"),
        "live parity source_report_sha256",
    );

    validate_artifact_manifest(dir);

    // Hash and content bindings are deliberately checked after semantic checks. This preserves
    // precise diagnostics for malformed evidence while still making a passing bundle immutable.
    assert_eq!(
        metadata_run_identity_sha,
        sha256_file(&dir.join(RUN_IDENTITY)),
        "run_metadata run_identity_sha256 must match run_identity.json bytes"
    );
    assert_eq!(
        required_value_str(&gas, "run_identity_sha256", "gas matrix"),
        metadata_run_identity_sha,
        "gas matrix must bind the exact run_identity.json bytes"
    );
    assert_eq!(
        required_value_str(&gas, "candidate_sha256", "gas matrix"),
        sha256_file(&dir.join(CANDIDATES)),
        "gas matrix must bind the exact candidates.jsonl bytes"
    );
    assert_eq!(
        metadata_deployment_sha,
        sha256_file(&dir.join(DEPLOYMENT_MANIFEST)),
        "run_metadata deployment_manifest_sha256 must match deployment manifest bytes"
    );
    assert_eq!(
        metadata_placement_sha,
        sha256_file(&dir.join(PLACEMENT_PROOF)),
        "run_metadata placement_proof_sha256 must match placement proof bytes"
    );
    assert_eq!(
        metadata_monitoring_sha,
        sha256_file(&dir.join(MONITORING_SUMMARY)),
        "run_metadata monitoring_summary_sha256 must match monitoring summary bytes"
    );
    assert_eq!(
        metadata_host_telemetry_sha,
        sha256_file(&dir.join(HOST_TELEMETRY)),
        "run_metadata host_telemetry_sha256 must match raw host telemetry bytes"
    );
    assert_eq!(
        metadata_host_telemetry_summary_sha,
        sha256_file(&dir.join(HOST_TELEMETRY_SUMMARY)),
        "run_metadata host_telemetry_summary_sha256 must match telemetry summary bytes"
    );
    assert_eq!(
        metadata_sidecar_stdout_sha,
        sha256_file(&dir.join(SIDECAR_STDOUT)),
        "run_metadata sidecar_stdout_sha256 must match sidecar stdout bytes"
    );
    assert_eq!(
        metadata_sidecar_stderr_sha,
        sha256_file(&dir.join(SIDECAR_STDERR)),
        "run_metadata sidecar_stderr_sha256 must match sidecar stderr bytes"
    );
    assert_eq!(
        required_value_str(&parity, "corpus_keccak256", "live parity").to_ascii_lowercase(),
        keccak_file(&dir.join(CANDIDATES)).to_ascii_lowercase(),
        "live parity corpus Keccak must match candidates.jsonl"
    );
    assert_eq!(
        required_value_str(&parity, "state_pin_source_keccak256", "live parity")
            .to_ascii_lowercase(),
        keccak_file(&dir.join(PARITY_STATE_PIN)).to_ascii_lowercase(),
        "live parity state-pin Keccak must match parity_state_pin.json"
    );
    assert_eq!(
        required_value_str(&parity, "source_report_sha256", "live parity"),
        sha256_file(&dir.join(PARITY_SOURCE_REPORT)),
        "live parity source report hash must match parity_source_report.json"
    );
    let parity_source = read_json(
        &dir.join(PARITY_SOURCE_REPORT),
        "strict parity source report",
    );
    assert_eq!(
        parity_source.get("schema_version").and_then(Value::as_u64),
        Some(2),
        "strict parity source report schema_version must be 2"
    );
    required_value_str(
        &parity_source,
        "comparison_transcript",
        "strict parity source report",
    );
    let source_transcript_sha = required_value_str(
        &parity_source,
        "comparison_transcript_sha256",
        "strict parity source report",
    );
    assert_sha256_digest(
        source_transcript_sha,
        "strict parity source report comparison_transcript_sha256",
    );
    let source_transcript_keccak = required_value_str(
        &parity_source,
        "comparison_transcript_keccak256",
        "strict parity source report",
    );
    assert_unprefixed_keccak_digest(
        source_transcript_keccak,
        "strict parity source report comparison_transcript_keccak256",
    );
    let source_transcript_rows = u64_field(&parity_source, "comparison_transcript_rows")
        .filter(|rows| *rows > 0)
        .unwrap_or_else(|| {
            panic!("strict parity source report comparison_transcript_rows must be positive")
        });
    let transcript_rows = read_jsonl(
        &dir.join(FASTLANE_PARITY_TRANSCRIPT),
        "portable fast-lane parity transcript",
    );
    assert_eq!(
        source_transcript_sha,
        sha256_file(&dir.join(FASTLANE_PARITY_TRANSCRIPT)),
        "strict parity source report transcript SHA256 must match portable transcript bytes"
    );
    assert_eq!(
        source_transcript_keccak.to_ascii_lowercase(),
        unprefixed_keccak_file(&dir.join(FASTLANE_PARITY_TRANSCRIPT)).to_ascii_lowercase(),
        "strict parity source report transcript Keccak must match portable transcript bytes"
    );
    assert_eq!(
        source_transcript_rows,
        transcript_rows.len() as u64,
        "strict parity source report transcript row count must match portable transcript"
    );
    for key in [
        "run_id",
        "source_manifest_sha256",
        "strict_byte_exact",
        "gate_pass",
        "required_families",
        "required_family_count",
        "promotion_eligible_families",
        "promotion_eligible_family_count",
        "families",
        "corpus_keccak256",
        "state_pin_source_keccak256",
    ] {
        assert_eq!(
            parity_source.get(key),
            parity.get(key),
            "normalized live parity field {key} must match its strict source report"
        );
    }

    let portable_report = read_json(
        &dir.join(FASTLANE_PARITY_REPORT),
        "portable fast-lane parity report",
    );
    assert_eq!(
        portable_report
            .get("schema_version")
            .and_then(Value::as_u64),
        Some(2),
        "portable fast-lane parity report schema_version must be 2"
    );
    assert_eq!(
        required_value_str(&portable_report, "corpus", "portable parity report"),
        FASTLANE_PARITY_CORPUS,
        "portable parity corpus path must be relative to the report"
    );
    assert_eq!(
        required_value_str(
            &portable_report,
            "state_pin_source",
            "portable parity report",
        ),
        PARITY_STATE_PIN,
        "portable parity state-pin path must be relative to the report"
    );
    assert_eq!(
        required_value_str(
            &portable_report,
            "comparison_transcript",
            "portable parity report",
        ),
        FASTLANE_PARITY_TRANSCRIPT,
        "portable parity transcript path must be relative to the report"
    );
    for key in [
        "run_id",
        "source_manifest_sha256",
        "strict_byte_exact",
        "gate_pass",
        "required_families",
        "required_family_count",
        "promotion_eligible_families",
        "promotion_eligible_family_count",
        "families",
        "corpus_keccak256",
        "state_pin_source_keccak256",
        "comparison_transcript_sha256",
        "comparison_transcript_keccak256",
        "comparison_transcript_rows",
    ] {
        assert_eq!(
            portable_report.get(key),
            parity_source.get(key),
            "portable fast-lane report field {key} must match strict source evidence"
        );
    }

    let fastlane_manifest = read_json(
        &dir.join(FASTLANE_PARITY_MANIFEST),
        "fast-lane parity bundle manifest",
    );
    assert_eq!(
        fastlane_manifest.get("schema").and_then(Value::as_str),
        Some("aws_dryrun_fastlane_parity_bundle_v1"),
        "fast-lane parity manifest schema is invalid"
    );
    assert_eq!(
        required_value_str(&fastlane_manifest, "run_id", "fast-lane parity manifest"),
        metadata_run_id,
        "fast-lane parity manifest run_id must match run identity"
    );
    assert_eq!(
        required_value_str(
            &fastlane_manifest,
            "source_manifest_sha256",
            "fast-lane parity manifest",
        ),
        metadata_source_sha,
        "fast-lane parity manifest must target this exact source manifest"
    );
    for (key, expected_file) in [
        ("portable_report_file", FASTLANE_PARITY_REPORT),
        ("corpus_file", FASTLANE_PARITY_CORPUS),
        ("state_pin_file", PARITY_STATE_PIN),
        ("transcript_file", FASTLANE_PARITY_TRANSCRIPT),
    ] {
        assert_eq!(
            required_value_str(&fastlane_manifest, key, "fast-lane parity manifest"),
            expected_file,
            "fast-lane parity manifest {key} must use the portable relative file"
        );
    }
    for (key, actual) in [
        (
            "source_report_sha256",
            sha256_file(&dir.join(PARITY_SOURCE_REPORT)),
        ),
        (
            "portable_report_sha256",
            sha256_file(&dir.join(FASTLANE_PARITY_REPORT)),
        ),
        (
            "corpus_sha256",
            sha256_file(&dir.join(FASTLANE_PARITY_CORPUS)),
        ),
        ("state_pin_sha256", sha256_file(&dir.join(PARITY_STATE_PIN))),
        (
            "transcript_sha256",
            sha256_file(&dir.join(FASTLANE_PARITY_TRANSCRIPT)),
        ),
    ] {
        assert_eq!(
            required_value_str(&fastlane_manifest, key, "fast-lane parity manifest"),
            actual,
            "fast-lane parity manifest {key} hash mismatch"
        );
    }
    assert_eq!(
        fastlane_manifest.get("corpus_keccak256"),
        parity.get("corpus_keccak256"),
        "fast-lane parity manifest corpus Keccak must match strict evidence"
    );
    assert_eq!(
        sha256_file(&dir.join(FASTLANE_PARITY_CORPUS)),
        sha256_file(&dir.join(CANDIDATES)),
        "portable fast-lane corpus must preserve exact candidate corpus bytes"
    );
    assert_eq!(
        fastlane_manifest.get("state_pin_keccak256"),
        parity.get("state_pin_source_keccak256"),
        "fast-lane parity manifest state-pin Keccak must match strict evidence"
    );
    assert_eq!(
        fastlane_manifest.get("transcript_keccak256"),
        parity_source.get("comparison_transcript_keccak256"),
        "fast-lane parity manifest transcript Keccak must match strict source evidence"
    );
    assert_eq!(
        fastlane_manifest.get("transcript_rows"),
        parity_source.get("comparison_transcript_rows"),
        "fast-lane parity manifest transcript row count must match strict source evidence"
    );
    assert_eq!(
        fastlane_manifest.get("promotion_eligible_families"),
        parity.get("promotion_eligible_families"),
        "fast-lane parity manifest must derive the exact promotion list"
    );
    assert_eq!(
        fastlane_manifest.get("promotion_eligible_family_count"),
        parity.get("promotion_eligible_family_count"),
        "fast-lane parity manifest promotion count must match strict evidence"
    );
}

fn validate_aws_dryrun_artifacts(dir: &Path) {
    validate_aws_dryrun_artifacts_with_expected_binary(dir, None);
}

fn temp_artifact_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aws_dryrun_artifact_contract_{}_{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp artifact dir");
    dir
}

fn write_manifest_fixture(dir: &Path) {
    let files = artifact_payload_files()
        .into_iter()
        .map(|file| format!(r#""{file}":"{}""#, sha256_file(&dir.join(file))))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        dir.join(ARTIFACT_MANIFEST),
        format!(r#"{{"schema":"aws_dryrun_artifact_manifest_v2","files":{{{files}}}}}"#),
    )
    .expect("write artifact manifest");
}

fn write_complete_fixture(dir: &Path) {
    const ENGINE_SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SOURCE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fs::write(
        dir.join(ENV_REDACTED),
        "\
DRY_RUN=true
SUBMIT_TX=false
REVM_VALIDATE=1
PAPER_MODE_LOG=logs/paper.jsonl
CANDIDATE_RECORD_JSONL=logs/candidates.jsonl
",
    )
    .expect("write env");
    fs::write(dir.join(BINARY_SHA256), format!("{ENGINE_SHA}\n")).expect("write hash");

    let placement = serde_json::json!({
        "schema": "aws_dryrun_placement_proof_v2",
        "verdict": "PLACEMENT_READY",
        "objective": "submission_priority",
        "objective_selection": "explicit_cli_only",
        "region": "eu-west-1",
        "az_name": "eu-west-1a",
        "az_id": "euw1-az2",
        "subnet_id": "subnet-0123456789abcdef0",
        "instance_type": "c8gn.2xlarge",
        "submit_pin_ip": "198.51.100.20",
        "feed_pin_ip": "203.0.113.20",
        "source": {"source_manifest_sha256": SOURCE_SHA},
        "submit": {
            "tested_in_exact_chosen_az": true,
            "report_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "campaign_config_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "bundle_manifest_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
        },
        "feed": {
            "front_door_tcp_placement_authority": false,
            "report_sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "campaign_config_sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "bundle_manifest_sha256": "1111111111111111111111111111111111111111111111111111111111111111"
        },
        "counterpart_evidence": {"submit_colocated": true, "feed_colocated": true}
    });
    fs::write(dir.join(PLACEMENT_PROOF), placement.to_string()).expect("write placement proof");
    let placement_sha = sha256_file(&dir.join(PLACEMENT_PROOF));

    let deployment = serde_json::json!({
        "schema": "aws_dryrun_deployment_bundle_v1",
        "source_manifest_sha256": SOURCE_SHA,
        "build_manifest_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "config_bundle_sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "unfunded_lane_attestation_sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "placement_proof_sha256": placement_sha,
        "host_placement_attestation_sha256": "1111111111111111111111111111111111111111111111111111111111111111",
        "engine_sha256": ENGINE_SHA,
        "relay_sidecar_sha256": "2222222222222222222222222222222222222222222222222222222222222222"
    });
    fs::write(dir.join(DEPLOYMENT_MANIFEST), deployment.to_string())
        .expect("write deployment manifest");
    let deployment_sha = sha256_file(&dir.join(DEPLOYMENT_MANIFEST));

    let telemetry_row = serde_json::json!({
        "schema":"mantis_host_telemetry_v1","sample_index":0,
        "timestamp_utc":"2026-06-20T00:00:00.000000Z","monotonic_ns":1_000_000_000u64,
        "wall_utc_ns":1_780_000_000_000_000_000u64,
        "sampler":{"available":true,"source":"sched_getaffinity","pid":99,
            "affinity_cpus":[0],"housekeeping_core":0,"affinity_valid":true},
        "cpu":{"available":true,"source":"/proc/stat","per_core":{"cpu0":{}}},
        "scheduler":{"available":true,"source":"/proc/stat","context_switches":10},
        "process":{"available":true,"source":"/proc/101/stat","pid":101,"alive":true},
        "processes":{"available":true,
            "engine":{"available":true,"source":"/proc/101/stat","pid":101,"alive":true},
            "sidecar":{"available":true,"source":"/proc/102/stat","pid":102,"alive":true},
            "relay":{"available":true,"source":"/proc/103/stat","pid":103,"alive":true}},
        "irq":{"available":true,"source":"/proc/interrupts","vectors":{}},
        "softirq":{"available":true,"source":"/proc/softirqs","vectors":{}},
        "ena":{"available":true,"source":"/sys/class/net","interfaces":{"ens5":{
            "available":true,"driver":"ena","queues":{"rx":2,"tx":2},
            "rss":{"available":true},"coalescing":{"available":true},
            "allowance_counters":{"pps_allowance_exceeded":0},
            "allowance_deltas":{"pps_allowance_exceeded":0}}}},
        "softnet":{"available":true,"source":"/proc/net/softnet_stat","per_core":[]},
        "sockets":{"available":true,"source":"/proc/net","ipv4":{}},
        "network":{"available":true,"source":"/proc/net/dev","interfaces":{"ens5":{}}},
        "queue":{"available":true,"source":"/proc/loadavg","run_queue":0},
        "memory":{"available":true,"source":"/proc/meminfo","available_bytes":1},
        "psi":{"available":true,"cpu":{"available":true},"memory":{"available":true},"io":{"available":true}},
        "disk":{"available":true,"source":"/proc/diskstats","devices":{"nvme0n1":{}}},
        "clock":{"available":true,"source":"CLOCK_REALTIME/CLOCK_MONOTONIC_RAW","realtime_ns":1,
            "chrony":{"available":true,"stratum":1,"system_time_offset_seconds":0.000001,
                "last_offset_seconds":0.000002,"rms_offset_seconds":0.000003,
                "frequency_ppm":1.0,"residual_frequency_ppm":0.1,"skew_ppm":0.2}}
    });
    let mut telemetry_row_2 = telemetry_row.clone();
    telemetry_row_2["sample_index"] = Value::from(1);
    telemetry_row_2["monotonic_ns"] = Value::from(2_000_000_000u64);
    telemetry_row_2["timestamp_utc"] = Value::from("2026-06-20T00:00:01.000000Z");
    fs::write(
        dir.join(HOST_TELEMETRY),
        format!("{telemetry_row}\n{telemetry_row_2}\n"),
    )
    .expect("write host telemetry JSONL");
    let host_telemetry_sha = sha256_file(&dir.join(HOST_TELEMETRY));
    let host_telemetry_summary = serde_json::json!({
        "schema":"mantis_host_telemetry_summary_v1","status":"pass",
        "artifact":HOST_TELEMETRY,"sha256":host_telemetry_sha,
        "sampler_sha256":"9999999999999999999999999999999999999999999999999999999999999999",
        "row_count":2,"monitor_exit_status":0,"configured_interval_seconds":1.0,"relay_pid":103,
        "required_domains":host_telemetry_required_domains(),
        "coverage_start_monotonic_ns":1_000_000_000u64,
        "coverage_end_monotonic_ns":2_000_000_000u64,
        "max_gap_ns":1_000_000_000u64,"max_allowed_gap_ns":3_000_000_000u64
    });
    fs::write(
        dir.join(HOST_TELEMETRY_SUMMARY),
        host_telemetry_summary.to_string(),
    )
    .expect("write host telemetry summary");
    let host_telemetry_summary_sha = sha256_file(&dir.join(HOST_TELEMETRY_SUMMARY));
    fs::write(dir.join(SIDECAR_STDOUT), "relay_sidecar started\n").expect("write sidecar stdout");
    fs::write(dir.join(SIDECAR_STDERR), "").expect("write sidecar stderr");
    let sidecar_stdout_sha = sha256_file(&dir.join(SIDECAR_STDOUT));
    let sidecar_stderr_sha = sha256_file(&dir.join(SIDECAR_STDERR));

    let effective_command = "DRY_RUN=true SUBMIT_TX=false REVM_VALIDATE=1 engine dry-run";
    let run_identity = serde_json::json!({
        "schema": "aws_dryrun_harness_run_v1",
        "run_id": "fixture-run-1",
        "profile": "aws_evidence_fine",
        "status": "pass",
        "started_utc": "2026-06-20T00:00:00Z",
        "finished_utc": "2026-06-20T00:05:00Z",
        "requested_live_seconds": 300,
        "live_window_seconds": 300,
        "instrumented": true,
        "source_manifest_sha256": SOURCE_SHA,
        "engine_sha256": ENGINE_SHA,
        "relay_sidecar_sha256": "2222222222222222222222222222222222222222222222222222222222222222",
        "deployment_manifest_sha256": deployment_sha,
        "placement_proof_sha256": placement_sha,
        "effective_env_sha256": "3333333333333333333333333333333333333333333333333333333333333333",
        "effective_config_sha256": "4444444444444444444444444444444444444444444444444444444444444444",
        "effective_command": effective_command,
        "host_telemetry": host_telemetry_summary
    });
    fs::write(dir.join(RUN_IDENTITY), run_identity.to_string()).expect("write run identity");
    let run_identity_sha = sha256_file(&dir.join(RUN_IDENTITY));

    let heartbeat = serde_json::json!({
        "timestamp": "2026-06-20T00:04:55Z",
        "level": "INFO",
        "fields": {
            "message": "pipeline heartbeat",
            "submitted": 1,
            "submit_wire_attempts": 0,
            "submit_wire_acks": 0
        },
        "target": "engine::pipeline"
    });
    fs::write(dir.join(ENGINE_LOG), format!("{heartbeat}\n")).expect("write engine log");
    fs::copy(dir.join(ENGINE_LOG), dir.join(PARITY_STATE_PIN))
        .expect("copy engine log as parity state pin");
    let engine_log_sha = sha256_file(&dir.join(ENGINE_LOG));
    let state_pin_keccak = keccak_file(&dir.join(PARITY_STATE_PIN));
    fs::write(
        dir.join(MONITORING_SUMMARY),
        serde_json::json!({
            "schema": "aws_dryrun_monitoring_summary_v1",
            "run_id": "fixture-run-1",
            "source_manifest_sha256": SOURCE_SHA,
            "engine_sha256": ENGINE_SHA,
            "live_window_seconds": 300,
            "engine_log_artifact": ENGINE_LOG,
            "engine_log_sha256": engine_log_sha,
            "heartbeat_rows": 1,
            "counter_maxima": {
                "submitted": 1,
                "submit_wire_attempts": 0,
                "submit_wire_acks": 0
            }
        })
        .to_string(),
    )
    .expect("write monitoring summary");
    let monitoring_sha = sha256_file(&dir.join(MONITORING_SUMMARY));

    fs::write(
        dir.join(RUN_METADATA),
        serde_json::json!({
            "schema": "aws_dryrun_run_metadata_v2",
            "assembled_at_utc": "2026-06-20T00:05:02Z",
            "run_id": "fixture-run-1",
            "profile": "aws_evidence_fine",
            "run_started_utc": "2026-06-20T00:00:00Z",
            "run_finished_utc": "2026-06-20T00:05:00Z",
            "run_duration_seconds": 300,
            "source_dir": dir.display().to_string(),
            "artifact_dir": dir.display().to_string(),
            "engine_bin": "/tmp/engine",
            "binary_sha256": ENGINE_SHA,
            "command_line": effective_command,
            "source_manifest_sha256": SOURCE_SHA,
            "run_identity_sha256": run_identity_sha,
            "deployment_manifest_sha256": deployment_sha,
            "placement_proof_sha256": placement_sha,
            "monitoring_summary_sha256": monitoring_sha,
            "host_telemetry_sha256": host_telemetry_sha,
            "host_telemetry_summary_sha256": host_telemetry_summary_sha,
            "sidecar_stdout_sha256": sidecar_stdout_sha,
            "sidecar_stderr_sha256": sidecar_stderr_sha
        })
        .to_string(),
    )
    .expect("write run metadata");
    fs::write(
        dir.join(RAW_FEED),
        r#"{"messages":[{"sequenceNumber":100,"message":{"message":{"header":{"timestamp":1780000000}}}}]}"#,
    )
    .expect("write raw feed");
    fs::write(
        dir.join(FRESHNESS),
        r#"{"seq":100,"sequencer_timestamp":1780000000,"capture_recv_ns":1780000001234567890,"event_age_ms":250}"#,
    )
    .expect("write freshness");
    fs::write(
        dir.join(FRESHNESS_REPORT),
        r#"{"candidate_count":1,"joined_candidate_count":1,"missing_join_count":0,"p95_event_age_ms":250,"p99_event_age_ms":250,"max_event_age_ms":250,"stale_threshold_ms":1000,"stale_candidate_count":0,"accepted_stale_candidate_count":0,"local_laptop_over_1s_allowed_only_if_rejected":true}"#,
    )
    .expect("write freshness report");
    let calldata = format!("0x{}", "de".repeat(388));
    let candidate = serde_json::json!({
        "candidate_id":"seq:100:route:362","block":465686729,"snapshot_block":465686729,
        "tx_hash":"0x1111111111111111111111111111111111111111111111111111111111111111",
        "log_index":0,"state_point":"replay_sequence","route_id":362,
        "tokens":["0x0000000000000000000000000000000000000001","0x0000000000000000000000000000000000000002","0x0000000000000000000000000000000000000001"],
        "pools":["0x0000000000000000000000000000000000000003","0x0000000000000000000000000000000000000004"],
        "exchange_ids":[1,2],
        "exchange_metadata":[
            {"family":"UniswapV3","fee_pips_or_bps":500,"pool":"0x0000000000000000000000000000000000000003"},
            {"family":"PancakeV3","fee_pips_or_bps":100,"pool":"0x0000000000000000000000000000000000000004"}
        ],
        "feed_seq":100,"recv_ns":1780000001234567890u64,"transaction_index":7,
        "trigger_tx_hash":"0x1111111111111111111111111111111111111111111111111111111111111111",
        "trigger_seq":100,"trigger_recv_ns":1780000001234567890u64,
        "state_point_mode":"LiveTip","calldata":calldata,"mantis_calldata":calldata,
        "executor_calldata":calldata,"mantis_executor":"0x00000000000000000000000000000000000000aa",
        "revm_enabled":true,"revm_result":"success","reject_reason":"none",
        "gross_profit_wei":"2000000000000000","gas_used_units":500000,
        "measured_execution_gas_used":500000,"l2_gas_price_wei":"20000000",
        "l1_fee_wei":"10000000000000","estimated_gas_wei":"1000000000000000",
        "total_gas_wei":"1000000000000000","net_profit_wei":"1000000000000000",
        "raw_calldata_bytes":388,"submit_calldata_bytes":388,"submit_gas_limit":1300000,
        "submit_min_profit_wei":"1200000000000000"
    });
    fs::write(dir.join(CANDIDATES), candidate.to_string()).expect("write candidates");
    fs::copy(dir.join(CANDIDATES), dir.join(FASTLANE_PARITY_CORPUS))
        .expect("write portable fast-lane corpus");
    let candidate_sha = sha256_file(&dir.join(CANDIDATES));
    let candidate_keccak = keccak_file(&dir.join(CANDIDATES));
    fs::write(
        dir.join(PAPER),
        r#"{"timestamp_ns":1780000001234567890,"route_id":362,"block_number_seen":465686729,"amount_in":"1000000000000000000","gross_profit_wei":"2000000000000000","net_profit_wei":"1000000000000000","pools":["0x3","0x4"]}"#,
    )
    .expect("write paper");
    let gas = serde_json::json!({
        "schema":"aws_dryrun_nodeinterface_gas_matrix_v2",
        "source":"Arbitrum NodeInterface direct eth_call gasEstimateL1Component",
        "run_id":"fixture-run-1","generated_at_utc":"2026-06-20T00:05:01Z",
        "source_manifest_sha256":SOURCE_SHA,"run_identity_sha256":run_identity_sha,
        "candidate_sha256":candidate_sha,
        "rpc_endpoint_sha256":"5555555555555555555555555555555555555555555555555555555555555555",
        "node_interface_address":"0x00000000000000000000000000000000000000C8",
        "method":"gasEstimateL1Component(address,bool,bytes)","selector":"0x77d488a2",
        "samples":[{
            "candidate_id":"seq:100:route:362","target":"0x00000000000000000000000000000000000000aa",
            "block_number":465686729,"block_tag":"0x1bc1d0c9",
            "block_hash":"0x6666666666666666666666666666666666666666666666666666666666666666",
            "calldata_sha256":sha256_bytes(calldata.as_bytes()),"calldata_bytes":388,
            "nodeinterface_calldata_sha256":"7777777777777777777777777777777777777777777777777777777777777777",
            "raw_result_sha256":"8888888888888888888888888888888888888888888888888888888888888888",
            "gas_estimate_for_l1":"500000","base_fee_wei":"20000000",
            "l1_base_fee_estimate_wei":"30000000000","l1_fee_wei":"10000000000000",
            "candidate_l1_fee_wei":"10000000000000","candidate_l2_gas_price_wei":"20000000"
        }]
    });
    fs::write(dir.join(GAS_MATRIX), gas.to_string()).expect("write gas");

    let transcript = serde_json::json!({
        "schema":"mantis_parity_comparison_v1",
        "candidate_id":"fixture-parity-1",
        "family":"UniswapV3",
        "chain_id":42161,
        "pin_block":465686729,
        "pin_block_hash":"0x6666666666666666666666666666666666666666666666666666666666666666",
        "pool_identity":"0x7777777777777777777777777777777777777777777777777777777777777777",
        "token_in":"0x0000000000000000000000000000000000000001",
        "token_out":"0x0000000000000000000000000000000000000002",
        "direction":"zero_for_one",
        "amount_in":"1000000000000000000",
        "kernel_amount_out":"999000000000000000",
        "authoritative_amount_out":"999000000000000000",
        "absolute_difference_wei":"0",
        "exact":true,
        "mechanical_edge_cases":[],
        "mechanical_variants":[],
        "authoritative_address":"0x00000000000000000000000000000000000000aa",
        "outcome":"resolved"
    });
    fs::write(
        dir.join(FASTLANE_PARITY_TRANSCRIPT),
        format!("{transcript}\n"),
    )
    .expect("write portable parity transcript");
    let transcript_sha = sha256_file(&dir.join(FASTLANE_PARITY_TRANSCRIPT));
    let transcript_keccak = unprefixed_keccak_file(&dir.join(FASTLANE_PARITY_TRANSCRIPT));
    let families = serde_json::json!({
        "UniswapV3":{"state_pinned":true,"comparisons":10,"disagree":0,"unresolved_errors":0,"byte_exact":true,"verdict":"pass"},
        "PancakeV3":{"state_pinned":true,"comparisons":8,"disagree":0,"unresolved_errors":0,"byte_exact":true,"verdict":"pass"}
    });
    let parity_source = serde_json::json!({
        "schema_version":2,"strict_byte_exact":true,"source_manifest_sha256":SOURCE_SHA,
        "run_id":"fixture-run-1","corpus":dir.join(CANDIDATES).display().to_string(),
        "corpus_keccak256":candidate_keccak,
        "state_pin_source":dir.join(PARITY_STATE_PIN).display().to_string(),
        "state_pin_source_keccak256":state_pin_keccak,
        "comparison_transcript":dir.join(FASTLANE_PARITY_TRANSCRIPT).display().to_string(),
        "comparison_transcript_sha256":transcript_sha,
        "comparison_transcript_keccak256":transcript_keccak,
        "comparison_transcript_rows":1,
        "required_families":["UniswapV3","PancakeV3"],"required_family_count":2,
        "promotion_eligible_families":["UniswapV3","PancakeV3"],
        "promotion_eligible_family_count":2,"families":families,"gate_pass":true
    });
    fs::write(dir.join(PARITY_SOURCE_REPORT), parity_source.to_string())
        .expect("write parity source report");
    let parity_source_sha = sha256_file(&dir.join(PARITY_SOURCE_REPORT));
    let parity = serde_json::json!({
        "schema":"aws_dryrun_live_parity_v2","generated_at_utc":"2026-06-20T00:05:01Z",
        "run_id":"fixture-run-1","source_manifest_sha256":SOURCE_SHA,
        "strict_byte_exact":true,"gate_pass":true,
        "required_families":["UniswapV3","PancakeV3"],"required_family_count":2,
        "promotion_eligible_families":["UniswapV3","PancakeV3"],
        "promotion_eligible_family_count":2,"families":families,
        "corpus":dir.join(CANDIDATES).display().to_string(),"corpus_keccak256":candidate_keccak,
        "state_pin_source":dir.join(PARITY_STATE_PIN).display().to_string(),
        "state_pin_source_keccak256":state_pin_keccak,"source_report":dir.join(PARITY_SOURCE_REPORT).display().to_string(),
        "source_report_sha256":parity_source_sha,"unsafe_overquotes":0
    });
    fs::write(dir.join(LIVE_PARITY), parity.to_string()).expect("write parity");
    let mut portable_report = parity_source.clone();
    portable_report["corpus"] = Value::from(FASTLANE_PARITY_CORPUS);
    portable_report["state_pin_source"] = Value::from(PARITY_STATE_PIN);
    portable_report["comparison_transcript"] = Value::from(FASTLANE_PARITY_TRANSCRIPT);
    fs::write(
        dir.join(FASTLANE_PARITY_REPORT),
        portable_report.to_string(),
    )
    .expect("write portable parity report");
    let portable_report_sha = sha256_file(&dir.join(FASTLANE_PARITY_REPORT));
    let fastlane_manifest = serde_json::json!({
        "schema":"aws_dryrun_fastlane_parity_bundle_v1","run_id":"fixture-run-1",
        "source_manifest_sha256":SOURCE_SHA,"source_report_sha256":parity_source_sha,
        "portable_report_file":FASTLANE_PARITY_REPORT,
        "portable_report_sha256":portable_report_sha,
        "corpus_file":FASTLANE_PARITY_CORPUS,"corpus_sha256":candidate_sha,
        "corpus_keccak256":candidate_keccak,"state_pin_file":PARITY_STATE_PIN,
        "state_pin_sha256":sha256_file(&dir.join(PARITY_STATE_PIN)),
        "state_pin_keccak256":state_pin_keccak,
        "transcript_file":FASTLANE_PARITY_TRANSCRIPT,"transcript_sha256":transcript_sha,
        "transcript_keccak256":transcript_keccak,"transcript_rows":1,
        "promotion_eligible_families":["UniswapV3","PancakeV3"],
        "promotion_eligible_family_count":2
    });
    fs::write(
        dir.join(FASTLANE_PARITY_MANIFEST),
        fastlane_manifest.to_string(),
    )
    .expect("write fast-lane parity manifest");
    write_manifest_fixture(dir);
}

fn mutate_host_telemetry_fixture(dir: &Path, mutator: impl Fn(&mut Value)) {
    let mut rows = read_jsonl(&dir.join(HOST_TELEMETRY), "host telemetry mutation fixture");
    for row in &mut rows {
        mutator(row);
    }
    let payload = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(dir.join(HOST_TELEMETRY), payload).expect("rewrite host telemetry fixture");
    let mut summary = read_json(
        &dir.join(HOST_TELEMETRY_SUMMARY),
        "host telemetry mutation summary",
    );
    summary["sha256"] = Value::from(sha256_file(&dir.join(HOST_TELEMETRY)));
    fs::write(dir.join(HOST_TELEMETRY_SUMMARY), summary.to_string())
        .expect("rewrite host telemetry mutation summary");
}

#[test]
fn synthetic_complete_aws_dryrun_bundle_passes_contract() {
    let dir = temp_artifact_dir("complete");
    write_complete_fixture(&dir);
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
fn contract_rejects_missing_no_submit_monitoring_summary() {
    let dir = temp_artifact_dir("missing_no_submit_monitoring_summary");
    write_complete_fixture(&dir);
    fs::remove_file(dir.join(MONITORING_SUMMARY)).expect("remove monitoring summary");

    let result = std::panic::catch_unwind(|| validate_aws_dryrun_artifacts(&dir));
    assert!(
        result.is_err(),
        "artifact contract must reject a bundle that cannot prove heartbeat coverage and zero wire counters"
    );
}

#[test]
fn contract_rejects_missing_host_telemetry_inputs() {
    for (name, file) in [
        ("missing_host_telemetry", HOST_TELEMETRY),
        ("missing_host_telemetry_summary", HOST_TELEMETRY_SUMMARY),
    ] {
        let dir = temp_artifact_dir(name);
        write_complete_fixture(&dir);
        fs::remove_file(dir.join(file)).expect("remove host telemetry input");
        let result = std::panic::catch_unwind(|| validate_aws_dryrun_artifacts(&dir));
        assert!(
            result.is_err(),
            "artifact contract must reject a bundle missing {file}"
        );
    }
}

#[test]
fn contract_rejects_mutated_host_telemetry_bytes() {
    let dir = temp_artifact_dir("mutated_host_telemetry");
    write_complete_fixture(&dir);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join(HOST_TELEMETRY))
        .expect("open host telemetry for mutation");
    file.write_all(b"\n").expect("mutate host telemetry bytes");
    let result = std::panic::catch_unwind(|| validate_aws_dryrun_artifacts(&dir));
    assert!(
        result.is_err(),
        "artifact contract must reject host telemetry bytes that no longer match the summary hash"
    );
}

#[test]
fn contract_rejects_dead_aws_relay_telemetry() {
    let dir = temp_artifact_dir("dead_aws_relay");
    write_complete_fixture(&dir);
    mutate_host_telemetry_fixture(&dir, |row| {
        row["processes"]["relay"]["alive"] = Value::from(false);
    });
    let identity = read_json(&dir.join(RUN_IDENTITY), "run identity JSON");
    let result = std::panic::catch_unwind(|| validate_host_telemetry(&dir, &identity));
    assert!(result.is_err(), "AWS telemetry must reject relay death");
}

#[test]
fn contract_rejects_unavailable_aws_domain() {
    let dir = temp_artifact_dir("unavailable_aws_irq");
    write_complete_fixture(&dir);
    mutate_host_telemetry_fixture(&dir, |row| {
        row["irq"] = serde_json::json!({
            "available":false,"source":"/proc/interrupts","reason":"permission_denied"
        });
    });
    let identity = read_json(&dir.join(RUN_IDENTITY), "run identity JSON");
    let result = std::panic::catch_unwind(|| validate_host_telemetry(&dir, &identity));
    assert!(
        result.is_err(),
        "AWS telemetry must reject typed-unavailable required domains"
    );
}

#[test]
fn contract_rejects_unavailable_aws_chrony_discipline() {
    let dir = temp_artifact_dir("unavailable_aws_chrony");
    write_complete_fixture(&dir);
    mutate_host_telemetry_fixture(&dir, |row| {
        row["clock"]["chrony"] = serde_json::json!({
            "available":false,"source":"chronyc","reason":"command_not_found"
        });
    });
    let identity = read_json(&dir.join(RUN_IDENTITY), "run identity JSON");
    let result = std::panic::catch_unwind(|| validate_host_telemetry(&dir, &identity));
    assert!(
        result.is_err(),
        "AWS telemetry must reject unavailable chrony discipline"
    );
}

#[test]
fn contract_rejects_incomplete_aws_ena_evidence() {
    for (name, field) in [
        ("unavailable_aws_ena_rss", "rss"),
        ("unavailable_aws_ena_coalescing", "coalescing"),
    ] {
        let dir = temp_artifact_dir(name);
        write_complete_fixture(&dir);
        mutate_host_telemetry_fixture(&dir, |row| {
            row["ena"]["interfaces"]["ens5"][field] = serde_json::json!({
                "available":false,"source":"ethtool","reason":"exit_status:1"
            });
        });
        let identity = read_json(&dir.join(RUN_IDENTITY), "run identity JSON");
        let result = std::panic::catch_unwind(|| validate_host_telemetry(&dir, &identity));
        assert!(
            result.is_err(),
            "AWS telemetry must reject unavailable ENA {field} evidence"
        );
    }

    let dir = temp_artifact_dir("negative_aws_ena_allowance");
    write_complete_fixture(&dir);
    mutate_host_telemetry_fixture(&dir, |row| {
        row["ena"]["interfaces"]["ens5"]["allowance_deltas"]["pps_allowance_exceeded"] =
            Value::from(-1);
    });
    let identity = read_json(&dir.join(RUN_IDENTITY), "run identity JSON");
    let result = std::panic::catch_unwind(|| validate_host_telemetry(&dir, &identity));
    assert!(
        result.is_err(),
        "AWS telemetry must reject negative ENA allowance deltas"
    );
}

#[test]
fn contract_rejects_missing_or_mutated_sidecar_logs() {
    for (name, file) in [
        ("missing_sidecar_stdout", SIDECAR_STDOUT),
        ("missing_sidecar_stderr", SIDECAR_STDERR),
    ] {
        let dir = temp_artifact_dir(name);
        write_complete_fixture(&dir);
        fs::remove_file(dir.join(file)).expect("remove sidecar log");
        let result = std::panic::catch_unwind(|| validate_aws_dryrun_artifacts(&dir));
        assert!(
            result.is_err(),
            "artifact contract must reject a bundle missing {file}"
        );
    }

    let dir = temp_artifact_dir("mutated_sidecar_stdout");
    write_complete_fixture(&dir);
    fs::write(dir.join(SIDECAR_STDOUT), "mutated sidecar log\n").expect("mutate sidecar stdout");
    let result = std::panic::catch_unwind(|| validate_aws_dryrun_artifacts(&dir));
    assert!(
        result.is_err(),
        "artifact contract must reject sidecar log bytes that no longer match their hashes"
    );
}

#[test]
fn contract_rejects_missing_fastlane_parity_transcript() {
    let dir = temp_artifact_dir("missing_fastlane_parity_transcript");
    write_complete_fixture(&dir);
    fs::remove_file(dir.join(FASTLANE_PARITY_TRANSCRIPT)).expect("remove parity transcript");

    let result = std::panic::catch_unwind(|| validate_aws_dryrun_artifacts(&dir));
    assert!(
        result.is_err(),
        "artifact contract must reject a bundle without the transcript required by strict parity"
    );
}

#[test]
#[ignore = "requires AWS_DRYRUN_ARTIFACT_DIR from a completed aws_evidence_fine run"]
fn externally_supplied_aws_dryrun_bundle_passes_contract() {
    let dir = std::env::var("AWS_DRYRUN_ARTIFACT_DIR")
        .expect("AWS_DRYRUN_ARTIFACT_DIR must name the assembled real AWS evidence bundle");
    validate_aws_dryrun_artifacts(Path::new(&dir));
}

#[test]
#[should_panic(expected = "binary.sha256 must match expected release binary")]
fn contract_rejects_binary_hash_mismatch_when_expected_hash_is_supplied() {
    let dir = temp_artifact_dir("binary_hash_mismatch");
    write_complete_fixture(&dir);
    validate_aws_dryrun_artifacts_with_expected_binary(
        &dir,
        Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    );
}

#[test]
#[should_panic(expected = "AWS dry-run artifact missing required file")]
fn contract_rejects_missing_run_metadata() {
    let dir = temp_artifact_dir("missing_run_metadata");
    write_complete_fixture(&dir);
    let _ = fs::remove_file(dir.join("run_metadata.json"));
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "run_metadata binary_sha256 must match binary.sha256")]
fn contract_rejects_run_metadata_binary_hash_mismatch() {
    let dir = temp_artifact_dir("run_metadata_binary_hash_mismatch");
    write_complete_fixture(&dir);
    fs::write(
        dir.join("run_metadata.json"),
        r#"{"schema":"aws_dryrun_run_metadata_v2","assembled_at_utc":"2026-06-20T00:00:00Z","source_dir":"/tmp/source","artifact_dir":"/tmp/artifact","engine_bin":"/tmp/engine","binary_sha256":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","command_line":"DRY_RUN=true SUBMIT_TX=false REVM_VALIDATE=1 engine"}"#,
    )
    .expect("write mismatched run metadata");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "run_metadata run_duration_seconds must be at least 300")]
fn contract_rejects_short_run_duration_metadata() {
    let dir = temp_artifact_dir("short_run_duration");
    write_complete_fixture(&dir);
    fs::write(
        dir.join("run_metadata.json"),
        r#"{"schema":"aws_dryrun_run_metadata_v2","assembled_at_utc":"2026-06-20T00:00:02Z","run_started_utc":"2026-06-20T00:00:00Z","run_finished_utc":"2026-06-20T00:00:01Z","run_duration_seconds":1,"source_dir":"/tmp/source","artifact_dir":"/tmp/artifact","engine_bin":"/tmp/engine","binary_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","command_line":"DRY_RUN=true SUBMIT_TX=false REVM_VALIDATE=1 engine"}"#,
    )
    .expect("write short-duration run metadata");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "run_metadata command_line must include SUBMIT_TX=false")]
fn contract_rejects_run_metadata_command_without_submit_tx_false() {
    let dir = temp_artifact_dir("command_missing_submit_false");
    write_complete_fixture(&dir);
    fs::write(
        dir.join("run_metadata.json"),
        r#"{"schema":"aws_dryrun_run_metadata_v2","assembled_at_utc":"2026-06-20T00:05:02Z","run_started_utc":"2026-06-20T00:00:00Z","run_finished_utc":"2026-06-20T00:05:00Z","run_duration_seconds":300,"source_dir":"/tmp/source","artifact_dir":"/tmp/artifact","engine_bin":"/tmp/engine","binary_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","command_line":"DRY_RUN=true REVM_VALIDATE=1 engine"}"#,
    )
    .expect("write command metadata without SUBMIT_TX=false");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "run_metadata command_line must not include SUBMIT_TX=true")]
fn contract_rejects_run_metadata_command_with_submit_tx_true() {
    let dir = temp_artifact_dir("command_submit_true");
    write_complete_fixture(&dir);
    fs::write(
        dir.join("run_metadata.json"),
        r#"{"schema":"aws_dryrun_run_metadata_v2","assembled_at_utc":"2026-06-20T00:05:02Z","run_started_utc":"2026-06-20T00:00:00Z","run_finished_utc":"2026-06-20T00:05:00Z","run_duration_seconds":300,"source_dir":"/tmp/source","artifact_dir":"/tmp/artifact","engine_bin":"/tmp/engine","binary_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","command_line":"DRY_RUN=true SUBMIT_TX=true REVM_VALIDATE=1 engine"}"#,
    )
    .expect("write unsafe command metadata");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "AWS dry-run artifact missing required file")]
fn contract_rejects_missing_artifact_manifest() {
    let dir = temp_artifact_dir("missing_artifact_manifest");
    write_complete_fixture(&dir);
    let _ = fs::remove_file(dir.join("artifact_manifest.json"));
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "artifact_manifest hash mismatch for candidates.jsonl")]
fn contract_rejects_stale_artifact_manifest_hash() {
    let dir = temp_artifact_dir("stale_artifact_manifest");
    write_complete_fixture(&dir);
    let candidates = read(&dir.join(CANDIDATES), "candidate JSONL");
    fs::write(dir.join(CANDIDATES), format!("{candidates}\n"))
        .expect("write byte-drifted candidates");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "redacted AWS env must contain SUBMIT_TX=false")]
fn contract_rejects_submit_tx_true() {
    let dir = temp_artifact_dir("submit_true");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(ENV_REDACTED),
        "\
DRY_RUN=true
SUBMIT_TX=true
REVM_VALIDATE=1
",
    )
    .expect("write unsafe env");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "redacted AWS dry-run env must not load PRIVATE_KEY")]
fn contract_rejects_private_key_in_dryrun_env() {
    let dir = temp_artifact_dir("private_key");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(ENV_REDACTED),
        "\
DRY_RUN=true
SUBMIT_TX=false
REVM_VALIDATE=1
PRIVATE_KEY=REDACTED
",
    )
    .expect("write unsafe env");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "redacted AWS dry-run env must not expose ARBITRUM_RPC_URL")]
fn contract_rejects_rpc_provider_url_in_dryrun_env() {
    let dir = temp_artifact_dir("rpc_provider_url");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(ENV_REDACTED),
        "\
DRY_RUN=true
SUBMIT_TX=false
REVM_VALIDATE=1
ARBITRUM_RPC_URL=https://arb-mainnet.g.alchemy.com/v2/REAL_PROVIDER_KEY
",
    )
    .expect("write RPC URL env");
    write_manifest_fixture(&dir);
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "redacted AWS dry-run env must not depend on EXECUTOR_ADDRESS")]
fn contract_rejects_executor_address_dependency_in_dryrun_env() {
    let dir = temp_artifact_dir("executor_address_dependency");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(ENV_REDACTED),
        "\
DRY_RUN=true
SUBMIT_TX=false
REVM_VALIDATE=1
EXECUTOR_ADDRESS=0x0000000000000000000000000000000000000001
",
    )
    .expect("write executor-dependent env");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "does not join to freshness sidecar")]
fn contract_rejects_unjoinable_candidate_freshness() {
    let dir = temp_artifact_dir("unjoinable_freshness");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(CANDIDATES),
        r#"{"route_id":362,"feed_seq":999,"recv_ns":1780000001234567890,"revm_enabled":true,"revm_result":"success","reject_reason":"none","gross_profit_wei":"2000000000000000","gas_used_units":500000,"measured_execution_gas_used":500000,"l2_gas_price_wei":"20000000","l1_fee_wei":"10000000000000","estimated_gas_wei":"1000000000000000","total_gas_wei":"1000000000000000","net_profit_wei":"1000000000000000","raw_calldata_bytes":388,"submit_calldata_bytes":388,"submit_gas_limit":1300000,"submit_min_profit_wei":"1200000000000000","tokens":["0x1","0x2"],"pools":["0x3","0x4"]}"#,
    )
    .expect("write unjoinable candidate");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "freshness row 1 seq 100 does not exist in raw feed capture")]
fn contract_rejects_freshness_seq_missing_from_raw_feed() {
    let dir = temp_artifact_dir("freshness_seq_missing_from_raw");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(RAW_FEED),
        r#"{"messages":[{"sequenceNumber":101,"message":{"message":{"header":{"timestamp":1780000000}}}}]}"#,
    )
    .expect("write raw feed with different seq");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(
    expected = "freshness row 1 sequencer_timestamp does not match raw feed header timestamp"
)]
fn contract_rejects_freshness_timestamp_mismatch_with_raw_feed() {
    let dir = temp_artifact_dir("freshness_timestamp_mismatch");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(FRESHNESS),
        r#"{"seq":100,"sequencer_timestamp":1780000999,"capture_recv_ns":1780000001234567890,"event_age_ms":250}"#,
    )
    .expect("write mismatched freshness timestamp");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "candidate row 1 recv_ns does not match freshness capture_recv_ns")]
fn contract_rejects_candidate_recv_ns_mismatch_with_freshness() {
    let dir = temp_artifact_dir("candidate_recv_ns_mismatch");
    write_complete_fixture(&dir);
    let mut rows = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    rows[0]["recv_ns"] = Value::from(1_780_000_009_999_999_999u64);
    fs::write(dir.join(CANDIDATES), rows[0].to_string())
        .expect("write candidate with mismatched recv_ns");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "accepted candidate row 1 trigger_seq must match feed_seq")]
fn contract_rejects_accepted_candidate_trigger_seq_mismatch() {
    let dir = temp_artifact_dir("trigger_seq_mismatch");
    write_complete_fixture(&dir);
    let mut rows = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    rows[0]["trigger_seq"] = Value::from(101u64);
    fs::write(dir.join(CANDIDATES), rows[0].to_string())
        .expect("write candidate with mismatched trigger_seq");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "accepted candidate row 1 trigger_recv_ns must match recv_ns")]
fn contract_rejects_accepted_candidate_trigger_recv_ns_mismatch() {
    let dir = temp_artifact_dir("trigger_recv_ns_mismatch");
    write_complete_fixture(&dir);
    let mut rows = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    rows[0]["trigger_recv_ns"] = Value::from(1_780_000_009_999_999_999u64);
    fs::write(dir.join(CANDIDATES), rows[0].to_string())
        .expect("write candidate with mismatched trigger_recv_ns");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "freshness report p99_event_age_ms must match joined candidates")]
fn contract_rejects_wrong_freshness_percentiles() {
    let dir = temp_artifact_dir("wrong_percentiles");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(FRESHNESS_REPORT),
        r#"{"candidate_count":1,"joined_candidate_count":1,"missing_join_count":0,"p95_event_age_ms":250,"p99_event_age_ms":999,"max_event_age_ms":250,"stale_threshold_ms":1000,"stale_candidate_count":0,"accepted_stale_candidate_count":0,"local_laptop_over_1s_allowed_only_if_rejected":true}"#,
    )
    .expect("write wrong freshness report");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "stale candidate row 1 was accepted before would-submit")]
fn contract_rejects_stale_would_submit_candidate() {
    let dir = temp_artifact_dir("stale_would_submit");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(FRESHNESS),
        r#"{"seq":100,"sequencer_timestamp":1780000000,"capture_recv_ns":1780000001234567890,"event_age_ms":1500}"#,
    )
    .expect("write stale freshness");
    fs::write(
        dir.join(FRESHNESS_REPORT),
        r#"{"candidate_count":1,"joined_candidate_count":1,"missing_join_count":0,"p95_event_age_ms":1500,"p99_event_age_ms":1500,"max_event_age_ms":1500,"stale_threshold_ms":1000,"stale_candidate_count":1,"accepted_stale_candidate_count":1,"local_laptop_over_1s_allowed_only_if_rejected":true}"#,
    )
    .expect("write stale freshness report");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "stale candidate row 2 must carry stale/freshness reject_reason")]
fn contract_rejects_stale_candidate_with_non_stale_reject_reason() {
    let dir = temp_artifact_dir("stale_non_stale_reject");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(RAW_FEED),
        "\
{\"messages\":[{\"sequenceNumber\":100,\"message\":{\"message\":{\"header\":{\"timestamp\":1780000000}}}}]}
{\"messages\":[{\"sequenceNumber\":101,\"message\":{\"message\":{\"header\":{\"timestamp\":1780000001}}}}]}
",
    )
    .expect("write two raw feed rows");
    fs::write(
        dir.join(FRESHNESS),
        "\
{\"seq\":100,\"sequencer_timestamp\":1780000000,\"capture_recv_ns\":1780000001234567890,\"event_age_ms\":250}
{\"seq\":101,\"sequencer_timestamp\":1780000001,\"capture_recv_ns\":1780000001234567999,\"event_age_ms\":1500}
",
    )
    .expect("write mixed freshness");
    fs::write(
        dir.join(FRESHNESS_REPORT),
        r#"{"candidate_count":2,"joined_candidate_count":2,"missing_join_count":0,"p95_event_age_ms":1500,"p99_event_age_ms":1500,"max_event_age_ms":1500,"stale_threshold_ms":1000,"stale_candidate_count":1,"accepted_stale_candidate_count":0,"local_laptop_over_1s_allowed_only_if_rejected":true}"#,
    )
    .expect("write freshness report");
    fs::write(
        dir.join(CANDIDATES),
        "\
{\"route_id\":362,\"block\":465686729,\"feed_seq\":100,\"recv_ns\":1780000001234567890,\"revm_enabled\":true,\"revm_result\":\"success\",\"reject_reason\":\"none\",\"gross_profit_wei\":\"2000000000000000\",\"gas_used_units\":500000,\"measured_execution_gas_used\":500000,\"l2_gas_price_wei\":\"20000000\",\"l1_fee_wei\":\"10000000000000\",\"estimated_gas_wei\":\"1000000000000000\",\"total_gas_wei\":\"1000000000000000\",\"net_profit_wei\":\"1000000000000000\",\"raw_calldata_bytes\":388,\"submit_calldata_bytes\":388,\"submit_gas_limit\":1300000,\"submit_min_profit_wei\":\"1200000000000000\",\"tokens\":[\"0x1\",\"0x2\"],\"pools\":[\"0x3\",\"0x4\"]}
{\"route_id\":363,\"feed_seq\":101,\"recv_ns\":1780000001234567999,\"revm_enabled\":true,\"revm_result\":\"loss\",\"reject_reason\":\"loss shortfall_wei=1\",\"gross_profit_wei\":\"2000000000000000\",\"gas_used_units\":500000,\"measured_execution_gas_used\":500000,\"l2_gas_price_wei\":\"20000000\",\"l1_fee_wei\":\"10000000000000\",\"estimated_gas_wei\":\"1000000000000000\",\"total_gas_wei\":\"1000000000000000\",\"net_profit_wei\":\"1000000000000000\",\"raw_calldata_bytes\":388,\"submit_calldata_bytes\":388,\"submit_gas_limit\":1300000,\"submit_min_profit_wei\":\"1200000000000000\",\"tokens\":[\"0x1\",\"0x2\"],\"pools\":[\"0x3\",\"0x4\"]}
",
    )
    .expect("write mixed candidates");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "candidate row 2 missing reject_reason")]
fn contract_rejects_stale_candidate_missing_reject_reason() {
    let dir = temp_artifact_dir("stale_missing_reject");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(RAW_FEED),
        "\
{\"messages\":[{\"sequenceNumber\":100,\"message\":{\"message\":{\"header\":{\"timestamp\":1780000000}}}}]}
{\"messages\":[{\"sequenceNumber\":101,\"message\":{\"message\":{\"header\":{\"timestamp\":1780000001}}}}]}
",
    )
    .expect("write two raw feed rows");
    fs::write(
        dir.join(FRESHNESS),
        "\
{\"seq\":100,\"sequencer_timestamp\":1780000000,\"capture_recv_ns\":1780000001234567890,\"event_age_ms\":250}
{\"seq\":101,\"sequencer_timestamp\":1780000001,\"capture_recv_ns\":1780000001234567999,\"event_age_ms\":1500}
",
    )
    .expect("write mixed freshness");
    fs::write(
        dir.join(FRESHNESS_REPORT),
        r#"{"candidate_count":2,"joined_candidate_count":2,"missing_join_count":0,"p95_event_age_ms":1500,"p99_event_age_ms":1500,"max_event_age_ms":1500,"stale_threshold_ms":1000,"stale_candidate_count":1,"accepted_stale_candidate_count":0,"local_laptop_over_1s_allowed_only_if_rejected":true}"#,
    )
    .expect("write freshness report");
    fs::write(
        dir.join(CANDIDATES),
        "\
{\"route_id\":362,\"block\":465686729,\"feed_seq\":100,\"recv_ns\":1780000001234567890,\"revm_enabled\":true,\"revm_result\":\"success\",\"reject_reason\":\"none\",\"gross_profit_wei\":\"2000000000000000\",\"gas_used_units\":500000,\"measured_execution_gas_used\":500000,\"l2_gas_price_wei\":\"20000000\",\"l1_fee_wei\":\"10000000000000\",\"estimated_gas_wei\":\"1000000000000000\",\"total_gas_wei\":\"1000000000000000\",\"net_profit_wei\":\"1000000000000000\",\"raw_calldata_bytes\":388,\"submit_calldata_bytes\":388,\"submit_gas_limit\":1300000,\"submit_min_profit_wei\":\"1200000000000000\",\"tokens\":[\"0x1\",\"0x2\"],\"pools\":[\"0x3\",\"0x4\"]}
{\"route_id\":363,\"feed_seq\":101,\"recv_ns\":1780000001234567999,\"revm_enabled\":true,\"revm_result\":\"loss\",\"gross_profit_wei\":\"2000000000000000\",\"gas_used_units\":500000,\"measured_execution_gas_used\":500000,\"l2_gas_price_wei\":\"20000000\",\"l1_fee_wei\":\"10000000000000\",\"estimated_gas_wei\":\"1000000000000000\",\"total_gas_wei\":\"1000000000000000\",\"net_profit_wei\":\"1000000000000000\",\"raw_calldata_bytes\":388,\"submit_calldata_bytes\":388,\"submit_gas_limit\":1300000,\"submit_min_profit_wei\":\"1200000000000000\",\"tokens\":[\"0x1\",\"0x2\"],\"pools\":[\"0x3\",\"0x4\"]}
",
    )
    .expect("write missing-reject candidate");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "is not net-positive after gas")]
fn contract_rejects_net_negative_accepted_success() {
    let dir = temp_artifact_dir("net_negative");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(CANDIDATES),
        r#"{"route_id":362,"feed_seq":100,"recv_ns":1780000001234567890,"revm_enabled":true,"revm_result":"success","reject_reason":"none","gross_profit_wei":"2000000000000000","gas_used_units":500000,"measured_execution_gas_used":500000,"l2_gas_price_wei":"20000000","l1_fee_wei":"10000000000000","estimated_gas_wei":"3000000000000000","total_gas_wei":"3000000000000000","net_profit_wei":"-1000000000000000","raw_calldata_bytes":388,"submit_calldata_bytes":388,"submit_gas_limit":1300000,"submit_min_profit_wei":"1200000000000000","tokens":["0x1","0x2"],"pools":["0x3","0x4"]}"#,
    )
    .expect("write net-negative candidate");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "missing decimal submit_min_profit_wei")]
fn contract_rejects_candidate_missing_submit_min_profit() {
    let dir = temp_artifact_dir("missing_submit_min_profit");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(CANDIDATES),
        r#"{"route_id":362,"feed_seq":100,"recv_ns":1780000001234567890,"revm_enabled":true,"revm_result":"success","reject_reason":"none","gross_profit_wei":"2000000000000000","gas_used_units":500000,"measured_execution_gas_used":500000,"l2_gas_price_wei":"20000000","l1_fee_wei":"10000000000000","estimated_gas_wei":"1000000000000000","total_gas_wei":"1000000000000000","net_profit_wei":"1000000000000000","raw_calldata_bytes":388,"submit_calldata_bytes":388,"submit_gas_limit":1300000,"tokens":["0x1","0x2"],"pools":["0x3","0x4"]}"#,
    )
    .expect("write candidate missing submit_min_profit_wei");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "gas sample 1 calldata_bytes must match candidate")]
fn contract_rejects_gas_matrix_without_candidate_submit_size() {
    let dir = temp_artifact_dir("missing_candidate_submit_size");
    write_complete_fixture(&dir);
    let mut gas = read_json(&dir.join(GAS_MATRIX), "gas matrix");
    gas["samples"][0]["calldata_bytes"] = Value::from(777u64);
    fs::write(dir.join(GAS_MATRIX), gas.to_string())
        .expect("write gas matrix without candidate submit size");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "direct NodeInterface L1 fee must match candidate")]
fn contract_rejects_gas_matrix_l1_fee_mismatch_for_candidate_submit_size() {
    let dir = temp_artifact_dir("gas_l1_fee_mismatch");
    write_complete_fixture(&dir);
    let mut gas = read_json(&dir.join(GAS_MATRIX), "gas matrix");
    gas["samples"][0]["base_fee_wei"] = Value::from("19999999");
    gas["samples"][0]["l1_fee_wei"] = Value::from("9999999500000");
    fs::write(dir.join(GAS_MATRIX), gas.to_string())
        .expect("write gas matrix with mismatched l1 fee");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "candidate_l2_gas_price_wei must match candidate JSONL")]
fn contract_rejects_gas_matrix_l2_gas_price_mismatch_for_candidate_submit_size() {
    let dir = temp_artifact_dir("gas_l2_price_mismatch");
    write_complete_fixture(&dir);
    let mut gas = read_json(&dir.join(GAS_MATRIX), "gas matrix");
    gas["samples"][0]["candidate_l2_gas_price_wei"] = Value::from("99999999");
    fs::write(dir.join(GAS_MATRIX), gas.to_string())
        .expect("write gas matrix with mismatched l2 gas price");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "enabled/promoted venue parity must have zero unsafe over-quotes")]
fn contract_rejects_live_parity_unsafe_overquotes() {
    let dir = temp_artifact_dir("unsafe_live_parity");
    write_complete_fixture(&dir);
    let mut parity = read_json(&dir.join(LIVE_PARITY), "live parity");
    parity["unsafe_overquotes"] = Value::from(1u64);
    fs::write(dir.join(LIVE_PARITY), parity.to_string()).expect("write unsafe live parity");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(expected = "accepted candidate row 1 missing executor_calldata")]
fn contract_rejects_accepted_candidate_missing_executor_calldata() {
    let dir = temp_artifact_dir("missing_executor_calldata");
    write_complete_fixture(&dir);
    let mut rows = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    rows[0]
        .as_object_mut()
        .expect("candidate row object")
        .remove("executor_calldata");
    fs::write(dir.join(CANDIDATES), rows[0].to_string()).expect("write candidate without calldata");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(
    expected = "accepted candidate row 1 exchange_metadata length must match pools length"
)]
fn contract_rejects_accepted_candidate_with_truncated_exchange_metadata() {
    let dir = temp_artifact_dir("truncated_exchange_metadata");
    write_complete_fixture(&dir);
    let mut rows = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    rows[0]["exchange_metadata"]
        .as_array_mut()
        .expect("exchange_metadata array")
        .pop();
    fs::write(dir.join(CANDIDATES), rows[0].to_string())
        .expect("write candidate with truncated exchange_metadata");
    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(
    expected = "accepted candidate row 2 route_id 363 missing matching paper JSONL route_id"
)]
fn contract_rejects_accepted_candidate_without_paper_route() {
    let dir = temp_artifact_dir("missing_paper_route");
    write_complete_fixture(&dir);

    fs::write(
        dir.join(RAW_FEED),
        "\
{\"messages\":[{\"sequenceNumber\":100,\"message\":{\"message\":{\"header\":{\"timestamp\":1780000000}}}}]}
{\"messages\":[{\"sequenceNumber\":101,\"message\":{\"message\":{\"header\":{\"timestamp\":1780000001}}}}]}
",
    )
    .expect("write two raw feed rows");
    fs::write(
        dir.join(FRESHNESS),
        "\
{\"seq\":100,\"sequencer_timestamp\":1780000000,\"capture_recv_ns\":1780000001234567890,\"event_age_ms\":250}
{\"seq\":101,\"sequencer_timestamp\":1780000001,\"capture_recv_ns\":1780000001234567999,\"event_age_ms\":260}
",
    )
    .expect("write two freshness rows");
    fs::write(
        dir.join(FRESHNESS_REPORT),
        r#"{"candidate_count":2,"joined_candidate_count":2,"missing_join_count":0,"p95_event_age_ms":260,"p99_event_age_ms":260,"max_event_age_ms":260,"stale_threshold_ms":1000,"stale_candidate_count":0,"accepted_stale_candidate_count":0,"local_laptop_over_1s_allowed_only_if_rejected":true}"#,
    )
    .expect("write two-candidate freshness report");

    let mut rows = read_jsonl(&dir.join(CANDIDATES), "candidate JSONL");
    let mut second = rows[0].clone();
    second["candidate_id"] = Value::String("seq:101:route:363".to_string());
    second["route_id"] = Value::from(363u64);
    second["feed_seq"] = Value::from(101u64);
    second["recv_ns"] = Value::from(1_780_000_001_234_567_999u64);
    second["trigger_seq"] = Value::from(101u64);
    second["trigger_recv_ns"] = Value::from(1_780_000_001_234_567_999u64);
    rows.push(second);
    fs::write(
        dir.join(CANDIDATES),
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .expect("write candidates with missing paper route");

    validate_aws_dryrun_artifacts(&dir);
}

#[test]
#[should_panic(
    expected = "accepted candidate row 1 route_id 362 missing matching paper JSONL record at same timestamp/block/profit"
)]
fn contract_rejects_paper_route_with_mismatched_timestamp() {
    let dir = temp_artifact_dir("paper_mismatched_timestamp");
    write_complete_fixture(&dir);
    fs::write(
        dir.join(PAPER),
        r#"{"timestamp_ns":1780000009999999999,"route_id":362,"block_number_seen":465686729,"amount_in":"1000000000000000000","gross_profit_wei":"2000000000000000","net_profit_wei":"1000000000000000","pools":["0x3","0x4"]}"#,
    )
    .expect("write paper route with mismatched timestamp");
    validate_aws_dryrun_artifacts(&dir);
}
