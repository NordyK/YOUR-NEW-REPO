//! Engine binary entrypoint.
//!
//! Boots the pinned-thread topology described in §2:
//! * vCPU 0 — background control plane (shadow/resync/logsub/bg)
//! * vCPU 1 — relay RX (binary UDS or WebSocket)
//! * vCPU 2 — parser
//! * vCPU 3 — sim (+ helper shards on 4)
//! * vCPU 5 — state-writer
//! * vCPU 6 — REVM validator
//! * vCPU 7 — submitter
//!
//! All inter-stage queues are SPSC rings (`rtrb`). The binary takes a config path from the
//! environment (`ENGINE_CONFIG`, default `./engine.json`) and exposes subcommands for
//! `replay` and `dry-run`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use mimalloc::MiMalloc;
use tracing::info;

use engine_config::EngineConfig;
use engine_gas::{GasOracle, GasSnapshot};
use engine_metrics::{LatencyHistograms, ReasonRing};
use engine_replay::{FeedFrameSource, ReplayDriver};

/// Use mimalloc (per §25.10).
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Pipeline module is exposed via the library crate (`src/lib.rs`). The binary references
// it through the library so integration tests and benches can share the same surface.
use engine::pipeline;

fn main() -> Result<()> {
    // Install the rustls CryptoProvider FIRST — before any feed/submit ClientConfig::builder() runs.
    // aws-lc-rs (workspace) AND ring (via tungstenite) are both compiled; rustls cannot auto-select,
    // so the builder panics unless we pick the provider explicitly up front.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    load_dotenv();
    let args: Vec<String> = env::args().collect();
    let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("dry-run");
    // `latency-probe` promises one machine-readable JSON object on stdout. Select its tracing sink
    // before the subscriber is installed so startup and connector diagnostics cannot corrupt it.
    install_tracing(subcmd == "latency-probe");
    install_panic_hook();
    apply_metrics_toggle();
    match subcmd {
        "replay" => run_replay(args.get(2).map(PathBuf::from)),
        "dry-run" => run_dry_run(),
        "latency-probe" => engine::latency_probe::run(),
        "reconcile-paper" => engine::paper_reconcile_cli::run(args.get(2).map(PathBuf::from)),
        "version" => {
            println!("engine v{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            eprintln!(
                "usage: engine [version|replay <feed.jsonl>|dry-run|latency-probe|reconcile-paper <paper.jsonl>]"
            );
            Ok(())
        }
    }
}

/// Minimal `.env` loader (no dependency): reads `KEY=VALUE` lines from `.env` (or
/// `$DOTENV_PATH`) and sets any variable not already present in the environment, so an
/// explicit shell `export` always wins over the file. Lines starting with `#` and blank
/// lines are ignored; surrounding quotes on values are stripped.
fn load_dotenv() {
    let path = env::var("DOTENV_PATH").unwrap_or_else(|_| ".env".to_string());
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.is_empty() && env::var(k).is_err() {
                env::set_var(k, v);
            }
        }
    }
    // Runs before the tracing subscriber is installed (so .env can set RUST_LOG), hence
    // eprintln rather than the tracing macros.
    eprintln!("loaded .env from {path}");
}

/// Install a panic hook that logs the panicking thread, source location and message via
/// `tracing` BEFORE the default hook runs. Under the release `panic = "abort"` profile a panic
/// anywhere takes the whole process down, so without this the death is silent — operators get a
/// bare abort with no cause. The named `ENGINE_PANIC` event makes every crash diagnosable and
/// greppable; the external supervisor then restarts the process for 24/7 operation.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic payload>");
        tracing::error!(
            target: "panic",
            thread = name,
            location = %loc,
            message = msg,
            "ENGINE_PANIC: thread panicked (process aborts under panic=abort; supervisor must restart)"
        );
        default(info);
    }));
}

/// Apply the `METRICS_ENABLED` operator toggle once at startup. Default ON; `0`/`false`/`off`
/// switch latency recording (and gated per-opportunity log formatting) off so the hot path pays
/// nothing for observability on the latency-critical box. Any other value leaves it ON.
fn apply_metrics_toggle() {
    let on = match env::var("METRICS_ENABLED")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
    {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no"),
        None => true,
    };
    engine_metrics::set_metrics_enabled(on);
    info!(metrics_enabled = on, "observability toggle applied");
}

/// Non-blocking, dependency-free log sink: tracing-subscriber writes each event into a `LineSink`,
/// which forwards complete lines over a bounded channel to a background writer thread. The hot
/// path NEVER blocks on file I/O — `try_send` drops a line if the (large) queue is full rather than
/// stalling the submit thread, so logging the per-opportunity OPP_LATENCY line costs ~nothing.
struct LineSink {
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    buf: Vec<u8>,
}
impl std::io::Write for LineSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            // best-effort: never block the producing (possibly hot) thread.
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
        Ok(())
    }
}
impl Drop for LineSink {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.flush();
    }
}

/// Resolve the tracing directive string from the environment (which `.env` already populated —
/// `load_dotenv()` runs first). Precedence: `RUST_LOG` (full per-target directive syntax, e.g.
/// `info,engine::pipeline=debug,diag=debug`) wins; else the simple `LOG_LEVEL` knob
/// (`trace|debug|info|warn|error|off`); else `info`. Pure logic in
/// [`resolve_log_directives_from`] so it is unit-testable without env mutation.
fn resolve_log_directives() -> String {
    resolve_log_directives_from(
        std::env::var("RUST_LOG").ok().as_deref(),
        std::env::var("LOG_LEVEL").ok().as_deref(),
    )
}

