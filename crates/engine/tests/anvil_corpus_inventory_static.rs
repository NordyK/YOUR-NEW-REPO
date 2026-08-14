use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const ANVIL_ROOT: &str = "../../../anvil";
const CATALOG: &str = "../../../anvil/anvil_catalog.csv";

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

fn nonempty_line_count(path: impl AsRef<Path>, label: &str) -> usize {
    read_required(path, label)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn parse_catalog() -> Vec<CatalogRow> {
    let raw = read_required(CATALOG, "anvil catalog");
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(idx, line)| {
            if idx == 0 {
                return None;
            }
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                8,
                "catalog line {} must have 8 comma-separated fields",
                idx + 1
            );
            let block = fields[0]
                .parse::<u64>()
                .unwrap_or_else(|err| panic!("catalog line {} invalid block: {err}", idx + 1));
            let tx = fields[1].to_string();
            assert!(
                tx.starts_with("0x") && tx.len() == 66,
                "catalog line {} invalid tx hash {tx}",
                idx + 1
            );
            let hops = fields[2]
                .parse::<u8>()
                .unwrap_or_else(|err| panic!("catalog line {} invalid hops: {err}", idx + 1));
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

fn block_cache_entry_size(block: u64) -> Option<u64> {
    let entry = PathBuf::from(ANVIL_ROOT)
        .join("block_cache")
        .join(block.to_string());
    let metadata = fs::metadata(&entry).ok()?;
    if metadata.is_file() {
        return Some(metadata.len());
    }
    if metadata.is_dir() {
        return fs::metadata(entry.join("storage.json"))
            .ok()
            .map(|m| m.len());
    }
    None
}

fn block_cache_counts() -> (usize, usize) {
    let dir = PathBuf::from(ANVIL_ROOT).join("block_cache");
    let mut entries = 0usize;
    let mut nonzero = 0usize;
    for entry in fs::read_dir(&dir).unwrap_or_else(|err| {
        panic!(
            "failed to read block cache directory {}: {err}",
            dir.display()
        )
    }) {
        let entry = entry.expect("block cache directory entry");
        entries += 1;
        let path = entry.path();
        let bytes = if path.is_file() {
            fs::metadata(&path)
                .expect("block cache file metadata")
                .len()
        } else {
            fs::metadata(path.join("storage.json"))
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        };
        if bytes > 0 {
            nonzero += 1;
        }
    }
    (entries, nonzero)
}

#[test]
fn saved_anvil_corpus_is_present_and_large_enough_for_offline_certification_work() {
    if !Path::new(ANVIL_ROOT).exists() {
        eprintln!("SKIP: artifact {ANVIL_ROOT} absent (out-of-branch)");
        return;
    }
    assert!(
        Path::new(ANVIL_ROOT).is_dir(),
        "root anvil directory is missing"
    );

    let rows = parse_catalog();
    assert!(
        rows.len() >= 1_000,
        "anvil catalog is too small for deep replay certification: {} rows",
        rows.len()
    );

    let mut hop_counts = BTreeMap::<u8, usize>::new();
    let mut family_counts = BTreeMap::<String, usize>::new();
    let mut unique_txs = BTreeSet::<String>::new();
    let mut min_block = u64::MAX;
    let mut max_block = 0u64;
    for row in &rows {
        *hop_counts.entry(row.hops).or_insert(0) += 1;
        unique_txs.insert(row.tx.clone());
        min_block = min_block.min(row.block);
        max_block = max_block.max(row.block);
        for family in &row.families {
            *family_counts.entry(family.clone()).or_insert(0) += 1;
        }
    }

    assert!(min_block >= 400_000_000, "unexpected low block {min_block}");
    assert!(max_block > min_block, "catalog block range is empty");
    assert!(
        unique_txs.len() >= 1_000,
        "catalog has too little transaction diversity: {} unique txs",
        unique_txs.len()
    );
    assert!(
        hop_counts.get(&2).copied().unwrap_or_default() >= 500,
        "catalog lacks enough 2-hop seeds: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&3).copied().unwrap_or_default() >= 400,
        "catalog lacks enough 3-hop seeds: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&4).copied().unwrap_or_default() >= 100,
        "catalog lacks enough 4-hop seeds: {hop_counts:?}"
    );

    for required_family in [
        "uniswap",
        "pancakeswap",
        "camelot",
        "sushiswap",
        "fluid",
        "dodo",
        "balancer",
        "trader_joe",
        "maverick",
        "ramses",
        "solidly",
        "kyberswap",
        "gammaswap",
    ] {
        assert!(
            family_counts
                .get(required_family)
                .copied()
                .unwrap_or_default()
                > 0,
            "catalog lacks required family {required_family}; families={family_counts:?}"
        );
    }

    let (cache_entries, nonzero_cache_entries) = block_cache_counts();
    assert!(
        cache_entries >= 1_600,
        "block cache has too few entries: {cache_entries}"
    );
    assert!(
        nonzero_cache_entries >= 1_500,
        "block cache has too few non-empty entries: {nonzero_cache_entries}/{cache_entries}"
    );

    for (block, min_bytes, label) in [
        (465_686_728, 1_000_000, "flagship 2-hop profit block"),
        (471_733_538, 1_000_000, "detected free-run replay block"),
        (464_734_242, 1_000_000, "route685 parent block cache"),
        (469_953_962, 1_000_000, "replacement-set parent block cache"),
        (473_144_130, 1_000_000, "latest observed large block cache"),
        (460_179_231, 20_000_000, "DODO offline state cache"),
        (467_233_189, 20_000_000, "4-hop executor state cache"),
    ] {
        let bytes = block_cache_entry_size(block).unwrap_or(0);
        assert!(
            bytes >= min_bytes,
            "{label} block {block} has insufficient cached bytes: {bytes} < {min_bytes}"
        );
    }

    for (path, min_lines, label) in [
        (
            "../../../anvil/corpus/capture_2026-06-11_79555frames.jsonl",
            79_000,
            "full raw feed capture",
        ),
        (
            "../../../anvil/corpus/capture_snapshot_76k.jsonl",
            76_000,
            "raw feed snapshot",
        ),
        (
            "../../../anvil/corpus/blk_464734243.jsonl",
            20,
            "route685 corpus",
        ),
        (
            "../../../anvil/corpus/blk_465686729.jsonl",
            20,
            "2-hop profit neighboring block corpus",
        ),
        (
            "../../../anvil/corpus/blk_469953963.jsonl",
            10,
            "replacement-set corpus",
        ),
    ] {
        let count = nonempty_line_count(path, label);
        assert!(
            count >= min_lines,
            "{label} has too few rows: {count} < {min_lines}"
        );
    }
}
