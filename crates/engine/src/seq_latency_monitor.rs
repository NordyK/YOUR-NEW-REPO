//! Background sequencer-latency monitor — measures + logs the sequencer network latency in EACH
//! DIRECTION SEPARATELY, off the hot path, so a live/dry run surfaces both legs continuously:
//!
//!   * **SEND**    box → `arb1-sequencer.arbitrum.io` (FCFS submit leg; half-RTT proxy)
//!   * **RECEIVE** `arb1-feed.arbitrum.io` → box   (feed ingest leg; half-RTT proxy)
//!
//! Pure TCP-connect RTT (SYN→SYN-ACK), reported honestly as RTT and a symmetry-assuming half-RTT
//! proxy, independent of TLS or the local relay. Respects `SUBMIT_PIN_IP` / `FEED_PIN_IP` so it
//! measures the pinned IPs the engine actually uses. Runs on the housekeeping core (1-7 are
//! isolcpus), never touching the pipeline.
//!
//! OPTION (env): `SEQ_LATENCY_MONITOR=1` enables it; `SEQ_LATENCY_MONITOR_SECS` (default 15) is the
//! cadence; `SEQ_LATENCY_SEND_HOST` / `SEQ_LATENCY_RECV_HOST` override the endpoints. Emits a
//! structured `SEQ_LATENCY` line (target `seq_latency`) every cycle:
//!   send_half_rtt_proxy_us / recv_half_rtt_proxy_us / send_rtt_us / recv_rtt_us /
//!   combined_half_rtt_proxy_us + IPs.
//!
//! ## P10 — real timestamping + clock discipline (network review 2026-07-13, §Priority 10)
//!
//! The half-RTT fields above are symmetry proxies (RTT/2), NOT observations of either direction — a
//! sub-ms sequencer→host claim is only defensible with hardware NIC timestamping + a disciplined PHC
//! + a clock error ≤50 µs. Two facilities here make that honest:
//!   * [`timestamping`] — a SAFE decoder that keeps the kernel **software** (`ts[0]`) and NIC
//!     **hardware** (`ts[2]`) receive stamps SEPARATE (`engine-sysopt`'s existing recvmsg helper
//!     collapses them into one), plus [`timestamping::StampStats`] that counts missing/fallback and
//!     gates a "hardware one-way" claim on a HW cmsg present for ≥99.99% of packets. The `recvmsg`
//!     itself is isolated `unsafe` in `engine-sysopt` (this crate is `#![forbid(unsafe_code)]`);
//!     setting the sockopt alone is insufficient because the stamps only arrive as ancillary cmsgs on
//!     `recvmsg`, so any `read()` path (tungstenite) drops them.
//!   * [`clock`] — probe `/dev/ptp_ena`, `ethtool -T`, `/sys/class/ptp/*/clock_name` and
//!     `chronyc tracking` to decide whether an absolute one-way claim is defensible, and otherwise
//!     publish a **clock-error interval** (`|last offset| + root dispersion`). Logged once at start.
//!
//! Same-host monotonic durations do NOT need synchronized UTC; cross-AZ same-sequence comparisons and
//! absolute one-way claims do — this module labels its numbers `metric=half_rtt_proxy` accordingly.

use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::info;

use engine_feed::relay_client::pick_best_addr;

const DEFAULT_SEND_HOST: &str = "arb1-sequencer.arbitrum.io";
const DEFAULT_RECV_HOST: &str = "arb1-feed.arbitrum.io";

