//! AZ-local, no-submit latency probe for the Arbitrum sequencer.
//!
//! The ranking signal is the warm HTTP application RTT for `eth_chainId` on a retained TLS
//! connection. DNS is snapshotted once, every IPv4 A record is measured independently, and every
//! label comes from the connected socket's `peer_addr`. Raw TCP connect, full TLS, verified resumed
//! TLS, and warm application RTT are reported separately. Failed attempts remain in percentile
//! populations at a worst-possible latency sentinel, as well as in an explicit failure rate.
//!
//! `PROBE_START_EPOCH` and `PROBE_WINDOW_SECS` define one shared wall-clock window across candidate
//! AZ instances. Run repeated windows and pin only when the same `(az_id, actual_peer_ip)` improves
//! both warm p50 by at least 50 us and warm p99 by at least 100 us in every window. RTT/2 is only a
//! `half_rtt_proxy`; no directional latency is claimed without hardware timestamps and a selected,
//! disciplined PHC. Cluster placement groups improve paths among their member instances, not the
//! external path to the sequencer.
//!
//! Stdout is exactly one `mantis.latency-probe.v2` JSON object. Diagnostics go to stderr and never
//! contain the credential-bearing URL path/query.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use engine_feed::relay_client::{parse_first_sequence_number, WsRelayClient};
use engine_submit::hot_spare::{HotSpare, SubmitConn};
use engine_submit::tls::SequencerTlsConnection;
use rustls::{ClientConfig, ClientConnection, HandshakeKind};
use serde::Serialize;
use serde_json::Value;

const DEFAULT_SEQUENCER: &str = "https://arb1-sequencer.arbitrum.io/rpc";
const DEFAULT_FEED: &str = "wss://arb1-feed.arbitrum.io/feed";
const DEFAULT_CHAIN_ID: &str = "0xa4b1";
const ATTEMPT_SCHEDULE: &str = "uniform_nonoverlapping_slots_v1";
const IMDS_MAX_RESPONSE_BYTES: usize = 16 * 1024;
const CHAIN_ID_BODY: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}"#;

#[derive(Debug, Clone, Serialize)]
struct MetricSummary {
    attempted: usize,
    successes: usize,
    failures: usize,
    failure_rate: f64,
    min_ns: u64,
    p50_ns: u64,
    p90_ns: u64,
    p99_ns: u64,
    max_ns: u64,
}

/// A sample set whose failed attempts are represented by a worst-possible latency sentinel.
#[derive(Debug)]
struct SampleSet {
    failure_penalty_ns: u64,
    values_ns: Vec<u64>,
    successes: usize,
}

impl SampleSet {
    fn with_failure_penalty(failure_penalty_ns: u64) -> Self {
        Self {
            failure_penalty_ns,
            values_ns: Vec::new(),
            successes: 0,
        }
    }

    fn record_success(&mut self, elapsed_ns: u64) {
        self.values_ns.push(elapsed_ns);
        self.successes += 1;
    }

    fn record_failure(&mut self) {
        self.values_ns.push(self.failure_penalty_ns);
    }

