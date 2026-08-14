//! Console dashboard for the Mantis engine — two-tier, zero-hot-path-cost design.
//!
//! ───────────────────────────────────────────────────────────────────────────
//! DESIGN GOAL: the dashboard must NOT cost the hot path any latency.
//!
//!   TIER 1  always-on aggregates   the hot path already maintains these (counter
//!           (counters + latency      atomics + `LatencyHistograms` + the gas oracle).
//!            histograms + gas)       The HUD READS them on the render thread only.
//!                                     No new hot-path work whatsoever.
//!
//!   TIER 2  per-event trace         the hot path push()es a 40-byte Copy `Event`
//!           (the scrolling tail)     into a bounded lock-free `ArrayQueue`. Push is
//!                                     non-blocking; on overflow we DROP + bump a
//!                                     counter (shown as `drop-ev`). Gated by an
//!                                     `AtomicBool` → ~1 ns and out when off.
//!
//!   BOUNDARY  a dedicated `engine-dashboard` thread (PINNED to housekeeping core 0;
//!             the hot threads own isolated cores 1-7, so the HUD can never time-share
//!             a hot core) drains the ring, owns ALL the expensive work — string
//!             formatting, float math, ANSI rendering, terminal I/O — and repaints at
//!             a fixed cadence (default 12 Hz, drift-free absolute deadlines) using
//!             the ALTERNATE SCREEN BUFFER + SYNCHRONIZED OUTPUT so the frame is
//!             flicker-free and the user's scrollback is preserved. Every line is
//!             clipped to the live terminal width (re-read each frame) so wrap can
//!             never shear the in-place repaint; a chained panic hook restores the
//!             screen if the process dies. Output matrix: tty+colour → full HUD;
//!             tty+NO_COLOR → same HUD, monochrome; piped → 1 Hz plain-text frames.
//!
//!   TRUTH     the safety pill reads the pipeline's REAL `KillSwitches` (registered
//!             via `set_kill`) — the same atomics the submit thread bails on — never
//!             a re-derivation that could disagree with the engine.
//!
//! Off by default (`DASHBOARD=1` to enable): no thread is spawned and `emit()` is a
//! ~1 ns no-op. The render thread is the only thing that ever writes to the terminal.
//! ───────────────────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_queue::ArrayQueue;
use engine_gas::GasOracle;
use engine_metrics::{KillSwitches, LatencyHistograms, StageLatency};

// ── palette (ANSI truecolor, matches the HTML mock; honors NO_COLOR) ─────────
const ICE: &str = "\x1b[38;2;212;224;234m";
const DIM: &str = "\x1b[38;2;120;140;160m";
const FNT: &str = "\x1b[38;2;90;106;120m";
const ACCENT: &str = "\x1b[38;2;234;242;248m";
const MINT: &str = "\x1b[38;2;47;227;180m";
const STEEL: &str = "\x1b[38;2;90;169;207m";
const AMBER: &str = "\x1b[38;2;227;164;74m";
const RED: &str = "\x1b[38;2;255;85;102m";
const BOLD: &str = "\x1b[1m";
const RST: &str = "\x1b[0m";

// ── terminal control ────────────────────────────────────────────────────────
const ALT_ENTER: &str = "\x1b[?1049h\x1b[?25l"; // alt screen + hide cursor
const ALT_LEAVE: &str = "\x1b[?25h\x1b[?1049l"; // show cursor + leave alt screen
const SYNC_BEGIN: &str = "\x1b[?2026h"; // begin synchronized update (ignored if unsupported)
const SYNC_END: &str = "\x1b[?2026l";

/// Bounded scrolling region (notable events) + repaint cadence.
const TAIL: usize = 16;
const HZ_DEFAULT: u64 = 12;

/// Sparkline trend history: one sample/sec, drawn `SPARK_W` chars wide (≈ last `SPARK_W` seconds).
/// Render-thread state only — never read or written by the hot path.
const SPARK_HIST: usize = 48;
const SPARK_W: usize = 14;
/// Unicode block ramp for sparklines (same family as the boot progress bar's `█`/`░`).
const SPARK_BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn color() -> bool {
    std::env::var("NO_COLOR").is_err()
}

// ════════════════════════ Startup progress (boot view) ══════════════════════
// Kept verbatim in shape so `startup.rs` keeps driving the init progress bars.

/// One startup phase with live (done/total) progress.
pub struct Phase {
    /// Display label.
    pub label: &'static str,
    done: AtomicU64,
    total: AtomicU64,
}

impl Phase {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }
    }
    fn fraction(&self) -> f64 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            return 0.0;
        }
        (self.done.load(Ordering::Relaxed) as f64 / t as f64).clamp(0.0, 1.0)
    }
}

/// Live startup progress, updated by `startup.rs` at phase boundaries (cheap relaxed
/// stores, only during boot — never on the hot path). Read once per dashboard refresh.
pub struct StartupProgress {
    /// Ordered boot phases.
    pub phases: Vec<Phase>,
    /// Index of the phase currently running.
    pub active: AtomicUsize,
    /// Set true once the pipeline enters the hot path (dashboard switches to live view).
    pub hot: AtomicBool,
    /// Free-text live detail of the current sub-step.
    pub status: std::sync::Mutex<String>,
}

/// Stable phase indices (match the order pushed in `StartupProgress::new`).
pub mod phase {
    /// Pool discovery.
    pub const DISCOVERY: usize = 0;
    /// On-chain state snapshot.
    pub const SNAPSHOT: usize = 1;
    /// Route enumeration.
    pub const ENUMERATE: usize = 2;
    /// CSR / readiness build (warm-up).
    pub const READY: usize = 3;
}

impl StartupProgress {
    fn new() -> Self {
        Self {
            phases: vec![
                Phase::new("discovery"),
                Phase::new("snapshot"),
                Phase::new("enumerate"),
                Phase::new("warm-up"),
            ],
            active: AtomicUsize::new(0),
            hot: AtomicBool::new(false),
            status: std::sync::Mutex::new(String::new()),
        }
    }

    /// Set the free-text live status (current boot sub-step). Cheap; boot-time only.
    pub fn set_status(&self, s: impl Into<String>) {
        if let Ok(mut g) = self.status.lock() {
            *g = s.into();
        }
    }

    /// Set a phase's (done, total) and mark it active. No-op if the index is out of range.
    pub fn set(&self, idx: usize, done: u64, total: u64) {
        if let Some(p) = self.phases.get(idx) {
            p.total.store(total, Ordering::Relaxed);
            p.done.store(done, Ordering::Relaxed);
            self.active.store(idx, Ordering::Relaxed);
        }
    }

    /// Mark a phase fully complete.
    pub fn complete(&self, idx: usize) {
        if let Some(p) = self.phases.get(idx) {
            let t = p.total.load(Ordering::Relaxed).max(1);
            p.total.store(t, Ordering::Relaxed);
            p.done.store(t, Ordering::Relaxed);
        }
    }

    /// Switch the dashboard to the live hot-path view.
    pub fn enter_hot(&self) {
        self.hot.store(true, Ordering::Relaxed);
    }
}

// ════════════════════════ Optimizations chip line (boot) ════════════════════

/// Static snapshot of which optimizations are active (computed once from env at startup).
#[derive(Clone)]
pub struct OptSnapshot {
    /// Rendered `(key, value)` chips.
    pub chips: Vec<(&'static str, String)>,
}

impl OptSnapshot {
    /// Build from the current environment + compile-time facts.
    pub fn from_env() -> Self {
        let onoff = |k: &str, default_on: bool| match std::env::var(k).ok().as_deref() {
            Some("0") | Some("false") | Some("off") | Some("no") => false,
            Some(_) => true,
            None => default_on,
        };
        let opt_in = |k: &str| {
            matches!(
                std::env::var(k).ok().as_deref(),
                Some("1") | Some("true") | Some("on") | Some("yes")
            )
        };
        let yn = |b: bool| {
            if b {
                "on".to_string()
            } else {
                "off".to_string()
            }
        };
        let dual_relay = std::env::var("RELAY_ENDPOINT_2")
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let chips = vec![
            ("sysopt", yn(!onoff("SYSOPT_DISABLE", false))),
            ("core-pin", "on".to_string()),
            ("rt-sched", yn(opt_in("SYSOPT_RT_SCHED"))),
            ("mlockall", "on".to_string()),
            ("hugepages", "on".to_string()),
            ("lockfree", "on".to_string()),
            ("prefetch", "on".to_string()),
            ("simd-scan", "on".to_string()),
            ("mimalloc", "on".to_string()),
            ("revm", yn(onoff("REVM_VALIDATE", true))),
            ("shadow", yn(onoff("SHADOW_REVM", true))),
            ("fluid", yn(onoff("FLUID_POLLER", true))),
            ("dual-relay", yn(dual_relay)),
            ("graviton", "neoverse-v2".to_string()),
            ("metrics", yn(onoff("METRICS_ENABLED", true))),
            ("breach-guard", "on".to_string()),
        ];
        Self { chips }
    }
}

// ════════════════════════ TIER 2 — per-event trace ═════════════════════════

/// The kind of a notable hot-loop event (drives glyph + colour + status text).
///
/// `Submit` and `DropOwnTx` are emitted from the hot path (gated, non-blocking). The four safety
/// transitions are synthesised on the render thread from the live pill state (so they need no
/// hot-path wiring and cost the hot path nothing). Per-candidate REJECT volume is shown as the
/// aggregate `reject` count in the funnel row rather than one stream line each.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventKind {
    /// A validated opportunity was submitted (dry-run → /dev/null, or live).
    Submit,
    /// Own-EOA tx, dropped (checked first).
    DropOwnTx,
    /// Submission gated because the gas snapshot went stale. (Render-synthesised.)
    GasGated,
    /// Submission re-armed after a fresh gas snapshot. (Render-synthesised.)
    GasArmed,
    /// A REAL kill switch armed (breach monitor: latency / submit-failure / loss streak) —
    /// submission halted. (Render-synthesised from the registered `KillSwitches`.)
    Kill,
    /// All kill switches cleared — submission re-armed. (Render-synthesised.)
    Armed,
}

