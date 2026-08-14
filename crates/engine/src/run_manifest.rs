//! P0 — network run manifest + ordering labels (GPT-5.6-sol network review, 2026-07-13, §Priority 0).
//!
//! Emits a small `network_run_manifest.json` beside each dry-run's output so every latency number a
//! run produces is interpretable against the **actual Arbitrum One ordering model**. This module owns
//! the STATIC manifest + the schema; the per-submit lane TAG (normal vs express ring origin) is
//! emitted separately by the submit path.
//!
//! ## Why this exists (the load-bearing protocol fact)
//!
//! Timeboost is LIVE on Arbitrum One (since 2025) — the chain is **not** globally pure FCFS. Mantis
//! runs in the *normal (non-express) lane*, which still has internal arrival ordering, but an
//! express-lane transaction receives a protocol-scale advantage commonly configured around **200 ms**
//! (Offchain Labs timeboost design spec). A microsecond transport win on our submit leg therefore
//! does NOT beat an express-lane transaction — the manifest makes that explicit so a dry-run's "347 µs
//! one-way" is never read as superiority to a 200 ms express priority.
//!
//! It also records that our headline submit number is a **half application-RTT proxy** (RTT/2 of an
//! `eth_chainId`-class round trip), not an observed one-way sequencer→host measurement: no
//! second-resolution feed timestamp can establish a sub-ms one-way (see `seq_latency_monitor.rs`).
//!
//! ## Emission
//!
//! `emit_beside_output()` writes `network_run_manifest.json` into `NETWORK_RUN_MANIFEST_DIR` (else
//! `LOG_DIR`, else `logs/`). Call it once at dry-run startup. It is measurement-only: it changes no
//! submitted bytes and never touches the Timeboost auction.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

/// Manifest schema identifier (bump on a breaking field change).
pub const SCHEMA: &str = "mantis.network-run-manifest.v1";

/// Chain the run targets.
pub const CHAIN: &str = "arbitrum-one";

/// The lane Mantis submits on. Mantis does NOT use the Timeboost express auction; it submits on the
/// delayed regular (normal) lane. The submit agent's per-submit lane TAG uses these same strings.
pub const LANE_NORMAL: &str = "normal_non_express";
/// The (currently unused) express lane label — exposed so the submit tag and the manifest agree.
pub const LANE_EXPRESS: &str = "express";

/// Ordering model in force on Arbitrum One for our lane (NOT pure FCFS — Timeboost is live).
pub const ORDERING: &str = "timeboost_normal_lane";

/// What the headline submit latency number actually is: half of an application RTT, a symmetry
/// proxy — NOT an observed one-way sequencer→host time.
pub const SUBMIT_PATH_METRIC: &str = "half_application_rtt_proxy";

/// Approx. express-lane priority advantage (ms) an express tx gets over the normal lane. Documented
/// so the manifest reader can weigh a µs transport delta against it. (~200 ms per Offchain Labs.)
pub const EXPRESS_LANE_PRIORITY_MS: u64 = 200;

/// Timeboost controller observation status. Default `Unknown` — a dry-run that did not positively
/// observe the auction/controller must say so rather than imply FCFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeboostControllerStatus {
    /// The controller / express auction state was positively observed this run.
    Observed,
    /// Not observed — the honest default.
    Unknown,
}

impl TimeboostControllerStatus {
    /// Canonical wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            TimeboostControllerStatus::Observed => "observed",
            TimeboostControllerStatus::Unknown => "unknown",
        }
    }
    /// Parse the `TIMEBOOST_CONTROLLER_STATUS` env override; anything but an explicit
    /// observed/1/true/on is treated as `Unknown` (fail-honest).
    fn from_env() -> Self {
        match std::env::var("TIMEBOOST_CONTROLLER_STATUS").ok().as_deref() {
            Some("observed") | Some("1") | Some("true") | Some("on") => {
                TimeboostControllerStatus::Observed
            }
            _ => TimeboostControllerStatus::Unknown,
        }
    }
}

/// The static network-run manifest. `chain`/`lane`/`ordering`/`submit_path_metric` are fixed truths
/// about the deployment; the placement / provenance fields are best-effort context (filled from the
/// launcher-provided `PROBE_*` env, or `?` when absent) so a number is attributable to a box.
#[derive(Debug, Clone)]
pub struct NetworkRunManifest {
    /// Chain the run targets (`arbitrum-one`).
    pub chain: &'static str,
    /// Submission lane (`normal_non_express`).
    pub lane: &'static str,
    /// Ordering model in force for our lane (`timeboost_normal_lane`).
    pub ordering: &'static str,
    /// What the headline submit latency number is (`half_application_rtt_proxy`).
    pub submit_path_metric: &'static str,
    /// Whether the Timeboost controller/auction was positively observed this run.
    pub timeboost_controller_status: TimeboostControllerStatus,
    /// Whether THIS run is a candidate for the express lane. Default false (Mantis does not bid).
    pub express_candidate: bool,
    /// Approx. express-lane priority advantage (ms) over the normal lane (~200).
    pub express_lane_priority_ms: u64,
    /// AWS region (best-effort, from `PROBE_REGION`).
    pub region: String,
    /// AWS AZ-ID (best-effort, from `PROBE_AZ_ID`) — the stable physical placement handle.
    pub az_id: String,
    /// AWS AZ name (best-effort, from `PROBE_AZ_NAME`).
    pub az_name: String,
    /// EC2 instance type (best-effort, from `PROBE_ITYPE`).
    pub instance_type: String,
    /// Git HEAD of the build (best-effort, from `GIT_HEAD`/`GIT_SHA`).
    pub git_head: String,
    /// Manifest generation wall-clock time (unix ns).
    pub generated_unix_ns: u64,
    /// WI-02: the sealed `RuntimePolicy` binding (active/proven family hashes, route-shape
    /// hash, executor exchange-id table hash, gas-profile table hash, REVM compiled + mode).
    /// `Null` only on the legacy no-policy path; the production boot always binds it.
    pub runtime_policy_binding: serde_json::Value,
}

