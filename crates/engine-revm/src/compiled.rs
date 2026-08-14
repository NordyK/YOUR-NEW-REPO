//! revmc AOT/JIT backend integration (2026-07-10), feature-gated behind `revmc`.
//!
//! Doctrine (shared with the sibling Fulcrum engine, and revmc's own positioning): the
//! compiled EVM must NEVER sit on the race-scoring path — detection stays on the
//! deterministic AMM kernels. What it accelerates is the INTERPRET share of REVM
//! validations/replays (~5-15 ms per validation once state is warm), which prices the
//! exotic lane and the sampler-audit turnaround, not promoted-family submits.
//!
//! Integration: `revmc::runtime::JitEvm` wraps our mainnet-shaped EVM and dispatches
//! per-code-hash compiled functions with transparent interpreter fallback — a compiled
//! function is used only once its background compile completes, so enabling JIT never
//! ADDS latency to any single execution.
//!
//! `REVM_BACKEND` env: `interpreter` (default) | `jit` | `aot`. Invalid values fail LOUD but
//! run as interpreter (a typo must never silently change execution semantics — the
//! byte-exactness test in this module is the correctness contract either way).
//! Building with `--features revmc` requires LLVM 22 on the build host.
//!
//! Stage 3 (2026-07-11) model — the compiled backend is ALWAYS non-blocking at runtime (the
//! `JitEvm` lookup transparently falls back to the interpreter for any code hash that is not yet
//! resident), so a compile in flight never stalls the validating thread. The difference between
//! `jit` and `aot` is entirely at BOOT:
//!   * `jit` — no boot precompile; each contract runs interpreted until its background JIT lands.
//!   * `aot` — a curated startup manifest (the code hashes the standing prewarm touches) is
//!     AOT-compiled BLOCKING before the warmup barrier ([`precompile_manifest`]) and the resulting
//!     dylibs are persisted ([`crate::revmc_artifacts`]), so those contracts run compiled from
//!     their very first validation AND a reboot skips the LLVM re-compile. Novel (non-manifest)
//!     bytecode still falls back to the interpreter and is counted (`lookup_misses`) — never
//!     fail-closed, because with Stage 0 the validator is off the submit race path so a fallback
//!     is correctness-neutral and merely latency-visible.

/// Execution backend mode, read once from `REVM_BACKEND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevmBackendMode {
    /// Plain revm interpreter (default; the byte-exactness baseline).
    Interpreter,
    /// JIT: compile hot bytecode in the background (NON-blocking) — the first execution of a
    /// given code hash runs interpreted, later ones run compiled. Lowest *steady-state*
    /// latency for a long-running validator with no boot cost; a warm-up window per contract.
    Jit,
    /// AOT-style: compile synchronously (BLOCKING) on first touch — no interpreter-fallback
    /// window, so a contract runs compiled from its very first validation. Lowest *cold-start*
    /// latency (pairs with a boot precompile of the known hot contracts); costs the compile
    /// time up front. Both JIT and AOT execute through the same `JitEvm` wrap.
    Aot,
}

impl RevmBackendMode {
    /// Parse an env value. `Err` carries the offending value for the caller's warn.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "interpreter" | "revm" => Ok(Self::Interpreter),
            "jit" => Ok(Self::Jit),
            "aot" => Ok(Self::Aot),
            other => Err(other.to_string()),
        }
    }

    /// Whether this mode runs a BLOCKING boot-precompile pass of the curated manifest before the
    /// warmup barrier (`aot`), vs relying purely on background compilation (`jit`). At RUNTIME both
    /// modes are non-blocking with interpreter fallback — see the module doc; the name is retained
    /// for API stability (`aot` = "blocking boot precompile", `jit` = "no boot precompile").
    pub fn compile_blocking(self) -> bool {
        matches!(self, Self::Aot)
    }

    /// Cached process-wide mode from `REVM_BACKEND`.
    pub fn from_env() -> Self {
        static V: std::sync::OnceLock<RevmBackendMode> = std::sync::OnceLock::new();
        *V.get_or_init(|| match Self::parse(
            &std::env::var("REVM_BACKEND").unwrap_or_default(),
        ) {
            Ok(m) => m,
            Err(bad) => {
                tracing::error!(
                    value = %bad,
                    "REVM_BACKEND invalid — running as interpreter (valid: interpreter|jit)"
                );
                RevmBackendMode::Interpreter
            }
        })
    }

    /// Whether the compiled `JitEvm` wrap applies (both Jit and Aot go through it; the wrap is
    /// non-blocking either way — see [`Self::compile_blocking`] for the boot difference).
    pub fn uses_jit(self) -> bool {
        matches!(self, Self::Jit | Self::Aot)
    }
}

