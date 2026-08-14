//! §14 telemetry-breach monitor + paging.
//!
//! Centralizes the "trip the kill switch when telemetry crosses a danger threshold" logic that
//! was previously an ad-hoc `consecutive_submit_failures >= 5` counter inline in the submit
//! loop. A `TelemetryBreachMonitor` watches four breach conditions and, on the first crossing,
//! (1) trips the global [`crate::KillSwitch`] and (2) fires a [`Pager`] so an operator is woken
//! up — rather than the bot silently halting and nobody noticing for hours.
//!
//! Breach conditions (each independently armed; any one trips global):
//!   * **consecutive submit failures** — dead sequencer path / bad nonce lane.
//!   * **consecutive realized losses** — our profit model is wrong vs chain (adverse selection).
//!   * **latency p99 ceiling** — the hot path is too slow to win FCFS; submitting is just paying
//!     gas to lose.
//!   * **explicit external trip** — operator or another subsystem calls `force_breach`.
//!
//! Default-on: the monitor is always constructed with sane thresholds. The `LogPager` (logs at
//! ERROR) is always active; a `WebhookPager` is added when `PAGER_WEBHOOK_URL` is set. No extra
//! script needed — breach paging is live the moment the engine starts.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::kill_switch::KillSwitches;

/// A paging sink. Implementations must be cheap + non-blocking on the hot path — a breach page
/// fires at most once per breach, but the call still happens from the submit thread.
pub trait Pager: Send + Sync {
    /// Deliver a one-line breach alert. `reason` is a short machine-tag (e.g.
    /// `"submit_failures"`); `detail` is a human sentence.
    fn page(&self, reason: &str, detail: &str);
}

/// Always-on pager: emits an ERROR-level tracing event. Picked up by any log sink the operator
/// has wired (journald, Loki, CloudWatch). Zero external dependency.
pub struct LogPager;

impl Pager for LogPager {
    fn page(&self, reason: &str, detail: &str) {
        tracing::error!(target: "breach", reason, detail, "TELEMETRY BREACH — kill switch tripped");
    }
}

/// Per-reason rate-limiting wrapper. A flapping breach (e.g. the kill switch toggling, or a
/// validator that alternates pass/fail) could otherwise spam the downstream pager. This wrapper
/// delivers at most one page **per distinct reason** per cooldown window; pages for *different*
/// reasons are never suppressed (an operator must still see a new failure mode immediately).
///
/// The clock is injected as a `now_ns` closure so the cooldown is unit-testable without sleeps.
pub struct RateLimitedPager<P: Pager> {
    inner: P,
    cooldown_ns: u64,
    last_paged: parking_lot::Mutex<std::collections::HashMap<String, u64>>,
    now_ns: fn() -> u64,
}

fn wall_now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

impl<P: Pager> RateLimitedPager<P> {
    /// Wrap `inner`, suppressing repeat pages of the same reason within `cooldown_secs`.
    pub fn new(inner: P, cooldown_secs: u64) -> Self {
        Self {
            inner,
            cooldown_ns: cooldown_secs.saturating_mul(1_000_000_000),
            last_paged: parking_lot::Mutex::new(std::collections::HashMap::new()),
            now_ns: wall_now_ns,
        }
    }

    /// Test seam: construct with an injected clock.
    #[cfg(test)]
    fn with_clock(inner: P, cooldown_ns: u64, now_ns: fn() -> u64) -> Self {
        Self {
            inner,
            cooldown_ns,
            last_paged: parking_lot::Mutex::new(std::collections::HashMap::new()),
            now_ns,
        }
    }

    /// Decide whether to deliver, updating the per-reason timestamp. Returns true if delivered.
    fn try_deliver(&self, reason: &str, detail: &str, now: u64) -> bool {
        {
            let mut map = self.last_paged.lock();
            if let Some(&last) = map.get(reason) {
                if now.saturating_sub(last) < self.cooldown_ns {
                    return false; // within cooldown for this reason — suppress
                }
            }
            map.insert(reason.to_string(), now);
        }
        self.inner.page(reason, detail);
        true
    }
}

impl<P: Pager> Pager for RateLimitedPager<P> {
    fn page(&self, reason: &str, detail: &str) {
        let now = (self.now_ns)();
        self.try_deliver(reason, detail, now);
    }
}

/// Webhook payload dialect. Different alerting backends expect different JSON shapes; we cover
/// the three common ones plus a generic envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookFormat {
    /// Slack incoming-webhook: `{"text": "..."}`.
    Slack,
    /// Generic JSON: `{"reason": "...", "detail": "...", "source": "mantis"}`.
    Generic,
}

impl WebhookFormat {
    /// Pick the format from a hint string (`PAGER_WEBHOOK_FORMAT`). Defaults to Slack (the most
    /// common incoming-webhook shape).
    pub fn from_hint(hint: Option<&str>) -> Self {
        match hint.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("generic") | Some("json") => WebhookFormat::Generic,
            _ => WebhookFormat::Slack,
        }
    }
}

/// Build the JSON body for a breach page. Pure + deterministic so it's unit-testable without a
/// network. `reason`/`detail` are escaped for JSON string context.
pub fn build_webhook_payload(format: WebhookFormat, reason: &str, detail: &str) -> String {
    let r = json_escape(reason);
    let d = json_escape(detail);
    match format {
        WebhookFormat::Slack => {
            format!("{{\"text\":\"🚨 MANTIS BREACH [{r}]: {d}\"}}")
        }
        WebhookFormat::Generic => {
            format!("{{\"source\":\"mantis\",\"reason\":\"{r}\",\"detail\":\"{d}\"}}")
        }
    }
}

