use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde_json::Value;

const MANIFEST: &str = "../../contracts/test/fixtures/certification_candidates.local.json";
const CATALOG: &str = "../../../anvil/anvil_catalog.csv";
const POSITIVE_REPLAY_ARTIFACT: &str =
    "../../contracts/test/MantisAnvilCatalogPositiveReplay.Fork.t.sol";

#[derive(Debug, Eq, PartialEq)]
struct ReplayProof {
    name: String,
    block: u64,
    tx_hash: String,
}

fn read_required(path: impl AsRef<Path>, label: &str) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {label} at {}: {err}", path.display()))
}

fn is_tx_hash(raw: &str) -> bool {
    raw.len() == 66 && raw.starts_with("0x") && raw[2..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn parse_tx_constant(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let rest = line.strip_prefix("bytes32 constant ")?;
    let (name, rhs) = rest.split_once(" = ")?;
    let tx_hash = rhs.trim_end_matches(';').to_ascii_lowercase();
    assert!(
        is_tx_hash(&tx_hash),
        "invalid tx constant {name}: {tx_hash}"
    );
    Some((name.to_string(), tx_hash))
}

fn parse_rollfork_name(line: &str) -> Option<String> {
    let line = line.trim();
    let start = line.find("vm.rollFork(")? + "vm.rollFork(".len();
    let end = line[start..].find(')')? + start;
    Some(line[start..end].trim().to_string())
}

fn parse_block_assertion(line: &str) -> Option<u64> {
    let line = line.trim();
    let start = line.find("assertEq(block.number,")? + "assertEq(block.number,".len();
    let end = line[start..].find(',')? + start;
    let raw = line[start..end].trim().replace('_', "");
    Some(
        raw.parse::<u64>()
            .unwrap_or_else(|err| panic!("invalid block assertion {raw}: {err}")),
    )
}

fn artifact_replay_proofs() -> Vec<ReplayProof> {
    let artifact = read_required(POSITIVE_REPLAY_ARTIFACT, "positive replay artifact");
    let mut constants = BTreeMap::<String, String>::new();
    for line in artifact.lines() {
        if let Some((name, tx_hash)) = parse_tx_constant(line) {
            constants.insert(name, tx_hash);
        }
    }

    let mut rollfork_name = None::<String>;
    let mut proofs = Vec::new();
    for line in artifact.lines() {
        if let Some(name) = parse_rollfork_name(line) {
            rollfork_name = Some(name);
            continue;
        }
        if let (Some(name), Some(block)) = (rollfork_name.take(), parse_block_assertion(line)) {
            let tx_hash = constants
                .get(&name)
                .unwrap_or_else(|| panic!("rollFork references missing tx constant {name}"))
                .clone();
            proofs.push(ReplayProof {
                name,
                block,
                tx_hash,
            });
        }
    }

    proofs
}

fn positive_manifest_keys() -> BTreeSet<String> {
    let manifest: Value = serde_json::from_str(&read_required(MANIFEST, "candidate manifest"))
        .expect("candidate manifest must parse as JSON");
    manifest["candidates"]
        .as_array()
        .expect("manifest candidates must be an array")
        .iter()
        .filter(|candidate| {
            candidate["route_reconstruction_status"].as_str() == Some("reconstructed")
                && candidate["profit_certification_status"].as_str() == Some("positive_realized")
        })
        .map(|candidate| {
            let block = candidate["block_number"]
                .as_u64()
                .expect("positive manifest candidate block_number must be u64");
            let tx_hash = candidate["tx_hash"]
                .as_str()
                .expect("positive manifest candidate tx_hash must be string")
                .to_ascii_lowercase();
            assert!(is_tx_hash(&tx_hash), "invalid manifest tx hash {tx_hash}");
            format!("{block}:{tx_hash}")
        })
        .collect()
}

fn catalog_keys() -> BTreeSet<String> {
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
            assert_eq!(fields.len(), 8, "catalog line {line_no} must have 8 fields");
            let block = fields[0]
                .parse::<u64>()
                .unwrap_or_else(|err| panic!("catalog line {line_no} invalid block: {err}"));
            let tx_hash = fields[1].to_ascii_lowercase();
            assert!(
                is_tx_hash(&tx_hash),
                "catalog line {line_no} invalid tx hash {tx_hash}"
            );
            Some(format!("{block}:{tx_hash}"))
        })
        .collect()
}

#[test]
fn positive_replay_artifact_has_explicit_manifest_coverage_boundary() {
    if !Path::new(CATALOG).exists() {
        eprintln!("SKIP: artifact {CATALOG} absent (out-of-branch)");
        return;
    }
    let proofs = artifact_replay_proofs();
    assert_eq!(
        proofs.len(),
        2,
        "positive replay artifact should keep both current replay tests visible: {proofs:?}"
    );

    let positive_manifest = positive_manifest_keys();
    let catalog = catalog_keys();
    assert_eq!(
        positive_manifest.len(),
        1,
        "manifest should keep exact current positive replay count visible: {positive_manifest:?}"
    );

    let missing_manifest_or_catalog = proofs
        .iter()
        .filter_map(|proof| {
            let key = format!("{}:{}", proof.block, proof.tx_hash);
            if positive_manifest.contains(&key) && catalog.contains(&key) {
                None
            } else {
                Some(format!(
                    "{}:{}:manifest={}:catalog={}",
                    proof.name,
                    key,
                    positive_manifest.contains(&key),
                    catalog.contains(&key)
                ))
            }
        })
        .collect::<Vec<_>>();

    assert_eq!(
        missing_manifest_or_catalog,
        vec![
            "TX_4HOP_UNI_V3:463931868:0x004d7ccb8f83cd09b76f591298a21e22006fca88e6e6318bcbf93b5e72d43d94:manifest=false:catalog=false"
        ],
        "positive replay manifest/catalog boundary changed; reclassify artifact coverage before promoting every replay proof: {missing_manifest_or_catalog:?}"
    );
}