/// One notable hot-loop event — 40-byte `Copy`, a memcpy into the ring and nothing more.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Event {
    /// Wall-clock millis since the unix epoch (read once at emit, only when tracing is on).
    pub ts_ms: u64,
    /// Trade size (input amount) in wei, saturated to u64 (display-only).
    pub size_wei: u64,
    /// GROSS profit in wei (signed; net isn't known at the submit hand-off — the candidate
    /// carries gross, and the EV net stays on the validator thread). Labeled "gross" in the HUD.
    pub gross_wei: i64,
    /// Route id (index into the route table).
    pub route_id: u32,
    /// recv→submit latency in ns (Submit only; 0 otherwise).
    pub lat_ns: u32,
    /// Boundary slack in ms (Submit only).
    pub slack_ms: u32,
    /// Hop count of the route.
    pub hops: u8,
    /// Event kind.
    pub kind: EventKind,
}

// ════════════════════════ Telemetry — global handle ════════════════════════

/// Global dashboard telemetry. `Some` only when `DASHBOARD=1` and `init()` has run.
pub struct Telemetry {
    /// Boot progress (init view).
    pub progress: Arc<StartupProgress>,
    /// Tier-2 lock-free event ring.
    ring: ArrayQueue<Event>,
    /// Tier-2 overflow counter (visible honesty in the HUD).
    dropped: AtomicU64,
    /// Last submit's boundary slack (ms), for the HUD.
    last_slack_ms: AtomicU64,
    /// Master tracing gate (Tier 2). False → emit() is a ~1 ns no-op.
    trace_on: AtomicBool,
    /// Process start, for uptime.
    start: Instant,
}

static TEL: OnceLock<Telemetry> = OnceLock::new();

/// Capacity of the Tier-2 ring (power of two; ~64k × 40 B ≈ 2.5 MB).
const RING_CAP: usize = 1 << 16;

/// Whether the dashboard is enabled (`DASHBOARD=1`). The single on/off switch.
pub fn enabled() -> bool {
    matches!(
        std::env::var("DASHBOARD").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// Initialize the global telemetry (idempotent). Returns the shared handle. Call only when
/// `enabled()`. The returned `progress` is what `startup.rs` drives via [`progress()`].
pub fn init() -> &'static Telemetry {
    TEL.get_or_init(|| Telemetry {
        progress: Arc::new(StartupProgress::new()),
        ring: ArrayQueue::new(RING_CAP),
        dropped: AtomicU64::new(0),
        last_slack_ms: AtomicU64::new(0),
        trace_on: AtomicBool::new(true),
        start: Instant::now(),
    })
}

/// The global startup-progress handle, if the dashboard is active. No-op when off.
pub fn progress() -> Option<&'static StartupProgress> {
    TEL.get().map(|t| &*t.progress)
}

/// The global telemetry handle, if the dashboard is active.
pub fn telemetry() -> Option<&'static Telemetry> {
    TEL.get()
}

/// The pipeline's REAL kill switches, registered once the pipeline is constructed (which is
/// after `spawn()` — the dashboard boots first so the init bars animate). The safety pill
/// reads the live halt state from these instead of re-deriving it, so the HUD can never
/// disagree with what the submit path is actually doing (the breach monitor trips/rearms
/// these from a *windowed* p99 with debounce — not reproducible from the cumulative
/// histograms the dashboard reads).
static KILL: OnceLock<Arc<KillSwitches>> = OnceLock::new();
/// Resident universe size (pools, routes) for the live header. Set once after boot.
static DASH_POOLS: AtomicU64 = AtomicU64::new(0);
static DASH_ROUTES: AtomicU64 = AtomicU64::new(0);

/// Register the pipeline's kill switches with the HUD (idempotent; the HUD only reads).
pub fn set_kill(k: Arc<KillSwitches>) {
    let _ = KILL.set(k);
}

/// Record the resident universe size for the live header. Boot-time, called once.
pub fn set_universe(pools: u64, routes: u64) {
    DASH_POOLS.store(pools, Ordering::Relaxed);
    DASH_ROUTES.store(routes, Ordering::Relaxed);
}

/// Wall-clock millis since the unix epoch.
#[inline]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Hot-path emit. Non-blocking. Drops on overflow. Never allocates or formats. Gated: when the
/// dashboard is off (or tracing off) this is a single relaxed load and return (~1 ns).
#[inline(always)]
fn emit(
    kind: EventKind,
    route_id: u32,
    hops: u8,
    size_wei: u64,
    gross_wei: i64,
    lat_ns: u32,
    slack_ms: u32,
) {
    let Some(t) = TEL.get() else { return };
    if !t.trace_on.load(Ordering::Relaxed) {
        return;
    }
    let ev = Event {
        ts_ms: now_ms(),
        size_wei,
        gross_wei,
        route_id,
        lat_ns,
        slack_ms,
        hops,
        kind,
    };
    if t.ring.push(ev).is_err() {
        t.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Emit a submitted opportunity. The two arguments the HUD needs beyond the counters: the trade
/// economics for the stream line, and the boundary slack for the gas row.
#[inline]
pub fn emit_submit(
    route_id: u32,
    hops: u8,
    size_wei: u64,
    gross_wei: i64,
    lat_ns: u32,
    slack_ms: u32,
) {
    if let Some(t) = TEL.get() {
        t.last_slack_ms.store(slack_ms as u64, Ordering::Relaxed);
    }
    emit(
        EventKind::Submit,
        route_id,
        hops,
        size_wei,
        gross_wei,
        lat_ns,
        slack_ms,
    );
}

/// Emit an own-EOA drop (the engine checks its own EOA first and skips those txs).
#[inline]
pub fn emit_drop_owntx() {
    emit(EventKind::DropOwnTx, 0, 0, 0, 0, 0, 0);
}

// ════════════════════════ BOUNDARY — render thread ═════════════════════════

/// Everything the render thread reads from the engine's existing Tier-1 state. All `Arc`s are
/// clones of atomics the hot path already maintains; reading them adds no hot-path work.
pub struct DashboardInputs {
    /// Sequencer frames consumed.
    pub frames: Arc<AtomicU64>,
    /// Dirty-event packets (≈ "seen").
    pub dirty: Arc<AtomicU64>,
    /// Sim candidates.
    pub candidates: Arc<AtomicU64>,
    /// REVM-validated candidates (≈ "pass").
    pub validated: Arc<AtomicU64>,
    /// Submitter shipments (≈ "hits").
    pub submitted: Arc<AtomicU64>,
    /// Relay reconnects (≈ "drop").
    pub reconnects: Arc<AtomicU64>,
    /// Sequencer feed sequence gaps.
    pub seq_gaps: Arc<AtomicU64>,
    /// Gas oracle — read once per refresh for the HUD gas readouts.
    pub gas: Arc<GasOracle>,
    /// Per-stage latency histograms.
    pub latency: Arc<LatencyHistograms>,
    /// Optimization snapshot (boot OPTIMIZATIONS line).
    pub opts: OptSnapshot,
    /// True when the engine is configured to BROADCAST (`live_submit`); drives the mode chip
    /// and the pnl suffix so the HUD never claims "dry-run" while real capital is at risk.
    pub live: bool,
    /// Chain id from the config (42161 renders as "arbitrum 42161").
    pub chain_id: u64,
    /// Shared shutdown flag.
    pub shutdown: Arc<AtomicBool>,
}

/// Spawn the render thread. Returns its handle. The thread exits when `shutdown` is set.
/// Runs entirely on the housekeeping core (the hot threads own the isolated cores), only READS
/// the Tier-1 atomics/histograms/gas and drains the Tier-2 ring — no hot-path interaction.
pub fn spawn(inputs: DashboardInputs) -> std::io::Result<JoinHandle<()>> {
    let hz: u64 = std::env::var("DASHBOARD_REFRESH_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&ms| ms >= 30)
        .map(|ms| (1000 / ms).max(1))
        .unwrap_or(HZ_DEFAULT);
    std::thread::Builder::new()
        .name("engine-dashboard".into())
        .spawn(move || {
            use std::io::{IsTerminal, Write};
            // Keep the render thread on the housekeeping core with the other background
            // threads — never time-sharing a pinned hot core (1-7), so the HUD can't add
            // scheduler jitter to the very latencies it displays. Best-effort (logs + runs
            // unpinned where the sandbox forbids it).
            crate::runtime::pin_to_core(0, "engine-dashboard");
            let tel = TEL
                .get()
                .expect("dashboard::init() must run before spawn()");
            let tty = std::io::stdout().is_terminal();
            let mut rs = RenderState::new();
            rs.tty = tty;
            rs.color = tty && color();
            // Piped/redirected stdout: control sequences are meaningless and 12 frames/s is
            // log spam — emit one plain-text frame per second instead.
            let hz = if tty { hz } else { 1 };
            let mut out = std::io::stdout().lock();
            if tty {
                // Ctrl+C / SIGTERM / hangup would otherwise kill the process INSIDE the
                // alternate screen (no handler exists anywhere in the engine — the kernel
                // default fires) and wedge the user's terminal. Restore-then-reraise hook,
                // installed before the alt screen is entered.
                engine_sysopt::install_terminal_restore_on_signals();
                let _ = write!(out, "{ALT_ENTER}");
                // Same protection for panics: restore the screen first, then let the default
                // hook report the panic (visibly, on the MAIN screen).
                let prev = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    let mut so = std::io::stdout();
                    let _ = write!(so, "{ALT_LEAVE}");
                    let _ = so.flush();
                    prev(info);
                }));
            }
            let frame_dt = Duration::from_millis(1000 / hz.max(1));
            // Absolute-deadline cadence (`next += dt`) so per-frame render cost never
            // accumulates as drift; clamped to "now" after a stall so one slow frame can't
            // trigger a catch-up burst.
            let mut next = Instant::now() + frame_dt;
            while !inputs.shutdown.load(Ordering::Acquire) {
                // Drain the Tier-2 ring into the bounded tail (bounded work per tick).
                let mut budget = 4096;
                while budget > 0 {
                    match tel.ring.pop() {
                        Some(ev) => {
                            rs.tail.push_back(ev);
                            budget -= 1;
                        }
                        None => break,
                    }
                }
                while rs.tail.len() > TAIL {
                    rs.tail.pop_front();
                }
                if tty {
                    // Re-read the size every frame (one ioctl, ~ns, render thread only) so a
                    // resize never leaves wrapped or scrolled lines corrupting the repaint.
                    (rs.cols, rs.rows) = term_size();
                }
                let frame = render(&mut rs, tel, &inputs);
                let _ = out.write_all(frame.as_bytes());
                let _ = out.flush();
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                }
                next = std::cmp::max(next + frame_dt, Instant::now());
            }
            if tty {
                let _ = write!(out, "{ALT_LEAVE}");
                let _ = out.flush();
            }
        })
}

