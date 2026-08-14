//! WI-06 T3 fix round 5 (codex round-5 item 5b) — production repair-order binding.
//!
//! codex round-5 nit I5b: the `stamp_repair_then_read` reorder-killer (which proves the early-stamp
//! seam captures the generation + poison version BEFORE the pinned reads) was definition+tests ONLY —
//! the LIVE Algebra param sampler (`param_sweep_algebra_pool`) captured its stamps INLINE, bypassing
//! the helper, so the reorder-killer could not kill a PRODUCTION reorder (moving the live sampling
//! after the RPC reads).
//!
//! Fix: `param_sweep_algebra_pool` now routes its early-stamp through `stamp_repair_then_read` — the
//! helper captures the stamps before calling `source_read` BY CONSTRUCTION, so the RPC reads live
//! inside its closure and cannot be reordered ahead of the stamp. This static contract FAILS (RED) if
//! the live sampler drifts back to an inline capture (an inline `.generation(pool_id)` stamp, or the
//! `fetch_algebra_*` reads escaping the helper's closure).
//!
//! WI-06 T3 fix round 6 (codex K7 / J5 nit): the round-5 contract bound the RPC reads but NOT the
//! resident `v3_state.snapshot(slot)` — moving that snapshot BEFORE the helper still passed, branding
//! stale resident state with the later stamped generation. The ORDER check below now also binds the
//! snapshot AFTER the stamp; the reorder mutant (snapshot hoisted ahead of `stamp_repair_then_read(`)
//! fails this test.

use std::path::PathBuf;

fn pipeline_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pipeline.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Slice a function's body: from `fn <name>(` up to the next top-of-line `fn ` (bounds a free fn).
fn fn_slice<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("`fn {name}(` not found in pipeline.rs"));
    let rest = &src[start..];
    let end = rest[3..].find("\nfn ").map(|e| e + 3).unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn param_sweep_routes_early_stamp_through_helper() {
    let src = pipeline_src();
    let body = fn_slice(&src, "param_sweep_algebra_pool");

    // 1. The live sampler routes its early-stamp through the shared reorder-safe seam.
    let stamp_at = body.find("stamp_repair_then_read(").unwrap_or_else(|| {
        panic!(
            "param_sweep_algebra_pool must route its early-stamp through stamp_repair_then_read so the \
             reorder-killer binds production (codex round-5 item 5b) — it did not call the helper"
        )
    });

    // 2. The pinned reads the repair is composed from live INSIDE the helper's closure (textually
    //    AFTER the `stamp_repair_then_read(` call), so they cannot be sampled before the stamp. This
    //    binds the ORDER (codex round-6 K7 / J5 nit): the resident `v3_state.snapshot(slot)` — whose
    //    `cur` becomes the repair's resident base — is the load-bearing one; moving it (or the RPC
    //    reads) BEFORE the helper brands a STALE snapshot with the later stamped generation, and MUST
    //    fail this test (the exact reorder mutant the round-5 test could not kill).
    for read in [
        ".v3_state.snapshot(",
        "fetch_algebra_tick_spacing(",
        "fetch_algebra_global_state(",
    ] {
        let at = body
            .find(read)
            .unwrap_or_else(|| panic!("expected `{read}` in param_sweep_algebra_pool"));
        assert!(
            at > stamp_at,
            "`{read}` must live inside the stamp_repair_then_read closure (AFTER the stamp), not be \
             sampled ahead of it — a pre-stamp read brands stale state with a later generation \
             (codex round-6 K7 / round-5 item 5b)"
        );
    }

    // 3. No INLINE early-stamp: the generation is stamped by the helper, never captured directly on the
    //    pool ahead of the reads (the exact reorder this fix forecloses).
    assert!(
        !body.contains(".generation(pool_id)"),
        "param_sweep_algebra_pool must NOT capture the generation INLINE (`.generation(pool_id)`) — the \
         early-stamp is delegated to stamp_repair_then_read so the reorder-killer binds it (round-5 5b)"
    );
}
