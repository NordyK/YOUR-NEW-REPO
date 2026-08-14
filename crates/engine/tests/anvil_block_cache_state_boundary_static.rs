use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const REQUIRED_CACHE_BLOCKS: &[u64] = &[
    465_686_728,
    471_733_538,
    464_734_242,
    469_953_962,
    473_144_130,
    460_179_231,
    467_233_189,
];

const STATE_TOP_LEVEL_KEYS: &[&str] = &["accounts", "block_hashes", "meta", "storage"];

const REPLAY_MANIFEST_KEYS: &[&str] = &[
    "balance_delta_wei",
    "candidate_id",
    "estimated_gas_wei",
    "executor_calldata",
    "gross_profit_wei",
    "log_index",
    "mantis_calldata",
    "min_profit",
    "net_profit_wei",
    "raw_calldata_bytes",
    "reject_reason",
    "revm_result",
    "route_id",
    "state_point",
    "transaction_index",
    "trigger_seq",
    "tx_hash",
];

#[derive(Debug)]
struct CacheSample {
    block: u64,
    min_bytes: u64,
    min_accounts: usize,
    min_storage_entries: usize,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn block_cache_root() -> PathBuf {
    repo_root().join("anvil/block_cache")
}

fn cache_entry_path(block: u64) -> PathBuf {
    let entry = block_cache_root().join(block.to_string());
    if entry.is_file() {
        return entry;
    }
    entry.join("storage.json")
}

fn relative_path(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn parse_cache_entry(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn sorted_object_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("cache sample must be a JSON object: {value}"))
        .keys()
        .cloned()
        .collect()
}

fn expected_top_level_keys() -> BTreeSet<String> {
    STATE_TOP_LEVEL_KEYS
        .iter()
        .copied()
        .map(str::to_string)
        .collect()
}

fn parse_hex_u64(raw: &str, label: &str) -> u64 {
    u64::from_str_radix(raw.trim_start_matches("0x"), 16)
        .unwrap_or_else(|err| panic!("{label} is not a hex u64 ({raw}): {err}"))
}

fn push_limited(out: &mut Vec<String>, value: String) {
    if out.len() < 30 {
        out.push(value);
    }
}

fn collect_replay_manifest_key_hits(path: &str, value: &Value, hits: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let child_path = format!("{path}.{key}");
                if REPLAY_MANIFEST_KEYS.contains(&key.as_str()) {
                    push_limited(hits, child_path.clone());
                }
                collect_replay_manifest_key_hits(&child_path, nested, hits);
            }
        }
        Value::Array(items) => {
            for (idx, nested) in items.iter().enumerate() {
                collect_replay_manifest_key_hits(&format!("{path}[{idx}]"), nested, hits);
            }
        }
        _ => {}
    }
}

#[test]
fn anvil_block_cache_is_state_material_not_standalone_replay_manifest() {
    let root = block_cache_root();
    if !root.exists() {
        eprintln!("SKIP: artifact {} absent (out-of-branch)", root.display());
        return;
    }
    assert!(root.is_dir(), "missing block cache root {}", root.display());

    let mut file_entries = 0usize;
    let mut dir_entries = 0usize;
    let mut nonzero_entries = 0usize;
    for entry in fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("read block cache root {}: {err}", root.display()))
    {
        let path = entry.expect("block cache dir entry").path();
        let bytes = if path.is_file() {
            file_entries += 1;
            fs::metadata(&path)
                .unwrap_or_else(|err| panic!("metadata {}: {err}", path.display()))
                .len()
        } else if path.is_dir() {
            dir_entries += 1;
            let storage = path.join("storage.json");
            fs::metadata(&storage)
                .unwrap_or_else(|err| panic!("metadata {}: {err}", storage.display()))
                .len()
        } else {
            panic!("unexpected block cache entry type {}", path.display());
        };
        if bytes > 0 {
            nonzero_entries += 1;
        }
    }

    assert!(
        file_entries >= 1_700,
        "block cache lost file-shaped entries: {file_entries}"
    );
    assert!(
        dir_entries >= 100,
        "block cache lost directory-shaped entries: {dir_entries}"
    );
    assert!(
        nonzero_entries >= 1_800,
        "block cache has too few non-empty entries: {nonzero_entries}"
    );

    for block in REQUIRED_CACHE_BLOCKS {
        let path = cache_entry_path(*block);
        assert!(
            path.is_file(),
            "required cache block {block} missing readable state file at {}",
            path.display()
        );
    }

    let mut replay_manifest_key_hits = Vec::new();
    for sample in [
        CacheSample {
            block: 465_686_728,
            min_bytes: 4_000_000,
            min_accounts: 150,
            min_storage_entries: 50,
        },
        CacheSample {
            block: 464_734_242,
            min_bytes: 1_000_000,
            min_accounts: 80,
            min_storage_entries: 20,
        },
        CacheSample {
            block: 467_233_189,
            min_bytes: 50_000_000,
            min_accounts: 2_000,
            min_storage_entries: 500,
        },
        CacheSample {
            block: 460_179_231,
            min_bytes: 20_000_000,
            min_accounts: 1_000,
            min_storage_entries: 250,
        },
    ] {
        let path = cache_entry_path(sample.block);
        let bytes = fs::metadata(&path)
            .unwrap_or_else(|err| panic!("metadata {}: {err}", path.display()))
            .len();
        assert!(
            bytes >= sample.min_bytes,
            "cache block {} too small for sampled state material: {bytes} < {}",
            sample.block,
            sample.min_bytes
        );

        let value = parse_cache_entry(&path);
        assert_eq!(
            sorted_object_keys(&value),
            expected_top_level_keys(),
            "cache block {} top-level state schema drifted",
            sample.block
        );

        let accounts = value
            .get("accounts")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("cache block {} missing accounts object", sample.block));
        assert!(
            accounts.len() >= sample.min_accounts,
            "cache block {} has too few accounts: {} < {}",
            sample.block,
            accounts.len(),
            sample.min_accounts
        );

        let storage = value
            .get("storage")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("cache block {} missing storage object", sample.block));
        assert!(
            storage.len() >= sample.min_storage_entries,
            "cache block {} has too few storage entries: {} < {}",
            sample.block,
            storage.len(),
            sample.min_storage_entries
        );

        let block_env = value
            .get("meta")
            .and_then(|meta| meta.get("block_env"))
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("cache block {} missing meta.block_env", sample.block));
        let block_number = block_env
            .get("number")
            .and_then(Value::as_str)
            .map(|raw| parse_hex_u64(raw, "meta.block_env.number"))
            .unwrap_or_else(|| panic!("cache block {} missing block_env.number", sample.block));
        assert!(
            block_number >= 20_000_000,
            "cache block {} has implausibly low Arbitrum block_env.number {block_number}",
            sample.block
        );
        assert!(
            block_env.get("timestamp").and_then(Value::as_str).is_some(),
            "cache block {} missing block_env.timestamp",
            sample.block
        );

        collect_replay_manifest_key_hits(
            &relative_path(&path),
            &value,
            &mut replay_manifest_key_hits,
        );
    }

    assert!(
        replay_manifest_key_hits.is_empty(),
        "block_cache now contains replay/profit manifest keys: {replay_manifest_key_hits:?}. Reclassify the laptop proof boundary before treating it as only cached state material."
    );
}
