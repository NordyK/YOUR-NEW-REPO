//! WI-7 — unified arb-taxonomy money-proof grid.
//!
//! Proves the engine's arbitrage taxonomy is grounded in REAL, replayable on-chain arbs across the
//! full {arb_type x subtype x family} grid, with NO unreplayable `0x` placeholders in proven cells.
//! The catalog (contracts/test/fixtures/arb_taxonomy_catalog.csv) is the replay-ready superset of
//! anvil/dune_arb_taxonomy.csv: the source CSV left every same_dex row's tx as a `0x` placeholder;
//! here each is resolved to a real on-chain tx (Dune 7730487, verified on-chain at its block).
//!
//! Every populated cell maps to an existing proof bucket, asserted here:
//!   * family_pnl  -> the family realizes positive PnL in MantisFamilyRealizedPnL.Fork.t.sol
//!   * candidate   -> net-positive REVM-success rows for that exchange_id in the economics fixture
//!   * composite   -> an executable <=4-hop cross-dex cycle whose every constituent family resolves
//!   * historical_taxonomy -> a deep >=5-hop cross-dex cycle kept for taxonomy coverage only
//! Grid coverage (cross_dex 2/3/4-hop; same_dex fee_arb/multihop/stable) is enforced so the catalog
//! cannot silently drop a taxonomy class.

use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const CATALOG: &str = "../../contracts/test/fixtures/arb_taxonomy_catalog.csv";
const FAMILY_SOL: &str = "../../contracts/test/MantisFamilyRealizedPnL.Fork.t.sol";
const ECON_FIXTURE: &str = "../../contracts/test/fixtures/candidate_records_economics_sample.jsonl";
const SOURCE_TAXONOMY: &str = "../../../anvil/dune_arb_taxonomy.csv";

#[derive(Debug, Clone)]
struct CatalogRow {
    block: u64,
    tx_hash: String,
    arb_type: String,
    subtype: String,
    hops: u32,
    families: String,
    proof_kind: String,
    proof_ref: String,
}

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn data_lines(raw: &str) -> impl Iterator<Item = &str> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

fn is_full_tx_hash(s: &str) -> bool {
    s.len() == 66 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_catalog() -> Vec<CatalogRow> {
    data_lines(&read(CATALOG))
        .map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            assert_eq!(f.len(), 10, "catalog row must have 10 fields: {line}");
            CatalogRow {
                block: f[0].parse().unwrap_or_else(|_| panic!("bad block: {line}")),
                tx_hash: f[1].to_ascii_lowercase(),
                arb_type: f[2].to_string(),
                subtype: f[3].to_string(),
                hops: f[4].parse().unwrap_or_else(|_| panic!("bad hops: {line}")),
                families: f[5].to_string(),
                proof_kind: f[8].to_string(),
                proof_ref: f[9].to_string(),
            }
        })
        .collect()
}

/// Families realized-positive (or documented) in the per-family fork test — the first quoted string
/// of each `Arb(...)` entry.
fn family_pnl_set() -> BTreeSet<String> {
    let src = read(FAMILY_SOL);
    let mut set = BTreeSet::new();
    for chunk in src.split("= Arb(").skip(1) {
        if let Some(start) = chunk.find('"') {
            if let Some(end) = chunk[start + 1..].find('"') {
                set.insert(chunk[start + 1..start + 1 + end].to_string());
            }
        }
    }
    set
}

/// dex project name (as used by Dune's `project`) -> Mantis exchange id with a candidate-record proof.
fn dex_to_exchange_id(dex: &str) -> Option<u64> {
    match dex {
        "uniswap" => Some(0),     // UniswapV3 (also V4=20)
        "pancakeswap" => Some(6), // PancakeV3
        "sushiswap" | "sushi" => Some(25),
        _ => None,
    }
}

fn econ_rows() -> Vec<Value> {
    read(ECON_FIXTURE)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("valid candidate-record JSON"))
        .collect()
}

