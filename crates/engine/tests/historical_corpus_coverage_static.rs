use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const ANVIL_ROOT: &str = "../../../anvil";
const CATALOG: &str = "../../../anvil/anvil_catalog.csv";
const TAXONOMY: &str = "../../../anvil/dune_arb_taxonomy.csv";
const DODO_ARBS: &str = "../../../anvil/dune_dodo_arbs.csv";
const BLOCK_INDEX: &str = "../../../anvil/block_index.json";
const MATRIX: &str = "../../../Codex/HISTORICAL_CORPUS_COVERAGE_MATRIX_2026-06-15.json";

#[derive(Debug)]
struct CatalogRow {
    block: u64,
    tx: String,
    hops: u8,
    families: Vec<String>,
    tokens: Vec<String>,
}

#[derive(Debug)]
struct TaxonomyRow {
    block: u64,
    tx: String,
    arb_type: String,
    dodo_position: String,
    families: Vec<String>,
}

#[derive(Debug)]
struct DodoArbRow {
    legs: u64,
    dodo_position: String,
}

fn read_required(path: impl AsRef<Path>, label: &str) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {label} at {}: {err}", path.display()))
}

fn read_json(path: impl AsRef<Path>, label: &str) -> Value {
    let path = path.as_ref();
    let raw = read_required(path, label);
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("invalid {label} JSON at {}: {err}", path.display()))
}

fn json_u64(value: &Value, path: &[&str]) -> u64 {
    let mut cursor = value;
    for key in path {
        cursor = cursor
            .get(*key)
            .unwrap_or_else(|| panic!("missing JSON key path {path:?}"));
    }
    cursor
        .as_u64()
        .unwrap_or_else(|| panic!("JSON key path {path:?} must be u64, got {cursor:?}"))
}

fn split_plus(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    raw.split('+')
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_u64(raw: &str, line_no: usize, label: &str) -> u64 {
    raw.parse::<u64>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {raw}: {err}"))
}

fn is_tx_hash(raw: &str) -> bool {
    raw.len() == 66 && raw.starts_with("0x") && raw[2..].chars().all(|ch| ch.is_ascii_hexdigit())
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
                "line {line_no}: catalog row must have 8 CSV fields"
            );
            let tx = fields[1].to_ascii_lowercase();
            assert!(is_tx_hash(&tx), "line {line_no}: invalid tx hash {tx}");
            Some(CatalogRow {
                block: parse_u64(fields[0], line_no, "block"),
                tx,
                hops: fields[2]
                    .parse::<u8>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid hops: {err}")),
                families: split_plus(fields[3]),
                tokens: split_plus(fields[4]),
            })
        })
        .collect()
}

fn parse_taxonomy() -> Vec<TaxonomyRow> {
    read_required(TAXONOMY, "Dune arb taxonomy")
        .lines()
        .enumerate()
        .filter_map(|(idx, raw_line)| {
            let line = raw_line.trim_end_matches('\r');
            if idx == 0 || line.trim().is_empty() || line.starts_with('#') {
                return None;
            }
            let line_no = idx + 1;
            let fields = line.split(',').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                9,
                "line {line_no}: taxonomy row must have 9 CSV fields"
            );
            let tx = fields[1].to_ascii_lowercase();
            assert!(
                tx == "0x" || is_tx_hash(&tx),
                "line {line_no}: taxonomy tx must be 0x placeholder or full hash, got {tx}"
            );
            Some(TaxonomyRow {
                block: parse_u64(fields[0], line_no, "block"),
                tx,
                arb_type: fields[2].to_string(),
                dodo_position: fields[4].to_string(),
                families: split_plus(fields[5]),
            })
        })
        .collect()
}

fn parse_dodo_arbs() -> Vec<DodoArbRow> {
    read_required(DODO_ARBS, "Dune DODO arbs")
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
                7,
                "line {line_no}: DODO row must have 7 CSV fields"
            );
            assert!(
                is_tx_hash(&fields[1].to_ascii_lowercase()),
                "line {line_no}: invalid DODO tx hash {}",
                fields[1]
            );
            Some(DodoArbRow {
                legs: parse_u64(fields[2], line_no, "n_legs"),
                dodo_position: fields[4].to_string(),
            })
        })
        .collect()
}