/// Minimal JSON string escaper (quotes, backslashes, control chars). Breach reasons/details are
/// engine-generated so this only needs to cover the characters that actually appear, but we
/// escape the full JSON-required set to be safe.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Pager that POSTs the breach alert to a webhook URL (Slack / PagerDuty-events / generic JSON).
/// Fires at most once per breach (cold path) with a short timeout so a slow/down webhook can't
/// stall the submit thread for long.
pub struct WebhookPager {
    url: String,
    format: WebhookFormat,
    timeout_ms: u64,
}

impl WebhookPager {
    /// Construct targeting `url` with the given payload `format`.
    pub fn new(url: String, format: WebhookFormat) -> Self {
        Self {
            url,
            format,
            timeout_ms: 2_000,
        }
    }
}

impl Pager for WebhookPager {
    fn page(&self, reason: &str, detail: &str) {
        let body = build_webhook_payload(self.format, reason, detail);
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .build();
        match agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(_) => {}
            Err(e) => {
                // Don't let a failed page mask the breach — the LogPager already recorded it.
                tracing::warn!(target: "breach", ?e, "WebhookPager POST failed");
            }
        }
    }
}

/// Fan-out pager: delivers each breach to every child pager. Used to combine the always-on
/// `LogPager` with an optional `WebhookPager`.
pub struct MultiPager {
    pagers: Vec<Box<dyn Pager>>,
}

impl MultiPager {
    /// New empty fan-out.
    pub fn new() -> Self {
        Self { pagers: Vec::new() }
    }

    /// Add a pager to the fan-out (builder style).
    pub fn with(mut self, p: Box<dyn Pager>) -> Self {
        self.pagers.push(p);
        self
    }

    /// Number of child pagers.
    pub fn len(&self) -> usize {
        self.pagers.len()
    }

    /// Whether there are no child pagers.
    pub fn is_empty(&self) -> bool {
        self.pagers.is_empty()
    }
}

impl Default for MultiPager {
    fn default() -> Self {
        Self::new()
    }
}

impl Pager for MultiPager {
    fn page(&self, reason: &str, detail: &str) {
        for p in &self.pagers {
            p.page(reason, detail);
        }
    }
}

/// Build the production pager from the environment: always a [`LogPager`]; plus a
/// [`WebhookPager`] when `PAGER_WEBHOOK_URL` is set (format from `PAGER_WEBHOOK_FORMAT`,
/// default Slack). Default-on for the log sink — paging is live the moment the engine starts,
/// no extra script.
pub fn pager_from_env() -> MultiPager {
    let mut m = MultiPager::new().with(Box::new(LogPager));
    if let Ok(url) = std::env::var("PAGER_WEBHOOK_URL") {
        let url = url.trim().to_string();
        if !url.is_empty() {
            let fmt =
                WebhookFormat::from_hint(std::env::var("PAGER_WEBHOOK_FORMAT").ok().as_deref());
            // Rate-limit the webhook to one page per reason per cooldown (default 60s) so a
            // flapping breach can't spam the external sink. The LogPager stays UNlimited — every
            // breach is recorded locally; only the noisy external delivery is throttled.
            let cooldown_secs: u64 = std::env::var("PAGER_WEBHOOK_COOLDOWN_SECS")
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(60);
            let webhook = WebhookPager::new(url, fmt);
            m = m.with(Box::new(RateLimitedPager::new(webhook, cooldown_secs)));
        }
    }
    m
}

/// Breach thresholds. Defaults are conservative production values; each is independently
/// overridable from the environment so an operator can tune without a rebuild.
#[derive(Debug, Clone, Copy)]
pub struct BreachThresholds {
    /// Trip after this many consecutive submit failures (dead path). 0 disables.
    pub max_consecutive_submit_failures: u32,
    /// Trip after this many consecutive realized losses (model wrong). 0 disables.
    pub max_consecutive_losses: u32,
    /// Trip if an observed latency p99 (ns) exceeds this ceiling. 0 disables.
    pub latency_p99_ceiling_ns: u64,
    /// Number of CONSECUTIVE over-ceiling latency windows required to trip (debounce). A single
    /// transient spike (one GC/scheduler stall) must not halt submission; default 2.
    pub latency_trip_consecutive: u32,
    /// Number of consecutive healthy (under-ceiling, non-empty) latency windows required to
    /// AUTO-REARM submission after a latency-caused trip. 0 disables auto-rearm (latched until
    /// restart). Default 3 — only auto-rearms a latency trip, never a submit-failure/loss trip.
    pub latency_recovery_windows: u32,
    /// T3.2 (Tigger `circuit_breaker.rs` rate-based pattern): trip if, over the last
    /// [`LOSS_RATE_WINDOW`](TelemetryBreachMonitor::LOSS_RATE_WINDOW) realized trades, at least this
    /// many were losses. Catches *sustained-but-intermittent* adverse selection (e.g. a 40% revert
    /// rate that never trips the consecutive-loss counter). `0` disables. Default 32 (= 50% of 64).
    pub loss_rate_window_trip_count: u32,
}

