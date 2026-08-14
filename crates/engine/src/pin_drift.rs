//! [P1.2 · task #19] SUBMIT_PIN_DRIFT guard — background re-probe of the sequencer A-record set
//! against the pinned submit IP.
//!
//! `SUBMIT_PIN_IP` freezes the FCFS-race-deciding submit leg to the AZ-probe-winning sequencer IP
//! (measured 2026-07-08: the pin was 2× better than both alternates — p50 908µs RTT vs ~1.9ms).
//! But the sequencer endpoint is multi-A-record behind a load balancer: IPs rotate, and a pin
//! that WAS optimal can silently start losing the race by hundreds of µs. This guard re-probes
//! every A-record (fresh DNS each window, so NEW IPs are seen) plus the pin every ~10 minutes on
//! the background core, and logs a re-pin recommendation once the pin loses to the best
//! alternative by more than `SUBMIT_PIN_DRIFT_THRESHOLD_US` (default 100µs) p50 for
//! `SUBMIT_PIN_DRIFT_WINDOWS` (default 3) consecutive windows — sustained loss, not a blip.
//!
//! Observation only: it never re-pins by itself (the pin is an operator decision tied to the AZ
//! campaign), and it costs a handful of TCP connect probes per window — nothing on the hot path.
//! Enabled automatically when `SUBMIT_PIN_IP` is set; `SUBMIT_PIN_DRIFT=0` opts out.

use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// One probed endpoint: the IP and its TCP-connect (SYN→SYN-ACK) p50 over the window's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpP50 {
    /// The probed sequencer IP.
    pub ip: IpAddr,
    /// Median TCP-connect round-trip for this window's samples, in nanoseconds.
    pub p50_ns: u64,
}

/// Consecutive-loss tracker: the recommendation needs SUSTAINED loss (N windows), not one noisy
/// window — transient cross-AZ jitter must not churn the pin.
#[derive(Default)]
pub struct PinDriftState {
    consecutive_losses: u32,
}

/// Verdict of one probe window.
#[derive(Debug, PartialEq, Eq)]
pub enum WindowVerdict {
    /// Pin still competitive (or nothing to compare against): counter reset.
    PinHolds,
    /// Pin lost this window but the streak is below the recommendation bar.
    Losing {
        /// Consecutive losing windows so far.
        streak: u32,
    },
    /// Sustained loss: recommend re-pinning to `best`.
    RecommendRepin {
        /// The lowest-p50 alternative to re-pin to.
        best: IpP50,
        /// Consecutive losing windows at recommendation time.
        streak: u32,
    },
}

/// Pure decision core (unit-testable): compare the pin's p50 against the best alternative and
/// advance the consecutive-loss streak. A window with no pin measurement or no alternatives
/// resets the streak (fail-quiet: no data ⇒ no recommendation).
pub fn evaluate_window(
    pin_p50_ns: Option<u64>,
    alternatives: &[IpP50],
    threshold_ns: u64,
    need_windows: u32,
    state: &mut PinDriftState,
) -> WindowVerdict {
    let (Some(pin), Some(best)) = (
        pin_p50_ns,
        alternatives.iter().min_by_key(|a| a.p50_ns).copied(),
    ) else {
        state.consecutive_losses = 0;
        return WindowVerdict::PinHolds;
    };
    if best.p50_ns.saturating_add(threshold_ns) < pin {
        state.consecutive_losses += 1;
        if state.consecutive_losses >= need_windows {
            return WindowVerdict::RecommendRepin {
                best,
                streak: state.consecutive_losses,
            };
        }
        return WindowVerdict::Losing {
            streak: state.consecutive_losses,
        };
    }
    state.consecutive_losses = 0;
    WindowVerdict::PinHolds
}