/// Cross-refresh render-thread state (render thread only; never shared with the hot path).
struct RenderState {
    tail: std::collections::VecDeque<Event>,
    prev_frames: u64,
    last_tick: Instant,
    feed_fps: f64,
    /// Last safety-pill name, to synthesise GasGated/Kill/Armed stream events on transition.
    last_pill: &'static str,
    commit: String,
    /// stdout is a terminal → in-place repaint with cursor/screen controls. Otherwise plain
    /// newline-separated frames (pipeable).
    tty: bool,
    /// Emit SGR colour (tty + NO_COLOR unset). Controls and colour are independent: a
    /// NO_COLOR terminal still gets the flicker-free alt-screen repaint, just monochrome.
    color: bool,
    /// Terminal width; every line is clipped below it so wrap can never corrupt the repaint.
    cols: usize,
    /// Terminal height; the event tail shrinks to fit so the frame never scrolls (a frame
    /// taller than the screen scrolls every repaint — the worst possible stutter).
    rows: usize,
    /// Trend history (one sample/sec) for the inline sparklines. Render-thread only.
    feed_hist: std::collections::VecDeque<f64>,
    gross_hist: std::collections::VecDeque<f64>,
    /// Detail rows: 0 = compact (core only), 1 = auto (draw the extras that fit), 2 = force.
    /// From `DASHBOARD_DETAIL` (unset → auto). Auto/force never overflow — the tail absorbs it.
    detail: u8,
    /// Cached count of fast-lane families that have met the promotion gate (recomputed ~1/s so
    /// the per-frame path never parses env / scans the family table).
    promo_ready: usize,
    /// When the safety pill first left NOMINAL (render-thread clock) → "held Xs" in the header.
    /// `None` while nominal. Avoids any wall-clock/monotonic ambiguity in the kill-switch stamp.
    halt_since: Option<Instant>,
}
impl RenderState {
    fn new() -> Self {
        use std::io::IsTerminal;
        let tty = std::io::stdout().is_terminal();
        let (cols, rows) = if tty {
            term_size()
        } else {
            (usize::MAX, usize::MAX)
        };
        let detail = match std::env::var("DASHBOARD_DETAIL").ok().as_deref() {
            Some("0") | Some("off") | Some("compact") => 0,
            Some("2") | Some("full") | Some("force") => 2,
            _ => 1, // auto
        };
        Self {
            tail: std::collections::VecDeque::with_capacity(TAIL + 1),
            prev_frames: 0,
            last_tick: Instant::now(),
            feed_fps: 0.0,
            last_pill: "NOMINAL",
            commit: option_env!("GIT_COMMIT")
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(7).collect())
                .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION"))),
            tty,
            color: tty && color(),
            cols,
            rows,
            feed_hist: std::collections::VecDeque::with_capacity(SPARK_HIST + 1),
            gross_hist: std::collections::VecDeque::with_capacity(SPARK_HIST + 1),
            detail,
            promo_ready: 0,
            halt_since: None,
        }
    }
}

/// Current terminal size as (cols, rows) (one `TIOCGWINSZ` ioctl; render thread only).
/// Falls back to (100, 30) when the query fails — still clip-bounded either way.
fn term_size() -> (usize, usize) {
    let (c, r) = engine_sysopt::terminal_size();
    (c as usize, r as usize)
}

// ── formatting helpers ──────────────────────────────────────────────────────

