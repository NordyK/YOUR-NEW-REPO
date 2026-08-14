//! Operational diagnostics — stable error codes, explanations, remedies.
//!
//! ───────────────────────────────────────────────────────────────────────────
//! WHY THIS EXISTS: on the AWS box the operator needs to know *exactly what is
//! blocking trading right now* without grepping 160 free-text warn! sites. Every
//! operationally meaningful failure gets a STABLE code (grep `code=` in the log,
//! or `target=diag`), a one-line explanation, and the concrete remedy.
//!
//! COST DISCIPLINE (the reason this is safe to ship on the hot box):
//!   * `report()` is only ever called on ERROR PATHS and STATE TRANSITIONS —
//!     never per-frame, never per-candidate-success. Errors are rare by
//!     construction; a flapping error is bounded by the per-code rate limiter.
//!   * Bookkeeping is three relaxed atomics per call (count, last-seen, and a
//!     CAS on last-logged only when the cooldown has expired). No locks, no
//!     allocation unless a line is actually emitted.
//!   * The aggregated view (`blockers_line()`) is built on the HEARTBEAT thread
//!     (5 s cadence, housekeeping core) — zero hot-path involvement.
//!
//! Severity model:
//!   * `Blocking`  — trading is halted or submits are being refused right now.
//!   * `Degraded`  — capture/freshness is reduced; the engine still trades.
//!   * `Info`      — state transition worth a classified line (e.g. kill cleared).
//!
//! The full code → meaning → fix table is rendered in `engine/LOGGING.md` from
//! the same strings below (single source of truth: this file).
//! ───────────────────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// How a condition affects trading (drives log level + the blockers line).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Impact {
    /// Trading halted / submits refused while this condition holds.
    Blocking,
    /// Still trading, but capture, freshness, or safety margin is reduced.
    Degraded,
    /// Classified state transition; no action needed.
    Info,
}

macro_rules! diag_codes {
    ($( $variant:ident = ($name:literal, $impact:ident, $cooldown_s:literal, $explain:literal, $remedy:literal) ),+ $(,)?) => {
        /// Every classified operational condition. `#[repr(u8)]` — the discriminant
        /// indexes the counter tables; append new codes at the end (names are the
        /// stable identifier, not the discriminant).
        #[repr(u8)]
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Diag { $( #[doc = $explain] $variant ),+ }

        /// All codes, for iteration (heartbeat snapshot, docs, tests).
        pub const ALL: &[Diag] = &[ $( Diag::$variant ),+ ];

        impl Diag {
            /// Stable grep-able identifier (`code=` field in every diag line).
            pub const fn name(self) -> &'static str {
                match self { $( Diag::$variant => $name ),+ }
            }
            /// How this condition affects trading.
            pub const fn impact(self) -> Impact {
                match self { $( Diag::$variant => Impact::$impact ),+ }
            }
            /// Per-code log cooldown (seconds): a flapping condition emits at most
            /// one line per cooldown (counters still count every occurrence).
            pub const fn cooldown_secs(self) -> u64 {
                match self { $( Diag::$variant => $cooldown_s ),+ }
            }
            /// One-line explanation of what the condition MEANS.
            pub const fn explain(self) -> &'static str {
                match self { $( Diag::$variant => $explain ),+ }
            }
            /// The concrete operator remedy.
            pub const fn remedy(self) -> &'static str {
                match self { $( Diag::$variant => $remedy ),+ }
            }
        }
    };
}

