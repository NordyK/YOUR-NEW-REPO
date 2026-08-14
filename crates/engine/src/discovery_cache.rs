//! On-disk discovery cache.
//!
//! Re-scanning every factory from its deployment block on each boot costs minutes of
//! chunked `eth_getLogs` (and gets throttled by public RPCs). This cache persists the full
//! discovered pool set (plus the V4 synthetic-addr → poolId map) keyed by a hash of the
//! discovery-relevant config. On the next boot, if the config is unchanged, startup seeds
//! from the cache and scans only `[cached_head+1, head]` — seconds instead of minutes.
//!
//! Safety: the cache is keyed by `config_hash` (venues + token allowlist), so editing the
//! venue list or token set invalidates it and forces a full rescan. The file is written
//! atomically (tmp + rename) so a crash mid-write can't leave a torn cache.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use ahash::AHashSet;
use anyhow::Result;
use engine_config::VenueEntry;
use engine_discovery::DiscoveredPool;
use serde::{Deserialize, Serialize};

/// Bump when the on-disk schema changes in a NON-backward-compatible way (invalidates all old
/// caches). Adding a new `#[serde(default)]` field is backward compatible and does NOT need a bump:
/// old caches load with the field defaulted, and the next live-RPC boot re-saves with it populated.
const CACHE_VERSION: u32 = 1;
/// Default cache path; overridable via `DISCOVERY_CACHE` (set empty to disable).
const DEFAULT_CACHE_PATH: &str = "discovery_cache.json";

/// Persisted discovery result.
#[derive(Serialize, Deserialize)]
pub struct DiscoveryCache {
    /// Schema version.
    pub version: u32,
    /// Hash of the discovery-relevant config; mismatch ⇒ ignore the cache.
    pub config_hash: u64,
    /// Block the cached set was scanned up to (incremental scans resume from here + 1).
    pub head_block: u64,
    /// All discovered pools across every venue.
    pub pools: Vec<DiscoveredPool>,
    /// V4 synthetic-address → 32-byte poolId map (needed for V4 StateView snapshots).
    pub v4_pool_ids: Vec<([u8; 20], [u8; 32])>,
    /// Resolver-discovered Fluid DEX pools (`PoolKind::FluidDex`). Fluid is enumerated via the
    /// `DexReservesResolver`/`DexResolver` contracts rather than the per-venue log scan, so its
    /// pools never flowed into `pools` and were lost on every reboot — leaving offline/replay and
    /// cold-cache boots (no live RPC to re-run the resolver) Fluid-blind. Persisting the discovered
    /// identities here lets startup seed Fluid routes from the cache when no RPC is available.
    /// `#[serde(default)]` keeps older v1 caches (written before this field existed) loadable.
    #[serde(default)]
    pub fluid_pools: Vec<DiscoveredPool>,
}

impl DiscoveryCache {
    /// Build a cache record stamped with the current schema version.
    pub fn new(
        config_hash: u64,
        head_block: u64,
        pools: Vec<DiscoveredPool>,
        v4_pool_ids: Vec<([u8; 20], [u8; 32])>,
    ) -> Self {
        Self {
            version: CACHE_VERSION,
            config_hash,
            head_block,
            pools,
            v4_pool_ids,
            fluid_pools: Vec::new(),
        }
    }

    /// Resolve the cache path from `DISCOVERY_CACHE` (default `discovery_cache.json`). Returns
    /// `None` if the env var is set to empty (caching disabled).
    pub fn path() -> Option<PathBuf> {
        match std::env::var("DISCOVERY_CACHE") {
            Ok(s) if s.is_empty() => None,
            Ok(s) => Some(PathBuf::from(s)),
            Err(_) => Some(PathBuf::from(DEFAULT_CACHE_PATH)),
        }
    }

    /// Load + validate a cache for `config_hash`. Returns `None` on missing/corrupt/stale.
    pub fn load(path: &Path, config_hash: u64) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let cache: DiscoveryCache = serde_json::from_slice(&bytes).ok()?;
        if cache.version != CACHE_VERSION || cache.config_hash != config_hash {
            return None;
        }
        Some(cache)
    }

    /// Atomically write the cache (tmp file + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// WI-02c: the POLICY-AWARE cache key. A narrow-family boot filters its working view AND
/// persists it back at an advanced head, so a policy-blind key would let that narrowed
/// cache lineage overwrite/serve a later broadened boot and permanently lose historical +
/// scan-window coverage (codex WI02B_VERIFY blocker 1). Folding the active-family tokens
/// into the key gives every ACTIVE_FAMILIES profile its own cache lineage: a broadened boot
/// can never hit a narrower boot's cache — it misses and performs the full rescan. The
/// tokens arrive in canonical `FamilyBitSet::tokens()` enum order, so the key is
/// deterministic per profile.
pub fn policy_aware_config_hash(
    venues: &[VenueEntry],
    token_allowlist: &AHashSet<[u8; 20]>,
    active_family_tokens: &[&str],
) -> u64 {
    let mut h = DefaultHasher::new();
    config_hash(venues, token_allowlist).hash(&mut h);
    // Length-prefixed fold so token-list boundaries can never alias.
    active_family_tokens.len().hash(&mut h);
    for t in active_family_tokens {
        t.hash(&mut h);
    }
    h.finish()
}

