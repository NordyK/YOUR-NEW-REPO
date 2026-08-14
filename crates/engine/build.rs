//! Capture the git commit hash at build time so the binary can print its provenance at
//! startup (Gate 0 of the certification runbook / P0.5 provenance manifest).
//!
//! Resolution order (P0.5, Codex 2026-07-10 — every box startup logged commit_hash=unknown
//! because deploy/aws/sync-to-host.sh excludes `.git/`):
//!   1. `git rev-parse HEAD` (+ `git status --porcelain` → dirty flag) when a git tree exists;
//!   2. the `.engine_commit` stamp file at the workspace root (written by sync-to-host.sh from
//!      the LOCAL tree before rsync — the box builds from a git-less copy);
//!   3. the `ENGINE_COMMIT_OVERRIDE` build environment variable;
//!   4. "unknown" (a REQUIRE_PROVENANCE=1 run refuses to boot on this).
use std::process::Command;

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn json_string_field(contents: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let tail = contents.split_once(&key)?.1;
    let value = tail.split_once(':')?.1.trim_start();
    let quoted = value.strip_prefix('"')?;
    Some(quoted.split_once('"')?.0.to_string())
}

fn main() {
    let git_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dirty = if git_sha.is_some() {
        Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                if o.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
                    "dirty"
                } else {
                    "clean"
                }
            })
            .unwrap_or("unknown")
    } else {
        "unknown"
    };
    let stamp = std::fs::read_to_string("../../.engine_commit")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let sha = git_sha
        .or(stamp)
        .or_else(|| std::env::var("ENGINE_COMMIT_OVERRIDE").ok())
        .unwrap_or_else(|| "unknown".to_string());
    // sync-to-host.sh creates and verifies this content digest before the git-less AWS build.
    // build.sh exports it after a second verification; reading the manifest directly keeps Cargo
    // rebuilds and non-wrapper invocations deterministic as well.  Never accept malformed input.
    let source_manifest_sha256 = std::env::var("ENGINE_SOURCE_MANIFEST_SHA256")
        .ok()
        .filter(|value| valid_sha256(value))
        .or_else(|| {
            std::fs::read_to_string("../../.engine_source_manifest.json")
                .ok()
                .and_then(|contents| json_string_field(&contents, "source_manifest_sha256"))
                .filter(|value| valid_sha256(value))
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ENGINE_GIT_COMMIT={sha}");
    println!("cargo:rustc-env=ENGINE_GIT_DIRTY={dirty}");
    println!("cargo:rustc-env=ENGINE_SOURCE_MANIFEST_SHA256={source_manifest_sha256}");
    // Re-run if HEAD moves / the stamp changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../.engine_commit");
    println!("cargo:rerun-if-changed=../../.engine_source_manifest.json");
    println!("cargo:rerun-if-env-changed=ENGINE_COMMIT_OVERRIDE");
    println!("cargo:rerun-if-env-changed=ENGINE_SOURCE_MANIFEST_SHA256");
}