fn eth(wei: i64) -> String {
    format!("{:+.4}", wei as f64 / 1e18)
}
fn size_eth(wei: u64) -> String {
    format!("{:.2}", wei as f64 / 1e18)
}
fn commas(n: u64) -> String {
    let s = n.to_string();
    let b = s.as_bytes();
    let mut o = String::with_capacity(s.len() + s.len() / 3);
    let first = b.len() % 3;
    for (i, c) in b.iter().enumerate() {
        if i != 0 && i % 3 == first {
            o.push(',');
        }
        o.push(*c as char);
    }
    o
}
fn human(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}
/// Raw p99 of a stage in nanoseconds, `None` with no samples (so callers can colour by threshold
/// AND print "—" distinctly from a real 0). Background reader path — never on the hot path.
fn p99v(h: &LatencyHistograms, st: StageLatency) -> Option<u64> {
    if h.count(st) == 0 {
        None
    } else {
        Some(h.percentile(st, 99.0))
    }
}
/// Format a nanosecond latency in the most readable unit (µs under 1 ms, else ms) — matches [`p99`].
fn fmt_lat(ns: u64) -> String {
    let v = ns as f64;
    if v >= 1e6 {
        format!("{:.1}ms", v / 1e6)
    } else {
        format!("{:.1}µs", v / 1e3)
    }
}
/// Format the feed-event-age histogram, whose samples are integer wall-clock seconds from the
/// sequencer header. Keeping this separate prevents the coarse cross-host value being mislabeled
/// as a sub-millisecond same-host latency.
fn fmt_feed_age(seconds: u64) -> String {
    format!("{seconds}s")
}
/// Threshold colour for a latency value vs DISPLAY heuristics (mint ok / amber warn / red bad).
/// These guide the eye only — the safety pill + kill switches own the real halt truth; a red cell
/// here never gates submission. `None` (no samples) renders faint.
fn lat_col(ns: Option<u64>, warn_ns: u64, bad_ns: u64) -> &'static str {
    match ns {
        None => FNT,
        Some(v) if v >= bad_ns => RED,
        Some(v) if v >= warn_ns => AMBER,
        Some(_) => MINT,
    }
}
/// One `label value` latency cell: p99 of `st`, coloured against the (warn,bad) heuristics.
fn lat_cell(
    label: &str,
    h: &LatencyHistograms,
    st: StageLatency,
    warn_ns: u64,
    bad_ns: u64,
) -> String {
    let v = p99v(h, st);
    let col = lat_col(v, warn_ns, bad_ns);
    let txt = v.map(fmt_lat).unwrap_or_else(|| "—".into());
    format!("{DIM}{label}{RST} {col}{txt}{RST}")
}
/// e2e latency as a `p50·p99` band (coloured by the p99 tail): shows whether the path is fast for
/// the median AND the tail, not just one number — the SRE/HFT-standard read. "—" with no samples.
fn e2e_band(h: &LatencyHistograms, warn_ns: u64, bad_ns: u64) -> String {
    if h.count(StageLatency::EndToEnd) == 0 {
        return format!("{DIM}e2e{RST} {FNT}—{RST}");
    }
    let p50 = h.percentile(StageLatency::EndToEnd, 50.0);
    let p99 = h.percentile(StageLatency::EndToEnd, 99.0);
    let col = lat_col(Some(p99), warn_ns, bad_ns);
    format!(
        "{DIM}e2e{RST} {ICE}{}{DIM}·{RST}{col}{}{RST}",
        fmt_lat(p50),
        fmt_lat(p99)
    )
}
/// Render an autoscaled unicode-block sparkline of the last `width` samples (min..max → ▁..█).
/// Pure render-thread function over `RenderState` history; empty/non-finite input → "". A flat
/// series renders a mid bar (a visible line) rather than nothing, so "steady" reads differently
/// from "no data".
fn spark(samples: &std::collections::VecDeque<f64>, width: usize) -> String {
    let n = samples.len();
    if n == 0 || width == 0 {
        return String::new();
    }
    let start = n.saturating_sub(width);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &v in samples.iter().skip(start) {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return String::new();
    }
    let span = hi - lo;
    let mut out = String::with_capacity((n - start) * 3);
    for &v in samples.iter().skip(start) {
        let idx = if !v.is_finite() {
            0
        } else if span <= 0.0 {
            3 // flat line, mid-height
        } else {
            (((v - lo) / span) * 7.0).round().clamp(0.0, 7.0) as usize
        };
        out.push(SPARK_BARS[idx]);
    }
    out
}
/// Append a sample to a bounded sparkline history, dropping the oldest past `SPARK_HIST`.
fn push_hist(h: &mut std::collections::VecDeque<f64>, v: f64) {
    h.push_back(v);
    while h.len() > SPARK_HIST {
        h.pop_front();
    }
}
/// Per-family `agree` samples required before a fast-lane family is promotion-eligible — matches
/// the 30 s heartbeat's gate so the HUD's `promo▸` count agrees with the log.
const FASTLANE_PROMO_MIN_SAMPLES: u64 = 10_000;
fn hms_from_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        (s / 3600) % 24,
        (s / 60) % 60,
        s % 60,
        ms % 1000
    )
}

/// Visible terminal columns of `s`: each char is one column except chars inside CSI escape
/// sequences (`ESC [ … final-byte`), which are zero. Every glyph the HUD prints is
/// single-width, so char-counting is exact here.
fn vis_width(s: &str) -> usize {
    let mut w = 0usize;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' && it.peek() == Some(&'[') {
            it.next();
            for c2 in it.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c2) {
                    break;
                }
            }
            continue;
        }
        w += 1;
    }
    w
}

/// Pad `s` with trailing spaces to `w` VISIBLE columns. Plain `{:<w}` formatting counts the
/// invisible SGR bytes, so a coloured cell would under-pad and the columns to its right
/// would wander row-to-row — this pads what the eye actually sees. Wider strings pass
/// through unchanged (`clip_vis` bounds the assembled line).
fn pad_vis(s: &str, w: usize) -> String {
    let vw = vis_width(s);
    if vw >= w {
        return s.to_string();
    }
    let mut o = String::with_capacity(s.len() + (w - vw));
    o.push_str(s);
    for _ in 0..(w - vw) {
        o.push(' ');
    }
    o
}

/// Clip `s` to at most `w` visible columns. Escape sequences pass through (zero width) and
/// a reset is appended when any colour was kept, so a clipped line can't bleed style into
/// the next. Keeping every physical line under the terminal width is what prevents
/// wrap — and a wrapped line is what turns the in-place repaint into flicker/garbage.
fn clip_vis(s: &str, w: usize) -> String {
    if s.len() <= w {
        return s.to_string(); // bytes ≥ visible columns → already fits
    }
    if vis_width(s) <= w {
        return s.to_string();
    }
    let mut o = String::with_capacity(w + 8);
    let mut vis = 0usize;
    let mut kept_sgr = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' && it.peek() == Some(&'[') {
            o.push(c);
            o.push(it.next().unwrap());
            for c2 in it.by_ref() {
                o.push(c2);
                if ('\u{40}'..='\u{7e}').contains(&c2) {
                    kept_sgr |= c2 == 'm';
                    break;
                }
            }
            continue;
        }
        if vis == w {
            break;
        }
        o.push(c);
        vis += 1;
    }
    if kept_sgr {
        o.push_str(RST);
    }
    o
}

/// Strip every `\x1b[…m` SGR sequence (single NO_COLOR enforcement point; HUD embeds colour
/// constants inline). Leaves cursor/screen control sequences untouched is unnecessary here —
/// this is applied to the body text only, before the control wrapper is added.
fn strip_sgr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'[' {
            let mut j = i + 2;
            while j < b.len() && b[j] != b'm' && b[j] != b'K' && b[j] != b'J' && b[j] != b'H' {
                j += 1;
            }
            // Only strip colour ('m'); keep cursor/clear controls (K/J/H).
            if j < b.len() && b[j] == b'm' {
                i = j + 1;
                continue;
            }
        }
        let len = utf8_len(b[i]);
        let end = (i + len).min(b.len());
        out.push_str(&s[i..end]);
        i = end;
    }
    out
}
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

// ── safety pill (REAL engine state: kill switches, then gas freshness) ──────

/// HALT states read the pipeline's actual kill switches — the exact atomics the submit
/// thread bails on — never a re-derivation. (The breach monitor trips/rearms those from a
/// windowed p99 with debounce; deriving HALT from the cumulative histogram here used to
/// latch the pill red forever after one early spike, while the engine submitted happily.)
fn derive_pill(i: &DashboardInputs) -> (&'static str, &'static str) {
    if let Some(k) = KILL.get() {
        if k.global.is_armed() {
            return (RED, "HALT");
        }
        if k.feed.is_armed() {
            return (RED, "HALT·feed");
        }
        if k.inclusion.is_armed() {
            return (RED, "HALT·incl");
        }
        if k.gas.is_armed() {
            return (AMBER, "HALT·gas");
        }
    }
    let gas = i.gas.load();
    let gas_stale =
        gas.captured_at_ms > 0 && now_ms().saturating_sub(gas.captured_at_ms) > i.gas.max_age_ms;
    if gas_stale {
        (AMBER, "GATED")
    } else {
        (MINT, "NOMINAL")
    }
}

/// Pill severity class, for synthesising stream events on transitions.
fn pill_class(name: &str) -> u8 {
    if name.starts_with("HALT") {
        2
    } else if name == "GATED" {
        1
    } else {
        0
    }
}

// ── one event row in the HOT LOOP stream ────────────────────────────────────

fn fmt_event(ev: &Event) -> String {
    let ts = hms_from_ms(ev.ts_ms);
    let route = format!("#{} · {}-hop", ev.route_id, ev.hops);
    let (col, gl, status, main, right) = match ev.kind {
        EventKind::Submit => (
            MINT,
            "✓",
            "submit",
            format!(
                "{route} · {ICE}{}Ξ{RST}{DIM} · gross {MINT}{}{RST}",
                size_eth(ev.size_wei),
                eth(ev.gross_wei)
            ),
            // recv→submit, i.e. feed RX to sequencer hand-off.
            format!(
                "rx→sub {:.1}ms · slack {}ms",
                ev.lat_ns as f64 / 1e6,
                ev.slack_ms
            ),
        ),
        EventKind::DropOwnTx => (
            DIM,
            "·",
            "drop OwnTx",
            "own EOA · checked first".to_string(),
            String::new(),
        ),
        EventKind::GasGated => (
            AMBER,
            "!",
            "gas gated",
            "L1 snapshot stale > max age".to_string(),
            "submit held".to_string(),
        ),
        EventKind::GasArmed => (
            MINT,
            "✓",
            "gas re-armed",
            "fresh L1 snapshot".to_string(),
            String::new(),
        ),
        EventKind::Kill => (
            RED,
            "✗",
            "kill-switch",
            "kill switch armed · submission halted".to_string(),
            "breach monitor".to_string(),
        ),
        EventKind::Armed => (
            MINT,
            "✓",
            "armed",
            "kill cleared · submission re-armed".to_string(),
            String::new(),
        ),
    };
    // pad_vis (not `{:<n}`): the Submit cell embeds SGR colour whose bytes a plain format
    // pad would count, drifting the columns to its right on exactly the rows people read.
    format!(
        "{FNT}{ts}{RST}  {col}{gl} {}{RST} {ICE}{}{RST}  {DIM}{right}{RST}",
        pad_vis(status, 22),
        pad_vis(&main, 52),
    )
}