/// TCP-connect p50 to one concrete address (`samples` sequential SYN handshakes, DNS excluded by
/// construction — the address is already resolved). ~`samples`×RTT of wall time per IP.
fn probe_addr_p50(addr: SocketAddr, samples: u32) -> Option<u64> {
    let mut xs = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        let t0 = Instant::now();
        if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            xs.push(t0.elapsed().as_nanos() as u64);
            drop(s);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    if xs.is_empty() {
        return None;
    }
    xs.sort_unstable();
    Some(xs[xs.len() / 2])
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Extract `host:port` from the sequencer endpoint URL (https default 443).
fn host_port(url: &str) -> Option<(String, u16)> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_string();
    let port = u.port_or_known_default().unwrap_or(443);
    Some((host, port))
}

/// Run the guard loop until `shutdown`. One window ≈ resolve the A-record set fresh, probe the
/// pin + every non-pin address, log the table, and evaluate the sustained-loss verdict.
pub fn pin_drift_loop(shutdown: &AtomicBool, sequencer_url: &str, pin: IpAddr) {
    let Some((host, port)) = host_port(sequencer_url) else {
        warn!(url = %sequencer_url, "SUBMIT_PIN_DRIFT: unparsable sequencer endpoint; guard off");
        return;
    };
    let window_secs = env_u64("SUBMIT_PIN_DRIFT_SECS", 600).max(30);
    let threshold_ns = env_u64("SUBMIT_PIN_DRIFT_THRESHOLD_US", 100).saturating_mul(1_000);
    let need_windows = env_u64("SUBMIT_PIN_DRIFT_WINDOWS", 3).clamp(1, 100) as u32;
    let samples = env_u64("SUBMIT_PIN_DRIFT_SAMPLES", 15).clamp(3, 200) as u32;
    info!(
        %host, port, %pin, window_secs, threshold_us = threshold_ns / 1_000, need_windows,
        samples, "SUBMIT_PIN_DRIFT guard online (observation-only; probes on the bg core)"
    );
    let mut state = PinDriftState::default();
    let mut last_window = Instant::now() - Duration::from_secs(window_secs); // first window immediately
    while !shutdown.load(Ordering::Acquire) {
        if last_window.elapsed().as_secs() < window_secs {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        last_window = Instant::now();
        // Fresh DNS each window: a rotated-in BETTER IP must be seen, not just the boot set.
        let resolved: Vec<SocketAddr> = (host.as_str(), port)
            .to_socket_addrs()
            .map(|a| a.collect())
            .unwrap_or_default();
        let pin_p50 = probe_addr_p50(SocketAddr::new(pin, port), samples);
        let mut alternatives: Vec<IpP50> = Vec::with_capacity(resolved.len());
        for a in resolved.iter().filter(|a| a.ip() != pin) {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            if let Some(p50_ns) = probe_addr_p50(*a, samples) {
                alternatives.push(IpP50 { ip: a.ip(), p50_ns });
            }
        }
        let table: Vec<String> = alternatives
            .iter()
            .map(|a| format!("{}={:.1}µs", a.ip, a.p50_ns as f64 / 1_000.0))
            .collect();
        info!(
            pin_p50_us = pin_p50.map(|p| p as f64 / 1_000.0),
            alternatives = ?table,
            "SUBMIT_PIN_DRIFT window"
        );
        match evaluate_window(pin_p50, &alternatives, threshold_ns, need_windows, &mut state) {
            WindowVerdict::RecommendRepin { best, streak } => warn!(
                target: "diag",
                %pin,
                pin_p50_us = pin_p50.unwrap_or(0) as f64 / 1_000.0,
                best_ip = %best.ip,
                best_p50_us = best.p50_ns as f64 / 1_000.0,
                streak,
                "SUBMIT_PIN_DRIFT: pinned submit IP has lost >threshold p50 for {streak} consecutive windows — recommend re-pin: SUBMIT_PIN_IP={}", best.ip
            ),
            WindowVerdict::Losing { streak } => info!(
                streak,
                "SUBMIT_PIN_DRIFT: pin losing this window (streak below recommendation bar)"
            ),
            WindowVerdict::PinHolds => {}
        }
    }
}

/// Spawn the guard when a pin is configured (`SUBMIT_PIN_IP`) and not opted out
/// (`SUBMIT_PIN_DRIFT=0`). Runs on the background core (0) — never a hot-path thread.
pub fn spawn_if_configured(
    shutdown: Arc<AtomicBool>,
    sequencer_url: String,
) -> Option<std::thread::JoinHandle<()>> {
    let pin: IpAddr = std::env::var("SUBMIT_PIN_IP")
        .ok()
        .and_then(|s| s.trim().parse().ok())?;
    let enabled = std::env::var("SUBMIT_PIN_DRIFT")
        .map(|v| v.trim() != "0" && !v.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    if !enabled {
        info!("SUBMIT_PIN_DRIFT=0 — pin drift guard disabled");
        return None;
    }
    std::thread::Builder::new()
        .name("engine-pindrift".into())
        .spawn(move || {
            crate::runtime::pin_to_core(0, "engine-pindrift");
            pin_drift_loop(&shutdown, &sequencer_url, pin);
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([18, 224, 87, last])
    }

    #[test]
    fn recommends_only_after_sustained_loss_and_resets_on_recovery() {
        let mut st = PinDriftState::default();
        let alts = [IpP50 {
            ip: ip(34),
            p50_ns: 700_000, // 700µs — beats the 908µs pin by >100µs
        }];
        // Windows 1–2: losing, but below the 3-window bar.
        assert_eq!(
            evaluate_window(Some(908_000), &alts, 100_000, 3, &mut st),
            WindowVerdict::Losing { streak: 1 }
        );
        assert_eq!(
            evaluate_window(Some(908_000), &alts, 100_000, 3, &mut st),
            WindowVerdict::Losing { streak: 2 }
        );
        // Window 3: sustained → recommend the best alternative.
        assert_eq!(
            evaluate_window(Some(908_000), &alts, 100_000, 3, &mut st),
            WindowVerdict::RecommendRepin {
                best: alts[0],
                streak: 3
            }
        );
        // Recovery (pin wins again) resets the streak — the next loss starts from 1.
        assert_eq!(
            evaluate_window(Some(650_000), &alts, 100_000, 3, &mut st),
            WindowVerdict::PinHolds
        );
        assert_eq!(
            evaluate_window(Some(908_000), &alts, 100_000, 3, &mut st),
            WindowVerdict::Losing { streak: 1 }
        );
    }

    #[test]
    fn within_threshold_is_not_a_loss_and_best_alternative_is_picked() {
        let mut st = PinDriftState::default();
        // Best alt is only 50µs better — inside the 100µs threshold ⇒ pin holds (no churn).
        let close = [IpP50 {
            ip: ip(34),
            p50_ns: 858_000,
        }];
        assert_eq!(
            evaluate_window(Some(908_000), &close, 100_000, 1, &mut st),
            WindowVerdict::PinHolds
        );
        // With several alternatives the MINIMUM p50 is the comparator/recommendation.
        let alts = [
            IpP50 { ip: ip(34), p50_ns: 800_000 },
            IpP50 { ip: ip(35), p50_ns: 600_000 },
            IpP50 { ip: ip(36), p50_ns: 900_000 },
        ];
        match evaluate_window(Some(908_000), &alts, 100_000, 1, &mut st) {
            WindowVerdict::RecommendRepin { best, .. } => assert_eq!(best.ip, ip(35)),
            other => panic!("expected recommendation, got {other:?}"),
        }
    }

    #[test]
    fn missing_data_fails_quiet_and_resets_the_streak() {
        let mut st = PinDriftState::default();
        let alts = [IpP50 { ip: ip(34), p50_ns: 1 }];
        assert_eq!(
            evaluate_window(Some(1_000_000), &alts, 0, 2, &mut st),
            WindowVerdict::Losing { streak: 1 }
        );
        // Pin unreachable this window (None) ⇒ no verdict AND the streak resets — a dead probe
        // must not accumulate into a recommendation.
        assert_eq!(
            evaluate_window(None, &alts, 0, 2, &mut st),
            WindowVerdict::PinHolds
        );
        // No alternatives resolved ⇒ same fail-quiet reset.
        assert_eq!(
            evaluate_window(Some(1_000_000), &[], 0, 2, &mut st),
            WindowVerdict::PinHolds
        );
    }
}
