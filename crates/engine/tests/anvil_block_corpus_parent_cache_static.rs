use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use serde_json::Value;

const CORPUS_ROW_KEYS: &[&str] = &["l2Msg", "sequenceNumber", "timestamp"];
const CACHE_TOP_LEVEL_KEYS: &[&str] = &["accounts", "block_hashes", "meta", "storage"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn corpus_dir() -> PathBuf {
    repo_root().join("anvil/corpus")
}

fn block_cache_root() -> PathBuf {
    repo_root().join("anvil/block_cache")
}

fn block_corpus_paths() -> Vec<PathBuf> {
    let mut paths = fs::read_dir(corpus_dir())
        .expect("read anvil corpus directory")
        .map(|entry| entry.expect("read corpus directory entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("blk_") && name.ends_with(".jsonl"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn parse_block_number(path: &Path) -> u64 {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| panic!("missing corpus filename for {}", path.display()));
    name.strip_prefix("blk_")
        .and_then(|name| name.strip_suffix(".jsonl"))
        .unwrap_or_else(|| panic!("unexpected block corpus filename {name}"))
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("invalid block corpus filename {name}: {err}"))
}

fn cache_entry_path(block: u64) -> PathBuf {
    let direct = block_cache_root().join(block.to_string());
    if direct.is_file() {
        return direct;
    }
    direct.join("storage.json")
}

fn sorted_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("expected JSON object: {value}"))
        .keys()
        .cloned()
        .collect()
}

fn expected_keys(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().copied().map(str::to_string).collect()
}

fn first_jsonl_row(path: &Path) -> Value {
    let file = File::open(path).unwrap_or_else(|err| panic!("open {}: {err}", path.display()));
    let mut lines = BufReader::new(file).lines();
    let line = lines
        .find_map(|line| {
            let line = line.unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            (!line.trim().is_empty()).then_some(line)
        })
        .unwrap_or_else(|| panic!("{} has no non-empty rows", path.display()));
    serde_json::from_str(&line)
        .unwrap_or_else(|err| panic!("parse first row {}: {err}", path.display()))
}

fn nonempty_rows(path: &Path) -> usize {
    let file = File::open(path).unwrap_or_else(|err| panic!("open {}: {err}", path.display()));
    BufReader::new(file)
        .lines()
        .filter(|line| {
            !line
                .as_ref()
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
                .trim()
                .is_empty()
        })
        .count()
}

fn parse_cache_entry(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn parse_hex_u64(raw: &str, label: &str) -> u64 {
    u64::from_str_radix(raw.trim_start_matches("0x"), 16)
        .unwrap_or_else(|err| panic!("{label} is not a hex u64 ({raw}): {err}"))
}

#[test]
fn saved_block_corpus_files_have_parent_state_cache_entries() {
    if !corpus_dir().exists() {
        eprintln!(
            "SKIP: artifact {} absent (out-of-branch)",
            corpus_dir().display()
        );
        return;
    }
    let corpus_paths = block_corpus_paths();
    assert!(
        corpus_paths.len() >= 8,
        "too few per-block corpus files: {}",
        corpus_paths.len()
    );

    let mut linked = 0usize;
    let mut total_rows = 0usize;
    let mut total_accounts = 0usize;
    let mut total_storage_entries = 0usize;
    let mut corpus_blocks = BTreeSet::new();
    let mut parent_blocks = BTreeSet::new();

    for path in &corpus_paths {
        let corpus_block = parse_block_number(path);
        let parent_block = corpus_block
            .checked_sub(1)
            .unwrap_or_else(|| panic!("block corpus {} cannot have parent block", path.display()));
        corpus_blocks.insert(corpus_block);
        parent_blocks.insert(parent_block);

        let first_row = first_jsonl_row(path);
        assert_eq!(
            sorted_keys(&first_row),
            expected_keys(CORPUS_ROW_KEYS),
            "per-block corpus {} must remain direct feed rows",
            path.display()
        );
        assert!(
            first_row.get("l2Msg").and_then(Value::as_str).is_some(),
            "{} first row must expose l2Msg",
            path.display()
        );
        assert!(
            first_row
                .get("sequenceNumber")
                .and_then(Value::as_u64)
                .is_some(),
            "{} first row must expose sequenceNumber",
            path.display()
        );
        assert!(
            first_row.get("timestamp").and_then(Value::as_u64).is_some(),
            "{} first row must expose timestamp",
            path.display()
        );

        let rows = nonempty_rows(path);
        assert!(rows > 0, "{} must contain feed rows", path.display());
        total_rows += rows;

        let cache_path = cache_entry_path(parent_block);
        assert!(
            cache_path.is_file(),
            "missing parent cache for corpus block {corpus_block}: expected {}",
            cache_path.display()
        );
        let bytes = fs::metadata(&cache_path)
            .unwrap_or_else(|err| panic!("metadata {}: {err}", cache_path.display()))
            .len();
        assert!(
            bytes >= 100_000,
            "parent cache {parent_block} too small: {bytes}"
        );

        let cache = parse_cache_entry(&cache_path);
        assert_eq!(
            sorted_keys(&cache),
            expected_keys(CACHE_TOP_LEVEL_KEYS),
            "parent cache {parent_block} state schema drifted"
        );
        let accounts = cache
            .get("accounts")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("parent cache {parent_block} missing accounts"));
        let storage = cache
            .get("storage")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("parent cache {parent_block} missing storage"));
        assert!(
            !accounts.is_empty(),
            "parent cache {parent_block} has no accounts"
        );
        assert!(
            !storage.is_empty(),
            "parent cache {parent_block} has no storage"
        );
        total_accounts += accounts.len();
        total_storage_entries += storage.len();

        let block_env_number = cache
            .get("meta")
            .and_then(|meta| meta.get("block_env"))
            .and_then(|block_env| block_env.get("number"))
            .and_then(Value::as_str)
            .map(|raw| parse_hex_u64(raw, "meta.block_env.number"))
            .unwrap_or_else(|| panic!("parent cache {parent_block} missing block_env.number"));
        assert!(
            block_env_number >= 20_000_000,
            "parent cache {parent_block} has implausible block_env.number {block_env_number}"
        );

        linked += 1;
    }

    assert!(
        total_rows >= 200,
        "per-block corpus rows are too sparse: {total_rows}"
    );
    assert!(
        total_accounts >= 500,
        "parent caches have too few account records: {total_accounts}"
    );
    assert!(
        total_storage_entries >= 300,
        "parent caches have too few storage records: {total_storage_entries}"
    );
    assert!(
        corpus_blocks.contains(&465_686_729),
        "Gate9 corpus block disappeared: {corpus_blocks:?}"
    );
    assert!(
        parent_blocks.contains(&465_686_728),
        "Gate9 parent cache disappeared: {parent_blocks:?}"
    );

    assert_eq!(
        linked,
        corpus_paths.len(),
        "not every per-block corpus file links to a parent cache; linked={linked}, corpus_files={}, rows={total_rows}, parents={parent_blocks:?}",
        corpus_paths.len()
    );
}