impl NetworkRunManifest {
    /// Build the manifest from the environment. All values are safe defaults when env is absent, so
    /// this never fails and never blocks a run.
    pub fn from_env() -> Self {
        // Reuse the launcher's probe labels when present; they are also what latency-probe emits.
        let env_or = |k: &str, d: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| d.into());
        NetworkRunManifest {
            chain: CHAIN,
            lane: LANE_NORMAL,
            ordering: ORDERING,
            submit_path_metric: SUBMIT_PATH_METRIC,
            timeboost_controller_status: TimeboostControllerStatus::from_env(),
            // Explicit opt-in only; Mantis does not bid into the express auction by default.
            express_candidate: matches!(
                std::env::var("EXPRESS_CANDIDATE").ok().as_deref(),
                Some("1") | Some("true") | Some("on")
            ),
            express_lane_priority_ms: EXPRESS_LANE_PRIORITY_MS,
            region: env_or("PROBE_REGION", "?"),
            az_id: env_or("PROBE_AZ_ID", "?"),
            az_name: env_or("PROBE_AZ_NAME", "?"),
            instance_type: env_or("PROBE_ITYPE", "?"),
            // GIT_HEAD/GIT_SHA is injected by the launcher/build when available; `unknown` otherwise.
            git_head: env_or("GIT_HEAD", &env_or("GIT_SHA", "unknown")),
            generated_unix_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            runtime_policy_binding: serde_json::Value::Null,
        }
    }

    /// [`NetworkRunManifest::from_env`] plus the WI-02 policy binding: the manifest carries
    /// the sealed active/proven/route-shape/executor/gas-profile hashes so every latency or
    /// detection number this run produces is attributable to the exact resolved policy.
    pub fn from_env_with_policy(policy: &crate::runtime_policy::RuntimePolicy) -> Self {
        let mut m = Self::from_env();
        m.runtime_policy_binding = crate::runtime_policy::manifest_binding(policy);
        m
    }

    /// Serialize to the canonical JSON value. Field order/keys are the schema contract.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "schema": SCHEMA,
            "chain": self.chain,
            "lane": self.lane,
            "ordering": self.ordering,
            "submit_path_metric": self.submit_path_metric,
            "timeboost_controller_status": self.timeboost_controller_status.as_str(),
            "express_candidate": self.express_candidate,
            "express_lane_priority_ms": self.express_lane_priority_ms,
            "region": self.region,
            "az_id": self.az_id,
            "az_name": self.az_name,
            "instance_type": self.instance_type,
            "git_head": self.git_head,
            "generated_unix_ns": self.generated_unix_ns,
            // WI-02: the sealed RuntimePolicy binding (hashes over active/proven families,
            // route shapes, the executor exchange-id table and the gas-profile table).
            "runtime_policy": self.runtime_policy_binding,
            // A human-facing interpretation guard, embedded so a stray JSON reader can't miss it.
            "notes": [
                "Arbitrum One runs Timeboost (live since 2025) — NOT pure FCFS. Mantis submits on the \
                 normal (non-express) lane.",
                "An express-lane tx gets ~200 ms protocol priority; a microsecond transport win on the \
                 normal lane does not beat that.",
                "submit_path_metric=half_application_rtt_proxy: the headline submit number is RTT/2 of \
                 an application round trip, a symmetry proxy — NOT an observed one-way.",
                "Every dry-run latency sample must state its lane (normal|express), the Timeboost \
                 controller status (or 'unknown'), and RTT vs half-RTT-proxy nomenclature."
            ]
        })
    }

    /// Pretty JSON string (what lands in the file).
    pub fn to_pretty_string(&self) -> String {
        serde_json::to_string_pretty(&self.to_json()).unwrap_or_else(|_| "{}".into())
    }

    /// Write `network_run_manifest.json` into `dir`. Returns the written path.
    pub fn write_to_dir(&self, dir: &Path) -> io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(MANIFEST_FILENAME);
        std::fs::write(&path, self.to_pretty_string())?;
        Ok(path)
    }
}