impl Default for BreachThresholds {
    fn default() -> Self {
        Self {
            max_consecutive_submit_failures: 5,
            max_consecutive_losses: 8,
            // 50 ms — far above the ~4 ms p99 sim envelope; if we're this slow the FCFS race is
            // already lost and we're just burning gas. Generous so it only fires on real stalls.
            latency_p99_ceiling_ns: 50_000_000,
            latency_trip_consecutive: 2,
            latency_recovery_windows: 3,
            // Half of the last 64 realized trades being losses is an unambiguous "model adverse"
            // signal even if never 8-in-a-row; conservative enough not to fire on normal variance.
            loss_rate_window_trip_count: 32,
        }
    }
}

impl BreachThresholds {
    /// Load thresholds from the environment, falling back to [`Default`]. Keys:
    /// `BREACH_MAX_SUBMIT_FAILURES`, `BREACH_MAX_LOSSES`, `BREACH_LATENCY_P99_MS`,
    /// `BREACH_LATENCY_TRIP_CONSECUTIVE`, `BREACH_RECOVERY_HEALTHY_WINDOWS`.
    pub fn from_env() -> Self {
        let d = Self::default();
        let parse_u32 = |k: &str, dv: u32| {
            std::env::var(k)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(dv)
        };
        let latency_ms = std::env::var("BREACH_LATENCY_P99_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(d.latency_p99_ceiling_ns / 1_000_000);
        Self {
            max_consecutive_submit_failures: parse_u32(
                "BREACH_MAX_SUBMIT_FAILURES",
                d.max_consecutive_submit_failures,
            ),
            max_consecutive_losses: parse_u32("BREACH_MAX_LOSSES", d.max_consecutive_losses),
            latency_p99_ceiling_ns: latency_ms.saturating_mul(1_000_000),
            latency_trip_consecutive: parse_u32(
                "BREACH_LATENCY_TRIP_CONSECUTIVE",
                d.latency_trip_consecutive,
            )
            .max(1),
            latency_recovery_windows: parse_u32(
                "BREACH_RECOVERY_HEALTHY_WINDOWS",
                d.latency_recovery_windows,
            ),
            loss_rate_window_trip_count: parse_u32(
                "BREACH_LOSS_RATE_TRIP",
                d.loss_rate_window_trip_count,
            ),
        }
    }
}

/// Telemetry-breach monitor. Owns the breach counters; trips the global kill + pages on the
/// first crossing. Lock-free counters so submit-thread updates are cheap.
pub struct TelemetryBreachMonitor {
    thresholds: BreachThresholds,
    consecutive_submit_failures: AtomicU32,
    consecutive_losses: AtomicU32,
    /// Consecutive over-ceiling latency windows (debounce counter toward a trip).
    latency_breach_streak: AtomicU32,
    /// Consecutive healthy latency windows since a latency trip (toward auto-rearm).
    latency_healthy_streak: AtomicU32,
    /// True iff the CURRENT global trip was caused by latency (so auto-rearm only re-arms a
    /// latency trip — a dead submit path / loss streak must NOT auto-clear on healthy latency).
    tripped_by_latency: core::sync::atomic::AtomicBool,
    /// T3.2: sliding window of the last 64 realized outcomes as a bitmask (bit = loss). A single
    /// `AtomicU64` + `count_ones()` gives the loss count in O(1), lock-free, matching this file's
    /// atomic style. Newest outcome is the LSB; each record shifts left.
    outcome_window: AtomicU64,
    /// Total realized outcomes recorded (so the rate breaker waits for a full window).
    outcome_count: AtomicU64,
    /// [Codex] Per-reason page latch (bit per breach reason). Each reason pages ONCE per breach
    /// episode even when the global kill was already armed by a DIFFERENT reason, so the operator
    /// sees every active cause (not just the first to trip). Cleared on auto-rearm.
    paged_reasons: AtomicU32,
}

impl TelemetryBreachMonitor {
    /// Size of the realized-outcome sliding window (one `u64`'s worth of bits).
    pub const LOSS_RATE_WINDOW: u32 = 64;

    // [Codex] Per-reason page-latch bits (see `paged_reasons`).
    const PAGE_LOSS_RATE: u32 = 1 << 0;
    const PAGE_SUBMIT_FAILURES: u32 = 1 << 1;
    const PAGE_CONSECUTIVE_LOSSES: u32 = 1 << 2;
    const PAGE_LATENCY: u32 = 1 << 3;

    /// Emit a page for `reason` exactly once per breach episode (idempotent via `paged_reasons`).
    /// Returns whether a page was actually emitted. The kill-trip decision is made SEPARATELY by each
    /// caller (unchanged), so this only governs paging — the safety/halt behavior is preserved. [Codex]
    fn page_once(&self, bit: u32, pager: &dyn Pager, reason: &str, msg: &str) -> bool {
        if self.paged_reasons.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            pager.page(reason, msg);
            true
        } else {
            false
        }
    }

    /// Construct with explicit thresholds.
    pub fn new(thresholds: BreachThresholds) -> Self {
        Self {
            thresholds,
            consecutive_submit_failures: AtomicU32::new(0),
            consecutive_losses: AtomicU32::new(0),
            latency_breach_streak: AtomicU32::new(0),
            latency_healthy_streak: AtomicU32::new(0),
            tripped_by_latency: core::sync::atomic::AtomicBool::new(false),
            outcome_window: AtomicU64::new(0),
            outcome_count: AtomicU64::new(0),
            paged_reasons: AtomicU32::new(0),
        }
    }