fn resolve_log_directives_from(rust_log: Option<&str>, log_level: Option<&str>) -> String {
    if let Some(v) = rust_log {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    if let Some(v) = log_level {
        let lv = v.trim().to_ascii_lowercase();
        if matches!(
            lv.as_str(),
            "trace" | "debug" | "info" | "warn" | "error" | "off"
        ) {
            return lv;
        }
        if !lv.is_empty() {
            eprintln!(
                "[log] LOG_LEVEL='{v}' invalid (trace|debug|info|warn|error|off); using info"
            );
        }
    }
    "info".to_string()
}

/// True for `1|true|on|yes` (the engine's usual opt-in convention).
fn env_opt_in(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref().map(str::trim),
        Some("1") | Some("true") | Some("on") | Some("yes")
    )
}

fn install_tracing(machine_stdout_only: bool) {
    use tracing_subscriber::EnvFilter;
    // Level: RUST_LOG > LOG_LEVEL > info — all settable from ./.env (loaded before this runs).
    // The difference between modes is the SINK and the FORMAT, never the level.
    let mut directives = resolve_log_directives();
    // WsRelayClient's normal connected event includes its full URL. A probe endpoint may carry a
    // credential in its query, so suppress that target and emit only the probe's redacted generic
    // feed status while machine-output mode is active.
    if machine_stdout_only {
        directives.push_str(",engine_feed::relay_client=off");
    }
    let fallback_directives = if machine_stdout_only {
        "info,engine_feed::relay_client=off"
    } else {
        "info"
    };
    let filter = || {
        EnvFilter::try_new(&directives).unwrap_or_else(|e| {
            eprintln!("[log] invalid log directives '{directives}' ({e}); using info");
            EnvFilter::new(fallback_directives)
        })
    };
    // LOG_FORMAT=json → one JSON object per line (jq/CloudWatch-friendly; includes `target`,
    // so `jq 'select(.target=="diag")'` isolates the classified diagnostics). Default: text.
    let json = std::env::var("LOG_FORMAT")
        .map(|v| v.trim().eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    // Sink: the dashboard owns the terminal → file. LOG_TO_FILE=1 forces the file sink without
    // the dashboard (e.g. systemd on AWS writing to LOG_DIR instead of the journal).
    let dash = engine::dashboard::enabled();
    let to_file = dash || env_opt_in("LOG_TO_FILE");

    if to_file {
        // Route ALL logs to a file (full fidelity, tail-able) via the non-blocking LineSink —
        // a bounded channel + a dedicated writer thread, so a slow disk can never stall a
        // producing (possibly hot) thread: `try_send` drops on overflow instead of blocking.
        let dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".to_string());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/engine.log");
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(65_536);
                std::thread::Builder::new()
                    .name("engine-logwriter".into())
                    .spawn(move || {
                        while let Ok(chunk) = rx.recv() {
                            let _ = file.write_all(&chunk);
                            // Drain any backlog before flushing to amortise fsync-free flushes.
                            while let Ok(more) = rx.try_recv() {
                                let _ = file.write_all(&more);
                            }
                            let _ = file.flush();
                        }
                    })
                    .ok();
                let mk = move || LineSink {
                    tx: tx.clone(),
                    buf: Vec::with_capacity(256),
                };
                if json {
                    let _ = tracing_subscriber::fmt()
                        .json()
                        .with_env_filter(filter())
                        .with_writer(mk)
                        .try_init();
                } else {
                    let _ = tracing_subscriber::fmt()
                        .with_env_filter(filter())
                        .with_target(false)
                        .with_ansi(false)
                        .with_writer(mk)
                        .try_init();
                }
                let why = if dash { "dashboard" } else { "LOG_TO_FILE" };
                eprintln!("[{why}] logs → {path} (level '{directives}'; tail -f for the stream)");
            }
            Err(e) => {
                // The latency probe reserves stdout for its single JSON artifact even when the
                // requested file sink fails. Other subcommands retain the established stdout
                // fallback so their logging behavior is unchanged.
                if machine_stdout_only {
                    eprintln!("[log] could not open {path} ({e}); logging to stderr");
                    let _ = tracing_subscriber::fmt()
                        .with_env_filter(filter())
                        .with_target(false)
                        .with_writer(std::io::stderr)
                        .try_init();
                } else {
                    eprintln!("[log] could not open {path} ({e}); logging to stdout");
                    let _ = tracing_subscriber::fmt()
                        .with_env_filter(filter())
                        .with_target(false)
                        .try_init();
                }
            }
        }
    } else if json {
        if machine_stdout_only {
            let _ = tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter())
                .with_writer(std::io::stderr)
                .try_init();
        } else {
            let _ = tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter())
                .try_init();
        }
    } else {
        if machine_stdout_only {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_target(false)
                .with_writer(std::io::stderr)
                .try_init();
        } else {
            // No dashboard → logs to stdout as usual.
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_target(false)
                .try_init();
        }
    }
}

