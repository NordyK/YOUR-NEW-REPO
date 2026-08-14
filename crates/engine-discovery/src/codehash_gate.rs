//! §5.4 gate 2 / §5.6: runtime-bytecode codehash validation.
//!
//! For every discovered pool, fetch its deployed bytecode (`eth_getCode`), keccak256 it,
//! and assert the hash matches the per-family allowlist. A mismatch demotes the pool to
//! `ResearchOnly` (we just drop it from the live universe). This catches the §5.4 step 2
//! failure mode where a factory `PoolCreated` event points at a non-canonical clone or
//! a proxy whose implementation we have not vetted.
//!
//! The allowlist is keyed by `PoolKind` so a Uniswap V3 pool clone (Sushi V3, Pancake V3
//! all share the V3 codehash family) maps to its expected runtime hash. A zero hash in
//! the allowlist means "skip this gate for this family" (used only during initial
//! research-pass discovery to seed the hash artifacts under `research/dex/<family>/`).

use ahash::AHashMap;
use engine_rpc::{ArbitrumRpcClient, BlockTag};
use engine_state::PoolKind;
use tracing::warn;

use crate::types::{Address, DiscoveryError};

/// Pure core of the STRICT_CODEHASH decision (unit-tested): fail-closed for unseeded families iff
/// the env value is a recognized truthy flag.
fn strict_codehash_from_env(val: Option<&str>) -> bool {
    matches!(
        val.map(|s| s.trim()),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("on")
    )
}

/// Whether `STRICT_CODEHASH` is enabled (env read once, cached). When on, an unseeded family is
/// rejected by [`validate_pool_codehash`] instead of admitted.
fn strict_codehash_enabled() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| strict_codehash_from_env(std::env::var("STRICT_CODEHASH").ok().as_deref()))
}

/// Per-family codehash allowlist. A pool passes if its runtime hash equals **any** of the
/// hashes for its family — V3 pools deployed from different factories share the same
/// proxy bytecode but Uniswap V3 / Sushi V3 / Pancake V3 each ships its own variant.
#[derive(Debug, Clone, Default)]
pub struct CodehashAllowlist {
    inner: AHashMap<PoolKind, Vec<[u8; 32]>>,
}

impl CodehashAllowlist {
    /// New empty allowlist. Every family added via `add` becomes mandatory; families
    /// with no hashes recorded are treated as "skip-codehash" (research mode).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an allowed hash for a family.
    pub fn add(&mut self, family: PoolKind, hash: [u8; 32]) {
        self.inner.entry(family).or_default().push(hash);
    }

    /// Does the allowlist consider this `(family, hash)` pair acceptable?
    /// Returns true if the family has no recorded hashes (skip mode) **or** the hash
    /// matches any recorded entry.
    pub fn accepts(&self, family: PoolKind, hash: &[u8; 32]) -> bool {
        match self.inner.get(&family) {
            None => true, // no hashes recorded → skip gate for this family
            Some(allowed) => allowed.iter().any(|h| h == hash),
        }
    }