    /// Push one realized outcome into the sliding window (newest = LSB). Lock-free; safe under
    /// concurrent recorders via CAS.
    fn push_outcome(&self, loss: bool) {
        let bit = loss as u64;
        let _ = self
            .outcome_window
            .fetch_update(Ordering::Release, Ordering::Relaxed, |w| {
                Some((w << 1) | bit)
            });
        self.outcome_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Check the windowed loss-rate breaker (T3.2). Trips once a FULL window of realized outcomes
    /// has ≥ `loss_rate_window_trip_count` losses — the sustained-adverse-selection guard the
    /// consecutive-loss counter misses. Returns `true` if this crossing tripped the kill switch.
    fn check_loss_rate(&self, kill: &KillSwitches, pager: &dyn Pager, now_ns: u64) -> bool {
        let trip = self.thresholds.loss_rate_window_trip_count;
        if trip == 0 || self.outcome_count.load(Ordering::Relaxed) < Self::LOSS_RATE_WINDOW as u64 {
            return false; // disabled, or not enough history for a full window yet
        }
        let losses = self.outcome_window.load(Ordering::Acquire).count_ones();
        if losses >= trip {
            let first = !kill.global.is_armed();
            if first {
                kill.global.trip(now_ns);
            }
            self.page_once(
                Self::PAGE_LOSS_RATE,
                pager,
                "loss_rate",
                &format!(
                    "{losses}/{} recent realized trades were losses (≥{trip}) — sustained adverse selection; halting",
                    Self::LOSS_RATE_WINDOW
                ),
            );
            return first;
        }
        false
    }

    /// The active thresholds (for logging at startup).
    pub fn thresholds(&self) -> BreachThresholds {
        self.thresholds
    }

    /// Record a successful submit — resets the failure streak.
    pub fn record_submit_success(&self) {
        self.consecutive_submit_failures.store(0, Ordering::Relaxed);
    }

    /// Record a submit failure. Returns `true` if this crossing tripped the kill switch.
    pub fn record_submit_failure(
        &self,
        kill: &KillSwitches,
        pager: &dyn Pager,
        now_ns: u64,
    ) -> bool {
        let n = self
            .consecutive_submit_failures
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let limit = self.thresholds.max_consecutive_submit_failures;
        if limit != 0 && n >= limit {
            let first = !kill.global.is_armed();
            if first {
                kill.global.trip(now_ns);
            }
            self.page_once(
                Self::PAGE_SUBMIT_FAILURES,
                pager,
                "submit_failures",
                &format!("{n} consecutive submit failures — sequencer path likely dead; halting"),
            );
            return first;
        }
        false
    }

    /// Record a realized win — resets the loss streak and pushes a win into the rate window.
    pub fn record_win(&self) {
        self.consecutive_losses.store(0, Ordering::Relaxed);
        self.push_outcome(false);
    }

    /// Record a realized loss. Returns `true` if this crossing tripped the kill switch (via either
    /// the consecutive-loss counter OR the windowed loss-rate breaker).
    pub fn record_loss(&self, kill: &KillSwitches, pager: &dyn Pager, now_ns: u64) -> bool {
        self.push_outcome(true);
        let n = self.consecutive_losses.fetch_add(1, Ordering::Relaxed) + 1;
        let limit = self.thresholds.max_consecutive_losses;
        if limit != 0 && n >= limit {
            let first = !kill.global.is_armed();
            if first {
                kill.global.trip(now_ns);
            }
            self.page_once(
                Self::PAGE_CONSECUTIVE_LOSSES,
                pager,
                "consecutive_losses",
                &format!("{n} consecutive realized losses — profit model adverse; halting"),
            );
            return first;
        }
        // T3.2: the sustained-rate guard the consecutive counter can't see (intermittent losses).
        self.check_loss_rate(kill, pager, now_ns)
    }

    /// Observe a WINDOWED latency p99 sample (ns) over `sample_count` recordings since the last
    /// observation. Returns `true` if this crossing tripped the kill switch.
    ///
    /// Two changes vs a naive "trip on first over-ceiling": (1) DEBOUNCE — requires
    /// `latency_trip_consecutive` consecutive over-ceiling windows to trip, so a single transient
    /// stall can't halt submission; (2) AUTO-REARM — after a latency-caused trip, once
    /// `latency_recovery_windows` consecutive healthy windows are seen AND no submit-failure/loss
    /// breach is active, the global kill is re-armed (closed) so submission resumes without a
    /// process restart. `sample_count == 0` (an idle window with no opportunities) is neutral —
    /// it neither trips nor counts toward recovery.
    pub fn observe_latency_p99(
        &self,
        p99_ns: u64,
        sample_count: u64,
        kill: &KillSwitches,
        pager: &dyn Pager,
        now_ns: u64,
    ) -> bool {
        let ceil = self.thresholds.latency_p99_ceiling_ns;
        if ceil == 0 || sample_count == 0 {
            return false; // disabled, or an empty window — no signal
        }
        if p99_ns > ceil {
            // Unhealthy window: advance the breach streak, reset the recovery streak.
            self.latency_healthy_streak.store(0, Ordering::Relaxed);
            let streak = self.latency_breach_streak.fetch_add(1, Ordering::Relaxed) + 1;
            if streak >= self.thresholds.latency_trip_consecutive {
                let first = !kill.global.is_armed();
                if first {
                    kill.global.trip(now_ns);
                    self.tripped_by_latency.store(true, Ordering::Release);
                }
                self.page_once(
                    Self::PAGE_LATENCY,
                    pager,
                    "latency_p99",
                    &format!(
                        "hot-path p99 {p99_ns}ns over ceiling {ceil}ns for {streak} windows — too slow to win FCFS; halting submission"
                    ),
                );
                return first;
            }
            return false;
        }
        // Healthy window: reset the breach streak; advance recovery toward an auto-rearm.
        self.latency_breach_streak.store(0, Ordering::Relaxed);
        let recovery = self.thresholds.latency_recovery_windows;
        if recovery == 0 {
            return false; // auto-rearm disabled — latched until restart
        }
        if !(kill.global.is_armed() && self.tripped_by_latency.load(Ordering::Acquire)) {
            return false; // not currently halted by latency — nothing to rearm
        }
        // Don't auto-rearm while a genuine submit-failure / loss breach is still active.
        let other_breach = (self.thresholds.max_consecutive_submit_failures != 0
            && self.consecutive_submit_failures.load(Ordering::Relaxed)
                >= self.thresholds.max_consecutive_submit_failures)
            || (self.thresholds.max_consecutive_losses != 0
                && self.consecutive_losses.load(Ordering::Relaxed)
                    >= self.thresholds.max_consecutive_losses);
        if other_breach {
            return false;
        }
        let healthy = self.latency_healthy_streak.fetch_add(1, Ordering::Relaxed) + 1;
        if healthy >= recovery {
            let armed_ns = kill.global.close(now_ns);
            self.tripped_by_latency.store(false, Ordering::Release);
            self.latency_healthy_streak.store(0, Ordering::Relaxed);
            // [Codex] Allow each reason to page again after recovery (a new breach episode).
            self.paged_reasons.store(0, Ordering::Relaxed);
            pager.page(
                "latency_recovered",
                &format!(
                    "hot-path p99 healthy for {healthy} windows (was halted {armed_ns}ns) — submission auto-rearmed"
                ),
            );
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test pager that records the last page for assertions.
    #[derive(Default)]
    struct SpyPager {
        pages: Mutex<Vec<(String, String)>>,
    }
    impl Pager for SpyPager {
        fn page(&self, reason: &str, detail: &str) {
            self.pages
                .lock()
                .unwrap()
                .push((reason.to_string(), detail.to_string()));
        }
    }

    fn monitor() -> (TelemetryBreachMonitor, KillSwitches, SpyPager) {
        (
            TelemetryBreachMonitor::new(BreachThresholds {
                max_consecutive_submit_failures: 3,
                max_consecutive_losses: 2,
                latency_p99_ceiling_ns: 10_000_000,
                latency_trip_consecutive: 2,
                latency_recovery_windows: 3,
                loss_rate_window_trip_count: 0, // isolated from existing tests; exercised separately
            }),
            KillSwitches::new(),
            SpyPager::default(),
        )
    }

    #[test]
    fn submit_failures_trip_at_threshold_and_page_once() {
        let (m, k, p) = monitor();
        assert!(!m.record_submit_failure(&k, &p, 1)); // 1
        assert!(!m.record_submit_failure(&k, &p, 2)); // 2
        assert!(m.record_submit_failure(&k, &p, 3)); // 3 → trip
        assert!(k.global.is_armed());
        // Further failures don't double-page (already armed).
        assert!(!m.record_submit_failure(&k, &p, 4));
        let pages = p.pages.lock().unwrap();
        assert_eq!(pages.len(), 1, "must page exactly once on first breach");
        assert_eq!(pages[0].0, "submit_failures");
    }

    #[test]
    fn submit_success_resets_failure_streak() {
        let (m, k, p) = monitor();
        m.record_submit_failure(&k, &p, 1);
        m.record_submit_failure(&k, &p, 2);
        m.record_submit_success(); // reset
        assert!(!m.record_submit_failure(&k, &p, 3)); // count is 1 again, not 3
        assert!(!k.global.is_armed());
    }

    #[test]
    fn consecutive_losses_trip_and_win_resets() {
        let (m, k, p) = monitor();
        assert!(!m.record_loss(&k, &p, 1)); // 1
        m.record_win(); // reset
        assert!(!m.record_loss(&k, &p, 2)); // 1 again
        assert!(m.record_loss(&k, &p, 3)); // 2 → trip
        assert!(k.global.is_armed());
        assert_eq!(p.pages.lock().unwrap()[0].0, "consecutive_losses");
    }

    #[test]
    fn second_reason_pages_even_after_global_armed() {
        // [Codex] Once one reason arms the global kill, a DIFFERENT reason that subsequently breaches
        // must STILL page — the operator needs to see every active cause, not just the first to trip.
        let (m, k, p) = monitor(); // submit threshold 3, loss threshold 2
                                   // Arm the global via the submit-failure reason.
        m.record_submit_failure(&k, &p, 1);
        m.record_submit_failure(&k, &p, 2);
        assert!(m.record_submit_failure(&k, &p, 3)); // trip
        assert!(k.global.is_armed());
        // Now a consecutive-loss breach occurs while the global is ALREADY armed.
        m.record_loss(&k, &p, 4); // loss 1
        m.record_loss(&k, &p, 5); // loss 2 → breach (global already armed by submit_failures)
        let pages = p.pages.lock().unwrap();
        let reasons: Vec<&str> = pages.iter().map(|(r, _)| r.as_str()).collect();
        assert!(reasons.contains(&"submit_failures"), "first reason paged");
        assert!(
            reasons.contains(&"consecutive_losses"),
            "second reason must ALSO page once after the global is armed, got {reasons:?}"
        );
        // But the SAME reason must not double-page.
        let loss_pages = reasons
            .iter()
            .filter(|r| **r == "consecutive_losses")
            .count();
        assert_eq!(loss_pages, 1, "each reason pages at most once per episode");
    }

    #[test]
    fn latency_first_does_not_suppress_later_submit_or_loss_pages() {
        let m = TelemetryBreachMonitor::new(BreachThresholds {
            max_consecutive_submit_failures: 2,
            max_consecutive_losses: 2,
            latency_p99_ceiling_ns: 10_000_000,
            latency_trip_consecutive: 1,
            latency_recovery_windows: 2,
            loss_rate_window_trip_count: 0,
        });
        let k = KillSwitches::new();
        let p = SpyPager::default();

        assert!(m.observe_latency_p99(20_000_000, 100, &k, &p, 1));
        assert!(k.global.is_armed(), "latency should arm global kill");

        assert!(!m.record_submit_failure(&k, &p, 2));
        assert!(!m.record_submit_failure(&k, &p, 3));
        assert!(!m.record_loss(&k, &p, 4));
        assert!(!m.record_loss(&k, &p, 5));

        let pages = p.pages.lock().unwrap();
        let reasons: Vec<&str> = pages.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(
            reasons,
            vec!["latency_p99", "submit_failures", "consecutive_losses"],
            "later breach reasons must still page after latency armed global kill"
        );
    }

    #[test]
    fn latency_breach_debounces_then_trips() {
        let (m, k, p) = monitor(); // default latency_trip_consecutive = 2
        assert!(!m.observe_latency_p99(5_000_000, 100, &k, &p, 1)); // under ceiling
        assert!(!m.observe_latency_p99(20_000_000, 100, &k, &p, 2)); // 1st over → debounced, no trip
        assert!(m.observe_latency_p99(20_000_000, 100, &k, &p, 3)); // 2nd consecutive over → trip
        assert!(k.global.is_armed());
        assert_eq!(p.pages.lock().unwrap()[0].0, "latency_p99");
    }

    #[test]
    fn empty_window_is_neutral_and_a_healthy_window_resets_debounce() {
        let (m, k, p) = monitor();
        assert!(!m.observe_latency_p99(20_000_000, 0, &k, &p, 1)); // empty window → ignored
        assert!(!m.observe_latency_p99(20_000_000, 100, &k, &p, 2)); // 1st over
        assert!(!m.observe_latency_p99(1_000_000, 100, &k, &p, 3)); // healthy → resets streak
        assert!(!m.observe_latency_p99(20_000_000, 100, &k, &p, 4)); // 1st over again (not 2nd)
        assert!(!k.global.is_armed());
    }

    #[test]
    fn latency_trip_auto_rearms_after_healthy_windows() {
        // Custom thresholds: trip on 1 over-ceiling window, rearm after 2 healthy windows.
        let m = TelemetryBreachMonitor::new(BreachThresholds {
            max_consecutive_submit_failures: 3,
            max_consecutive_losses: 2,
            latency_p99_ceiling_ns: 10_000_000,
            latency_trip_consecutive: 1,
            latency_recovery_windows: 2,
            loss_rate_window_trip_count: 0,
        });
        let k = KillSwitches::new();
        let p = SpyPager::default();
        assert!(m.observe_latency_p99(20_000_000, 100, &k, &p, 1)); // trip
        assert!(k.global.is_armed());
        assert!(!m.observe_latency_p99(1_000_000, 100, &k, &p, 2)); // 1 healthy — not yet
        assert!(k.global.is_armed());
        assert!(!m.observe_latency_p99(1_000_000, 100, &k, &p, 3)); // 2 healthy — auto-rearm
        assert!(
            !k.global.is_armed(),
            "submission auto-rearmed after sustained healthy latency"
        );
        let pages = p.pages.lock().unwrap();
        assert_eq!(pages[0].0, "latency_p99");
        assert_eq!(pages[1].0, "latency_recovered");
    }

    #[test]
    fn auto_rearm_does_not_clear_a_submit_failure_trip() {
        // A submit-failure trip must NOT be auto-cleared by healthy latency (different failure mode).
        let m = TelemetryBreachMonitor::new(BreachThresholds {
            max_consecutive_submit_failures: 2,
            max_consecutive_losses: 8,
            latency_p99_ceiling_ns: 10_000_000,
            latency_trip_consecutive: 1,
            latency_recovery_windows: 1,
            loss_rate_window_trip_count: 0,
        });
        let k = KillSwitches::new();
        let p = SpyPager::default();
        m.record_submit_failure(&k, &p, 1);
        assert!(m.record_submit_failure(&k, &p, 2)); // trip via submit failures
        assert!(k.global.is_armed());
        // Healthy latency windows must not rearm (tripped_by_latency is false).
        assert!(!m.observe_latency_p99(1_000_000, 100, &k, &p, 3));
        assert!(!m.observe_latency_p99(1_000_000, 100, &k, &p, 4));
        assert!(
            k.global.is_armed(),
            "submit-failure trip stays latched (not a latency trip)"
        );
    }

    #[test]
    fn zero_threshold_disables_a_condition() {
        let m = TelemetryBreachMonitor::new(BreachThresholds {
            max_consecutive_submit_failures: 0, // disabled
            max_consecutive_losses: 2,
            latency_p99_ceiling_ns: 0, // disabled
            latency_trip_consecutive: 2,
            latency_recovery_windows: 3,
            loss_rate_window_trip_count: 0,
        });
        let k = KillSwitches::new();
        let p = SpyPager::default();
        for i in 0..100 {
            assert!(
                !m.record_submit_failure(&k, &p, i),
                "disabled condition must never trip"
            );
        }
        assert!(
            !m.observe_latency_p99(u64::MAX, 100, &k, &p, 1),
            "disabled latency must never trip"
        );
        assert!(!k.global.is_armed());
    }

    /// Monitor with ONLY the loss-rate breaker (and an optional consecutive-loss limit) armed.
    fn rate_monitor(consec: u32, trip: u32) -> (TelemetryBreachMonitor, KillSwitches, SpyPager) {
        (
            TelemetryBreachMonitor::new(BreachThresholds {
                max_consecutive_submit_failures: 0,
                max_consecutive_losses: consec,
                latency_p99_ceiling_ns: 0,
                latency_trip_consecutive: 1,
                latency_recovery_windows: 1,
                loss_rate_window_trip_count: trip,
            }),
            KillSwitches::new(),
            SpyPager::default(),
        )
    }

    #[test]
    fn loss_rate_trips_on_intermittent_losses_consecutive_misses() {
        // consec=5 would catch 5-in-a-row, but a perfectly ALTERNATING win/loss stream never has
        // 2 in a row — so only the windowed 32/64 rate breaker can catch this sustained 50% bleed.
        let (m, k, p) = rate_monitor(5, 32);
        let mut tripped = false;
        for i in 0..70u64 {
            if i % 2 == 0 {
                tripped |= m.record_loss(&k, &p, i);
            } else {
                m.record_win();
            }
        }
        assert!(
            tripped,
            "sustained 50% loss rate must trip the windowed breaker"
        );
        assert!(k.global.is_armed());
        let pages = p.pages.lock().unwrap();
        assert_eq!(pages.len(), 1, "page exactly once");
        assert_eq!(
            pages[0].0, "loss_rate",
            "rate breaker (not consecutive) caught it"
        );
    }

    #[test]
    fn loss_rate_waits_for_a_full_window() {
        // trip=1 would fire on any loss ONCE the window is full; 63 losses is one short of full.
        let (m, k, p) = rate_monitor(0, 1);
        for i in 0..63u64 {
            assert!(
                !m.record_loss(&k, &p, i),
                "must not trip before a full 64-sample window"
            );
        }
        assert!(!k.global.is_armed());
        assert!(
            m.record_loss(&k, &p, 63),
            "the 64th outcome completes the window → trip"
        );
        assert!(k.global.is_armed());
    }

    #[test]
    fn loss_rate_below_threshold_does_not_trip() {
        // A full window with only 20/64 losses is below the 32 floor → healthy, no trip.
        let (m, k, p) = rate_monitor(0, 32);
        for _ in 0..44 {
            m.record_win();
        }
        let mut tripped = false;
        for i in 0..20u64 {
            tripped |= m.record_loss(&k, &p, i);
        }
        assert!(!tripped, "20/64 losses is under the floor");
        assert!(!k.global.is_armed());
    }

    #[test]
    fn loss_rate_zero_disables_the_breaker() {
        let (m, k, p) = rate_monitor(0, 0); // both consecutive and rate disabled
        let mut tripped = false;
        for i in 0..64u64 {
            tripped |= m.record_loss(&k, &p, i);
        }
        assert!(
            !tripped,
            "disabled rate breaker must never trip even on all-losses"
        );
        assert!(!k.global.is_armed());
    }

    #[test]
    fn slack_payload_is_valid_json_shape() {
        let body = build_webhook_payload(WebhookFormat::Slack, "submit_failures", "5 in a row");
        assert!(body.starts_with("{\"text\":\""));
        assert!(body.ends_with("\"}"));
        assert!(body.contains("submit_failures"));
        assert!(body.contains("5 in a row"));
    }

    #[test]
    fn generic_payload_carries_reason_and_detail() {
        let body = build_webhook_payload(WebhookFormat::Generic, "latency_p99", "too slow");
        assert!(body.contains("\"source\":\"mantis\""));
        assert!(body.contains("\"reason\":\"latency_p99\""));
        assert!(body.contains("\"detail\":\"too slow\""));
    }

    #[test]
    fn payload_escapes_quotes_and_newlines() {
        // A detail with a quote + newline must not break the JSON.
        let body = build_webhook_payload(WebhookFormat::Generic, "x", "a\"b\nc");
        assert!(
            body.contains("a\\\"b\\nc"),
            "quotes/newlines must be escaped: {body}"
        );
        // No raw newline in the output (would break a line-delimited webhook consumer).
        assert!(!body.contains('\n'));
    }

    #[test]
    fn webhook_format_from_hint() {
        assert_eq!(
            WebhookFormat::from_hint(Some("generic")),
            WebhookFormat::Generic
        );
        assert_eq!(
            WebhookFormat::from_hint(Some("json")),
            WebhookFormat::Generic
        );
        assert_eq!(
            WebhookFormat::from_hint(Some("slack")),
            WebhookFormat::Slack
        );
        assert_eq!(WebhookFormat::from_hint(None), WebhookFormat::Slack); // default
    }

    #[test]
    fn multipager_fans_out_to_all_children() {
        let a = std::sync::Arc::new(SpyPager::default());
        let b = std::sync::Arc::new(SpyPager::default());
        struct ArcPager(std::sync::Arc<SpyPager>);
        impl Pager for ArcPager {
            fn page(&self, r: &str, d: &str) {
                self.0.page(r, d);
            }
        }
        let m = MultiPager::new()
            .with(Box::new(ArcPager(a.clone())))
            .with(Box::new(ArcPager(b.clone())));
        assert_eq!(m.len(), 2);
        m.page("reason", "detail");
        assert_eq!(a.pages.lock().unwrap().len(), 1);
        assert_eq!(b.pages.lock().unwrap().len(), 1);
        assert_eq!(
            a.pages.lock().unwrap()[0],
            ("reason".to_string(), "detail".to_string())
        );
    }

    // Fixed-clock helpers for the rate-limiter tests (no sleeps).
    fn clock_zero() -> u64 {
        0
    }
    fn clock_30s() -> u64 {
        30_000_000_000
    }
    fn clock_90s() -> u64 {
        90_000_000_000
    }

    #[test]
    fn rate_limiter_suppresses_same_reason_within_cooldown() {
        let spy = std::sync::Arc::new(SpyPager::default());
        struct ArcPager(std::sync::Arc<SpyPager>);
        impl Pager for ArcPager {
            fn page(&self, r: &str, d: &str) {
                self.0.page(r, d);
            }
        }
        // 60s cooldown, clock fixed at t=0.
        let rl = RateLimitedPager::with_clock(ArcPager(spy.clone()), 60_000_000_000, clock_zero);
        assert!(rl.try_deliver("submit_failures", "x", 0)); // first → delivered
        assert!(!rl.try_deliver("submit_failures", "x", 30_000_000_000)); // +30s → suppressed
        assert!(rl.try_deliver("submit_failures", "x", 90_000_000_000)); // +90s → past cooldown
        assert_eq!(
            spy.pages.lock().unwrap().len(),
            2,
            "2 delivered, 1 suppressed"
        );
    }

    #[test]
    fn rate_limiter_never_suppresses_distinct_reasons() {
        let spy = std::sync::Arc::new(SpyPager::default());
        struct ArcPager(std::sync::Arc<SpyPager>);
        impl Pager for ArcPager {
            fn page(&self, r: &str, d: &str) {
                self.0.page(r, d);
            }
        }
        let rl = RateLimitedPager::with_clock(ArcPager(spy.clone()), 60_000_000_000, clock_zero);
        // All at t=0 but different reasons → all delivered (a new failure mode must page now).
        assert!(rl.try_deliver("submit_failures", "a", 0));
        assert!(rl.try_deliver("consecutive_losses", "b", 0));
        assert!(rl.try_deliver("latency_p99", "c", 0));
        assert_eq!(spy.pages.lock().unwrap().len(), 3);
    }

    #[test]
    fn rate_limiter_page_uses_injected_clock() {
        // Exercising the public `page()` path with a non-zero clock: two pages at the same
        // injected instant collapse to one for the same reason.
        let spy = std::sync::Arc::new(SpyPager::default());
        struct ArcPager(std::sync::Arc<SpyPager>);
        impl Pager for ArcPager {
            fn page(&self, r: &str, d: &str) {
                self.0.page(r, d);
            }
        }
        let rl = RateLimitedPager::with_clock(ArcPager(spy.clone()), 60_000_000_000, clock_30s);
        rl.page("r", "d");
        rl.page("r", "d"); // same clock (30s) → within cooldown of the first → suppressed
        assert_eq!(spy.pages.lock().unwrap().len(), 1);
        let _ = clock_90s; // referenced to keep the helper meaningful
    }

    #[test]
    fn pager_from_env_always_has_log_sink() {
        // No webhook URL → just the LogPager (len 1).
        std::env::remove_var("PAGER_WEBHOOK_URL");
        assert_eq!(pager_from_env().len(), 1);
        // With a webhook URL → LogPager + WebhookPager (len 2).
        std::env::set_var("PAGER_WEBHOOK_URL", "https://hooks.example.com/x");
        assert_eq!(pager_from_env().len(), 2);
        std::env::remove_var("PAGER_WEBHOOK_URL");
    }

    #[test]
    fn thresholds_from_env_defaults_are_sane() {
        // Without env overrides, defaults must be the documented production values.
        let t = BreachThresholds::default();
        assert_eq!(t.max_consecutive_submit_failures, 5);
        assert_eq!(t.max_consecutive_losses, 8);
        assert_eq!(t.latency_p99_ceiling_ns, 50_000_000);
    }
}