/// Canonical manifest filename.
pub const MANIFEST_FILENAME: &str = "network_run_manifest.json";

/// Resolve the output directory the manifest should sit beside: `NETWORK_RUN_MANIFEST_DIR`, else
/// `LOG_DIR` (what the dry-run launcher passes), else `logs/`.
fn output_dir() -> PathBuf {
    for k in ["NETWORK_RUN_MANIFEST_DIR", "LOG_DIR"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim();
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from("logs")
}

/// Emit the manifest beside the current run's output. Best-effort: logs and returns the path on
/// success, logs a warning and returns `None` on any I/O error (never blocks a run). Call this once
/// at dry-run startup, BEFORE the first latency number is produced.
pub fn emit_beside_output() -> Option<PathBuf> {
    emit_manifest(NetworkRunManifest::from_env())
}

/// [`emit_beside_output`] with the WI-02 sealed-policy binding — the production boot path.
pub fn emit_beside_output_with_policy(
    policy: &crate::runtime_policy::RuntimePolicy,
) -> Option<PathBuf> {
    emit_manifest(NetworkRunManifest::from_env_with_policy(policy))
}

fn emit_manifest(manifest: NetworkRunManifest) -> Option<PathBuf> {
    let dir = output_dir();
    match manifest.write_to_dir(&dir) {
        Ok(path) => {
            tracing::info!(
                target: "run_manifest",
                path = %path.display(),
                lane = manifest.lane,
                ordering = manifest.ordering,
                submit_path_metric = manifest.submit_path_metric,
                timeboost_controller_status = manifest.timeboost_controller_status.as_str(),
                express_candidate = manifest.express_candidate,
                "network run manifest written (Timeboost normal lane; submit metric = half-RTT proxy)"
            );
            Some(path)
        }
        Err(e) => {
            tracing::warn!(target: "run_manifest", dir = %dir.display(), error = %e, "could not write network run manifest");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Isolate env mutation: these tests read/write process env, so serialize them with a lock.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn schema_has_the_required_ordering_fields_with_correct_values() {
        let _g = ENV_LOCK.lock().unwrap();
        // Clear influencing env so we test the honest default.
        std::env::remove_var("TIMEBOOST_CONTROLLER_STATUS");
        std::env::remove_var("EXPRESS_CANDIDATE");
        let m = NetworkRunManifest::from_env();
        let v = m.to_json();
        assert_eq!(v["chain"], "arbitrum-one");
        assert_eq!(v["lane"], "normal_non_express");
        assert_eq!(v["ordering"], "timeboost_normal_lane");
        assert_eq!(v["submit_path_metric"], "half_application_rtt_proxy");
        // Honest defaults: not observed, not an express candidate.
        assert_eq!(v["timeboost_controller_status"], "unknown");
        assert_eq!(v["express_candidate"], false);
        assert_eq!(v["schema"], SCHEMA);
        // The 200ms express-priority fact must be present so a µs win is never over-read.
        assert_eq!(v["express_lane_priority_ms"], 200);
        // notes must warn about Timeboost + the RTT/2 nomenclature.
        let notes = v["notes"].as_array().expect("notes array");
        assert!(notes.iter().any(|n| n.as_str().unwrap_or("").contains("Timeboost")));
        assert!(notes.iter().any(|n| n.as_str().unwrap_or("").to_lowercase().contains("half")));
    }

    #[test]
    fn env_overrides_are_honoured() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("TIMEBOOST_CONTROLLER_STATUS", "observed");
        std::env::set_var("EXPRESS_CANDIDATE", "1");
        std::env::set_var("PROBE_REGION", "us-east-2");
        std::env::set_var("PROBE_AZ_ID", "use2-az2");
        let m = NetworkRunManifest::from_env();
        assert_eq!(m.timeboost_controller_status, TimeboostControllerStatus::Observed);
        assert!(m.express_candidate);
        let v = m.to_json();
        assert_eq!(v["timeboost_controller_status"], "observed");
        assert_eq!(v["express_candidate"], true);
        assert_eq!(v["region"], "us-east-2");
        assert_eq!(v["az_id"], "use2-az2");
        // cleanup so we don't leak into other tests
        std::env::remove_var("TIMEBOOST_CONTROLLER_STATUS");
        std::env::remove_var("EXPRESS_CANDIDATE");
        std::env::remove_var("PROBE_REGION");
        std::env::remove_var("PROBE_AZ_ID");
    }

    #[test]
    fn write_to_dir_lands_a_parseable_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("mantis_manifest_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let m = NetworkRunManifest::from_env();
        let path = m.write_to_dir(&dir).expect("write manifest");
        assert!(path.ends_with(MANIFEST_FILENAME));
        let body = std::fs::read_to_string(&path).expect("read back");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(parsed["ordering"], "timeboost_normal_lane");
        assert_eq!(parsed["lane"], "normal_non_express");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lane_constants_match_the_submit_tag_contract() {
        // The submit agent tags each result with these exact strings; keep them stable.
        assert_eq!(LANE_NORMAL, "normal_non_express");
        assert_eq!(LANE_EXPRESS, "express");
    }
}
