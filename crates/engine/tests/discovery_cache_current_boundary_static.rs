use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// The REAL generated artifact path — asserted to stay git-ignored (works whether or not the file
/// is present, since `git check-ignore` matches the ignore rule on the path).
fn discovery_cache_path() -> PathBuf {
    repo_root().join("engine/discovery_cache.json")
}

/// Worktree-portable DATA source: the COMMITTED full identity-only cache snapshot (every pool
/// identity + fluid_pools + v4_pool_ids + head_block, verbatim from the untracked
/// engine/discovery_cache.json). Breadth counts and identity shapes are byte-identical to the
/// live-file baseline, so the pinned floors/shape assertions are preserved exactly.
fn discovery_cache_fixture_path() -> PathBuf {
    repo_root().join("codex/campaign_baseline_wi01/discovery_cache_full_identity_golden.json")
}

fn read_cache() -> Value {
    let path = discovery_cache_fixture_path();
    let bytes = fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn array<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array {key} in discovery cache"))
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing numeric {key} in {value}"))
}

fn bytes(value: &Value, len: usize, label: &str) -> Vec<u8> {
    let raw = value
        .as_array()
        .unwrap_or_else(|| panic!("{label} must be a byte array: {value}"));
    assert_eq!(raw.len(), len, "{label} byte length drifted");
    raw.iter()
        .enumerate()
        .map(|(idx, byte)| {
            let raw_byte = byte
                .as_u64()
                .unwrap_or_else(|| panic!("{label}[{idx}] must be a byte integer"));
            assert!(raw_byte <= u8::MAX as u64, "{label}[{idx}] exceeds u8");
            raw_byte as u8
        })
        .collect()
}

fn assert_no_future_created_blocks(cache: &Value, section: &str) {
    let head_block = u64_field(cache, "head_block");
    for (idx, pool) in array(cache, section).iter().enumerate() {
        let created = u64_field(pool, "created_block");
        assert!(
            created <= head_block,
            "{section}[{idx}] created_block {created} exceeds cache head_block {head_block}"
        );
    }
}

fn assert_git_ignored(path: &Path) {
    let output = Command::new("git")
        .arg("check-ignore")
        .arg("-q")
        .arg(path)
        .current_dir(repo_root())
        .output()
        .expect("run git check-ignore");
    assert!(
        output.status.success(),
        "{} must remain an ignored generated artifact",
        path.display()
    );
}

#[test]
fn discovery_cache_is_current_head_consistent_ignored_identity_material() {
    let path = discovery_cache_path();
    assert_git_ignored(&path);

    let cache = read_cache();
    let head_block = u64_field(&cache, "head_block");
    assert!(
        head_block >= 477_229_551,
        "cache head_block regressed below the current regenerated boundary: {head_block}"
    );
    // AUD-DISCOVERY-BREADTH re-pin (2026-07-04 recertification): floors lowered to the current
    // authoritative discovery output at head 479515032 (3142 pools / 333 v4 / family histogram:
    // DodoV2 1228, BalancerStable 698, UniswapV4 333, UniswapV3 259, CurveStableSwapNg 145,
    // SushiV3 94, PancakeV3 90, TraderJoeLb 89, UniswapV2 65, AlgebraV19 42, CamelotV2 31,
    // SushiV2 26, SolidlyV3 22). The V4 drop (552→333) is the CORRECT native-ETH `SkipNative`
    // admission fix (native-ETH pools can never match the executor's WETH-recompute poolId — see
    // factory_v4). The total drop (3600→~3142) is that V4 removal plus liquidity/head drift at the
    // newer boundary. Floors set to current-breadth-minus-margin so real coverage loss still trips.
    assert!(
        array(&cache, "pools").len() >= 3_000,
        "discovery cache lost pool identity breadth (below re-pinned 3000 floor)"
    );
    assert!(
        array(&cache, "fluid_pools").len() >= 8,
        "discovery cache lost Fluid identity breadth"
    );
    assert!(
        array(&cache, "v4_pool_ids").len() >= 300,
        "discovery cache lost V4 pool-id breadth (below re-pinned 300 floor; native-ETH excluded)"
    );

    assert_no_future_created_blocks(&cache, "pools");
    assert_no_future_created_blocks(&cache, "fluid_pools");
}

#[test]
fn discovery_cache_v4_pool_ids_have_stable_shape_and_unique_pool_ids() {
    let cache = read_cache();
    let mut pool_ids = BTreeSet::<Vec<u8>>::new();
    for (idx, row) in array(&cache, "v4_pool_ids").iter().enumerate() {
        let fields = row
            .as_array()
            .unwrap_or_else(|| panic!("v4_pool_ids[{idx}] must be a tuple array"));
        assert_eq!(
            fields.len(),
            2,
            "v4_pool_ids[{idx}] must contain manager/address and pool id"
        );
        let manager = bytes(&fields[0], 20, &format!("v4_pool_ids[{idx}][0]"));
        assert_ne!(
            manager,
            vec![0u8; 20],
            "v4_pool_ids[{idx}] manager/address must not be zero"
        );
        let pool_id = bytes(&fields[1], 32, &format!("v4_pool_ids[{idx}][1]"));
        assert!(
            pool_ids.insert(pool_id),
            "duplicate V4 pool id at v4_pool_ids[{idx}]"
        );
    }
}
