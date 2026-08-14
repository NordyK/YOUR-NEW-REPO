//! WI-06 T3 fix round 5 (codex round-5 item 2, totality) — BlockTag proof-param static contract.
//!
//! codex round-5 nit I2: the generic proof-fetch APIs (`fetch_state_root`, `fetch_account_proof`)
//! accepted a `BlockTag` but serialized it with `block.as_str()`. For `BlockTag::Hash` that emits a
//! BARE hash hex — an INVALID positional JSON-RPC block parameter; a hash MUST use the EIP-1898
//! object (`BlockTag::to_json_param`). Current callers happened to pass `Number` only, so production
//! was correct, but the API was not TOTAL: a future caller passing `Hash` would silently serialize
//! wrong.
//!
//! The fix is TYPE-LEVEL: those APIs now take a `u64` block NUMBER, so `BlockTag::Hash` cannot be
//! passed at all (only `BlockTag::Number` is ever constructed internally, whose `as_str` is the
//! correct `0x…` number form). This static tripwire FAILS (RED) if either signature reverts to a
//! `BlockTag` block param — reopening the wrong-serialization hole.

use std::path::PathBuf;

fn revm_fork_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/revm_fork.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Extract a function's SIGNATURE (from `pub fn <name>(` up to the opening `{`).
fn signature_of<'a>(src: &'a str, name: &str) -> &'a str {
    let needle = format!("pub fn {name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("`pub fn {name}(` not found in revm_fork.rs"));
    let rest = &src[start..];
    let end = rest.find('{').expect("function body brace");
    &rest[..end]
}

/// The generic proof-fetch APIs must take a NUMERIC (`u64`) block param — never a `BlockTag` (which
/// could be a `Hash` that serializes wrong via `as_str`).
#[test]
fn generic_proof_fetch_apis_take_numeric_block_not_blocktag() {
    let src = revm_fork_src();
    for name in ["fetch_state_root", "fetch_account_proof"] {
        let sig = signature_of(&src, name);
        assert!(
            sig.contains("block: u64"),
            "{name} must take a `u64` block NUMBER (type-level guarantee that BlockTag::Hash can \
             never reach the invalid `as_str` positional serialization); signature was:\n{sig}"
        );
        assert!(
            !sig.contains("block: BlockTag"),
            "{name} must NOT take a `BlockTag` block param — a `Hash` would serialize as a bare hash \
             hex, an invalid positional JSON-RPC block parameter (codex round-5 item 2); signature \
             was:\n{sig}"
        );
    }
}
