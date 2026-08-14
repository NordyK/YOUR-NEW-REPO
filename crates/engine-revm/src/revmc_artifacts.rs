//! Persistent revmc AOT artifact cache (Stage 3 · 2026-07-11), feature-gated behind `revmc`.
//!
//! revmc ships a [`RuntimeArtifactStore`](revmc::runtime::RuntimeArtifactStore) that writes
//! compiled dylibs into a `tempfile::TempDir` and keeps the index in RAM — so it is scoped to a
//! single process and every reboot re-runs the (expensive) LLVM AOT compile of the whole standing
//! manifest. On the 7-8 min-warmup AWS box that boot cost is the gate.
//!
//! [`PersistentArtifactStore`] is the durable analog: it writes each compiled `.so` plus a tiny
//! sidecar manifest under `REVMC_ARTIFACT_DIR/<namespace>/`, and reloads them on the next boot so
//! LLVM re-compiles are skipped. This is the Mantis port of Fulcrum's `CompiledCodeKey`
//! (compiler.rs — chain/arbos/spec/code_hash + revm_version + revm_compiler_source + target +
//! cpu identity) and Shadow's `AotArtifactMetadata` loader with its stale ⇒ recompile-never-load
//! rule.
//!
//! ## Identity (the "never share artifacts across engines/reboots-that-changed" contract)
//! revmc's own [`ArtifactKey`](revmc::runtime::ArtifactKey) only carries `{code_hash, spec_id,
//! backend, opt_level}` — it does NOT encode the revmc revision, the target arch, or the target
//! CPU. A dylib compiled by a different revmc rev, or for a different CPU, is ABI-incompatible and
//! must never be dlopen'd. We put that extra identity in the on-disk NAMESPACE directory:
//! `v{CACHE_VERSION}__{REVMC_REV}__{ARCH}__{TARGET_CPU}`. A mismatch on any of those ⇒ a different
//! directory ⇒ the old artifacts are simply invisible ⇒ they are recompiled into the new
//! namespace. Nothing stale is ever loaded. On top of that, every load re-hashes the `.so` bytes
//! and drops the entry if the digest does not match the sidecar (on-disk corruption / truncation).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alloy_primitives::{B256, keccak256};
use revmc::OptimizationLevel;
use revmc::runtime::{
    ArtifactKey, ArtifactManifest, ArtifactStore, BackendSelection, RuntimeCacheKey, StoredArtifact,
};
use revm::primitives::hardfork::SpecId;

/// Bump when the on-disk sidecar format changes so old caches are ignored (new namespace).
const CACHE_VERSION: u32 = 1;

/// The revmc git revision this crate is pinned to — MUST be kept in lock-step with the
/// `rev = "…"` in `engine-revm/Cargo.toml`. It namespaces the cache so an artifact compiled by a
/// different revmc build is never loaded (its ABI would not match). Only the short prefix is used
/// in the directory name (uniqueness is plenty at 12 hex chars).
pub const REVMC_REV: &str = "b5b60c1491c4cda92626e01e8e838bf29e53736a";

/// Sidecar file magic/version line — a load that does not see this exact first line is skipped.
const SIDECAR_MAGIC: &str = "revmc-artifact-v1";

/// The target-CPU identity for the cache namespace. Operators building with `-C target-cpu=native`
/// (or a specific `-mcpu` on Graviton) set `REVMC_TARGET_CPU` to that name so a `native` laptop
/// build and a `neoverse-v2` box build never collide. Absent ⇒ `"generic"`.
fn target_cpu() -> String {
    std::env::var("REVMC_TARGET_CPU")
        .ok()
        .map(|s| sanitize(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "generic".to_string())
}

/// The namespace subdirectory encoding the full artifact identity revmc's own key omits.
fn namespace() -> String {
    format!(
        "v{CACHE_VERSION}__{}__{}__{}",
        &REVMC_REV[..REVMC_REV.len().min(12)],
        sanitize(std::env::consts::ARCH),
        target_cpu(),
    )
}

/// Keep directory/file names filesystem-safe (alnum, `_`, `-`, `.`); everything else → `_`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' })
        .collect()
}