    fn summary(&self) -> MetricSummary {
        let mut sorted = self.values_ns.clone();
        sorted.sort_unstable();
        let attempted = sorted.len();
        let failures = attempted.saturating_sub(self.successes);
        let percentile = |p: f64| -> u64 {
            if sorted.is_empty() {
                return self.failure_penalty_ns;
            }
            // Nearest-rank percentile: ceil(p*n)-1, clamped to the observed population.
            let rank = ((p * sorted.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(sorted.len() - 1);
            sorted[rank]
        };
        MetricSummary {
            attempted,
            successes: self.successes,
            failures,
            failure_rate: if attempted == 0 {
                1.0
            } else {
                failures as f64 / attempted as f64
            },
            min_ns: sorted.first().copied().unwrap_or(self.failure_penalty_ns),
            p50_ns: percentile(0.50),
            p90_ns: percentile(0.90),
            p99_ns: percentile(0.99),
            max_ns: sorted.last().copied().unwrap_or(self.failure_penalty_ns),
        }
    }
}

#[derive(Serialize)]
struct SequencerIpResult {
    target_ip: String,
    actual_peer_ip: Option<String>,
    observed_peer_ips: Vec<String>,
    peer_integrity_ok: bool,
    tcp_connect_rtt_ns: MetricSummary,
    tls_full_handshake_ns: MetricSummary,
    tls_resumed_handshake_ns: MetricSummary,
    warm_app_rtt_ns: MetricSummary,
}

struct IpAccumulator {
    target: IpAddr,
    observed_peers: Vec<IpAddr>,
    peer_integrity_ok: bool,
    tcp: SampleSet,
    tls_full: SampleSet,
    tls_resumed: SampleSet,
    warm_app: SampleSet,
}

impl IpAccumulator {
    fn new(target: IpAddr, penalty_ns: u64) -> Self {
        Self {
            target,
            observed_peers: Vec::new(),
            peer_integrity_ok: true,
            tcp: SampleSet::with_failure_penalty(penalty_ns),
            tls_full: SampleSet::with_failure_penalty(penalty_ns),
            tls_resumed: SampleSet::with_failure_penalty(penalty_ns),
            warm_app: SampleSet::with_failure_penalty(penalty_ns),
        }
    }

    fn apply_phase(&mut self, phase: ProbePhase, result: PhaseResult) {
        if !result.peer_integrity_ok {
            self.peer_integrity_ok = false;
        }
        for peer in result.observed_peers {
            if !self.observed_peers.contains(&peer) {
                self.observed_peers.push(peer);
            }
            if peer != self.target {
                self.peer_integrity_ok = false;
            }
        }
        if self.observed_peers.len() > 1 {
            self.peer_integrity_ok = false;
        }
        match phase {
            ProbePhase::Tcp => self.tcp = result.samples,
            ProbePhase::TlsFull => self.tls_full = result.samples,
            ProbePhase::TlsResumed => self.tls_resumed = result.samples,
            ProbePhase::WarmApp => self.warm_app = result.samples,
        }
    }

    fn finish(self) -> SequencerIpResult {
        let peer_integrity_ok = self.peer_integrity_ok
            && self.observed_peers.len() == 1
            && self.observed_peers[0] == self.target;
        let actual_peer_ip = self.observed_peers.first().map(ToString::to_string);
        let observed_peer_ips = self
            .observed_peers
            .into_iter()
            .map(|ip| ip.to_string())
            .collect();
        SequencerIpResult {
            target_ip: self.target.to_string(),
            actual_peer_ip,
            observed_peer_ips,
            peer_integrity_ok,
            tcp_connect_rtt_ns: self.tcp.summary(),
            tls_full_handshake_ns: self.tls_full.summary(),
            tls_resumed_handshake_ns: self.tls_resumed.summary(),
            warm_app_rtt_ns: self.warm_app.summary(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ProbePhase {
    Tcp,
    TlsFull,
    TlsResumed,
    WarmApp,
}

#[derive(Clone, Copy, Debug)]
struct ProbeWindow {
    start: Instant,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug)]
struct AttemptSlot {
    start: Instant,
    end: Instant,
}

struct PhaseResult {
    samples: SampleSet,
    observed_peers: Vec<IpAddr>,
    peer_integrity_ok: bool,
}

struct PhaseRecorder {
    target: IpAddr,
    samples: SampleSet,
    observed_peers: Vec<IpAddr>,
    peer_integrity_ok: bool,
}

impl PhaseRecorder {
    fn new(target: IpAddr, penalty_ns: u64) -> Self {
        Self {
            target,
            samples: SampleSet::with_failure_penalty(penalty_ns),
            observed_peers: Vec::new(),
            peer_integrity_ok: true,
        }
    }

    fn observe_peer(&mut self, actual: SocketAddr) -> bool {
        let peer = actual.ip();
        if !self.observed_peers.contains(&peer) {
            self.observed_peers.push(peer);
        }
        let valid =
            actual_peer_label(self.target, actual).is_ok() && self.observed_peers.len() == 1;
        if !valid {
            self.peer_integrity_ok = false;
        }
        valid
    }

    fn finish(self) -> PhaseResult {
        PhaseResult {
            samples: self.samples,
            observed_peers: self.observed_peers,
            peer_integrity_ok: self.peer_integrity_ok,
        }
    }
}

#[derive(Default, Serialize)]
struct FeedResult {
    ok: bool,
    connect_to_first_frame_ns: u64,
    frames: Vec<(u64, u64)>,
}

#[derive(Debug)]
struct PlacementIdentity {
    region: String,
    az_name: String,
    az_id: String,
    instance_type: String,
    subnet_id: String,
}

#[derive(Serialize)]
struct ProbeConfig {
    tcp_samples: u64,
    tls_samples: u64,
    warm_app_samples: u64,
    warmups: u64,
    feed_frames: u64,
    feed_secs: u64,
    probe_timeout_ms: u64,
    submit_connect_timeout_ms: u64,
    submit_write_timeout_ms: u64,
    submit_ack_timeout_ms: u64,
    submit_tcp_user_timeout_ms: u64,
    failure_penalty_ns: u64,
    start_tolerance_ms: u64,
    phase_workers_per_ip: u8,
    attempt_schedule: &'static str,
}

#[derive(Serialize)]
struct ProbeOutput {
    schema: &'static str,
    producer: &'static str,
    window_id: String,
    probe_start_epoch: u64,
    probe_window_secs: u64,
    probe_end_epoch: u64,
    probe_actual_start_ns: u64,
    probe_start_skew_ns: u64,
    probe_start_tolerance_ms: u64,
    probe_actual_finish_ns: u64,
    started_wall_ns: u64,
    finished_wall_ns: u64,
    region: String,
    az_name: String,
    az_id: String,
    instance_type: String,
    subnet_id: String,
    placement_verified: bool,
    sequencer_host: String,
    sequencer_port: u16,
    expected_chain_id: String,
    ranking_metric: &'static str,
    rtt_division_label: &'static str,
    probe_config: ProbeConfig,
    sequencer_ips: Vec<SequencerIpResult>,
    feed: FeedResult,
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

fn required_env(key: &str) -> Result<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{key} is required for an AZ-local probe"))
}

fn parse_imds_response(response: &[u8]) -> Result<&[u8]> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("IMDS response omitted the HTTP header terminator")?;
    let header = std::str::from_utf8(&response[..header_end - 4])
        .context("IMDS response headers were not UTF-8")?;
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .context("IMDS response omitted a status line")?;
    let status_code = status
        .split_ascii_whitespace()
        .nth(1)
        .context("IMDS response status line was malformed")?;
    if status_code != "200" {
        bail!("IMDS returned HTTP status {status_code}");
    }

    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            bail!("IMDS response contained a malformed header");
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("IMDS response used unsupported transfer encoding");
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .context("IMDS Content-Length was invalid")?;
            if content_length.replace(parsed).is_some() {
                bail!("IMDS response repeated Content-Length");
            }
        }
    }
    let body = &response[header_end..];
    if let Some(expected) = content_length {
        if body.len() != expected {
            bail!("IMDS response body length did not match Content-Length");
        }
    }
    Ok(body)
}

fn imds_request(method: &str, path: &str, token: Option<&str>) -> Result<Vec<u8>> {
    if !path.starts_with('/') || path.bytes().any(|byte| byte.is_ascii_control()) {
        bail!("invalid internal IMDS request path");
    }
    if token.is_some_and(|value| {
        value.is_empty() || value.len() > 4_096 || value.bytes().any(|byte| byte.is_ascii_control())
    }) {
        bail!("IMDSv2 returned an invalid token");
    }
    let token_header = token
        .map(|value| format!("X-aws-ec2-metadata-token: {value}\r\n"))
        .unwrap_or_default();
    let ttl_header = if method == "PUT" {
        "X-aws-ec2-metadata-token-ttl-seconds: 60\r\n"
    } else {
        ""
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 169.254.169.254\r\n{ttl_header}{token_header}Accept: text/plain\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 80);
    let timeout = Duration::from_secs(1);
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .context("connect directly to EC2 IMDSv2 link-local endpoint")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream
        .write_all(request.as_bytes())
        .context("write direct IMDSv2 request")?;

    let mut response = Vec::with_capacity(1_024);
    let mut chunk = [0u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).context("read IMDSv2 response")?;
        if read == 0 {
            break;
        }
        if response.len() + read > IMDS_MAX_RESPONSE_BYTES {
            bail!("IMDSv2 response exceeded the bounded response size");
        }
        response.extend_from_slice(&chunk[..read]);
    }
    Ok(parse_imds_response(&response)?.to_vec())
}

fn imds_text(path: &str, token: &str) -> Result<String> {
    let body = imds_request("GET", path, Some(token))?;
    let value = std::str::from_utf8(&body)
        .context("IMDSv2 metadata value was not UTF-8")?
        .trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        bail!("IMDSv2 metadata value was empty or contained a control character");
    }
    Ok(value.to_owned())
}

fn fetch_placement_identity() -> Result<PlacementIdentity> {
    // Raw link-local TCP deliberately bypasses HTTP(S)_PROXY/NO_PROXY and never exposes the
    // short-lived token in argv, diagnostics, or the probe artifact.
    let token_body = imds_request("PUT", "/latest/api/token", None)?;
    let token = std::str::from_utf8(&token_body)
        .context("IMDSv2 token was not UTF-8")?
        .trim();
    if token.is_empty() || token.len() > 4_096 || token.bytes().any(|byte| byte.is_ascii_control())
    {
        bail!("IMDSv2 returned an invalid token");
    }
    let primary_mac = imds_text("/latest/meta-data/mac", token)?;
    let primary_mac = primary_mac.trim_end_matches('/');
    if primary_mac.len() != 17
        || primary_mac
            .bytes()
            .any(|byte| !(byte.is_ascii_hexdigit() || byte == b':'))
    {
        bail!("IMDSv2 returned an invalid primary ENI MAC");
    }
    Ok(PlacementIdentity {
        region: imds_text("/latest/meta-data/placement/region", token)?,
        az_name: imds_text("/latest/meta-data/placement/availability-zone", token)?,
        az_id: imds_text("/latest/meta-data/placement/availability-zone-id", token)?,
        instance_type: imds_text("/latest/meta-data/instance-type", token)?,
        subnet_id: imds_text(
            &format!("/latest/meta-data/network/interfaces/macs/{primary_mac}/subnet-id"),
            token,
        )?,
    })
}

