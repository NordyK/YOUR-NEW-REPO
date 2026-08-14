//! [S5 · WP3 · Fix D] Source guard: a candidate's fetch-error VERDICT reads ONLY the validator-local
//! per-instance counter bundle, never the process-global `rpc_fetch_error_count()`. A static
//! complement to the runtime deterministic-race test (`validator_local_error_delta_unaffected_by_
//! concurrent_advancer_error` in revm_fork) — it cannot prove behavior, but it prevents a future edit
//! from silently re-introducing the cross-thread contamination (an unrelated thread's RPC error
//! spuriously fail-closing an in-flight candidate).

use std::{fs, path::Path};

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn candidate_verdict_never_reads_the_global_error_count() {
    let src = read("src/pipeline.rs");
    // The process-global error reader must not appear anywhere in the validator pipeline — the
    // per-candidate fail-closed verdict is validator-local only.
    assert!(
        !src.contains("rpc_fetch_error_count("),
        "pipeline candidate-verdict code must not read the process-global rpc_fetch_error_count() — \
         use the validator-local RpcFetchCounters bundle"
    );
    // [WP3 · FIX-F1] Also forbid a DIRECT read/write of the process-global error static (`.load(`/
    // `.fetch...`) in the pipeline — not just the reader function. The candidate verdict must be
    // validator-local only. (A plain-text mention in a comment is fine; a `.`-access is a real read.)
    assert!(
        !src.contains("RPC_FETCH_ERRORS."),
        "pipeline must not read/write the process-global RPC_FETCH_ERRORS static — the candidate \
         verdict reads only the validator-local RpcFetchCounters bundle"
    );
    // The fetch-error verdict is computed from the validator-local counter bundle.
    assert!(
        src.contains("let fetch_errors = val_counters"),
        "the validator's fetch_errors must be a delta of the validator-local counter bundle"
    );
}

#[test]
fn error_pre_snapshot_precedes_every_candidate_read() {
    // WP3 RED-6 + FIX-F1: the validator-local ERROR pre-snapshot (`err_pre`) must be captured BEFORE
    // EVERY candidate-specific read — the account PRELOAD/prewarm (preload_learned_then_prefetch /
    // warm_call_prefetch) AND the overlay `exec.overlay_storage` writes — so a cold fetch / RPC error
    // during preload OR overlay setup is attributed to THIS candidate, never missed. (The prior guard
    // only checked before the overlay, staying green while preload errors escaped — a false assurance.)
    let src = read("src/pipeline.rs");
    // Scope to the validation section (the calldata build precedes the preload).
    let section = src
        .find("let calldata_len = calldata.len() as u32;")
        .expect("validation section anchor not found");
    let err_pre_idx = src[section..]
        .find("let err_pre = val_counters")
        .map(|i| section + i)
        .expect("validator-local err_pre snapshot not found in the validation section");
    // The FIRST candidate-specific read after the section anchor: preload, prewarm, or overlay.
    let first_read = ["preload_learned_then_prefetch(", "warm_call_prefetch(", ".prewarm(", ".overlay_storage("]
        .iter()
        .filter_map(|needle| src[section..].find(needle).map(|i| section + i))
        .min()
        .expect("no candidate read (preload/prewarm/overlay) found");
    assert!(
        err_pre_idx < first_read,
        "the validator-local err_pre snapshot must precede EVERY candidate read (preload/prewarm/\
         overlay) so their errors are attributed to this candidate — found err_pre at {err_pre_idx} \
         but the first candidate read at {first_read}"
    );
}