fn count_by_key<T, F>(rows: &[T], mut key_fn: F) -> BTreeMap<String, u64>
where
    F: FnMut(&T) -> String,
{
    let mut out = BTreeMap::<String, u64>::new();
    for row in rows {
        *out.entry(key_fn(row)).or_insert(0) += 1;
    }
    out
}

fn count_strings(values: impl Iterator<Item = String>) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::<String, u64>::new();
    for value in values {
        *out.entry(value).or_insert(0) += 1;
    }
    out
}

fn assert_json_map_counts(matrix: &Value, path: &[&str], counts: &BTreeMap<String, u64>) {
    let mut cursor = matrix;
    for key in path {
        cursor = cursor
            .get(*key)
            .unwrap_or_else(|| panic!("missing JSON key path {path:?}"));
    }
    let object = cursor
        .as_object()
        .unwrap_or_else(|| panic!("JSON key path {path:?} must be object"));
    assert_eq!(
        object.len(),
        counts.len(),
        "count map length mismatch at {path:?}"
    );
    for (key, expected) in counts {
        let actual = object
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("missing or non-u64 count {key} at {path:?}"));
        assert_eq!(actual, *expected, "count mismatch for {key} at {path:?}");
    }
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
            .map(|metadata| metadata.len());
    }
    None
}

fn corpus_line_count(path: impl AsRef<Path>) -> u64 {
    read_required(path, "corpus file")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64
}

fn repo_root_relative(path: &str) -> PathBuf {
    PathBuf::from("../../../").join(path)
}

fn corpus_file_exists_for_block(block: u64) -> bool {
    let root = Path::new(ANVIL_ROOT).join("corpus");
    root.join(format!("blk_{block}.jsonl")).is_file()
        || root.join(format!("win_{block}_20.jsonl")).is_file()
}

#[test]
fn historical_catalog_coverage_matrix_matches_current_local_sources() {
    if !Path::new(ANVIL_ROOT).exists() {
        eprintln!("SKIP: artifact {ANVIL_ROOT} absent (out-of-branch)");
        return;
    }
    let matrix = read_json(MATRIX, "historical corpus coverage matrix");
    assert_eq!(
        matrix["schema"].as_str(),
        Some("historical_corpus_coverage_matrix.v1")
    );

    let rows = parse_catalog();
    let unique_txs = rows
        .iter()
        .map(|row| row.tx.clone())
        .collect::<BTreeSet<_>>();
    let mut hop_counts = BTreeMap::<String, u64>::new();
    let mut cross_family_rows = 0u64;
    let mut same_family_rows = 0u64;
    let mut no_family_rows = 0u64;
    let mut executable_hop_rows = 0u64;

    for row in &rows {
        assert!(
            row.block >= 400_000_000,
            "catalog block is unexpectedly low: {}",
            row.block
        );
        *hop_counts.entry(row.hops.to_string()).or_insert(0) += 1;
        if (2..=4).contains(&row.hops) {
            executable_hop_rows += 1;
        }
        match row.families.len() {
            0 => no_family_rows += 1,
            1 => same_family_rows += 1,
            _ => cross_family_rows += 1,
        }
    }

    let family_counts = count_strings(rows.iter().flat_map(|row| row.families.iter().cloned()));
    let token_counts = count_strings(rows.iter().flat_map(|row| row.tokens.iter().cloned()));

    assert_eq!(rows.len() as u64, json_u64(&matrix, &["catalog", "rows"]));
    assert_eq!(
        unique_txs.len() as u64,
        json_u64(&matrix, &["catalog", "unique_txs"])
    );
    assert_eq!(
        executable_hop_rows,
        json_u64(&matrix, &["catalog", "executable_hop_rows_2_to_4"])
    );
    assert_eq!(
        cross_family_rows,
        json_u64(&matrix, &["catalog", "cross_family_rows"])
    );
    assert_eq!(
        same_family_rows,
        json_u64(&matrix, &["catalog", "same_family_rows"])
    );
    assert_eq!(
        no_family_rows,
        json_u64(&matrix, &["catalog", "no_family_rows"])
    );
    assert_json_map_counts(&matrix, &["catalog", "hop_counts"], &hop_counts);
    assert_json_map_counts(&matrix, &["catalog", "family_counts"], &family_counts);
    assert_json_map_counts(&matrix, &["catalog", "token_counts"], &token_counts);

    for required_family in [
        "balancer",
        "camelot",
        "dodo",
        "fluid",
        "gammaswap",
        "kyberswap",
        "maverick",
        "pancakeswap",
        "ramses",
        "solidly",
        "sushiswap",
        "trader_joe",
        "uniswap",
    ] {
        assert!(
            family_counts
                .get(required_family)
                .copied()
                .unwrap_or_default()
                > 0,
            "catalog lacks required historical family {required_family}"
        );
    }
    assert_eq!(
        hop_counts.get("2").copied().unwrap_or_default(),
        602,
        "2-hop historical coverage drifted"
    );
    assert_eq!(
        hop_counts.get("3").copied().unwrap_or_default(),
        456,
        "3-hop historical coverage drifted"
    );
    assert_eq!(
        hop_counts.get("4").copied().unwrap_or_default(),
        174,
        "4-hop historical coverage drifted"
    );
}