fn spec_to_u8(s: SpecId) -> u8 {
    s as u8
}
fn spec_from_u8(v: u8) -> Option<SpecId> {
    SpecId::try_from(v).ok()
}

fn backend_to_u8(b: BackendSelection) -> u8 {
    match b {
        BackendSelection::Auto => 0,
        BackendSelection::Llvm => 1,
    }
}
fn backend_from_u8(v: u8) -> Option<BackendSelection> {
    match v {
        0 => Some(BackendSelection::Auto),
        1 => Some(BackendSelection::Llvm),
        _ => None,
    }
}

fn opt_to_u8(o: OptimizationLevel) -> u8 {
    match o {
        OptimizationLevel::None => 0,
        OptimizationLevel::Less => 1,
        OptimizationLevel::Default => 2,
        OptimizationLevel::Aggressive => 3,
    }
}
fn opt_from_u8(v: u8) -> Option<OptimizationLevel> {
    match v {
        0 => Some(OptimizationLevel::None),
        1 => Some(OptimizationLevel::Less),
        2 => Some(OptimizationLevel::Default),
        3 => Some(OptimizationLevel::Aggressive),
        _ => None,
    }
}

/// The stem (no extension) uniquely naming an artifact's files within a namespace directory.
fn key_stem(key: &ArtifactKey) -> String {
    format!(
        "{:x}__s{}__b{}__o{}",
        key.runtime.code_hash,
        spec_to_u8(key.runtime.spec_id),
        backend_to_u8(key.backend),
        opt_to_u8(key.opt_level),
    )
}

