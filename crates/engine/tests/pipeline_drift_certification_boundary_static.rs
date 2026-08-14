use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("engine/crates/engine must have repo root ancestor")
        .to_path_buf()
}

fn read_required(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

#[test]
fn pipeline_spot_cache_and_early_exit_drift_are_tracked_as_certification_blocker() {
    let pipeline = read_required("engine/crates/engine/src/pipeline.rs");
    let spot_cache = read_required("engine/crates/engine/src/spot_cache.rs");
    let cargo = read_required("engine/crates/engine/Cargo.toml");

    for required in [
        "let pool_spot = crate::spot_cache::build_prewarmed_cache(&detection.universe);",
        "crate::spot_cache::refresh_pool_spot(&detection.universe, &pool_spot, pid);",
        "crate::spot_cache::cycle_marginal_product_hops(",
        "crate::spot_cache::partial_sort_desc_topk(&mut scored, LANE1_K + SIM_TOPK);",
        "env_flag_default_on(\"SIM_EARLY_EXIT\")",
        "crate::spot_cache::u256_to_f64(x)",
    ] {
        assert!(
            pipeline.contains(required),
            "pipeline drift guard missing expected current marker: {required}"
        );
    }

    // RESOLVED (2026-07-04 recertification): the former docstring/code default mismatch — the
    // outer docstring said "(default OFF)" while `sim_early_exit()` returns
    // `env_flag_default_on("SIM_EARLY_EXIT")` — was FIXED. The docstring now states "default ON"
    // consistently with the code. Assert the resolved, consistent state (a regression guard) rather
    // than the former tracked mismatch.
    assert!(
        pipeline.contains("`SIM_EARLY_EXIT` (**default ON**")
            && pipeline.contains("env_flag_default_on(\"SIM_EARLY_EXIT\")"),
        "SIM_EARLY_EXIT docstring must state default ON, consistent with env_flag_default_on"
    );

    for required in [
        "pub struct PoolSpotCache",
        "pub fn build_prewarmed_cache",
        "pub fn cycle_marginal_product_hops",
        "pub(crate) fn partial_sort_desc_topk",
        "select_nth_unstable_by",
        "cached_marginal_product_matches_reference_across_families_and_order",
        "fold_preserves_first_sentinel_order_dependence",
    ] {
        assert!(
            spot_cache.contains(required),
            "spot_cache drift guard missing expected current marker: {required}"
        );
    }

    assert!(
        cargo.contains("name = \"rank_cache\""),
        "rank_cache bench must remain visible in Cargo.toml while this drift is present"
    );

    assert!(
        pipeline.contains("SIM_EARLY_EXIT") && spot_cache.contains("PoolSpotCache"),
        "pipeline drift certification blocker must be asserted from source, not Markdown tracker text"
    );
}
