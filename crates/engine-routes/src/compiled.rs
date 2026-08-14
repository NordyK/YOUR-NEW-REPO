//! PB-PATHS — cold-path compiled route-table artifact.
//!
//! The enumerated route table (the cyclic "paths") is a *pure* function of the discovered
//! pool set, the token set, and the enumeration topology config. The DFS enumeration that
//! produces it is the costly, deterministic part of boot; re-running it every start is
//! wasted work and makes the route set non-deterministic across runs.
//!
//! This module persists the route table to one versioned, fingerprinted, checksummed
//! binary artifact (bincode body, atomic temp+rename write) and loads it at boot — but
//! ONLY if the artifact's stored fingerprint equals the *current* universe's fingerprint
//! and the format version + body checksum validate. Any mismatch / corruption / missing
//! file returns `None`, so the caller rebuilds from scratch and a stale artifact can never
//! silently mis-route a changed universe.
//!
//! NOTE: only the route *table* (topology) is cached — NOT the pool→route CSR. The CSR is
//! score-ordered using live per-pool volume seeds that vary run-to-run, so it is cheap to
//! rebuild and is always rebuilt fresh at boot. Pattern copied from Tigger's
//! `compiled_graph.rs` (magic/version/checksum/atomic-write) + `cache_provenance.rs`
//! (input-fingerprint gate), adapted to a bincode body so the schema rides the `Route`
//! serde derives.

use crate::table::RouteTable;
use std::io::Write;
use std::path::Path;

/// Artifact magic. Bumping magic/version/body-schema invalidates old files.
const MAGIC: &[u8; 8] = b"MNTSROUT";
/// On-disk format version (header layout + body schema generation).
const FORMAT_VERSION: u32 = 1;
/// magic(8) + version(4) + fingerprint(8) + checksum(8) + body_len(8).
const HEADER_LEN: usize = 8 + 4 + 8 + 8 + 8;

/// FNV-1a 64-bit over the body bytes — corruption detection beyond what the atomic
/// temp+rename write already prevents. Deterministic across builds (unlike a seeded hash).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The serialized payload: the route table the boot would otherwise re-enumerate.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct CompiledBody {
    route_table: RouteTable,
}

/// A persisted, fingerprint-gated compiled route table (PB-PATHS).
pub struct CompiledRouteGraph {
    body: CompiledBody,
}

impl CompiledRouteGraph {
    /// Wrap an in-memory route table for persistence.
    pub fn new(route_table: RouteTable) -> Self {
        Self {
            body: CompiledBody { route_table },
        }
    }

    /// The route table.
    pub fn route_table(&self) -> &RouteTable {
        &self.body.route_table
    }

    /// Consume into the owned route table for boot wiring.
    pub fn into_route_table(self) -> RouteTable {
        self.body.route_table
    }

    /// Atomically write the artifact, stamping `fingerprint` (the hash of the universe it
    /// was built for) into the header. Temp file + `rename` so a crash mid-write never
    /// leaves a torn artifact.
    pub fn save(&self, path: &Path, fingerprint: u64) -> std::io::Result<()> {
        let body = bincode::serde::encode_to_vec(&self.body, bincode::config::standard())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let checksum = fnv1a_64(&body);
        let mut buf = Vec::with_capacity(HEADER_LEN + body.len());
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&fingerprint.to_le_bytes());
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf.extend_from_slice(&(body.len() as u64).to_le_bytes());
        buf.extend_from_slice(&body);

