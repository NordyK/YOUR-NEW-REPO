use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const LEGACY_EXECUTOR_MARKERS: &[&str] = &[
    "ArbitrageExecutor",
    "EXECUTE_SELECTOR",
    "CalldataTemplate",
    "RouteKind::selector",
    "execute(uint256,uint256,uint256,uint256[8])",
];

fn engine_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("engine/crates/engine has engine root ancestor")
        .to_path_buf()
}

fn repo_root() -> PathBuf {
    engine_root()
        .parent()
        .expect("engine root has repo root parent")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn assert_no_legacy_markers(path: &Path) {
    let text = read(path);
    for marker in LEGACY_EXECUTOR_MARKERS {
        assert!(
            !text.contains(marker),
            "{} still contains legacy marker {marker:?}",
            path.display()
        );
    }
}

#[test]
fn active_code_and_deploy_surfaces_are_mantis_only() {
    let engine = engine_root();

    assert!(
        engine.join("contracts/src/MantisExecutor.sol").is_file(),
        "production executor source must be MantisExecutor.sol"
    );
    assert!(
        !engine.join("contracts/src/ArbitrageExecutor.sol").exists(),
        "legacy ArbitrageExecutor source must stay removed"
    );
    assert!(
        !engine
            .join("contracts/test/ArbitrageExecutor.t.sol")
            .exists(),
        "legacy ArbitrageExecutor ABI-shape test must stay removed"
    );

    for rel in [
        "contracts/foundry.toml",
        "crates/engine-routes/src/calldata.rs",
        "crates/engine-routes/src/lib.rs",
        "crates/engine/tests/end_to_end.rs",
        "crates/engine/benches/pipeline.rs",
        "crates/engine/examples/latency_percentiles.rs",
        "crates/engine/src/pipeline.rs",
        "deploy/aws/engine.env.maxlogging.example",
    ] {
        let path = engine.join(rel);
        assert!(path.exists(), "missing active surface {}", path.display());
        assert_no_legacy_markers(&path);
    }
}

#[test]
#[ignore = "docs-lint: reads repository Markdown; excluded from correctness certification"]
fn living_docs_only_reference_legacy_executor_as_removed() {
    let repo = repo_root();
    for rel in [
        "ARCHITECTURE.md",
        "Documents/DEPLOYMENT_READINESS.md",
        "Documents/LIVE_DEPLOYMENT.md",
        "Documents/WORLD_CLASS_REVIEW_2026.md",
    ] {
        let path = repo.join(rel);
        let text = read(&path);
        for (line_no, line) in text.lines().enumerate() {
            if !line.contains("ArbitrageExecutor") {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            assert!(
                lower.contains("removed")
                    || lower.contains("legacy")
                    || lower.contains("superseded")
                    || lower.contains("placeholder"),
                "{}:{} references ArbitrageExecutor without removed/legacy context: {line}",
                path.display(),
                line_no + 1
            );
        }
    }
}

#[test]
fn mantis_artifact_is_available_for_revm_executor_deployment_gate() {
    let engine = engine_root();
    let artifact_path = engine.join("contracts/out/MantisExecutor.sol/MantisExecutor.json");
    if !artifact_path.exists() {
        eprintln!(
            "SKIP: artifact {} absent (out-of-branch; run `forge build`)",
            artifact_path.display()
        );
        return;
    }
    let pipeline = read(&engine.join("crates/engine/src/pipeline.rs"));
    assert!(
        pipeline.contains("contracts/out/MantisExecutor.sol/MantisExecutor.json"),
        "pipeline must keep the default Mantis forge artifact path wired"
    );
    assert!(
        pipeline.contains("mantis_init_code_available"),
        "pipeline must keep the Mantis artifact availability gate wired"
    );

    let artifact_raw = read(&artifact_path);
    let artifact: Value = serde_json::from_str(&artifact_raw)
        .unwrap_or_else(|err| panic!("parse {}: {err}", artifact_path.display()));
    let bytecode = artifact
        .pointer("/bytecode/object")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{} missing bytecode.object", artifact_path.display()));

    assert!(
        bytecode.starts_with("0x"),
        "{} bytecode.object must be hex-prefixed",
        artifact_path.display()
    );
    assert!(
        bytecode.len() > 100_000,
        "{} bytecode.object unexpectedly short: {} bytes",
        artifact_path.display(),
        bytecode.len()
    );
}