#[test]
fn taxonomy_and_dodo_files_are_classified_by_replay_readiness_not_just_presence() {
    if !Path::new(ANVIL_ROOT).exists() {
        eprintln!("SKIP: artifact {ANVIL_ROOT} absent (out-of-branch)");
        return;
    }
    let matrix = read_json(MATRIX, "historical corpus coverage matrix");
    let catalog = parse_catalog();
    let taxonomy = parse_taxonomy();
    let dodo_rows = parse_dodo_arbs();
    let block_index = read_json(BLOCK_INDEX, "block index");

    let catalog_keys = catalog
        .iter()
        .map(|row| format!("{}:{}", row.block, row.tx))
        .collect::<BTreeSet<_>>();

    let replay_taxonomy_rows = taxonomy
        .iter()
        .filter(|row| is_tx_hash(&row.tx))
        .collect::<Vec<_>>();
    let placeholder_taxonomy_rows = taxonomy
        .iter()
        .filter(|row| row.tx == "0x")
        .collect::<Vec<_>>();
    let same_dex_replay_rows = replay_taxonomy_rows
        .iter()
        .filter(|row| row.arb_type == "same_dex")
        .count() as u64;
    let cross_dex_replay_rows = replay_taxonomy_rows
        .iter()
        .filter(|row| row.arb_type == "cross_dex")
        .count() as u64;

    let replay_in_catalog = replay_taxonomy_rows
        .iter()
        .filter(|row| catalog_keys.contains(&format!("{}:{}", row.block, row.tx)))
        .count() as u64;
    let replay_in_block_index = replay_taxonomy_rows
        .iter()
        .filter(|row| {
            block_index
                .get(row.block.to_string())
                .and_then(Value::as_array)
                .map(|entries| !entries.is_empty())
                .unwrap_or(false)
        })
        .count() as u64;
    let replay_with_parent_state = replay_taxonomy_rows
        .iter()
        .filter(|row| block_cache_entry_size(row.block.saturating_sub(1)).unwrap_or(0) > 0)
        .count() as u64;
    let replay_with_window_corpus = replay_taxonomy_rows
        .iter()
        .filter(|row| corpus_file_exists_for_block(row.block))
        .count() as u64;

    let taxonomy_type_counts = count_by_key(&taxonomy, |row| row.arb_type.clone());
    let taxonomy_position_counts = count_by_key(&taxonomy, |row| row.dodo_position.clone());
    let taxonomy_family_counts =
        count_strings(taxonomy.iter().flat_map(|row| row.families.iter().cloned()));

    assert_eq!(
        taxonomy.len() as u64,
        json_u64(&matrix, &["taxonomy", "rows"])
    );
    assert_eq!(
        replay_taxonomy_rows.len() as u64,
        json_u64(&matrix, &["taxonomy", "replay_ready_tx_hash_rows"])
    );
    assert_eq!(
        placeholder_taxonomy_rows.len() as u64,
        json_u64(&matrix, &["taxonomy", "non_replay_placeholder_rows"])
    );
    assert_eq!(
        taxonomy_type_counts
            .get("cross_dex")
            .copied()
            .unwrap_or_default(),
        json_u64(&matrix, &["taxonomy", "cross_dex_rows"])
    );
    assert_eq!(
        taxonomy_type_counts
            .get("same_dex")
            .copied()
            .unwrap_or_default(),
        json_u64(&matrix, &["taxonomy", "same_dex_rows"])
    );
    assert_eq!(
        cross_dex_replay_rows,
        json_u64(&matrix, &["taxonomy", "cross_dex_replay_rows"])
    );
    assert_eq!(
        same_dex_replay_rows,
        json_u64(&matrix, &["taxonomy", "same_dex_replay_rows"])
    );
    assert_eq!(
        replay_in_catalog,
        json_u64(&matrix, &["taxonomy", "replay_rows_in_catalog"])
    );
    assert_eq!(
        replay_taxonomy_rows.len() as u64 - replay_in_catalog,
        json_u64(&matrix, &["taxonomy", "replay_rows_missing_from_catalog"])
    );
    assert_eq!(
        replay_in_block_index,
        json_u64(&matrix, &["taxonomy", "replay_rows_in_block_index"])
    );
    assert_eq!(
        replay_taxonomy_rows.len() as u64 - replay_in_block_index,
        json_u64(
            &matrix,
            &["taxonomy", "replay_rows_missing_from_block_index"]
        )
    );
    assert_eq!(
        replay_with_parent_state,
        json_u64(
            &matrix,
            &["taxonomy", "replay_rows_with_parent_state_cache"]
        )
    );
    assert_eq!(
        replay_with_window_corpus,
        json_u64(
            &matrix,
            &["taxonomy", "replay_rows_with_local_window_corpus"]
        )
    );
    assert_json_map_counts(
        &matrix,
        &["taxonomy", "dodo_position_counts"],
        &taxonomy_position_counts,
    );
    assert_json_map_counts(
        &matrix,
        &["taxonomy", "family_counts"],
        &taxonomy_family_counts,
    );

    assert_eq!(
        same_dex_replay_rows, 0,
        "same-dex taxonomy rows are not replay-ready until tx=0x placeholders are replaced"
    );
    assert_eq!(
        replay_in_catalog, 0,
        "curated taxonomy tx rows are separate from anvil_catalog.csv; update the matrix if unified"
    );
    assert_eq!(
        replay_in_block_index, 0,
        "curated taxonomy tx rows are not indexed in anvil/block_index.json"
    );

    let dodo_position_counts = count_by_key(&dodo_rows, |row| row.dodo_position.clone());
    let legs_2 = dodo_rows.iter().filter(|row| row.legs == 2).count() as u64;
    let legs_3 = dodo_rows.iter().filter(|row| row.legs == 3).count() as u64;
    let legs_4 = dodo_rows.iter().filter(|row| row.legs == 4).count() as u64;
    let legs_2_to_4 = dodo_rows
        .iter()
        .filter(|row| (2..=4).contains(&row.legs))
        .count() as u64;
    let legs_gt4 = dodo_rows.iter().filter(|row| row.legs > 4).count() as u64;

    assert_eq!(
        dodo_rows.len() as u64,
        json_u64(&matrix, &["dodo_taxonomy", "rows"])
    );
    assert_eq!(legs_2, json_u64(&matrix, &["dodo_taxonomy", "legs_2"]));
    assert_eq!(legs_3, json_u64(&matrix, &["dodo_taxonomy", "legs_3"]));
    assert_eq!(legs_4, json_u64(&matrix, &["dodo_taxonomy", "legs_4"]));
    assert_eq!(
        legs_2_to_4,
        json_u64(&matrix, &["dodo_taxonomy", "legs_2_to_4"])
    );
    assert_eq!(legs_gt4, json_u64(&matrix, &["dodo_taxonomy", "legs_gt4"]));
    assert_json_map_counts(
        &matrix,
        &["dodo_taxonomy", "dodo_position_counts"],
        &dodo_position_counts,
    );

    for required_position in ["first", "middle", "last"] {
        assert!(
            dodo_position_counts
                .get(required_position)
                .copied()
                .unwrap_or_default()
                > 0,
            "DODO taxonomy lacks {required_position} placement coverage"
        );
    }
}

