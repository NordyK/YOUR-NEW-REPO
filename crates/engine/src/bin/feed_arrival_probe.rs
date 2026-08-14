//! Sequence-matched application feed-arrival evidence producer.
//!
//! The probe consumes the production local Nitro relay boundary. It never connects directly to the
//! public feed and never submits a transaction. Network access is guarded by an exact opt-in token.

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::fd::AsRawFd;
#[cfg(test)]
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use nix::sched::{sched_getaffinity, sched_setaffinity, CpuSet};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};
use url::Url;

const MANIFEST_SCHEMA: &str = "mantis.feed-arrival-manifest.v1";
const ARTIFACT_SCHEMA: &str = "mantis.feed-arrival.v1";
const PRODUCER: &str = "feed-arrival-probe/rust-v1";
const BOUNDARY: &str = "userspace_complete_ws_message_rx_v1";
const SEMANTIC_HASH: &str = "sha256_seq_header_block_decoded_l2msg_v1";
const OPT_IN_ENV: &str = "MANTIS_FEED_ARRIVAL_OPT_IN";
const OPT_IN_TOKEN: &str = "READ_ONLY_PUBLIC_FEED";
const MANIFEST_ENV: &str = "FEED_ARRIVAL_MANIFEST_FILE";
const BINARY_SHA_ENV: &str = "FEED_ARRIVAL_BINARY_SHA256";
const SOURCE_PROVENANCE_ENV: &str = "FEED_ARRIVAL_SOURCE_PROVENANCE_FILE";
const CONFIG_ENV: &str = "FEED_ARRIVAL_CONFIG_FILE";
const HOST_IDENTITY_ENV: &str = "FEED_ARRIVAL_HOST_IDENTITY_FILE";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Placement {
    region: String,
    az_name: String,
    az_id: String,
    subnet_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Topology {
    measurement_boundary: String,
    relay_endpoint: String,
    public_feed_host: String,
    public_feed_port: u16,
    relay_pin_ip: String,
    feed_peer_set: Vec<String>,
    feed_peer_set_sha256: String,
    relay_image: String,
    input_compression: bool,
    output_compression: bool,
    relay_cpu: usize,
    probe_cpu: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClockPolicy {
    expected_source: String,
    error_formula: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentAuthorityArchive {
    bundle_relpath: String,
    config_relpath: String,
    report_relpath: String,
    final_relpath: String,
    aggregator_relpath: String,
    results_relpath: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentAuthority {
    schema: String,
    campaign_id: String,
    winning_region: String,
    report_sha256: String,
    bundle_final_sha256: String,
    config_sha256: String,
    source_head: String,
    source_manifest_sha256: String,
    feed_peer_set_sha256: String,
    relay_pin_ip: String,
    screen_mode: String,
    instance_type: String,
    archive: ParentAuthorityArchive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    screen_mode: String,
    campaign_id: String,
    window_id: String,
    probe_start_epoch: u64,
    probe_window_secs: u64,
    warmup_secs: u64,
    warmup_min_contiguous: u64,
    source_head: String,
    source_manifest_sha256: String,
    probe_sha256: String,
    config_sha256: String,
    instance_type: String,
    ami_release: String,
    kernel_release: String,
    selection_scope: String,
    replicas_per_cell: u64,
    fresh_host_holdout: bool,
    parent_authority: Option<ParentAuthority>,
    minimum_matched_sequences: u64,
    minimum_decisive_win_rate_bps: u64,
    practical_margin_ns: u64,
    max_clock_error_ns: u64,
    max_missing_losses_per_window: u64,
    max_reconnects_per_window: u64,
    max_gaps_per_window: u64,
    max_hash_conflicts_per_window: u64,
    clock_step_tolerance_ns: u64,
    max_timestamp_pair_span_ns: u64,
    start_tolerance_ms: u64,
    finish_grace_secs: u64,
    required_windows: Vec<String>,
    semantic_hash: String,
    topology: Topology,
    clock: ClockPolicy,
    placements: Vec<Placement>,
}

impl Manifest {
    fn validate(&self) -> Result<()> {
        if self.schema != MANIFEST_SCHEMA {
            bail!("manifest schema must be {MANIFEST_SCHEMA}");
        }
        if !matches!(self.screen_mode.as_str(), "region_screen" | "az_deep_dive") {
            bail!("screen_mode must be region_screen or az_deep_dive");
        }
        if self.campaign_id.is_empty() || self.window_id.is_empty() {
            bail!("campaign_id and window_id are required");
        }
        if self.probe_window_secs < 300 {
            bail!("probe_window_secs must be at least 300");
        }
        if self.warmup_secs < 30 || self.warmup_min_contiguous < 100 {
            bail!("warm connection requires >=30 seconds and >=100 contiguous envelopes");
        }
        validate_hex(&self.source_head, 40, "source_head")?;
        validate_hex(&self.source_manifest_sha256, 64, "source_manifest_sha256")?;
        validate_hex(&self.probe_sha256, 64, "probe_sha256")?;
        validate_hex(&self.config_sha256, 64, "config_sha256")?;
        if self.screen_mode == "az_deep_dive" && self.instance_type != "c8gn.2xlarge" {
            bail!("az_deep_dive requires exact c8gn.2xlarge");
        }
        if self.screen_mode == "region_screen" && self.instance_type != "c7g.large" {
            bail!("region_screen requires fixed c7g.large");
        }
        if self.selection_scope != "sampled_candidate_only"
            || self.replicas_per_cell != 1
            || self.fresh_host_holdout
        {
            bail!("current controller is candidate-only: one replica and same-host holdout");
        }
        match (&self.screen_mode[..], &self.parent_authority) {
            ("region_screen", None) => {}
            ("az_deep_dive", Some(parent)) => {
                validate_hex(&parent.report_sha256, 64, "parent report_sha256")?;
                validate_hex(
                    &parent.bundle_final_sha256,
                    64,
                    "parent bundle_final_sha256",
                )?;
                validate_hex(&parent.config_sha256, 64, "parent config_sha256")?;
                if parent.schema != "mantis.feed-region-authority.v1"
                    || parent.screen_mode != "region_screen"
                    || parent.instance_type != "c7g.large"
                    || parent.source_head != self.source_head
                    || parent.source_manifest_sha256 != self.source_manifest_sha256
                    || parent.feed_peer_set_sha256 != self.topology.feed_peer_set_sha256
                    || parent.relay_pin_ip != self.topology.relay_pin_ip
                    || self
                        .placements
                        .iter()
                        .any(|placement| placement.region != parent.winning_region)
                {
                    bail!("AZ parent authority differs from frozen source/peer/region scope");
                }
            }
            _ => {
                bail!("parent_authority must be absent for region screen and present for AZ stage")
            }
        }
        if self.minimum_matched_sequences < 100
            || self.minimum_decisive_win_rate_bps == 0
            || self.minimum_decisive_win_rate_bps > 10_000
            || self.max_clock_error_ns == 0
            || self.max_timestamp_pair_span_ns == 0
            || self.finish_grace_secs > 60
            || self.max_reconnects_per_window != 0
            || self.max_gaps_per_window != 0
            || self.max_hash_conflicts_per_window != 0
        {
            bail!("invalid evidence thresholds");
        }
        if self.required_windows != ["w1", "w2", "w3", "holdout"] {
            bail!("required_windows must be w1,w2,w3,holdout");
        }
        if self.semantic_hash != SEMANTIC_HASH {
            bail!("unsupported semantic_hash");
        }
        if self.topology.measurement_boundary != BOUNDARY
            || self.topology.relay_endpoint != "ws://127.0.0.1:9642/feed"
            || self.topology.public_feed_host != "arb1-feed.arbitrum.io"
            || self.topology.public_feed_port != 443
            || !self.topology.input_compression
            || self.topology.output_compression
        {
            bail!("topology differs from the production Nitro relay boundary");
        }
        self.topology
            .relay_pin_ip
            .parse::<Ipv4Addr>()
            .context("relay_pin_ip must be IPv4")?;
        if self.topology.feed_peer_set.is_empty()
            || !self
                .topology
                .feed_peer_set
                .iter()
                .any(|value| value == &self.topology.relay_pin_ip)
        {
            bail!("feed_peer_set must contain relay_pin_ip");
        }
        let mut peers = self.topology.feed_peer_set.clone();
        for peer in &peers {
            let canonical = peer.parse::<Ipv4Addr>().context("feed peer must be IPv4")?;
            if canonical.to_string() != *peer {
                bail!("feed peer must use canonical IPv4 spelling");
            }
        }
        peers.sort();
        peers.dedup();
        if peers != self.topology.feed_peer_set {
            bail!("feed_peer_set must be sorted and unique");
        }
        validate_hex(
            &self.topology.feed_peer_set_sha256,
            64,
            "feed_peer_set_sha256",
        )?;
        let peer_value = serde_json::to_value(&self.topology.feed_peer_set)?;
        if sha256_hex(&canonical_json_bytes(&peer_value)?) != self.topology.feed_peer_set_sha256 {
            bail!("feed_peer_set_sha256 mismatch");
        }
        if self.topology.relay_cpu == self.topology.probe_cpu {
            bail!("relay and probe CPU affinity must be distinct");
        }
        let Some((_, digest)) = self.topology.relay_image.rsplit_once("@sha256:") else {
            bail!("relay_image must be digest-pinned");
        };
        validate_hex(digest, 64, "relay image digest")?;
        if self.clock.expected_source != "169.254.169.123"
            || self.clock.error_formula
                != "abs_system_time_plus_root_dispersion_plus_half_root_delay_v1"
        {
            bail!("clock policy must use AWS Time Sync and the frozen error formula");
        }
        if self.placements.len() < 2 {
            bail!("at least two placements are required");
        }
        let mut unique = self.placements.clone();
        unique.sort_by(|left, right| left.az_id.cmp(&right.az_id));
        unique.dedup_by(|left, right| left.az_id == right.az_id);
        if unique.len() != self.placements.len() {
            bail!("placement AZ IDs must be unique");
        }
        let mut per_region: HashMap<&str, usize> = HashMap::new();
        for placement in &self.placements {
            *per_region.entry(&placement.region).or_default() += 1;
        }
        if self.screen_mode == "region_screen" {
            let counts: Vec<_> = per_region.values().copied().collect();
            if counts.len() < 2
                || counts.iter().any(|count| *count < 2)
                || counts.iter().any(|count| *count != counts[0])
            {
                bail!("region_screen requires equal AZ sampling >=2 per region");
            }
        } else if per_region.len() != 1 {
            bail!("az_deep_dive must contain one region");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
struct HostIdentity {
    region: String,
    az_name: String,
    az_id: String,
    subnet_id: String,
    instance_id: String,
    instance_type: String,
    ami_id: String,
    kernel_release: String,
    effective_probe_cpus: Vec<usize>,
    effective_relay_cpus: Vec<usize>,
}

impl HostIdentity {
    fn from_imdsv2(
        manifest: &Manifest,
        effective_probe_cpus: Vec<usize>,
        identity: &Value,
    ) -> Result<Self> {
        let relay_cpu: usize = required_env("FEED_ARRIVAL_RELAY_EFFECTIVE_CPU")?
            .parse()
            .context("FEED_ARRIVAL_RELAY_EFFECTIVE_CPU must be an integer")?;
        if require_json_string(identity, "schema", "host identity")?
            != "mantis.aws-imdsv2-host-identity.v1"
            || require_json_string(identity, "metadata_version", "host identity")? != "imdsv2"
        {
            bail!("host identity must be archived from IMDSv2");
        }
        let document = identity
            .get("document")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("host identity document is absent"))?;
        let document = Value::Object(document.clone());
        if require_json_string(&document, "architecture", "identity document")? != "arm64"
            || require_json_string(identity, "pkcs7", "host identity")?.len() < 64
        {
            bail!("host identity architecture/signature is invalid");
        }
        let kernel_release = command_text("uname", &["-r"]);
        if kernel_release.starts_with("ERROR") {
            bail!("cannot read local kernel release");
        }
        let host = Self {
            region: require_json_string(&document, "region", "identity document")?.into(),
            az_name: require_json_string(&document, "availabilityZone", "identity document")?
                .into(),
            az_id: require_json_string(identity, "availability_zone_id", "host identity")?.into(),
            subnet_id: require_json_string(identity, "subnet_id", "host identity")?.into(),
            instance_id: require_json_string(&document, "instanceId", "identity document")?.into(),
            instance_type: require_json_string(&document, "instanceType", "identity document")?
                .into(),
            ami_id: require_json_string(&document, "imageId", "identity document")?.into(),
            kernel_release,
            effective_probe_cpus,
            effective_relay_cpus: vec![relay_cpu],
        };
        if host.instance_type != manifest.instance_type
            || host.kernel_release != manifest.kernel_release
        {
            bail!("host type/kernel differs from frozen manifest");
        }
        if host.effective_probe_cpus != [manifest.topology.probe_cpu]
            || host.effective_relay_cpus != [manifest.topology.relay_cpu]
        {
            bail!("effective probe/relay affinity differs from frozen topology");
        }
        let placement_matches = manifest.placements.iter().any(|placement| {
            placement.region == host.region
                && placement.az_name == host.az_name
                && placement.az_id == host.az_id
                && placement.subnet_id == host.subnet_id
        });
        if !placement_matches {
            bail!("host placement is absent from frozen manifest");
        }
        Ok(host)
    }
}

#[derive(Debug, Clone, Serialize)]
struct TimestampPair {
    realtime_ns: u64,
    monotonic_raw_ns: u64,
    pair_span_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ClockSnapshot {
    snapshot_index: u64,
    realtime_ns: u64,
    monotonic_raw_ns: u64,
    pair_span_ns: u64,
    system_time_offset_ns: i64,
    root_delay_ns: u64,
    root_dispersion_ns: u64,
    skew_ppb: u64,
    error_bound_ns: u64,
    leap_normal: bool,
    selected_expected_source: bool,
    tracking: String,
    sources: String,
    sourcestats: String,
    clockbound: String,
}

#[derive(Debug, Clone, Serialize)]
struct ConnectionRecord {
    generation: u64,
    connected_realtime_ns: u64,
    connected_raw_ns: u64,
    disconnected_realtime_ns: Option<u64>,
    disconnected_raw_ns: Option<u64>,
    failure: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Observation {
    frame_ordinal: u64,
    sequence_number: u64,
    semantic_payload_hash: String,
    recv_realtime_ns: u64,
    recv_monotonic_raw_ns: u64,
    timestamp_pair_span_ns: u64,
    connection_generation: u64,
    frame_bytes: usize,
    measurement_eligible: bool,
    duplicate: bool,
    reordered: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Counters {
    observations: u64,
    warmup_observations: u64,
    window_observations: u64,
    eligible_observations: u64,
    duplicates: u64,
    reordered: u64,
    gaps: u64,
    reconnects: u64,
    parse_errors: u64,
    hash_conflicts: u64,
}

#[derive(Debug, Clone, Serialize)]
struct WarmupEvidence {
    required_secs: u64,
    required_contiguous: u64,
    ready_before_window: bool,
    ready_realtime_ns: Option<u64>,
    ready_raw_ns: Option<u64>,
    ready_generation: Option<u64>,
}

#[derive(Debug)]
struct SequenceState {
    generation: u64,
    connected_raw_ns: u64,
    last_sequence: Option<u64>,
    contiguous: u64,
    first_hash_by_sequence: HashMap<u64, [u8; 32]>,
    warm_ready: bool,
}

impl SequenceState {
    fn new(generation: u64, connected_raw_ns: u64) -> Self {
        Self {
            generation,
            connected_raw_ns,
            last_sequence: None,
            contiguous: 0,
            first_hash_by_sequence: HashMap::new(),
            warm_ready: false,
        }
    }

    fn classify(
        &mut self,
        sequence: u64,
        payload_hash: [u8; 32],
        recv_raw_ns: u64,
        manifest: &Manifest,
    ) -> SequenceClassification {
        let prior = self.first_hash_by_sequence.get(&sequence).copied();
        let duplicate = prior == Some(payload_hash);
        let hash_conflict = prior.is_some_and(|hash| hash != payload_hash);
        self.first_hash_by_sequence
            .entry(sequence)
            .or_insert(payload_hash);
        let reordered = self.last_sequence.is_some_and(|last| sequence < last);
        let gap = self
            .last_sequence
            .is_some_and(|last| sequence > last.saturating_add(1));
        match self.last_sequence {
            None => self.contiguous = 1,
            Some(last) if sequence == last.saturating_add(1) => {
                self.contiguous = self.contiguous.saturating_add(1);
            }
            Some(last) if sequence > last => self.contiguous = 1,
            Some(_) => {}
        }
        self.last_sequence = Some(
            self.last_sequence
                .map_or(sequence, |last| last.max(sequence)),
        );
        let warm_duration_ns = manifest.warmup_secs.saturating_mul(1_000_000_000);
        if !self.warm_ready
            && recv_raw_ns.saturating_sub(self.connected_raw_ns) >= warm_duration_ns
            && self.contiguous >= manifest.warmup_min_contiguous
        {
            self.warm_ready = true;
        }
        SequenceClassification {
            duplicate,
            reordered,
            gap,
            hash_conflict,
            warm_ready: self.warm_ready,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SequenceClassification {
    duplicate: bool,
    reordered: bool,
    gap: bool,
    hash_conflict: bool,
    warm_ready: bool,
}

#[derive(Debug)]
struct ProbeState {
    observations: Vec<Observation>,
    connections: Vec<ConnectionRecord>,
    counters: Counters,
    warmup: WarmupEvidence,
    sequence: Option<SequenceState>,
    frame_ordinal: u64,
}

impl ProbeState {
    fn new(manifest: &Manifest) -> Self {
        Self {
            observations: Vec::new(),
            connections: Vec::new(),
            counters: Counters::default(),
            warmup: WarmupEvidence {
                required_secs: manifest.warmup_secs,
                required_contiguous: manifest.warmup_min_contiguous,
                ready_before_window: false,
                ready_realtime_ns: None,
                ready_raw_ns: None,
                ready_generation: None,
            },
            sequence: None,
            frame_ordinal: 0,
        }
    }

    fn connected(&mut self, generation: u64, pair: &TimestampPair) {
        self.connections.push(ConnectionRecord {
            generation,
            connected_realtime_ns: pair.realtime_ns,
            connected_raw_ns: pair.monotonic_raw_ns,
            disconnected_realtime_ns: None,
            disconnected_raw_ns: None,
            failure: None,
        });
        self.sequence = Some(SequenceState::new(generation, pair.monotonic_raw_ns));
    }

    fn disconnected(&mut self, pair: &TimestampPair, failure: String, in_window: bool) {
        if let Some(connection) = self.connections.last_mut() {
            connection.disconnected_realtime_ns = Some(pair.realtime_ns);
            connection.disconnected_raw_ns = Some(pair.monotonic_raw_ns);
            connection.failure = Some(failure);
        }
        if in_window {
            self.counters.reconnects = self.counters.reconnects.saturating_add(1);
        }
    }
}

#[derive(Serialize)]
struct Artifact {
    schema: &'static str,
    producer: &'static str,
    manifest: Manifest,
    manifest_sha256: String,
    manifest_content_sha256: String,
    manifest_raw_base64: String,
    binary_sha256: String,
    source_provenance: Value,
    source_provenance_file_sha256: String,
    source_provenance_raw_base64: String,
    config: Value,
    config_raw_base64: String,
    host_identity: Value,
    host_identity_sha256: String,
    host_identity_raw_base64: String,
    measurement_boundary: &'static str,
    semantic_hash: &'static str,
    host: HostIdentity,
    probe_actual_start_realtime_ns: u64,
    probe_actual_start_raw_ns: u64,
    probe_actual_finish_realtime_ns: u64,
    probe_actual_finish_raw_ns: u64,
    window_actual_start_realtime_ns: u64,
    window_actual_start_raw_ns: u64,
    window_actual_start_skew_ns: i64,
    window_raw_end_ns: u64,
    clock_snapshots: Vec<ClockSnapshot>,
    connections: Vec<ConnectionRecord>,
    warmup: WarmupEvidence,
    counters: Counters,
    observations: Vec<Observation>,
}

fn validate_hex(value: &str, length: usize, label: &str) -> Result<()> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} must be {length} lowercase hex characters");
    }
    Ok(())
}

fn required_env(key: &str) -> Result<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn exact_opt_in(value: Option<&str>) -> bool {
    value == Some(OPT_IN_TOKEN)
}

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn timestamp_pair() -> TimestampPair {
    let before = engine_sysopt::clock_monotonic_raw_ns();
    let realtime_ns = wall_ns();
    let after = engine_sysopt::clock_monotonic_raw_ns();
    TimestampPair {
        realtime_ns,
        monotonic_raw_ns: before.saturating_add(after.saturating_sub(before) / 2),
        pair_span_ns: after.saturating_sub(before),
    }
}

fn command_text(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(output) => format!(
            "ERROR status={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!("ERROR {error}"),
    }
}

fn leap_is_normal(tracking: &str) -> bool {
    tracking.lines().any(|line| {
        line.split_once(':')
            .is_some_and(|(label, value)| label.trim() == "Leap status" && value.trim() == "Normal")
    })
}

fn expected_source_selected(sources: &str, expected: &str) -> bool {
    let mut selected = sources.lines().filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        (fields.len() >= 2 && fields[0].ends_with('*')).then_some(fields[1])
    });
    selected.next() == Some(expected) && selected.next().is_none()
}

fn tracking_number(tracking: &str, label: &str) -> Result<f64> {
    let value = tracking
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(candidate, _)| candidate.trim() == label)
                .map(|(_, value)| value.trim())
        })
        .ok_or_else(|| anyhow!("chrony tracking is missing {label}"))?;
    let number = value
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("chrony {label} has no numeric value"))?
        .parse::<f64>()
        .with_context(|| format!("parse chrony {label}"))?;
    if !number.is_finite() {
        bail!("chrony {label} is non-finite");
    }
    Ok(number)
}

fn seconds_to_signed_ns(value: f64, label: &str) -> Result<i64> {
    let ns = value * 1_000_000_000.0;
    if ns < i64::MIN as f64 || ns > i64::MAX as f64 {
        bail!("chrony {label} exceeds signed nanosecond range");
    }
    Ok(ns.round() as i64)
}

fn seconds_to_ns(value: f64, label: &str) -> Result<u64> {
    if value < 0.0 {
        bail!("chrony {label} must be non-negative");
    }
    let ns = value * 1_000_000_000.0;
    if ns > u64::MAX as f64 {
        bail!("chrony {label} exceeds nanosecond range");
    }
    Ok(ns.round() as u64)
}

fn clock_snapshot(index: u64, expected_source: &str) -> Result<ClockSnapshot> {
    let pair = timestamp_pair();
    let tracking = command_text("chronyc", &["-n", "tracking"]);
    let sources = command_text("chronyc", &["-n", "sources", "-v"]);
    let sourcestats = command_text("chronyc", &["-n", "sourcestats"]);
    let clockbound = command_text("clockbound", &["status"]);
    let system_time_offset_ns =
        seconds_to_signed_ns(tracking_number(&tracking, "System time")?, "System time")?;
    let root_delay_ns = seconds_to_ns(tracking_number(&tracking, "Root delay")?, "Root delay")?;
    let root_dispersion_ns = seconds_to_ns(
        tracking_number(&tracking, "Root dispersion")?,
        "Root dispersion",
    )?;
    let skew_ppm = tracking_number(&tracking, "Skew")?;
    if skew_ppm < 0.0 || skew_ppm * 1_000.0 > u64::MAX as f64 {
        bail!("chrony Skew is outside ppb range");
    }
    let skew_ppb = (skew_ppm * 1_000.0).ceil() as u64;
    let error_bound_ns = system_time_offset_ns
        .unsigned_abs()
        .saturating_add(root_dispersion_ns)
        .saturating_add(root_delay_ns.saturating_add(1) / 2);
    Ok(ClockSnapshot {
        snapshot_index: index,
        realtime_ns: pair.realtime_ns,
        monotonic_raw_ns: pair.monotonic_raw_ns,
        pair_span_ns: pair.pair_span_ns,
        system_time_offset_ns,
        root_delay_ns,
        root_dispersion_ns,
        skew_ppb,
        error_bound_ns,
        leap_normal: leap_is_normal(&tracking),
        selected_expected_source: expected_source_selected(&sources, expected_source),
        tracking,
        sources,
        sourcestats,
        clockbound,
    })
}

fn semantic_payload_hash(
    sequence: u64,
    header_timestamp: u64,
    block_number: u64,
    decoded_l2msg: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"mantis-feed-semantic-v1\0");
    hasher.update(sequence.to_be_bytes());
    hasher.update(header_timestamp.to_be_bytes());
    hasher.update(block_number.to_be_bytes());
    hasher.update(decoded_l2msg);
    hasher.finalize().into()
}

fn hex32(value: &[u8; 32]) -> String {
    let mut output = vec![0u8; 64];
    faster_hex::hex_encode(value, &mut output).expect("fixed hex buffer");
    String::from_utf8(output).expect("hex is ASCII")
}

fn connect_local(endpoint: &str) -> Result<WebSocket<TcpStream>> {
    let url = Url::parse(endpoint).context("parse relay endpoint")?;
    if url.scheme() != "ws"
        || url.host_str() != Some("127.0.0.1")
        || url.port_or_known_default() != Some(9642)
        || url.path() != "/feed"
    {
        bail!("probe endpoint is not the frozen local Nitro relay");
    }
    let tcp = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9642).into(),
        Duration::from_secs(2),
    )
    .context("connect local Nitro relay")?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_millis(250)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
    let fd = tcp.as_raw_fd();
    let _ = engine_sysopt::set_so_busy_poll(fd, 50);
    let _ = engine_sysopt::set_so_rcvbuf(fd, 8 * 1024 * 1024);
    let request = endpoint.into_client_request()?;
    let (websocket, _response) = tungstenite::client(request, tcp)?;
    Ok(websocket)
}

