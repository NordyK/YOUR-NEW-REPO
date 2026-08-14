use std::{
    fs,
    path::{Path, PathBuf},
};

const POSITIVE_REPLAY_ARTIFACT: &str =
    "engine/contracts/test/MantisAnvilCatalogPositiveReplay.Fork.t.sol";
const FOUNDRY_TOML: &str = "engine/contracts/foundry.toml";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("engine/crates/engine has repo root ancestor")
        .to_path_buf()
}

fn read_required(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn positive_replay_artifact_is_not_yet_a_fresh_offline_laptop_execution_proof() {
    let artifact = read_required(POSITIVE_REPLAY_ARTIFACT);
    let foundry_toml = read_required(FOUNDRY_TOML);

    assert_eq!(
        count_occurrences(&artifact, "vm.rollFork("),
        2,
        "positive replay artifact should expose exactly two fork-backed replay tests"
    );
    assert!(
        artifact.contains("vm.rollFork(TX_3HOP_PANCAKE_UNI);")
            && artifact.contains("vm.rollFork(TX_4HOP_UNI_V3);"),
        "positive replay artifact must keep both tx-hash rollFork proofs explicit"
    );
    assert!(
        artifact.contains("require(block.chainid == 42161, \"fork Arbitrum One\")"),
        "positive replay artifact must remain tied to an Arbitrum One fork"
    );
    assert!(
        !artifact.contains("vm.createSelectFork(")
            && !artifact.contains("ARBITRUM_RPC_URL")
            && !artifact.contains("anvil/block_cache"),
        "positive replay artifact should not masquerade as an offline cache-backed harness"
    );

    assert!(
        foundry_toml.contains("arbitrum = \"${ARBITRUM_RPC_URL}\""),
        "foundry.toml must expose the Arbitrum fork endpoint via ARBITRUM_RPC_URL"
    );
    assert!(
        foundry_toml.contains("[profile.default.rpc_storage_caching]")
            && foundry_toml.contains("chains = [\"arbitrum\"]")
            && foundry_toml.contains("endpoints = \"all\""),
        "foundry.toml must keep fork storage caching visible as replay acceleration, not proof"
    );
    assert!(
        artifact.contains("function test_")
            && artifact.contains("vm.rollFork(")
            && foundry_toml.contains("arbitrum = \"${ARBITRUM_RPC_URL}\""),
        "positive replay boundary must be proven from Solidity source plus Foundry config, not archived Markdown reports"
    );
}