fn validate_placement_claims(
    identity: &PlacementIdentity,
    region: &str,
    az_name: &str,
    az_id: &str,
    instance_type: &str,
    subnet_id: &str,
) -> Result<()> {
    for (field, actual, claimed) in [
        ("PROBE_REGION", identity.region.as_str(), region),
        ("PROBE_AZ_NAME", identity.az_name.as_str(), az_name),
        ("PROBE_AZ_ID", identity.az_id.as_str(), az_id),
        (
            "PROBE_ITYPE",
            identity.instance_type.as_str(),
            instance_type,
        ),
        ("PROBE_SUBNET_ID", identity.subnet_id.as_str(), subnet_id),
    ] {
        if actual != claimed {
            bail!("{field} does not match IMDSv2 placement identity");
        }
    }
    Ok(())
}

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

fn failure_penalty_ns() -> u64 {
    u64::MAX
}

fn completed_within_deadline(completed: Instant, deadline: Instant) -> bool {
    completed <= deadline
}

fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}

fn scaled_duration(total: Duration, numerator: u64, denominator: u64) -> Duration {
    let total_ns = total.as_nanos();
    let numerator = numerator as u128;
    let denominator = denominator as u128;
    let quotient = total_ns / denominator;
    let remainder = total_ns % denominator;
    duration_from_nanos(quotient * numerator + (remainder * numerator) / denominator)
}

fn attempt_slot(window: ProbeWindow, attempts: u64, index: u64) -> Option<AttemptSlot> {
    if attempts == 0 || index >= attempts || window.start >= window.deadline {
        return None;
    }
    let total = window.deadline.checked_duration_since(window.start)?;
    let start = window
        .start
        .checked_add(scaled_duration(total, index, attempts))?;
    let end = window
        .start
        .checked_add(scaled_duration(total, index + 1, attempts))?;
    Some(AttemptSlot { start, end })
}

/// Wait for one deterministic slot. A slot already consumed by startup skew or a prior overrun is
/// one failed planned attempt; callers must not launch network work when this returns `None`.
fn enter_attempt_slot(slot: AttemptSlot, samples: &mut SampleSet) -> Option<Instant> {
    loop {
        let now = Instant::now();
        if now >= slot.start {
            break;
        }
        std::thread::sleep(slot.start.duration_since(now));
    }
    if Instant::now() >= slot.end {
        samples.record_failure();
        None
    } else {
        Some(slot.end)
    }
}

fn remaining_timeout(deadline: Instant, configured: Duration) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .map(|remaining| remaining.min(configured))
}

fn spawn_window_worker<T, F>(
    gate: Arc<Barrier>,
    window: Arc<OnceLock<ProbeWindow>>,
    work: F,
) -> std::thread::JoinHandle<T>
where
    T: Send + 'static,
    F: FnOnce(ProbeWindow) -> T + Send + 'static,
{
    std::thread::spawn(move || {
        gate.wait();
        let window = *window
            .get()
            .expect("probe window must be installed before barrier release");
        work(window)
    })
}

fn all_failed_phase(target: IpAddr, penalty_ns: u64, attempts: u64) -> PhaseResult {
    let mut recorder = PhaseRecorder::new(target, penalty_ns);
    recorder.peer_integrity_ok = false;
    for _ in 0..attempts {
        recorder.samples.record_failure();
    }
    recorder.finish()
}

fn actual_peer_label(target: IpAddr, actual: SocketAddr) -> Result<IpAddr> {
    if actual.ip() != target {
        bail!(
            "connected peer {} did not match pinned target {target}",
            actual.ip()
        );
    }
    Ok(actual.ip())
}

fn resolve_ipv4_once(host: &str, port: u16) -> Result<Vec<IpAddr>> {
    let mut ips: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolve sequencer host {host}"))?
        .filter_map(|address| address.ip().is_ipv4().then_some(address.ip()))
        .collect();
    ips.sort_unstable();
    ips.dedup();
    if ips.is_empty() {
        bail!("sequencer host {host} returned no IPv4 A records");
    }
    Ok(ips)
}

fn validate_chain_id_response(body: &[u8], expected: &str) -> Result<()> {
    let value: Value = serde_json::from_slice(body).context("eth_chainId response was not JSON")?;
    let object = value
        .as_object()
        .context("eth_chainId response was not a JSON object")?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        bail!("eth_chainId response had an invalid JSON-RPC version");
    }
    if object.get("id").and_then(Value::as_u64) != Some(1) {
        bail!("eth_chainId response had an unexpected request id");
    }
    if value.get("error").is_some() {
        bail!("eth_chainId returned a JSON-RPC error");
    }
    let result = value
        .get("result")
        .and_then(Value::as_str)
        .context("eth_chainId response omitted a string result")?;
    if !result.eq_ignore_ascii_case(expected) {
        bail!("eth_chainId returned an unexpected chain id");
    }
    Ok(())
}

fn measured_tls_handshake(
    host: &str,
    port: u16,
    target: IpAddr,
    timeout: Duration,
    config: Arc<ClientConfig>,
) -> Result<(u64, SocketAddr, HandshakeKind)> {
    let address = SocketAddr::new(target, port);
    let mut tcp = TcpStream::connect_timeout(&address, timeout).context("TLS TCP connect")?;
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;
    tcp.set_nodelay(true)?;
    let actual = tcp.peer_addr().context("TLS peer_addr")?;
    actual_peer_label(target, actual)?;

    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| anyhow::anyhow!("invalid TLS server name"))?;
    let started = Instant::now();
    let mut connection =
        ClientConnection::new(config, server_name).context("create TLS client connection")?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut tcp)
            .context("complete TLS handshake")?;
    }
    let elapsed_ns = started.elapsed().as_nanos() as u64;
    let kind = connection
        .handshake_kind()
        .context("TLS handshake completed without a classification")?;
    Ok((elapsed_ns, actual, kind))
}

fn seed_tls_resumption(
    endpoint: &str,
    expected_chain_id: &str,
    target: IpAddr,
    config: Arc<ClientConfig>,
) -> Result<SocketAddr> {
    let mut connection = SequencerTlsConnection::with_config_and_pin(endpoint, config, target)
        .context("build pinned resumption seed")?;
    connection.connect().context("connect resumption seed")?;
    let peer = connection
        .peer_addr()
        .context("read resumption seed peer")?
        .context("resumption seed had no connected peer")?;
    actual_peer_label(target, peer)?;
    let response = connection
        .try_post(CHAIN_ID_BODY)
        .map_err(|failure| anyhow::anyhow!("resumption seed eth_chainId failed: {failure}"))?;
    validate_chain_id_response(&response, expected_chain_id)?;
    Ok(peer)
}

fn prepare_warm_connection(
    endpoint: &str,
    expected_chain_id: &str,
    target: IpAddr,
    config: Arc<ClientConfig>,
    warmups: u64,
) -> Result<(SequencerTlsConnection, SocketAddr)> {
    let mut connection = SequencerTlsConnection::with_config_and_pin(endpoint, config, target)
        .context("build pinned warm connection")?;
    connection
        .connect()
        .context("connect warm application socket")?;
    let peer = connection
        .peer_addr()
        .context("read warm peer")?
        .context("warm connector had no connected peer")?;
    actual_peer_label(target, peer)?;
    for _ in 0..warmups {
        let response = connection
            .try_post(CHAIN_ID_BODY)
            .map_err(|failure| anyhow::anyhow!("warmup eth_chainId failed: {failure}"))?;
        validate_chain_id_response(&response, expected_chain_id)?;
    }
    Ok((connection, peer))
}

