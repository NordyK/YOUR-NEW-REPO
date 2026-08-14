//! [Stage 5 · MPT-verified cold tier] Merkle-Patricia-Trie proof verification for EIP-1186
//! account/storage proofs — a **provider-integrity** upgrade for the cold RPC tier.
//!
//! Ported in spirit from Fulcrum's `local_executor/mpt_verifier.rs`, adapted to Mantis's lean type
//! layer: this crate deliberately avoids the full `alloy` stack (only `alloy-primitives` is a dep),
//! so the proof types here are **local lean structs** ([`AccountProof`] / [`StorageProof`]) rather
//! than `alloy-rpc-types`, and RLP decoding is a small self-contained reader (no `alloy-rlp` dep).
//!
//! WHAT THIS IS (and is not). Stage 5 hardening, **default OFF** (`MIRROR_COLD_VERIFY=0`): when
//! enabled, a cold RPC storage value — before it enters the standing mirror — is cryptographically
//! proven to be anchored to the block's state root via `eth_getProof` + this verifier. As of WP1 the
//! NORMAL block reconcile path is fail-closed *at the reconcile boundary*: a value whose proof or
//! anchor cannot be established is REJECTED — it enters no mirror mutation, records reconciliation
//! debt, trips the mirror untrusted, and publishes no snapshot — instead of silently poisoning the
//! mirror (Finding 1). The independently-anchored root requires a distinct archive provider from base
//! (else the checked path fails closed, `UntrustedAnchor`); the header identity used for provenance +
//! resume is likewise two-provider cross-checked (WP5 FIX-B). As of WP4/WP5 ALL S5 mutation paths ride
//! this checked seam under `=1`: normal reconcile (WP1), carried reconciliation debt (WP1), deep-reorg
//! footprint recovery (WP4, plan/verify/commit + `Proven`), and persisted-journal resume hydration
//! (WP5, validated at one checked anchor before activation). Honest residual: the cross-check raises
//! forgery to needing a second agreeing provider but two colluding / same-infrastructure providers
//! still defeat it; warm resume is `=1`-only and its in-run recovery is bounded by
//! `MIRROR_RECONCILE_BUDGET_MS`.
//! It is NOT a Gate-C correctness requirement (the earlier ~79% drift was self-inflicted sampling,
//! not RPC lies) and it does NOT touch the capital / validator decision path. With the flag off, the
//! cold tier is byte-identical to today's bare `eth_getStorageAt` fill.
//!
//! Correctness model (EIP-1186). A proof is a root→leaf list of RLP-encoded trie nodes. Each node is
//! referenced by the keccak256 of its RLP (for nodes ≥ 32 bytes) or embedded inline (< 32 bytes). We
//! walk the key's nibble path, verifying at every step that the presented node hashes to the
//! reference we expected, and finally decode the leaf value and check it equals the claimed value.
//! A verified branch-miss or divergent leaf proves a zero / non-existent value (non-inclusion).
//!
//! This module is PURE (no I/O, no globals) and unit-tested with fixed, self-rooting vectors (each
//! test builds a trie by hand and derives its root with keccak, so a known-good proof verifies and
//! any tamper — value, hash chain, path, length — is rejected). The live `eth_getProof` fetch that
//! feeds it lives in [`crate::revm_fork`] and is soak/archive-gated.

use alloy_primitives::{keccak256, B256, U256};

/// Canonical empty-trie root: `keccak256(rlp(""))`. An account/storage proof that resolves to
/// "absent" is only valid against this (or a zero) root, or when the claimed values are all empty.
pub const EMPTY_ROOT_HASH: B256 = B256::new([
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
]);