fn net_positive_success_for_exchange(rows: &[Value], id: u64) -> usize {
    rows.iter()
        .filter(|r| r.get("revm_result").and_then(Value::as_str) == Some("success"))
        .filter(|r| {
            r.get("exchange_ids")
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_u64() == Some(id)))
        })
        .filter(|r| {
            r.get("net_profit_wei")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<i128>().ok())
                .is_some_and(|n| n > 0)
        })
        .count()
}

/// Resolve a single family name to a money-proof (or `native` WETH-wrap leg, which needs no AMM proof).
fn family_resolves(fam: &str, pnl: &BTreeSet<String>, econ: &[Value]) -> bool {
    if fam == "native" {
        return true;
    }
    if pnl.contains(fam) {
        return true;
    }
    if let Some(id) = dex_to_exchange_id(fam) {
        return net_positive_success_for_exchange(econ, id) >= 1;
    }
    false
}

fn assert_composite_constituents_resolve(row: &CatalogRow, pnl: &BTreeSet<String>, econ: &[Value]) {
    for fam in row.proof_ref.split('+') {
        assert!(
            family_resolves(fam, pnl, econ),
            "block {}: composite constituent `{fam}` does not resolve to a proof bucket",
            row.block
        );
    }
    assert_eq!(
        row.families, row.proof_ref,
        "block {}: composite proof_ref must mirror the families column",
        row.block
    );
}

#[test]
fn no_placeholder_tx_hashes_in_proven_cells() {
    for row in parse_catalog() {
        assert!(
            is_full_tx_hash(&row.tx_hash),
            "block {} ({}/{}) has a non-resolved tx_hash `{}` — proven cells may not contain 0x placeholders",
            row.block,
            row.arb_type,
            row.subtype,
            row.tx_hash
        );
    }
}

#[test]
fn every_cell_maps_to_a_resolvable_money_proof() {
    let pnl = family_pnl_set();
    let econ = econ_rows();
    for row in parse_catalog() {
        match row.proof_kind.as_str() {
            "family_pnl" => assert!(
                pnl.contains(&row.proof_ref),
                "block {}: family_pnl proof `{}` not present in MantisFamilyRealizedPnL",
                row.block,
                row.proof_ref
            ),
            "candidate" => {
                let id: u64 = row.proof_ref.parse().unwrap_or_else(|_| {
                    panic!(
                        "block {}: candidate proof_ref must be an exchange id",
                        row.block
                    )
                });
                let n = net_positive_success_for_exchange(&econ, id);
                assert!(
                    n >= 1,
                    "block {}: candidate proof exchange_id {id} has no net-positive success row",
                    row.block
                );
            }
            "composite" => assert_composite_constituents_resolve(&row, &pnl, &econ),
            "historical_taxonomy" => {
                assert!(
                    row.hops > engine_routes::calldata::MANTIS_MAX_HOPS as u32,
                    "block {}: historical taxonomy rows must be deeper than the executable Mantis hop bound",
                    row.block
                );
                assert_composite_constituents_resolve(&row, &pnl, &econ);
            }
            other => panic!("block {}: unknown proof_kind `{other}`", row.block),
        }
    }
}

#[test]
fn grid_covers_cross_dex_hop_classes_and_same_dex_subtypes() {
    let rows = parse_catalog();

    // cross_dex must exercise 2-, 3-, and 4-hop cycles (plus at least one deep >=5-hop cycle).
    let cross_hops: BTreeSet<u32> = rows
        .iter()
        .filter(|r| r.arb_type == "cross_dex")
        .map(|r| r.hops)
        .collect();
    for h in [2u32, 3, 4] {
        assert!(
            cross_hops.contains(&h),
            "grid missing a cross_dex {h}-hop arb (have {cross_hops:?})"
        );
    }
    assert!(
        cross_hops.iter().any(|&h| h >= 5),
        "grid should include at least one deep (>=5-hop) cross_dex cycle (have {cross_hops:?})"
    );

    // same_dex must exercise fee_arb, multihop, and stable subtypes.
    let same_subtypes: BTreeSet<String> = rows
        .iter()
        .filter(|r| r.arb_type == "same_dex")
        .map(|r| r.subtype.clone())
        .collect();
    for s in ["fee_arb", "multihop", "stable"] {
        assert!(
            same_subtypes.contains(s),
            "grid missing a same_dex `{s}` arb (have {same_subtypes:?})"
        );
    }

    // Breadth: the catalog must span a diverse family set.
    let families: BTreeSet<String> = rows
        .iter()
        .flat_map(|r| r.families.split('+').map(str::to_string))
        .collect();
    assert!(
        families.len() >= 8,
        "catalog family breadth too low: {families:?}"
    );
}