fn prepare_warm_hot_spare(
    endpoint: &str,
    expected_chain_id: &str,
    target: IpAddr,
    config: Arc<ClientConfig>,
    warmups: u64,
) -> Result<(HotSpare<SequencerTlsConnection>, [SocketAddr; 2])> {
    // Build exactly the two independently warmed/pinned TLS sockets used by production submit.
    // Every warmup remains the hard-coded free read; no signer or transaction bytes exist here.
    let (primary, primary_peer) =
        prepare_warm_connection(endpoint, expected_chain_id, target, config.clone(), warmups)?;
    let (spare, spare_peer) =
        prepare_warm_connection(endpoint, expected_chain_id, target, config, warmups)?;
    Ok((HotSpare::new(primary, spare), [primary_peer, spare_peer]))
}

fn probe_tcp_phase(
    target: IpAddr,
    port: u16,
    attempts: u64,
    timeout: Duration,
    penalty_ns: u64,
    window: ProbeWindow,
) -> PhaseResult {
    let mut recorder = PhaseRecorder::new(target, penalty_ns);
    let address = SocketAddr::new(target, port);
    for index in 0..attempts {
        let Some(slot) = attempt_slot(window, attempts, index) else {
            recorder.samples.record_failure();
            continue;
        };
        let Some(slot_deadline) = enter_attempt_slot(slot, &mut recorder.samples) else {
            continue;
        };
        let Some(attempt_timeout) = remaining_timeout(slot_deadline, timeout) else {
            recorder.samples.record_failure();
            continue;
        };
        let started = Instant::now();
        match TcpStream::connect_timeout(&address, attempt_timeout) {
            Ok(stream) => {
                let completed = Instant::now();
                let peer_ok = stream
                    .peer_addr()
                    .ok()
                    .is_some_and(|peer| recorder.observe_peer(peer));
                if peer_ok && completed_within_deadline(completed, slot_deadline) {
                    recorder
                        .samples
                        .record_success(completed.duration_since(started).as_nanos() as u64);
                } else {
                    recorder.samples.record_failure();
                }
            }
            Err(_) => recorder.samples.record_failure(),
        }
    }
    recorder.finish()
}

fn probe_full_tls_phase(
    host: &str,
    port: u16,
    target: IpAddr,
    attempts: u64,
    timeout: Duration,
    penalty_ns: u64,
    config: Arc<ClientConfig>,
    window: ProbeWindow,
) -> PhaseResult {
    let mut recorder = PhaseRecorder::new(target, penalty_ns);
    for index in 0..attempts {
        let Some(slot) = attempt_slot(window, attempts, index) else {
            recorder.samples.record_failure();
            continue;
        };
        let Some(slot_deadline) = enter_attempt_slot(slot, &mut recorder.samples) else {
            continue;
        };
        let Some(attempt_timeout) = remaining_timeout(slot_deadline, timeout) else {
            recorder.samples.record_failure();
            continue;
        };
        match measured_tls_handshake(host, port, target, attempt_timeout, config.clone()) {
            Ok((elapsed, peer, HandshakeKind::Full | HandshakeKind::FullWithHelloRetryRequest)) => {
                let completed = Instant::now();
                let peer_ok = recorder.observe_peer(peer);
                if peer_ok && completed_within_deadline(completed, slot_deadline) {
                    recorder.samples.record_success(elapsed);
                } else {
                    recorder.samples.record_failure();
                }
            }
            _ => recorder.samples.record_failure(),
        }
    }
    recorder.finish()
}

#[allow(clippy::too_many_arguments)]
fn probe_resumed_tls_phase(
    endpoint: &str,
    expected_chain_id: &str,
    host: &str,
    port: u16,
    target: IpAddr,
    attempts: u64,
    timeout: Duration,
    penalty_ns: u64,
    config: Arc<ClientConfig>,
    window: ProbeWindow,
) -> PhaseResult {
    let mut recorder = PhaseRecorder::new(target, penalty_ns);
    for index in 0..attempts {
        let Some(slot) = attempt_slot(window, attempts, index) else {
            recorder.samples.record_failure();
            continue;
        };
        let Some(slot_deadline) = enter_attempt_slot(slot, &mut recorder.samples) else {
            continue;
        };
        if remaining_timeout(slot_deadline, timeout).is_none() {
            recorder.samples.record_failure();
            continue;
        }
        let seeded = match seed_tls_resumption(endpoint, expected_chain_id, target, config.clone())
        {
            Ok(peer) => {
                let completed = Instant::now();
                recorder.observe_peer(peer) && completed_within_deadline(completed, slot_deadline)
            }
            Err(_) => false,
        };
        if !seeded {
            recorder.samples.record_failure();
            continue;
        }
        let Some(attempt_timeout) = remaining_timeout(slot_deadline, timeout) else {
            recorder.samples.record_failure();
            continue;
        };
        match measured_tls_handshake(host, port, target, attempt_timeout, config.clone()) {
            Ok((elapsed, peer, HandshakeKind::Resumed)) => {
                let completed = Instant::now();
                let peer_ok = recorder.observe_peer(peer);
                if peer_ok && completed_within_deadline(completed, slot_deadline) {
                    recorder.samples.record_success(elapsed);
                } else {
                    recorder.samples.record_failure();
                }
            }
            _ => recorder.samples.record_failure(),
        }
    }
    recorder.finish()
}

#[allow(clippy::too_many_arguments)]
fn probe_warm_app_phase(
    endpoint: &str,
    expected_chain_id: &str,
    target: IpAddr,
    attempts: u64,
    warmups: u64,
    timeout: Duration,
    penalty_ns: u64,
    config: Arc<ClientConfig>,
    window: ProbeWindow,
) -> PhaseResult {
    let mut recorder = PhaseRecorder::new(target, penalty_ns);
    let mut warm_connector = None;
    for index in 0..attempts {
        let Some(slot) = attempt_slot(window, attempts, index) else {
            recorder.samples.record_failure();
            continue;
        };
        let Some(slot_deadline) = enter_attempt_slot(slot, &mut recorder.samples) else {
            continue;
        };
        if remaining_timeout(slot_deadline, timeout).is_none() {
            recorder.samples.record_failure();
            continue;
        }
        if warm_connector.is_none() {
            match prepare_warm_hot_spare(
                endpoint,
                expected_chain_id,
                target,
                config.clone(),
                warmups,
            ) {
                Ok((connector, peers)) => {
                    let completed = Instant::now();
                    if peers.into_iter().all(|peer| recorder.observe_peer(peer))
                        && completed_within_deadline(completed, slot_deadline)
                    {
                        warm_connector = Some(connector);
                    } else {
                        recorder.samples.record_failure();
                        continue;
                    }
                }
                Err(_) => {
                    recorder.samples.record_failure();
                    continue;
                }
            }
        }

        let started = Instant::now();
        let result = warm_connector
            .as_mut()
            .expect("warm hot-spare connector was prepared above")
            // `HotSpare::submit` is the exact active/spare/reconnect policy used by a real submit;
            // only the immutable JSON-RPC body differs, and this body is eth_chainId below.
            .submit(CHAIN_ID_BODY)
            .map_err(|failure| failure.to_string())
            .and_then(|response| {
                validate_chain_id_response(&response, expected_chain_id)
                    .map_err(|error| error.to_string())
            });
        let completed = Instant::now();
        if result.is_ok() && completed_within_deadline(completed, slot_deadline) {
            recorder
                .samples
                .record_success(completed.duration_since(started).as_nanos() as u64);
        } else {
            recorder.samples.record_failure();
            if result.is_err() {
                warm_connector = None;
            }
        }
    }
    recorder.finish()
}