/// Startup assertion — the Mantis port of Fulcrum's `guard_hot_path` (compiler.rs:181): the
/// compiled backend must be UNREACHABLE from the Stage-0 express lane (the promoted-candidate
/// kernel re-walk + sign/send on core 7 that carries 84-90% of gross). A compiled EVM execution
/// on the submit race path would break the "compiled backend never gates capital / never on the
/// race path" doctrine.
///
/// `interpreter` is trivially safe (no compiled code anywhere). For `jit`/`aot`, the caller passes
/// `express_lane_revm_free = true` to ATTEST — structurally, at the wiring point — that the express
/// lane runs pure AMM kernels and never constructs a `JitEvm` / touches [`global_jit_backend`]
/// (the compiled backend is reachable only through the REVM `ForkExecutor` on the validator core 6
/// and the mirror-advancer core 0, both off the submit path). Passing `false` while a compiled
/// backend is selected is a hard configuration error.
pub fn guard_hot_path(
    mode: RevmBackendMode,
    express_lane_revm_free: bool,
) -> Result<(), String> {
    if mode == RevmBackendMode::Interpreter {
        return Ok(());
    }
    if !express_lane_revm_free {
        return Err(format!(
            "REVM_BACKEND={mode:?} selects a compiled backend, but the express lane was not \
             attested REVM-free — a compiled execution must never sit on the submit race path"
        ));
    }
    Ok(())
}

#[cfg(feature = "revmc")]
pub use with_revmc::*;

#[cfg(feature = "revmc")]
mod with_revmc {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use alloy_primitives::{B256, Bytes};
    use revm::primitives::hardfork::SpecId;
    use revmc::runtime::{
        AotRequest, ArtifactStore, JitBackend, RuntimeConfig, RuntimeStatsSnapshot, RuntimeTuning,
    };

    use super::RevmBackendMode;

    /// Build the runtime config for a given backend mode. ALWAYS non-blocking at runtime (the
    /// `JitEvm` wrap falls back to the interpreter for any not-yet-resident code hash, so a compile
    /// in flight never stalls the validating thread — the Stage-3 doctrine). `aot` mode additionally
    /// turns on revmc's `aot` promotion (runtime misses become persisted AOT artifacts, not just
    /// in-memory JIT) and attaches the durable [`crate::revmc_artifacts::PersistentArtifactStore`]
    /// (keyed by revmc-rev + arch + cpu) when `REVMC_ARTIFACT_DIR` is set, so reboots skip the LLVM
    /// re-compile of the standing manifest.
    fn runtime_config(mode: RevmBackendMode) -> RuntimeConfig {
        let aot = mode == RevmBackendMode::Aot;
        let store: Option<Arc<dyn ArtifactStore>> = if aot {
            match std::env::var("REVMC_ARTIFACT_DIR").ok().filter(|s| !s.trim().is_empty()) {
                Some(dir) => match crate::revmc_artifacts::PersistentArtifactStore::open(&dir) {
                    Ok(s) => {
                        tracing::info!(
                            dir = %s.dir().display(),
                            preloaded = s.len(),
                            "revmc persistent artifact store online (AOT reboot cache)"
                        );
                        Some(Arc::new(s))
                    }
                    Err(e) => {
                        tracing::error!(err = %e, dir, "revmc artifact store open failed — in-memory only");
                        None
                    }
                },
                None => None,
            }
        } else {
            None
        };
        RuntimeConfig {
            // Non-blocking runtime: interpreter fallback for novel bytecode. NEVER `blocking:true`
            // here — that would synchronously LLVM-compile on the validating/advancing thread and
            // defeat the interpreter-fallback contract.
            blocking: false,
            aot,
            store,
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        }
    }