#[test]
fn executable_capital_proof_rows_stop_at_mantis_submit_hop_bound() {
    let max_hops = engine_routes::calldata::MANTIS_MAX_HOPS as u32;
    for row in parse_catalog() {
        if row.hops > max_hops {
            assert_eq!(
                row.proof_kind, "historical_taxonomy",
                "deep row block {} has {} hops and must not be counted as executable capital proof",
                row.block, row.hops
            );
        } else {
            assert!(
                matches!(
                    row.proof_kind.as_str(),
                    "family_pnl" | "candidate" | "composite"
                ),
                "executable row block {} has unsupported proof_kind {}",
                row.block,
                row.proof_kind
            );
        }
    }
}

#[test]
fn catalog_is_consistent_with_source_taxonomy_and_resolves_its_placeholders() {
    if !Path::new(SOURCE_TAXONOMY).exists() {
        eprintln!("SKIP: artifact {SOURCE_TAXONOMY} absent (out-of-branch)");
        return;
    }
    let catalog = parse_catalog();
    let raw = read(SOURCE_TAXONOMY);

    let mut source_cross: Vec<(u64, String)> = Vec::new();
    let mut source_same_placeholder_blocks: BTreeSet<u64> = BTreeSet::new();
    for line in data_lines(&raw) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 3 || f[0] == "block" {
            continue; // header
        }
        let block: u64 = match f[0].parse() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let tx = f[1].to_ascii_lowercase();
        let arb_type = f[2];
        if arb_type == "cross_dex" && is_full_tx_hash(&tx) {
            source_cross.push((block, tx));
        } else if arb_type == "same_dex" && !is_full_tx_hash(&tx) {
            source_same_placeholder_blocks.insert(block);
        }
    }

    // (a) every cross_dex tx hash in the source must appear identically in the catalog.
    for (block, tx) in &source_cross {
        let found = catalog
            .iter()
            .any(|r| r.block == *block && &r.tx_hash == tx);
        assert!(
            found,
            "source cross_dex block {block} tx {tx} not mirrored in the catalog"
        );
    }

    // (b) every same_dex row that was a `0x` placeholder in the source must be resolved to a real
    //     on-chain tx in the catalog (this is the WI-7 backfill).
    for block in &source_same_placeholder_blocks {
        let resolved = catalog
            .iter()
            .find(|r| r.block == *block && r.arb_type == "same_dex");
        let resolved = resolved.unwrap_or_else(|| {
            panic!("source same_dex placeholder block {block} missing from catalog")
        });
        assert!(
            is_full_tx_hash(&resolved.tx_hash),
            "source same_dex placeholder block {block} is still unresolved in the catalog"
        );
    }

    assert_eq!(
        source_same_placeholder_blocks.len(),
        9,
        "expected the 9 known same_dex placeholders in the source taxonomy"
    );
}

#[test]
fn catalog_paths_exist() {
    if !Path::new(SOURCE_TAXONOMY).exists() {
        eprintln!("SKIP: artifact {SOURCE_TAXONOMY} absent (out-of-branch)");
        return;
    }
    for p in [CATALOG, FAMILY_SOL, ECON_FIXTURE, SOURCE_TAXONOMY] {
        assert!(PathBuf::from(p).exists(), "required artifact missing: {p}");
    }
}