fn load_config() -> Result<EngineConfig> {
    let path = env::var("ENGINE_CONFIG").unwrap_or_else(|_| "./engine.json".to_string());
    let mut cfg = if PathBuf::from(&path).exists() {
        EngineConfig::load(&path).with_context(|| format!("loading config {path}"))?
    } else {
        info!("config file {path} not found; using built-in Arbitrum One defaults");
        EngineConfig::default_arbitrum_one()
    };
    // Operator one-knob hop-depth override: `MAX_HOPS=N` enables tiers 2..=N and disables
    // tiers above N (clamped to [2, 8]). Lets us "swap to 8 or any number" per run without
    // editing the per-tier topology flags in engine.json. Unset → config/defaults stand.
    if let Some(raw) = env::var("MAX_HOPS").ok().filter(|s| !s.trim().is_empty()) {
        match raw.trim().parse::<u8>() {
            Ok(n) => {
                cfg.topology.apply_max_hops(n);
                info!(max_hops = n, "MAX_HOPS override applied to topology tiers");
            }
            Err(_) => {
                tracing::warn!(value = %raw, "MAX_HOPS is not a valid integer; ignoring (config/defaults stand)");
            }
        }
    }
    // Operator knob for how many pools to monitor: `HOT_POOL_CAP` caps the resident set to the
    // top-N by score. Pair with `POOL_NOTIONAL_FLOOR_USD` (lower it so ≥N pools survive the
    // floor, or the cap can't bind). Unset → engine.json value stands.
    if let Some(raw) = env::var("HOT_POOL_CAP")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        match raw.trim().parse::<usize>() {
            Ok(n) if n > 0 => {
                cfg.discovery.hot_pool_cap = n;
                info!(
                    hot_pool_cap = n,
                    "HOT_POOL_CAP override applied (resident pool count cap)"
                );
            }
            _ => {
                tracing::warn!(value = %raw, "HOT_POOL_CAP is not a positive integer; ignoring (config stands)");
            }
        }
    }
    // Offline-replay snapshot pin: `ENGINE_SNAPSHOT_BLOCK=N` pins discovery's head block to N so
    // the cached pool set is snapshotted at exactly that historical block (Alchemy archive). Pair
    // with a `file:` corpus of block N+1.. transactions so engine state and the replayed feed are
    // aligned, as in production. Unset → live head (current chain). See run_dry_run for the
    // matching snapshot `BlockTag`.
    if let Some(raw) = env::var("ENGINE_SNAPSHOT_BLOCK")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        match raw.trim().parse::<u64>() {
            Ok(n) => {
                cfg.discovery.head_block_override = Some(n);
                info!(
                    snapshot_block = n,
                    "ENGINE_SNAPSHOT_BLOCK pin applied (discovery head + state snapshot block)"
                );
            }
            Err(_) => {
                tracing::warn!(value = %raw, "ENGINE_SNAPSHOT_BLOCK is not a valid integer; ignoring")
            }
        }
    }
    // S2 transport override: `RELAY_ENDPOINT=binunix:/path.sock` selects the stream binary fast
    // path; default-off `binseq:/path.sock` selects message-preserving SOCK_SEQPACKET.
    // (feed sidecar / forked relay), `ws://…` the WebSocket path, a plain path the JSON-over-UDS
    // path — without editing engine.json. Unset → config value stands.
    if let Some(raw) = env::var("RELAY_ENDPOINT")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        info!(relay_endpoint = %raw, "RELAY_ENDPOINT override applied");
        cfg.relay_endpoint = raw.trim().to_string();
    }
    Ok(cfg)
}

/// Gate-0 startup identity (certification runbook §4): emit commit + config/universe hashes +
/// mode + key flags so every binary mode logs a comparable provenance block. Hashes are FNV-1a
/// over the serialized config sections — stable and dependency-free; identical across modes for
/// the same config, which is exactly the runbook's acceptance check.
fn print_startup_identity(cfg: &EngineConfig, mode: &str) -> Result<()> {
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
    let hx = |r: Result<Vec<u8>, _>| {
        r.map(|b| format!("{:016x}", fnv1a(&b)))
            .unwrap_or_else(|_| "err".into())
    };
    let env_flag = |k: &str, default_on: bool| {
        std::env::var(k)
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
            .unwrap_or(default_on)
    };
    // P0.5 provenance gate: a certification/box run must never produce results that cannot be
    // attributed to a code identity (Codex 2026-07-10: all 8 box startups had commit=unknown).
    let commit_hash = option_env!("ENGINE_GIT_COMMIT").unwrap_or("unknown");
    if commit_hash == "unknown"
        && std::env::var("REQUIRE_PROVENANCE")
            .map(|v| v.trim() == "1")
            .unwrap_or(false)
    {
        anyhow::bail!(
            "REQUIRE_PROVENANCE=1: refusing to run a binary with unknown commit identity —              build inside a git tree, or let deploy/aws/sync-to-host.sh write .engine_commit,              or set ENGINE_COMMIT_OVERRIDE at build time"
        );
    }
    info!(
        target: "engine::startup",
        commit_hash,
        build_tree = option_env!("ENGINE_GIT_DIRTY").unwrap_or("unknown"),
        config_hash = %hx(serde_json::to_vec(cfg)),
        tokens_hash = %hx(serde_json::to_vec(&cfg.tokens)),
        venues_hash = %hx(serde_json::to_vec(&cfg.venues)),
        routers_hash = %hx(serde_json::to_vec(&cfg.routers)),
        tokens = cfg.tokens.len(),
        venues = cfg.venues.len(),
        routers = cfg.routers.len(),
        mode,
        revm_validate = env_flag("REVM_VALIDATE", true),
        dry_run = env_flag("DRY_RUN", true),
        submit_tx = env_flag("SUBMIT_TX", false),
        "startup identity (Gate 0)"
    );
    Ok(())
}

fn validate_submit_switches(dry_run: bool, submit_tx: bool, config_live_submit: bool) -> Result<()> {
    if dry_run && submit_tx {
        anyhow::bail!(
            "contradictory submission policy: DRY_RUN=true cannot be combined with SUBMIT_TX=true"
        );
    }
    if submit_tx && !config_live_submit {
        anyhow::bail!(
            "SUBMIT_TX=true but config live_submit=false; refusing a silently non-broadcasting run"
        );
    }
    Ok(())
}