/// Render the sidecar for `manifest`. Dependency-free `key value` lines — a cache sidecar must
/// never pull a serde stack, and a hand-checked fixed schema is trivial to skip-on-malformed.
fn render_sidecar(manifest: &ArtifactManifest) -> String {
    let k = &manifest.artifact_key;
    format!(
        "{SIDECAR_MAGIC}\n\
         code_hash {:x}\n\
         spec {}\n\
         backend {}\n\
         opt {}\n\
         symbol {}\n\
         bytecode_len {}\n\
         artifact_len {}\n\
         created_at {}\n\
         content_hash {}\n",
        k.runtime.code_hash,
        spec_to_u8(k.runtime.spec_id),
        backend_to_u8(k.backend),
        opt_to_u8(k.opt_level),
        manifest.symbol_name,
        manifest.bytecode_len,
        manifest.artifact_len,
        manifest.created_at_unix_secs,
        hex32(&manifest.content_hash),
    )
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Parse a sidecar back into an [`ArtifactManifest`]. Any malformed / missing / unknown-enum field
/// returns `None` (the caller SKIPS the artifact — corruption is a cache miss, never a panic).
fn parse_sidecar(text: &str) -> Option<ArtifactManifest> {
    let mut lines = text.lines();
    if lines.next()?.trim() != SIDECAR_MAGIC {
        return None;
    }
    let mut code_hash: Option<B256> = None;
    let mut spec: Option<u8> = None;
    let mut backend: Option<u8> = None;
    let mut opt: Option<u8> = None;
    let mut symbol: Option<String> = None;
    let mut bytecode_len: Option<usize> = None;
    let mut artifact_len: Option<usize> = None;
    let mut created_at: Option<u64> = None;
    let mut content_hash: Option<[u8; 32]> = None;
    for line in lines {
        let line = line.trim_end_matches('\r');
        let Some((key, val)) = line.split_once(' ') else { continue };
        match key {
            "code_hash" => code_hash = parse_hex32(val).map(B256::from),
            "spec" => spec = val.trim().parse().ok(),
            "backend" => backend = val.trim().parse().ok(),
            "opt" => opt = val.trim().parse().ok(),
            "symbol" => symbol = Some(val.to_string()),
            "bytecode_len" => bytecode_len = val.trim().parse().ok(),
            "artifact_len" => artifact_len = val.trim().parse().ok(),
            "created_at" => created_at = val.trim().parse().ok(),
            "content_hash" => content_hash = parse_hex32(val),
            _ => {}
        }
    }
    let key = ArtifactKey {
        runtime: RuntimeCacheKey {
            code_hash: code_hash?,
            spec_id: spec_from_u8(spec?)?,
        },
        backend: backend_from_u8(backend?)?,
        opt_level: opt_from_u8(opt?)?,
    };
    Some(ArtifactManifest {
        artifact_key: key,
        symbol_name: symbol?,
        bytecode_len: bytecode_len?,
        artifact_len: artifact_len?,
        created_at_unix_secs: created_at?,
        content_hash: content_hash?,
    })
}

/// A durable [`ArtifactStore`] that persists compiled AOT dylibs across process restarts, keyed
/// (via its namespace directory) by cache-version + revmc rev + target arch + target CPU on top of
/// revmc's `{code_hash, spec, backend, opt}` — so a Graviton reboot reuses its own artifacts and
/// never loads an incompatible one.
#[derive(Debug)]
pub struct PersistentArtifactStore {
    /// The namespace directory (base/`<namespace>`) that owns every `.so`/`.json` pair.
    dir: PathBuf,
    /// In-RAM index, seeded from disk at construction and updated on every `store`.
    index: Mutex<HashMap<ArtifactKey, StoredArtifact>>,
}

impl PersistentArtifactStore {
    /// Open (creating if needed) the persistent store rooted at `base_dir`, seeding the index from
    /// any valid artifacts already on disk in this build's namespace.
    pub fn open(base_dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = base_dir.as_ref().join(namespace());
        fs::create_dir_all(&dir)?;
        let store = Self { dir, index: Mutex::new(HashMap::new()) };
        let seeded = store.scan_disk();
        if !seeded.is_empty() {
            let mut idx = store.index.lock().unwrap();
            for (k, v) in seeded {
                idx.insert(k, v);
            }
        }
        Ok(store)
    }

    /// The namespace directory this store owns (for logging / tests).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Number of valid artifacts currently indexed.
    pub fn len(&self) -> usize {
        self.index.lock().unwrap().len()
    }

    /// Whether the store holds no artifacts.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn sidecar_path(&self, key: &ArtifactKey) -> PathBuf {
        self.dir.join(format!("{}.json", key_stem(key)))
    }
    fn dylib_path(&self, key: &ArtifactKey) -> PathBuf {
        self.dir.join(format!("{}.so", key_stem(key)))
    }

    /// Scan the namespace directory and return every artifact whose sidecar parses AND whose `.so`
    /// re-hashes to the persisted `content_hash`. Malformed sidecars, missing dylibs, and corrupt
    /// dylibs are skipped (stale ⇒ recompile-never-load).
    fn scan_disk(&self) -> Vec<(ArtifactKey, StoredArtifact)> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let Some(manifest) = parse_sidecar(&text) else { continue };
            let dylib = self.dylib_path(&manifest.artifact_key);
            let Ok(bytes) = fs::read(&dylib) else { continue };
            // Integrity: the `.so` must hash to what we persisted, else it is torn/tampered.
            if keccak256(&bytes).0 != manifest.content_hash {
                continue;
            }
            let key = manifest.artifact_key.clone();
            out.push((key, StoredArtifact { manifest, dylib_path: dylib }));
        }
        out
    }
}

