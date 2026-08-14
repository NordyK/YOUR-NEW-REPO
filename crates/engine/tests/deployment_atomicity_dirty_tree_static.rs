//! Deployment-atomicity guard: the release surface (rust/solidity/config/script/deploy files) must be
//! COMMITTED — this test is RED while tracked release-surface files are dirty and clears on commit.
//!
//! [WP3 F3 — orchestration note, not a code fix] This guard reads `git status` of the shared working
//! tree, so it is inherently sensitive to CONCURRENT writers. If multiple agents (or a parallel
//! codegen/format step) touch tracked files while this test runs, it can flap between RED and GREEN
//! independent of any code change. A runner must SERIALIZE this guard against other tree-mutating work
//! (run it after the tree settles / commits are made, not interleaved with concurrent edits). This is
//! an orchestration property of the harness, not a defect in the guard or the code under test.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const GO_NO_GO: &str = "Codex/LIVE_CAPITAL_GO_NO_GO_2026-06-19.json";

#[derive(Debug, Clone, Eq, PartialEq)]
struct StatusEntry {
    status: String,
    path: String,
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("engine/crates/engine has repo root ancestor")
        .to_path_buf()
}

fn git_status(repo: &Path) -> Vec<StatusEntry> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .expect("run git status");
    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            assert!(
                line.len() >= 4,
                "unexpected porcelain status line: {line:?}"
            );
            let status = line[..2].to_string();
            let path = line[3..]
                .split(" -> ")
                .last()
                .expect("status path")
                .to_string();
            StatusEntry { status, path }
        })
        .collect()
}

fn read_required(repo: &Path, rel: &str) -> String {
    let path = repo.join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn release_surface_kind(path: &str) -> Option<&'static str> {
    if matches!(
        path,
        ".gitignore" | "engine/.gitignore" | "engine/Cargo.toml"
    ) || (path.starts_with("engine/crates/") && path.ends_with("/Cargo.toml"))
    {
        return Some("repo_or_cargo_config");
    }
    if path.starts_with("engine/deploy/") {
        return Some("aws_deploy_surface");
    }
    if path.starts_with("engine/scripts/") {
        return Some("operator_script_surface");
    }
    if path.starts_with("engine/contracts/src/") || path.starts_with("engine/contracts/script/") {
        return Some("solidity_release_surface");
    }
    if path.starts_with("engine/crates/")
        && (path.contains("/src/") || path.contains("/benches/") || path.contains("/examples/"))
    {
        return Some("rust_release_surface");
    }
    None
}

#[test]
fn committed_release_surface_keeps_live_capital_no_go_until_aws_gate() {
    // 2026-07-04 recertification: the operator reviewed and committed the exact tested tree (143fba0),
    // closing the deploy-atomicity DIRTY-TREE gate. This guard now asserts the COMMITTED state — the
    // release/deploy surface is clean (no rust/solidity/config/script/deploy file is uncommitted) —
    // AND that AWS LIVE CAPITAL REMAINS NO_GO (Gate-10 unrun). It is a regression guard: it trips
    // again if any release-surface file drifts uncommitted, or if the matrix ever flips live capital
    // to "go" without the AWS-measurement preconditions being green.
    let repo = repo_root();
    let mut dirty_release_surface = Vec::new();
    let mut by_kind: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();

    for entry in git_status(&repo) {
        if let Some(kind) = release_surface_kind(&entry.path) {
            by_kind
                .entry(kind)
                .or_default()
                .push(format!("{} {}", entry.status, entry.path));
            dirty_release_surface.push(entry);
        }
    }

    assert!(
        dirty_release_surface.is_empty(),
        "release/deploy surface must be fully committed for deploy atomicity; still dirty: {by_kind:#?}"
    );

    let go_no_go = read_required(&repo, GO_NO_GO);
    for required in [
        "\"aws_dry_run\": \"go\"",
        "\"aws_live_capital\": \"no_go\"",
        "\"id\": \"deploy_atomicity_green\"",
        "\"current_status\": \"green_operator_reviewed_and_committed_20260704\"",
        "Review dirty tree and commit the exact tested state.",
        "git status --short && cargo test --workspace && cargo build --workspace --release",
    ] {
        assert!(
            go_no_go.contains(required),
            "go/no-go matrix must pin the committed deployment boundary: {required}"
        );
    }

    // Live capital MUST stay no-go until the AWS-gated preconditions are green.
    assert!(
        !go_no_go.contains("\"aws_live_capital\": \"go\""),
        "live capital must remain no_go until AWS Gate-10 preconditions are green"
    );
    assert!(
        go_no_go.contains("\"live_capital_may_run_on_aws\": false"),
        "hard boundary must keep live capital disabled on AWS"
    );
}