// ── the frame ───────────────────────────────────────────────────────────────

fn render(rs: &mut RenderState, tel: &Telemetry, i: &DashboardInputs) -> String {
    // feed rate — averaged over a ≥1 s window, NOT per render tick. The feed is bursty (a batch
    // of txs every ~250 ms), so sampling over one 12 Hz frame (~83 ms) flickers between 0 and a
    // spike. Recompute once per second over the full window and hold the value between updates.
    let frames = i.frames.load(Ordering::Relaxed);
    let now = Instant::now();
    let win = now.duration_since(rs.last_tick).as_secs_f64();
    let hot = tel.progress.hot.load(Ordering::Relaxed);
    if win >= 1.0 {
        rs.feed_fps = (frames.saturating_sub(rs.prev_frames)) as f64 / win;
        rs.prev_frames = frames;
        rs.last_tick = now;
        // Trend history + promotion-ready cache: once/sec, render thread only (no hot-path read).
        // Gated on `hot` so the boot view never parses env or scans the family table.
        if hot {
            push_hist(&mut rs.feed_hist, rs.feed_fps);
            push_hist(&mut rs.gross_hist, crate::pipeline::dash_gross_profit_eth());
            rs.promo_ready = crate::pipeline::dash_fastlane_promotion_ready(
                &crate::fastlane_policy::FastlanePromotion::from_env(),
                FASTLANE_PROMO_MIN_SAMPLES,
            )
            .len();
        }
    }

    let up = tel.start.elapsed().as_secs();
    let (h, mi, s) = (up / 3600, (up / 60) % 60, up % 60);
    let (pcol, pname) = derive_pill(i);

    // Track how long the pill has been off-NOMINAL (render-thread clock → "held Xs" in the
    // header). Reset on return to NOMINAL. Using the render clock sidesteps any wall-clock vs
    // monotonic ambiguity in the kill-switch's own trip stamp.
    if hot {
        if pname == "NOMINAL" {
            rs.halt_since = None;
        } else if rs.halt_since.is_none() {
            rs.halt_since = Some(now);
        }
    }

    // Synthesise safety-transition stream events from the pill (render-thread only → zero
    // hot-path cost). Only meaningful once live; pushed straight into the tail by time.
    if hot && pname != rs.last_pill {
        let kind = match (pill_class(rs.last_pill), pill_class(pname)) {
            (a, 2) if a != 2 => Some(EventKind::Kill),
            (a, 1) if a != 1 => Some(EventKind::GasGated),
            (2, 0) => Some(EventKind::Armed),
            (1, 0) => Some(EventKind::GasArmed),
            _ => None,
        };
        if let Some(kind) = kind {
            rs.tail.push_back(Event {
                ts_ms: now_ms(),
                size_wei: 0,
                gross_wei: 0,
                route_id: 0,
                lat_ns: 0,
                slack_ms: 0,
                hops: 0,
                kind,
            });
            while rs.tail.len() > TAIL {
                rs.tail.pop_front();
            }
        }
        rs.last_pill = pname;
    }

    // Clip to width-1 (not width): exact-width lines wrap immediately on terminals without
    // deferred-wrap, and one wrapped line shears every repaint below it.
    let clipw = rs.cols.saturating_sub(1);
    let tty = rs.tty;
    let mut o = String::with_capacity(6144);
    if tty {
        o.push_str("\x1b[H"); // home — repaint in place, never scroll
    }
    macro_rules! line {
        ($($a:tt)*) => {{
            let l = format!($($a)*);
            o.push_str(&clip_vis(&l, clipw));
            o.push_str(if tty { "\x1b[K\r\n" } else { "\n" });
        }}
    }

    let (mcol, mlabel) = if i.live {
        (AMBER, "LIVE")
    } else {
        (DIM, "dry-run")
    };
    let chain = if i.chain_id == 42_161 {
        "arbitrum 42161".to_string()
    } else {
        format!("chain {}", i.chain_id)
    };

    // Pill "held Xs" — how long submission has been gated/halted; empty while nominal.
    let pill_held = match rs.halt_since {
        Some(t0) if pname != "NOMINAL" => {
            format!("{DIM} · held {pcol}{}s{RST}", t0.elapsed().as_secs())
        }
        _ => String::new(),
    };

    // ── header (always) ──
    line!("{BOLD}{ACCENT}  M A N T I S{RST}{DIM}  arbitrage engine · {chain} · {mcol}{mlabel}{RST}{DIM}        up {h:02}:{mi:02}:{s:02}        [ {pcol}{pname}{RST}{pill_held} {DIM}]{RST}");
    line!("{FNT}  ──────────────────────────────────────────────────────────────────────────────────────{RST}");

    if !tel.progress.hot.load(Ordering::Relaxed) {
        // ════════ BOOT VIEW — banner + animated progress bars + optimizations ════════
        line!("{DIM}  commit {ICE}{}{RST}{DIM}   mode {mcol}{mlabel}{RST}{DIM}   executor {ICE}MantisExecutor.sol{RST}", rs.commit);
        line!("");
        line!("{DIM}  INITIALIZING{RST}");
        for p in &tel.progress.phases {
            let frac = p.fraction();
            let pct = (frac * 100.0).round() as u64;
            let done = p.done.load(Ordering::Relaxed);
            let total = p.total.load(Ordering::Relaxed);
            let track_w = 44usize;
            let fill = ((frac * track_w as f64).round() as usize).min(track_w);
            let bar = format!(
                "{MINT}{}{FNT}{}{RST}",
                "█".repeat(fill),
                "░".repeat(track_w - fill)
            );
            let stat = if pct >= 100 {
                format!("{MINT}✓{RST}")
            } else {
                format!("{MINT}▸{RST}")
            };
            let count = if total > 0 {
                format!("{ICE}{}{FNT} / {}{RST}", commas(done), commas(total))
            } else {
                String::new()
            };
            line!(
                "  {stat} {DIM}{:<10}{RST} {bar} {ICE}{:>3}%{RST}  {count}",
                p.label,
                pct
            );
        }
        if let Ok(st) = tel.progress.status.lock() {
            if !st.is_empty() {
                line!("    {STEEL}▸ {}{RST}", st);
            }
        }
        line!("");
        // OPTIMIZATIONS chips
        let mut chips = String::new();
        for (k, v) in &i.opts.chips {
            let vc = match v.as_str() {
                "on" => MINT,
                "off" => FNT,
                _ => ICE,
            };
            chips.push_str(&format!("{DIM}{k}:{vc}{v}{RST}  "));
        }
        line!("  {STEEL}OPTIMIZATIONS{RST}  {chips}");
    } else {
        // ════════ LIVE VIEW — full HUD instrument cluster ════════
        let lat = &i.latency;
        let (gas_l2, gas_l1, gas_age, gas_stale) = gas_snapshot_strs(i);
        let (ign, kp, kr, nr, own) = crate::pipeline::dash_txclass();
        let class_total = ign + kp + kr + nr + own;
        let known_pct = if class_total > 0 {
            100.0 * (kp + kr) as f64 / class_total as f64
        } else {
            0.0
        };
        let block = crate::pipeline::dash_last_l2_block();
        let gross_eth = crate::pipeline::dash_gross_profit_eth();
        let seen = i.dirty.load(Ordering::Relaxed);
        let pass = i.validated.load(Ordering::Relaxed);
        let cand = i.candidates.load(Ordering::Relaxed);
        let reject = cand.saturating_sub(pass);
        let sub = i.submitted.load(Ordering::Relaxed);
        let gaps = i.seq_gaps.load(Ordering::Relaxed);
        let recon = i.reconnects.load(Ordering::Relaxed);
        let dropped = tel.dropped.load(Ordering::Relaxed);
        let slack = tel.last_slack_ms.load(Ordering::Relaxed);
        let pools = DASH_POOLS.load(Ordering::Relaxed);
        let routes = DASH_ROUTES.load(Ordering::Relaxed);
        // Sub-10 fr/s keeps a decimal: the feed is bursty and "0 vs 0.8" is signal on a
        // quiet link, where a truncated integer reads as a dead feed.
        let fps = if rs.feed_fps < 10.0 {
            format!("{:.1}", rs.feed_fps)
        } else {
            human(rs.feed_fps as u64)
        };
        // seq→rx is recorded from integer-second sequencer header timestamps. Render and colour
        // in that actual unit (warn 1 s / bad 2 s); treating it as ns made a one-second age look
        // like 0.0µs and falsely green.
        let feed_age_seconds = p99v(lat, StageLatency::FeedLatency);
        let feed_col = lat_col(feed_age_seconds, 1, 2);
        let feed_txt = feed_age_seconds
            .map(fmt_feed_age)
            .unwrap_or_else(|| "—".into());

        // ── feed: block height · rate (+14 s trend) · seq→rx · gap/drop · known-flow % ──
        line!(
            "  {DIM}block{RST} {ICE}{:>12}{RST}  {DIM}feed{RST} {ICE}{:>5}{RST}{FNT} fr/s{RST} {STEEL}{}{RST}  {DIM}seq→rx{RST} {feed_col}{feed_txt}{RST}  {DIM}gap/drop{RST} {ICE}{}/{}{RST}  {DIM}known{RST} {ICE}{:.1}%{RST}",
            commas(block), fps, spark(&rs.feed_hist, SPARK_W), gaps, recon, known_pct
        );
        // ── pipe: per-stage p99 (threshold-coloured) + e2e p50·p99 band — the egress instrument.
        // Adds submit (sign→wire) which was tracked but never shown; e2e shows median AND tail.
        line!(
            "  {DIM}pipe{RST}  {}  {}  {}  {}  {}",
            lat_cell("classify", lat, StageLatency::Classify, 50_000, 200_000),
            lat_cell("sim", lat, StageLatency::Sim, 1_000_000, 3_000_000),
            lat_cell("revm", lat, StageLatency::Revm, 5_000_000, 30_000_000),
            lat_cell("submit", lat, StageLatency::Submit, 200_000, 1_000_000),
            e2e_band(lat, 3_000_000, 8_000_000)
        );
        // ── funnel: seen → cand (hit-rate) → pass (accept-rate) → reject · dropped events ──
        let cand_rate = if seen > 0 {
            100.0 * cand as f64 / seen as f64
        } else {
            0.0
        };
        let pass_rate = if cand > 0 {
            100.0 * pass as f64 / cand as f64
        } else {
            0.0
        };
        line!(
            "  {DIM}funnel{RST} {DIM}seen{RST} {ICE}{}{RST}  {DIM}cand{RST} {ICE}{}{RST}{FNT} {:.2}%{RST}  {DIM}pass{RST} {MINT}{}{RST}{FNT} {:.0}%{RST}  {DIM}reject{RST} {AMBER}{}{RST}  {DIM}drop-ev{RST} {}{}{RST}",
            human(seen), human(cand), cand_rate, pass, pass_rate, human(reject),
            if dropped > 0 { AMBER } else { FNT }, dropped
        );
        // ── gas: L2 price · L1 base fee per gas · snapshot age (amber if stale) · boundary slack ──
        line!(
            "  {DIM}gas{RST}   {DIM}l2{RST} {ICE}{} gwei{RST}  {DIM}l1{RST} {ICE}{} gwei/gas{RST}  {DIM}age{RST} {}{}{RST}  {DIM}slack{RST} {ICE}{}ms{RST}",
            gas_l2, gas_l1, if gas_stale { AMBER } else { ICE }, gas_age, slack
        );
        // ── pnl: submissions · cumulative gross (+14 s trend) · resident universe size ──
        line!(
            "  {DIM}pnl{RST}   {DIM}sub{RST} {ICE}{}{RST}  {DIM}gross Σ{RST} {}{:+.4} ETH{RST} {STEEL}{}{RST}{DIM} ({}){RST}  {DIM}universe{RST} {ICE}{}{RST}{DIM} pools · {ICE}{}{RST}{DIM} routes{RST}",
            sub, if gross_eth >= 0.0 { MINT } else { RED }, gross_eth, spark(&rs.gross_hist, SPARK_W),
            if i.live { "live" } else { "dry" }, commas(pools), commas(routes)
        );

        // ── optional detail rows (height-gated; DASHBOARD_DETAIL 0=off · 2=force) ──────────────
        // Core above = 5 rows; header+divider = 2; HOT LOOP chrome = 2. Draw fastlane/io/class
        // only while a readable tail survives, so an 80×24 still fits and a taller terminal
        // reveals everything — denser when there is room, never at the cost of the event stream.
        const HOTLOOP_CHROME: usize = 2;
        const TAIL_MIN: usize = 3;
        let avail = rs.rows.saturating_sub(1); // one line of headroom (final line must not scroll)
        let extras = match rs.detail {
            0 => 0,
            2 => 3usize.min(avail.saturating_sub(7 + HOTLOOP_CHROME + 1)), // force; keep tail ≥ 1
            _ => 3usize.min(avail.saturating_sub(7 + HOTLOOP_CHROME + TAIL_MIN)), // auto
        };
        if extras >= 1 {
            // fastlane: zero-RPC shadow funnel + agreement (disagree MUST be 0 to promote) + the
            // authoritative accept/reject/strict tallies + how many families now clear the gate.
            let (fl_seen, fl_elig, fl_agree, fl_dis, fl_inc) =
                crate::pipeline::dash_fastlane_funnel();
            let (fa_acc, fa_rej, fa_strict) = crate::pipeline::dash_fastlane_auth();
            let dis_col = if fl_dis > 0 { RED } else { MINT };
            let promo_col = if rs.promo_ready > 0 { MINT } else { FNT };
            line!(
                "  {DIM}fastlane{RST} {DIM}seen{RST} {ICE}{}{DIM}→elig{RST} {ICE}{}{RST}  {DIM}agree{RST} {MINT}{}{RST} {DIM}dis{RST} {dis_col}{}{RST} {DIM}inc{RST} {FNT}{}{RST}  {DIM}auth{RST} {MINT}{}{DIM}a {AMBER}{}{DIM}r {FNT}{}{DIM}strict{RST}  {DIM}promo▸{RST} {promo_col}{}{RST}",
                human(fl_seen), human(fl_elig), human(fl_agree), human(fl_dis), human(fl_inc),
                fa_acc, fa_rej, fa_strict, rs.promo_ready
            );
        }
        if extras >= 2 {
            // io/integrity: RPC volume + Alchemy rtt + the COLD hot-path-RPC smoke alarm (must be
            // 0; red if not) + validator fork health (repin / suspect / replay-drain budget hits).
            let (rpc_cold, drain_hit, fork_suspect, repin) = crate::pipeline::dash_revm_integrity();
            let rpc_fetches = crate::revm_fork::rpc_fetch_count();
            let rpc_errs = crate::revm_fork::rpc_fetch_error_count();
            let rpc_lat = p99v(lat, StageLatency::RpcLatency);
            let rpc_col = lat_col(rpc_lat, 30_000_000, 80_000_000);
            let rpc_txt = rpc_lat.map(fmt_lat).unwrap_or_else(|| "—".into());
            let cold_col = if rpc_cold > 0 { RED } else { MINT };
            let err_col = if rpc_errs > 0 { AMBER } else { FNT };
            let susp_col = if fork_suspect > 0 { AMBER } else { FNT };
            line!(
                "  {DIM}io{RST}    {DIM}rpc{RST} {ICE}{}{RST}  {DIM}rtt{RST} {rpc_col}{rpc_txt}{RST}  {DIM}cold{RST} {cold_col}{}{RST}  {DIM}err{RST} {err_col}{}{RST}  {DIM}repin{RST} {ICE}{}{RST}  {DIM}suspect{RST} {susp_col}{}{RST}  {DIM}drain-hit{RST} {ICE}{}{RST}",
                human(rpc_fetches), rpc_cold, rpc_errs, repin, fork_suspect, drain_hit
            );
        }
        if extras >= 3 {
            // class: full tx-classification split + classifier health (predict-fail amber if any,
            // calldata-sniff fallback volume) — the 30 s "txclass breakdown" log, live.
            let (cl_ign, pf, pe, sh) = crate::pipeline::dash_class_health();
            let pf_col = if pf > 0 { AMBER } else { FNT };
            line!(
                "  {DIM}class{RST} {DIM}ign{RST} {ICE}{}{RST}  {DIM}pool{RST} {ICE}{}{RST}  {DIM}router{RST} {ICE}{}{RST}  {DIM}revm{RST} {ICE}{}{RST}  {DIM}own{RST} {ICE}{}{RST}  {DIM}pred-fail{RST} {pf_col}{}{RST}  {DIM}empty{RST} {ICE}{}{RST}  {DIM}sniff{RST} {ICE}{}{RST}",
                human(cl_ign), human(kp), human(kr), human(nr), human(own), pf, human(pe), human(sh)
            );
        }

        line!("{MINT}  ─────────────────────────────────  HOT LOOP · notable events  ──────────────────────{RST}");
        line!(
            "{FNT}  {:<12}  {:<24} {:<52}  {}{RST}",
            "time",
            "event",
            "candidate",
            "detail"
        );

        // Shrink the tail to the rows actually available — header+divider(2)+core(5)+extras+HOT
        // LOOP chrome(2) — so the frame never exceeds the screen (an overflowing frame scrolls on
        // every repaint). With extras=0 this is exactly the original `rows-10` budget.
        let tail_rows = TAIL
            .min(avail.saturating_sub(7 + extras + HOTLOOP_CHROME))
            .max(1);
        let n = rs.tail.len();
        for _ in 0..tail_rows.saturating_sub(n) {
            line!("");
        }
        for ev in rs.tail.iter().skip(n.saturating_sub(tail_rows)) {
            line!("  {}", fmt_event(ev));
        }
    }

    if tty {
        o.push_str("\x1b[J"); // clear anything below the frame
    } else {
        o.push('\n'); // frame separator for piped output
    }

    // Monochrome strips SGR only — the cursor/clear controls stay, because they are what
    // keep a NO_COLOR terminal flicker-free too. Synchronized-output marks every tty frame.
    let body = if rs.color { o } else { strip_sgr(&o) };
    if tty {
        format!("{SYNC_BEGIN}{body}{SYNC_END}")
    } else {
        body
    }
}

