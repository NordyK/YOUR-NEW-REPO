//! §5.6 codehash artifact store — seed + load per-family runtime-hash allowlists.
//!
//! The codehash gate (`codehash_gate.rs`) enforces that every discovered pool's deployed
//! bytecode hashes to a known-good value for its family. But the allowlist has to be POPULATED
//! from somewhere. Two paths, both handled here:
//!
//!   * **Record pass** (`enforce_codehash=false`): during a live discovery run we observe the
//!     real runtime hash of each pool and accumulate the distinct `(family → {hash})` set, then
//!     persist it to `research/dex/<family>/runtime.hash` (one lowercase hex hash per line).
//!     This is the "seed the hash artifacts" workflow the gate doc references.
//!   * **Enforce pass** (`enforce_codehash=true`): load those artifact files into a
//!     `CodehashAllowlist` so the gate rejects any pool whose bytecode doesn't match a recorded
//!     hash. Operator workflow: run once to record → eyeball the artifacts → flip enforce.
//!
//! Artifacts are plain text (one hex hash per line, `#` comments allowed) so they're diffable in
//! git and a human can audit exactly which bytecode variants are trusted before going live.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use engine_state::PoolKind;

use crate::codehash_gate::CodehashAllowlist;

/// Map a `PoolKind` to its artifact subdirectory name under `research/dex/`. Stable strings
/// (NOT the Debug repr, which could change) so artifacts survive enum refactors.
pub fn family_dir_name(kind: PoolKind) -> &'static str {
    match kind {
        PoolKind::UniswapV2 => "uniswap_v2",
        PoolKind::SushiV2 => "sushi_v2",
        PoolKind::CamelotV2 => "camelot_v2",
        PoolKind::UniswapV3 => "uniswap_v3",
        PoolKind::SushiV3 => "sushi_v3",
        PoolKind::PancakeV3 => "pancake_v3",
        PoolKind::AlgebraV19 => "algebra_v19",
        PoolKind::SolidlyV3 => "solidly_v3",
        PoolKind::CurveStableSwapNg => "curve_stableswap_ng",
        PoolKind::CurveCryptoSwap => "curve_cryptoswap",
        PoolKind::TraderJoeLb => "trader_joe_lb",
        PoolKind::MaverickV2 => "maverick_v2",
        PoolKind::FluidDex => "fluid_dex",
        PoolKind::DodoV2 => "dodo_v2",
        PoolKind::KyberSwapElastic => "kyberswap_elastic",
        PoolKind::UniswapV4 => "uniswap_v4",
        PoolKind::BalancerStable => "balancer_v2",
        PoolKind::BalancerWeighted => "balancer_weighted",
    }
}

/// The families the engine records/enforces codehashes for. (All `PoolKind` variants the
/// discovery layer can produce.)
pub const RECORDED_FAMILIES: &[PoolKind] = &[
    PoolKind::UniswapV2,
    PoolKind::SushiV2,
    PoolKind::CamelotV2,
    PoolKind::UniswapV3,
    PoolKind::SushiV3,
    PoolKind::PancakeV3,
    PoolKind::AlgebraV19,
    PoolKind::SolidlyV3,
    PoolKind::CurveStableSwapNg,
    PoolKind::CurveCryptoSwap,
    PoolKind::TraderJoeLb,
    PoolKind::MaverickV2,
    PoolKind::FluidDex,
    PoolKind::DodoV2,
    PoolKind::KyberSwapElastic,
    PoolKind::UniswapV4,
    PoolKind::BalancerStable,
    PoolKind::BalancerWeighted,
];

/// Accumulator for a record pass: collects distinct runtime hashes per family.
#[derive(Debug, Default)]
pub struct CodehashRecorder {
    seen: std::collections::HashMap<PoolKind, BTreeSet<[u8; 32]>>,
}

impl CodehashRecorder {
    /// New empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observed `(family, runtime_hash)`. Deduped — recording the same hash twice
    /// is a no-op.
    pub fn record(&mut self, family: PoolKind, hash: [u8; 32]) {
        self.seen.entry(family).or_default().insert(hash);
    }

    /// Number of distinct hashes recorded for a family.
    pub fn count(&self, family: PoolKind) -> usize {
        self.seen.get(&family).map(|s| s.len()).unwrap_or(0)
    }

    /// Serialize one family's hashes to the canonical artifact text (sorted hex, one per line,
    /// with a provenance header comment). Returns `None` if nothing was recorded for it.
    pub fn render_family(&self, family: PoolKind) -> Option<String> {
        let set = self.seen.get(&family)?;
        if set.is_empty() {
            return None;
        }
        let mut out = String::new();
        out.push_str(&format!(
            "# {} runtime-bytecode keccak256 hashes (recorded by engine discovery).\n",
            family_dir_name(family)
        ));
        out.push_str("# One lowercase hex hash per line. Audited before enforce_codehash=true.\n");
        for h in set {
            out.push_str(&hex_lower(h));
            out.push('\n');
        }
        Some(out)
    }