fn validate_shared_start(
    start: SystemTime,
    actual: SystemTime,
    tolerance: Duration,
) -> Result<u64> {
    let skew = actual.duration_since(start).unwrap_or_default();
    if skew > tolerance {
        bail!(
            "missed shared start barrier: skew={}us tolerance={}us",
            skew.as_micros(),
            tolerance.as_micros()
        );
    }
    Ok(skew.as_nanos().min(u64::MAX as u128) as u64)
}

fn wait_for_shared_window(
    start_epoch: u64,
    window_secs: u64,
    start_tolerance: Duration,
) -> Result<(ProbeWindow, u64, u64)> {
    if start_epoch == 0 || window_secs == 0 {
        bail!("PROBE_START_EPOCH and PROBE_WINDOW_SECS must both be positive");
    }
    let start = UNIX_EPOCH + Duration::from_secs(start_epoch);
    let end = start + Duration::from_secs(window_secs);
    loop {
        let monotonic_now = Instant::now();
        let now = SystemTime::now();
        if now >= start {
            if now >= end {
                bail!("shared probe window has already ended");
            }
            let start_skew_ns = validate_shared_start(start, now, start_tolerance)?;
            let remaining = end.duration_since(now).unwrap_or_default();
            let actual_start_ns = now
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
                .unwrap_or(0);
            let deadline = monotonic_now + remaining;
            let window_start = deadline
                .checked_sub(Duration::from_secs(window_secs))
                .context("shared probe window cannot be represented by the monotonic clock")?;
            return Ok((
                ProbeWindow {
                    start: window_start,
                    deadline,
                },
                actual_start_ns,
                start_skew_ns,
            ));
        }
        let remaining = start.duration_since(now).unwrap_or_default();
        std::thread::sleep(remaining.min(Duration::from_millis(200)));
    }
}

fn submit_human_summary(summary: &MetricSummary, submit_ip: &str) -> String {
    format!(
        "   warm_app_rtt peer={submit_ip} p50={:.3}ms p99={:.3}ms max={:.3}ms fail={}/{} ({:.3}%) half_rtt_proxy_p50={:.3}ms",
        summary.p50_ns as f64 / 1e6,
        summary.p99_ns as f64 / 1e6,
        summary.max_ns as f64 / 1e6,
        summary.failures,
        summary.attempted,
        summary.failure_rate * 100.0,
        summary.p50_ns as f64 / 2e6,
    )
}

fn required_phases_valid(result: &SequencerIpResult) -> bool {
    result.peer_integrity_ok
        && result.actual_peer_ip.as_deref() == Some(result.target_ip.as_str())
        && result.observed_peer_ips.as_slice() == [result.target_ip.as_str()]
        && [
            &result.tcp_connect_rtt_ns,
            &result.tls_full_handshake_ns,
            &result.tls_resumed_handshake_ns,
            &result.warm_app_rtt_ns,
        ]
        .into_iter()
        .all(|summary| summary.successes > 0)
}

