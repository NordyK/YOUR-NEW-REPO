use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy_primitives::keccak256;
use serde_json::Value;

const GO_NO_GO: &str = "../../../Codex/LIVE_CAPITAL_GO_NO_GO_2026-06-19.json";
const AWS_ENV: &str = "../../deploy/aws/engine.env.maxlogging.example";
const AWS_PRODUCTION_ENV: &str = "../../deploy/aws/engine.env.production.example";
const SYSTEMD_UNIT: &str = "../../deploy/mantis-engine.service";
const RUN_ALL_HARNESSES: &str = "../../deploy/aws/run-all-harnesses.sh";
const ROOT_GITIGNORE: &str = "../../../.gitignore";
const AWS_BUNDLE_ASSEMBLER: &str = "../../scripts/assemble_aws_dryrun_artifact_dir.sh";
const FRESHNESS_REPORT_BUILDER: &str = "../../scripts/build_feed_candidate_freshness_report.sh";
const AWS_LIVE_ARTIFACT_PROBE: &str = "../../scripts/aws_live_artifact_probe.sh";
const PROVISION: &str = "../../deploy/aws/provision.sh";
const SYNC_TO_HOST: &str = "../../deploy/aws/sync-to-host.sh";
const PHASE2_CONFIG_ENFORCER: &str = "../../deploy/aws/enforce-phase2-baseline.sh";
const SOURCE_PROVENANCE_STAGER: &str = "../../deploy/aws/stage-source-provenance.sh";
const AWS_BUILD: &str = "../../deploy/aws/build.sh";
const ENGINE_BUILD_RS: &str = "build.rs";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn read_json(path: &str) -> Value {
    serde_json::from_str(&read(path))
        .unwrap_or_else(|err| panic!("{path} is not valid JSON: {err}"))
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

fn keccak_file(path: &Path) -> String {
    format!(
        "{:#x}",
        keccak256(fs::read(path).unwrap_or_else(|err| {
            panic!("failed to read Keccak input {}: {err}", path.display())
        }))
    )
}

fn array_field<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field {key} in {value}"))
}

fn all_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(all_text).collect::<Vec<_>>().join("\n"),
        Value::Object(map) => map.values().map(all_text).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
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

fn active_keys(contents: &str) -> BTreeSet<String> {
    contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            trimmed
                .split_once('=')
                .map(|(key, _)| key.trim().to_string())
        })
        .collect()
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("mantis_{label}_{}_{}", std::process::id(), nanos))
}

fn run_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap_or_else(|err| panic!("run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout utf8")
        .trim()
        .to_string()
}