/// Deterministic hash of the discovery-relevant config. Any change to the venue set (family,
/// factory, deployment block, default fee) or the token allowlist invalidates the cache.
pub fn config_hash(venues: &[VenueEntry], token_allowlist: &AHashSet<[u8; 20]>) -> u64 {
    let mut h = DefaultHasher::new();
    CACHE_VERSION.hash(&mut h);
    for v in venues {
        v.family.hash(&mut h);
        v.factory.hash(&mut h);
        v.deployment_block.hash(&mut h);
        v.default_fee_bps.hash(&mut h);
    }
    // Sort token addresses for order-independence.
    let mut toks: Vec<[u8; 20]> = token_allowlist.iter().copied().collect();
    toks.sort_unstable();
    for t in toks {
        t.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_discovery::DiscoveredPool;
    use engine_state::PoolKind;

    fn sample_pool(addr: u8) -> DiscoveredPool {
        DiscoveredPool {
            address: [addr; 20],
            family: PoolKind::UniswapV3,
            token0: [1; 20],
            token1: [2; 20],
            fee_pips: 500,
            fee_bps: 0,
            tick_spacing: 10,
            created_block: 123,
        }
    }

    fn sample_fluid(addr: u8) -> DiscoveredPool {
        DiscoveredPool {
            address: [addr; 20],
            family: PoolKind::FluidDex,
            token0: [3; 20],
            token1: [4; 20],
            fee_pips: 100,
            fee_bps: 0,
            tick_spacing: 0,
            created_block: 999,
        }
    }

    #[test]
    fn config_hash_changes_with_tokens_stable_with_order() {
        let mut a = AHashSet::default();
        a.insert([1u8; 20]);
        a.insert([2u8; 20]);
        let mut b = AHashSet::default();
        b.insert([2u8; 20]); // inserted in the other order
        b.insert([1u8; 20]);
        assert_eq!(
            config_hash(&[], &a),
            config_hash(&[], &b),
            "order-independent"
        );
        let mut c = a.clone();
        c.insert([3u8; 20]);
        assert_ne!(
            config_hash(&[], &a),
            config_hash(&[], &c),
            "token change invalidates"
        );
    }

    #[test]
    fn save_load_roundtrip_and_staleness() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("disc_cache_test_{}.json", std::process::id()));
        let mut cache = DiscoveryCache::new(
            0xdead_beef,
            1000,
            vec![sample_pool(0xaa)],
            vec![([0x11; 20], [0x22; 32])],
        );
        cache.fluid_pools = vec![sample_fluid(0xbb)];
        cache.save(&path).expect("save");

        // Correct hash → loads with all data intact.
        let loaded = DiscoveryCache::load(&path, 0xdead_beef).expect("load");
        assert_eq!(loaded.head_block, 1000);
        assert_eq!(loaded.pools.len(), 1);
        assert_eq!(loaded.pools[0].address, [0xaa; 20]);
        assert_eq!(loaded.v4_pool_ids, vec![([0x11; 20], [0x22; 32])]);
        // Fluid pools round-trip so offline/cold-cache boots can re-register Fluid routes.
        assert_eq!(loaded.fluid_pools.len(), 1);
        assert_eq!(loaded.fluid_pools[0].address, [0xbb; 20]);
        assert_eq!(loaded.fluid_pools[0].family, PoolKind::FluidDex);

        // Wrong config hash → rejected (forces full rescan).
        assert!(
            DiscoveryCache::load(&path, 0x1234).is_none(),
            "stale config must be rejected"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A cache written by an older binary (before `fluid_pools` existed) must still load: the
    /// `#[serde(default)]` field defaults to empty rather than failing the whole parse. This is
    /// what avoids a forced full rescan on the first boot after the field was added.
    #[test]
    fn loads_legacy_cache_without_fluid_field() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("disc_cache_legacy_{}.json", std::process::id()));
        // Hand-write a v1-shaped cache JSON with NO `fluid_pools` key.
        let legacy = serde_json::json!({
            "version": CACHE_VERSION,
            "config_hash": 0xfeedu64,
            "head_block": 42u64,
            "pools": [],
            "v4_pool_ids": [],
        });
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let loaded = DiscoveryCache::load(&path, 0xfeed).expect("legacy cache must load");
        assert_eq!(loaded.head_block, 42);
        assert!(
            loaded.fluid_pools.is_empty(),
            "missing fluid_pools must default to empty, not fail the parse"
        );

        let _ = std::fs::remove_file(&path);
    }
}