fn run_dry_run() -> Result<()> {
    let cfg = load_config()?;
    let dry_run = pipeline::safety_switch_from_env("DRY_RUN", true)?;
    let submit_tx = pipeline::safety_switch_from_env("SUBMIT_TX", false)?;
    validate_submit_switches(dry_run, submit_tx, cfg.live_submit)?;
    // [Codex-F3] In live-submit mode, validate the full submit config (executor/EOA/key shape, lanes,
    // venues) at boot BEFORE building anything — so a malformed live config fails fast instead of
    // silently falling back to a zero executor / dummy signing key in SubmitterCtx::from_config.
    if cfg.live_submit {
        cfg.validate_for_live()
            .context("live_submit=true but config failed live validation")?;
        info!("LIVE-SAFE: config passed validate_for_live (executor/EOA/key well-formed)");
    }
    print_startup_identity(&cfg, "dry-run")?;
    // WI-02: resolve the ONE immutable RuntimePolicy BEFORE discovery/manifest — a refusal
    // (unset live field, unknown token/mode, unproven family on a live profile) is a fatal
    // boot error, never a silent default.
    let runtime_policy = engine::runtime_policy::resolve_runtime_policy()
        .map_err(|e| anyhow::anyhow!("RuntimePolicy refused boot: {e}"))?;
    info!(
        active_families = ?runtime_policy.active_families.tokens(),
        proven_families = ?runtime_policy.proven_families.tokens(),
        revm_validation_mode = runtime_policy.revm_validation_mode.as_str(),
        live_submit_authority = runtime_policy.live_submit_authority.as_str(),
        "RuntimePolicy sealed (immutable for this boot)"
    );
    // [P0] Emit the network run manifest beside the dry-run output: ordering labels (Arbitrum One is
    // Timeboost normal-lane, NOT pure FCFS; ~200ms express priority) so latency numbers are read
    // against the right baseline. WI-02: the manifest binds the sealed policy hashes.
    // Best-effort — logs + returns the path, never blocks startup.
    let _ = engine::run_manifest::emit_beside_output_with_policy(&runtime_policy);
    // Print every effective threshold (all from .env) so the operator can confirm a config edit
    // landed — change .env, restart, watch this line. Single source of truth: see THRESHOLDS.md.
    pipeline::log_effective_thresholds();
    info!(
        chain_id = cfg.chain_id,
        relay = %cfg.relay_endpoint,
        sequencer = %cfg.sequencer_endpoint,
        tokens = cfg.tokens.len(),
        venues = cfg.venues.len(),
        lanes = cfg.lanes.len(),
        "engine starting in dry-run mode"
    );

    // §2 OS hardening (best-effort): raise RLIMIT_MEMLOCK (so the mlockall below can pin) +
    // RLIMIT_NOFILE, and ensure THP is enabled — BEFORE locking pages. No-op on the dev
    // sandbox (EPERM logged); realized on the AWS host with CAP_SYS_RESOURCE/IPC_LOCK.
    let _sysopt = engine_sysopt::apply_process_optimizations();
    engine_sysopt::build_cpu_plan(engine_sysopt::online_cores());
    // §7.3 hot-path runtime guards: pin every page into RAM before we start. Avoids
    // minor page-fault stalls (~5–20 µs each) on the first access to hot tables
    // during sim.
    let mlock_ok = engine::runtime::lock_pages_to_ram().is_ok();
    if !mlock_ok {
        tracing::warn!(
            "lock_pages_to_ram failed (CAP_IPC_LOCK?) — page-fault stalls possible on hot path"
        );
    }

    // Active-opts banner: one log line operators can grep to confirm every optimization is on by
    // default (no extra scripts). Reads the same env flags the pipeline reads, so the banner can
    // never drift out of sync with the actual runtime configuration.
    {
        let binary_feed =
            cfg.relay_endpoint.starts_with("binunix:") || cfg.relay_endpoint.starts_with("binseq:");
        let shadow_revm = pipeline::env_flag_default_on("SHADOW_REVM");
        let shadow_emit = pipeline::env_flag_default_on("SHADOW_EMIT");
        let revm_validate = pipeline::env_flag_default_on("REVM_VALIDATE");
        let metrics_on = std::env::var("METRICS_ENABLED")
            .ok()
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
            .unwrap_or(true);
        let rt_sched = std::env::var("SYSOPT_RT_SCHED")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
            .unwrap_or(false);
        info!(
            target: "engine::startup",
            mimalloc = true,
            mlockall = mlock_ok,
            core_pinning = true,
            rt_sched,
            metrics = metrics_on,
            shadow_revm,
            shadow_emit,
            revm_validate,
            binary_feed,
            "active opts (defaults-on; opt out per-flag via env)"
        );
        // (Earlier "binary feed bypasses parser_loop → no shadow" warning is gone: with V2 binary
        // frames the sidecar attaches the raw signed-tx envelope and `rx_loop_unix_binary` tees
        // it into the shadow queue. Both paths run together by default.)
    }
    // Gas-snapshot staleness ceiling. Read from .env (GAS_MAX_AGE_MS) so .env is the single
    // source of truth for every threshold; default 500ms. (Formerly engine.json thresholds.)
    let gas_max_age_ms: u64 = env::var("GAS_MAX_AGE_MS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(500);
    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), gas_max_age_ms));
    let metrics_hist = Arc::new(LatencyHistograms::new());
    let reasons = Arc::new(ReasonRing::new());

    // --- optional console dashboard (DASHBOARD=1) ---
    // Spawned BEFORE discovery so the init progress bars animate live. It reads the same
    // counter Arcs the pipeline threads increment (shared below) + the existing histograms,
    // on its own thread — zero hot-path cost. Off → not even spawned.
    use std::sync::atomic::AtomicU64;
    let dash = engine::dashboard::enabled();
    let dash_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut dash_handle: Option<std::thread::JoinHandle<()>> = None;
    let dash_counters: Option<[Arc<AtomicU64>; 7]> = if dash {
        // Initialize the global telemetry (Tier-1 progress + Tier-2 event ring). `startup.rs`
        // drives the progress bars via `dashboard::progress()`; the hot path emits notable
        // events via `dashboard::emit_*` (no-ops until this runs).
        engine::dashboard::init();
        // 7 shared counters: frames, dirty, candidates, validated, submitted, reconnects, seq_gaps.
        let c: [Arc<AtomicU64>; 7] = std::array::from_fn(|_| Arc::new(AtomicU64::new(0)));
        let inputs = engine::dashboard::DashboardInputs {
            frames: c[0].clone(),
            dirty: c[1].clone(),
            candidates: c[2].clone(),
            validated: c[3].clone(),
            submitted: c[4].clone(),
            reconnects: c[5].clone(),
            seq_gaps: c[6].clone(),
            gas: gas.clone(),
            latency: metrics_hist.clone(),
            opts: engine::dashboard::OptSnapshot::from_env(),
            live: cfg.live_submit,
            chain_id: cfg.chain_id,
            shutdown: dash_shutdown.clone(),
        };
        dash_handle = Some(
            engine::dashboard::spawn(inputs)
                .map_err(|e| anyhow::anyhow!("dashboard spawn: {e}"))?,
        );
        Some(c)
    } else {
        None
    };

    // E.3 wakeup widening: operator-supplied StrategyExecutor `index → pool` map (built from
    // live Swap-log correlation — DATA_CAPTURE §2; artifact at
    // research/strategy_executor/index_pool_map.csv). Unset/empty = the sniff stays a no-op.
    // A present-but-malformed file is a boot refusal: a wrong map silently wakes wrong pools.
    if let Some(path) = env::var("STRATEGY_EXECUTOR_MAP")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
    {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("STRATEGY_EXECUTOR_MAP {path}: read failed: {e}"))?;
        let map = pipeline::parse_strategy_executor_map(&text)
            .map_err(|e| anyhow::anyhow!("STRATEGY_EXECUTOR_MAP {path}: {e}"))?;
        info!(
            entries = map.len(),
            path = %path,
            "StrategyExecutor index→pool wakeup map installed"
        );
        pipeline::set_strategy_executor_index_map(map);
    }

    // Build the detection context from the config. `ARBITRUM_RPC_URL` is mandatory:
    // the startup orchestrator snapshots every discovered pool at `latest` via the
    // user's RPC endpoint. A failed build (or one that registers zero pools) is
    // FATAL — booting an empty `DetectionContext` would produce an engine that
    // looks alive (feed connected, heartbeats ticking) but can never detect an
    // arb, which is a dangerous silent no-op. Offline tests construct the empty
    // context directly via `build_detection_from_config(.., None, ..)`; the
    // dry-run / live binary refuses to.
    let rpc_endpoint = env::var("ARBITRUM_RPC_URL").ok();
    // G9: the SAME isolation predicate the pipeline uses (file: OR FORCE_ISOLATED_REPLAY) —
    // computed BEFORE any live worker spawns so a forced-isolation binunix replay never starts
    // the gas refresher / Fluid pollers (live RPC would contaminate the isolated leg).
    let bounded_file_replay = pipeline::is_isolated_replay(&cfg.relay_endpoint);
    // Snapshot block: pinned for offline replay (ENGINE_SNAPSHOT_BLOCK), else live head.
    let snapshot_tag = env::var("ENGINE_SNAPSHOT_BLOCK")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(engine_rpc::client::BlockTag::Number)
        .unwrap_or(engine_rpc::client::BlockTag::Latest);
    let (detection_ctx, state_writer_permit) =
        engine::startup::build_detection_from_config(
            &cfg,
            rpc_endpoint.as_deref(),
            snapshot_tag,
            &runtime_policy,
        )
        .map_err(|e| {
            engine::diag::report(
                engine::diag::Diag::BootNoPools,
                &format!("startup discovery failed: {e}"),
            );
            anyhow::anyhow!(
                "startup discovery failed: {e}. Set ARBITRUM_RPC_URL and ensure the \
                 RPC is reachable — the engine refuses to boot with no pools (silent no-op)."
            )
        })?;
    let detection = Arc::new(detection_ctx);
    let n_pools = detection.universe.pools.len();
    if n_pools == 0 {
        engine::diag::report(
            engine::diag::Diag::BootNoPools,
            "0 registered pools at boot",
        );
        anyhow::bail!(
            "startup produced 0 registered pools — refusing to run a no-op engine. \
             Check ARBITRUM_RPC_URL, discovery venue config, and the token allowlist."
        );
    }
    info!(pools = n_pools, "detection context built");
    if dash {
        // Universe readout for the live HUD header (display-only; set once).
        engine::dashboard::set_universe(n_pools as u64, detection.route_table.len() as u64);
    }

    // LIVE-SAFE boot gate: if we are going to BROADCAST (`live_submit`) with REVM validation on,
    // the REVM validator MUST be available. Otherwise the engine would silently fall back to the
    // `gross>0` gate (pipeline revm_loop) and submit phantom (net-negative) candidates with real
    // capital. Hard-fail at boot rather than degrade. Dry-run (`live_submit=false`) is unaffected —
    // it WANTS to observe even with gross-only. Opt out explicitly with `REVM_VALIDATE=0`.
    if cfg.live_submit && pipeline::env_flag_default_on("REVM_VALIDATE") {
        let rpc_ok = rpc_endpoint
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let artifact_ok = pipeline::mantis_init_code_available();
        if !rpc_ok || !artifact_ok {
            engine::diag::report(
                engine::diag::Diag::BootRevmOff,
                &format!("rpc_ok={rpc_ok} mantis_artifact_ok={artifact_ok}"),
            );
            anyhow::bail!(
                "LIVE-SAFE refuse-to-boot: live_submit=true with REVM_VALIDATE on, but the REVM \
                 validator is unavailable (rpc_ok={rpc_ok}, mantis_artifact_ok={artifact_ok}). \
                 Refusing to broadcast without the phantom-rejection gate. Fix ARBITRUM_RPC_URL / \
                 MANTIS_ARTIFACT, or set REVM_VALIDATE=0 to run gross-only (NEVER for live capital)."
            );
        }
        info!("LIVE-SAFE: live_submit + REVM_VALIDATE — validator prerequisites present");
    }

    // LIVE-SAFE threshold lint: refuse to broadcast real capital with a loosened (observe-mode)
    // safety threshold left in .env — specifically a gas-spike cushion < 1.5× or confidence < 0.95.
    // No-op for dry-run / observe mode. Override with ALLOW_LOOSE_THRESHOLDS=1 (logged at ERROR).
    // LIVE-SAFE boot gate (capital mode, Codex #6): a real broadcast with REVM_VALIDATE=0 collapses
    // the accept rule to the gross>0 branch — bypassing the net-gate + fetch-error fail-closed +
    // singleton-offline guards. Hard-fail rather than trade gross-only with real capital. The earlier
    // gate covers REVM-on-but-unavailable; this covers the explicit REVM-off opt-out.
    if pipeline::is_gross_only_capital_broadcast(
        cfg.live_submit,
        submit_tx,
        pipeline::env_flag_default_on("REVM_VALIDATE"),
    ) {
        engine::diag::report(
            engine::diag::Diag::BootRevmOff,
            "live_submit + SUBMIT_TX with REVM_VALIDATE=0 (gross-only) — refusing capital broadcast",
        );
        anyhow::bail!(
            "LIVE-SAFE refuse-to-boot: live_submit=true and SUBMIT_TX=true with REVM_VALIDATE=0. \
             Gross-only acceptance must never broadcast real capital — the net-gate, fetch-error \
             fail-closed, and singleton-offline guards all require REVM validation. Set \
             REVM_VALIDATE=1 for any capital run, or keep SUBMIT_TX=false / live_submit=false."
        );
    }
    pipeline::enforce_live_safe_thresholds(cfg.live_submit, submit_tx).inspect_err(|e| {
        engine::diag::report(engine::diag::Diag::BootThresholds, &e.to_string());
    })?;

    // §6.2 background scorer thread. Re-ranks the pool table every
    // `discovery.score_interval_secs` minutes against live telemetry. Owns its own
    // shutdown handle so the engine can join it cleanly on Ctrl+C.
    let scorer_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let scorer_handle = engine::scorer::spawn_scorer(
        Arc::clone(&detection),
        cfg.discovery.score_interval_secs,
        Arc::clone(&scorer_shutdown),
    )
    .map_err(|e| anyhow::anyhow!("could not spawn scorer thread: {e}"))?;
    info!(
        interval_secs = cfg.discovery.score_interval_secs,
        "§6.2 background scorer started"
    );

    // §13 background gas / L1-data-fee oracle refresh. Polls every 250 ms (REFRESH_INTERVAL_MS,
    // under the GAS_MAX_AGE_MS=500 ceiling). Without this every EV decision uses
    // GasSnapshot::default() (l1_price_per_byte = 0), silently over-estimating net profit.
    let gas_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gas_handle = if bounded_file_replay {
        info!("bounded file: replay — live gas refresher skipped");
        None
    } else if let Some(rpc) = rpc_endpoint.clone() {
        let h = engine::gas_refresher::spawn_gas_refresher(
            rpc,
            Arc::clone(&gas),
            Arc::clone(&gas_shutdown),
        )
        .map_err(|e| anyhow::anyhow!("could not spawn gas refresher: {e}"))?;
        info!("§13 background gas refresher started");
        Some(h)
    } else {
        info!("ARBITRUM_RPC_URL absent — gas oracle frozen at default (l2=0.02 gwei, l1=0)");
        None
    };

    // §6/§5.4 background Fluid snapshot poller. [Codex-F8] SPAWNED BELOW (after the pipeline is built)
    // so it can take the pipeline's FluidWakeHandle — Fluid `LogOperate` is off the main WSS feed, so
    // the poller is the only thing that wakes a Fluid pool's routes after it refreshes the snapshot.
    // `detection` is cloned here because `Pipeline::new` moves it.
    let detection_poller = Arc::clone(&detection);
    let fluid_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // WI-06 §3.3: the sole publisher over the SAME `Arc<Universe>` the readers hold. The state
    // thread (spawned by `start_threads`) owns the publisher + permit and is the ONLY writer; the
    // off-thread writers (resync / WSS log / Fluid poller / shadow-REVM) route their mutations
    // through send-only command senders. Queue depth bounds the off-thread backlog before a
    // full/rejected submit atomically disarms authority (never a silent drop). Built here, before
    // `Pipeline::new` moves `detection`, so it shares the detection universe Arc.
    const STATE_COMMAND_QUEUE_DEPTH: usize = 8192;
    let state_publisher = engine_state::publication::StatePublisher::new(
        Arc::clone(&detection.universe),
        detection.route_table.len(),
        STATE_COMMAND_QUEUE_DEPTH,
    );
    // A sender for the externally-spawned Fluid poller (spawned AFTER start_threads, below).
    let fluid_command_sender = state_publisher.command_sender();

    // Wire the full pipeline. In dry-run, the submitter writes to /dev/null so the engine
    // exercises every hot stage without touching the sequencer.
    let mut pipeline =
        pipeline::Pipeline::new(cfg.clone(), gas.clone(), metrics_hist, reasons, detection);
    // Share the dashboard's counter Arcs with the pipeline (the threads increment these exact
    // Arcs; the dashboard reads them) — must happen before start_threads clones them in.
    if dash {
        // Hand the HUD the pipeline's REAL kill switches so the safety pill shows the actual
        // halt state of the submit path (display-only reader).
        engine::dashboard::set_kill(pipeline.kill.clone());
    }
    if let Some(c) = &dash_counters {
        pipeline.frames_processed = c[0].clone();
        pipeline.dirty_events = c[1].clone();
        pipeline.candidates = c[2].clone();
        pipeline.validated = c[3].clone();
        pipeline.submitted = c[4].clone();
        pipeline.relay_reconnects = c[5].clone();
        pipeline.relay_sequence_gaps = c[6].clone();
    }
    // WI-06 §3.3: hand the sealed permit + publisher to the pipeline; `start_threads` moves them
    // into the sole `engine-state-writer` thread and mints the off-thread writers' command senders.
    pipeline.attach_state_publication(state_writer_permit, state_publisher);
    // B3: enable the WARMUP readiness gate for the live binary (tests/replay leave it off). Live
    // RX ingestion will wait — timeout-bounded — until REVM is pre-warmed and gas is fresh.
    pipeline.enable_warmup_gate(true);
    pipeline.start_threads()?;
    // §6/§5.4 background Fluid snapshot poller — event-driven LogOperate (500 ms) + periodic safety-net
    // full refresh (5 min). [Codex-F8] Spawned HERE so it can wake route evaluation via the pipeline's
    // FluidWakeHandle (Fluid is off the main WSS feed). No-op when no Fluid pools are registered.
    let fluid_handle = if bounded_file_replay {
        info!("bounded file: replay — live Fluid poller skipped");
        None
    } else if !engine::runtime_policy::fluid_workers_allowed(&runtime_policy) {
        // WI-02 boundary 2 (worker construction): the Fluid shared-state pollers never
        // start when FluidDex is inactive under the boot policy.
        info!("FluidDex INACTIVE under the boot RuntimePolicy — Fluid poller not started");
        None
    } else if let Some(rpc) = rpc_endpoint.clone() {
        let v1 = engine::fluid_poller::parse_resolver_addr(&cfg.discovery.fluid_v1_resolver);
        let v2 = engine::fluid_poller::parse_resolver_addr(&cfg.discovery.fluid_v2_resolver);
        let ll = engine::fluid_poller::parse_resolver_addr(&cfg.discovery.fluid_liquidity_layer);
        if v1.is_some() || v2.is_some() {
            let h = engine::fluid_poller::spawn_fluid_poller(
                rpc,
                detection_poller,
                v1,
                v2,
                ll,
                // WI-06 §3.3 T3: the poller routes refreshes through the sole publisher (Fluid
                // origin); the state-thread drain bumps + emits the DirtyEvent (commit-before-wake),
                // replacing the old FluidWakeHandle.
                Some(fluid_command_sender),
                fluid_shutdown.clone(),
            )
            .map_err(|e| anyhow::anyhow!("could not spawn fluid poller: {e}"))?;
            info!(
                ?ll,
                "Fluid snapshot poller started (event-driven + periodic, wake-wired)"
            );
            Some(h)
        } else {
            info!("no Fluid resolvers configured — snapshot poller skipped");
            None
        }
    } else {
        info!("ARBITRUM_RPC_URL absent — Fluid poller skipped");
        None
    };
    if dash {
        if let Some(p) = engine::dashboard::progress() {
            p.enter_hot(); // switch the dashboard from init bars to the live hot-path view
        }
    }
    info!("dry-run pipeline live; press Ctrl+C to exit");
    // Capture the wait result so we always tear down cleanly (incl. on a watchdog Err) before
    // propagating it.
    let wait_result = pipeline.wait_for_shutdown_signal();
    dash_shutdown.store(true, std::sync::atomic::Ordering::Release);
    scorer_shutdown.store(true, std::sync::atomic::Ordering::Release);
    gas_shutdown.store(true, std::sync::atomic::Ordering::Release);
    fluid_shutdown.store(true, std::sync::atomic::Ordering::Release);
    let _ = scorer_handle.join();
    if let Some(h) = gas_handle {
        let _ = h.join();
    }
    if let Some(h) = fluid_handle {
        let _ = h.join();
    }
    if let Some(h) = dash_handle {
        let _ = h.join();
    }
    wait_result
}