diag_codes! {
    // ── feed (the engine's eyes — without it nothing trades) ────────────────
    FeedDown = ("FEED-DOWN", Blocking, 10,
        "relay/feed connection lost or reconnecting — no new sequencer frames are arriving",
        "check the relay process (deploy/run-relay.sh) + upstream wss://arb1-feed endpoint; dual-relay via RELAY_ENDPOINT_2 rides through single-relay stalls"),
    FeedGap = ("FEED-GAP", Degraded, 30,
        "sequencer sequence-number gap — one or more frames were missed; pool state may be briefly stale until resync",
        "occasional gaps are normal on reconnect; a climbing rate means relay overload or network loss — check relay host CPU + nic-tuning.sh"),
    FeedNoStart = ("FEED-NOSTART", Blocking, 10,
        "no frames arrived within the startup grace window — the feed never started",
        "verify RELAY_ENDPOINT (ws://127.0.0.1:9642/feed or binunix socket), the relay is running, and the upstream feed is reachable; engine exits for supervisor restart"),
    FeedStall = ("FEED-STALL", Blocking, 10,
        "frames stopped advancing past the watchdog threshold — pipeline hung or feed dead",
        "engine exits non-zero for supervisor restart (systemd Restart=always); if recurring, check relay + ENGINE_STALL context in the log"),

    // ── gas oracle (EV math is wrong without it; live submits are refused) ──
    GasStale = ("GAS-STALE", Blocking, 10,
        "gas snapshot older than GAS_MAX_AGE_MS — L1 data fee would be mispriced, so live submits are refused (dry-run: EV uses the stale snapshot)",
        "check the gas refresher + ARBITRUM_RPC_URL health; see GAS-REFRESH-FAIL lines for the underlying RPC error"),
    GasRefreshFail = ("GAS-REFRESH-FAIL", Degraded, 30,
        "background gas/L1-fee refresh RPC call failed — oracle keeps the last snapshot (ages toward GAS-STALE)",
        "check ARBITRUM_RPC_URL reachability/rate limits; transient failures self-heal on the next 250 ms cycle"),

    // ── REVM validator (the phantom-rejection gate) ──────────────────────────
    RevmCold = ("REVM-COLD", Degraded, 30,
        "the REVM validator paid a synchronous RPC round-trip on the validation path (cold fork-miss) — adds ~30-80 ms to that candidate",
        "expected occasionally after repin; a climbing rate means prewarm coverage is short — check PREWARM_TOKEN_BALANCES, REVM_REPIN_SECS, and the a1_prewarm gate"),
    RevmSuspect = ("REVM-SUSPECT", Degraded, 60,
        "REVM fork flagged suspect — trigger replays kept reverting, so its pinned state has likely drifted; an early repin is forced",
        "self-healing (forced repin); if recurring, lower REVM_REPIN_SECS or check RPC consistency"),

    // ── submit path (live mode only) ─────────────────────────────────────────
    SubmitFail = ("SUBMIT-FAIL", Blocking, 10,
        "the signed tx could not be delivered to the sequencer (wire/TLS error) — candidate dropped, nonce reclaimed",
        "check sequencer endpoint + network; the hot-spare connection covers single drops; a streak trips the kill switch (KILL-ARMED)"),
    SubmitReject = ("SUBMIT-REJECT", Degraded, 10,
        "sequencer ACCEPTED the HTTP call but REJECTED the tx (reverted/underpriced/nonce) — counts toward the loss streak",
        "inspect the SUBMIT_ACK preview in the log; recurring rejects usually mean stale state (check freshness predictors) or gas underpricing"),

    // ── kill switches / breach monitor (the safety stack) ───────────────────
    KillArmed = ("KILL-ARMED", Blocking, 5,
        "a kill switch tripped (latency breach, submit-failure streak, or loss streak) — submission is HALTED",
        "the breach monitor auto-rearms a latency trip after sustained-healthy windows; failure/loss trips need operator review (check `target=breach` lines + the pager) then a restart"),
    KillCleared = ("KILL-CLEAR", Info, 5,
        "kill switch cleared — submission re-armed",
        "no action; verify the original cause was understood"),

    // ── boot-time refusals (the engine fails closed) ─────────────────────────
    BootNoPools = ("BOOT-NO-POOLS", Blocking, 60,
        "startup discovery produced zero registered pools — the engine refuses to boot a silent no-op",
        "check ARBITRUM_RPC_URL (reachable? archive?), discovery venue config in engine.json, and the token allowlist"),
    BootRevmOff = ("BOOT-REVM-OFF", Blocking, 60,
        "live_submit=true with REVM_VALIDATE on, but the validator is unavailable — refusing to broadcast without the phantom-rejection gate",
        "fix ARBITRUM_RPC_URL / MANTIS_ARTIFACT, or explicitly accept gross-only with REVM_VALIDATE=0 (NEVER for live capital)"),
    BootThresholds = ("BOOT-THRESHOLDS", Blocking, 60,
        "live broadcast requested with a loosened safety threshold (gas-spike cushion <1.5x or confidence <0.95)",
        "restore hardened values in .env (GAS_SPIKE_RATIO_Q8>=384, CONFIDENCE_FLOOR>=0.95) or override knowingly with ALLOW_LOOSE_THRESHOLDS=1"),
    WarmupTimeout = ("WARMUP-TIMEOUT", Blocking, 60,
        "REVM prewarm / fresh-gas warmup timed out with live broadcast enabled — refusing to ingest+submit cold",
        "engine halts for supervisor restart; check RPC health and the warmup budget envs"),
}

/// Number of codes (sizes the counter tables).
const N: usize = ALL.len();

// Per-code bookkeeping. Index = discriminant. All relaxed: these are statistics,
// and the rate-limit CAS below is the only ordering-sensitive op (AcqRel there).
static COUNTS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static LAST_SEEN_MS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
static LAST_LOGGED_MS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

/// A blocking code is shown in the heartbeat blockers line while it was seen
/// within this window (a stale blocker ages out rather than alarming forever).
const ACTIVE_WINDOW_MS: u64 = 120_000;