        let mut tmp = path.to_path_buf();
        tmp.set_extension(format!("tmp{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Load the artifact IFF magic + version + body checksum validate AND the stored
    /// fingerprint equals `expected_fingerprint`. Any mismatch / corruption / missing file
    /// returns `None` so the caller rebuilds — a stale artifact can never mis-route.
    pub fn load(path: &Path, expected_fingerprint: u64) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() < HEADER_LEN || &bytes[0..8] != MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        if version != FORMAT_VERSION {
            return None;
        }
        let fingerprint = u64::from_le_bytes(bytes[12..20].try_into().ok()?);
        if fingerprint != expected_fingerprint {
            return None;
        }
        let checksum = u64::from_le_bytes(bytes[20..28].try_into().ok()?);
        let body_len = u64::from_le_bytes(bytes[28..36].try_into().ok()?) as usize;
        let body = bytes.get(HEADER_LEN..HEADER_LEN.checked_add(body_len)?)?;
        if fnv1a_64(body) != checksum {
            return None;
        }
        let (decoded, _): (CompiledBody, usize) =
            bincode::serde::decode_from_slice(body, bincode::config::standard()).ok()?;
        Some(Self { body: decoded })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Hop, Route, RouteTable, StrategyKind};
    use arrayvec::ArrayVec;
    use engine_state::{PoolId, RouteId, TokenId};

    fn mk_route(hops: u8, hot: bool) -> Route {
        let mut hops_arr = ArrayVec::new();
        let mut tokens = ArrayVec::new();
        tokens.push(TokenId(0));
        for i in 0..hops {
            hops_arr.push(Hop::new(PoolId(i as u32), (i & 1) as u8));
            tokens.push(TokenId(i as u16 + 1));
        }
        *tokens.last_mut().unwrap() = TokenId(0);
        Route {
            id: RouteId(0),
            strategy: if hops == 2 {
                StrategyKind::TwoHop
            } else {
                StrategyKind::Triangle
            },
            hop_count: hops,
            hops: hops_arr,
            tokens,
            family_signature: 7,
            calldata_template_id: 3,
            gas_template_id: 2,
            l1_size_bucket_template: 1,
            amount_grid_id: 4,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 99,
            hot,
        }
    }

    fn sample() -> RouteTable {
        let mut rt = RouteTable::new();
        rt.insert(mk_route(2, true));
        rt.insert(mk_route(3, true));
        rt.insert(mk_route(2, false));
        rt
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mantis_pbpaths_{}_{}.bin",
            std::process::id(),
            name
        ))
    }

    #[test]
    fn save_load_roundtrip_preserves_route_table() {
        let rt = sample();
        let path = tmp("roundtrip");
        CompiledRouteGraph::new(rt.clone())
            .save(&path, 0x00AB_CDEF)
            .expect("save ok");
        let loaded =
            CompiledRouteGraph::load(&path, 0x00AB_CDEF).expect("matching fingerprint must load");
        assert_eq!(loaded.route_table(), &rt);
        assert_eq!(loaded.into_route_table(), rt);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fingerprint_mismatch_refuses_load_so_caller_rebuilds() {
        // THE capital-safety contract: a graph built for a DIFFERENT universe (different
        // fingerprint) must NOT load — else we would rank/route against stale topology.
        let path = tmp("fp_mismatch");
        CompiledRouteGraph::new(sample())
            .save(&path, 0x1111_1111)
            .expect("save ok");
        assert!(CompiledRouteGraph::load(&path, 0x2222_2222).is_none());
        assert!(CompiledRouteGraph::load(&path, 0x1111_1111).is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupted_body_fails_checksum_and_refuses_load() {
        let path = tmp("corrupt");
        CompiledRouteGraph::new(sample())
            .save(&path, 7)
            .expect("save ok");
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF; // flip a body byte
        std::fs::write(&path, &bytes).unwrap();
        assert!(CompiledRouteGraph::load(&path, 7).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_magic_or_short_or_missing_refuses_load() {
        let path = tmp("badmagic");
        std::fs::write(&path, b"NOTMAGIC\x00\x00\x00\x00garbagegarbage").unwrap();
        assert!(CompiledRouteGraph::load(&path, 0).is_none());
        std::fs::write(&path, b"MNTS").unwrap(); // shorter than header
        assert!(CompiledRouteGraph::load(&path, 0).is_none());
        let _ = std::fs::remove_file(&path);
        assert!(CompiledRouteGraph::load(&path, 0).is_none()); // missing
    }

    /// Manual: measure load time of a real persisted cache produced by a live boot.
    /// `PB_PATHS_REAL_CACHE=<path> PB_PATHS_FP=<fp> cargo test -p engine-routes --release \
    ///   measure_real_cache_load -- --ignored --nocapture`
    #[test]
    #[ignore = "manual measurement against a real boot-produced cache"]
    fn measure_real_cache_load() {
        let path = std::env::var("PB_PATHS_REAL_CACHE").expect("set PB_PATHS_REAL_CACHE");
        let fp: u64 = std::env::var("PB_PATHS_FP")
            .expect("set PB_PATHS_FP")
            .parse()
            .unwrap();
        let mut best = f64::INFINITY;
        let mut routes = 0usize;
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let g = CompiledRouteGraph::load(std::path::Path::new(&path), fp)
                .expect("real cache must load with the matching fingerprint");
            best = best.min(t0.elapsed().as_secs_f64() * 1000.0);
            routes = g.route_table().len();
            std::hint::black_box(&g);
        }
        eprintln!("PB-PATHS real-cache load: {routes} routes, best-of-5 = {best:.2} ms");
    }
}