fn run_replay(path: Option<PathBuf>) -> Result<()> {
    let p = path.context("replay <feed.jsonl> required")?;
    if let Ok(cfg) = load_config() {
        print_startup_identity(&cfg, "replay")?;
    }
    info!("replaying corpus {:?}", p);
    let mut driver = ReplayDriver::new();
    let src = FeedFrameSource::open(&p).with_context(|| format!("opening {p:?}"))?;
    let timing_path = env::var("REPLAY_EVENT_TIMINGS_JSONL").ok();
    let timing_requested = replay_timing_enabled(
        env::var("NETWORK_FINE").ok().as_deref(),
        timing_path.as_deref(),
    );
    let mut timing_out = if timing_requested {
        let path = PathBuf::from(timing_path.as_deref().expect("checked path"));
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating replay timing directory {parent:?}"))?;
        }
        Some(std::io::BufWriter::new(
            std::fs::File::create(&path)
                .with_context(|| format!("creating replay timing JSONL {path:?}"))?,
        ))
    } else {
        None
    };
    for frame in src {
        if let Some(out) = timing_out.as_mut() {
            match driver.process_timed(&frame) {
                Ok(timing) => write_replay_timing_row(
                    out,
                    frame.index,
                    Some(driver.stats().last_sequence),
                    Some(timing),
                    None,
                )?,
                Err(e) => {
                    write_replay_timing_row(
                        out,
                        frame.index,
                        Some(driver.stats().last_sequence),
                        None,
                        Some(&e.to_string()),
                    )?;
                    tracing::warn!("frame {} dropped: {e}", frame.index);
                }
            }
        } else {
            match driver.process(&frame) {
                Ok(()) => {}
                Err(e) => tracing::warn!("frame {} dropped: {e}", frame.index),
            }
        }
    }
    if let Some(out) = timing_out.as_mut() {
        use std::io::Write as _;
        out.flush().context("flushing replay timing JSONL")?;
    }
    let s = driver.stats();
    println!("frames_seen      = {}", s.frames_seen);
    println!("frames_parsed    = {}", s.frames_parsed);
    println!("signed_tx_count  = {}", s.signed_tx_count);
    println!("batch_tx_count   = {}", s.batch_tx_count);
    println!("sequence_gaps    = {}", s.sequence_gaps);
    println!("last_sequence    = {}", s.last_sequence);
    println!(
        "unsupported_non_swap_messages = {}",
        s.unsupported_non_swap_messages
    );
    println!(
        "unsupported_envelope_kind_drops = {}",
        s.unsupported_envelope_kind_drops
    );
    println!("parser_bug_drops = {}", s.parser_bug_drops);
    println!("relevant_swap_drops = {}", s.relevant_swap_drops);
    println!("unknown_drops    = {}", s.unknown_drops);
    Ok(())
}