fn raw_window_contains(pair: &TimestampPair, start: &AtomicU64, end: &AtomicU64) -> bool {
    let start = start.load(Ordering::Acquire);
    let end = end.load(Ordering::Acquire);
    start != 0 && pair.monotonic_raw_ns >= start && pair.monotonic_raw_ns < end
}

fn process_frame(
    frame: &[u8],
    pair: &TimestampPair,
    manifest: &Manifest,
    state: &mut ProbeState,
    raw_window_start: &AtomicU64,
    raw_window_end: &AtomicU64,
) {
    state.frame_ordinal = state.frame_ordinal.saturating_add(1);
    let frame_ordinal = state.frame_ordinal;
    let window_start = raw_window_start.load(Ordering::Acquire);
    let window_end = raw_window_end.load(Ordering::Acquire);
    let in_window = raw_window_contains(pair, raw_window_start, raw_window_end);
    let mut envelopes = 0u64;
    for envelope in engine_feed::iter_envelopes(frame) {
        envelopes = envelopes.saturating_add(1);
        let mut decoded = envelope.l2msg_b64.to_vec();
        let decoded_len = match base64_simd::forgiving_decode_inplace(&mut decoded) {
            Ok(value) => value.len(),
            Err(_) => {
                if in_window {
                    state.counters.parse_errors = state.counters.parse_errors.saturating_add(1);
                }
                continue;
            }
        };
        let payload_hash = semantic_payload_hash(
            envelope.sequence_number,
            envelope.timestamp,
            envelope.block_number,
            &decoded[..decoded_len],
        );
        let Some(sequence) = state.sequence.as_mut() else {
            if in_window {
                state.counters.parse_errors = state.counters.parse_errors.saturating_add(1);
            }
            continue;
        };
        let classification = sequence.classify(
            envelope.sequence_number,
            payload_hash,
            pair.monotonic_raw_ns,
            manifest,
        );
        if classification.warm_ready && !state.warmup.ready_before_window && window_start == 0 {
            state.warmup.ready_before_window = true;
            state.warmup.ready_realtime_ns = Some(pair.realtime_ns);
            state.warmup.ready_raw_ns = Some(pair.monotonic_raw_ns);
            state.warmup.ready_generation = Some(sequence.generation);
        }
        let eligible = in_window
            && state.warmup.ready_before_window
            && state.warmup.ready_generation == Some(sequence.generation);
        state.counters.observations = state.counters.observations.saturating_add(1);
        if window_start == 0 || pair.monotonic_raw_ns < window_start {
            state.counters.warmup_observations =
                state.counters.warmup_observations.saturating_add(1);
        } else if window_end != 0 && pair.monotonic_raw_ns < window_end {
            state.counters.window_observations =
                state.counters.window_observations.saturating_add(1);
        }
        if eligible {
            state.counters.eligible_observations =
                state.counters.eligible_observations.saturating_add(1);
            if classification.duplicate {
                state.counters.duplicates = state.counters.duplicates.saturating_add(1);
            }
            if classification.reordered {
                state.counters.reordered = state.counters.reordered.saturating_add(1);
            }
            if classification.gap {
                state.counters.gaps = state.counters.gaps.saturating_add(1);
            }
            if classification.hash_conflict {
                state.counters.hash_conflicts = state.counters.hash_conflicts.saturating_add(1);
            }
        }
        state.observations.push(Observation {
            frame_ordinal,
            sequence_number: envelope.sequence_number,
            semantic_payload_hash: hex32(&payload_hash),
            recv_realtime_ns: pair.realtime_ns,
            recv_monotonic_raw_ns: pair.monotonic_raw_ns,
            timestamp_pair_span_ns: pair.pair_span_ns,
            connection_generation: sequence.generation,
            frame_bytes: frame.len(),
            measurement_eligible: eligible,
            duplicate: eligible && classification.duplicate,
            reordered: eligible && classification.reordered,
        });
    }
    if envelopes == 0 && in_window {
        state.counters.parse_errors = state.counters.parse_errors.saturating_add(1);
    }
}

