//! WI-06 T3 fix round 9 — N1 static contract: the shadow POST-STATE promotion (the Shadow-origin
//! resident-store write) is EXPERIMENTAL and DEFAULT-OFF, gated at the SOLE promotion call site.
//!
//! codex round-9 (`codex/WI06_T3_ROUND9_VERIFY_2026-07-19.md`) proved the shadow PRODUCER cannot be
//! made fail-closed at this scope (a whole-transaction repeated-pool round-trip restores tick + L
//! with a stale interior sqrt; Algebra's timepoint/adaptive-fee state is outside `V3State`). The
//! orchestrator decision: shadow promotion becomes an EXPERIMENTAL, DEFAULT-OFF feature deferred to
//! WI-09; the shadow lane stays fully alive as diagnostics; only the resident-store WRITE is
//! disarmed by default. This test PINS that disarm structurally so it cannot silently regress to a
//! default-on unguarded submit:
//!
//!  1. `shadow_revm.rs` calls `submit_offthread_family_write(` EXACTLY once (the sole promotion call
//!     site), and that call lives INSIDE the gate fn `submit_shadow_promotion_if_enabled`, whose body
//!     early-returns (routing to the parity+resync diagnostic sink) when `!promotion_enabled` BEFORE
//!     reaching the submit.
//!  2. The gate reads `SHADOW_PROMOTION` and DEFAULTS OFF — no default-ON read exists.
//!  3. `promotion_enabled` is threaded to `process_job` (so the per-worker read reaches the call
//!     site), and both worker loops read the gate.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// The production source of `shadow_revm.rs` with its trailing `#[cfg(test)] mod tests { ... }`
/// removed (the promotion gate lives entirely in production code; the runtime tests that exercise
/// it must not be miscounted as call sites).
fn shadow_revm_production_src() -> String {
    let path = repo_root().join("engine/crates/engine/src/shadow_revm.rs");
    let raw = std::fs::read_to_string(&path).expect("read shadow_revm.rs");
    match raw.find("#[cfg(test)]") {
        Some(idx) => raw[..idx].to_string(),
        None => raw,
    }
}

/// The name of the top-level (`fn ` at column 0) function that lexically ENCLOSES the byte at
/// `pos` — the nearest `\nfn NAME` before it.
fn enclosing_top_level_fn(src: &str, pos: usize) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    let needle = "\nfn ";
    let mut search = 0usize;
    while let Some(rel) = src[search..].find(needle) {
        let at = search + rel;
        if at >= pos {
            break;
        }
        let name_start = at + needle.len();
        let name: String = src[name_start..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        best = Some((at, name));
        search = name_start;
    }
    best.map(|(_, name)| name)
}

#[test]
fn shadow_promotion_submit_is_gated_at_the_sole_call_site() {
    let src = shadow_revm_production_src();

    // (1) The submit to the sole publisher — the Shadow-origin resident-store write — appears
    // EXACTLY once as a call. (The `use` import carries no `(`, so this matches only the call.)
    let call = "submit_offthread_family_write(";
    let count = src.matches(call).count();
    assert_eq!(
        count, 1,
        "expected EXACTLY ONE `submit_offthread_family_write(` call site in shadow_revm.rs (the sole \
         shadow promotion submit) — found {count}. A second call site would be an ungated promotion \
         path outside the N1 default-off gate."
    );
    let call_pos = src.find(call).expect("the sole call site");

    // (2) That sole call site is INSIDE the gate fn `submit_shadow_promotion_if_enabled`.
    let enclosing = enclosing_top_level_fn(&src, call_pos)
        .expect("the submit call must sit inside a top-level fn");
    assert_eq!(
        enclosing, "submit_shadow_promotion_if_enabled",
        "the sole `submit_offthread_family_write` call must live inside the N1 gate fn \
         `submit_shadow_promotion_if_enabled` (found it inside `{enclosing}`) — the default-off gate \
         must sit AT the promotion call site, never bypassed"
    );

    // (3) Inside that gate fn, `if !promotion_enabled` early-returns BEFORE the submit — so the
    // default-off path (routing to the parity+resync diagnostic sink) is reached without ever
    // touching the publisher.
    let gate_start = src
        .find("fn submit_shadow_promotion_if_enabled")
        .expect("gate fn defined");
    let guard = "if !promotion_enabled";
    let guard_pos = src[gate_start..]
        .find(guard)
        .map(|r| gate_start + r)
        .unwrap_or_else(|| panic!("gate fn must early-return on `{guard}`"));
    assert!(
        guard_pos < call_pos,
        "the `if !promotion_enabled` early-return must precede the `submit_offthread_family_write` \
         call — the OFF (default) path must never reach the submit"
    );
}

#[test]
fn shadow_promotion_flag_defaults_off_and_is_never_default_on() {
    let src = shadow_revm_production_src();

    // The gate reads the SHADOW_PROMOTION env var.
    assert!(
        src.contains("\"SHADOW_PROMOTION\""),
        "the N1 gate must read the `SHADOW_PROMOTION` env var"
    );
    assert!(
        src.contains("fn shadow_promotion_enabled"),
        "the N1 gate helper `shadow_promotion_enabled` must exist"
    );

    // DEFAULT-OFF: the pure parser maps a MISSING value to `false`. No default-on reader may exist.
    assert!(
        src.contains("fn parse_shadow_promotion"),
        "the N1 gate must parse via `parse_shadow_promotion` so the default (missing) case is \
         explicit and unit-testable"
    );
    assert!(
        !src.contains("env_flag_default_on(\"SHADOW_PROMOTION\")"),
        "SHADOW_PROMOTION must NOT be read default-ON — it is an EXPERIMENTAL default-OFF gate"
    );
    assert!(
        !src.contains("\"SHADOW_PROMOTION\", true"),
        "SHADOW_PROMOTION must NOT be read with a default-ON (`, true`) — it is default-OFF"
    );

    // The loud one-shot operator warning exists for the explicit opt-in.
    assert!(
        src.contains("EXPERIMENTAL shadow promotion"),
        "enabling the experimental gate must emit a LOUD operator warning naming it EXPERIMENTAL"
    );
}

#[test]
fn promotion_enabled_is_threaded_to_process_job_and_read_by_both_workers() {
    let src = shadow_revm_production_src();

    // `process_job` carries the per-worker gate decision to the call site. Bound the scan to the
    // signature — from `fn process_job(` up to its `\n) {` terminator (params/comments may contain
    // parentheses, so a naive first-`)` scan is wrong).
    let pj = src.find("fn process_job(").expect("process_job defined");
    let pj_sig_end = src[pj..]
        .find("\n) {")
        .map(|r| pj + r)
        .expect("process_job signature terminator");
    assert!(
        src[pj..pj_sig_end].contains("promotion_enabled: bool"),
        "`process_job` must take `promotion_enabled: bool` so the sole call site is gated per-worker"
    );

    // Both worker loops read the gate once (and warn on opt-in).
    for worker in ["fn shadow_loop(", "fn advancer_loop("] {
        let start = src.find(worker).unwrap_or_else(|| panic!("{worker} defined"));
        // Bound the scan to this fn (up to the next top-level `\nfn `).
        let rest = &src[start + worker.len()..];
        let end = rest.find("\nfn ").map(|r| start + worker.len() + r).unwrap_or(src.len());
        assert!(
            src[start..end].contains("shadow_promotion_enabled()"),
            "`{worker}` must read `shadow_promotion_enabled()` once and thread it into `process_job`"
        );
    }
}