fn replay_timing_enabled(network_fine: Option<&str>, output_path: Option<&str>) -> bool {
    engine_feed::parse_enabled_flag(network_fine)
        && output_path.is_some_and(|path| !path.trim().is_empty())
}

fn write_replay_timing_row(
    out: &mut dyn std::io::Write,
    frame_index: u64,
    sequence: Option<u64>,
    timing: Option<engine_replay::ReplayFrameTiming>,
    error: Option<&str>,
) -> Result<()> {
    let stage_deltas = timing.map(|timing| {
        serde_json::json!({
            "envelope_decode": timing.envelope_done_ns.saturating_sub(timing.start_ns),
            "base64_decode": timing.base64_done_ns.saturating_sub(timing.envelope_done_ns),
            "l2_decode": timing.l2_decode_done_ns.saturating_sub(timing.base64_done_ns),
            "replay_decode_total": timing.total_ns(),
        })
    });
    let row = serde_json::json!({
        "schema": "mantis.flag-harness.replay-event.v1",
        "frame_index": frame_index,
        "sequence": sequence,
        "clock": "CLOCK_MONOTONIC_RAW",
        "valid": timing.is_some() && error.is_none(),
        "stage_deltas_ns": stage_deltas,
        "error": error,
        "unavailable_stages": ["detect", "sim", "sign", "wire_encode"],
    });
    writeln!(out, "{row}").context("writing replay timing JSONL row")
}

