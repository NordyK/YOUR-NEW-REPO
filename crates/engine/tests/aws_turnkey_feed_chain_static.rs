use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .expect("engine/crates/engine must have the repository root as an ancestor")
        .to_path_buf()
}

fn read_required(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("mantis_{label}_{}_{}", std::process::id(), nanos))
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|err| panic!("chmod {}: {err}", path.display()));
}

#[test]
fn relay_launcher_uses_verified_public_feed_contract() {
    let relay = read_required("engine/deploy/run-relay.sh");

    assert!(
        relay.contains("offchainlabs/nitro-node:v3.11.2-3599aca@sha256:ebc985e3b105980734630744981e1542001c22d74cba57509fe0d5ed8bb84c14"),
        "relay must default to the currently verified multi-arch Nitro manifest"
    );
    for required in [
        "--node.feed.input.require-chain-id=false",
        "--node.feed.input.require-feed-version=false",
        "--node.feed.output.addr=127.0.0.1",
        "--node.feed.output.enable-compression=false",
        "--network host",
        "--cpuset-cpus=\"0\"",
        "RELAY_PIN_IP",
    ] {
        assert!(
            relay.contains(required),
            "relay launcher missing {required}"
        );
    }
    assert!(
        !relay.contains("--node.feed.input.require-chain-id=true")
            && !relay.contains("--node.feed.input.require-feed-version=true"),
        "the public feed rejects the strict handshake flags"
    );
}

#[test]
fn systemd_orders_relay_then_sidecar_then_engine() {
    let relay = read_required("engine/deploy/mantis-relay.service");
    let sidecar = read_required("engine/deploy/mantis-relay-sidecar.service");
    let engine = read_required("engine/deploy/mantis-engine.service");

    for required in [
        "After=network-online.target docker.service",
        "Requires=docker.service",
        "Before=mantis-relay-sidecar.service",
        "ConditionFileIsExecutable=/opt/mantis/engine/bin/run-relay.sh",
        "ExecStart=/opt/mantis/engine/bin/run-relay.sh",
        "Restart=always",
    ] {
        assert!(relay.contains(required), "relay unit missing {required}");
    }
    assert!(
        sidecar.contains("After=network-online.target host-tuning.service mantis-relay.service")
            && sidecar.contains("Wants=network-online.target mantis-relay.service")
            && sidecar.contains("ConditionFileIsExecutable=/opt/mantis/engine/bin/relay_sidecar"),
        "sidecar must start after and pull in the relay"
    );
    assert!(
        engine.contains(
            "After=network-online.target host-tuning.service mantis-relay-sidecar.service"
        ) && engine.contains(
            "Wants=network-online.target host-tuning.service mantis-relay-sidecar.service"
        ),
        "engine must start after and pull in the sidecar"
    );
    for required in [
        "ConditionFileIsExecutable=/opt/mantis/engine/bin/engine",
        "ConditionPathExists=/opt/mantis/engine/.env",
        "ConditionPathExists=/opt/mantis/engine/engine.json",
    ] {
        assert!(
            engine.contains(required),
            "engine unit must remain dormant before config/build: {required}"
        );
    }
}

#[test]
fn bootstrap_installs_feed_chain_without_starting_engine() {
    let bootstrap = read_required("engine/deploy/aws/bootstrap-host.sh");

    for required in [
        "libffi-devel libxml2-devel",
        "install_llvm_config_wrapper",
        "libzstd\\.a",
        "b67817634e8e1c2632dfc056af14d61b94f8e6502f4e557560eea227aa22ce37",
        "mantis-relay.service",
        "mantis-relay-sidecar.service",
        "/opt/mantis/engine/bin/run-relay.sh",
        "/opt/mantis/engine/bin/enforce-phase2-baseline.sh",
        "/opt/mantis/engine/bin/activate-phase2-dryrun.sh",
        "/opt/mantis/engine/logs",
        "/run/mantis",
        "systemctl enable mantis-relay.service mantis-relay-sidecar.service",
        "systemctl disable --now mantis-engine.service",
    ] {
        assert!(bootstrap.contains(required), "bootstrap missing {required}");
    }
    assert!(
        !bootstrap.contains("systemctl enable mantis-engine")
            && !bootstrap.contains("systemctl enable --now mantis-engine")
            && !bootstrap.contains("systemctl start mantis-engine")
            && !bootstrap.contains(
                "systemctl enable mantis-relay.service mantis-relay-sidecar.service mantis-engine.service"
            ),
        "bootstrap must neither enable nor start the engine before the explicit Phase-2 activation gate"
    );
    assert!(
        bootstrap.contains("FATAL: ENABLE_REVMC=1"),
        "an explicitly requested revmc bootstrap must fail closed"
    );
}