/// Keccak256 of empty code (an EOA / codeless account's `codeHash`).
pub const EMPTY_CODE_HASH: B256 = B256::new([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

/// Max nodes in a single proof (DoS guard — a real Arbitrum state proof is well under this).
const MAX_PROOF_NODES: usize = 20;
/// Max bytes per trie node (DoS guard).
const MAX_NODE_SIZE: usize = 4096;

/// A verification failure with a human-readable reason (never panics; the caller maps this to a
/// fail-closed reject + a metric bump).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MptError(String);

impl MptError {
    fn new(msg: impl Into<String>) -> Self {
        MptError(msg.into())
    }
    /// The failure reason (for logging / metrics classification).
    pub fn reason(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for MptError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MptError {}

/// One EIP-1186 storage-slot proof: the slot key, the claimed value, and the RLP trie nodes proving
/// it under the account's `storageHash`.
#[derive(Debug, Clone)]
pub struct StorageProof {
    /// Storage slot key (the raw slot, pre-hash).
    pub key: U256,
    /// The value the provider claims lives at `key`.
    pub value: U256,
    /// RLP-encoded trie nodes from the storage root to the leaf (or divergence point).
    pub proof: Vec<Vec<u8>>,
}

/// One EIP-1186 account proof: the account fields the provider claims, the account-trie nodes proving
/// them under the block state root, and the per-slot storage proofs.
#[derive(Debug, Clone)]
pub struct AccountProof {
    /// The account address the proof is for.
    pub address: [u8; 20],
    /// Claimed account balance.
    pub balance: U256,
    /// Claimed account nonce.
    pub nonce: u64,
    /// Claimed storage root (`storageHash`) — the root the storage proofs verify against.
    pub storage_hash: B256,
    /// Claimed code hash.
    pub code_hash: B256,
    /// RLP-encoded trie nodes from the state root to the account leaf (or divergence point).
    pub account_proof: Vec<Vec<u8>>,
    /// The per-slot storage proofs requested alongside this account.
    pub storage_proof: Vec<StorageProof>,
}

/// Verify an EIP-1186 account proof against a trusted block `state_root`.
///
/// Confirms the account-trie hash chain from `state_root` and that the leaf's
/// `[nonce, balance, storageRoot, codeHash]` equals the proof's claimed fields — or, for an absent
/// account, that all claimed fields are empty. `Ok(())` ⇒ the account fields (incl. `storage_hash`,
/// the anchor for [`verify_storage_proof`]) are cryptographically trusted.
pub fn verify_account_proof(proof: &AccountProof, state_root: B256) -> Result<(), MptError> {
    check_proof_shape(&proof.account_proof, "account")?;

    let account_key = keccak256(proof.address.as_slice());
    match walk_trie_path(&proof.account_proof, account_key, state_root)? {
        TriePathResult::Present(leaf_value) => {
            let acct = decode_account_leaf(&leaf_value)?;
            if acct.nonce != proof.nonce {
                return Err(MptError::new(format!(
                    "account nonce mismatch: leaf {} vs claimed {}",
                    acct.nonce, proof.nonce
                )));
            }
            if acct.balance != proof.balance {
                return Err(MptError::new("account balance mismatch vs leaf"));
            }
            if acct.storage_hash != proof.storage_hash {
                return Err(MptError::new("account storage_hash mismatch vs leaf"));
            }
            if acct.code_hash != proof.code_hash {
                return Err(MptError::new("account code_hash mismatch vs leaf"));
            }
            Ok(())
        }
        TriePathResult::Absent => {
            // A verified non-inclusion only proves an EMPTY account.
            if proof.nonce == 0
                && proof.balance.is_zero()
                && proof.storage_hash == EMPTY_ROOT_HASH
                && proof.code_hash == EMPTY_CODE_HASH
            {
                Ok(())
            } else {
                Err(MptError::new(
                    "account proof proves absence but claimed fields are non-empty",
                ))
            }
        }
    }
}

/// Verify one EIP-1186 storage-slot proof against a trusted `storage_root` (typically an
/// account's already-verified `storage_hash`).
///
/// Handles the empty-root / non-inclusion cases (a verified miss proves a zero value) and, on
/// inclusion, decodes the RLP leaf value and checks it equals `sp.value`.
pub fn verify_storage_proof(sp: &StorageProof, storage_root: B256) -> Result<(), MptError> {
    // Empty storage trie: no slot exists, so only a zero value with an empty proof is valid. This is
    // the ONLY case where an empty proof is admissible.
    if storage_root == B256::ZERO || storage_root == EMPTY_ROOT_HASH {
        if !sp.proof.is_empty() || !sp.value.is_zero() {
            return Err(MptError::new("non-empty proof/value against empty storage root"));
        }
        return Ok(());
    }
    // [WP0 · A4] A non-empty storage root is NOT proven empty by an empty proof. EIP-1186 requires a
    // proof of absence to carry the nodes from the root through the last matching node (a branch with
    // an empty child/value slot, or a divergent leaf). The pre-WP0 shortcut here accepted an empty
    // proof + zero value against ANY non-empty root, which proves nothing — it is deleted. An empty
    // proof now falls through to `check_proof_shape`, which rejects it ("empty storage proof").
    check_proof_shape(&sp.proof, "storage")?;

    let slot_key = keccak256(B256::from(sp.key).as_slice());
    match walk_trie_path(&sp.proof, slot_key, storage_root)? {
        TriePathResult::Present(leaf_value) => {
            // The storage leaf's value bytes are `rlp(value)` — decode the integer and compare.
            let stored = rlp_decode_uint256(&leaf_value)?;
            if stored != sp.value {
                return Err(MptError::new("storage value mismatch vs leaf"));
            }
            Ok(())
        }
        TriePathResult::Absent => {
            if sp.value.is_zero() {
                Ok(())
            } else {
                Err(MptError::new(
                    "storage proof proves absence but claimed value is non-zero",
                ))
            }
        }
    }
}

/// Structural DoS caps shared by account + storage proofs.
fn check_proof_shape(nodes: &[Vec<u8>], what: &str) -> Result<(), MptError> {
    if nodes.is_empty() {
        return Err(MptError::new(format!("empty {what} proof")));
    }
    if nodes.len() > MAX_PROOF_NODES {
        return Err(MptError::new(format!(
            "{what} proof too long: {} nodes (max {MAX_PROOF_NODES})",
            nodes.len()
        )));
    }
    for (i, n) in nodes.iter().enumerate() {
        if n.len() > MAX_NODE_SIZE {
            return Err(MptError::new(format!(
                "{what} node {i} too large: {} bytes (max {MAX_NODE_SIZE})",
                n.len()
            )));
        }
    }
    Ok(())
}

/// Result of walking a key's nibble path through a proof.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TriePathResult {
    /// The key is present; carries the leaf's value bytes (the trie's opaque stored value).
    Present(Vec<u8>),
    /// The key is provably absent (verified branch-miss or divergent leaf).
    Absent,
}

/// A node reference we expect the next node to satisfy: either a keccak hash (node ≥ 32 bytes,
/// pulled from the proof list) or an embedded inline node (< 32 bytes, already inside the parent's
/// verified bytes — we recurse into it without consuming a proof-list entry).
enum ExpectedNode {
    /// keccak256(next node RLP) must equal this.
    Hash(B256),
    /// The next node's RLP bytes are exactly these (embedded inline in the verified parent).
    Inline(Vec<u8>),
}

/// Walk `key`'s nibbles through `proof_nodes`, verifying the hash chain from `expected_root` and
/// returning the leaf value (Present) or a verified miss (Absent).
fn walk_trie_path(
    proof_nodes: &[Vec<u8>],
    key: B256,
    expected_root: B256,
) -> Result<TriePathResult, MptError> {
    let key_nibbles = key_to_nibbles(key);
    let mut nibble_idx = 0usize;
    let mut expected = ExpectedNode::Hash(expected_root);
    // Cursor into the proof list; inline nodes do NOT advance it (they are embedded in the parent).
    let mut cursor = 0usize;
    // Bound the loop by nodes seen (hashed advance the cursor; inline recursion is bounded by node
    // size). A generous cap over MAX_PROOF_NODES accommodates a few inline hops.
    let mut steps = 0usize;

    // Walk to a terminal (Present/Absent), then verify the proof list was fully consumed. `break`
    // carries the terminal out so the post-walk consumption check runs on every success path.
    let result = loop {
        steps += 1;
        if steps > MAX_PROOF_NODES * 2 + 4 {
            return Err(MptError::new("trie walk exceeded node bound"));
        }
        let node_bytes: Vec<u8> = match &expected {
            ExpectedNode::Hash(h) => {
                let node = proof_nodes
                    .get(cursor)
                    .ok_or_else(|| MptError::new("proof ended before leaf"))?;
                cursor += 1;
                let got = keccak256(node.as_slice());
                if got != *h {
                    return Err(MptError::new("hash chain broken: node does not match reference"));
                }
                node.clone()
            }
            ExpectedNode::Inline(bytes) => bytes.clone(),
        };

        let items = rlp_decode_list(&node_bytes)?;
        match items.len() {
            // Branch node: 16 child slots + 1 value slot.
            17 => {
                if nibble_idx >= key_nibbles.len() {
                    let value = &items[16];
                    if value.payload.is_empty() {
                        break TriePathResult::Absent;
                    }
                    break TriePathResult::Present(value.payload.clone());
                }
                let nibble = key_nibbles[nibble_idx] as usize;
                nibble_idx += 1;
                let child = &items[nibble];
                if child.payload.is_empty() && !child.is_list {
                    // Empty child slot ⇒ the key is absent below here.
                    break TriePathResult::Absent;
                }
                expected = child_reference(child)?;
            }
            // Extension or leaf node: [compact-path, next|value].
            2 => {
                let (is_leaf, path) = decode_compact_hex(items[0].payload.as_slice())?;
                let remaining = &key_nibbles[nibble_idx..];
                if !remaining.starts_with(&path) {
                    break TriePathResult::Absent;
                }
                nibble_idx += path.len();
                if is_leaf {
                    // A leaf must consume the entire remaining key.
                    if nibble_idx != key_nibbles.len() {
                        break TriePathResult::Absent;
                    }
                    break TriePathResult::Present(items[1].payload.clone());
                }
                // Extension: the second item references the next node.
                expected = child_reference(&items[1])?;
            }
            n => {
                return Err(MptError::new(format!("invalid trie node with {n} items")));
            }
        }
    };

    // [WP0 · A4] Strictness: a valid proof consumes exactly its hash-referenced nodes. Any trailing,
    // never-walked node means the proof is malformed (and needlessly enlarges the DoS surface); reject
    // it rather than silently ignore it. Inline nodes never advance `cursor`, so this compares against
    // the number of top-level proof-list entries the walk actually referenced.
    if cursor != proof_nodes.len() {
        return Err(MptError::new(format!(
            "proof has {} unconsumed trailing node(s)",
            proof_nodes.len().saturating_sub(cursor)
        )));
    }
    Ok(result)
}

/// Classify a branch child / extension target as a hash reference (32-byte string) or an inline
/// embedded node (a nested list).
fn child_reference(item: &RlpItem) -> Result<ExpectedNode, MptError> {
    if item.is_list {
        // Embedded inline node (its RLP is < 32 bytes, so it lives inside the parent).
        Ok(ExpectedNode::Inline(item.raw.clone()))
    } else if item.payload.len() == 32 {
        Ok(ExpectedNode::Hash(B256::from_slice(&item.payload)))
    } else {
        Err(MptError::new(format!(
            "invalid node reference: {}-byte string (expected 32-byte hash or inline list)",
            item.payload.len()
        )))
    }
}

/// 32-byte key → 64 nibbles (high nibble first).
fn key_to_nibbles(key: B256) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    for b in key.as_slice() {
        out.push(b >> 4);
        out.push(b & 0x0f);
    }
    out
}

/// Decode MPT compact hex-prefix encoding of an extension/leaf path.
///
/// First nibble flags: bit1 = leaf, bit0 = odd-length (first path nibble in the low 4 bits of byte 0).
/// Returns `(is_leaf, path_nibbles)`.
fn decode_compact_hex(encoded: &[u8]) -> Result<(bool, Vec<u8>), MptError> {
    if encoded.is_empty() {
        return Ok((false, Vec::new()));
    }
    let first = encoded[0];
    let flags = first >> 4;
    if flags > 3 {
        return Err(MptError::new("invalid compact-hex flags"));
    }
    let is_leaf = (flags & 0x02) != 0;
    let odd = (flags & 0x01) != 0;
    let mut nibbles = Vec::with_capacity(encoded.len() * 2);
    if odd {
        nibbles.push(first & 0x0f);
    } else if first & 0x0f != 0 {
        // Even-length paths must have a zero low nibble in the flag byte.
        return Err(MptError::new("even compact-hex path has non-zero padding nibble"));
    }
    for &b in &encoded[1..] {
        nibbles.push(b >> 4);
        nibbles.push(b & 0x0f);
    }
    Ok((is_leaf, nibbles))
}

/// The four account fields decoded from an account-trie leaf value.
struct AccountData {
    nonce: u64,
    balance: U256,
    storage_hash: B256,
    code_hash: B256,
}

/// Decode an account-trie leaf value = `rlp([nonce, balance, storageRoot, codeHash])`.
fn decode_account_leaf(value: &[u8]) -> Result<AccountData, MptError> {
    let items = rlp_decode_list(value)?;
    if items.len() != 4 {
        return Err(MptError::new(format!(
            "account leaf has {} fields (expected 4)",
            items.len()
        )));
    }
    let nonce = be_u64(items[0].payload.as_slice())?;
    let balance = be_uint256(items[1].payload.as_slice())?;
    let storage_hash = be_b256(items[2].payload.as_slice())?;
    let code_hash = be_b256(items[3].payload.as_slice())?;
    Ok(AccountData {
        nonce,
        balance,
        storage_hash,
        code_hash,
    })
}

// ── Minimal RLP reader ────────────────────────────────────────────────────────────────────────
//
// Just enough to decode trie nodes (lists of strings / nested lists) and integer/hash leaf values.
// Strict: rejects truncation, trailing bytes, non-canonical length prefixes, and non-minimal
// integers (leading zeros) — the properties a tamper test flexes.

/// One decoded RLP item: whether it is a list, its content bytes (`payload`), and its full
/// header+payload encoding (`raw`, needed to recurse into an inline node).
struct RlpItem {
    is_list: bool,
    payload: Vec<u8>,
    raw: Vec<u8>,
}

/// Decode the single RLP item at the front of `input`, returning it plus the remaining bytes.
fn rlp_split(input: &[u8]) -> Result<(RlpItem, &[u8]), MptError> {
    let b0 = *input.first().ok_or_else(|| MptError::new("rlp: empty input"))?;
    let (is_list, hdr_len, payload_len) = match b0 {
        // Single byte < 0x80 is its own encoding (payload = the byte, no header).
        0x00..=0x7f => (false, 0usize, 1usize),
        // Short string (0..=55 bytes).
        0x80..=0xb7 => (false, 1usize, (b0 - 0x80) as usize),
        // Long string: next (b0-0xb7) bytes are the big-endian length.
        0xb8..=0xbf => {
            let ll = (b0 - 0xb7) as usize;
            let len_bytes = input
                .get(1..1 + ll)
                .ok_or_else(|| MptError::new("rlp: truncated long-string length"))?;
            let len = be_len(len_bytes)?;
            if len < 56 {
                return Err(MptError::new("rlp: non-canonical long string (< 56 bytes)"));
            }
            (false, 1 + ll, len)
        }
        // Short list.
        0xc0..=0xf7 => (true, 1usize, (b0 - 0xc0) as usize),
        // Long list.
        0xf8..=0xff => {
            let ll = (b0 - 0xf7) as usize;
            let len_bytes = input
                .get(1..1 + ll)
                .ok_or_else(|| MptError::new("rlp: truncated long-list length"))?;
            let len = be_len(len_bytes)?;
            if len < 56 {
                return Err(MptError::new("rlp: non-canonical long list (< 56 bytes)"));
            }
            (true, 1 + ll, len)
        }
    };
    let total = hdr_len + payload_len;
    let raw = input
        .get(..total)
        .ok_or_else(|| MptError::new("rlp: item exceeds input"))?;
    let payload = &raw[hdr_len..];
    // Canonical single-byte string: a 1-byte string whose byte is < 0x80 must use the bare form.
    if (0x81..=0xb7).contains(&b0) && payload_len == 1 && payload[0] < 0x80 {
        return Err(MptError::new("rlp: non-canonical single-byte string"));
    }
    Ok((
        RlpItem {
            is_list,
            payload: payload.to_vec(),
            raw: raw.to_vec(),
        },
        &input[total..],
    ))
}

/// Decode an RLP list node into its items (rejects a non-list, trailing bytes, or a malformed
/// interior).
fn rlp_decode_list(node: &[u8]) -> Result<Vec<RlpItem>, MptError> {
    let (head, rest) = rlp_split(node)?;
    if !head.is_list {
        return Err(MptError::new("rlp: expected list node, got string"));
    }
    if !rest.is_empty() {
        return Err(MptError::new("rlp: trailing bytes after list node"));
    }
    let mut items = Vec::new();
    let mut buf = head.payload.as_slice();
    while !buf.is_empty() {
        let (item, next) = rlp_split(buf)?;
        items.push(item);
        buf = next;
    }
    Ok(items)
}

/// Decode a full `rlp(uint)` byte string into a `U256` (the storage leaf value form).
fn rlp_decode_uint256(rlp: &[u8]) -> Result<U256, MptError> {
    let (item, rest) = rlp_split(rlp)?;
    if item.is_list {
        return Err(MptError::new("rlp: expected integer string, got list"));
    }
    if !rest.is_empty() {
        return Err(MptError::new("rlp: trailing bytes after integer"));
    }
    be_uint256(item.payload.as_slice())
}

/// Big-endian minimal-encoding bytes → `U256` (rejects leading zeros and > 32 bytes).
fn be_uint256(bytes: &[u8]) -> Result<U256, MptError> {
    if bytes.len() > 32 {
        return Err(MptError::new("integer > 32 bytes"));
    }
    if bytes.len() > 1 && bytes[0] == 0 {
        return Err(MptError::new("non-canonical integer (leading zero)"));
    }
    Ok(U256::from_be_slice(bytes))
}

/// Big-endian minimal-encoding bytes → `u64` (rejects leading zeros and > 8 bytes).
fn be_u64(bytes: &[u8]) -> Result<u64, MptError> {
    if bytes.len() > 8 {
        return Err(MptError::new("u64 field > 8 bytes"));
    }
    if bytes.len() > 1 && bytes[0] == 0 {
        return Err(MptError::new("non-canonical u64 (leading zero)"));
    }
    let mut buf = [0u8; 8];
    buf[8 - bytes.len()..].copy_from_slice(bytes);
    Ok(u64::from_be_bytes(buf))
}

/// Exactly-32-byte string → `B256` (account storage/code hash fields).
fn be_b256(bytes: &[u8]) -> Result<B256, MptError> {
    if bytes.len() != 32 {
        return Err(MptError::new("hash field is not 32 bytes"));
    }
    Ok(B256::from_slice(bytes))
}

/// Decode a big-endian RLP length field (rejects leading zeros and lengths that overflow `usize`
/// meaningfully; caps at 4 bytes which is far beyond any node size).
fn be_len(bytes: &[u8]) -> Result<usize, MptError> {
    if bytes.is_empty() || bytes.len() > 4 {
        return Err(MptError::new("rlp: bad length field width"));
    }
    if bytes[0] == 0 {
        return Err(MptError::new("rlp: non-canonical length (leading zero)"));
    }
    let mut v = 0usize;
    for &b in bytes {
        v = (v << 8) | b as usize;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RLP builders (test-only) — construct trie nodes by hand for fixed vectors ───────────────

    /// RLP-encode a byte string.
    fn rlp_string(bytes: &[u8]) -> Vec<u8> {
        if bytes.len() == 1 && bytes[0] < 0x80 {
            return vec![bytes[0]];
        }
        let mut out = Vec::new();
        if bytes.len() <= 55 {
            out.push(0x80 + bytes.len() as u8);
        } else {
            let len = bytes.len();
            let len_be = len.to_be_bytes();
            let first = len_be.iter().position(|&b| b != 0).unwrap();
            let len_bytes = &len_be[first..];
            out.push(0xb7 + len_bytes.len() as u8);
            out.extend_from_slice(len_bytes);
        }
        out.extend_from_slice(bytes);
        out
    }

    /// RLP-encode a list from already-encoded items.
    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = items.concat();
        let mut out = Vec::new();
        if body.len() <= 55 {
            out.push(0xc0 + body.len() as u8);
        } else {
            let len_be = body.len().to_be_bytes();
            let first = len_be.iter().position(|&b| b != 0).unwrap();
            let len_bytes = &len_be[first..];
            out.push(0xf7 + len_bytes.len() as u8);
            out.extend_from_slice(len_bytes);
        }
        out.extend_from_slice(&body);
        out
    }

    /// Minimal big-endian bytes of a U256 (empty for zero) — the pre-RLP integer form.
    fn min_be(v: U256) -> Vec<u8> {
        let full = v.to_be_bytes::<32>();
        let first = full.iter().position(|&b| b != 0).unwrap_or(32);
        full[first..].to_vec()
    }

    /// Compact hex-prefix encode a nibble path (leaf or extension). Flag nibble: bit1 = leaf,
    /// bit0 = odd length (with the first path nibble packed into the flag byte's low 4 bits).
    fn compact(path: &[u8], is_leaf: bool) -> Vec<u8> {
        let flag = if is_leaf { 2u8 } else { 0u8 };
        let mut out = Vec::new();
        let rest_start = if path.len() % 2 == 1 {
            // Odd: flag byte carries (flag|odd) in the high nibble and path[0] in the low nibble.
            out.push(((flag | 1) << 4) | path[0]);
            1
        } else {
            // Even: flag byte is flag<<4 with a zero low (padding) nibble.
            out.push(flag << 4);
            0
        };
        let mut i = rest_start;
        while i < path.len() {
            out.push((path[i] << 4) | path[i + 1]);
            i += 2;
        }
        out
    }

    /// Build a leaf node for `path` (remaining nibbles) storing pre-RLP `value_bytes`.
    fn leaf_node(path: &[u8], value_bytes: &[u8]) -> Vec<u8> {
        rlp_list(&[rlp_string(&compact(path, true)), rlp_string(value_bytes)])
    }

    fn nibbles(key: B256) -> Vec<u8> {
        key_to_nibbles(key)
    }

    // ── Storage: single-leaf inclusion + tamper ─────────────────────────────────────────────────

    #[test]
    fn storage_single_leaf_verifies_and_rejects_tamper() {
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64); // 200 — exercises 0x81-prefixed integer
        let slot_key = keccak256(B256::from(slot).as_slice());
        // Leaf value stored in the storage trie is rlp(value); the leaf's 2nd element wraps it.
        let value_rlp = rlp_string(&min_be(value));
        let leaf = leaf_node(&nibbles(slot_key), &value_rlp);
        let root = keccak256(leaf.as_slice());

        let good = StorageProof {
            key: slot,
            value,
            proof: vec![leaf.clone()],
        };
        verify_storage_proof(&good, root).expect("known-good storage proof verifies");

        // Tamper 1: claim a different value → reject.
        let bad_val = StorageProof {
            key: slot,
            value: U256::from(0xc9u64),
            proof: vec![leaf.clone()],
        };
        assert!(verify_storage_proof(&bad_val, root).is_err(), "wrong value must be rejected");

        // Tamper 2: corrupt a proof byte → root no longer matches → reject.
        let mut corrupt = leaf.clone();
        let n = corrupt.len();
        corrupt[n - 1] ^= 0xff;
        let bad_proof = StorageProof {
            key: slot,
            value,
            proof: vec![corrupt],
        };
        assert!(
            verify_storage_proof(&bad_proof, root).is_err(),
            "corrupted node must break the hash chain"
        );

        // Tamper 3: wrong root → reject.
        assert!(
            verify_storage_proof(&good, B256::repeat_byte(0xAB)).is_err(),
            "wrong root must be rejected"
        );
    }

    #[test]
    fn storage_zero_value_small_int_verifies() {
        // value 42 → rlp(42) == 0x2a (bare byte) — exercises the single-byte value path.
        let slot = U256::from(123u64);
        let value = U256::from(42u64);
        let slot_key = keccak256(B256::from(slot).as_slice());
        let leaf = leaf_node(&nibbles(slot_key), &rlp_string(&min_be(value)));
        let root = keccak256(leaf.as_slice());
        verify_storage_proof(
            &StorageProof {
                key: slot,
                value,
                proof: vec![leaf],
            },
            root,
        )
        .expect("small-int storage value verifies");
    }

    // ── Storage: two-node branch + leaf (hash chain across nodes) ───────────────────────────────

    #[test]
    fn storage_branch_then_leaf_verifies_and_detects_chain_break() {
        // Find two slots whose hashed key paths differ in the FIRST nibble.
        let mut sa = 0u64;
        let mut sb = 1u64;
        loop {
            let ka = keccak256(B256::from(U256::from(sa)).as_slice());
            let kb = keccak256(B256::from(U256::from(sb)).as_slice());
            if key_to_nibbles(ka)[0] != key_to_nibbles(kb)[0] {
                break;
            }
            sb += 1;
            if sb > 5000 {
                sa += 1;
                sb = sa + 1;
            }
        }
        let slot_a = U256::from(sa);
        let slot_b = U256::from(sb);
        let val_a = U256::from(0x1111u64);
        let val_b = U256::from(0x2222u64);
        let ka = keccak256(B256::from(slot_a).as_slice());
        let kb = keccak256(B256::from(slot_b).as_slice());
        let na = key_to_nibbles(ka);
        let nb = key_to_nibbles(kb);

        // Each leaf consumes its path AFTER the branch's first nibble.
        let leaf_a = leaf_node(&na[1..], &rlp_string(&min_be(val_a)));
        let leaf_b = leaf_node(&nb[1..], &rlp_string(&min_be(val_b)));
        let ha = keccak256(leaf_a.as_slice());
        let hb = keccak256(leaf_b.as_slice());

        // Branch: 16 child slots + value slot; children at the two first-nibbles are the leaf hashes.
        let mut children: Vec<Vec<u8>> = (0..16).map(|_| rlp_string(&[])).collect();
        children[na[0] as usize] = rlp_string(ha.as_slice());
        children[nb[0] as usize] = rlp_string(hb.as_slice());
        children.push(rlp_string(&[])); // value slot (empty)
        let branch = rlp_list(&children);
        let root = keccak256(branch.as_slice());

        let proof_a = StorageProof {
            key: slot_a,
            value: val_a,
            proof: vec![branch.clone(), leaf_a.clone()],
        };
        verify_storage_proof(&proof_a, root).expect("branch→leaf proof for A verifies");

        let proof_b = StorageProof {
            key: slot_b,
            value: val_b,
            proof: vec![branch.clone(), leaf_b.clone()],
        };
        verify_storage_proof(&proof_b, root).expect("branch→leaf proof for B verifies");

        // Chain break: swap in B's leaf under A's key → the branch points at ha, not hb → reject.
        let mixed = StorageProof {
            key: slot_a,
            value: val_a,
            proof: vec![branch.clone(), leaf_b.clone()],
        };
        assert!(
            verify_storage_proof(&mixed, root).is_err(),
            "a leaf that does not match the branch reference must be rejected"
        );
    }

    // ── Storage: non-inclusion ──────────────────────────────────────────────────────────────────

    #[test]
    fn storage_non_inclusion_accepts_zero_rejects_nonzero() {
        // A leaf for a DIFFERENT key at the root: our key diverges → verified miss → zero only.
        let present_slot = U256::from(7u64);
        let present_key = keccak256(B256::from(present_slot).as_slice());
        let leaf = leaf_node(&nibbles(present_key), &rlp_string(&min_be(U256::from(9u64))));
        let root = keccak256(leaf.as_slice());

        // Query a different slot whose path diverges from present_key.
        let mut absent = 8u64;
        loop {
            let k = keccak256(B256::from(U256::from(absent)).as_slice());
            if key_to_nibbles(k)[0] != key_to_nibbles(present_key)[0] {
                break;
            }
            absent += 1;
        }
        let absent_slot = U256::from(absent);

        verify_storage_proof(
            &StorageProof {
                key: absent_slot,
                value: U256::ZERO,
                proof: vec![leaf.clone()],
            },
            root,
        )
        .expect("verified non-inclusion of a zero value");

        assert!(
            verify_storage_proof(
                &StorageProof {
                    key: absent_slot,
                    value: U256::from(1u64),
                    proof: vec![leaf],
                },
                root,
            )
            .is_err(),
            "non-inclusion cannot back a non-zero value"
        );
    }

    #[test]
    fn storage_empty_root_only_accepts_zero() {
        let sp0 = StorageProof {
            key: U256::from(3u64),
            value: U256::ZERO,
            proof: vec![],
        };
        verify_storage_proof(&sp0, EMPTY_ROOT_HASH).expect("empty root + zero value ok");
        verify_storage_proof(&sp0, B256::ZERO).expect("zero root + zero value ok");
        let sp_bad = StorageProof {
            key: U256::from(3u64),
            value: U256::from(5u64),
            proof: vec![],
        };
        assert!(
            verify_storage_proof(&sp_bad, EMPTY_ROOT_HASH).is_err(),
            "empty root cannot back a non-zero value"
        );
    }

    // ── [WP0 · A4] non-inclusion soundness: an empty proof under a NON-empty root proves nothing ──

    #[test]
    fn nonempty_root_empty_storage_proof_zero_is_rejected() {
        // WP0 RED-1 (Finding 6): a non-empty storage root with `value=0` and an EMPTY proof does NOT
        // prove non-inclusion — EIP-1186 requires the nodes root→last-matching-node. The pre-WP0
        // shortcut (`mpt_verifier.rs:163-169`) wrongly returned `Ok`; a real proof of absence must be
        // walked. `0xAB..` is neither `B256::ZERO` nor `EMPTY_ROOT_HASH`, so it is a non-empty root.
        let nonempty_root = B256::repeat_byte(0xAB);
        assert_ne!(nonempty_root, EMPTY_ROOT_HASH);
        let sp = StorageProof {
            key: U256::from(7u64),
            value: U256::ZERO,
            proof: vec![],
        };
        assert!(
            verify_storage_proof(&sp, nonempty_root).is_err(),
            "empty proof under a non-empty storage root must be rejected (it is not a proof of absence)"
        );
    }

    #[test]
    fn nonempty_root_absent_slot_via_branch_miss_still_verifies_zero() {
        // WP0 RED-2 (control): a WALKED proof of absence via a verified BRANCH miss (the queried
        // nibble lands on an empty child slot) must still succeed for a zero value — deleting the
        // unsound empty-proof shortcut must not over-reject a genuine walked non-inclusion. Existing
        // coverage exercises divergent-leaf absence; this exercises the branch-miss shape.
        let mut sa = 0u64;
        let mut sb = 1u64;
        loop {
            let ka = keccak256(B256::from(U256::from(sa)).as_slice());
            let kb = keccak256(B256::from(U256::from(sb)).as_slice());
            if key_to_nibbles(ka)[0] != key_to_nibbles(kb)[0] {
                break;
            }
            sb += 1;
            if sb > 5000 {
                sa += 1;
                sb = sa + 1;
            }
        }
        let present = U256::from(sa);
        let absent = U256::from(sb);
        let ka = keccak256(B256::from(present).as_slice());
        let na = key_to_nibbles(ka);
        // Branch node: only the present slot's first nibble carries a (leaf) child; every other child
        // slot — including the absent slot's first nibble — is empty, which IS the proof of absence.
        let leaf = leaf_node(&na[1..], &rlp_string(&min_be(U256::from(0x1234u64))));
        let hleaf = keccak256(leaf.as_slice());
        let mut children: Vec<Vec<u8>> = (0..16).map(|_| rlp_string(&[])).collect();
        children[na[0] as usize] = rlp_string(hleaf.as_slice());
        children.push(rlp_string(&[])); // value slot (empty)
        let branch = rlp_list(&children);
        let root = keccak256(branch.as_slice());

        // The branch node alone proves the absent slot's child slot is empty → verified miss → zero ok.
        verify_storage_proof(
            &StorageProof {
                key: absent,
                value: U256::ZERO,
                proof: vec![branch.clone()],
            },
            root,
        )
        .expect("verified branch-miss non-inclusion of a zero value must still succeed");

        // …but a non-zero value cannot ride a proof of absence.
        assert!(
            verify_storage_proof(
                &StorageProof {
                    key: absent,
                    value: U256::from(1u64),
                    proof: vec![branch],
                },
                root,
            )
            .is_err(),
            "a branch-miss cannot back a non-zero value"
        );
    }

    #[test]
    fn storage_inline_node_child_is_verified() {
        // WP0 A4 parity (inline node): a child whose RLP is < 32 bytes is embedded INLINE in its
        // parent (not referenced by hash and not a separate proof-list entry). Build a root EXTENSION
        // whose target is a tiny inline leaf, and prove the walker verifies through the inline hop
        // (ExpectedNode::Inline) without consuming a proof-list slot. This path was previously only
        // exercised indirectly.
        let slot = U256::from(7u64);
        let slot_key = keccak256(B256::from(slot).as_slice());
        let nib = nibbles(slot_key); // 64 nibbles
        let value = U256::from(5u64);
        // Tiny leaf for the LAST nibble only ⇒ its RLP is a few bytes (< 32) ⇒ inline-eligible.
        let inline_leaf = leaf_node(&nib[63..], &rlp_string(&min_be(value)));
        assert!(inline_leaf.len() < 32, "leaf must be inline-eligible (<32 bytes)");
        // Root extension: path = first 63 nibbles, target = the inline leaf (embedded list, not a hash).
        let ext = rlp_list(&[rlp_string(&compact(&nib[0..63], false)), inline_leaf.clone()]);
        let root = keccak256(ext.as_slice());
        verify_storage_proof(
            &StorageProof {
                key: slot,
                value,
                proof: vec![ext.clone()],
            },
            root,
        )
        .expect("a value proven through an inline node must verify");
        // The inline hop consumes no extra proof-list slot: a spurious trailing node is still rejected.
        assert!(
            verify_storage_proof(
                &StorageProof {
                    key: slot,
                    value,
                    proof: vec![ext.clone(), ext],
                },
                root,
            )
            .is_err(),
            "a trailing node after an inline-terminated walk must be rejected"
        );
    }

    #[test]
    fn storage_leaf_shorter_than_key_is_absent() {
        // WP0 A4 parity (full-key consumption): a leaf is accepted as PRESENT only if its path
        // consumes the ENTIRE remaining key. A leaf whose path is a proper prefix of the queried key
        // terminates a shorter key, so the queried key is ABSENT (zero only) — it can never back a
        // non-zero value. This directly exercises the `nibble_idx != key_nibbles.len()` guard.
        let slot = U256::from(7u64);
        let slot_key = keccak256(B256::from(slot).as_slice());
        let nib = nibbles(slot_key);
        // Leaf path = only the first 10 nibbles of the 64-nibble key ⇒ our full key is not consumed.
        let short_leaf = leaf_node(&nib[0..10], &rlp_string(&min_be(U256::from(9u64))));
        let root = keccak256(short_leaf.as_slice());
        verify_storage_proof(
            &StorageProof {
                key: slot,
                value: U256::ZERO,
                proof: vec![short_leaf.clone()],
            },
            root,
        )
        .expect("a leaf that does not consume the full key ⇒ absent ⇒ zero is valid");
        assert!(
            verify_storage_proof(
                &StorageProof {
                    key: slot,
                    value: U256::from(1u64),
                    proof: vec![short_leaf],
                },
                root,
            )
            .is_err(),
            "a partial-key leaf cannot back a non-zero value"
        );
    }

    #[test]
    fn storage_proof_with_trailing_unconsumed_node_is_rejected() {
        // WP0 RED-5 (part): a valid proof consumes exactly its hash-referenced nodes. A trailing,
        // never-walked node must be rejected (strictness + reduced DoS surface). Pre-WP0 the walker
        // stopped at the leaf and silently ignored anything after it.
        let slot = U256::from(7u64);
        let value = U256::from(0xc8u64);
        let slot_key = keccak256(B256::from(slot).as_slice());
        let leaf = leaf_node(&nibbles(slot_key), &rlp_string(&min_be(value)));
        let root = keccak256(leaf.as_slice());
        // Good single-leaf proof verifies…
        verify_storage_proof(
            &StorageProof {
                key: slot,
                value,
                proof: vec![leaf.clone()],
            },
            root,
        )
        .expect("baseline single-leaf proof verifies");
        // …but appending an extra, unconsumed node must fail.
        assert!(
            verify_storage_proof(
                &StorageProof {
                    key: slot,
                    value,
                    proof: vec![leaf.clone(), leaf],
                },
                root,
            )
            .is_err(),
            "a proof carrying a trailing unconsumed node must be rejected"
        );
    }

    #[test]
    fn storage_proof_noncanonical_leaf_value_is_rejected() {
        // WP0 RED-5 (part, storage value canonicality): a storage leaf whose stored value is a
        // NON-canonical RLP integer must be rejected end-to-end (the verifier decodes the leaf value
        // with the strict canonical reader, so a lenient "it would decode to 5" provider cannot slip
        // a mis-encoded word past the hash chain). Two shapes: a leading-zero multi-byte integer and
        // a non-minimal single-byte string encoding of zero.
        let slot = U256::from(7u64);
        let slot_key = keccak256(B256::from(slot).as_slice());
        // Leading-zero 2-byte integer `0x82 0x00 0x05` (would be 5 if we were lenient).
        for bad_value_bytes in [vec![0x82u8, 0x00, 0x05], vec![0x81u8, 0x00]] {
            let leaf = leaf_node(&nibbles(slot_key), &bad_value_bytes);
            let root = keccak256(leaf.as_slice());
            let sp = StorageProof {
                key: slot,
                value: U256::from(5u64),
                proof: vec![leaf],
            };
            assert!(
                verify_storage_proof(&sp, root).is_err(),
                "a non-canonically-encoded storage leaf value must be rejected: {bad_value_bytes:?}"
            );
        }
    }

    #[test]
    fn account_proof_with_trailing_unconsumed_node_is_rejected() {
        // WP0 RED-5 (part, account side): the same trailing-node strictness applies to account proofs.
        let address = [0x42u8; 20];
        let nonce = 5u64;
        let balance = U256::from(1_000_000u64);
        let storage_hash = B256::repeat_byte(0x11);
        let code_hash = B256::repeat_byte(0x22);
        let acct_rlp = rlp_list(&[
            rlp_string(&min_be(U256::from(nonce))),
            rlp_string(&min_be(balance)),
            rlp_string(storage_hash.as_slice()),
            rlp_string(code_hash.as_slice()),
        ]);
        let addr_key = keccak256(address.as_slice());
        let leaf = leaf_node(&nibbles(addr_key), &acct_rlp);
        let root = keccak256(leaf.as_slice());
        let mut good = AccountProof {
            address,
            balance,
            nonce,
            storage_hash,
            code_hash,
            account_proof: vec![leaf.clone()],
            storage_proof: vec![],
        };
        verify_account_proof(&good, root).expect("baseline single-leaf account proof verifies");
        good.account_proof.push(leaf); // trailing unconsumed node
        assert!(
            verify_account_proof(&good, root).is_err(),
            "an account proof carrying a trailing unconsumed node must be rejected"
        );
    }

    // ── Account: single-leaf inclusion + tamper ─────────────────────────────────────────────────

    #[test]
    fn account_single_leaf_verifies_and_rejects_tamper() {
        let address = [0x42u8; 20];
        let nonce = 5u64;
        let balance = U256::from(1_000_000u64);
        let storage_hash = B256::repeat_byte(0x11);
        let code_hash = B256::repeat_byte(0x22);
        let acct_rlp = rlp_list(&[
            rlp_string(&min_be(U256::from(nonce))),
            rlp_string(&min_be(balance)),
            rlp_string(storage_hash.as_slice()),
            rlp_string(code_hash.as_slice()),
        ]);
        let addr_key = keccak256(address.as_slice());
        let leaf = leaf_node(&nibbles(addr_key), &acct_rlp);
        let root = keccak256(leaf.as_slice());

        let good = AccountProof {
            address,
            balance,
            nonce,
            storage_hash,
            code_hash,
            account_proof: vec![leaf.clone()],
            storage_proof: vec![],
        };
        verify_account_proof(&good, root).expect("known-good account proof verifies");

        // Tamper: claim a different balance than the leaf encodes → reject.
        let mut bad = good.clone();
        bad.balance = U256::from(999u64);
        assert!(verify_account_proof(&bad, root).is_err(), "wrong balance rejected");

        // Tamper: claim a different storage_hash → reject (this is the anchor for storage proofs).
        let mut bad_sh = good.clone();
        bad_sh.storage_hash = B256::repeat_byte(0x33);
        assert!(
            verify_account_proof(&bad_sh, root).is_err(),
            "wrong storage_hash rejected (would poison storage verification)"
        );
    }

    #[test]
    fn account_non_inclusion_only_accepts_empty_account() {
        let present = [0x01u8; 20];
        let acct_rlp = rlp_list(&[
            rlp_string(&min_be(U256::from(1u64))),
            rlp_string(&min_be(U256::from(2u64))),
            rlp_string(B256::repeat_byte(0x11).as_slice()),
            rlp_string(B256::repeat_byte(0x22).as_slice()),
        ]);
        let present_key = keccak256(present.as_slice());
        let leaf = leaf_node(&nibbles(present_key), &acct_rlp);
        let root = keccak256(leaf.as_slice());

        // An address whose path diverges from `present` at nibble 0.
        let mut a = [0u8; 20];
        for cand in 0u8..=255 {
            a = [cand; 20];
            let k = keccak256(a.as_slice());
            if key_to_nibbles(k)[0] != key_to_nibbles(present_key)[0] {
                break;
            }
        }
        // Empty account (all-zero/empty fields) → verified absence ok.
        let empty = AccountProof {
            address: a,
            balance: U256::ZERO,
            nonce: 0,
            storage_hash: EMPTY_ROOT_HASH,
            code_hash: EMPTY_CODE_HASH,
            account_proof: vec![leaf.clone()],
            storage_proof: vec![],
        };
        verify_account_proof(&empty, root).expect("verified absence of an empty account");

        // Non-empty claim on a proven-absent account → reject.
        let mut nonempty = empty.clone();
        nonempty.nonce = 1;
        assert!(
            verify_account_proof(&nonempty, root).is_err(),
            "absence cannot back a non-empty account"
        );
    }

    // ── RLP reader units ────────────────────────────────────────────────────────────────────────

    #[test]
    fn rlp_rejects_trailing_and_noncanonical() {
        // Trailing bytes after a list node.
        let mut node = leaf_node(&[1, 2, 3, 4], &rlp_string(&[0x05]));
        node.push(0xff);
        assert!(rlp_decode_list(&node).is_err(), "trailing bytes rejected");

        // Non-canonical integer (leading zero) via a 2-byte string 0x00 0x05.
        let noncanon = vec![0x82, 0x00, 0x05];
        assert!(rlp_decode_uint256(&noncanon).is_err(), "leading-zero integer rejected");

        // Non-canonical single-byte string (0x81 0x2a should have been bare 0x2a).
        let noncanon_sb = vec![0x81, 0x2a];
        assert!(rlp_split(&noncanon_sb).is_err(), "non-canonical single-byte string rejected");
    }

    #[test]
    fn rlp_uint_roundtrips_small_and_large() {
        for v in [0u64, 1, 42, 200, 255, 256, 65535, u64::MAX] {
            let rlp = rlp_string(&min_be(U256::from(v)));
            assert_eq!(rlp_decode_uint256(&rlp).unwrap(), U256::from(v), "value {v}");
        }
    }

    #[test]
    fn compact_hex_roundtrip_even_and_odd() {
        for path in [
            vec![],
            vec![1u8],
            vec![1u8, 2],
            vec![0xa, 0xb, 0xc],
            vec![0xf, 0x0, 0x1, 0x2, 0x3],
        ] {
            for leaf in [false, true] {
                let enc = compact(&path, leaf);
                let (got_leaf, got_path) = decode_compact_hex(&enc).unwrap();
                assert_eq!(got_leaf, leaf, "leaf flag for {path:?}");
                assert_eq!(got_path, path, "path for {path:?} leaf={leaf}");
            }
        }
    }
}
