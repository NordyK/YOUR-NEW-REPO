//! WI-06 F1 fix-round-2 (codex item 1) — brand TOTALITY static contract.
//!
//! Round 1 branded the permit + stores, but codex re-FAIL item 1 found the brand was not TOTAL:
//! public production constructors still minted `WILDCARD` stores, `assert_universe` accepted a
//! `WILDCARD` store under any brand in EVERY build, and `SealedUniverse`'s `universe`/`permit`
//! fields were public — so a foreign builder's permit could replace a sealed store (or write a
//! swapped-in `WILDCARD` store) with no `test_mint`.
//!
//! These assertions scan engine-state SOURCE (cfg-independent — they read the `.rs` text, so they
//! hold regardless of which features a given `cargo` invocation unifies on; a runtime test cannot
//! prove the production shape because `cargo test`/`--all-targets` turns `test-permit` ON). They
//! pin the three structural closures:
//!   1. every resident store is BORN BRANDED in production (`new_branded`); the WILDCARD `new()` /
//!      `with_capacity()` / `Default` are compiled out of any production build;
//!   2. `assert_universe`'s WILDCARD escape exists ONLY under the test cfg — production requires an
//!      exact brand match (a WILDCARD store fails closed against a real permit, never fails open);
//!   3. `SealedUniverse`'s universe/permit are private and the sealed universe is handed out
//!      `Arc`-wrapped (`into_parts`), so no owned `&mut Universe` ever escapes to have a store
//!      field replaced before `Arc` wrapping.

use std::path::PathBuf;

/// The exact cfg that gates every test-only wildcard surface (mirrors `test_mint`).
const TEST_CFG: &str = r#"#[cfg(any(test, loom, feature = "test-permit"))]"#;
const PROD_CFG: &str = r#"#[cfg(not(any(test, loom, feature = "test-permit")))]"#;

/// The ten family state stores plus the pool table — every resident store branded by the builder,
/// paired with the store TYPE (so the `Default` check targets the STORE, not an unrelated
/// value-type `Default` such as `impl Default for V3State`).
const STORE_FILES: &[(&str, &str)] = &[
    ("v2_state.rs", "V2StateStore"),
    ("v3_state.rs", "V3StateStore"),
    ("fluid_state.rs", "FluidStateStore"),
    ("dodo_state.rs", "DodoStateStore"),
    ("balancer_state.rs", "BalancerStateStore"),
    ("balancer_weighted_state.rs", "BalancerWeightedStateStore"),
    ("curve_state.rs", "CurveStateStore"),
    ("curve_crypto_state.rs", "CryptoCurveStateStore"),
    ("lb_state.rs", "LbStateStore"),
    ("maverick_state.rs", "MaverickStateStore"),
    ("pool.rs", "PoolTable"),
];

