//! WI-06 T3 fix round 5 (codex round-5 item 3) — commit→wake catch-up CALL-SITE wiring contract.
//!
//! codex round-5 nit I3: the receipt-boundary wake path (`push_receipt_boundary_wake`) independently
//! recorded a resync obligation + owed wake, sitting OUTSIDE the shared `record_shed_committed_wake`
//! helper — so the "all commit→wake producers centralize" claim was FALSE, and the existing RED tested
//! the helper directly, not call-site WIRING (a new bypassing producer would not trip it).
//!
//! Fix: the resync-obligation + owed-wake pairing for a shed committed pool now lives in ONE primitive,
//! `record_committed_pool_catchup`. Every commit→wake producer whose wake could not be delivered (the
//! scanner/drain shed via `record_shed_committed_wake`, the receipt boundary's full-channel fallback)
//! routes through it. This static contract FAILS (RED) if any producer hand-rolls the
//! `record_stale_resync` obligation OUTSIDE the two allowlisted recorders — i.e. if a new commit→wake
//! producer bypasses the shared primitive.

use std::path::PathBuf;

fn pipeline_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pipeline.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The name of the function lexically ENCLOSING byte offset `pos` — the nearest preceding `fn <name>`.
fn enclosing_fn(src: &str, pos: usize) -> String {
    let head = &src[..pos];
    // Scan for the last `fn <name>(` before `pos` (matches free fns at col 0 and methods alike).
    let mut best: Option<(usize, String)> = None;
    let bytes = head.as_bytes();
    let mut i = 0;
    while let Some(rel) = head[i..].find("fn ") {
        let at = i + rel;
        // Require `fn ` to start a token (preceded by whitespace / start).
        let ok_prefix = at == 0 || bytes[at - 1].is_ascii_whitespace();
        if ok_prefix {
            let after = &head[at + 3..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                best = Some((at, name));
            }
        }
        i = at + 3;
    }
    best.map(|(_, n)| n).unwrap_or_else(|| "<none>".to_string())
}

/// Slice a function's body text: from `fn <name>(` up to the next top-of-line `fn ` (bounds a free
/// function well enough for a `contains` check).
fn fn_slice<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("`fn {name}(` not found in pipeline.rs"));
    let rest = &src[start..];
    let end = rest[3..].find("\nfn ").map(|e| e + 3).unwrap_or(rest.len());
    &rest[..end]
}

/// The resync-obligation recorder for a shed committed-pool wake (`record_stale_resync`) may be called
/// ONLY inside these two allowlisted helpers — no commit→wake producer may hand-roll it.
const RESYNC_OBLIGATION_ALLOWLIST: &[&str] = &[
    // The ONE shared per-committed-pool catch-up primitive (resync obligation + owed wake).
    "record_committed_pool_catchup",
    // The targeted-resync CHANNEL fallback (a full targeted_q re-records the obligation) — a distinct
    // resync re-enqueue path, not a commit→wake shed. It is used by the DRAIN's serviced-obligation
    // re-enqueue (`service_reconciliation_obligations` results, pipeline.rs). The receipt success path
    // does NOT use it: it pushes `targeted_q` and owes the wake directly (`targeted_q.push` +
    // `record_owed_wake`), only routing through `record_committed_pool_catchup` on a FULL channel.
    "enqueue_or_record_resync",
];

#[test]
fn commit_wake_catchup_is_centralized() {
    let src = pipeline_src();

    // 1. The shared primitive exists and records BOTH halves of the catch-up.
    let catchup = fn_slice(&src, "record_committed_pool_catchup");
    assert!(
        catchup.contains("record_stale_resync(") && catchup.contains("record_owed_wake("),
        "record_committed_pool_catchup must record BOTH a resync obligation AND an owed wake"
    );

    // 2. Every `record_stale_resync(` call site lives inside an allowlisted recorder — no commit→wake
    //    producer (scanner / drain / receipt) hand-rolls the resync obligation outside the shared
    //    primitive. (RED if a new producer bypasses it.)
    let needle = "record_stale_resync(";
    let mut sites = 0;
    let mut from = 0;
    while let Some(rel) = src[from..].find(needle) {
        let at = from + rel;
        from = at + needle.len();
        // Only count actual method-CALL sites (`<recv>.record_stale_resync(`); ignore any prose /
        // identifier mentions (a doc comment referencing the name is not a call).
        if !src[..at].ends_with('.') {
            continue;
        }
        sites += 1;
        let owner = enclosing_fn(&src, at);
        assert!(
            RESYNC_OBLIGATION_ALLOWLIST.contains(&owner.as_str()),
            "record_stale_resync call site (byte {at}) is in `{owner}`, which is NOT an allowlisted \
             recorder {RESYNC_OBLIGATION_ALLOWLIST:?} — a commit→wake producer must route its shed \
             disposition through the shared `record_committed_pool_catchup` primitive (codex round-5 \
             item 3), never hand-roll the resync obligation."
        );
    }
    assert!(sites >= 2, "expected the two allowlisted record_stale_resync call sites, found {sites}");

    // 3. Both shed producers route through the shared primitive (the receipt path can no longer drift).
    for producer in ["record_shed_committed_wake", "push_receipt_boundary_wake"] {
        let body = fn_slice(&src, producer);
        assert!(
            body.contains("record_committed_pool_catchup("),
            "{producer} must route its shed disposition through the shared \
             record_committed_pool_catchup primitive (codex round-5 item 3)"
        );
    }
}