#[inline]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Report an occurrence of `code`. `detail` is the site-specific context (the
/// underlying error, ids, sizes); the stable taxonomy fields (code / impact /
/// explain / fix) are attached automatically. Rate-limited per code: the first
/// occurrence always logs; afterwards at most one line per `cooldown_secs()`
/// (every occurrence still counts toward the heartbeat counters).
pub fn report(code: Diag, detail: &str) {
    let i = code as usize;
    let n = COUNTS[i].fetch_add(1, Ordering::Relaxed) + 1;
    let now = now_ms();
    LAST_SEEN_MS[i].store(now, Ordering::Relaxed);

    // Rate-limit election: one racer wins the CAS and emits; everyone else only counted.
    let last = LAST_LOGGED_MS[i].load(Ordering::Relaxed);
    let due = last == 0 || now.saturating_sub(last) >= code.cooldown_secs() * 1000;
    if !due {
        return;
    }
    if LAST_LOGGED_MS[i]
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return; // another thread just logged this code
    }
    match code.impact() {
        Impact::Blocking => tracing::error!(
            target: "diag",
            code = code.name(),
            n,
            blocks_trading = true,
            explain = code.explain(),
            fix = code.remedy(),
            detail,
            "DIAG"
        ),
        Impact::Degraded => tracing::warn!(
            target: "diag",
            code = code.name(),
            n,
            blocks_trading = false,
            explain = code.explain(),
            fix = code.remedy(),
            detail,
            "DIAG"
        ),
        Impact::Info => tracing::info!(
            target: "diag",
            code = code.name(),
            n,
            explain = code.explain(),
            detail,
            "DIAG"
        ),
    }
}

/// Total occurrences of `code` since boot.
pub fn count(code: Diag) -> u64 {
    COUNTS[code as usize].load(Ordering::Relaxed)
}

/// The heartbeat "what blocks us" line: every `Blocking` code seen within the
/// active window, as `CODE×count(age s)`. `None` when nothing blocks — the
/// heartbeat stays quiet (no allocation) on the healthy path.
pub fn blockers_line() -> Option<String> {
    let now = now_ms();
    let mut out = String::new();
    for &code in ALL {
        if code.impact() != Impact::Blocking {
            continue;
        }
        let i = code as usize;
        let seen = LAST_SEEN_MS[i].load(Ordering::Relaxed);
        if seen == 0 || now.saturating_sub(seen) > ACTIVE_WINDOW_MS {
            continue;
        }
        let n = COUNTS[i].load(Ordering::Relaxed);
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!(
            "{}×{}({}s ago)",
            code.name(),
            n,
            now.saturating_sub(seen) / 1000
        ));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Snapshot every code with a non-zero count: `(name, impact, count, secs_since_last)`.
/// Heartbeat/30 s summary + tests.
pub fn snapshot() -> Vec<(&'static str, Impact, u64, u64)> {
    let now = now_ms();
    ALL.iter()
        .filter_map(|&c| {
            let n = COUNTS[c as usize].load(Ordering::Relaxed);
            if n == 0 {
                return None;
            }
            let seen = LAST_SEEN_MS[c as usize].load(Ordering::Relaxed);
            Some((c.name(), c.impact(), n, now.saturating_sub(seen) / 1000))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_nonempty() {
        let mut seen = std::collections::HashSet::new();
        for &c in ALL {
            assert!(!c.name().is_empty());
            assert!(!c.explain().is_empty(), "{} missing explain", c.name());
            assert!(!c.remedy().is_empty(), "{} missing remedy", c.name());
            assert!(seen.insert(c.name()), "duplicate diag name {}", c.name());
            assert!(c.cooldown_secs() > 0);
        }
    }

    #[test]
    fn report_counts_every_occurrence() {
        let before = count(Diag::FeedGap);
        report(Diag::FeedGap, "test gap 1");
        report(Diag::FeedGap, "test gap 2");
        report(Diag::FeedGap, "test gap 3");
        assert_eq!(count(Diag::FeedGap), before + 3);
    }

    #[test]
    fn blocking_code_appears_in_blockers_line_with_count_and_age() {
        report(Diag::GasStale, "test stale");
        let line = blockers_line().expect("blocking code just reported");
        assert!(line.contains("GAS-STALE×"), "line: {line}");
        assert!(line.contains("s ago)"), "line: {line}");
    }

    #[test]
    fn degraded_codes_never_enter_blockers_line() {
        report(Diag::RevmCold, "test cold");
        if let Some(line) = blockers_line() {
            assert!(!line.contains("REVM-COLD"), "line: {line}");
        }
    }

    #[test]
    fn snapshot_lists_reported_codes() {
        report(Diag::SubmitReject, "test reject");
        let snap = snapshot();
        assert!(snap.iter().any(|(n, imp, c, _)| {
            *n == "SUBMIT-REJECT" && *imp == Impact::Degraded && *c >= 1
        }));
    }

    #[test]
    fn rate_limiter_emits_first_then_suppresses_within_cooldown() {
        // Counting is unconditional; the LAST_LOGGED_MS CAS gates emission. After one
        // report, last_logged is set — an immediate second report must not win the CAS.
        report(Diag::KillCleared, "first");
        let i = Diag::KillCleared as usize;
        let logged_after_first = LAST_LOGGED_MS[i].load(Ordering::Relaxed);
        assert!(logged_after_first > 0, "first report must log");
        report(Diag::KillCleared, "second (suppressed)");
        assert_eq!(
            LAST_LOGGED_MS[i].load(Ordering::Relaxed),
            logged_after_first,
            "second report within cooldown must not re-log"
        );
        assert!(count(Diag::KillCleared) >= 2, "but it must still count");
    }
}