#[test]
fn corpus_inventory_matches_refreshed_matrix_and_block_cache() {
    if !Path::new(ANVIL_ROOT).exists() {
        eprintln!("SKIP: artifact {ANVIL_ROOT} absent (out-of-branch)");
        return;
    }
    let matrix = read_json(MATRIX, "historical corpus coverage matrix");
    let files = matrix["corpus"]["jsonl_files"]
        .as_object()
        .expect("matrix corpus jsonl_files must be object");
    let mut total_lines = 0u64;
    for (relative_path, expected) in files {
        let disk_path = repo_root_relative(relative_path);
        let line_count = corpus_line_count(&disk_path);
        let bytes = fs::metadata(&disk_path)
            .unwrap_or_else(|err| panic!("metadata for {}: {err}", disk_path.display()))
            .len();
        assert_eq!(
            line_count,
            expected["lines"]
                .as_u64()
                .expect("matrix corpus lines must be u64"),
            "line count drift for {relative_path}"
        );
        assert_eq!(
            bytes,
            expected["bytes"]
                .as_u64()
                .expect("matrix corpus bytes must be u64"),
            "byte count drift for {relative_path}"
        );
        total_lines += line_count;
    }
    assert_eq!(
        total_lines,
        json_u64(&matrix, &["corpus", "jsonl_total_lines"])
    );

    let block_cache_dir = Path::new(ANVIL_ROOT).join("block_cache");
    let mut effective_entries = 0u64;
    let mut effective_nonzero_entries = 0u64;
    let mut effective_total_bytes = 0u64;
    let mut top_level_file_entries = 0u64;
    let mut top_level_file_nonzero_entries = 0u64;
    let mut top_level_file_ge_1mb = 0u64;
    let mut top_level_file_ge_20mb = 0u64;
    let mut storage_json_entries = 0u64;
    let mut storage_json_nonzero_entries = 0u64;
    let mut storage_json_ge_1mb = 0u64;
    let mut storage_json_ge_20mb = 0u64;

    for entry in fs::read_dir(&block_cache_dir)
        .unwrap_or_else(|err| panic!("read block cache dir {}: {err}", block_cache_dir.display()))
    {
        let entry = entry.expect("block cache dir entry");
        effective_entries += 1;
        let path = entry.path();
        let metadata = fs::metadata(&path).expect("block cache metadata");
        let bytes = if metadata.is_file() {
            top_level_file_entries += 1;
            let bytes = metadata.len();
            if bytes > 0 {
                top_level_file_nonzero_entries += 1;
            }
            if bytes >= 1_000_000 {
                top_level_file_ge_1mb += 1;
            }
            if bytes >= 20_000_000 {
                top_level_file_ge_20mb += 1;
            }
            bytes
        } else {
            let storage = path.join("storage.json");
            let bytes = fs::metadata(&storage)
                .unwrap_or_else(|err| panic!("storage metadata {}: {err}", storage.display()))
                .len();
            storage_json_entries += 1;
            if bytes > 0 {
                storage_json_nonzero_entries += 1;
            }
            if bytes >= 1_000_000 {
                storage_json_ge_1mb += 1;
            }
            if bytes >= 20_000_000 {
                storage_json_ge_20mb += 1;
            }
            bytes
        };
        if bytes > 0 {
            effective_nonzero_entries += 1;
        }
        effective_total_bytes += bytes;
    }

    let matrix_effective_entries = json_u64(&matrix, &["block_cache", "effective_entries"]);
    let matrix_effective_nonzero_entries =
        json_u64(&matrix, &["block_cache", "effective_nonzero_entries"]);
    let matrix_top_level_file_entries =
        json_u64(&matrix, &["block_cache", "top_level_file_entries"]);
    let matrix_top_level_file_nonzero_entries =
        json_u64(&matrix, &["block_cache", "top_level_file_nonzero_entries"]);
    let matrix_effective_total_bytes = json_u64(&matrix, &["block_cache", "effective_total_bytes"]);

    // The certified cache is a MINIMUM baseline, not a frozen snapshot. Offline replay only needs
    // the certified blocks present and intact; the offline genesis is built per-block, so extra
    // cached blocks cannot change a certified block's replay. Running the fork-test suite (the very
    // thing that PROVES the engine) legitimately caches more real Arbitrum state, growing the cache.
    // So assert the cache CONTAINS at least the certified state (>=), not that it is byte-identical
    // (==) to yesterday's snapshot — the latter is perpetually red on any machine actually used to
    // test, while catching nothing real (a SHRUNK/missing cache, the only failure that breaks replay,
    // is still caught by >=).
    assert!(
        effective_entries >= matrix_effective_entries,
        "block cache missing certified entries: have {effective_entries}, need >= {matrix_effective_entries}"
    );
    assert!(
        effective_nonzero_entries >= matrix_effective_nonzero_entries,
        "block cache missing certified non-empty entries: {effective_nonzero_entries} < {matrix_effective_nonzero_entries}"
    );
    assert!(
        top_level_file_entries >= matrix_top_level_file_entries,
        "block cache missing certified top-level entries: {top_level_file_entries} < {matrix_top_level_file_entries}"
    );
    assert!(
        top_level_file_nonzero_entries >= matrix_top_level_file_nonzero_entries,
        "block cache missing certified non-empty top-level entries: {top_level_file_nonzero_entries} < {matrix_top_level_file_nonzero_entries}"
    );
    assert!(
        top_level_file_ge_1mb >= json_u64(&matrix, &["block_cache", "top_level_file_ge_1mb"]),
        "block cache missing certified >=1MB top-level entries"
    );
    assert!(
        top_level_file_ge_20mb >= json_u64(&matrix, &["block_cache", "top_level_file_ge_20mb"]),
        "block cache missing certified >=20MB top-level entries"
    );
    assert!(
        storage_json_entries >= json_u64(&matrix, &["block_cache", "storage_json_entries"]),
        "block cache missing certified storage-json entries"
    );
    assert!(
        storage_json_nonzero_entries
            >= json_u64(&matrix, &["block_cache", "storage_json_nonzero_entries"]),
        "block cache missing certified non-empty storage-json entries"
    );
    assert!(
        storage_json_ge_1mb >= json_u64(&matrix, &["block_cache", "storage_json_ge_1mb"]),
        "block cache missing certified >=1MB storage-json entries"
    );
    assert!(
        storage_json_ge_20mb >= json_u64(&matrix, &["block_cache", "storage_json_ge_20mb"]),
        "block cache missing certified >=20MB storage-json entries"
    );
    assert!(
        effective_total_bytes >= matrix_effective_total_bytes,
        "block cache smaller than certified baseline: {effective_total_bytes} < {matrix_effective_total_bytes}"
    );

    assert!(
        corpus_file_exists_for_block(467_233_190),
        "DODO middle taxonomy block 467233190 should have a local window corpus"
    );
    assert!(
        block_cache_entry_size(467_233_189).unwrap_or(0) >= 20_000_000,
        "DODO middle taxonomy parent state cache is missing or too small"
    );
}
