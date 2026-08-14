use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

const MATRIX: &str = "Codex/GAS_ECONOMICS_CERTIFICATION_MATRIX_2026-06-15.json";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("engine/crates/engine has repo root ancestor")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|err| panic!("run sha256sum {}: {err}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha256sum output must be utf8")
        .split_whitespace()
        .next()
        .expect("sha256sum output must include hash")
        .to_string()
}

fn git_diff_clean(paths: &[&str]) -> bool {
    let output = Command::new("git")
        .arg("diff")
        .arg("--exit-code")
        .arg("--")
        .args(paths)
        .current_dir(repo_root())
        .output()
        .expect("run git diff");
    output.status.success()
}

#[test]
fn direct_gas_and_profit_formula_sources_are_unchanged_from_head() {
    assert!(
        git_diff_clean(&[
            "engine/crates/engine-gas/src/lib.rs",
            "engine/crates/engine-sim/src/profit.rs",
            "Codex/GATE9_B_BINARY_MONITORED_RERUN_2026-06-16.json",
        ]),
        "direct gas/profit formula files or the tracked Gate9 report drifted from HEAD; stop and inspect before repinning economics artifacts"
    );
}

#[test]
fn current_matrix_sources_match_current_files() {
    let root = repo_root();
    let matrix_path = root.join(MATRIX);
    let matrix: Value =
        serde_json::from_str(&read(&matrix_path)).expect("gas economics matrix must parse");
    let sources = matrix
        .get("sources")
        .and_then(Value::as_object)
        .expect("matrix sources must be an object");

    let mut mismatches = BTreeMap::<String, Vec<String>>::new();
    for (name, source) in sources {
        let rel_path = source
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("source {name} missing path"));
        let path = root.join(rel_path);
        let expected_bytes = source
            .get("bytes")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("source {name} missing bytes"));
        let expected_sha = source
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("source {name} missing sha256"));
        let actual_bytes = fs::metadata(&path)
            .unwrap_or_else(|err| panic!("metadata {}: {err}", path.display()))
            .len();
        let actual_sha = sha256_file(&path);

        if actual_bytes != expected_bytes {
            mismatches
                .entry(name.clone())
                .or_default()
                .push(format!("bytes {actual_bytes} != {expected_bytes}"));
        }
        if actual_sha != expected_sha {
            mismatches
                .entry(name.clone())
                .or_default()
                .push(format!("sha256 {actual_sha} != {expected_sha}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "gas economics matrix source pins must match the current checkout after recertification: {mismatches:#?}"
    );
}