#[test]
fn phase2_activation_is_fail_closed_and_ordered() {
    let activation = read_required("engine/deploy/aws/activate-phase2-dryrun.sh");

    for required in [
        "enforce-phase2-baseline.sh",
        "DRY_RUN=true",
        "SUBMIT_TX=false",
        "MIRROR_COLD_VERIFY=0",
        "MIRROR_REORG_UNWIND=0",
        "MIRROR_JOURNAL_PERSIST=0",
        "live_submit=false",
        "length == 1",
        "measurement_only == true",
        "aws_dryrun_ephemeral_unfunded",
        "systemctl stop mantis-engine.service",
        "systemctl disable mantis-engine.service",
    ] {
        assert!(
            activation.contains(required),
            "activation helper missing {required}"
        );
    }

    let relay = activation
        .find("systemctl enable --now mantis-relay.service")
        .expect("activation must enable/start relay");
    let sidecar = activation
        .find("systemctl enable --now mantis-relay-sidecar.service")
        .expect("activation must enable/start sidecar");
    let engine = activation
        .find("systemctl enable --now mantis-engine.service")
        .expect("activation must enable/start engine only after the baseline gate");
    assert!(
        relay < sidecar && sidecar < engine,
        "feed chain activation order drifted"
    );
    assert!(
        activation.contains("unset RPC_URL")
            && activation.contains("CURL_CONFIG")
            && !activation.contains("source \"$ENV_FILE\"")
            && !activation.contains("cat \"$ENV_FILE\"")
            && !activation.contains("cat \"$CONFIG_FILE\""),
        "activation helper must keep the endpoint out of argv/output while performing read-only attestation"
    );
}

#[test]
fn build_fails_closed_for_requested_revmc_and_installs_sidecar() {
    let build = read_required("engine/deploy/aws/build.sh");
    let verifier = read_required("engine/deploy/aws/verify-revmc-artifact.sh");

    for required in [
        "FATAL: ENABLE_REVMC=1",
        "SIDECAR_BIN=",
        "verify-revmc-artifact.sh",
        "/opt/mantis/engine/bin/relay_sidecar",
        "install -o mantis -g mantis -m755 \"$SIDECAR_BIN\"",
    ] {
        assert!(build.contains(required), "build missing {required}");
    }
    assert!(
        !build.contains("REVM_BACKEND=aot will degrade gracefully")
            && !build.contains("interpreter build (identical to the default build)"),
        "ENABLE_REVMC=1 must not silently ship an interpreter binary"
    );
    assert!(
        build.find("verify-revmc-artifact.sh")
            < build.find("cp -f \"$DEPLOY_BIN\" /opt/mantis/engine/bin/engine"),
        "REVMc runtime verification must happen before artifact installation"
    );
    for required in ["\"$ARTIFACT\" version", "ldd", "not found", "FATAL:"] {
        assert!(
            verifier.contains(required),
            "REVMc artifact verifier missing {required}"
        );
    }
}

#[test]
fn revmc_artifact_verifier_rejects_smoke_and_linker_failures() {
    let root = unique_temp_dir("revmc_artifact_verifier");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("create verifier fixture bin dir");
    let artifact = root.join("engine");
    let ldd = bin.join("ldd");
    let verifier = repo_root().join("engine/deploy/aws/verify-revmc-artifact.sh");
    let path = format!("{}:/usr/bin:/bin", bin.display());

    write_executable(&artifact, "#!/bin/sh\n[ \"${1:-}\" = version ]\n");
    write_executable(&ldd, "#!/bin/sh\necho 'libc.so.6 => /lib/libc.so.6'\n");
    let ok = Command::new("bash")
        .arg(&verifier)
        .arg(&artifact)
        .env("PATH", &path)
        .output()
        .expect("run successful REVMc verifier fixture");
    assert!(
        ok.status.success(),
        "valid fixture rejected: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    write_executable(&artifact, "#!/bin/sh\nexit 42\n");
    let smoke_fail = Command::new("bash")
        .arg(&verifier)
        .arg(&artifact)
        .env("PATH", &path)
        .output()
        .expect("run smoke-failure fixture");
    assert!(
        !smoke_fail.status.success(),
        "runtime smoke failure must be fatal"
    );

    write_executable(&artifact, "#!/bin/sh\n[ \"${1:-}\" = version ]\n");
    write_executable(&ldd, "#!/bin/sh\necho 'libLLVM.so.22 => not found'\n");
    let missing_lib = Command::new("bash")
        .arg(&verifier)
        .arg(&artifact)
        .env("PATH", &path)
        .output()
        .expect("run unresolved-library fixture");
    assert!(
        !missing_lib.status.success(),
        "unresolved runtime library must be fatal"
    );

    fs::remove_dir_all(root).ok();
}
