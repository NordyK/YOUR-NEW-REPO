use serde_json::Value;
use std::{fs, path::Path};

#[test]
fn dune_candidate_manifest_schema_contains_raw_reconstruction_contract() {
    let path = Path::new("../../contracts/test/fixtures/dune_candidate_manifest.schema.json");
    let raw = fs::read_to_string(path).expect("read dune candidate schema");
    let schema: Value = serde_json::from_str(&raw).expect("schema must be valid json");

    let required = schema["properties"]["candidates"]["items"]["required"]
        .as_array()
        .expect("candidate required list");
    for field in [
        "block_number",
        "tx_hash",
        "family_tags",
        "project_path",
        "dune_amount_usd",
        "first_token",
        "last_token",
        "raw_reconstruction_status",
    ] {
        assert!(
            required.iter().any(|v| v.as_str() == Some(field)),
            "candidate schema must require {field}"
        );
    }

    let statuses = schema["properties"]["candidates"]["items"]["properties"]
        ["raw_reconstruction_status"]["enum"]
        .as_array()
        .expect("raw reconstruction status enum");
    for status in [
        "unverified",
        "reconstructed",
        "rejected_non_contiguous",
        "rejected_missing_raw",
    ] {
        assert!(
            statuses.iter().any(|v| v.as_str() == Some(status)),
            "schema must include raw_reconstruction_status={status}"
        );
    }
}

#[test]
fn dune_candidate_sample_manifest_is_discovery_only_until_reconstructed() {
    let path = Path::new("../../contracts/test/fixtures/dune_candidate_manifest.sample.json");
    let raw = fs::read_to_string(path).expect("read dune sample manifest");
    let manifest: Value = serde_json::from_str(&raw).expect("sample manifest must be valid json");

    let query_ids = manifest["dune_query_ids"]
        .as_array()
        .expect("dune_query_ids must be an array");
    for query_id in [7716387_u64, 7716480, 7716482] {
        assert!(
            query_ids.iter().any(|v| v.as_u64() == Some(query_id)),
            "sample manifest must cite Dune query {query_id}"
        );
    }

    let candidates = manifest["candidates"]
        .as_array()
        .expect("candidates must be an array");
    assert!(
        !candidates.is_empty(),
        "sample manifest must include candidates"
    );

    for candidate in candidates {
        let status = candidate["raw_reconstruction_status"]
            .as_str()
            .expect("raw_reconstruction_status must be a string");
        let promotion = candidate["certification_promotion_status"]
            .as_str()
            .expect("certification_promotion_status must be a string");
        let raw_log_count = candidate["raw_log_count"]
            .as_u64()
            .expect("raw_log_count must be present");
        let calldata_bytes = candidate["calldata_bytes"]
            .as_u64()
            .expect("calldata_bytes must be present");

        assert!(
            raw_log_count > 0,
            "sample candidates must have raw log material"
        );
        assert!(
            calldata_bytes > 0,
            "sample candidates must have calldata material"
        );
        if status != "reconstructed" {
            assert_ne!(
                promotion, "certified_fixture",
                "unreconstructed candidates cannot be certification fixtures"
            );
        }
    }
}