#[cfg(test)]
mod log_config_tests {
    use super::{
        replay_timing_enabled, resolve_log_directives_from, validate_submit_switches,
        write_replay_timing_row,
    };
    use engine_replay::ReplayFrameTiming;

    #[test]
    fn rust_log_wins_over_log_level() {
        assert_eq!(
            resolve_log_directives_from(Some("info,engine=debug"), Some("warn")),
            "info,engine=debug"
        );
    }

    #[test]
    fn log_level_used_when_rust_log_absent_or_empty() {
        assert_eq!(resolve_log_directives_from(None, Some("debug")), "debug");
        assert_eq!(
            resolve_log_directives_from(Some("  "), Some("ERROR")),
            "error"
        );
    }

    #[test]
    fn invalid_or_missing_falls_back_to_info() {
        assert_eq!(resolve_log_directives_from(None, Some("loud")), "info");
        assert_eq!(resolve_log_directives_from(None, None), "info");
    }

    #[test]
    fn replay_event_timing_requires_network_fine_and_an_output_path() {
        assert!(!replay_timing_enabled(None, None));
        assert!(!replay_timing_enabled(Some("1"), None));
        assert!(!replay_timing_enabled(Some("0"), Some("out.jsonl")));
        assert!(!replay_timing_enabled(Some("1"), Some("  ")));
        assert!(replay_timing_enabled(Some("1"), Some("out.jsonl")));
    }