#[test]
fn phase2_config_enforcer_repairs_existing_stage_without_printing_endpoints() {
    let root = unique_temp_dir("phase2_config_enforcer");
    fs::create_dir_all(&root).expect("create phase2 config temp dir");
    let env_file = root.join(".env");
    let config_file = root.join("engine.json");
    let endpoint_marker = "https://secret.example/v2/never-print-this";

    fs::write(
        &env_file,
        format!(
            "ARBITRUM_RPC_URL={endpoint_marker}\n\
DRY_RUN=false\n\
DRY_RUN=false\n\
SUBMIT_TX=true\n\
MIRROR_COLD_VERIFY=1\n\
MIRROR_REORG_UNWIND=1\n\
MIRROR_JOURNAL_PERSIST=1\n\
STRATEGY_EXECUTOR_MAP=research/strategy_executor/index_pool_map.csv\n\
PRESERVE_ME=yes\n"
        ),
    )
    .expect("write unsafe staged env fixture");
    fs::write(
        &config_file,
        r#"{"live_submit":true,"executor":"0x000000000000000000000000000000000000dEaD","lanes":[{"eoa_id":0,"eoa":"0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A","lane_class":"high_ev_two_hop","max_in_flight":1,"key_hex":"0x1111111111111111111111111111111111111111111111111111111111111111","measurement_only":true,"funding_policy":"aws_dryrun_ephemeral_unfunded"}],"preserve":"yes"}"#,
    )
    .expect("write unsafe staged config fixture");
    fs::write(
        root.join("placement_proof.json"),
        r#"{"schema":"aws_dryrun_placement_proof_v2","verdict":"PLACEMENT_READY","submit_pin_ip":"198.51.100.20","feed_pin_ip":"203.0.113.20","counterpart_evidence":{"submit_colocated":true,"feed_colocated":true}}"#,
    )
    .expect("write placement proof fixture");
    fs::set_permissions(&env_file, fs::Permissions::from_mode(0o664)).expect("set unsafe env mode");
    fs::set_permissions(&config_file, fs::Permissions::from_mode(0o644))
        .expect("set unsafe config mode");

    for pass in 0..2 {
        let output = Command::new("bash")
            .arg(PHASE2_CONFIG_ENFORCER)
            .arg(&env_file)
            .arg(&config_file)
            .output()
            .expect("run Phase-2 config enforcer");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "Phase-2 config enforcer pass {pass} failed: {combined}"
        );
        assert!(
            !combined.contains(endpoint_marker) && !combined.contains("secret.example"),
            "Phase-2 config enforcer leaked the staged RPC endpoint: {combined}"
        );
    }

    let env = fs::read_to_string(&env_file).expect("read repaired env");
    for (key, expected) in [
        ("DRY_RUN", "true"),
        ("SUBMIT_TX", "false"),
        ("MIRROR_COLD_VERIFY", "0"),
        ("MIRROR_REORG_UNWIND", "0"),
        ("MIRROR_JOURNAL_PERSIST", "0"),
        (
            "STRATEGY_EXECUTOR_MAP",
            "/opt/mantis/src/engine/research/strategy_executor/index_pool_map.csv",
        ),
        ("SUBMIT_PIN_IP", "198.51.100.20"),
        ("FEED_PIN_IP", "203.0.113.20"),
    ] {
        assert_eq!(
            active_setting(&env, key).as_deref(),
            Some(expected),
            "Phase-2 baseline setting {key} was not repaired"
        );
        assert_eq!(
            env.lines()
                .filter(|line| line.trim_start().starts_with(&format!("{key}=")))
                .count(),
            1,
            "Phase-2 baseline setting {key} must be unique"
        );
    }
    assert!(env.contains("PRESERVE_ME=yes"));
    assert!(
        env.contains(endpoint_marker),
        "enforcer must preserve the RPC value"
    );

    for template_path in [AWS_ENV, AWS_PRODUCTION_ENV] {
        let template = read(template_path);
        for (key, expected) in [
            ("DRY_RUN", "true"),
            ("SUBMIT_TX", "false"),
            ("MIRROR_COLD_VERIFY", "0"),
            ("MIRROR_REORG_UNWIND", "0"),
            ("MIRROR_JOURNAL_PERSIST", "0"),
            (
                "STRATEGY_EXECUTOR_MAP",
                "/opt/mantis/src/engine/research/strategy_executor/index_pool_map.csv",
            ),
        ] {
            assert_eq!(
                active_setting(&template, key).as_deref(),
                Some(expected),
                "{template_path} must carry the Phase-2 baseline setting {key}"
            );
        }
    }

    let config: Value = serde_json::from_str(
        &fs::read_to_string(&config_file).expect("read repaired engine config"),
    )
    .expect("repaired engine config parses");
    assert_eq!(config["live_submit"], false);
    assert_eq!(config["lanes"].as_array().map(Vec::len), Some(1));
    assert_eq!(config["lanes"][0]["measurement_only"], true);
    assert_eq!(
        config["lanes"][0]["funding_policy"],
        "aws_dryrun_ephemeral_unfunded"
    );
    assert_eq!(config["preserve"], "yes");
    assert_eq!(
        fs::metadata(&env_file)
            .expect("env metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&config_file)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let provision = read(PROVISION);
    assert!(
        provision.contains("enforce-phase2-baseline.sh")
            && provision.contains("\"$ENV_OUT\" \"$CFG_OUT\""),
        "provision.sh must always pass both staged files through the Phase-2 enforcer"
    );
    assert!(
        !provision.contains("RPC host:")
            && !provision.contains("grep '^ARBITRUM_RPC_URL=' \"$ENV_OUT\""),
        "provision.sh must never print an RPC endpoint or host"
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn gitless_sync_stages_exact_head_and_verifiable_source_diff_manifest() {
    let root = unique_temp_dir("source_provenance");
    let engine = root.join("engine");
    fs::create_dir_all(&engine).expect("create synthetic engine repo");
    run_git(&root, &["init", "-q"]);
    fs::write(engine.join("source.txt"), "version one\n").expect("write source fixture");
    fs::write(engine.join(".engine_commit"), "stale\n").expect("write stale stamp");
    run_git(
        &root,
        &["add", "engine/source.txt", "engine/.engine_commit"],
    );
    run_git(
        &root,
        &[
            "-c",
            "user.name=Mantis Test",
            "-c",
            "user.email=mantis-test@example.invalid",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
    let head = run_git(&root, &["rev-parse", "HEAD"]);

    let staged = Command::new("bash")
        .arg(SOURCE_PROVENANCE_STAGER)
        .arg(&engine)
        .output()
        .expect("stage source provenance");
    assert!(
        staged.status.success(),
        "source provenance staging failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&staged.stdout),
        String::from_utf8_lossy(&staged.stderr)
    );
    assert_eq!(
        fs::read_to_string(engine.join(".engine_commit"))
            .expect("read staged commit")
            .trim(),
        head,
        "gitless build stamp must equal the exact source HEAD"
    );
    let first: Value = serde_json::from_str(
        &fs::read_to_string(engine.join(".engine_source_manifest.json"))
            .expect("read source manifest"),
    )
    .expect("source manifest parses");
    assert_eq!(first["schema"], "mantis_gitless_source_v2");
    assert_eq!(first["head_commit"], head);
    assert_eq!(first["tracked_dirty"], false);
    for field in ["head_tree", "source_manifest_sha256", "head_diff_sha256"] {
        assert!(
            first[field].as_str().is_some_and(|digest| {
                matches!(digest.len(), 40 | 64) && digest.chars().all(|ch| ch.is_ascii_hexdigit())
            }),
            "manifest field {field} must be a sha256/git-tree digest"
        );
    }

    let verified = Command::new("bash")
        .arg(SOURCE_PROVENANCE_STAGER)
        .arg("--verify")
        .arg(&engine)
        .output()
        .expect("verify source provenance");
    assert!(
        verified.status.success(),
        "fresh source manifest must verify"
    );

    fs::create_dir_all(engine.join("tools/__pycache__")).expect("create generated pycache");
    fs::write(
        engine.join("tools/__pycache__/probe.cpython-313.pyc"),
        b"generated",
    )
    .expect("write generated pyc");
    fs::create_dir_all(engine.join("deploy/aws/.provisioned"))
        .expect("create generated provision bundle");
    fs::write(
        engine.join("deploy/aws/.provisioned/engine.json"),
        "operator-only generated config",
    )
    .expect("write generated provision config");
    fs::create_dir_all(engine.join("logs")).expect("create generated run logs");
    fs::write(
        engine.join("logs/candidates.jsonl"),
        "generated run output\n",
    )
    .expect("write generated run output");
    assert!(
        Command::new("bash")
            .arg(SOURCE_PROVENANCE_STAGER)
            .arg("--verify")
            .arg(&engine)
            .status()
            .expect("verify with generated pycache")
            .success(),
        "generated __pycache__/*.pyc must not perturb immutable source provenance"
    );

    // The AWS build tree intentionally excludes .git.  Content provenance therefore has to
    // remain independently verifiable after transfer, not merely while the local worktree exists.
    let gitless = root.join("gitless-engine");
    fs::create_dir_all(&gitless).expect("create gitless engine copy");
    let copied = Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", engine.display()))
        .arg(&gitless)
        .output()
        .expect("copy staged source without git metadata");
    assert!(copied.status.success(), "copy staged source fixture failed");
    let gitless_verified = Command::new("bash")
        .arg(SOURCE_PROVENANCE_STAGER)
        .arg("--verify-gitless")
        .arg(&gitless)
        .output()
        .expect("verify gitless source provenance");
    assert!(
        gitless_verified.status.success(),
        "gitless source verification failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&gitless_verified.stdout),
        String::from_utf8_lossy(&gitless_verified.stderr)
    );
    fs::write(gitless.join("source.txt"), "mutated after transfer\n")
        .expect("mutate transferred source fixture");
    assert!(
        !Command::new("bash")
            .arg(SOURCE_PROVENANCE_STAGER)
            .arg("--verify-gitless")
            .arg(&gitless)
            .status()
            .expect("reject mutated gitless source")
            .success(),
        "post-transfer source mutation must invalidate gitless provenance"
    );

    fs::write(engine.join("source.txt"), "version two\n").expect("dirty source fixture");
    let stale = Command::new("bash")
        .arg(SOURCE_PROVENANCE_STAGER)
        .arg("--verify")
        .arg(&engine)
        .output()
        .expect("verify stale source provenance");
    assert!(
        !stale.status.success(),
        "source mutation after staging must invalidate provenance"
    );

    let restaged = Command::new("bash")
        .arg(SOURCE_PROVENANCE_STAGER)
        .arg(&engine)
        .output()
        .expect("restage dirty source provenance");
    assert!(
        restaged.status.success(),
        "dirty source provenance restage failed"
    );
    let second: Value = serde_json::from_str(
        &fs::read_to_string(engine.join(".engine_source_manifest.json"))
            .expect("read restaged manifest"),
    )
    .expect("restaged source manifest parses");
    assert_eq!(second["head_commit"], head);
    assert_eq!(second["tracked_dirty"], true);
    assert_ne!(
        first["source_manifest_sha256"], second["source_manifest_sha256"],
        "source digest must change with copied source bytes"
    );
    assert_ne!(
        first["head_diff_sha256"], second["head_diff_sha256"],
        "diff digest must change with tracked source drift"
    );

    // PGO corpus bytes are a second immutable input.  The manifest must be non-vacuous and must
    // reject drift independently of the source-tree manifest.
    let corpus = root.join("corpus");
    fs::create_dir_all(&corpus).expect("create corpus fixture");
    fs::write(corpus.join("one.jsonl"), "{\"seq\":1}\n").expect("write corpus fixture one");
    fs::write(corpus.join("two.jsonl"), "{\"seq\":2}\n").expect("write corpus fixture two");
    let corpus_staged = Command::new("bash")
        .arg(SOURCE_PROVENANCE_STAGER)
        .arg("--stage-corpus")
        .arg(&corpus)
        .output()
        .expect("stage corpus provenance");
    assert!(
        corpus_staged.status.success(),
        "corpus provenance staging failed: {}",
        String::from_utf8_lossy(&corpus_staged.stderr)
    );
    let corpus_manifest = read_json(
        corpus
            .join(".mantis_corpus_manifest.json")
            .to_str()
            .unwrap(),
    );
    assert_eq!(corpus_manifest["schema"], "mantis_pgo_corpus_v1");
    assert_eq!(corpus_manifest["file_count"], 2);
    assert!(corpus_manifest["total_bytes"]
        .as_u64()
        .is_some_and(|n| n > 0));
    assert!(
        Command::new("bash")
            .arg(SOURCE_PROVENANCE_STAGER)
            .arg("--verify-corpus")
            .arg(&corpus)
            .status()
            .expect("verify corpus provenance")
            .success(),
        "fresh corpus manifest must verify"
    );
    fs::write(corpus.join("one.jsonl"), "{\"seq\":999}\n").expect("mutate corpus fixture");
    assert!(
        !Command::new("bash")
            .arg(SOURCE_PROVENANCE_STAGER)
            .arg("--verify-corpus")
            .arg(&corpus)
            .status()
            .expect("reject mutated corpus")
            .success(),
        "corpus mutation must invalidate its manifest"
    );

    let sync = read(SYNC_TO_HOST);
    assert!(
        sync.contains("stage-source-provenance.sh")
            && sync.contains("--verify-gitless")
            && sync.contains("--stage-corpus")
            && sync.contains("--verify-corpus")
            && sync.contains("/opt/mantis/src/anvil/corpus")
            && sync.contains("ec2-user")
            && sync.contains("chown -R mantis:mantis")
            && sync.contains("--exclude '__pycache__/'")
            && sync.contains("--exclude '*.pyc'")
            && sync.contains("--exclude '.provisioned/'")
            && sync.contains("--exclude 'logs/'"),
        "sync-to-host.sh must transfer source and corpus through ec2-user, verify remote bytes, then hand ownership to mantis"
    );
    assert!(
        !sync.contains("> .engine_commit") && !sync.contains("mantis@"),
        "sync-to-host.sh must neither write a cwd-relative stamp nor assume direct mantis SSH"
    );

    let build = read(AWS_BUILD);
    assert!(
        build.contains("--verify-gitless")
            && build.contains("ENGINE_SOURCE_MANIFEST_SHA256"),
        "AWS build must verify the transferred source manifest before Cargo and export its exact digest"
    );
    let build_rs = read(ENGINE_BUILD_RS);
    assert!(
        build_rs.contains("cargo:rustc-env=ENGINE_SOURCE_MANIFEST_SHA256={source_manifest_sha256}")
            && build_rs.contains(".engine_source_manifest.json"),
        "engine build.rs must embed ENGINE_SOURCE_MANIFEST_SHA256 from the verified staged manifest"
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn aws_env_example_matches_go_no_go_no_capital_invariants() {
    let matrix = read_json(GO_NO_GO);
    let env = read(AWS_ENV);
    let invariants = array_field(&matrix, "dry_run_required_invariants")
        .iter()
        .map(|value| value.as_str().expect("invariant string"))
        .collect::<BTreeSet<_>>();

    for required in ["DRY_RUN=true", "SUBMIT_TX=false", "REVM_VALIDATE=1"] {
        assert!(
            invariants.contains(required),
            "go/no-go matrix missing invariant {required}"
        );
        let (key, expected) = required.split_once('=').expect("KEY=value invariant");
        assert_eq!(
            active_setting(&env, key).as_deref(),
            Some(expected),
            "AWS env example drifted from go/no-go invariant {required}"
        );
    }

    for (key, expected_suffix) in [
        ("PAPER_MODE_LOG", "logs/paper.jsonl"),
        ("CANDIDATE_RECORD_JSONL", "logs/candidates.jsonl"),
    ] {
        assert!(
            invariants.contains(match key {
                "PAPER_MODE_LOG" => "PAPER_MODE_LOG is set",
                "CANDIDATE_RECORD_JSONL" => "CANDIDATE_RECORD_JSONL is set",
                _ => unreachable!(),
            }),
            "go/no-go matrix missing {key} invariant"
        );
        assert_eq!(
            active_setting(&env, key).as_deref(),
            Some(expected_suffix),
            "AWS env example must persist {key} under logs/"
        );
    }

    assert_eq!(
        active_setting(&env, "LOG_DIR").as_deref(),
        Some("logs"),
        "dry-run artifacts must stay under the app-root logs/ folder"
    );
}

#[test]
fn aws_env_example_does_not_load_capital_secrets_or_enable_broadcast() {
    let matrix = read_json(GO_NO_GO);
    let env = read(AWS_ENV);
    let text = all_text(&matrix);
    for no_go in [
        "SUBMIT_TX=true during dry-run",
        "funded private key loaded during dry-run",
        "REVM_VALIDATE=0 with live_submit and broadcast enabled",
    ] {
        assert!(text.contains(no_go), "missing no-go condition: {no_go}");
    }

    let keys = active_keys(&env);
    for forbidden in [
        "PRIVATE_KEY",
        "CANARY_PRIVATE_KEY",
        "TRADING_PRIVATE_KEY",
        "KEY_HEX",
        "EXECUTOR_PRIVATE_KEY",
    ] {
        assert!(
            !keys.contains(forbidden),
            "AWS dry-run env example must not activate {forbidden}"
        );
    }

    assert_ne!(
        active_setting(&env, "SUBMIT_TX").as_deref(),
        Some("true"),
        "AWS dry-run env example must not broadcast"
    );
    assert_ne!(
        active_setting(&env, "REVM_VALIDATE").as_deref(),
        Some("0"),
        "AWS dry-run env example must keep REVM validation on"
    );
}

#[test]
fn systemd_unit_runs_engine_in_dry_run_mode_with_shared_runtime_socket_path() {
    let unit = read(SYSTEMD_UNIT);

    for required in [
        "WorkingDirectory=/opt/mantis/engine",
        "ExecStart=/opt/mantis/engine/bin/engine dry-run",
        "ExecStartPre=/usr/bin/install -d -o mantis -g mantis /opt/mantis/engine/logs",
        "ExecStartPre=/usr/bin/install -d -o mantis -g mantis /run/mantis",
        "StandardOutput=append:/opt/mantis/engine/logs/engine.log",
        "StandardError=append:/opt/mantis/engine/logs/engine.err",
        "PrivateTmp=true",
        "RuntimeDirectory=mantis",
        "ReadWritePaths=/opt/mantis/engine /run/mantis",
    ] {
        assert!(
            unit.contains(required),
            "systemd unit missing required dry-run deployment line: {required}"
        );
    }

    assert!(
        unit.contains("PrivateTmp isolates /tmp per service")
            && unit.contains("socket paths must use /run/mantis"),
        "systemd unit must document the PrivateTmp/binunix shared socket boundary"
    );
}

#[test]
fn go_no_go_keeps_raw_feed_capture_as_required_aws_artifact_not_local_env_claim() {
    let matrix = read_json(GO_NO_GO);
    let env = read(AWS_ENV);
    let text = all_text(&matrix);

    for required in [
        "raw sequencer capture is enabled",
        "freshness sidecar JSONL is enabled",
        "raw_feed_and_freshness",
        "candidate_and_paper_records",
        "Join feed freshness to candidates and compute p95, p99, and max event_age_ms",
    ] {
        assert!(
            text.contains(required),
            "go/no-go matrix missing AWS capture requirement: {required}"
        );
    }

    assert!(
        active_setting(&env, "RAW_FEED_CAPTURE_JSONL").is_none(),
        "raw capture is owned by the AWS relay/launch command, not this engine-only env example"
    );
}

#[test]
fn current_launch_scripts_do_not_silently_claim_complete_aws_bundle_assembly() {
    let matrix = read_json(GO_NO_GO);
    let text = all_text(&matrix);
    let gate8 = read("../../scripts/gate8_binary_live.sh");
    let dryrun_3h = read("../../scripts/dryrun_3h.sh");
    let assembler = read(AWS_BUNDLE_ASSEMBLER);
    let live_probe = read(AWS_LIVE_ARTIFACT_PROBE);

    assert!(
        gate8.contains("exec bash scripts/dryrun_3h.sh")
            && gate8.contains("MIN_RAW_FEED_ROWS")
            && gate8.contains("MIN_FRESHNESS_ROWS")
            && gate8.contains("MIN_CANDIDATE_ROWS")
            && gate8.contains("MIN_PAPER_ROWS")
            && !gate8.contains("pkill"),
        "Gate8 must delegate to the shared fail-closed capture/liveness/evidence harness"
    );

    assert!(
        dryrun_3h.contains("PAPER_MODE_LOG=$PAPER")
            && dryrun_3h.contains("CANDIDATE_RECORD_JSONL=$CANDREC")
            && dryrun_3h.contains("run_identity.json")
            && dryrun_3h.contains("aws_evidence_fine")
            && !dryrun_3h.contains("pkill"),
        "3h dry-run harness should own paper plus candidate records"
    );
    assert!(
        dryrun_3h.contains("--capture-jsonl \"$RAW_FEED\""),
        "3h dry-run harness must capture raw feed from the same no-capital run boundary"
    );
    assert!(
        dryrun_3h.contains("RAW_FEED=\"$RUNDIR/raw_feed.jsonl\""),
        "3h dry-run harness must use the artifact-contract raw_feed.jsonl name"
    );
    assert!(
        dryrun_3h.contains("FRESHNESS=\"$RAW_FEED.freshness.jsonl\""),
        "3h dry-run harness must bind the sidecar freshness artifact to its raw feed"
    );

    for required in [
        "the no-capital AWS dry-run source directory must include raw feed, freshness, candidates, paper, gas, and parity artifacts from the same commit/run boundary",
        "compose or generate env.redacted, binary.sha256, run_metadata.json, artifact_manifest.json, raw_feed.jsonl, raw_feed.jsonl.freshness.jsonl, feed_candidate_freshness_report.json, candidates.jsonl, paper.jsonl, nodeinterface_gas_matrix.json, and live_parity.json from the same commit/run boundary",
        "aws_dryrun_artifact_contract",
        "scripts/assemble_aws_dryrun_artifact_dir.sh",
        "scripts/build_feed_candidate_freshness_report.sh",
        "scripts/aws_live_artifact_probe.sh",
    ] {
        assert!(
            text.contains(required),
            "go/no-go matrix must keep the AWS bundle assembly gap explicit: {required}"
        );
    }

    for required in [
        "AWS_DRYRUN_SOURCE_DIR",
        "AWS_DRYRUN_ARTIFACT_DIR",
        "AWS_DRYRUN_ENGINE_BIN",
        "env.redacted",
        "binary.sha256",
        "run_metadata.json",
        "artifact_manifest.json",
        "run_duration_seconds",
        "raw_feed.jsonl",
        "raw_feed.jsonl.freshness.jsonl",
        "feed_candidate_freshness_report.json",
        "candidates.jsonl",
        "paper.jsonl",
        "nodeinterface_gas_matrix.json",
        "live_parity.json",
        "run_identity.json",
        "deployment_manifest.json",
        "placement_proof.json",
        "fastlane_parity_state_pin.json",
        "parity_source_report.json",
        "fastlane_parity_corpus.jsonl",
        "fastlane_parity_report.json",
        "fastlane_parity_manifest.json",
        "build_feed_candidate_freshness_report.sh",
        "sha256sum",
        "aws_dryrun_artifact_contract",
    ] {
        assert!(
            assembler.contains(required),
            "AWS dry-run bundle assembler missing required contract surface: {required}"
        );
    }

    for required in [
        "ARBITRUM_RPC_URL",
        "AWS_DRYRUN_SOURCE_DIR",
        "nodeinterface_gas_matrix.json",
        "live_parity.json",
        "Arbitrum NodeInterface",
        "unsafe_overquotes",
        "calldata_bytes",
        "l1_fee_wei",
        "l2_gas_price_wei",
    ] {
        assert!(
            live_probe.contains(required),
            "AWS live artifact probe missing required output contract: {required}"
        );
    }
    assert!(
        !live_probe.contains("SUBMIT_TX=true"),
        "AWS live artifact probe must not enable transaction broadcast"
    );
}

#[test]
fn root_gitignore_excludes_operator_secrets_and_terraform_workdir() {
    let root_gitignore = read(ROOT_GITIGNORE);

    for required in [
        "*accessKeys*.csv",
        "*AccessKeys*.csv",
        "engine/deploy/aws/terraform/.terraform/",
    ] {
        assert!(
            root_gitignore.contains(required),
            "root .gitignore must exclude operator-local file pattern: {required}"
        );
    }
}

#[test]
fn run_all_harnesses_default_stays_laptop_static_only() {
    let script = read(RUN_ALL_HARNESSES);
    let forge_begin = script
        .find("# BEGIN PHASE1_FORGE_SCOPE")
        .expect("run-all harness must mark the opt-in Phase-1 Forge scope");
    let forge_end = script
        .find("# END PHASE1_FORGE_SCOPE")
        .expect("run-all harness must close the opt-in Phase-1 Forge scope")
        + "# END PHASE1_FORGE_SCOPE".len();
    assert!(
        forge_begin < forge_end
            && script.contains("case \"${HARNESS_SCOPE:-default}\"")
            && script.contains("HARNESS_SCOPE=forge")
            && script.contains("run_phase1_forge_scope"),
        "the networked fork harness must remain behind an explicit HARNESS_SCOPE=forge branch"
    );

    // Preserve the original laptop/static boundary on the default surface. The marked, explicit
    // Forge scope has its own closed-manifest/safety contract in forge_harness_driver_static.rs;
    // removing only that block here prevents the opt-in local fork gate from weakening the default
    // path's longstanding ban on RPC, Forge, ignored, and live/AWS-only commands.
    let default_surface = format!("{}{}", &script[..forge_begin], &script[forge_end..]);

    for required in [
        "run local/static engine correctness harnesses",
        "What it runs locally:",
        "cargo test --release --workspace",
        "zero_rpc_family_readiness_static",
        "v4_fastlane_readiness_static",
        "fastlane_promotion_readiness_static",
        "laptop_only_certification_boundary_static",
        "aws_dryrun_artifact_contract",
        "Live parity, live feed freshness, and the real AWS dry-run artifact are separate external",
        "they are not represented here as ignored cargo tests",
    ] {
        assert!(
            default_surface.contains(required),
            "run-all harness must keep laptop/static boundary text or command: {required}"
        );
    }

    for forbidden in [
        "EVERY engine correctness harness",
        concat!("--", "ignored"),
        "ARBITRUM_RPC_URL",
        "AWS_DRYRUN_ARTIFACT_DIR",
        concat!("source ../", ".env"),
        concat!("forge", " test"),
        "cast ",
        concat!("v3_mint_burn", "_live_parity"),
        concat!("curve_stableswap", "_live_parity"),
        concat!("v4_modify_liquidity", "_live_parity"),
        concat!("trader_joe_lb", "_live_parity"),
        concat!("curve_crypto", "_live_parity"),
        concat!("balancer_weighted", "_live_parity"),
        concat!("fluid_v1", "_live_parity"),
        concat!("l1_gas_nodeinterface", "_matrix_live"),
        concat!("live_feed_event_age", "_probe"),
        concat!("gate9_equivalence_report", "_contract"),
        concat!("deploy_readiness", "_static"),
    ] {
        assert!(
            !default_surface.contains(forbidden),
            "run-all default surface must not reintroduce AWS/live-only local gate marker: {forbidden}"
        );
    }
}

#[test]
fn aws_bundle_assembler_normalizes_split_outputs_and_redacts_env() {
    let syntax = Command::new("bash")
        .arg("-n")
        .arg(AWS_BUNDLE_ASSEMBLER)
        .output()
        .expect("run bash -n on assembler");
    assert!(
        syntax.status.success(),
        "assembler shell syntax failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&syntax.stdout),
        String::from_utf8_lossy(&syntax.stderr)
    );

    let root = unique_temp_dir("aws_bundle_assembler");
    let source = root.join("source");
    let artifact = root.join("artifact");
    let env_file = root.join("engine.env");
    let engine_bin = root.join("engine-bin");
    fs::create_dir_all(&source).expect("create source dir");

    fs::write(
        &env_file,
        "\
DRY_RUN=true
SUBMIT_TX=false
REVM_VALIDATE=1
PRIVATE_KEY=0xdeadbeef
PAPER_MODE_LOG=logs/paper.jsonl
CANDIDATE_RECORD_JSONL=logs/candidates.jsonl
",
    )
    .expect("write synthetic env");
    fs::write(&engine_bin, "synthetic release binary").expect("write synthetic binary");
    let env_sha = sha256_file(&env_file);
    let engine_sha = sha256_file(&engine_bin);
    let source_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fs::write(
        source.join("placement_proof.json"),
        r#"{"schema":"aws_dryrun_placement_proof_v2","verdict":"PLACEMENT_READY","objective":"submission_priority","objective_selection":"explicit_cli_only","region":"eu-west-1","az_name":"eu-west-1a","az_id":"euw1-az2","subnet_id":"subnet-0123456789abcdef0","instance_type":"c8gn.2xlarge","submit_pin_ip":"198.51.100.20","feed_pin_ip":"203.0.113.20","source":{"source_manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"submit":{"tested_in_exact_chosen_az":true,"report_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","campaign_config_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","bundle_manifest_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"},"feed":{"front_door_tcp_placement_authority":false,"report_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","campaign_config_sha256":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","bundle_manifest_sha256":"1111111111111111111111111111111111111111111111111111111111111111"},"counterpart_evidence":{"submit_colocated":true,"feed_colocated":true}}"#,
    )
    .expect("write placement proof");
    let placement_sha = sha256_file(&source.join("placement_proof.json"));
    fs::write(
        source.join("deployment_manifest.json"),
        serde_json::json!({
            "schema":"aws_dryrun_deployment_bundle_v1",
            "source_manifest_sha256":source_sha,
            "engine_sha256":engine_sha,
            "placement_proof_sha256":placement_sha
        })
        .to_string(),
    )
    .expect("write deployment manifest");
    let deployment_sha = sha256_file(&source.join("deployment_manifest.json"));
    fs::write(
        source.join("raw_feed.jsonl"),
        r#"{"messages":[{"sequenceNumber":100,"message":{"message":{"header":{"timestamp":1700000000}}}}]}"#,
    )
    .expect("write raw feed");
    fs::write(
        source.join("raw_feed.jsonl.freshness.jsonl"),
        r#"{"seq":100,"sequencer_timestamp":1700000000,"capture_recv_ns":1780000001234567890,"event_age_ms":250}"#,
    )
    .expect("write freshness");
    fs::write(
        source.join("candidate_records.jsonl"),
        r#"{"route_id":362,"feed_seq":100,"recv_ns":1780000001234567890,"revm_result":"success","reject_reason":"none","gross_profit_wei":"2","net_profit_wei":"1","submit_calldata_bytes":388}"#,
    )
    .expect("write candidates");
    let candidate_sha = sha256_file(&source.join("candidate_records.jsonl"));
    let candidate_keccak = keccak_file(&source.join("candidate_records.jsonl"));
    fs::write(source.join("paper_candidates.jsonl"), r#"{"route_id":362}"#).expect("write paper");
    fs::write(source.join("engine.log"), "{\"state\":\"pin\"}\n").expect("write state pin");
    let engine_log_sha = sha256_file(&source.join("engine.log"));
    fs::write(
        source.join("monitoring_summary.json"),
        serde_json::json!({
            "schema":"aws_dryrun_monitoring_summary_v1",
            "run_id":"fixture-run",
            "source_manifest_sha256":source_sha,
            "engine_sha256":engine_sha,
            "live_window_seconds":300,
            "engine_log_artifact":"engine.log",
            "engine_log_sha256":engine_log_sha,
            "heartbeat_rows":1,
            "counter_maxima":{
                "submitted":1,
                "submit_wire_attempts":0,
                "submit_wire_acks":0
            }
        })
        .to_string(),
    )
    .expect("write monitoring summary");
    let host_telemetry_row = serde_json::json!({
        "schema":"mantis_host_telemetry_v1","sample_index":0,"monotonic_ns":1_000_000_000u64,
        "sampler":{"available":true,"source":"sched_getaffinity","pid":99,
            "affinity_cpus":[0],"housekeeping_core":0,"affinity_valid":true},
        "cpu":{"available":true},"scheduler":{"available":true},
        "process":{"available":true,"alive":true},
        "processes":{"available":true,
            "engine":{"available":true,"alive":true},
            "sidecar":{"available":true,"alive":true},
            "relay":{"available":true,"pid":103,"alive":true}},
        "irq":{"available":true},"softirq":{"available":true},
        "ena":{"available":true,"interfaces":{"ens5":{
            "available":true,"driver":"ena","queues":{"rx":2,"tx":2},
            "rss":{"available":true},"coalescing":{"available":true},
            "allowance_counters":{"pps_allowance_exceeded":0},
            "allowance_deltas":{"pps_allowance_exceeded":0}}}},
        "softnet":{"available":true},"sockets":{"available":true},
        "network":{"available":true},"queue":{"available":true},
        "memory":{"available":true},"psi":{"available":true,
            "cpu":{"available":true},"memory":{"available":true},"io":{"available":true}},
        "disk":{"available":true},"clock":{"available":true,
            "chrony":{"available":true,"stratum":1,"system_time_offset_seconds":0.000001,
                "last_offset_seconds":0.000002,"rms_offset_seconds":0.000003,
                "frequency_ppm":1.0,"residual_frequency_ppm":0.1,"skew_ppm":0.2}}
    });
    fs::write(
        source.join("host_telemetry.jsonl"),
        format!("{host_telemetry_row}\n"),
    )
    .expect("write host telemetry");
    let host_telemetry_sha = sha256_file(&source.join("host_telemetry.jsonl"));
    let host_telemetry_summary = serde_json::json!({
        "schema":"mantis_host_telemetry_summary_v1","status":"pass",
        "artifact":"host_telemetry.jsonl","sha256":host_telemetry_sha,
        "sampler_sha256":"9999999999999999999999999999999999999999999999999999999999999999",
        "row_count":1,"monitor_exit_status":0,"configured_interval_seconds":1.0,"relay_pid":103,
        "required_domains":["cpu","scheduler","process","processes","irq","softirq","ena",
            "softnet","sockets","network","queue","memory","psi","disk","clock"],
        "coverage_start_monotonic_ns":1_000_000_000u64,
        "coverage_end_monotonic_ns":1_000_000_000u64,"max_gap_ns":0,
        "max_allowed_gap_ns":3_000_000_000u64
    });
    fs::write(
        source.join("host_telemetry_summary.json"),
        host_telemetry_summary.to_string(),
    )
    .expect("write host telemetry summary");
    fs::write(source.join("sidecar.stdout.log"), "relay sidecar started\n")
        .expect("write sidecar stdout");
    fs::write(source.join("sidecar.stderr.log"), "no sidecar errors\n")
        .expect("write sidecar stderr");
    let state_pin_keccak = keccak_file(&source.join("engine.log"));
    fs::write(
        source.join("parity-transcript.jsonl"),
        "{\"family\":\"UniswapV3\"}\n",
    )
    .expect("write parity transcript");
    let transcript_sha = sha256_file(&source.join("parity-transcript.jsonl"));
    let transcript_keccak = keccak_file(&source.join("parity-transcript.jsonl"));
    let parity_source = serde_json::json!({
        "schema_version":2,"strict_byte_exact":true,"source_manifest_sha256":source_sha,
        "run_id":"fixture-run","corpus":"candidate_records.jsonl",
        "corpus_keccak256":candidate_keccak,
        "state_pin_source":"engine.log",
        "state_pin_source_keccak256":state_pin_keccak,
        "comparison_transcript":"parity-transcript.jsonl",
        "comparison_transcript_sha256":transcript_sha,
        "comparison_transcript_keccak256":transcript_keccak.trim_start_matches("0x"),
        "comparison_transcript_rows":1,
        "required_families":["UniswapV3"],"required_family_count":1,
        "promotion_eligible_families":["UniswapV3"],"promotion_eligible_family_count":1,
        "families":{"UniswapV3":{"state_pinned":true,"comparisons":1,"disagree":0,"unresolved_errors":0,"byte_exact":true}},
        "gate_pass":true
    });
    fs::write(source.join("parity-source.json"), parity_source.to_string())
        .expect("write parity source report");
    let parity_source_sha = sha256_file(&source.join("parity-source.json"));
    let run_identity = serde_json::json!({
        "schema":"aws_dryrun_harness_run_v1","run_id":"fixture-run",
        "profile":"aws_evidence_fine","status":"pass","instrumented":true,
        "source_manifest_sha256":source_sha,"engine_sha256":engine_sha,
        "deployment_manifest_sha256":deployment_sha,"placement_proof_sha256":placement_sha,
        "effective_env_sha256":env_sha,
        "effective_command":"DRY_RUN=true SUBMIT_TX=false REVM_VALIDATE=1 engine dry-run",
        "started_utc":"2026-06-20T00:00:00Z","finished_utc":"2026-06-20T00:05:00Z",
        "live_window_seconds":300,
        "host_telemetry":host_telemetry_summary
    });
    fs::write(source.join("run_identity.json"), run_identity.to_string())
        .expect("write run identity");
    let run_identity_sha = sha256_file(&source.join("run_identity.json"));
    fs::write(
        source.join("nodeinterface_gas_matrix.json"),
        serde_json::json!({
            "schema":"aws_dryrun_nodeinterface_gas_matrix_v2",
            "source":"Arbitrum NodeInterface direct eth_call gasEstimateL1Component",
            "run_id":"fixture-run","source_manifest_sha256":source_sha,
            "run_identity_sha256":run_identity_sha,"candidate_sha256":candidate_sha,
            "node_interface_address":"0x00000000000000000000000000000000000000C8",
            "selector":"0x77d488a2","samples":[{}]
        })
        .to_string(),
    )
    .expect("write gas matrix");
    fs::write(
        source.join("live_parity.json"),
        serde_json::json!({
            "schema":"aws_dryrun_live_parity_v2","run_id":"fixture-run",
            "source_manifest_sha256":source_sha,"strict_byte_exact":true,"gate_pass":true,
            "unsafe_overquotes":0,"required_families":["UniswapV3"],"required_family_count":1,
            "promotion_eligible_families":["UniswapV3"],"promotion_eligible_family_count":1,
            "corpus":"candidate_records.jsonl",
            "corpus_keccak256":candidate_keccak,
            "state_pin_source":"engine.log",
            "state_pin_source_keccak256":state_pin_keccak,
            "source_report":"parity-source.json",
            "source_report_sha256":parity_source_sha
        })
        .to_string(),
    )
    .expect("write live parity");
    let output = Command::new("bash")
        .arg(AWS_BUNDLE_ASSEMBLER)
        .env("AWS_DRYRUN_SOURCE_DIR", &source)
        .env("AWS_DRYRUN_ARTIFACT_DIR", &artifact)
        .env("AWS_DRYRUN_ENV_FILE", &env_file)
        .env("AWS_DRYRUN_ENGINE_BIN", &engine_bin)
        .env("MANTIS_ASSEMBLER_TEST_MODE", "1")
        .output()
        .expect("run AWS bundle assembler");
    assert!(
        output.status.success(),
        "assembler failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for required in [
        "env.redacted",
        "binary.sha256",
        "run_metadata.json",
        "artifact_manifest.json",
        "raw_feed.jsonl",
        "raw_feed.jsonl.freshness.jsonl",
        "feed_candidate_freshness_report.json",
        "candidates.jsonl",
        "paper.jsonl",
        "nodeinterface_gas_matrix.json",
        "live_parity.json",
        "host_telemetry.jsonl",
        "host_telemetry_summary.json",
        "sidecar.stdout.log",
        "sidecar.stderr.log",
    ] {
        let path = artifact.join(required);
        assert!(
            path.is_file(),
            "assembler did not create {}",
            path.display()
        );
        assert!(
            path.metadata().expect("artifact metadata").len() > 0,
            "assembler created empty {}",
            path.display()
        );
    }

    let redacted = fs::read_to_string(artifact.join("env.redacted")).expect("read redacted env");
    assert!(redacted.contains("DRY_RUN=true"));
    assert!(redacted.contains("SUBMIT_TX=false"));
    assert!(redacted.contains("REVM_VALIDATE=1"));
    assert!(
        !redacted.contains("PRIVATE_KEY"),
        "assembler must remove active private-key settings from env.redacted"
    );

    let hash = fs::read_to_string(artifact.join("binary.sha256")).expect("read binary hash");
    assert!(
        hash.trim().len() == 64 && hash.trim().chars().all(|ch| ch.is_ascii_hexdigit()),
        "assembler must write a 64-hex binary.sha256 digest"
    );
    let run_metadata = read_json(artifact.join("run_metadata.json").to_str().unwrap());
    assert_eq!(
        run_metadata["schema"], "aws_dryrun_run_metadata_v2",
        "assembler must write versioned run metadata"
    );
    assert_eq!(
        run_metadata["binary_sha256"],
        hash.trim(),
        "run_metadata binary_sha256 must match binary.sha256"
    );
    assert_eq!(
        run_metadata["run_started_utc"], "2026-06-20T00:00:00Z",
        "run metadata must preserve operator-supplied start timestamp"
    );
    assert_eq!(
        run_metadata["run_finished_utc"], "2026-06-20T00:05:00Z",
        "run metadata must preserve operator-supplied finish timestamp"
    );
    assert_eq!(
        run_metadata["run_duration_seconds"], 300,
        "run metadata must compute the dry-run duration from start/finish timestamps"
    );
    assert!(
        run_metadata["command_line"]
            .as_str()
            .is_some_and(|value| value.contains("DRY_RUN=true")),
        "run metadata must preserve the exact dry-run command line"
    );
    let manifest = read_json(artifact.join("artifact_manifest.json").to_str().unwrap());
    assert_eq!(
        manifest["schema"], "aws_dryrun_artifact_manifest_v2",
        "assembler must write versioned artifact manifest"
    );
    assert_eq!(
        manifest["files"]["candidates.jsonl"],
        sha256_file(&artifact.join("candidates.jsonl")),
        "artifact manifest must hash candidates.jsonl content"
    );
    let live_parity = read_json(artifact.join("live_parity.json").to_str().unwrap());
    assert_eq!(
        live_parity["unsafe_overquotes"], 0,
        "assembler must preserve strict zero-unsafe parity evidence"
    );

    let freshness_report = read_json(
        artifact
            .join("feed_candidate_freshness_report.json")
            .to_str()
            .unwrap(),
    );
    assert_eq!(freshness_report["candidate_count"], 1);
    assert_eq!(freshness_report["joined_candidate_count"], 1);
    assert_eq!(freshness_report["missing_join_count"], 0);
    assert_eq!(freshness_report["p95_event_age_ms"], 250);
    assert_eq!(freshness_report["stale_candidate_count"], 0);
    assert_eq!(freshness_report["accepted_stale_candidate_count"], 0);

    fs::remove_dir_all(root).ok();
}

#[test]
fn aws_bundle_assembler_rejects_executor_address_dependency_in_redacted_env() {
    let root = unique_temp_dir("aws_bundle_assembler_executor_address");
    let source = root.join("source");
    let artifact = root.join("artifact");
    let env_file = root.join("engine.env");
    let engine_bin = root.join("engine-bin");
    fs::create_dir_all(&source).expect("create source dir");

    fs::write(
        &env_file,
        "\
DRY_RUN=true
SUBMIT_TX=false
REVM_VALIDATE=1
EXECUTOR_ADDRESS=0x0000000000000000000000000000000000000001
",
    )
    .expect("write executor-dependent env");
    fs::write(&engine_bin, "synthetic release binary").expect("write synthetic binary");

    let output = Command::new("bash")
        .arg(AWS_BUNDLE_ASSEMBLER)
        .env("AWS_DRYRUN_SOURCE_DIR", &source)
        .env("AWS_DRYRUN_ARTIFACT_DIR", &artifact)
        .env("AWS_DRYRUN_ENV_FILE", &env_file)
        .env("AWS_DRYRUN_ENGINE_BIN", &engine_bin)
        .env(
            "AWS_DRYRUN_COMMAND_LINE",
            "DRY_RUN=true SUBMIT_TX=false REVM_VALIDATE=1 synthetic-engine",
        )
        .env("AWS_DRYRUN_RUN_STARTED_UTC", "2026-06-20T00:00:00Z")
        .env("AWS_DRYRUN_RUN_FINISHED_UTC", "2026-06-20T00:05:00Z")
        .output()
        .expect("run AWS bundle assembler with executor address");
    assert!(
        !output.status.success(),
        "assembler must reject executor-dependent dry-run env"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("executor address"),
        "assembler failed for the wrong reason; stderr={stderr}"
    );

    fs::remove_dir_all(root).ok();
}

#[test]
fn freshness_report_builder_surfaces_missing_joins_and_accepted_stale_candidates() {
    let root = unique_temp_dir("freshness_report_builder_red_flags");
    fs::create_dir_all(&root).expect("create freshness red-flag temp dir");
    let freshness = root.join("raw_feed.jsonl.freshness.jsonl");
    let candidates = root.join("candidates.jsonl");
    let report = root.join("feed_candidate_freshness_report.json");

    fs::write(
        &freshness,
        "\
{\"seq\":20,\"sequencer_timestamp\":1700000020,\"capture_recv_ns\":1780000000000000020,\"event_age_ms\":1500}
",
    )
    .expect("write freshness rows");
    fs::write(
        &candidates,
        "\
{\"route_id\":20,\"feed_seq\":20,\"recv_ns\":1780000000000000020,\"reject_reason\":\"none\"}
{\"route_id\":21,\"feed_seq\":21,\"recv_ns\":1780000000000000021,\"reject_reason\":\"none\"}
",
    )
    .expect("write candidate rows");

    let output = Command::new("bash")
        .arg(FRESHNESS_REPORT_BUILDER)
        .env("FRESHNESS_JSONL", &freshness)
        .env("CANDIDATE_JSONL", &candidates)
        .env("FRESHNESS_REPORT_JSON", &report)
        .env("STALE_THRESHOLD_MS", "1000")
        .output()
        .expect("run freshness report builder red-flag case");
    assert!(
        output.status.success(),
        "freshness report builder failed red-flag case:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let built = read_json(report.to_str().expect("report path utf8"));
    assert_eq!(built["candidate_count"], 2);
    assert_eq!(built["joined_candidate_count"], 1);
    assert_eq!(built["missing_join_count"], 1);
    assert_eq!(built["p95_event_age_ms"], 1500);
    assert_eq!(built["p99_event_age_ms"], 1500);
    assert_eq!(built["max_event_age_ms"], 1500);
    assert_eq!(built["stale_candidate_count"], 1);
    assert_eq!(built["accepted_stale_candidate_count"], 1);
    assert_eq!(built["local_laptop_over_1s_allowed_only_if_rejected"], true);

    fs::remove_dir_all(root).ok();
}

#[test]
fn freshness_report_builder_computes_candidate_join_and_stale_rejection_metrics() {
    let syntax = Command::new("bash")
        .arg("-n")
        .arg(FRESHNESS_REPORT_BUILDER)
        .output()
        .expect("run bash -n on freshness report builder");
    assert!(
        syntax.status.success(),
        "freshness report builder shell syntax failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&syntax.stdout),
        String::from_utf8_lossy(&syntax.stderr)
    );

    let root = unique_temp_dir("freshness_report_builder");
    fs::create_dir_all(&root).expect("create freshness temp dir");
    let freshness = root.join("raw_feed.jsonl.freshness.jsonl");
    let candidates = root.join("candidates.jsonl");
    let report = root.join("feed_candidate_freshness_report.json");

    fs::write(
        &freshness,
        "\
{\"seq\":10,\"sequencer_timestamp\":1700000000,\"capture_recv_ns\":1780000000000000010,\"event_age_ms\":100}
{\"seq\":11,\"sequencer_timestamp\":1700000001,\"capture_recv_ns\":1780000000000000011,\"event_age_ms\":1200}
{\"seq\":12,\"sequencer_timestamp\":1700000002,\"capture_recv_ns\":1780000000000000012,\"event_age_ms\":200}
",
    )
    .expect("write freshness rows");
    fs::write(
        &candidates,
        "\
{\"route_id\":1,\"feed_seq\":10,\"recv_ns\":1780000000000000010,\"reject_reason\":\"none\"}
{\"route_id\":2,\"feed_seq\":11,\"recv_ns\":1780000000000000011,\"reject_reason\":\"stale_feed\"}
{\"route_id\":3,\"feed_seq\":12,\"recv_ns\":1780000000000000012,\"reject_reason\":\"none\"}
",
    )
    .expect("write candidate rows");

    let output = Command::new("bash")
        .arg(FRESHNESS_REPORT_BUILDER)
        .env("FRESHNESS_JSONL", &freshness)
        .env("CANDIDATE_JSONL", &candidates)
        .env("FRESHNESS_REPORT_JSON", &report)
        .env("STALE_THRESHOLD_MS", "1000")
        .output()
        .expect("run freshness report builder");
    assert!(
        output.status.success(),
        "freshness report builder failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let built = read_json(report.to_str().expect("report path utf8"));
    assert_eq!(built["candidate_count"], 3);
    assert_eq!(built["joined_candidate_count"], 3);
    assert_eq!(built["missing_join_count"], 0);
    assert_eq!(built["p95_event_age_ms"], 1200);
    assert_eq!(built["p99_event_age_ms"], 1200);
    assert_eq!(built["max_event_age_ms"], 1200);
    assert_eq!(built["stale_threshold_ms"], 1000);
    assert_eq!(built["stale_candidate_count"], 1);
    assert_eq!(built["accepted_stale_candidate_count"], 0);
    assert_eq!(built["local_laptop_over_1s_allowed_only_if_rejected"], true);

    fs::remove_dir_all(root).ok();
}
