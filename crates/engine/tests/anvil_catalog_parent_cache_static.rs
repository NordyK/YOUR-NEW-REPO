use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const ANVIL_ROOT: &str = "../../../anvil";
const CATALOG: &str = "../../../anvil/anvil_catalog.csv";
const CACHE_TOP_LEVEL_KEYS: &[&str] = &["accounts", "block_hashes", "meta", "storage"];

#[derive(Debug)]
struct CatalogRow {
    block: u64,
    tx: String,
    hops: u8,
    families: Vec<String>,
}

fn read_required(path: impl AsRef<Path>, label: &str) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {label} at {}: {err}", path.display()))
}

fn parse_catalog() -> Vec<CatalogRow> {
    read_required(CATALOG, "anvil catalog")
        .lines()
        .enumerate()
        .filter_map(|(idx, raw_line)| {
            let line = raw_line.trim_end_matches('\r');
            if idx == 0 || line.trim().is_empty() {
                return None;
            }

            let line_no = idx + 1;
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                8,
                "catalog line {line_no} must have 8 comma-separated fields"
            );
            let block = fields[0]
                .parse::<u64>()
                .unwrap_or_else(|err| panic!("catalog line {line_no} invalid block: {err}"));
            let tx = fields[1].to_ascii_lowercase();
            assert!(
                tx.starts_with("0x")
                    && tx.len() == 66
                    && tx[2..].chars().all(|ch| ch.is_ascii_hexdigit()),
                "catalog line {line_no} invalid tx hash {tx}"
            );
            let hops = fields[2]
                .parse::<u8>()
                .unwrap_or_else(|err| panic!("catalog line {line_no} invalid hops: {err}"));
            let families = fields[3]
                .split('+')
                .filter(|family| !family.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();

            Some(CatalogRow {
                block,
                tx,
                hops,
                families,
            })
        })
        .collect()
}

fn block_cache_root() -> PathBuf {
    PathBuf::from(ANVIL_ROOT).join("block_cache")
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
        .unwrap_or_else(|| panic!("expected cache JSON object: {value}"))
        .keys()
        .cloned()
        .collect()
}

fn expected_keys(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().copied().map(str::to_string).collect()
}

fn parse_cache_entry(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn parse_hex_u64(raw: &str, label: &str) -> u64 {
    u64::from_str_radix(raw.trim_start_matches("0x"), 16)
        .unwrap_or_else(|err| panic!("{label} is not a hex u64 ({raw}): {err}"))
}

fn assert_cache_state_shape(parent_block: u64) {
    let path = cache_entry_path(parent_block);
    assert!(
        path.is_file(),
        "sample parent cache {parent_block} missing at {}",
        path.display()
    );
    let cache = parse_cache_entry(&path);
    assert_eq!(
        sorted_keys(&cache),
        expected_keys(CACHE_TOP_LEVEL_KEYS),
        "sample parent cache {parent_block} top-level schema drifted"
    );

    let accounts = cache
        .get("accounts")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("sample parent cache {parent_block} missing accounts"));
    let storage = cache
        .get("storage")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("sample parent cache {parent_block} missing storage"));
    assert!(
        !accounts.is_empty(),
        "sample parent cache {parent_block} has no accounts"
    );
    assert!(
        !storage.is_empty(),
        "sample parent cache {parent_block} has no storage"
    );

    let block_env_number = cache
        .get("meta")
        .and_then(|meta| meta.get("block_env"))
        .and_then(|block_env| block_env.get("number"))
        .and_then(Value::as_str)
        .map(|raw| parse_hex_u64(raw, "meta.block_env.number"))
        .unwrap_or_else(|| panic!("sample parent cache {parent_block} missing block_env.number"));
    assert!(
        block_env_number >= 20_000_000,
        "sample parent cache {parent_block} has implausible block_env.number {block_env_number}"
    );
}