    /// Process-global compiled backend: one compiler runtime + one compiled-function cache shared by
    /// EVERY fork executor — the validator (core 6) and the mirror-advancer (core 0) reuse each
    /// other's compiles and the same persisted store. Background worker compiles on lookup miss;
    /// executions fall back to the interpreter until the function is resident. The config is derived
    /// from `REVM_BACKEND` (the master revmc mode): `aot` ⇒ persisted AOT store; anything else ⇒
    /// in-memory JIT. Kept a pure singleton so the byte-exactness test can drive its own backend.
    pub fn global_jit_backend() -> Option<&'static JitBackend> {
        static V: std::sync::OnceLock<Option<JitBackend>> = std::sync::OnceLock::new();
        V.get_or_init(|| {
            let mode = RevmBackendMode::from_env();
            match JitBackend::new(runtime_config(mode)) {
                Ok(b) => {
                    tracing::info!(?mode, "revmc compiled backend online (non-blocking, 1 worker)");
                    Some(b)
                }
                Err(e) => {
                    tracing::error!(err = %e, "revmc backend failed to start — interpreter");
                    None
                }
            }
        })
        .as_ref()
    }

    /// The `SpecId` Mantis's mainnet EVM — and therefore the `JitEvm` per-frame dispatch — runs
    /// under. AOT artifacts MUST be keyed under this exact spec, or the runtime lookup (which keys
    /// on the live `cfg.spec`) misses them and the manifest runs interpreted. Derived from the same
    /// `CfgEnv` default `Context::mainnet()` builds, so it tracks a revm hardfork bump; the port-
    /// check test pins the current value and asserts a freshly-built context agrees.
    pub fn precompile_spec_id() -> SpecId {
        *revm::context::CfgEnv::default().spec()
    }

    /// Outcome of a [`precompile_manifest`] pass, for the boot log + the AWS boot-budget audit.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct PrecompileReport {
        /// Distinct code hashes submitted for AOT compilation (already-resident ones excluded).
        pub requested: usize,
        /// How many became resident within the budget (compiled fresh OR loaded from the store).
        pub compiled: usize,
        /// Requested but not resident when the budget expired — these fall back to the interpreter
        /// (and get background-AOT-promoted on their first live touch). NOT an error.
        pub not_ready: usize,
        /// Already resident before this pass (e.g. preloaded from the persistent store on reboot).
        pub already_resident: usize,
        /// Wall-clock spent in the pass.
        pub elapsed_ms: u64,
    }

    /// BLOCKING boot precompile of the curated manifest — the "AOT" in Stage 3. `codes` is the
    /// DERIVED manifest: every distinct `(code_hash, bytecode)` the standing prewarm left resident
    /// in the validator's fork cache (pools, tokens, Balancer Vault, WETH, V4 PoolManager,
    /// MantisExecutor runtime — see `ForkExecutor::warm_code_hashes`). Keying by code hash dedups
    /// the hundreds of pools to the ~dozen distinct family bytecodes, so this is bounded.
    ///
    /// Runs at boot BEFORE the warmup barrier so the manifest contracts run compiled from the first
    /// live validation. `budget` bounds the wait; anything not resident by then simply falls back to
    /// the interpreter (correctness-neutral). Returns a [`PrecompileReport`] for the boot log.
    pub fn precompile_manifest(
        backend: &JitBackend,
        codes: &[(B256, Bytes)],
        budget: Duration,
    ) -> PrecompileReport {
        let spec = precompile_spec_id();
        let t0 = Instant::now();
        let mut report = PrecompileReport::default();
        let mut pending: Vec<B256> = Vec::new();
        let mut reqs: Vec<AotRequest> = Vec::new();
        for (hash, code) in codes {
            if code.is_empty() {
                continue;
            }
            if backend.get_compiled(*hash, spec).is_some() {
                report.already_resident += 1;
                continue;
            }
            reqs.push(AotRequest { code_hash: *hash, code: code.clone(), spec_id: spec });
            pending.push(*hash);
        }
        report.requested = reqs.len();
        if reqs.is_empty() {
            report.elapsed_ms = t0.elapsed().as_millis() as u64;
            return report;
        }
        backend.prepare_aot_batch(reqs);
        // Poll for residency until every requested hash is compiled or the budget expires. The
        // single AOT worker serializes compiles; a few ms poll granularity is far below one compile.
        let deadline = t0 + budget;
        loop {
            pending.retain(|h| backend.get_compiled(*h, spec).is_none());
            if pending.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        report.not_ready = pending.len();
        report.compiled = report.requested - report.not_ready;
        report.elapsed_ms = t0.elapsed().as_millis() as u64;
        report
    }

    /// A point-in-time snapshot of the global backend's runtime stats — the observability behind the
    /// `revmc_fallback_execs` counter. `lookup_misses` = executions that fell back to the interpreter
    /// (novel / not-yet-compiled bytecode); `lookup_hits` = compiled dispatches; `resident_entries` =
    /// live compiled programs; `compilations_{succeeded,failed}` = the background compile ledger.
    /// `None` when the backend never came online.
    pub fn backend_stats() -> Option<RuntimeStatsSnapshot> {
        global_jit_backend().map(|b| b.stats())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mode parsing: defaults + aliases + loud-invalid contract.
    #[test]
    fn backend_mode_parse() {
        assert_eq!(RevmBackendMode::parse(""), Ok(RevmBackendMode::Interpreter));
        assert_eq!(RevmBackendMode::parse("interpreter"), Ok(RevmBackendMode::Interpreter));
        assert_eq!(RevmBackendMode::parse("revm"), Ok(RevmBackendMode::Interpreter));
        assert_eq!(RevmBackendMode::parse("JIT"), Ok(RevmBackendMode::Jit));
        assert_eq!(RevmBackendMode::parse("AOT"), Ok(RevmBackendMode::Aot));
        assert!(RevmBackendMode::parse("fast").is_err(), "unknown must be loud");
        assert!(!RevmBackendMode::Interpreter.uses_jit());
        assert!(RevmBackendMode::Jit.uses_jit() && !RevmBackendMode::Jit.compile_blocking());
        assert!(RevmBackendMode::Aot.uses_jit() && RevmBackendMode::Aot.compile_blocking());
    }

    /// `guard_hot_path` (Fulcrum port): interpreter is always safe; a compiled backend REQUIRES an
    /// attestation that the express lane is REVM-free, else it is a hard configuration error.
    #[test]
    fn guard_hot_path_enforces_offrace_contract() {
        // Interpreter: safe regardless of the attestation.
        assert!(guard_hot_path(RevmBackendMode::Interpreter, false).is_ok());
        assert!(guard_hot_path(RevmBackendMode::Interpreter, true).is_ok());
        // Compiled backends require the express lane be attested REVM-free.
        assert!(guard_hot_path(RevmBackendMode::Jit, true).is_ok());
        assert!(guard_hot_path(RevmBackendMode::Aot, true).is_ok());
        assert!(guard_hot_path(RevmBackendMode::Jit, false).is_err());
        assert!(guard_hot_path(RevmBackendMode::Aot, false).is_err());
    }

    /// SpecId port-check: the spec the AOT precompile keys artifacts under MUST equal the spec a
    /// freshly-built mainnet `CfgEnv` (what `Context::mainnet()` / the `JitEvm` dispatch reads)
    /// reports — else precompiled artifacts are keyed wrong and silently never hit at runtime.
    #[cfg(feature = "revmc")]
    #[test]
    fn precompile_spec_id_matches_mainnet_cfg() {
        let derived = precompile_spec_id();
        let from_cfg = *revm::context::CfgEnv::default().spec();
        assert_eq!(derived, from_cfg, "AOT spec must match the runtime cfg spec");
        // Pin the current mainnet default so a revm hardfork bump is a loud, reviewed change.
        assert_eq!(
            derived,
            revm::primitives::hardfork::SpecId::OSAKA,
            "mainnet default spec changed — review AOT artifact-cache invalidation (bump CACHE_VERSION)"
        );
    }
}