    /// Persist every recorded family to `<root>/<family>/runtime.hash`. Returns the number of
    /// files written. Best-effort per family; an IO error on one family doesn't abort the rest.
    pub fn write_dir(&self, root: &Path) -> usize {
        let mut written = 0usize;
        for &family in RECORDED_FAMILIES {
            if let Some(text) = self.render_family(family) {
                let dir = root.join(family_dir_name(family));
                if std::fs::create_dir_all(&dir).is_err() {
                    continue;
                }
                let path = dir.join("runtime.hash");
                if std::fs::write(&path, text).is_ok() {
                    written += 1;
                }
            }
        }
        written
    }
}

/// Parse a codehash artifact text body into a list of hashes. Ignores blank lines + `#`
/// comments; tolerates an optional `0x` prefix. Bad lines are skipped (logged by the caller).
pub fn parse_artifact(body: &str) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    for line in body.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        let hex = s.strip_prefix("0x").unwrap_or(s);
        if let Some(h) = parse_hex32(hex) {
            out.push(h);
        }
    }
    out
}

/// Load all `<root>/<family>/runtime.hash` artifacts into a `CodehashAllowlist`. Families with
/// no artifact file (or an empty one) are left unrecorded → the gate stays in skip-mode for
/// them. Returns `(allowlist, families_loaded)`.
pub fn load_allowlist_from_dir(root: &Path) -> (CodehashAllowlist, usize) {
    let mut allow = CodehashAllowlist::new();
    let mut families_loaded = 0usize;
    for &family in RECORDED_FAMILIES {
        let path: PathBuf = root.join(family_dir_name(family)).join("runtime.hash");
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let hashes = parse_artifact(&body);
        if hashes.is_empty() {
            continue;
        }
        for h in hashes {
            allow.add(family, h);
        }
        families_loaded += 1;
    }
    (allow, families_loaded)
}

fn hex_lower(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_dir_names_are_unique_and_stable() {
        let mut seen = std::collections::HashSet::new();
        for &f in RECORDED_FAMILIES {
            let name = family_dir_name(f);
            assert!(seen.insert(name), "duplicate dir name {name} for {f:?}");
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn recorder_dedups_and_counts() {
        let mut r = CodehashRecorder::new();
        r.record(PoolKind::UniswapV3, [0x11; 32]);
        r.record(PoolKind::UniswapV3, [0x11; 32]); // dup — no-op
        r.record(PoolKind::UniswapV3, [0x22; 32]);
        r.record(PoolKind::UniswapV2, [0x33; 32]);
        assert_eq!(r.count(PoolKind::UniswapV3), 2);
        assert_eq!(r.count(PoolKind::UniswapV2), 1);
        assert_eq!(r.count(PoolKind::DodoV2), 0);
    }

    #[test]
    fn render_then_parse_roundtrips() {
        let mut r = CodehashRecorder::new();
        r.record(PoolKind::UniswapV3, [0xab; 32]);
        r.record(PoolKind::UniswapV3, [0x01; 32]);
        let text = r.render_family(PoolKind::UniswapV3).expect("non-empty");
        // Must contain a provenance comment + both hashes.
        assert!(
            text.contains('#'),
            "artifact should carry a provenance header"
        );
        let parsed = parse_artifact(&text);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&[0xab; 32]));
        assert!(parsed.contains(&[0x01; 32]));
    }

    #[test]
    fn render_empty_family_is_none() {
        let r = CodehashRecorder::new();
        assert!(r.render_family(PoolKind::UniswapV3).is_none());
    }

    #[test]
    fn parse_artifact_ignores_comments_blanks_and_0x_prefix() {
        let body = "# header\n\n0xaabbccddeeff00112233445566778899aabbccddeeff00112233445566778899\n   \n# another comment\nffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100\n";
        let hashes = parse_artifact(body);
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0][0], 0xaa);
        assert_eq!(hashes[1][0], 0xff);
    }

    #[test]
    fn parse_artifact_rejects_malformed_lines() {
        // Wrong length, non-hex → skipped, not panicked.
        let body =
            "deadbeef\nnot_hex_at_all_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
        assert!(parse_artifact(body).is_empty());
    }

    #[test]
    fn write_then_load_roundtrips_through_real_fs() {
        // End-to-end: record → write_dir → load_allowlist_from_dir → allowlist enforces with
        // exactly the recorded hashes. Proves the seed→enforce workflow works on a real FS.
        let tmp =
            std::env::temp_dir().join(format!("codehash_artifacts_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let mut r = CodehashRecorder::new();
        r.record(PoolKind::UniswapV3, [0x11; 32]);
        r.record(PoolKind::UniswapV3, [0x22; 32]);
        r.record(PoolKind::DodoV2, [0x33; 32]);
        let n = r.write_dir(&tmp);
        assert_eq!(n, 2, "two families had recorded hashes");

        let (allow, families) = load_allowlist_from_dir(&tmp);
        assert_eq!(families, 2);
        assert!(allow.enforces(PoolKind::UniswapV3));
        assert!(allow.accepts(PoolKind::UniswapV3, &[0x11; 32]));
        assert!(allow.accepts(PoolKind::UniswapV3, &[0x22; 32]));
        assert!(!allow.accepts(PoolKind::UniswapV3, &[0x99; 32]));
        assert!(allow.enforces(PoolKind::DodoV2));
        // A family that was never recorded stays in skip-mode (accepts anything).
        assert!(!allow.enforces(PoolKind::UniswapV2));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