    /// Whether the family has recorded hashes (i.e. the gate is enforcing for it).
    pub fn enforces(&self, family: PoolKind) -> bool {
        self.inner
            .get(&family)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Among `live_families`, the de-duplicated subset with NO recorded codehash. In enforce
    /// mode `accepts()` is fail-OPEN for these (it returns true for any unregistered family), so
    /// the caller should `warn!` each one — the operator otherwise has no signal that a live venue
    /// lacks bytecode identity. Pure (no I/O) so it is unit-tested directly. Order follows
    /// `live_families`, first occurrence only.
    pub fn unseeded_families(&self, live_families: &[PoolKind]) -> Vec<PoolKind> {
        let mut out: Vec<PoolKind> = Vec::new();
        for &f in live_families {
            if !self.enforces(f) && !out.contains(&f) {
                out.push(f);
            }
        }
        out
    }
}

/// Validate a single pool's runtime bytecode hash against the family allowlist.
/// Returns `Ok(())` on pass, `Err` on RPC failure, `Ok(())` on "skip mode" (no hashes
/// recorded for the family — caller MUST check `allowlist.enforces(family)` for full
/// guarantees).
pub fn validate_pool_codehash(
    client: &ArbitrumRpcClient,
    pool: &Address,
    family: PoolKind,
    allowlist: &CodehashAllowlist,
) -> Result<bool, DiscoveryError> {
    if !allowlist.enforces(family) {
        // AUD-CODEHASH-STRICT: default = fail-OPEN for an unseeded family (admit + the caller
        // `warn!`s so the operator can seed `research/dex/<family>/runtime.hash`). Setting
        // `STRICT_CODEHASH=1` flips this to fail-CLOSED: an unseeded family is REJECTED so no pool
        // is ever admitted with unverified bytecode identity before live capital.
        if strict_codehash_enabled() {
            warn!(
                ?family,
                "STRICT_CODEHASH: rejecting pool of an unseeded family (no recorded runtime.hash)"
            );
            return Ok(false);
        }
        return Ok(true);
    }
    let code = client.get_code(pool, BlockTag::Latest)?;
    if code.is_empty() {
        warn!(
            ?pool,
            ?family,
            "validate_pool_codehash: empty bytecode (EOA?)"
        );
        return Ok(false);
    }
    let mut k = tiny_keccak::Keccak::v256();
    use tiny_keccak::Hasher;
    k.update(&code);
    let mut hash = [0u8; 32];
    k.finalize(&mut hash);
    Ok(allowlist.accepts(family, &hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_does_not_enforce() {
        let a = CodehashAllowlist::new();
        assert!(!a.enforces(PoolKind::UniswapV3));
        // accepts() returns true for any unregistered family (skip mode).
        assert!(a.accepts(PoolKind::UniswapV3, &[0u8; 32]));
    }

    #[test]
    fn registered_family_enforces() {
        let mut a = CodehashAllowlist::new();
        a.add(PoolKind::UniswapV3, [0x11; 32]);
        assert!(a.enforces(PoolKind::UniswapV3));
        assert!(a.accepts(PoolKind::UniswapV3, &[0x11; 32]));
        assert!(!a.accepts(PoolKind::UniswapV3, &[0x22; 32]));
        // Other families are unaffected.
        assert!(a.accepts(PoolKind::UniswapV2, &[0xAA; 32]));
    }

    #[test]
    fn unseeded_families_reports_only_unrecorded() {
        // Seed V3 only; V2 and Curve are live but unseeded → fail-open under enforce, must report.
        let mut a = CodehashAllowlist::new();
        a.add(PoolKind::UniswapV3, [0x11; 32]);
        let live = [
            PoolKind::UniswapV3,
            PoolKind::UniswapV2,
            PoolKind::CurveStableSwapNg,
            PoolKind::UniswapV2, // duplicate must collapse
        ];
        let unseeded = a.unseeded_families(&live);
        assert_eq!(
            unseeded,
            vec![PoolKind::UniswapV2, PoolKind::CurveStableSwapNg],
            "only unseeded families, de-duplicated, in input order"
        );
        // A fully-seeded live set reports nothing.
        assert!(a.unseeded_families(&[PoolKind::UniswapV3]).is_empty());
    }
}

#[cfg(test)]
mod strict_mode_tests {
    //! AUD-CODEHASH-STRICT: the STRICT_CODEHASH flag parse is fail-closed only on truthy values.
    use super::strict_codehash_from_env;
    #[test]
    fn strict_flag_parses_truthy_only() {
        assert!(strict_codehash_from_env(Some("1")));
        assert!(strict_codehash_from_env(Some("true")));
        assert!(strict_codehash_from_env(Some(" on ")));
        assert!(!strict_codehash_from_env(Some("0")));
        assert!(!strict_codehash_from_env(Some("")));
        assert!(!strict_codehash_from_env(None), "default is fail-open (backward compatible)");
    }
}