fn read_src(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True iff one of the `window` non-blank lines immediately preceding the FIRST line containing
/// `needle` is (normalized) the test cfg attribute — i.e. `needle`'s item is test-only.
fn is_test_gated(src: &str, needle: &str, window: usize) -> bool {
    let lines: Vec<&str> = src.lines().collect();
    let cfg = norm(TEST_CFG);
    for (idx, line) in lines.iter().enumerate() {
        if line.contains(needle) {
            let lo = idx.saturating_sub(window);
            for prev in &lines[lo..idx] {
                if norm(prev) == cfg {
                    return true;
                }
            }
            return false;
        }
    }
    // Absent needle ⇒ vacuously satisfied (store may lack that ctor).
    true
}

fn contains_norm(src: &str, needle: &str) -> bool {
    norm(src).contains(&norm(needle))
}

/// 1. Every resident store is born branded in production; the WILDCARD constructors are test-only.
#[test]
fn production_stores_are_born_branded_and_wildcard_ctors_are_test_only() {
    let mut failures = Vec::new();
    for (file, store_ty) in STORE_FILES {
        let src = read_src(file);
        // Production constructor exists and takes the builder's brand.
        if !src.contains("pub(crate) fn new_branded(") {
            failures.push(format!(
                "{file}: missing `pub(crate) fn new_branded(universe: UniverseId)` — the ONLY \
                 production constructor; a store must be born branded, never WILDCARD"
            ));
        }
        // WILDCARD constructors, if present, are compiled out of production.
        if src.contains("pub fn new() -> Self") && !is_test_gated(&src, "pub fn new() -> Self", 4) {
            failures.push(format!(
                "{file}: `pub fn new() -> Self` (WILDCARD) is NOT behind `{TEST_CFG}` — a \
                 production build can still mint an unbranded store"
            ));
        }
        if src.contains("pub fn with_capacity(")
            && !is_test_gated(&src, "pub fn with_capacity(", 4)
        {
            failures.push(format!(
                "{file}: `pub fn with_capacity(` (WILDCARD) is NOT behind `{TEST_CFG}`"
            ));
        }
        // Target the STORE's Default (not an unrelated value-type Default in the same file).
        let store_default = format!("impl Default for {store_ty}");
        if src.contains(&store_default) && !is_test_gated(&src, &store_default, 4) {
            failures.push(format!(
                "{file}: `{store_default}` (mints a WILDCARD store via `new()`) is NOT behind \
                 `{TEST_CFG}` — `{store_ty}::default()` is a production WILDCARD-construction path"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "brand-totality store-constructor violations:\n  {}",
        failures.join("\n  ")
    );
}

/// Extract the source text of `fn <name>(` through its matching close brace (brace-tracking from
/// the first `{` after the signature). Panics if the function or its body is not found.
fn fn_body(src: &str, sig_needle: &str) -> String {
    let start = src
        .find(sig_needle)
        .unwrap_or_else(|| panic!("function `{sig_needle}` not found"));
    let rest = &src[start..];
    let open = rest.find('{').expect("function body open brace");
    let mut depth = 0i64;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return rest[..open + i + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated body for `{sig_needle}`");
}

/// 2. The WILDCARD escape in `assert_universe` exists ONLY under the test cfg; production requires
///    an exact brand match. Scanned WITHIN the function body so it cannot be satisfied by an
///    unrelated cfg elsewhere in the file (e.g. `test_mint`).
#[test]
fn assert_universe_wildcard_escape_is_test_only() {
    let src = read_src("publication.rs");
    let body = fn_body(&src, "pub fn assert_universe(");
    // Production branch: an exact-match `authorized` binding gated to NOT(test/loom/test-permit).
    assert!(
        contains_norm(&body, PROD_CFG),
        "assert_universe must carry a production branch `{PROD_CFG}` requiring an EXACT brand \
         match (no WILDCARD escape in a release build). Body:\n{body}"
    );
    // Test branch: the WILDCARD-accepting binding gated to the test cfg, INSIDE assert_universe.
    assert!(
        contains_norm(&body, TEST_CFG),
        "assert_universe's WILDCARD-accepting arm must be gated to `{TEST_CFG}`. Body:\n{body}"
    );
    // Fail-closed shape: `UniverseId::WILDCARD` may be compared ONLY after the test cfg gate — it
    // must not appear before the production gate (which would be an ungated wildcard escape).
    let nb = norm(&body);
    let prod_at = nb.find(&norm(PROD_CFG)).expect("prod cfg in body");
    let wildcard_at = nb
        .find("UniverseId :: WILDCARD")
        .or_else(|| nb.find("UniverseId::WILDCARD"))
        .expect("wildcard compared in body");
    assert!(
        wildcard_at > prod_at,
        "every `UniverseId::WILDCARD` comparison in assert_universe must sit under the test-cfg \
         branch (after the production exact-match branch), never ungated"
    );
}

/// 3. `SealedUniverse` cannot have a resident store replaced out of band: its `universe`/`permit`
///    are private and it hands out an `Arc`-wrapped universe (`into_parts`), so no owned
///    `&mut Universe` escapes the crate.
#[test]
fn sealed_universe_is_not_externally_replaceable() {
    let src = read_src("builder.rs");
    let start = src
        .find("pub struct SealedUniverse")
        .expect("SealedUniverse struct present");
    let block = &src[start..];
    let end = block.find('}').expect("SealedUniverse block close");
    let fields = &block[..end];
    assert!(
        !fields.contains("pub universe"),
        "SealedUniverse.universe must be PRIVATE (accessor-only) — a public field lets foreign \
         code replace a sealed store before Arc wrapping"
    );
    assert!(
        !fields.contains("pub permit"),
        "SealedUniverse.permit must be PRIVATE (accessor-only)"
    );
    // The sealed universe is handed out Arc-wrapped through a move-out accessor.
    assert!(
        src.contains("pub fn into_parts("),
        "SealedUniverse must expose `into_parts(self) -> (Arc<Universe>, StateWriterPermit)` — the \
         universe leaves the crate ALREADY Arc-wrapped, so no owned &mut Universe can be mutated"
    );
    assert!(
        contains_norm(&src, "universe: std::sync::Arc<Universe>")
            || contains_norm(&src, "universe: Arc<Universe>"),
        "SealedUniverse.universe must hold an Arc<Universe> (built + wrapped inside seal), not a \
         moveable owned Universe"
    );
}

/// 4. `UniverseBuilder::build` is NOT a production public constructor of an owned `Universe`. A
///    public `build` returns an owned `Universe` whose resident store fields are all `pub`, so a
///    release-build caller could replace a sealed store BEFORE any `Arc` wrapping (the `seal()`
///    path is the only production route, and it hands out the universe already `Arc`-wrapped). In
///    production `build` is `pub(crate)`; the PUBLIC `build` used by cross-crate tests/benches is
///    gated behind the test cfg (`test-permit`), exactly like the WILDCARD constructors.
#[test]
fn universe_build_is_crate_internal_in_production() {
    let src = read_src("builder.rs");
    // A crate-internal `build` must exist (the production finaliser used by `seal()`).
    assert!(
        contains_norm(&src, "pub(crate) fn build("),
        "UniverseBuilder::build must be `pub(crate)` in production — external callers go through \
         `seal()` so the universe leaves the crate already Arc-wrapped (no owned &mut Universe)"
    );
    // Any PUBLIC `build` (for cross-crate tests/benches) must be behind the test cfg, never a
    // release constructor of an owned Universe with public store fields.
    if src.contains("pub fn build(") {
        assert!(
            is_test_gated(&src, "pub fn build(", 4),
            "a `pub fn build(` on UniverseBuilder must be gated `{TEST_CFG}` — a release build must \
             not expose an owned-Universe constructor whose public store fields can be swapped out"
        );
    }
}

/// 5. The pool-table control-plane mutators (`set_tier`/`set_score`/`record_update`) and every
///    family store `allocate(&mut self)` are NOT part of the public post-seal mutation surface:
///    each is `pub(crate)` (builder / control-plane, pre-seal only). Combined with (3) (no owned
///    `&mut Universe` post-seal), they are unreachable to swap resident state out of band, and are
///    not even a public unwitnessed `&mut` write path in-crate.
#[test]
fn control_plane_mutators_and_family_allocate_are_crate_internal() {
    let mut failures = Vec::new();
    let pool_src = read_src("pool.rs");
    for m in ["set_tier", "set_score", "record_update"] {
        let public = format!("pub fn {m}(");
        let crate_only = format!("pub(crate) fn {m}(");
        if pool_src.contains(&public) {
            failures.push(format!(
                "pool.rs: `{public}` must be `pub(crate)` — a `&mut self` control-plane mutator \
                 must not be part of the public (post-seal) mutation surface"
            ));
        }
        if !pool_src.contains(&crate_only) {
            failures.push(format!(
                "pool.rs: missing `{crate_only}` — the control-plane mutator must exist as \
                 crate-internal (builder/control-plane use only)"
            ));
        }
    }
    for (file, _ty) in STORE_FILES {
        if *file == "pool.rs" {
            continue; // the pool table has no `allocate`; its mutators are checked above
        }
        let src = read_src(file);
        if src.contains("pub fn allocate(") {
            failures.push(format!(
                "{file}: `pub fn allocate(` must be `pub(crate)` — slot allocation is a pre-seal \
                 builder path, never a public post-seal `&mut self` surface"
            ));
        }
        if !src.contains("pub(crate) fn allocate(") {
            failures.push(format!(
                "{file}: missing `pub(crate) fn allocate(` — allocate must be crate-internal"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "control-plane mutator / family-allocate visibility violations:\n  {}",
        failures.join("\n  ")
    );
}