impl ArtifactStore for PersistentArtifactStore {
    fn load_all(&self) -> revmc::eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
        // Re-scan disk (authoritative) rather than trusting only the RAM index, so a sibling
        // process that wrote artifacts is picked up, and refresh the index to match.
        let scanned = self.scan_disk();
        let mut idx = self.index.lock().unwrap();
        idx.clear();
        for (k, v) in &scanned {
            idx.insert(k.clone(), v.clone());
        }
        Ok(scanned)
    }

    fn load(&self, key: &ArtifactKey) -> revmc::eyre::Result<Option<StoredArtifact>> {
        if let Some(hit) = self.index.lock().unwrap().get(key).cloned() {
            // Confirm the file is still present + intact before handing back a path to dlopen.
            if let Ok(bytes) = fs::read(&hit.dylib_path) {
                if keccak256(&bytes).0 == hit.manifest.content_hash {
                    return Ok(Some(hit));
                }
            }
        }
        Ok(None)
    }

    fn store(
        &self,
        key: &ArtifactKey,
        manifest: &ArtifactManifest,
        dylib_bytes: &[u8],
    ) -> revmc::eyre::Result<()> {
        let dylib = self.dylib_path(key);
        // Write the dylib first, then the sidecar — a reader that sees the sidecar always finds a
        // complete `.so` (and the content_hash check rejects a torn one regardless).
        fs::write(&dylib, dylib_bytes)
            .map_err(|e| revmc::eyre::eyre!("write dylib {:?}: {e}", dylib))?;
        let sidecar = self.sidecar_path(key);
        fs::write(&sidecar, render_sidecar(manifest))
            .map_err(|e| revmc::eyre::eyre!("write sidecar {:?}: {e}", sidecar))?;
        self.index.lock().unwrap().insert(
            key.clone(),
            StoredArtifact { manifest: manifest.clone(), dylib_path: dylib },
        );
        Ok(())
    }

    fn delete(&self, key: &ArtifactKey) -> revmc::eyre::Result<()> {
        self.index.lock().unwrap().remove(key);
        let _ = fs::remove_file(self.dylib_path(key));
        let _ = fs::remove_file(self.sidecar_path(key));
        Ok(())
    }

    fn clear(&self) -> revmc::eyre::Result<()> {
        let keys: Vec<ArtifactKey> = self.index.lock().unwrap().keys().cloned().collect();
        for k in keys {
            let _ = self.delete(&k);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code_hash_byte: u8, spec: SpecId, opt: OptimizationLevel) -> ArtifactKey {
        ArtifactKey {
            runtime: RuntimeCacheKey { code_hash: B256::with_last_byte(code_hash_byte), spec_id: spec },
            backend: BackendSelection::Llvm,
            opt_level: opt,
        }
    }

    fn manifest_for(key: &ArtifactKey, dylib: &[u8], symbol: &str) -> ArtifactManifest {
        ArtifactManifest {
            artifact_key: key.clone(),
            symbol_name: symbol.to_string(),
            bytecode_len: 3,
            artifact_len: dylib.len(),
            created_at_unix_secs: 1_752_000_000,
            content_hash: keccak256(dylib).0,
        }
    }

    /// Store → load round-trip across a FRESH store instance (simulates a reboot): the persisted
    /// dylib + sidecar are found and reconstructed byte-for-byte. This is the plan's laptop
    /// "artifact round-trip" gate, with NO LLVM (pure store code).
    #[test]
    fn store_then_reopen_round_trips_artifact() {
        let tmp = tempdir();
        let k = key(1, SpecId::OSAKA, OptimizationLevel::Default);
        let dylib = b"\x7fELF-not-really-but-bytes-are-what-matter".to_vec();
        {
            let store = PersistentArtifactStore::open(&tmp).unwrap();
            store.store(&k, &manifest_for(&k, &dylib, "revmc_sym_A"), &dylib).unwrap();
            assert_eq!(store.len(), 1);
        }
        // Fresh instance = a new process boot: it must find the artifact on disk.
        let reopened = PersistentArtifactStore::open(&tmp).unwrap();
        assert_eq!(reopened.len(), 1, "reboot must re-discover the persisted artifact");
        let all = reopened.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, k);
        assert_eq!(all[0].1.manifest.symbol_name, "revmc_sym_A");
        assert_eq!(std::fs::read(&all[0].1.dylib_path).unwrap(), dylib);
        let one = reopened.load(&k).unwrap().unwrap();
        assert_eq!(one.manifest.content_hash, keccak256(&dylib).0);
    }

    /// A corrupt/truncated dylib whose bytes no longer match the sidecar `content_hash` is SKIPPED,
    /// not loaded — the stale ⇒ recompile-never-load rule.
    #[test]
    fn corrupt_dylib_is_skipped_on_reopen() {
        let tmp = tempdir();
        let k = key(2, SpecId::OSAKA, OptimizationLevel::Default);
        let dylib = b"good-dylib-bytes".to_vec();
        {
            let store = PersistentArtifactStore::open(&tmp).unwrap();
            store.store(&k, &manifest_for(&k, &dylib, "revmc_sym_B"), &dylib).unwrap();
        }
        // Corrupt the .so on disk.
        let store = PersistentArtifactStore::open(&tmp).unwrap();
        std::fs::write(store.dylib_path(&k), b"tampered").unwrap();
        assert!(store.load(&k).unwrap().is_none(), "corrupt dylib must not load");
        let reopened = PersistentArtifactStore::open(&tmp).unwrap();
        assert!(reopened.is_empty(), "corrupt artifact must be skipped on reboot scan");
        assert!(reopened.load_all().unwrap().is_empty());
    }

    /// A malformed sidecar (bad magic / missing field / unknown enum) is skipped, never panics.
    #[test]
    fn malformed_sidecar_is_skipped() {
        assert!(parse_sidecar("").is_none());
        assert!(parse_sidecar("wrong-magic\ncode_hash 00\n").is_none());
        // Missing content_hash line.
        let partial = "revmc-artifact-v1\ncode_hash 0000000000000000000000000000000000000000000000000000000000000001\nspec 42\nbackend 1\nopt 2\nsymbol x\nbytecode_len 1\nartifact_len 1\ncreated_at 1\n";
        assert!(parse_sidecar(partial).is_none());
        // Unknown spec discriminant (255 is not a valid SpecId) → skipped.
        let bad_spec = "revmc-artifact-v1\ncode_hash 0000000000000000000000000000000000000000000000000000000000000001\nspec 255\nbackend 1\nopt 2\nsymbol x\nbytecode_len 1\nartifact_len 1\ncreated_at 1\ncontent_hash 0000000000000000000000000000000000000000000000000000000000000000\n";
        assert!(parse_sidecar(bad_spec).is_none());
    }

    /// The namespace encodes the revmc rev + arch, so it changes when the build identity changes —
    /// the "never load an ABI-incompatible artifact" guard. Sidecar parse is exact-inverse of render.
    #[test]
    fn sidecar_render_parse_is_identity_and_namespace_pins_identity() {
        let k = key(9, SpecId::CANCUN, OptimizationLevel::Aggressive);
        let m = manifest_for(&k, b"zz", "revmc_roundtrip");
        let parsed = parse_sidecar(&render_sidecar(&m)).unwrap();
        assert_eq!(parsed.artifact_key, k);
        assert_eq!(parsed.symbol_name, "revmc_roundtrip");
        assert_eq!(parsed.content_hash, m.content_hash);
        let ns = namespace();
        assert!(ns.contains(&REVMC_REV[..12]), "namespace must pin the revmc rev: {ns}");
        assert!(ns.contains(std::env::consts::ARCH), "namespace must pin the arch: {ns}");
    }

    /// Store-supplied key with a different opt level is a DIFFERENT artifact (distinct files).
    #[test]
    fn distinct_keys_do_not_collide() {
        let tmp = tempdir();
        let a = key(3, SpecId::OSAKA, OptimizationLevel::Default);
        let b = key(3, SpecId::OSAKA, OptimizationLevel::Aggressive);
        let store = PersistentArtifactStore::open(&tmp).unwrap();
        store.store(&a, &manifest_for(&a, b"aa", "sa"), b"aa").unwrap();
        store.store(&b, &manifest_for(&b, b"bbb", "sb"), b"bbb").unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(std::fs::read(store.load(&a).unwrap().unwrap().dylib_path).unwrap(), b"aa");
        assert_eq!(std::fs::read(store.load(&b).unwrap().unwrap().dylib_path).unwrap(), b"bbb");
    }

    /// A unique temp dir under the session scratch (no external crates needed).
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mantis-revmc-artifacts-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
}