/// True if the operator turned the monitor on.
fn enabled() -> bool {
    matches!(
        std::env::var("SEQ_LATENCY_MONITOR").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

fn env_ip(key: &str) -> Option<IpAddr> {
    std::env::var(key).ok().and_then(|s| s.trim().parse().ok())
}

/// Median TCP-connect RTT (ns) to `host:port` over `n` samples, plus the IP used. Honours `pin`.
fn tcp_rtt_p50(host: &str, port: u16, pin: Option<IpAddr>, n: u32) -> Option<(u64, String)> {
    let addr = match pin {
        Some(ip) => SocketAddr::new(ip, port),
        None => pick_best_addr(host, port)?,
    };
    let mut xs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let t = Instant::now();
        if TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
            xs.push(t.elapsed().as_nanos() as u64);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    if xs.is_empty() {
        return None;
    }
    xs.sort_unstable();
    Some((xs[xs.len() / 2], addr.ip().to_string()))
}

/// Spawn the monitor thread if `SEQ_LATENCY_MONITOR` is set. No-op otherwise. Stops when `shutdown` flips.
pub fn spawn(shutdown: Arc<AtomicBool>) {
    if !enabled() {
        return;
    }
    let secs = std::env::var("SEQ_LATENCY_MONITOR_SECS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(15u64)
        .max(1);
    let send_host = std::env::var("SEQ_LATENCY_SEND_HOST").unwrap_or_else(|_| DEFAULT_SEND_HOST.into());
    let recv_host = std::env::var("SEQ_LATENCY_RECV_HOST").unwrap_or_else(|_| DEFAULT_RECV_HOST.into());
    let samples = 32u32;

    let _ = std::thread::Builder::new()
        .name("seq-lat-mon".into())
        .spawn(move || {
            let send_pin = env_ip("SUBMIT_PIN_IP");
            let recv_pin = env_ip("FEED_PIN_IP");
            info!(
                target: "seq_latency",
                every_secs = secs, send_host = %send_host, recv_host = %recv_host,
                "sequencer-latency monitor ON — SEND=box→sequencer, RECEIVE=feed→box (half-RTT proxy)"
            );
            // P10: surface clock discipline ONCE so the RTT/2 numbers below are interpretable. A
            // sub-ms one-way claim is only defensible with HW timestamping + PHC + clock error
            // ≤50µs; otherwise these remain half-RTT proxies with an explicit clock-error interval.
            let disc = clock::probe(&clock::default_iface().unwrap_or_default());
            let no_observed_stamps = timestamping::StampStats::default();
            info!(
                target: "seq_latency",
                iface = %disc.iface,
                phc = disc.phc_device.as_deref().unwrap_or("none"),
                phc_clock = disc.phc_clock_name.as_deref().unwrap_or("none"),
                hw_rx_capable = disc.hw_rx_capable,
                hw_rx_configured = disc.hw_rx_configured,
                selected_phc_disciplined = disc.selected_phc_disciplined,
                hw_cmsg_reads = no_observed_stamps.total,
                hw_cmsg_coverage = no_observed_stamps.hardware_fraction(),
                clock_error_us = disc.clock_error_ns().map(|e| (e / 1000) as i64).unwrap_or(-1),
                hardware_one_way_defensible = disc.hardware_one_way_defensible(&no_observed_stamps),
                "CLOCK_DISCIPLINE — metric nomenclature = half_rtt_proxy unless HW one-way is defensible"
            );
            let clock_err_us: i64 = disc.clock_error_ns().map(|e| (e / 1000) as i64).unwrap_or(-1);
            while !shutdown.load(Ordering::Acquire) {
                let send = tcp_rtt_p50(&send_host, 443, send_pin, samples);
                let recv = tcp_rtt_p50(&recv_host, 443, recv_pin, samples);
                match (send, recv) {
                    (Some((s_rtt, s_ip)), Some((r_rtt, r_ip))) => info!(
                        target: "seq_latency",
                        // These fields are RTT/2 symmetry proxies, NOT measured one-way latency.
                        // `metric` + `clock_error_us` (−1 = unknown) make that explicit.
                        metric = "half_rtt_proxy",
                        clock_error_us = clock_err_us,
                        send_half_rtt_proxy_us = s_rtt / 2000,
                        recv_half_rtt_proxy_us = r_rtt / 2000,
                        send_rtt_us = s_rtt / 1000,
                        recv_rtt_us = r_rtt / 1000,
                        combined_half_rtt_proxy_us = (s_rtt + r_rtt) / 2000,
                        send_ip = %s_ip,
                        recv_ip = %r_ip,
                        "SEQ_LATENCY"
                    ),
                    (s, r) => info!(
                        target: "seq_latency",
                        send_ok = s.is_some(), recv_ok = r.is_some(),
                        "SEQ_LATENCY measurement incomplete (endpoint unresolved/unreachable)"
                    ),
                }
                // Sleep in 100ms slices so shutdown is honoured promptly.
                for _ in 0..(secs * 10) {
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
}

/// P10 — kernel/NIC receive-timestamp decoding + coverage accounting. Setting `SO_TIMESTAMPING` is not
/// enough: the timestamps arrive only as ancillary control messages on `recvmsg`, so a normal `read()`
/// (tungstenite, `TcpStream::read`) drops them. The actual `recvmsg` + cmsg walk is isolated `unsafe`
/// that lives in `engine-sysopt` (this crate is `#![forbid(unsafe_code)]`); `engine-sysopt` already
/// has `set_so_timestamping` + `recvmsg_timestamp_ns`, but the latter COLLAPSES software and hardware
/// into one value. This module supplies what the review adds on top: a SAFE decoder that keeps the
/// **software** (`ts[0]`) and **hardware** (`ts[2]`) stamps SEPARATE, and [`StampStats`] to count
/// missing/fallback and gate a "hardware one-way" claim on ≥99.99% HW coverage.
pub mod timestamping {
    /// One receive's kernel-SW and NIC-HW timestamps (nanoseconds since the UNIX epoch), each present
    /// only if the kernel populated a non-zero slot for it.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RxTimestamps {
        /// Kernel software receive timestamp (ns since epoch), if provided (`ts[0]`).
        pub software_ns: Option<u64>,
        /// NIC raw hardware receive timestamp (ns since epoch), if provided (`ts[2]`).
        pub hardware_ns: Option<u64>,
    }

    fn ts_at(data: &[u8], i: usize) -> Option<u64> {
        let off = i * 16;
        let end = off + 16;
        if data.len() < end {
            return None;
        }
        let sec = i64::from_ne_bytes(data[off..off + 8].try_into().ok()?);
        let nsec = i64::from_ne_bytes(data[off + 8..end].try_into().ok()?);
        // The kernel leaves a slot as {0,0} when it did not fill it (fallback / not requested).
        if sec == 0 && nsec == 0 {
            return None;
        }
        if sec < 0 || nsec < 0 {
            return None;
        }
        (sec as u64).checked_mul(1_000_000_000)?.checked_add(nsec as u64)
    }

    /// Parse the `CMSG_DATA` payload of an `SCM_TIMESTAMPING(_NEW)` control message: three
    /// `timespec64` slots (`{i64 sec, i64 nsec}` little-/native-endian) = 48 bytes. `ts[0]` = software,
    /// `ts[1]` = deprecated, `ts[2]` = raw hardware. Pure (no `unsafe`) + unit-tested with synthetic
    /// buffers — this is the SW/HW-SEPARATE decoder `engine-sysopt::scm_timestamp_from_msghdr` lacks.
    pub fn parse_scm_timestamping64(data: &[u8]) -> RxTimestamps {
        RxTimestamps { software_ns: ts_at(data, 0), hardware_ns: ts_at(data, 2) }
    }

    /// Rolling stamp coverage counters. Feeds the review's "HW cmsg on ≥99.99% of packets before
    /// calling a number `hardware one-way`" gate.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct StampStats {
        /// Packets observed.
        pub total: u64,
        /// Packets that carried a software stamp.
        pub software: u64,
        /// Packets that carried a hardware stamp.
        pub hardware: u64,
        /// Packets that carried neither (fallback).
        pub missing: u64,
    }
    impl StampStats {
        /// Fold one receive into the counters.
        pub fn record(&mut self, ts: &RxTimestamps) {
            self.total += 1;
            if ts.hardware_ns.is_some() {
                self.hardware += 1;
            }
            if ts.software_ns.is_some() {
                self.software += 1;
            }
            if ts.hardware_ns.is_none() && ts.software_ns.is_none() {
                self.missing += 1;
            }
        }
        /// Fold counters already aggregated per underlying `recvmsg` below TLS/WebSocket framing.
        /// One callback can represent several socket reads, so adding these deltas preserves the
        /// real timestamp-coverage denominator.
        pub fn record_counts(
            &mut self,
            total: u64,
            software: u64,
            hardware: u64,
            missing: u64,
        ) {
            self.total = self.total.saturating_add(total);
            self.software = self.software.saturating_add(software.min(total));
            self.hardware = self.hardware.saturating_add(hardware.min(total));
            self.missing = self.missing.saturating_add(missing.min(total));
        }
        /// Fraction of packets with a hardware stamp (0.0 when none observed).
        pub fn hardware_fraction(&self) -> f64 {
            if self.total == 0 {
                0.0
            } else {
                self.hardware as f64 / self.total as f64
            }
        }
        /// Only true with a HW stamp on ≥99.99% of ≥10k packets — the bar for a "hardware one-way".
        pub fn hardware_trustworthy(&self) -> bool {
            self.total >= 10_000 && (self.hardware as u128) * 10_000 >= (self.total as u128) * 9_999
        }
    }
}

/// P10 — PTP/PHC + chrony clock-discipline probe. Decides whether an absolute sub-ms one-way claim is
/// defensible (HW timestamping + PHC + clock error ≤50µs), and otherwise supplies a clock-error
/// interval so a number is published with its uncertainty rather than false precision. All shell-outs
/// degrade to `None`/`false` — never panic. Pure parsers are unit-tested.
pub mod clock {
    use super::timestamping::StampStats;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Snapshot of the box's time discipline relevant to one-way latency claims.
    #[derive(Debug, Clone, Default)]
    pub struct ClockDiscipline {
        /// Primary NIC the stamps would reference.
        pub iface: String,
        /// The ENA PHC device (`/dev/ptp_ena` — the stable AWS symlink), if present.
        pub phc_device: Option<String>,
        /// The PHC clock name (`ena-ptp-*`) from `/sys/class/ptp/*/clock_name`, if found.
        pub phc_clock_name: Option<String>,
        /// Whether `ethtool -T` reports hardware receive timestamping capability.
        pub hw_rx_capable: bool,
        /// Whether this process successfully completed `SIOCSHWTSTAMP` with a non-`NONE` effective
        /// hardware RX filter for `iface`. Capability or a pre-existing configuration alone does not
        /// set this.
        pub hw_rx_configured: bool,
        /// Whether chrony reports a PHC as its selected source and its config names the exact PHC
        /// device derived from this NIC's `ethtool -T` index.
        pub selected_phc_disciplined: bool,
        /// chrony `Last offset` (ns, signed), if chrony is running.
        pub last_offset_ns: Option<i64>,
        /// chrony `Root dispersion` (ns), if chrony is running.
        pub root_dispersion_ns: Option<u64>,
    }
    impl ClockDiscipline {
        /// Clock-error interval = `|last offset| + root dispersion` (ns). `None` if chrony data absent.
        pub fn clock_error_ns(&self) -> Option<u64> {
            match (self.last_offset_ns, self.root_dispersion_ns) {
                (Some(o), Some(d)) => Some(o.unsigned_abs().saturating_add(d)),
                _ => None,
            }
        }
        /// A sub-ms absolute one-way claim needs every independent evidence layer: driver
        /// capability, successful device configuration, the selected same-NIC PHC, ≤50µs clock
        /// error, and an actual raw-HW cmsg on ≥99.99% of at least 10,000 receives.
        pub fn hardware_one_way_defensible(&self, stamps: &StampStats) -> bool {
            self.hw_rx_capable
                && self.hw_rx_configured
                && self.phc_device.is_some()
                && self.selected_phc_disciplined
                && self.clock_error_ns().map(|e| e <= 50_000).unwrap_or(false)
                && stamps.hardware_trustworthy()
        }
    }

    /// The interface of the default route (from `/proc/net/route`; dependency-free).
    pub fn default_iface() -> Option<String> {
        engine_sysopt::default_route_iface()
    }

    /// `ethtool -T` output reports hardware receive timestamping (`hardware-receive` capability).
    pub fn ethtool_t_has_hw_rx(out: &str) -> bool {
        out.to_lowercase().contains("hardware-receive")
    }

    /// Parse the `PTP Hardware Clock: N` index reported by `ethtool -T`. `none`, negative, missing,
    /// and malformed values return `None` so an unrelated `/dev/ptp*` can never be selected.
    pub fn ethtool_t_phc_index(out: &str) -> Option<u32> {
        out.lines().find_map(|line| {
            let (label, value) = line.trim().split_once(':')?;
            if !label.trim().eq_ignore_ascii_case("PTP Hardware Clock") {
                return None;
            }
            value.trim().parse::<u32>().ok()
        })
    }

    /// Extract the seconds value following `label:` and convert to signed ns. Handles `+`/`-` and a
    /// trailing unit word (e.g. `Last offset     : +0.000001234 seconds`).
    fn labeled_seconds_ns(out: &str, label: &str) -> Option<i64> {
        for line in out.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix(label) {
                // `rest` may be "   : +0.000001234 seconds": drop leading spaces, then the ':'.
                let rest = rest.trim().trim_start_matches(':').trim();
                let tok = rest.split_whitespace().next()?;
                let secs: f64 = tok.parse().ok()?;
                if !secs.is_finite() {
                    return None;
                }
                let ns = (secs * 1e9).round();
                if !ns.is_finite() || ns < i64::MIN as f64 || ns > i64::MAX as f64 {
                    return None;
                }
                return Some(ns as i64);
            }
        }
        None
    }

    /// Parse `chronyc tracking` → (last offset ns, root dispersion ns).
    pub fn parse_chrony_tracking(out: &str) -> (Option<i64>, Option<u64>) {
        let offset = labeled_seconds_ns(out, "Last offset");
        let disp = labeled_seconds_ns(out, "Root dispersion").map(|v| v.unsigned_abs());
        (offset, disp)
    }

    /// Return the unique refclock selected by `chronyc sources` (`#*`). Selection is not sufficient
    /// evidence by itself: the returned refid must be mapped back to the exact configured PHC.
    fn selected_chrony_refclock(out: &str) -> Option<&str> {
        let mut selected = out.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let marker = fields.next().unwrap_or_default();
            let source = fields.next().unwrap_or_default();
            (marker == "#*" && !source.is_empty()).then_some(source)
        });
        let first = selected.next()?;
        selected.next().is_none().then_some(first)
    }

    fn active_chrony_refclock_fields(line: &str) -> Option<Vec<&str>> {
        let active = line.split('#').next().unwrap_or_default().trim();
        let fields: Vec<_> = active.split_whitespace().collect();
        matches!(fields.first(), Some(v) if v.eq_ignore_ascii_case("refclock")).then_some(fields)
    }

    fn configured_refclock_id(fields: &[&str], ordinal: usize) -> Option<String> {
        let driver = *fields.get(1)?;
        if let Some(index) = fields
            .iter()
            .position(|field| field.eq_ignore_ascii_case("refid"))
        {
            return fields.get(index + 1).map(|value| (*value).to_string());
        }
        let prefix: String = driver
            .chars()
            .take(3)
            .map(|ch| ch.to_ascii_uppercase())
            .collect();
        (!prefix.is_empty()).then(|| format!("{prefix}{ordinal}"))
    }

    /// Bind the unique selected chrony refclock to the exact NIC PHC configuration. The mapping
    /// honours explicit `refid`, chrony's default DRIVER+ordinal refid, and `noselect`; a non-PHC
    /// source spoofing a `PHC*` refid can therefore never certify the NIC clock.
    pub fn chrony_selected_phc_matches_device(sources: &str, config: &str, device: &str) -> bool {
        let Some(selected_refid) = selected_chrony_refclock(sources) else {
            return false;
        };
        let mut selected_target_bindings = 0usize;
        for (ordinal, fields) in config
            .lines()
            .filter_map(active_chrony_refclock_fields)
            .enumerate()
        {
            let Some(refid) = configured_refclock_id(&fields, ordinal) else {
                return false;
            };
            if !refid.eq_ignore_ascii_case(selected_refid) {
                continue;
            }
            let driver_is_phc = matches!(fields.get(1), Some(v) if v.eq_ignore_ascii_case("PHC"));
            let configured_device = fields
                .get(2)
                .and_then(|value| value.split(':').next())
                .unwrap_or_default();
            let selectable = !fields
                .iter()
                .any(|field| field.eq_ignore_ascii_case("noselect"));
            if !driver_is_phc || configured_device != device || !selectable {
                return false;
            }
            selected_target_bindings += 1;
        }
        selected_target_bindings == 1
    }

    fn configured_chrony_selected_phc_matches_device(sources: &str, device: &str) -> bool {
        ["/etc/chrony/chrony.conf", "/etc/chrony.conf"]
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
            .map(|config| chrony_selected_phc_matches_device(sources, &config, device))
            .unwrap_or(false)
    }

    fn phc_device_for_index(index: u32) -> Option<String> {
        let indexed = PathBuf::from(format!("/dev/ptp{index}"));
        let stable = Path::new("/dev/ptp_ena");
        if stable.exists() {
            // The stable AWS symlink is accepted only when it resolves to THIS interface's
            // ethtool-derived PHC. A mismatched ptp_ena is worse than no evidence.
            let stable_real = std::fs::canonicalize(stable).ok()?;
            let indexed_real = std::fs::canonicalize(&indexed).ok()?;
            return (stable_real == indexed_real).then(|| "/dev/ptp_ena".to_string());
        }
        indexed
            .exists()
            .then(|| indexed.to_string_lossy().into_owned())
    }

    fn phc_clock_name_for_index(index: u32) -> Option<String> {
        let name = std::fs::read_to_string(format!("/sys/class/ptp/ptp{index}/clock_name")).ok()?;
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_string())
    }

    /// Probe the live box. Best-effort: any missing tool yields `None`/`false`.
    pub fn probe(iface: &str) -> ClockDiscipline {
        let ethtool = if iface.is_empty() {
            None
        } else {
            Command::new("ethtool")
                .arg("-T")
                .arg(iface)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let hw_rx_capable = ethtool.as_deref().map(ethtool_t_has_hw_rx).unwrap_or(false);
        let phc_index = ethtool.as_deref().and_then(ethtool_t_phc_index);
        let phc_device = phc_index.and_then(phc_device_for_index);
        let phc_clock_name = phc_index.and_then(phc_clock_name_for_index);
        let hw_rx_configured = hw_rx_capable
            && !iface.is_empty()
            && engine_sysopt::configure_hardware_rx_timestamping_for_iface(iface);
        let (last_offset_ns, root_dispersion_ns) = Command::new("chronyc")
            .arg("tracking")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|o| parse_chrony_tracking(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or((None, None));
        let chrony_sources = Command::new("chronyc")
            .args(["sources", "-n"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned());
        let selected_phc_disciplined = phc_device
            .as_deref()
            .zip(chrony_sources.as_deref())
            .map(|(device, sources)| configured_chrony_selected_phc_matches_device(sources, device))
            .unwrap_or(false);
        ClockDiscipline {
            iface: iface.to_string(),
            phc_device,
            phc_clock_name,
            hw_rx_capable,
            hw_rx_configured,
            selected_phc_disciplined,
            last_offset_ns,
            root_dispersion_ns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clock::*;
    use super::timestamping::*;

    fn ts_bytes(slots: &[(i64, i64)]) -> Vec<u8> {
        let mut v = Vec::with_capacity(48);
        for i in 0..3 {
            let (s, n) = slots.get(i).copied().unwrap_or((0, 0));
            v.extend_from_slice(&s.to_ne_bytes());
            v.extend_from_slice(&n.to_ne_bytes());
        }
        v
    }

    fn trusted_hw_stamps() -> StampStats {
        let mut stamps = StampStats::default();
        stamps.record_counts(10_000, 10_000, 9_999, 0);
        stamps
    }

    #[test]
    fn parse_software_only() {
        let b = ts_bytes(&[(100, 200), (0, 0), (0, 0)]);
        let ts = parse_scm_timestamping64(&b);
        assert_eq!(ts.software_ns, Some(100 * 1_000_000_000 + 200));
        assert_eq!(ts.hardware_ns, None);
    }

    #[test]
    fn parse_hardware_slot() {
        let b = ts_bytes(&[(0, 0), (0, 0), (5, 6)]);
        let ts = parse_scm_timestamping64(&b);
        assert_eq!(ts.software_ns, None);
        assert_eq!(ts.hardware_ns, Some(5 * 1_000_000_000 + 6));
    }

    #[test]
    fn parse_all_zero_and_short_are_missing() {
        assert_eq!(parse_scm_timestamping64(&ts_bytes(&[])), RxTimestamps::default());
        // A truncated (<48B) buffer must not panic and must report nothing.
        assert_eq!(parse_scm_timestamping64(&[0u8; 10]), RxTimestamps::default());
    }

    #[test]
    fn stamp_stats_hardware_gate() {
        let mut s = StampStats::default();
        // 9,999 HW + 1 SW-only over 10,000 → 99.99% exactly → trustworthy.
        for _ in 0..9_999 {
            s.record(&RxTimestamps { software_ns: Some(1), hardware_ns: Some(2) });
        }
        s.record(&RxTimestamps { software_ns: Some(1), hardware_ns: None });
        assert_eq!(s.total, 10_000);
        assert_eq!(s.hardware, 9_999);
        assert!(s.hardware_trustworthy(), "9999/10000 == 99.99% must pass");
        let mut below = StampStats::default();
        below.record_counts(10_000, 10_000, 9_998, 0);
        assert!(!below.hardware_trustworthy(), "9998/10000 is below 99.99%");
        // Below 10k samples is never trustworthy regardless of fraction.
        let mut few = StampStats::default();
        few.record(&RxTimestamps { software_ns: None, hardware_ns: Some(1) });
        assert!(!few.hardware_trustworthy());
        assert_eq!(few.hardware_fraction(), 1.0);
    }

    #[test]
    fn stamp_stats_fold_preserves_recvmsg_denominator() {
        let mut stats = StampStats::default();
        stats.record_counts(4, 3, 2, 1);
        stats.record_counts(2, 2, 0, 0);
        assert_eq!(stats.total, 6);
        assert_eq!(stats.software, 5);
        assert_eq!(stats.hardware, 2);
        assert_eq!(stats.missing, 1);
    }

    #[test]
    fn chrony_tracking_parse() {
        let sample = "\
Reference ID    : 169.254.169.123 (169.254.169.123)
Stratum         : 4
Ref time (UTC)  : Mon Jul 13 00:00:00 2026
System time     : 0.000000500 seconds slow of NTP time
Last offset     : +0.000001234 seconds
RMS offset      : 0.000002000 seconds
Frequency       : 1.234 ppm slow
Residual freq   : +0.000 ppm
Skew            : 0.010 ppm
Root delay      : 0.000123000 seconds
Root dispersion : 0.000045000 seconds
Update interval : 16.0 seconds
Leap status     : Normal
";
        let (off, disp) = parse_chrony_tracking(sample);
        assert_eq!(off, Some(1234)); // +0.000001234 s = 1234 ns
        assert_eq!(disp, Some(45_000)); // 0.000045 s = 45,000 ns
        let d = ClockDiscipline {
            hw_rx_capable: true,
            hw_rx_configured: true,
            phc_device: Some("/dev/ptp_ena".into()),
            selected_phc_disciplined: true,
            last_offset_ns: off,
            root_dispersion_ns: disp,
            ..Default::default()
        };
        let stamps = trusted_hw_stamps();
        // |1234| + 45000 = 46,234 ns ≤ 50µs and every HW/PHC predicate present → defensible.
        assert_eq!(d.clock_error_ns(), Some(46_234));
        assert!(d.hardware_one_way_defensible(&stamps));
        // Bump dispersion over 50µs → no longer defensible.
        let mut d2 = d.clone();
        d2.root_dispersion_ns = Some(60_000);
        assert!(!d2.hardware_one_way_defensible(&stamps));
    }

    #[test]
    fn chrony_tracking_rejects_non_finite_or_out_of_range_values() {
        for value in ["NaN", "+inf", "-inf", "1e100"] {
            let sample =
                format!("Last offset : {value} seconds\nRoot dispersion : {value} seconds\n");
            assert_eq!(
                parse_chrony_tracking(&sample),
                (None, None),
                "non-finite/out-of-range clock evidence must fail closed: {value}"
            );
        }
    }

    #[test]
    fn hardware_claim_rejects_missing_runtime_timestamp_evidence() {
        let d = ClockDiscipline {
            iface: "ens5".into(),
            phc_device: Some("/dev/ptp_ena".into()),
            phc_clock_name: Some("ena-ptp-0".into()),
            hw_rx_capable: true,
            hw_rx_configured: true,
            selected_phc_disciplined: true,
            last_offset_ns: Some(1_000),
            root_dispersion_ns: Some(1_000),
        };

        assert!(
            !d.hardware_one_way_defensible(&StampStats::default()),
            "capability + PHC presence + clock error cannot substitute for observed HW cmsg coverage"
        );
    }

    #[test]
    fn hardware_claim_requires_every_independent_evidence_layer() {
        let good = ClockDiscipline {
            iface: "ens5".into(),
            phc_device: Some("/dev/ptp_ena".into()),
            phc_clock_name: Some("ena-ptp-0".into()),
            hw_rx_capable: true,
            hw_rx_configured: true,
            selected_phc_disciplined: true,
            last_offset_ns: Some(1_000),
            root_dispersion_ns: Some(1_000),
        };
        let stamps = trusted_hw_stamps();
        assert!(good.hardware_one_way_defensible(&stamps));

        let mut missing = good.clone();
        missing.hw_rx_capable = false;
        assert!(!missing.hardware_one_way_defensible(&stamps));
        missing = good.clone();
        missing.hw_rx_configured = false;
        assert!(!missing.hardware_one_way_defensible(&stamps));
        missing = good.clone();
        missing.selected_phc_disciplined = false;
        assert!(!missing.hardware_one_way_defensible(&stamps));
        missing = good.clone();
        missing.phc_device = None;
        assert!(!missing.hardware_one_way_defensible(&stamps));
        missing = good.clone();
        missing.root_dispersion_ns = Some(50_000);
        assert!(!missing.hardware_one_way_defensible(&stamps));
    }

    #[test]
    fn selected_phc_and_same_device_config_are_parsed_strictly() {
        let selected = "\
MS Name/IP address         Stratum Poll Reach LastRx Last sample
===============================================================================
#* PHC0                          0   0   377     0   +184ns[ +198ns] +/- 5032ns
^- 169.254.169.123               1   4   377     8    -18us[  -18us] +/- 115us
";
        let config = "\
# refclock PHC /dev/ptp0 prefer
refclock PHC /dev/ptp_ena poll 0 delay 0.000010 prefer trust
";
        assert!(chrony_selected_phc_matches_device(
            selected,
            config,
            "/dev/ptp_ena"
        ));
        assert!(!chrony_selected_phc_matches_device(
            &selected.replace("#*", "#+"),
            config,
            "/dev/ptp_ena"
        ));
        assert!(!chrony_selected_phc_matches_device(
            selected,
            config,
            "/dev/ptp0"
        ));

        // Adversarial binding case: chrony selected PHC0, which is configured for /dev/ptp0;
        // merely mentioning the target as a second PHC1 must not certify /dev/ptp_ena.
        let multi_sources = "\
MS Name/IP address         Stratum Poll Reach LastRx Last sample
===============================================================================
#* PHC0                          0   0   377     0   +100ns[ +100ns] +/- 5000ns
#+ PHC1                          0   0   377     0   +200ns[ +200ns] +/- 5000ns
";
        let multi_config = "\
refclock PHC /dev/ptp0 poll 0 prefer trust
refclock PHC /dev/ptp_ena poll 0 trust
";
        assert!(
            !chrony_selected_phc_matches_device(multi_sources, multi_config, "/dev/ptp_ena"),
            "target PHC merely present beside another active PHC must fail closed"
        );

        let spoofed_sources = "\
MS Name/IP address         Stratum Poll Reach LastRx Last sample
===============================================================================
#* PHC0                          0   0   377     0   +100ns[ +100ns] +/- 5000ns
";
        let spoofed_config = "\
refclock PHC /dev/ptp_ena refid ENA0 noselect
refclock SHM 0 refid PHC0
";
        assert!(
            !chrony_selected_phc_matches_device(spoofed_sources, spoofed_config, "/dev/ptp_ena"),
            "a selected non-PHC refclock named PHC0 cannot certify a noselect NIC PHC"
        );

        let explicit_sources = spoofed_sources.replace("PHC0", "ENA0");
        let explicit_config = "refclock PHC /dev/ptp_ena refid ENA0 prefer trust\n";
        assert!(chrony_selected_phc_matches_device(
            &explicit_sources,
            explicit_config,
            "/dev/ptp_ena"
        ));
    }

    #[test]
    fn ethtool_t_hw_rx_detect() {
        let with = "Capabilities:\n\thardware-transmit\n\thardware-receive\n\thardware-raw-clock\nPTP Hardware Clock: 3";
        let without = "Capabilities:\n\tsoftware-transmit\n\tsoftware-receive";
        assert!(ethtool_t_has_hw_rx(with));
        assert!(!ethtool_t_has_hw_rx(without));
        assert_eq!(ethtool_t_phc_index(with), Some(3));
        assert_eq!(ethtool_t_phc_index("PTP Hardware Clock: none"), None);
        assert_eq!(ethtool_t_phc_index("PTP Hardware Clock: -1"), None);
    }
}