#[test]
fn anvil_catalog_rows_have_parent_block_cache_state_material() {
    if !Path::new(ANVIL_ROOT).exists() {
        eprintln!("SKIP: artifact {ANVIL_ROOT} absent (out-of-branch)");
        return;
    }
    let rows = parse_catalog();
    assert!(
        rows.len() >= 1_200,
        "anvil catalog lost breadth: {} rows",
        rows.len()
    );

    let mut unique_txs = BTreeSet::new();
    let mut unique_blocks = BTreeSet::new();
    let mut unique_parent_blocks = BTreeSet::new();
    let mut hop_counts = BTreeMap::<u8, usize>::new();
    let mut family_counts = BTreeMap::<String, usize>::new();
    let mut parent_hits = 0usize;
    let mut min_parent_bytes = u64::MAX;
    let mut max_parent_bytes = 0u64;

    for row in &rows {
        assert!(
            row.block > 400_000_000,
            "unexpected catalog block {}",
            row.block
        );
        unique_txs.insert(row.tx.clone());
        unique_blocks.insert(row.block);
        *hop_counts.entry(row.hops).or_insert(0) += 1;
        for family in &row.families {
            *family_counts.entry(family.clone()).or_insert(0) += 1;
        }

        let parent_block = row
            .block
            .checked_sub(1)
            .unwrap_or_else(|| panic!("catalog block {} cannot have parent", row.block));
        unique_parent_blocks.insert(parent_block);
        let cache_path = cache_entry_path(parent_block);
        assert!(
            cache_path.is_file(),
            "missing parent cache for catalog block {} at {}",
            row.block,
            cache_path.display()
        );
        let bytes = fs::metadata(&cache_path)
            .unwrap_or_else(|err| panic!("metadata {}: {err}", cache_path.display()))
            .len();
        assert!(
            bytes > 0,
            "parent cache {parent_block} for catalog block {} is empty",
            row.block
        );
        min_parent_bytes = min_parent_bytes.min(bytes);
        max_parent_bytes = max_parent_bytes.max(bytes);
        parent_hits += 1;
    }

    assert!(
        unique_txs.len() >= 1_000,
        "catalog transaction diversity too low: {}",
        unique_txs.len()
    );
    assert!(
        unique_blocks.len() >= 1_250,
        "catalog block diversity too low: {}",
        unique_blocks.len()
    );
    assert!(
        unique_parent_blocks.len() >= 1_250,
        "parent cache block diversity too low: {}",
        unique_parent_blocks.len()
    );
    assert!(
        hop_counts.get(&2).copied().unwrap_or_default() >= 500,
        "catalog lacks 2-hop rows: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&3).copied().unwrap_or_default() >= 400,
        "catalog lacks 3-hop rows: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&4).copied().unwrap_or_default() >= 100,
        "catalog lacks 4-hop rows: {hop_counts:?}"
    );

    for family in ["uniswap", "pancakeswap", "camelot", "dodo", "balancer"] {
        assert!(
            family_counts.get(family).copied().unwrap_or_default() > 0,
            "catalog lacks required family {family}: {family_counts:?}"
        );
    }

    assert!(
        min_parent_bytes >= 100,
        "smallest parent cache is unexpectedly tiny: {min_parent_bytes}"
    );
    assert!(
        max_parent_bytes >= 5_000_000,
        "largest parent cache is unexpectedly small: {max_parent_bytes}"
    );

    for sample_index in [0, rows.len() / 2, rows.len() - 1] {
        let parent_block = rows[sample_index].block - 1;
        assert_cache_state_shape(parent_block);
    }

    assert_eq!(
        parent_hits,
        rows.len(),
        "not every catalog row links to a parent cache entry; parent_hits={parent_hits}, rows={}, unique_parent_blocks={}, min_parent_bytes={min_parent_bytes}, max_parent_bytes={max_parent_bytes}",
        rows.len(),
        unique_parent_blocks.len()
    );
}