fn reader_loop(
    manifest: Manifest,
    state: Arc<Mutex<ProbeState>>,
    shutdown: Arc<AtomicBool>,
    raw_window_start: Arc<AtomicU64>,
    raw_window_end: Arc<AtomicU64>,
) {
    let mut generation = 0u64;
    while !shutdown.load(Ordering::Acquire) {
        match connect_local(&manifest.topology.relay_endpoint) {
            Ok(mut websocket) => {
                let connected = timestamp_pair();
                if let Ok(mut guard) = state.lock() {
                    guard.connected(generation, &connected);
                }
                let mut failure = "websocket_closed".to_string();
                loop {
                    if shutdown.load(Ordering::Acquire) {
                        failure = "probe_shutdown".to_string();
                        break;
                    }
                    match websocket.read() {
                        Ok(Message::Text(text)) => {
                            let pair = timestamp_pair();
                            if let Ok(mut guard) = state.lock() {
                                process_frame(
                                    text.as_bytes(),
                                    &pair,
                                    &manifest,
                                    &mut guard,
                                    &raw_window_start,
                                    &raw_window_end,
                                );
                            }
                        }
                        Ok(Message::Binary(bytes)) => {
                            let pair = timestamp_pair();
                            if let Ok(mut guard) = state.lock() {
                                process_frame(
                                    &bytes,
                                    &pair,
                                    &manifest,
                                    &mut guard,
                                    &raw_window_start,
                                    &raw_window_end,
                                );
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            if let Err(error) = websocket.send(Message::Pong(payload)) {
                                failure = format!("pong:{error}");
                                break;
                            }
                        }
                        Ok(Message::Pong(_) | Message::Frame(_)) => {}
                        Ok(Message::Close(_)) => break,
                        Err(tungstenite::Error::Io(error))
                            if matches!(
                                error.kind(),
                                ErrorKind::TimedOut | ErrorKind::WouldBlock
                            ) => {}
                        Err(error) => {
                            failure = format!("read:{error}");
                            break;
                        }
                    }
                }
                let disconnected = timestamp_pair();
                let in_window =
                    raw_window_contains(&disconnected, &raw_window_start, &raw_window_end);
                if let Ok(mut guard) = state.lock() {
                    guard.disconnected(&disconnected, failure, in_window);
                }
                let _ = websocket.close(None);
            }
            Err(error) => {
                eprintln!("feed-arrival-probe: local relay connect failed: {error:#}");
            }
        }
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        generation = generation.saturating_add(1);
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_until_wall(target_ns: u64) {
    loop {
        let remaining = target_ns.saturating_sub(wall_ns());
        if remaining == 0 {
            break;
        }
        std::thread::sleep(Duration::from_nanos(remaining.min(50_000_000)));
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    hex32(&digest)
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => serde_json::to_writer(output, value)?,
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key)?;
                output.push(b':');
                write_canonical_json(&values[key], output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    write_canonical_json(value, &mut output)?;
    Ok(output)
}

fn pin_probe_cpu(cpu: usize) -> Result<Vec<usize>> {
    let mut requested = CpuSet::new();
    requested.set(cpu).context("build probe CPU affinity set")?;
    sched_setaffinity(Pid::from_raw(0), &requested).context("apply probe CPU affinity")?;
    let effective = sched_getaffinity(Pid::from_raw(0)).context("read effective probe affinity")?;
    let mut cpus = Vec::new();
    for candidate in 0..libc::CPU_SETSIZE as usize {
        if effective.is_set(candidate).unwrap_or(false) {
            cpus.push(candidate);
        }
    }
    if cpus != [cpu] {
        bail!("effective probe CPU affinity is {cpus:?}, expected [{cpu}]");
    }
    Ok(cpus)
}

fn parse_json_file(path_env: &str, label: &str) -> Result<(Vec<u8>, Value)> {
    let path = required_env(path_env)?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {label}"))?;
    let value = serde_json::from_slice(&bytes).with_context(|| format!("parse {label}"))?;
    Ok((bytes, value))
}

fn require_json_string<'a>(value: &'a Value, key: &str, label: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{label}.{key} must be a non-empty string"))
}

fn validate_source_and_config(manifest: &Manifest) -> Result<(Vec<u8>, Value, Vec<u8>, Value)> {
    let (source_bytes, source) = parse_json_file(SOURCE_PROVENANCE_ENV, "source provenance")?;
    if require_json_string(&source, "schema", "source provenance")? != "mantis_gitless_source_v2"
        || require_json_string(&source, "head_commit", "source provenance")? != manifest.source_head
        || require_json_string(&source, "source_manifest_sha256", "source provenance")?
            != manifest.source_manifest_sha256
    {
        bail!("source provenance differs from frozen source head/digest");
    }
    let (config_bytes, config) = parse_json_file(CONFIG_ENV, "feed campaign config")?;
    if sha256_hex(&config_bytes) != manifest.config_sha256 {
        bail!("feed campaign config SHA-256 differs from manifest");
    }
    if require_json_string(&config, "schema", "feed campaign config")?
        != "mantis.feed-az-campaign.v1"
        || require_json_string(&config, "campaign_id", "feed campaign config")?
            != manifest.campaign_id
        || require_json_string(&config, "source_head", "feed campaign config")?
            != manifest.source_head
        || require_json_string(&config, "source_manifest_sha256", "feed campaign config")?
            != manifest.source_manifest_sha256
        || require_json_string(&config, "relay_pin_ip", "feed campaign config")?
            != manifest.topology.relay_pin_ip
        || require_json_string(&config, "peer_scope", "feed campaign config")?
            != "operator_selected_pin"
    {
        bail!("feed campaign config identity/topology differs from manifest");
    }
    Ok((source_bytes, source, config_bytes, config))
}

fn run() -> Result<()> {
    if !exact_opt_in(std::env::var(OPT_IN_ENV).ok().as_deref()) {
        bail!(
            "feed arrival probe is default-off; no network request was attempted. Set \
             {OPT_IN_ENV}={OPT_IN_TOKEN} for the read-only local-relay measurement"
        );
    }
    let manifest_path = required_env(MANIFEST_ENV)?;
    let manifest_bytes = std::fs::read(&manifest_path).context("read feed arrival manifest")?;
    let manifest_value: Value =
        serde_json::from_slice(&manifest_bytes).context("parse feed arrival manifest JSON")?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse feed arrival manifest")?;
    manifest.validate()?;
    let expected_binary_sha256 = required_env(BINARY_SHA_ENV)?;
    validate_hex(&expected_binary_sha256, 64, BINARY_SHA_ENV)?;
    let executable_bytes = std::fs::read("/proc/self/exe").context("hash /proc/self/exe")?;
    let binary_sha256 = sha256_hex(&executable_bytes);
    if binary_sha256 != expected_binary_sha256 || binary_sha256 != manifest.probe_sha256 {
        bail!("binary SHA-256 differs from frozen manifest");
    }
    let (source_bytes, source_provenance, config_bytes, config) =
        validate_source_and_config(&manifest)?;
    let (host_identity_bytes, host_identity) =
        parse_json_file(HOST_IDENTITY_ENV, "IMDSv2 host identity")?;
    let effective_probe_cpus = pin_probe_cpu(manifest.topology.probe_cpu)?;
    let host = HostIdentity::from_imdsv2(&manifest, effective_probe_cpus, &host_identity)?;
    let probe_started = timestamp_pair();
    let window_start_wall_ns = manifest.probe_start_epoch.saturating_mul(1_000_000_000);
    let minimum_lead_ns = manifest.warmup_secs.saturating_mul(1_000_000_000);
    if window_start_wall_ns.saturating_sub(probe_started.realtime_ns) < minimum_lead_ns {
        bail!("probe did not start early enough to satisfy the frozen warmup duration");
    }

    let state = Arc::new(Mutex::new(ProbeState::new(&manifest)));
    let shutdown = Arc::new(AtomicBool::new(false));
    let raw_window_start = Arc::new(AtomicU64::new(0));
    let raw_window_end = Arc::new(AtomicU64::new(0));
    let reader = {
        let manifest = manifest.clone();
        let state = state.clone();
        let shutdown = shutdown.clone();
        let raw_window_start = raw_window_start.clone();
        let raw_window_end = raw_window_end.clone();
        std::thread::Builder::new()
            .name("feed-arrival-reader".into())
            .spawn(move || {
                reader_loop(manifest, state, shutdown, raw_window_start, raw_window_end)
            })?
    };

    let mut snapshots = vec![clock_snapshot(0, &manifest.clock.expected_source)?];
    wait_until_wall(window_start_wall_ns);
    let shared_start = timestamp_pair();
    let raw_deadline = shared_start
        .monotonic_raw_ns
        .saturating_add(manifest.probe_window_secs.saturating_mul(1_000_000_000));
    raw_window_end.store(raw_deadline, Ordering::Release);
    raw_window_start.store(shared_start.monotonic_raw_ns, Ordering::Release);
    snapshots.push(clock_snapshot(1, &manifest.clock.expected_source)?);
    let mut next_snapshot = shared_start.monotonic_raw_ns.saturating_add(30_000_000_000);
    while engine_sysopt::clock_monotonic_raw_ns() < raw_deadline {
        let now = engine_sysopt::clock_monotonic_raw_ns();
        if now >= next_snapshot {
            snapshots.push(clock_snapshot(
                snapshots.len() as u64,
                &manifest.clock.expected_source,
            )?);
            next_snapshot = next_snapshot.saturating_add(30_000_000_000);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    snapshots.push(clock_snapshot(
        snapshots.len() as u64,
        &manifest.clock.expected_source,
    )?);
    shutdown.store(true, Ordering::Release);
    reader
        .join()
        .map_err(|_| anyhow!("feed reader thread panicked"))?;
    let finished = timestamp_pair();
    let state = Arc::try_unwrap(state)
        .map_err(|_| anyhow!("feed state still shared"))?
        .into_inner()
        .map_err(|_| anyhow!("feed state lock poisoned"))?;
    let start_skew = i128::from(shared_start.realtime_ns) - i128::from(window_start_wall_ns);
    let window_actual_start_skew_ns =
        i64::try_from(start_skew).context("window start skew exceeds signed range")?;
    let manifest_content_sha256 = sha256_hex(&canonical_json_bytes(&manifest_value)?);
    let artifact = Artifact {
        schema: ARTIFACT_SCHEMA,
        producer: PRODUCER,
        manifest,
        manifest_sha256: sha256_hex(&manifest_bytes),
        manifest_content_sha256,
        manifest_raw_base64: base64_simd::STANDARD.encode_to_string(&manifest_bytes),
        binary_sha256,
        source_provenance,
        source_provenance_file_sha256: sha256_hex(&source_bytes),
        source_provenance_raw_base64: base64_simd::STANDARD.encode_to_string(&source_bytes),
        config,
        config_raw_base64: base64_simd::STANDARD.encode_to_string(&config_bytes),
        host_identity,
        host_identity_sha256: sha256_hex(&host_identity_bytes),
        host_identity_raw_base64: base64_simd::STANDARD.encode_to_string(&host_identity_bytes),
        measurement_boundary: BOUNDARY,
        semantic_hash: SEMANTIC_HASH,
        host,
        probe_actual_start_realtime_ns: probe_started.realtime_ns,
        probe_actual_start_raw_ns: probe_started.monotonic_raw_ns,
        probe_actual_finish_realtime_ns: finished.realtime_ns,
        probe_actual_finish_raw_ns: finished.monotonic_raw_ns,
        window_actual_start_realtime_ns: shared_start.realtime_ns,
        window_actual_start_raw_ns: shared_start.monotonic_raw_ns,
        window_actual_start_skew_ns,
        window_raw_end_ns: raw_deadline,
        clock_snapshots: snapshots,
        connections: state.connections,
        warmup: state.warmup,
        counters: state.counters,
        observations: state.observations,
    };
    serde_json::to_writer(std::io::stdout().lock(), &artifact)?;
    std::io::stdout().lock().write_all(b"\n")?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("feed-arrival-probe: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest() -> Manifest {
        let placements = ["us-east-2", "us-east-1"]
            .into_iter()
            .flat_map(|region| {
                [1, 2].map(move |number| Placement {
                    region: region.to_string(),
                    az_name: format!("{region}{number}"),
                    az_id: format!("{region}-az{number}"),
                    subnet_id: format!("subnet-{region}-{number}"),
                })
            })
            .collect();
        let feed_peer_set = vec!["104.20.38.96".into(), "104.20.39.96".into()];
        let feed_peer_set_sha256 = sha256_hex(
            &canonical_json_bytes(&serde_json::to_value(&feed_peer_set).unwrap()).unwrap(),
        );
        Manifest {
            schema: MANIFEST_SCHEMA.into(),
            screen_mode: "region_screen".into(),
            campaign_id: "fixture-campaign".into(),
            window_id: "w1".into(),
            probe_start_epoch: 1_800_000_000,
            probe_window_secs: 300,
            warmup_secs: 30,
            warmup_min_contiguous: 100,
            source_head: "1".repeat(40),
            source_manifest_sha256: "5".repeat(64),
            probe_sha256: "2".repeat(64),
            config_sha256: "3".repeat(64),
            instance_type: "c7g.large".into(),
            ami_release: "al2023-fixture".into(),
            kernel_release: "6.1.0-fixture".into(),
            selection_scope: "sampled_candidate_only".into(),
            replicas_per_cell: 1,
            fresh_host_holdout: false,
            parent_authority: None,
            minimum_matched_sequences: 100,
            minimum_decisive_win_rate_bps: 6000,
            practical_margin_ns: 50_000,
            max_clock_error_ns: 100_000,
            max_missing_losses_per_window: 10,
            max_reconnects_per_window: 0,
            max_gaps_per_window: 0,
            max_hash_conflicts_per_window: 0,
            clock_step_tolerance_ns: 10_000,
            max_timestamp_pair_span_ns: 100_000,
            start_tolerance_ms: 250,
            finish_grace_secs: 5,
            required_windows: vec!["w1".into(), "w2".into(), "w3".into(), "holdout".into()],
            semantic_hash: SEMANTIC_HASH.into(),
            topology: Topology {
                measurement_boundary: BOUNDARY.into(),
                relay_endpoint: "ws://127.0.0.1:9642/feed".into(),
                public_feed_host: "arb1-feed.arbitrum.io".into(),
                public_feed_port: 443,
                relay_pin_ip: "104.20.39.96".into(),
                feed_peer_set,
                feed_peer_set_sha256,
                relay_image: format!("offchainlabs/nitro-node:v@sha256:{}", "a".repeat(64)),
                input_compression: true,
                output_compression: false,
                relay_cpu: 0,
                probe_cpu: 1,
            },
            clock: ClockPolicy {
                expected_source: "169.254.169.123".into(),
                error_formula: "abs_system_time_plus_root_dispersion_plus_half_root_delay_v1"
                    .into(),
            },
            placements,
        }
    }

    #[test]
    fn default_off_requires_exact_read_only_token() {
        for value in [
            None,
            Some(""),
            Some("1"),
            Some("true"),
            Some("READ_ONLY_PUBLIC_FEED "),
        ] {
            assert!(!exact_opt_in(value));
        }
        assert!(exact_opt_in(Some(OPT_IN_TOKEN)));
    }

    #[test]
    fn manifest_enforces_warm_topology_and_equal_region_sampling() {
        let manifest = test_manifest();
        assert!(manifest.validate().is_ok());
        let mut bad = manifest.clone();
        bad.warmup_secs = 29;
        assert!(bad.validate().is_err());
        let mut bad = manifest.clone();
        bad.topology.relay_endpoint = "wss://arb1-feed.arbitrum.io/feed".into();
        assert!(bad.validate().is_err());
        let mut bad = manifest;
        bad.placements.pop();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn az_stage_requires_exact_final_shape() {
        let mut manifest = test_manifest();
        manifest.screen_mode = "az_deep_dive".into();
        manifest
            .placements
            .retain(|placement| placement.region == "us-east-2");
        assert!(manifest.validate().is_err());
        manifest.instance_type = "c8gn.2xlarge".into();
        manifest.parent_authority = Some(ParentAuthority {
            schema: "mantis.feed-region-authority.v1".into(),
            campaign_id: "parent-campaign".into(),
            winning_region: "us-east-2".into(),
            report_sha256: "6".repeat(64),
            bundle_final_sha256: "7".repeat(64),
            config_sha256: "8".repeat(64),
            source_head: manifest.source_head.clone(),
            source_manifest_sha256: manifest.source_manifest_sha256.clone(),
            feed_peer_set_sha256: manifest.topology.feed_peer_set_sha256.clone(),
            relay_pin_ip: manifest.topology.relay_pin_ip.clone(),
            screen_mode: "region_screen".into(),
            instance_type: "c7g.large".into(),
            archive: ParentAuthorityArchive {
                bundle_relpath: "provenance/parent-authority/bundle".into(),
                config_relpath: "provenance/parent-authority/bundle/manifests/config.json".into(),
                report_relpath: "provenance/parent-authority/bundle/reports/feed-arrival.txt"
                    .into(),
                final_relpath: "provenance/parent-authority/bundle/reports/FINAL.sha256".into(),
                aggregator_relpath:
                    "provenance/parent-authority/bundle/provenance/feed_arrival_aggregate.py".into(),
                results_relpath: "provenance/parent-authority/bundle/results".into(),
            },
        });
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn semantic_hash_covers_sequence_header_block_and_decoded_payload() {
        let baseline = semantic_payload_hash(1, 2, 3, b"payload");
        assert_eq!(baseline, semantic_payload_hash(1, 2, 3, b"payload"));
        assert_ne!(baseline, semantic_payload_hash(2, 2, 3, b"payload"));
        assert_ne!(baseline, semantic_payload_hash(1, 3, 3, b"payload"));
        assert_ne!(baseline, semantic_payload_hash(1, 2, 4, b"payload"));
        assert_ne!(baseline, semantic_payload_hash(1, 2, 3, b"payload!"));
    }

    #[test]
    fn sequence_state_preserves_duplicate_reorder_gap_and_conflict() {
        let manifest = test_manifest();
        let connected = 1_000_000_000;
        let mut state = SequenceState::new(0, connected);
        let a = semantic_payload_hash(10, 1, 1, b"a");
        let b = semantic_payload_hash(11, 1, 1, b"b");
        let first = state.classify(10, a, connected, &manifest);
        assert!(!first.duplicate && !first.gap && !first.reordered && !first.hash_conflict);
        assert!(state.classify(10, a, connected, &manifest).duplicate);
        assert!(
            state
                .classify(
                    10,
                    semantic_payload_hash(10, 1, 1, b"different"),
                    connected,
                    &manifest
                )
                .hash_conflict
        );
        assert!(!state.classify(11, b, connected, &manifest).gap);
        assert!(
            state
                .classify(
                    13,
                    semantic_payload_hash(13, 1, 1, b"c"),
                    connected,
                    &manifest
                )
                .gap
        );
        assert!(state.classify(12, b, connected, &manifest).reordered);
    }

    #[test]
    fn warm_state_needs_both_time_and_one_hundred_contiguous_envelopes() {
        let manifest = test_manifest();
        let connected = 1_000_000_000;
        let mut state = SequenceState::new(0, connected);
        for sequence in 1..=100 {
            let now = if sequence == 100 {
                connected + 30_000_000_000
            } else {
                connected + 29_000_000_000
            };
            let result = state.classify(
                sequence,
                semantic_payload_hash(sequence, 1, 1, &[sequence as u8]),
                now,
                &manifest,
            );
            assert_eq!(result.warm_ready, sequence == 100);
        }
    }

    #[test]
    fn clock_parsers_require_normal_leap_and_unique_expected_source() {
        assert!(leap_is_normal("Leap status : Normal\n"));
        assert!(!leap_is_normal("Leap status : Not synchronised\n"));
        assert!(expected_source_selected(
            "^* 169.254.169.123 1 4 377\n^- 192.0.2.1 2 4 377",
            "169.254.169.123"
        ));
        assert!(!expected_source_selected(
            "^* 192.0.2.1 1 4 377",
            "169.254.169.123"
        ));
        let tracking = "System time : -0.000000010 seconds fast of NTP time\nRoot delay : 0.000000040 seconds\nRoot dispersion : 0.000000020 seconds\nSkew : 0.050 ppm\n";
        assert_eq!(
            seconds_to_signed_ns(
                tracking_number(tracking, "System time").unwrap(),
                "System time"
            )
            .unwrap(),
            -10
        );
        assert_eq!(
            seconds_to_ns(
                tracking_number(tracking, "Root delay").unwrap(),
                "Root delay"
            )
            .unwrap(),
            40
        );
    }

    #[test]
    fn one_ws_frame_preserves_one_arrival_pair_for_all_envelopes() {
        let manifest = test_manifest();
        let mut state = ProbeState::new(&manifest);
        let connected = TimestampPair {
            realtime_ns: 1_000,
            monotonic_raw_ns: 1_000,
            pair_span_ns: 10,
        };
        state.connected(0, &connected);
        state.warmup.ready_before_window = true;
        state.warmup.ready_generation = Some(0);
        state.sequence.as_mut().unwrap().warm_ready = true;
        let start = AtomicU64::new(2_000);
        let end = AtomicU64::new(4_000);
        let pair = TimestampPair {
            realtime_ns: 3_000,
            monotonic_raw_ns: 3_000,
            pair_span_ns: 12,
        };
        let frame = br#"{"messages":[{"sequenceNumber":1,"timestamp":2,"blockNumber":3,"l2Msg":"AA=="},{"sequenceNumber":2,"timestamp":2,"blockNumber":3,"l2Msg":"AQ=="}]}"#;
        process_frame(frame, &pair, &manifest, &mut state, &start, &end);
        assert_eq!(state.observations.len(), 2);
        assert_eq!(
            state.observations[0].frame_ordinal,
            state.observations[1].frame_ordinal
        );
        assert_eq!(state.observations[0].recv_realtime_ns, pair.realtime_ns);
        assert_eq!(state.observations[1].recv_realtime_ns, pair.realtime_ns);
        assert_eq!(
            state.observations[0].recv_monotonic_raw_ns,
            state.observations[1].recv_monotonic_raw_ns
        );
        assert_ne!(
            state.observations[0].semantic_payload_hash,
            state.observations[1].semantic_payload_hash
        );
    }

    #[test]
    fn timestamp_is_taken_after_complete_ws_read_and_before_parse() {
        let source = include_str!("feed_arrival_probe.rs");
        let read = source.find("match websocket.read()").unwrap();
        let branch = &source[read..];
        let stamp = branch.find("let pair = timestamp_pair();").unwrap();
        let parse = branch.find("process_frame(").unwrap();
        assert!(stamp < parse);
    }

    #[test]
    fn manifest_sha_is_real_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn local_endpoint_validation_happens_before_socket_creation() {
        let error = connect_local("wss://arb1-feed.arbitrum.io/feed").unwrap_err();
        assert!(error.to_string().contains("local Nitro relay"));
    }

    #[test]
    fn manifest_path_must_be_a_regular_readable_input_in_real_run() {
        assert!(!Path::new("/definitely/not/a/feed/manifest").exists());
    }
}
