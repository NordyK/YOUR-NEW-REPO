//! [S5 · WP1 · ADV-F2] Source guard: the trust-sensitive NORMAL reconcile path is provably unable to
//! consume an unproven `Ok`. `close_block` must reconcile ONLY through the checked seam
//! (`reconcile_block_checked` → `stage_checked` → `StateFetcher::fetch_storage_checked`) and never
//! call the legacy infallible `reconcile_block`, `fetch_storage`, or `try_fetch_storage` (all of which
//! fabricate a value on error). This is a static complement to the runtime fail-closed tests — it
//! cannot by itself prove behavior, but it prevents a future edit from silently re-introducing the
//! Finding-1 fail-open path.
//!
//! Covers both the WP1 normal-close path AND (since WP4 · Fix E) the deep-reorg footprint reconcile:
//! `reconcile_footprint_checked` commits only through the Proven-demanding `apply_checked_batch`, and
//! `Proven::checked` is guard-enforced to a single construction site (`stage_checked`). The legacy
//! infallible `reconcile_footprint` remains only on the `MIRROR_COLD_VERIFY=0` compat path.

use std::{fs, path::Path};

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

/// Return the slice of `src` from `sig` to the next top-level `\nfn ` (approximate function body).
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("signature not found: {sig}"));
    let rest = &src[start..];
    // Bound at the next top-level `fn ` OR the `#[cfg(test)]` module marker (whichever comes first),
    // so extracting the LAST free fn before the tests does not spill into the test module.
    let next_fn = rest[sig.len()..].find("\nfn ").map(|i| i + sig.len());
    let test_mod = rest.find("\n#[cfg(test)]");
    let end = [next_fn, test_mod]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn close_block_reconciles_only_through_the_checked_seam() {
    let src = read("src/shadow_revm.rs");
    let body = fn_body(&src, "fn close_block(");
    assert!(
        body.contains("reconcile_block_checked"),
        "close_block must reconcile through the CHECKED, fail-closed core"
    );
    // The legacy infallible reconcile and its fetch primitives must never appear in the trust path.
    // (`reconcile_block_checked(` does NOT contain the substring `reconcile_block(`.)
    for forbidden in [".reconcile_block(", ".fetch_storage(", ".try_fetch_storage("] {
        assert!(
            !body.contains(forbidden),
            "close_block (trust-sensitive reconcile) must not call `{forbidden}` — it fabricates a \
             value on error; use the checked seam"
        );
    }
}

#[test]
fn checked_reconcile_core_uses_only_the_checked_fetch() {
    let src = read("src/mirror_journal.rs");
    // The shared checked-staging core must consume the checked seam, never infallible fetch_storage.
    let stage = fn_body(&src, "fn stage_checked<");
    assert!(
        stage.contains("fetch_storage_checked"),
        "stage_checked must consume StateFetcher::fetch_storage_checked"
    );
    assert!(
        !stage.contains(".fetch_storage("),
        "stage_checked must not call infallible fetch_storage (`.fetch_storage_checked(` is allowed)"
    );
    let block = fn_body(&src, "pub fn reconcile_block_checked<");
    assert!(
        !block.contains(".fetch_storage("),
        "reconcile_block_checked must not call infallible fetch_storage in the trust path"
    );
    // [WP4 · Fix E] The checked reconcile cores must COMMIT to the mirror through the Proven-demanding
    // `apply_checked_batch`, never the raw `apply_batch` — so an unproven value is type-impossible to
    // apply. `stage_checked` produces `Proven` (its sole construction site).
    assert!(
        stage.contains("Proven<U256>") && stage.contains("Proven::checked("),
        "stage_checked must stage engine_revm::Proven values (the sole Proven construction site)"
    );
    for core in ["pub fn reconcile_block_checked<", "pub(crate) fn reconcile_footprint_checked<"] {
        let body = fn_body(&src, core);
        assert!(
            body.contains("apply_checked_batch"),
            "{core} must commit through apply_checked_batch (Proven-demanding)"
        );
        assert!(
            !body.contains(".apply_batch("),
            "{core} must NOT commit via the raw apply_batch — use apply_checked_batch (Proven)"
        );
    }
}

#[test]
fn proven_checked_is_constructed_only_in_stage_checked() {
    // [WP4 · FIX-A/L1] `Proven::checked` is the type-level "this value flowed through the checked seam"
    // constructor. It must be built in exactly ONE place — `stage_checked`, right after the value returns
    // from `fetch_storage_checked` — so "sole construction site" is GUARD-ENFORCED, not merely claimed.
    // A `Proven::checked(...)` anywhere else (e.g. fabricating a proven ZERO for an unstaged footprint
    // slot, the L1 finding) would be a latent fail-open: an unproven value laundered into a Proven.
    // Scope to PRODUCTION code (before the `#[cfg(test)]` module): the guard is about the trust path,
    // not tests — a unit test legitimately constructs a `Proven` to exercise `activate()`/hydration.
    let production = |src: &str| -> String {
        src.split("\n#[cfg(test)]").next().unwrap_or(src).to_string()
    };
    let journal = production(&read("src/mirror_journal.rs"));
    let total = journal.matches("Proven::checked(").count();
    assert_eq!(
        total, 1,
        "expected exactly ONE Proven::checked( call site in mirror_journal.rs (production), found {total}"
    );
    let stage = fn_body(&journal, "fn stage_checked<");
    assert_eq!(
        stage.matches("Proven::checked(").count(),
        1,
        "the sole Proven::checked( construction must live inside stage_checked"
    );
    // The other engine trust-path files must never construct a Proven directly in PRODUCTION — they
    // receive them from stage_checked (as `Proven<U256>`) and consume via `.get()` / `apply_checked_batch`.
    for f in ["src/shadow_revm.rs", "src/state_mirror.rs"] {
        assert_eq!(
            production(&read(f)).matches("Proven::checked(").count(),
            0,
            "{f} (production) must not construct Proven values — only stage_checked may"
        );
    }
}