/// (l2, l1-base-fee-per-gas, age, stale?) display strings from the gas oracle, read once.
fn gas_snapshot_strs(i: &DashboardInputs) -> (String, String, String, bool) {
    let g = i.gas.load();
    let gwei = |wei: u128| format!("{:.3}", wei as f64 / 1e9);
    let (age, stale) = if g.captured_at_ms > 0 {
        let a = now_ms().saturating_sub(g.captured_at_ms);
        (format!("{a}ms"), a > i.gas.max_age_ms)
    } else {
        ("—".to_string(), false)
    };
    (
        gwei(g.l2_gas_price_wei),
        gwei(g.l1_base_fee_per_gas_wei),
        age,
        stale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> DashboardInputs {
        let mk = |v: u64| Arc::new(AtomicU64::new(v));
        DashboardInputs {
            frames: mk(1_243_000),
            dirty: mk(842_100),
            candidates: mk(318),
            validated: mk(311),
            submitted: mk(7),
            reconnects: mk(0),
            seq_gaps: mk(0),
            gas: Arc::new(GasOracle::new(engine_gas::GasSnapshot::default(), 500)),
            latency: Arc::new(LatencyHistograms::new()),
            opts: OptSnapshot::from_env(),
            live: false,
            chain_id: 42_161,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn commas_groups() {
        assert_eq!(commas(343_912_884), "343,912,884");
        assert_eq!(commas(0), "0");
        assert_eq!(commas(1_000), "1,000");
    }

    #[test]
    fn progress_set_and_complete() {
        let p = StartupProgress::new();
        p.set(phase::DISCOVERY, 50, 200);
        assert!((p.phases[phase::DISCOVERY].fraction() - 0.25).abs() < 1e-9);
        p.complete(phase::DISCOVERY);
        assert!((p.phases[phase::DISCOVERY].fraction() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn strip_sgr_keeps_controls() {
        let s = format!("{MINT}hi{RST}\x1b[K");
        let stripped = strip_sgr(&s);
        assert_eq!(stripped, "hi\x1b[K");
    }

    #[test]
    fn opt_snapshot_has_chips() {
        let s = OptSnapshot::from_env();
        assert!(s.chips.iter().any(|(k, _)| *k == "metrics"));
        assert!(s
            .chips
            .iter()
            .any(|(k, v)| *k == "graviton" && v == "neoverse-v2"));
    }

    #[test]
    fn vis_width_ignores_escape_sequences() {
        assert_eq!(vis_width("abc"), 3);
        assert_eq!(vis_width(&format!("{MINT}ab{RST}")), 2);
        // Non-SGR CSI controls (cursor/clear) are zero-width too.
        assert_eq!(vis_width("\x1b[Kab\x1b[H"), 2);
        assert_eq!(vis_width("✓ Ξ"), 3);
    }

    #[test]
    fn pad_vis_pads_to_visible_width() {
        assert_eq!(pad_vis("ab", 6), "ab    ");
        let coloured = pad_vis(&format!("{MINT}ab{RST}"), 6);
        assert_eq!(vis_width(&coloured), 6);
        assert!(coloured.ends_with("    "));
        // Already-wide strings pass through.
        assert_eq!(pad_vis("abcdef", 3), "abcdef");
    }

    #[test]
    fn clip_vis_bounds_width_and_closes_colour() {
        let s = format!("{MINT}abcdef{RST}");
        let c = clip_vis(&s, 3);
        assert_eq!(vis_width(&c), 3);
        assert!(c.ends_with(RST), "clipped colour must be reset: {c:?}");
        // Fitting strings are returned unchanged (fast path).
        assert_eq!(clip_vis("short", 10), "short");
        assert_eq!(clip_vis(&s, 64), s);
    }

    #[test]
    fn event_rows_align_across_kinds() {
        // The Submit row embeds SGR colour in its "candidate" cell; the GasGated row is
        // plain. The "detail" column must start at the same VISIBLE offset in both.
        let submit = fmt_event(&Event {
            ts_ms: 0,
            size_wei: 4_210_000_000_000_000_000,
            gross_wei: 4_200_000_000_000_000,
            route_id: 2665,
            lat_ns: 2_900_000,
            slack_ms: 18,
            hops: 3,
            kind: EventKind::Submit,
        });
        let gated = fmt_event(&Event {
            ts_ms: 0,
            size_wei: 0,
            gross_wei: 0,
            route_id: 0,
            lat_ns: 0,
            slack_ms: 0,
            hops: 0,
            kind: EventKind::GasGated,
        });
        let detail_col = |row: &str, needle: &str| {
            let at = row.find(needle).expect("detail text present");
            vis_width(&row[..at])
        };
        assert_eq!(
            detail_col(&submit, "rx→sub"),
            detail_col(&gated, "submit held")
        );
    }

    #[test]
    fn pill_reads_real_kill_state() {
        let i = inputs();
        let k = Arc::new(KillSwitches::new());
        set_kill(k.clone());
        assert_eq!(derive_pill(&i).1, "NOMINAL");
        k.global.trip(1);
        assert_eq!(derive_pill(&i).1, "HALT");
        k.global.close(2);
        assert_eq!(derive_pill(&i).1, "NOMINAL");
        k.feed.trip(3);
        assert_eq!(derive_pill(&i).1, "HALT·feed");
        k.feed.close(4);
    }

    #[test]
    fn frame_height_fits_small_terminals() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = false;
        rs.cols = 80;
        rs.rows = 15;
        rs.detail = 1; // auto — height-gated detail rows (deterministic regardless of env)
        t.progress.enter_hot();
        for k in 0..20 {
            rs.tail.push_back(Event {
                ts_ms: k,
                size_wei: 0,
                gross_wei: 0,
                route_id: 0,
                lat_ns: 0,
                slack_ms: 0,
                hops: 0,
                kind: EventKind::DropOwnTx,
            });
        }
        let frame = render(&mut rs, t, &i);
        let lines = frame.matches("\r\n").count();
        // ≤ rows-1: the final line's newline must not scroll the screen.
        assert!(
            lines <= 14,
            "frame must fit a 15-row terminal: {lines} lines"
        );
    }

    #[test]
    fn render_cost_stays_far_below_frame_budget() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = true;
        rs.cols = 120;
        rs.rows = 40;
        t.progress.enter_hot();
        for k in 0..TAIL as u64 {
            rs.tail.push_back(Event {
                ts_ms: k,
                size_wei: 1_000_000_000_000_000_000,
                gross_wei: 9_000_000_000_000,
                route_id: 1,
                lat_ns: 1_000_000,
                slack_ms: 3,
                hops: 2,
                kind: EventKind::Submit,
            });
        }
        let t0 = Instant::now();
        let mut bytes = 0usize;
        for _ in 0..500 {
            bytes += render(&mut rs, t, &i).len();
        }
        let per_frame = t0.elapsed() / 500;
        // The 12 Hz budget is 83 ms/frame; rendering must be orders of magnitude cheaper.
        // 4 ms is ~20× headroom on a busy CI box — catches an accidental O(huge) regression
        // without ever being flaky.
        assert!(
            per_frame < Duration::from_millis(4),
            "render too slow: {per_frame:?}/frame"
        );
        assert!(bytes > 0);
    }

    #[test]
    fn frame_lines_never_exceed_terminal_width() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = true;
        rs.cols = 60;
        t.progress.enter_hot();
        rs.tail.push_back(Event {
            ts_ms: now_ms(),
            size_wei: 4_210_000_000_000_000_000,
            gross_wei: 4_200_000_000_000_000,
            route_id: 2665,
            lat_ns: 2_900_000,
            slack_ms: 18,
            hops: 3,
            kind: EventKind::Submit,
        });
        let frame = render(&mut rs, t, &i);
        for line in frame.split("\r\n") {
            // vis_width skips ALL CSI (incl. the \x1b[H / \x1b[K / sync controls).
            assert!(
                vis_width(line) <= 59,
                "line wider than cols-1 ({}): {line:?}",
                vis_width(line)
            );
        }
    }

    #[test]
    fn gas_row_labels_l1_base_fee_per_gas_not_byte_cost() {
        let t = init();
        let i = inputs();
        i.gas.store(engine_gas::GasSnapshot {
            l2_gas_price_wei: 20_000_000,
            l1_base_fee_per_gas_wei: 1_000_000_000,
            l1_gas_per_data_byte: 16,
            captured_at_ms: 0,
            ..engine_gas::GasSnapshot::default()
        });
        let (l2, l1, _, _) = gas_snapshot_strs(&i);
        assert_eq!(l2, "0.020");
        assert_eq!(l1, "1.000");

        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = false;
        rs.cols = 200;
        rs.rows = 40;
        t.progress.enter_hot();
        let frame = render(&mut rs, t, &i);
        let gas = frame
            .split("\r\n")
            .find(|line| line.contains("gas") && line.contains("l1"))
            .expect("gas row present");
        assert!(
            gas.contains("l1 1.000 gwei/gas"),
            "l1 base fee must be labeled per gas, not per byte: {gas:?}"
        );
        assert!(
            !gas.contains("gwei/B"),
            "raw l1_base_fee_per_gas_wei must not be labeled as byte cost: {gas:?}"
        );
    }

    // NO_COLOR=1 cargo test -p engine --lib dashboard::tests::preview -- --nocapture
    #[test]
    fn preview() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        // boot view
        t.progress.set(phase::DISCOVERY, 6412, 6412);
        t.progress.set(phase::SNAPSHOT, 3120, 6412);
        t.progress
            .set_status("snapshot BalancerV2 · multicall3 · 3,120 validated");
        println!("\n===== BOOT VIEW =====\n{}", render(&mut rs, t, &i));
        // live view — wide + tall + detail forced so the full instrument cluster is visible.
        rs.cols = 130;
        rs.rows = 30;
        rs.detail = 2;
        t.progress.enter_hot();
        for _ in 0..1000 {
            i.latency.record(StageLatency::Classify, 3_200);
            i.latency.record(StageLatency::Sim, 740_000);
            i.latency.record(StageLatency::Revm, 41_000_000);
            i.latency.record(StageLatency::Submit, 250_000);
            i.latency.record(StageLatency::EndToEnd, 2_900_000);
            i.latency.record(StageLatency::FeedLatency, 610_000);
            i.latency.record(StageLatency::RpcLatency, 38_000_000);
        }
        // Seed the sparkline history (the 1 s tick can't fire inside a single render call).
        for k in 0..SPARK_W {
            push_hist(&mut rs.feed_hist, 120.0 + (k as f64 * 17.0) % 90.0);
            push_hist(&mut rs.gross_hist, (k as f64 * 0.0007).sin().abs() * 0.005);
        }
        rs.tail.push_back(Event {
            ts_ms: now_ms(),
            size_wei: 4_210_000_000_000_000_000,
            gross_wei: 4_200_000_000_000_000,
            route_id: 2665,
            lat_ns: 2_900_000,
            slack_ms: 18,
            hops: 3,
            kind: EventKind::Submit,
        });
        rs.tail.push_back(Event {
            ts_ms: now_ms(),
            size_wei: 0,
            gross_wei: 0,
            route_id: 0,
            lat_ns: 0,
            slack_ms: 0,
            hops: 0,
            kind: EventKind::DropOwnTx,
        });
        rs.tail.push_back(Event {
            ts_ms: now_ms(),
            size_wei: 0,
            gross_wei: 0,
            route_id: 0,
            lat_ns: 0,
            slack_ms: 0,
            hops: 0,
            kind: EventKind::GasGated,
        });
        println!("\n===== LIVE VIEW =====\n{}", render(&mut rs, t, &i));
    }

    #[test]
    fn spark_is_width_bounded_and_handles_edges() {
        let mut h: std::collections::VecDeque<f64> = std::collections::VecDeque::new();
        assert_eq!(spark(&h, 8), "", "empty history → empty sparkline");
        for v in [1.0, 2.0, 3.0, 8.0, 4.0, 2.0] {
            h.push_back(v);
        }
        let s = spark(&h, 8);
        assert_eq!(vis_width(&s), 6, "one glyph per sample, ≤ width");
        assert!(s.chars().all(|c| SPARK_BARS.contains(&c)));
        assert!(s.contains(SPARK_BARS[0]), "min sample → bottom bar");
        assert!(s.contains(SPARK_BARS[7]), "max sample → top bar");
        // Width clamps to the most recent `width` samples.
        assert_eq!(vis_width(&spark(&h, 3)), 3);
        // A flat series renders a visible mid bar (steady ≠ no-data), never empty.
        let flat: std::collections::VecDeque<f64> = [5.0, 5.0, 5.0].into_iter().collect();
        let sf = spark(&flat, 8);
        assert_eq!(sf.chars().count(), 3);
        assert!(sf.chars().all(|c| c == SPARK_BARS[3]));
    }

    #[test]
    fn lat_col_threshold_mapping() {
        assert_eq!(lat_col(None, 10, 20), FNT, "no samples → faint");
        assert_eq!(lat_col(Some(5), 10, 20), MINT, "below warn → ok");
        assert_eq!(lat_col(Some(10), 10, 20), AMBER, "at warn → warn");
        assert_eq!(lat_col(Some(25), 10, 20), RED, "at/over bad → bad");
    }

    #[test]
    fn feed_event_age_is_rendered_in_seconds_not_nanoseconds() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = false;
        rs.cols = 200;
        rs.rows = 40;
        t.progress.enter_hot();
        i.latency.record(StageLatency::FeedLatency, 1);

        let frame = render(&mut rs, t, &i);
        let feed = frame
            .split("\r\n")
            .find(|line| line.contains("seq→rx"))
            .expect("feed row present");
        assert!(
            feed.contains("seq→rx 1s"),
            "integer-second feed_event_age must retain its seconds unit: {feed:?}"
        );
        assert!(
            !feed.contains("µs") && !feed.contains("ms"),
            "feed_event_age must never be passed through the nanosecond formatter: {feed:?}"
        );
        assert_eq!(
            lat_col(Some(1), 1, 2),
            AMBER,
            "a nonzero one-second coarse age must be visibly non-green"
        );
    }

    #[test]
    fn pipe_row_surfaces_submit_and_e2e_band() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = false; // strip SGR so we can assert on plain label/value text
        rs.cols = 200;
        rs.rows = 40;
        t.progress.enter_hot();
        for _ in 0..1000 {
            i.latency.record(StageLatency::Submit, 250_000);
            i.latency.record(StageLatency::EndToEnd, 2_900_000);
        }
        let frame = render(&mut rs, t, &i);
        let pipe = frame
            .split("\r\n")
            .find(|l| l.contains("pipe"))
            .expect("pipe row present");
        // submit latency was tracked but never shown before — it must appear now.
        assert!(pipe.contains("submit"), "pipe row missing submit: {pipe:?}");
        // e2e renders as a p50·p99 band (two values joined by ·), not a bare p99.
        assert!(pipe.contains("e2e"), "pipe row missing e2e: {pipe:?}");
        assert!(
            pipe.matches('·').count() >= 1,
            "e2e band must show p50·p99: {pipe:?}"
        );
    }

    #[test]
    fn detail_rows_reveal_hidden_telemetry() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = false;
        rs.cols = 200;
        rs.rows = 40;
        rs.detail = 2; // force all extra rows
        t.progress.enter_hot();
        let frame = render(&mut rs, t, &i);
        for needle in [
            "fastlane",
            "promo",
            "agree", // fast-lane shadow + promotion evidence
            "cold",
            "repin",
            "suspect", // REVM/RPC integrity (cold hot-path RPC alarm)
            "sniff",
            "pred-fail", // classifier health
        ] {
            assert!(
                frame.contains(needle),
                "detail rows must surface `{needle}`"
            );
        }
        // Compact mode suppresses them entirely (original minimal view).
        rs.detail = 0;
        let compact = render(&mut rs, t, &i);
        assert!(!compact.contains("fastlane"), "detail=0 must hide extras");
    }

    #[test]
    fn frame_fits_80x24_with_detail_forced() {
        let t = init();
        let i = inputs();
        let mut rs = RenderState::new();
        rs.tty = true;
        rs.color = true;
        rs.cols = 80;
        rs.rows = 24;
        rs.detail = 2; // force extras even on the small/standard terminal
        t.progress.enter_hot();
        for k in 0..20 {
            rs.tail.push_back(Event {
                ts_ms: k,
                size_wei: 0,
                gross_wei: 0,
                route_id: 0,
                lat_ns: 0,
                slack_ms: 0,
                hops: 0,
                kind: EventKind::DropOwnTx,
            });
        }
        let frame = render(&mut rs, t, &i);
        let lines = frame.matches("\r\n").count();
        assert!(lines <= 23, "80×24 frame must not scroll: {lines} lines");
        // …and the forced detail rows still made it in (height-gated, tail absorbs the rest).
        assert!(frame.contains("fastlane"), "forced detail must render");
        for line in frame.split("\r\n") {
            assert!(vis_width(line) <= 79, "line wider than cols-1: {line:?}");
        }
    }
}