    #[test]
    fn contradictory_or_silently_ineffective_submit_switches_fail_boot() {
        assert!(validate_submit_switches(true, true, true).is_err());
        assert!(validate_submit_switches(false, true, false).is_err());
        assert!(validate_submit_switches(true, false, false).is_ok());
        assert!(validate_submit_switches(false, true, true).is_ok());
    }

    #[test]
    fn replay_timing_row_contains_correlated_deltas_and_explicit_unavailable_stages() {
        let timing = ReplayFrameTiming {
            start_ns: 100,
            envelope_done_ns: 130,
            base64_done_ns: 180,
            l2_decode_done_ns: 260,
        };
        let mut out = Vec::new();
        write_replay_timing_row(&mut out, 7, Some(99), Some(timing), None)
            .expect("write timing row");
        let row: serde_json::Value = serde_json::from_slice(&out).expect("valid JSONL");
        assert_eq!(row["frame_index"], 7);
        assert_eq!(row["sequence"], 99);
        assert_eq!(row["stage_deltas_ns"]["envelope_decode"], 30);
        assert_eq!(row["stage_deltas_ns"]["base64_decode"], 50);
        assert_eq!(row["stage_deltas_ns"]["l2_decode"], 80);
        assert_eq!(row["stage_deltas_ns"]["replay_decode_total"], 160);
        assert_eq!(
            row["unavailable_stages"],
            serde_json::json!(["detect", "sim", "sign", "wire_encode"])
        );
    }
}
