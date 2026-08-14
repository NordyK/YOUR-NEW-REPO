use serde_json::Value;
use std::fs;

const MATRIX: &str = "../../../Codex/HISTORICAL_CORPUS_COVERAGE_MATRIX_2026-06-15.json";
const BLOCK_CACHE: &str = "../../../anvil/block_cache";

#[derive(Default)]
struct BlockCacheStats {
    effective_entries: u64,
    effective_nonzero_entries: u64,
    effective_total_bytes: u64,
    top_level_file_entries: u64,
    top_level_file_nonzero_entries: u64,
    top_level_file_ge_1mb: u64,
    top_level_file_ge_20mb: u64,
    storage_json_entries: u64,
    storage_json_nonzero_entries: u64,
    storage_json_ge_1mb: u64,
    storage_json_ge_20mb: u64,
}

fn read_json(path: &str) -> Value {
    let raw = fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parse {path}: {err}"))
}

fn matrix_u64(matrix: &Value, key: &str) -> u64 {
    matrix["block_cache"][key]
        .as_u64()
        .unwrap_or_else(|| panic!("missing block_cache.{key} in matrix"))
}

fn block_cache_stats() -> BlockCacheStats {
    let mut out = BlockCacheStats::default();
    for entry in fs::read_dir(BLOCK_CACHE).expect("read block_cache") {
        let entry = entry.expect("block_cache entry");
        let path = entry.path();
        out.effective_entries += 1;
        let metadata = fs::metadata(&path).expect("block_cache metadata");
        let bytes = if metadata.is_file() {
            out.top_level_file_entries += 1;
            let bytes = metadata.len();
            if bytes > 0 {
                out.top_level_file_nonzero_entries += 1;
            }
            if bytes >= 1_000_000 {
                out.top_level_file_ge_1mb += 1;
            }
            if bytes >= 20_000_000 {
                out.top_level_file_ge_20mb += 1;
            }
            bytes
        } else {
            let storage = path.join("storage.json");
            let bytes = fs::metadata(&storage)
                .unwrap_or_else(|err| panic!("storage metadata {}: {err}", storage.display()))
                .len();
            out.storage_json_entries += 1;
            if bytes > 0 {
                out.storage_json_nonzero_entries += 1;
            }
            if bytes >= 1_000_000 {
                out.storage_json_ge_1mb += 1;
            }
            if bytes >= 20_000_000 {
                out.storage_json_ge_20mb += 1;
            }
            bytes
        };
        if bytes > 0 {
            out.effective_nonzero_entries += 1;
        }
        out.effective_total_bytes += bytes;
    }
    out
}

#[test]
fn current_block_cache_matches_refreshed_historical_matrix() {
    if !std::path::Path::new(BLOCK_CACHE).exists() {
        eprintln!("SKIP: artifact {BLOCK_CACHE} absent (out-of-branch)");
        return;
    }
    let matrix = read_json(MATRIX);
    let current = block_cache_stats();

    assert_eq!(matrix_u64(&matrix, "effective_entries"), 2132);
    // MINIMUM baseline, not a frozen snapshot. The cache must CONTAIN at least the certified state
    // so offline replay is reproducible; extra real-Arbitrum blocks cached by the fork-test suite
    // (which is what PROVES the engine) are harmless and must not turn this red. Assert >= certified,
    // not == yesterday's snapshot — the latter is perpetually red on any machine actually testing,
    // while a SHRUNK/missing cache (the only failure that breaks replay) is still caught by >=.
    assert!(
        current.effective_entries >= matrix_u64(&matrix, "effective_entries"),
        "block cache missing certified entries: {} < {}",
        current.effective_entries,
        matrix_u64(&matrix, "effective_entries")
    );
    assert!(
        current.top_level_file_entries >= matrix_u64(&matrix, "top_level_file_entries"),
        "block cache missing certified top-level entries"
    );
    assert!(
        current.effective_nonzero_entries >= matrix_u64(&matrix, "effective_nonzero_entries"),
        "block cache missing certified non-empty entries"
    );
    assert!(
        current.top_level_file_nonzero_entries
            >= matrix_u64(&matrix, "top_level_file_nonzero_entries"),
        "block cache missing certified non-empty top-level entries"
    );
    assert!(
        current.effective_total_bytes >= matrix_u64(&matrix, "effective_total_bytes"),
        "block cache smaller than certified baseline: {} < {}",
        current.effective_total_bytes,
        matrix_u64(&matrix, "effective_total_bytes")
    );

    assert!(
        current.storage_json_entries >= matrix_u64(&matrix, "storage_json_entries"),
        "directory-backed storage cache below certified baseline"
    );
    assert!(
        current.storage_json_nonzero_entries >= matrix_u64(&matrix, "storage_json_nonzero_entries"),
        "directory-backed nonzero storage cache below certified baseline"
    );
    assert!(
        current.top_level_file_ge_1mb >= matrix_u64(&matrix, "top_level_file_ge_1mb"),
        "large top-level cache below certified baseline"
    );
    assert!(
        current.top_level_file_ge_20mb >= matrix_u64(&matrix, "top_level_file_ge_20mb"),
        "very large top-level cache below certified baseline"
    );
    assert!(
        current.storage_json_ge_1mb >= matrix_u64(&matrix, "storage_json_ge_1mb"),
        "large storage-json cache below certified baseline"
    );
    assert!(
        current.storage_json_ge_20mb >= matrix_u64(&matrix, "storage_json_ge_20mb"),
        "very large storage-json cache below certified baseline"
    );
}
