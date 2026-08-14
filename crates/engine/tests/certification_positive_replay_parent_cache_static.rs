use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const POSITIVE_REPLAY_ARTIFACT: &str =
    "../../contracts/test/MantisAnvilCatalogPositiveReplay.Fork.t.sol";
const CACHE_TOP_LEVEL_KEYS: &[&str] = &["accounts", "block_hashes", "meta", "storage"];

#[derive(Debug)]
struct ReplayProof {
    name: String,
    block: u64,
    tx_hash: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from("../../..")
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

fn cache_path(block: u64) -> PathBuf {
    let direct = repo_root()
        .join("anvil/block_cache")
        .join(block.to_string());
    if direct.is_file() {
        return direct;
    }
    direct.join("storage.json")
}

fn parse_cache(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
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

fn parse_hex_u64(raw: &str, label: &str) -> u64 {
    u64::from_str_radix(raw.trim_start_matches("0x"), 16)
        .unwrap_or_else(|err| panic!("{label} is not hex u64 ({raw}): {err}"))
}

#[test]
fn positive_replay_artifact_blocks_have_parent_cache_state_material() {
    let block_cache = repo_root().join("anvil/block_cache");
    if !block_cache.exists() {
        eprintln!(
            "SKIP: artifact {} absent (out-of-branch)",
            block_cache.display()
        );
        return;
    }
    let proofs = artifact_replay_proofs();
    assert_eq!(
        proofs.len(),
        2,
        "positive replay artifact should expose two replay proofs: {proofs:?}"
    );

    let mut parent_hits = 0usize;
    let mut total_parent_bytes = 0u64;
    let mut parent_blocks = BTreeSet::new();
    let mut proof_names = BTreeSet::new();

    for proof in &proofs {
        assert!(is_tx_hash(&proof.tx_hash), "invalid proof tx {:?}", proof);
        proof_names.insert(proof.name.clone());
        let parent_block = proof
            .block
            .checked_sub(1)
            .unwrap_or_else(|| panic!("replay proof block {} has no parent", proof.block));
        parent_blocks.insert(parent_block);
        let path = cache_path(parent_block);
        assert!(
            path.is_file(),
            "positive replay proof {} missing parent cache {} at {}",
            proof.name,
            parent_block,
            path.display()
        );
        let bytes = fs::metadata(&path)
            .unwrap_or_else(|err| panic!("metadata {}: {err}", path.display()))
            .len();
        assert!(
            bytes >= 1_000_000,
            "positive replay proof {} parent cache too small: {bytes}",
            proof.name
        );
        total_parent_bytes += bytes;
        parent_hits += 1;

        let cache = parse_cache(&path);
        assert_eq!(
            sorted_keys(&cache),
            expected_keys(CACHE_TOP_LEVEL_KEYS),
            "positive replay proof {} parent cache schema drifted",
            proof.name
        );
        let accounts = cache
            .get("accounts")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("positive replay proof {} missing accounts", proof.name));
        let storage = cache
            .get("storage")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("positive replay proof {} missing storage", proof.name));
        assert!(
            !accounts.is_empty(),
            "positive replay proof {} parent cache has no accounts",
            proof.name
        );
        assert!(
            !storage.is_empty(),
            "positive replay proof {} parent cache has no storage",
            proof.name
        );
        let block_env_number = cache
            .get("meta")
            .and_then(|meta| meta.get("block_env"))
            .and_then(|block_env| block_env.get("number"))
            .and_then(Value::as_str)
            .map(|raw| parse_hex_u64(raw, "meta.block_env.number"))
            .unwrap_or_else(|| {
                panic!(
                    "positive replay proof {} missing block_env.number",
                    proof.name
                )
            });
        assert!(
            block_env_number >= 20_000_000,
            "positive replay proof {} has implausible block_env.number {block_env_number}",
            proof.name
        );
    }

    assert!(
        proof_names.contains("TX_3HOP_PANCAKE_UNI") && proof_names.contains("TX_4HOP_UNI_V3"),
        "unexpected positive replay proof names: {proof_names:?}"
    );
    assert_eq!(
        parent_hits,
        proofs.len(),
        "not every positive replay proof has parent cache state; parent_hits={parent_hits}, proofs={}, parent_blocks={parent_blocks:?}, total_parent_bytes={total_parent_bytes}",
        proofs.len()
    );
}