/// Feed leg retained as supplementary, clock-correlated relative-arrival evidence.
fn probe_feed(url: &str, max_frames: u64, budget_secs: u64) -> Result<(u64, Vec<(u64, u64)>)> {
    let client = WsRelayClient::new(url)
        .context("build feed client")?
        .with_read_poll_timeout(Duration::from_millis(250));
    let shutdown = Arc::new(AtomicBool::new(false));
    let frames: Arc<Mutex<Vec<(u64, u64)>>> =
        Arc::new(Mutex::new(Vec::with_capacity(max_frames as usize)));
    let first_frame_ns = Arc::new(AtomicU64::new(0));
    let count = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let (sh, fr, ff, cnt) = (
        shutdown.clone(),
        frames.clone(),
        first_frame_ns.clone(),
        count.clone(),
    );
    let handle = std::thread::spawn(move || {
        let _ = client.run(&sh, |callback| {
            let _ = ff.compare_exchange(
                0,
                start.elapsed().as_nanos() as u64,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if let Some(sequence) = parse_first_sequence_number(callback.frame) {
                if let Ok(mut guard) = fr.lock() {
                    guard.push((sequence, callback.recv_ns));
                }
            }
            if cnt.fetch_add(1, Ordering::Relaxed) + 1 >= max_frames {
                sh.store(true, Ordering::Release);
            }
        });
    });
    while !shutdown.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(budget_secs) {
        std::thread::sleep(Duration::from_millis(50));
    }
    shutdown.store(true, Ordering::Release);
    let _ = handle.join();
    let out = frames.lock().map(|guard| guard.clone()).unwrap_or_default();
    Ok((first_frame_ns.load(Ordering::Acquire), out))
}

const FUNDLESS_PROBE_OPT_IN_ENV: &str = "MANTIS_FUNDLESS_PROBE_OPT_IN";
const FUNDLESS_PROBE_OPT_IN_TOKEN: &str = "READ_ONLY_ETH_CHAIN_ID";

/// Execute the connector body only after an exact, unambiguous read-only opt-in.
///
/// Keeping the policy outside the network implementation makes the default-off guarantee directly
/// testable: a missing, misspelled, or generic truthy value returns before DNS resolution, socket
/// creation, TLS, feed connection, or an HTTP request can occur.
fn run_if_fundless_probe_enabled<F>(raw_opt_in: Option<&str>, connector_body: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    if raw_opt_in != Some(FUNDLESS_PROBE_OPT_IN_TOKEN) {
        bail!(
            "fundless latency probe is default-off; no network request was attempted. Set \
             {FUNDLESS_PROBE_OPT_IN_ENV}={FUNDLESS_PROBE_OPT_IN_TOKEN} to permit only the \
             read-only eth_chainId/feed measurement"
        );
    }
    connector_body()
}

/// Entry point for `engine latency-probe`.
pub fn run() -> Result<()> {
    let opt_in = std::env::var(FUNDLESS_PROBE_OPT_IN_ENV).ok();
    run_if_fundless_probe_enabled(opt_in.as_deref(), run_enabled)
}

fn run_enabled() -> Result<()> {
    let endpoint = std::env::var("SEQUENCER_URL").unwrap_or_else(|_| DEFAULT_SEQUENCER.into());
    let feed_url = std::env::var("FEED_URL").unwrap_or_else(|_| DEFAULT_FEED.into());
    let parsed = url::Url::parse(&endpoint).context("SEQUENCER_URL is not a valid URL")?;
    if parsed.scheme() != "https" {
        bail!("SEQUENCER_URL must use https for the TLS probe");
    }
    let host = parsed
        .host_str()
        .context("SEQUENCER_URL has no host")?
        .to_owned();
    let port = parsed.port_or_known_default().unwrap_or(443);

    let claimed_region = required_env("PROBE_REGION")?;
    let claimed_az_id = required_env("PROBE_AZ_ID")?;
    let claimed_az_name = required_env("PROBE_AZ_NAME")?;
    let claimed_instance_type = required_env("PROBE_ITYPE")?;
    let claimed_subnet_id = required_env("PROBE_SUBNET_ID")?;
    let start_epoch = required_env("PROBE_START_EPOCH")?
        .parse::<u64>()
        .context("PROBE_START_EPOCH must be Unix seconds")?;
    let window_secs = env_u64("PROBE_WINDOW_SECS", 300);
    let start_tolerance_ms = env_u64("PROBE_START_TOLERANCE_MS", 50);
    if !(1..=250).contains(&start_tolerance_ms) {
        bail!("PROBE_START_TOLERANCE_MS must be in 1..=250");
    }
    let window_id = std::env::var("PROBE_WINDOW_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| start_epoch.to_string());
    let expected_chain_id = std::env::var("PROBE_EXPECTED_CHAIN_ID")
        .unwrap_or_else(|_| DEFAULT_CHAIN_ID.into())
        .to_ascii_lowercase();
    let chain_digits = expected_chain_id.strip_prefix("0x").unwrap_or_default();
    if chain_digits.is_empty() || !chain_digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("PROBE_EXPECTED_CHAIN_ID must be 0x-prefixed hexadecimal");
    }

    let placement = fetch_placement_identity()
        .context("IMDSv2 placement verification failed; run on the candidate EC2 instance")?;
    validate_placement_claims(
        &placement,
        &claimed_region,
        &claimed_az_name,
        &claimed_az_id,
        &claimed_instance_type,
        &claimed_subnet_id,
    )?;
    let PlacementIdentity {
        region,
        az_name,
        az_id,
        instance_type,
        subnet_id,
    } = placement;

    let tcp_samples = env_u64("PROBE_TCP_SAMPLES", 100);
    let tls_samples = env_u64("PROBE_TLS_SAMPLES", 20);
    let warm_samples = env_u64("PROBE_SUBMIT_SAMPLES", 300);
    let warmups = env_u64("PROBE_WARMUPS", 5);
    let feed_frames = env_u64("PROBE_FEED_FRAMES", 3_000);
    let feed_secs = env_u64("PROBE_FEED_SECS", window_secs.min(30));
    let timeout_ms = env_u64("PROBE_TIMEOUT_MS", 3_000);
    if tcp_samples == 0 || tls_samples == 0 || warm_samples == 0 || timeout_ms == 0 {
        bail!("probe sample counts and PROBE_TIMEOUT_MS must be positive");
    }
    let timeout = Duration::from_millis(timeout_ms);
    let submit_connect_timeout_ms = env_u64("SUBMIT_CONNECT_TIMEOUT_MS", 5_000);
    let submit_write_timeout_ms = env_u64("SUBMIT_WRITE_TIMEOUT_MS", 5_000);
    let submit_ack_timeout_ms = env_u64("SUBMIT_ACK_TIMEOUT_MS", 10_000);
    let submit_tcp_user_timeout_ms = env_u64("SUBMIT_TCP_USER_TIMEOUT_MS", 0);
    if submit_connect_timeout_ms == 0 || submit_write_timeout_ms == 0 || submit_ack_timeout_ms == 0
    {
        bail!("SUBMIT_CONNECT/WRITE/ACK_TIMEOUT_MS must be positive");
    }
    if submit_tcp_user_timeout_ms > u32::MAX as u64 {
        bail!("SUBMIT_TCP_USER_TIMEOUT_MS must fit in u32 milliseconds");
    }
    // No successful observation can sort behind a failed attempt, even when an application
    // operation spans multiple independently configured socket deadlines.
    let failure_penalty_ns = failure_penalty_ns();

    // The only sequencer DNS lookup in this process. Every later connection is a fixed SocketAddr.
    let ips = resolve_ipv4_once(&host, port)?;
    eprintln!(
        "latency-probe: region={region} az_id={az_id} host={host}:{port} A_records={} shared_start={start_epoch} window={window_secs}s",
        ips.len()
    );

    let mut probes: Vec<IpAccumulator> = ips
        .into_iter()
        .map(|ip| IpAccumulator::new(ip, failure_penalty_ns))
        .collect();

    // Every (A-record, phase) gets an independent worker. A blackholed peer or slow TLS phase can
    // therefore consume only its own budget; it cannot starve another peer or the ranking metric.
    let worker_count = probes
        .len()
        .checked_mul(4)
        .context("too many sequencer probe workers")?;
    let gate = Arc::new(Barrier::new(worker_count + 1));
    let shared_window = Arc::new(OnceLock::new());
    let mut workers = Vec::with_capacity(worker_count);

    let mut full_config = (*SequencerTlsConnection::shared_client_config()).clone();
    full_config.resumption = rustls::client::Resumption::disabled();
    let full_config = Arc::new(full_config);
    for index in 0..probes.len() {
        let target = probes[index].target;

        let handle = spawn_window_worker(gate.clone(), shared_window.clone(), move |window| {
            probe_tcp_phase(
                target,
                port,
                tcp_samples,
                timeout,
                failure_penalty_ns,
                window,
            )
        });
        workers.push((index, ProbePhase::Tcp, tcp_samples, handle));

        let worker_host = host.clone();
        let worker_config = full_config.clone();
        let handle = spawn_window_worker(gate.clone(), shared_window.clone(), move |window| {
            probe_full_tls_phase(
                &worker_host,
                port,
                target,
                tls_samples,
                timeout,
                failure_penalty_ns,
                worker_config,
                window,
            )
        });
        workers.push((index, ProbePhase::TlsFull, tls_samples, handle));

        let worker_endpoint = endpoint.clone();
        let worker_chain_id = expected_chain_id.clone();
        let worker_host = host.clone();
        let worker_config = SequencerTlsConnection::shared_client_config();
        let handle = spawn_window_worker(gate.clone(), shared_window.clone(), move |window| {
            probe_resumed_tls_phase(
                &worker_endpoint,
                &worker_chain_id,
                &worker_host,
                port,
                target,
                tls_samples,
                timeout,
                failure_penalty_ns,
                worker_config,
                window,
            )
        });
        workers.push((index, ProbePhase::TlsResumed, tls_samples, handle));

        let worker_endpoint = endpoint.clone();
        let worker_chain_id = expected_chain_id.clone();
        let worker_config = SequencerTlsConnection::shared_client_config();
        let handle = spawn_window_worker(gate.clone(), shared_window.clone(), move |window| {
            probe_warm_app_phase(
                &worker_endpoint,
                &worker_chain_id,
                target,
                warm_samples,
                warmups,
                timeout,
                failure_penalty_ns,
                worker_config,
                window,
            )
        });
        workers.push((index, ProbePhase::WarmApp, warm_samples, handle));
    }

    // No network measurement begins before the common wall-clock barrier.
    let (window, started_wall_ns, start_skew_ns) = wait_for_shared_window(
        start_epoch,
        window_secs,
        Duration::from_millis(start_tolerance_ms),
    )?;
    shared_window
        .set(window)
        .map_err(|_| anyhow::anyhow!("probe window was already installed"))?;
    gate.wait();
    let feed_handle = std::thread::spawn(move || probe_feed(&feed_url, feed_frames, feed_secs));

    for (index, phase, attempts, handle) in workers {
        let target = probes[index].target;
        let result = match handle.join() {
            Ok(result) => result,
            Err(_) => {
                eprintln!("latency-probe: {phase:?} worker for {target} panicked");
                all_failed_phase(target, failure_penalty_ns, attempts)
            }
        };
        probes[index].apply_phase(phase, result);
    }

    let mut results = Vec::with_capacity(probes.len());
    for probe in probes {
        let result = probe.finish();
        eprintln!(
            "{}",
            submit_human_summary(
                &result.warm_app_rtt_ns,
                result.actual_peer_ip.as_deref().unwrap_or("<no-peer>")
            )
        );
        results.push(result);
    }

    let feed = match feed_handle.join() {
        Ok(Ok((first, frames))) => FeedResult {
            ok: true,
            connect_to_first_frame_ns: first,
            frames,
        },
        _ => {
            eprintln!("latency-probe: supplementary feed measurement failed");
            FeedResult::default()
        }
    };
    let valid = results.iter().all(required_phases_valid);
    let finished_wall_ns = wall_ns();
    let probe_config = ProbeConfig {
        tcp_samples,
        tls_samples,
        warm_app_samples: warm_samples,
        warmups,
        feed_frames,
        feed_secs,
        probe_timeout_ms: timeout_ms,
        submit_connect_timeout_ms,
        submit_write_timeout_ms,
        submit_ack_timeout_ms,
        submit_tcp_user_timeout_ms,
        failure_penalty_ns,
        start_tolerance_ms,
        phase_workers_per_ip: 4,
        attempt_schedule: ATTEMPT_SCHEDULE,
    };
    let output = ProbeOutput {
        schema: "mantis.latency-probe.v2",
        producer: "engine/latency-probe-v2",
        window_id,
        probe_start_epoch: start_epoch,
        probe_window_secs: window_secs,
        probe_end_epoch: start_epoch.saturating_add(window_secs),
        probe_actual_start_ns: started_wall_ns,
        probe_start_skew_ns: start_skew_ns,
        probe_start_tolerance_ms: start_tolerance_ms,
        probe_actual_finish_ns: finished_wall_ns,
        started_wall_ns,
        finished_wall_ns,
        region,
        az_name,
        az_id,
        instance_type,
        subnet_id,
        placement_verified: true,
        sequencer_host: host,
        sequencer_port: port,
        expected_chain_id,
        ranking_metric: "warm_app_rtt",
        rtt_division_label: "half_rtt_proxy",
        probe_config,
        sequencer_ips: results,
        feed,
    };
    serde_json::to_writer(std::io::stdout().lock(), &output).context("serialize probe JSON")?;
    println!();
    if !valid {
        bail!("one or more sequencer IPs missed a required phase or failed peer integrity");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::{mpsc, Arc, Barrier, OnceLock};
    use std::time::Instant;
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        actual_peer_label, attempt_slot, completed_within_deadline, enter_attempt_slot,
        failure_penalty_ns, parse_imds_response, required_phases_valid,
        run_if_fundless_probe_enabled, spawn_window_worker, submit_human_summary,
        validate_chain_id_response, validate_placement_claims, validate_shared_start,
        IpAccumulator, MetricSummary, PhaseRecorder, PlacementIdentity, ProbePhase, ProbeWindow,
        SampleSet, SequencerIpResult, CHAIN_ID_BODY,
    };

    #[test]
    fn fundless_probe_default_refuses_before_connector_body() {
        let entered = std::cell::Cell::new(false);
        let result = run_if_fundless_probe_enabled(None, || {
            entered.set(true);
            Ok(())
        });

        assert!(
            result.is_err(),
            "missing opt-in must refuse the network probe"
        );
        assert!(
            !entered.get(),
            "default-off probe must not enter DNS/TCP/TLS/request code"
        );
    }

    #[test]
    fn fundless_probe_accepts_only_explicit_read_only_opt_in() {
        for raw in [Some(""), Some("0"), Some("true"), Some("yes"), Some("1 ")] {
            let entered = std::cell::Cell::new(false);
            assert!(
                run_if_fundless_probe_enabled(raw, || {
                    entered.set(true);
                    Ok(())
                })
                .is_err(),
                "ambiguous opt-in {raw:?} must fail closed"
            );
            assert!(!entered.get(), "refused opt-in {raw:?} entered connector");
        }

        let entered = std::cell::Cell::new(false);
        run_if_fundless_probe_enabled(Some("READ_ONLY_ETH_CHAIN_ID"), || {
            entered.set(true);
            Ok(())
        })
        .expect("exact read-only token should enable the probe");
        assert!(entered.get());
    }

    #[test]
    fn fundless_probe_payload_is_only_the_free_chain_id_read() {
        let body = std::str::from_utf8(CHAIN_ID_BODY).expect("chainId body is UTF-8");
        assert!(body.contains(r#""method":"eth_chainId""#));
        assert!(!body.contains("eth_sendRawTransaction"));
        assert!(!body.contains("rawTransaction"));
        assert!(!body.contains("private"));
    }

    #[test]
    fn rtt_division_is_labeled_as_half_rtt_proxy() {
        let summary = MetricSummary {
            attempted: 3,
            successes: 3,
            failures: 0,
            failure_rate: 0.0,
            min_ns: 1_000_000,
            p50_ns: 2_000_000,
            p90_ns: 3_000_000,
            p99_ns: 4_000_000,
            max_ns: 4_000_000,
        };
        let text = submit_human_summary(&summary, "192.0.2.1");
        assert!(text.contains("p50=2.000ms"));
        assert!(text.contains("half_rtt_proxy_p50=1.000ms"));
        assert!(!text.contains("one-way"));
    }

    #[test]
    fn failed_attempts_stay_in_the_percentile_denominator() {
        let mut samples = SampleSet::with_failure_penalty(5_000_000);
        samples.record_success(100_000);
        samples.record_success(200_000);
        samples.record_failure();
        let summary = samples.summary();
        assert_eq!(summary.attempted, 3);
        assert_eq!(summary.successes, 2);
        assert_eq!(summary.failures, 1);
        assert!((summary.failure_rate - (1.0 / 3.0)).abs() < f64::EPSILON);
        assert_eq!(summary.p50_ns, 200_000);
        assert_eq!(summary.p90_ns, 5_000_000);
        assert_eq!(summary.p99_ns, 5_000_000);
        assert_eq!(summary.max_ns, 5_000_000);
    }

    #[test]
    fn failure_sentinel_can_never_improve_a_latency_percentile() {
        let penalty = failure_penalty_ns();
        assert_eq!(penalty, u64::MAX);
        let mut samples = SampleSet::with_failure_penalty(penalty);
        samples.record_success(u64::MAX - 1);
        let before = samples.summary();
        samples.record_failure();
        let after = samples.summary();
        assert_eq!(after.p50_ns, before.p50_ns);
        assert_eq!(after.max_ns, u64::MAX);
    }

    #[test]
    fn completion_after_the_shared_deadline_is_not_a_success() {
        let deadline = Instant::now() + Duration::from_millis(10);
        assert!(completed_within_deadline(deadline, deadline));
        assert!(!completed_within_deadline(
            deadline + Duration::from_nanos(1),
            deadline
        ));
    }

    #[test]
    fn gated_phase_workers_are_independent_after_release() {
        let gate = Arc::new(Barrier::new(3));
        let window = Arc::new(OnceLock::new());
        let start = Instant::now();
        window
            .set(ProbeWindow {
                start,
                deadline: start + Duration::from_secs(1),
            })
            .unwrap();
        let (fast_tx, fast_rx) = mpsc::channel();
        let slow = spawn_window_worker(gate.clone(), window.clone(), |_| {
            std::thread::sleep(Duration::from_millis(100));
        });
        let fast = spawn_window_worker(gate.clone(), window, move |_| {
            fast_tx.send(()).unwrap();
        });
        gate.wait();
        fast_rx
            .recv_timeout(Duration::from_millis(50))
            .expect("fast worker must not wait behind the slow worker");
        fast.join().unwrap();
        slow.join().unwrap();
    }

    #[test]
    fn attempt_slots_span_the_window_without_starting_at_the_deadline() {
        let start = Instant::now() + Duration::from_millis(10);
        let window = ProbeWindow {
            start,
            deadline: start + Duration::from_secs(8),
        };
        let slots: Vec<_> = (0..4)
            .map(|index| attempt_slot(window, 4, index).expect("valid slot"))
            .collect();

        assert_eq!(slots[0].start, window.start);
        assert_eq!(slots[0].end, slots[1].start);
        assert_eq!(slots[1].end, slots[2].start);
        assert_eq!(slots[2].end, slots[3].start);
        assert_eq!(slots[3].end, window.deadline);
        assert!(slots[3].start < window.deadline);
    }

    #[test]
    fn missed_attempt_slot_records_failure_without_launching() {
        let now = Instant::now();
        let window = ProbeWindow {
            start: now - Duration::from_millis(20),
            deadline: now - Duration::from_millis(10),
        };
        let slot = attempt_slot(window, 1, 0).expect("valid slot");
        let mut samples = SampleSet::with_failure_penalty(failure_penalty_ns());
        let mut launched = false;
        if enter_attempt_slot(slot, &mut samples).is_some() {
            launched = true;
        }

        assert!(!launched);
        let summary = samples.summary();
        assert_eq!(summary.attempted, 1);
        assert_eq!(summary.failures, 1);
        assert_eq!(summary.max_ns, u64::MAX);
    }

    #[test]
    fn placement_claim_mismatch_fails_closed() {
        let identity = PlacementIdentity {
            region: "us-east-2".into(),
            az_name: "us-east-2a".into(),
            az_id: "use2-az1".into(),
            instance_type: "c8gn.2xlarge".into(),
            subnet_id: "subnet-verified".into(),
        };
        assert!(validate_placement_claims(
            &identity,
            "us-east-2",
            "us-east-2a",
            "use2-az1",
            "c8gn.2xlarge",
            "subnet-verified"
        )
        .is_ok());
        assert!(validate_placement_claims(
            &identity,
            "us-east-1",
            "us-east-2a",
            "use2-az1",
            "c8gn.2xlarge",
            "subnet-verified"
        )
        .is_err());
    }

    #[test]
    fn imds_http_parser_requires_success_and_a_complete_body() {
        assert_eq!(
            parse_imds_response(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nuse2-az1").unwrap(),
            b"use2-az1"
        );
        assert!(
            parse_imds_response(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").is_err()
        );
        assert!(parse_imds_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort").is_err());
    }

    #[test]
    fn peer_label_comes_from_the_connected_socket_and_mismatch_fails() {
        let target = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let actual = SocketAddr::new(target, 443);
        assert_eq!(
            actual_peer_label(target, actual).expect("matching peer"),
            target
        );
        let other = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11)), 443);
        assert!(actual_peer_label(target, other).is_err());
    }

    #[test]
    fn peer_mismatch_is_ineligible_but_preserves_the_actual_socket_peer() {
        let target = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let actual_ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));
        let mut phase = PhaseRecorder::new(target, failure_penalty_ns());
        assert!(!phase.observe_peer(SocketAddr::new(actual_ip, 443)));
        phase.samples.record_failure();

        let mut accumulator = IpAccumulator::new(target, failure_penalty_ns());
        accumulator.apply_phase(ProbePhase::Tcp, phase.finish());
        let result = accumulator.finish();
        assert_eq!(result.actual_peer_ip.as_deref(), Some("192.0.2.11"));
        assert_eq!(result.observed_peer_ips, ["192.0.2.11"]);
        assert!(!result.peer_integrity_ok);
    }

    #[test]
    fn no_observed_peer_is_explicitly_not_peer_integrity_ok() {
        let target = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let result = IpAccumulator::new(target, failure_penalty_ns()).finish();
        assert_eq!(result.actual_peer_ip, None);
        assert!(result.observed_peer_ips.is_empty());
        assert!(!result.peer_integrity_ok);
    }

    #[test]
    fn chain_id_validation_rejects_auth_errors_and_wrong_networks() {
        validate_chain_id_response(br#"{"jsonrpc":"2.0","id":1,"result":"0xa4b1"}"#, "0xa4b1")
            .unwrap();
        assert!(validate_chain_id_response(br#"{"id":1,"result":"0xa4b1"}"#, "0xa4b1").is_err());
        assert!(validate_chain_id_response(
            br#"{"jsonrpc":"2.0","id":2,"result":"0xa4b1"}"#,
            "0xa4b1"
        )
        .is_err());
        assert!(validate_chain_id_response(
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000}}"#,
            "0xa4b1"
        )
        .is_err());
        assert!(validate_chain_id_response(
            br#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#,
            "0xa4b1"
        )
        .is_err());
    }

    #[test]
    fn every_required_phase_needs_a_success_for_valid_output() {
        let good = MetricSummary {
            attempted: 1,
            successes: 1,
            failures: 0,
            failure_rate: 0.0,
            min_ns: 1,
            p50_ns: 1,
            p90_ns: 1,
            p99_ns: 1,
            max_ns: 1,
        };
        let mut result = SequencerIpResult {
            target_ip: "192.0.2.1".into(),
            actual_peer_ip: Some("192.0.2.1".into()),
            observed_peer_ips: vec!["192.0.2.1".into()],
            peer_integrity_ok: true,
            tcp_connect_rtt_ns: good.clone(),
            tls_full_handshake_ns: good.clone(),
            tls_resumed_handshake_ns: good.clone(),
            warm_app_rtt_ns: good,
        };
        assert!(required_phases_valid(&result));
        result.tls_resumed_handshake_ns.successes = 0;
        assert!(!required_phases_valid(&result));
    }

    #[test]
    fn shared_start_fails_closed_outside_tolerance() {
        let start = UNIX_EPOCH + Duration::from_secs(100);
        assert!(validate_shared_start(
            start,
            start + Duration::from_millis(50),
            Duration::from_millis(50)
        )
        .is_ok());
        assert!(validate_shared_start(
            start,
            start + Duration::from_millis(51),
            Duration::from_millis(50)
        )
        .is_err());
    }
}
