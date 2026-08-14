//! OS-level latency hardening (Linux), isolated here so the rest of the engine can keep
//! `#![forbid(unsafe_code)]`. Every function is **best-effort**: on a box without the needed
//! privilege (`CAP_SYS_RESOURCE`/`CAP_IPC_LOCK`/`CAP_SYS_NICE`) it logs and continues — the dev
//! sandbox can't apply most of these, and that must never be fatal.
//!
//! Plan §2 (`MANTIS_ULTRAFAST_OPTIMIZATION_PLAN.md`): superset of `runtime.rs`. Provides
//! `setrlimit` (so `mlockall` can actually pin), transparent-hugepage enablement, an SMT-aware
//! CPU plan that reserves core 0 for the OS/IRQ, and per-thread `SCHED_FIFO`.
//!
//! ## Safety / soft-lock guard
//! `SCHED_FIFO` at a high priority on a **non-isolated** core can starve the kernel and soft-lock
//! the box. Per the plan it stays OFF until cores are `isolcpus`-isolated. So RT scheduling is
//! **opt-in** (`SYSOPT_RT_SCHED=1`) and uses `SCHED_RESET_ON_FORK`; everything is disabled by the
//! master switch `SYSOPT_DISABLE=1`.

#![deny(missing_docs)]
// clippy: cosmetic — Default-then-assign reads clearer for the libc structs here; doc-list nits.
#![allow(clippy::field_reassign_with_default, clippy::doc_lazy_continuation)]

use tracing::{info, warn};

pub mod prefetch;
pub mod simd;
pub mod xdp;

/// Master off-switch: `SYSOPT_DISABLE=1` skips every optimization in this crate.
fn disabled() -> bool {
    matches!(
        std::env::var("SYSOPT_DISABLE").ok().as_deref(),
        Some("1") | Some("true")
    )
}

/// True if the kernel cmdline reserves cores with `isolcpus=` — the precondition for safely
/// running hot threads at `SCHED_FIFO`. Without it, RT priority on a busy-spinning hot thread
/// starves per-CPU kernel work and soft-locks the host (observed in the wild: the process is
/// SIGKILLed the instant priority 90 is applied at pipeline-live).
fn cores_isolated() -> bool {
    std::fs::read_to_string("/proc/cmdline")
        .map(|c| c.contains("isolcpus="))
        .unwrap_or(false)
}

/// RT scheduling is opt-in (`SYSOPT_RT_SCHED=1`). SAFETY GUARD: even when requested it is REFUSED
/// unless the hot cores are `isolcpus`-reserved, because `SCHED_FIFO` on a non-isolated core
/// soft-locks the host. This makes the dangerous flag safe-by-default on dev laptops. Override the
/// guard on a deliberately-tuned bare-metal box with `SYSOPT_RT_FORCE=1`.
/// Pure RT-eligibility decision (unit-testable independent of the host):
/// - `flag`: raw `SYSOPT_RT_SCHED` value (None = unset). DEFAULT OFF — reverted
///   2026-07-05 after LIVE evidence: with FIFO on `engine-sim` and its TS sim-helper
///   threads sharing the isolated cores, the sim dispatch spin-barrier
///   (`while remaining != 0 {{ yield_now }}`) live-locks (SCHED_FIFO never yields to
///   TS on the same core) — measured on AWS run dryrun-20260704-234044: sim_quotes=0,
///   candidates=0 for 10 min while scorer/feed ran normally. RT stays an explicit
///   opt-in until the thread architecture is RT-coherent (all-FIFO graded priorities
///   or helper-aware core plan).
/// - `forced`: `SYSOPT_RT_FORCE` override for deliberately-tuned bare-metal.
/// - `isolated`: kernel cmdline has an `isolcpus=` reservation (still required).
fn rt_sched_decision(flag: Option<&str>, forced: bool, isolated: bool) -> bool {
    let requested = match flag.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no"),
        None => false, // default OFF — see the live-lock evidence above
    };
    requested && (isolated || forced)
}

fn rt_sched_enabled() -> bool {
    let flag = std::env::var("SYSOPT_RT_SCHED").ok();
    let forced = matches!(
        std::env::var("SYSOPT_RT_FORCE").ok().as_deref(),
        Some("1") | Some("true")
    );
    let requested = match flag.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no"),
        None => true,
    };
    if !requested {
        return false;
    }
    if rt_sched_decision(flag.as_deref(), forced, cores_isolated()) {
        return true;
    }
    // Requested but UNSAFE (no isolcpus) → refuse and warn ONCE, then stay on CFS.
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        warn!(
            "engine-sysopt: RT scheduling (SYSOPT_RT_SCHED, default ON) IGNORED — kernel cmdline has no `isolcpus` reservation. \
             SCHED_FIFO on non-isolated cores soft-locks the host (SIGKILL at pipeline-live), so RT is \
             refused and hot threads stay on CFS. To enable RT: boot with `isolcpus=1-7 nohz_full=1-7 \
             rcu_nocbs=1-7` (AWS), or set SYSOPT_RT_FORCE=1 to override this guard."
        );
    });
    false
}

/// Hot-thread role → RT priority. Engine sim must out-prioritize the submitter; background
/// helpers stay on the normal scheduler (RT-exempt) so they can't starve the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    /// Detection/sim hot thread (highest RT priority).
    Engine,
    /// Submitter hot thread (one below engine).
    Submit,
    /// Background helper (scorer/gas/fluid) — never RT.
    Background,
}

impl ThreadRole {
    /// SCHED_FIFO priority for this role, or `None` for roles that must stay on the normal
    /// scheduler. 90/89 per plan §2; 1..99 is the valid `SCHED_FIFO` range.
    fn fifo_priority(self) -> Option<i32> {
        match self {
            ThreadRole::Engine => Some(90),
            ThreadRole::Submit => Some(89),
            ThreadRole::Background => None,
        }
    }
}

/// What `apply_process_optimizations` managed to do (for the startup log / tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessOptReport {
    /// `RLIMIT_MEMLOCK` raised to (effectively) unlimited.
    pub memlock_unlimited: bool,
    /// `RLIMIT_NOFILE` soft limit raised toward 1,048,576.
    pub nofile_raised: bool,
    /// Transparent hugepages confirmed enabled for the process.
    pub thp_enabled: bool,
    /// The whole crate was disabled via `SYSOPT_DISABLE`.
    pub skipped: bool,
}

/// Apply process-wide optimizations at startup, BEFORE `mlockall`/thread spawn: raise
/// `RLIMIT_MEMLOCK` (so page-locking can succeed) and `RLIMIT_NOFILE`, and ensure transparent
/// hugepages are not disabled for the process. Best-effort; never panics.
pub fn apply_process_optimizations() -> ProcessOptReport {
    if disabled() {
        info!("engine-sysopt: SYSOPT_DISABLE set — skipping all OS optimizations");
        return ProcessOptReport {
            skipped: true,
            ..Default::default()
        };
    }
    let mut r = ProcessOptReport::default();
    r.memlock_unlimited = set_rlimit_infinity(libc::RLIMIT_MEMLOCK, "RLIMIT_MEMLOCK");
    r.nofile_raised = raise_rlimit_nofile(1_048_576);
    r.thp_enabled = enable_transparent_hugepages();
    info!(
        memlock_unlimited = r.memlock_unlimited,
        nofile_raised = r.nofile_raised,
        thp_enabled = r.thp_enabled,
        "engine-sysopt: process optimizations applied (best-effort)"
    );
    r
}

/// Raise `RLIMIT_MEMLOCK` and lock the current and future process address space with
/// `mlockall(MCL_CURRENT | MCL_FUTURE)`. This keeps syscall unsafety in this crate while allowing
/// latency-sensitive binaries to remain `forbid(unsafe_code)`. Best-effort: returns `false` and logs
/// when the process lacks `CAP_SYS_RESOURCE`/`CAP_IPC_LOCK`, or when sysopt is disabled.
pub fn lock_process_memory() -> bool {
    if disabled() {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        let _ = set_rlimit_infinity(libc::RLIMIT_MEMLOCK, "RLIMIT_MEMLOCK");
        // SAFETY: mlockall has no pointer arguments. These flags request locking of the process's
        // current mappings and mappings created later; failure is reported without changing memory.
        let locked = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) == 0 };
        if !locked {
            let err = std::io::Error::last_os_error();
            warn!(%err, "engine-sysopt: mlockall failed (need CAP_IPC_LOCK); continuing");
        }
        locked
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Put the CURRENT thread on `SCHED_FIFO` at the role's priority (with `RESET_ON_FORK`). No-op
/// unless `SYSOPT_RT_SCHED=1` and the role is RT-eligible. Returns whether RT was applied.
/// Best-effort: logs and returns false on `EPERM` (needs `CAP_SYS_NICE`) — never panics.
pub fn apply_current_thread_rt(role: ThreadRole) -> bool {
    if disabled() || !rt_sched_enabled() {
        return false;
    }
    let Some(prio) = role.fifo_priority() else {
        return false;
    };
    // SAFETY: `sched_param` is a POD with a single int field; we pass a valid pointer to a
    // stack value that outlives the call. pid 0 = the calling thread.
    let applied = unsafe {
        let param = libc::sched_param {
            sched_priority: prio,
        };
        libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) == 0
    };
    if applied {
        info!(?role, prio, "engine-sysopt: thread set to SCHED_FIFO");
    } else {
        let err = std::io::Error::last_os_error();
        warn!(?role, prio, %err, "engine-sysopt: SCHED_FIFO failed (need CAP_SYS_NICE); staying on CFS");
    }
    applied
}

/// SMT-aware core assignment plan. Reserves logical core 0 for the OS/IRQ/relay and hands the
/// hot stages dedicated cores from 1.. . Advisory: the operator still pins via systemd
/// `CPUAffinity=1-N` + `isolcpus`; this documents/logs the intended layout.
#[derive(Debug, Clone)]
pub struct CpuPlan {
    /// Total logical cores seen.
    pub logical: usize,
    /// Core reserved for OS/IRQ (always 0).
    pub os_core: usize,
    /// Cores available to the engine's hot stages (1..logical), in assignment order.
    pub hot_cores: Vec<usize>,
}

/// Build the advisory CPU plan for `n_logical` cores (reserves core 0).
pub fn build_cpu_plan(n_logical: usize) -> CpuPlan {
    let logical = n_logical.max(1);
    let hot_cores = if logical > 1 {
        (1..logical).collect()
    } else {
        vec![0]
    };
    let plan = CpuPlan {
        logical,
        os_core: 0,
        hot_cores,
    };
    info!(
        logical = plan.logical,
        os_core = plan.os_core,
        hot_cores = ?plan.hot_cores,
        "engine-sysopt: CPU plan (advisory — pin via systemd CPUAffinity + isolcpus)"
    );
    plan
}

/// Bytes the fatal-signal hook writes to stdout: show cursor + leave the alternate screen.
static TERM_RESTORE_SEQ: &[u8] = b"\x1b[?25h\x1b[?1049l";

extern "C" fn restore_terminal_and_reraise(sig: libc::c_int) {
    // Async-signal-safety: only `write(2)`, `signal(2)` and `raise(2)` — all on the POSIX
    // async-signal-safe list. No allocation, no locks, no formatting.
    // SAFETY: writes a static byte string to a constant fd; resets the disposition to the
    // default and re-raises, so the process still terminates BY this signal (the exit status
    // a supervisor expects) — the hook only adds the terminal restore in front.
    unsafe {
        let _ = libc::write(
            libc::STDOUT_FILENO,
            TERM_RESTORE_SEQ.as_ptr() as *const libc::c_void,
            TERM_RESTORE_SEQ.len(),
        );
        libc::signal(sig, libc::SIG_DFL);
        let _ = libc::raise(sig);
    }
}

/// Install SIGINT/SIGTERM/SIGHUP hooks that restore the terminal (cursor visible, main
/// screen) and then re-raise the signal with its default action. The console dashboard owns
/// the alternate screen and Ctrl+C is its documented exit path — without this, the default
/// disposition kills the process *inside* the alt screen and wedges the user's terminal
/// (hidden cursor, blank screen, `reset` required). Call only when stdout is a real tty.
pub fn install_terminal_restore_on_signals() {
    // SAFETY: sigaction with a zeroed mask and a handler that is async-signal-safe (above).
    // Best-effort: a failed install just means the old (wedging) behavior remains.
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = restore_terminal_and_reraise as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            let _ = libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// Current terminal size as `(cols, rows)` via `TIOCGWINSZ` on stdout. Falls back to
/// `(100, 30)` when stdout is not a tty (piped/redirected) or the ioctl fails. Used by the
/// dashboard renderer to stay responsive to terminal resizes.
pub fn terminal_size() -> (u16, u16) {
    // SAFETY: `winsize` is a POD we fully initialize; ioctl on a valid fd writes into it.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        let rc = libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws);
        if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (100, 30)
        }
    }
}

/// Number of logical cores online (`_SC_NPROCESSORS_ONLN`), clamped to ≥1.
pub fn online_cores() -> usize {
    // SAFETY: sysconf with a constant name returns a long; we clamp the result.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n < 1 {
        1
    } else {
        n as usize
    }
}

/// Pin the current thread to one logical CPU with `sched_setaffinity(2)`. Invalid CPU indexes are
/// rejected before constructing the libc CPU set. Best-effort: returns `false` on an invalid core,
/// insufficient permissions, or an affinity mask that excludes the requested CPU; never panics.
pub fn pin_current_thread_to_core(core: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        let capacity = std::mem::size_of::<libc::cpu_set_t>() * 8;
        if core >= capacity {
            warn!(
                core,
                capacity, "engine-sysopt: requested affinity core is out of range"
            );
            return false;
        }

        // SAFETY: cpu_set_t is a C POD initialized with CPU_ZERO. `core` was bounds-checked against
        // the set's bit capacity before CPU_SET, and sched_setaffinity copies the live set.
        let applied = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            libc::CPU_SET(core, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
        };
        if !applied {
            let err = std::io::Error::last_os_error();
            warn!(core, %err, "engine-sysopt: sched_setaffinity failed; continuing");
        }
        applied
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = core;
        false
    }
}

// ---- low-latency socket options (isolated unsafe so callers keep `forbid(unsafe_code)`) --------

/// Re-arm `TCP_QUICKACK` on a raw TCP fd. Linux auto-clears the flag after every recv, so a
/// latency consumer must re-arm it per read to keep ACKs immediate (no delayed-ACK). Best-effort:
/// returns whether the syscall succeeded; never panics. No-op on non-Linux.
#[inline]
pub fn set_tcp_quickack(fd: std::os::unix::io::RawFd) -> bool {
    #[cfg(target_os = "linux")]
    {
        let one: libc::c_int = 1;
        // SAFETY: setsockopt with a valid fd, a constant level/option, and a pointer to a live
        // c_int of the declared length. Failure is reported, not acted on.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_QUICKACK,
                &one as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        rc == 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        false
    }
}

/// Enable `SO_BUSY_POLL` (microseconds) on a raw socket fd so the kernel busy-polls the NIC RX
/// ring instead of sleeping for an IRQ. Needs `CAP_NET_ADMIN` (or a raised sysctl); best-effort.
#[inline]
pub fn set_so_busy_poll(fd: std::os::unix::io::RawFd, usecs: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let v = usecs as libc::c_int;
        // SAFETY: as above — valid fd, constant level/option, live c_int of declared length.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BUSY_POLL,
                &v as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        rc == 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (fd, usecs);
        false
    }
}

/// Block until any of `fds` becomes readable (`POLLIN`) or `timeout_ms` elapses; returns `poll(2)`'s
/// value (`>0` = number of ready fds, `0` = timeout, `<0` = error). Used by the dual-relay reader to
/// wake the instant EITHER socket has a frame instead of `sleep()`-ing a fixed interval — a fixed
/// sleep folds up to that interval of latency into every frame that arrives mid-sleep. `poll()`
/// returns immediately on arrival, and because `SO_BUSY_POLL` is set on the feed sockets the kernel
/// busy-polls the NIC RX ring inside this `poll()` (sub-µs wake). `timeout_ms` bounds only the
/// idle-shutdown-check cadence, never the data path. Caps at 4 fds (dual-relay uses 2).
#[inline]
pub fn poll_readable(fds: &[std::os::unix::io::RawFd], timeout_ms: i32) -> i32 {
    #[cfg(target_os = "linux")]
    {
        let n = fds.len().min(4);
        if n == 0 {
            return 0;
        }
        let mut pfds: [libc::pollfd; 4] = unsafe { std::mem::zeroed() };
        for (i, &fd) in fds.iter().take(n).enumerate() {
            pfds[i] = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
        }
        // SAFETY: `pfds[..n]` are live, initialized pollfds; `n` matches the slice length passed to
        // poll; libc::poll only reads/writes those n entries.
        unsafe { libc::poll(pfds.as_mut_ptr(), n as libc::nfds_t, timeout_ms) }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (fds, timeout_ms);
        0
    }
}

/// Set `SO_RCVBUF` (receive buffer bytes) on a socket fd so a bursty feed isn't dropped/throttled by
/// a small kernel buffer. The kernel doubles the value (bookkeeping) and clamps to `rmem_max`, so
/// pair with the `net.core.rmem_max` sysctl. Best-effort.
#[inline]
pub fn set_so_rcvbuf(fd: std::os::unix::io::RawFd, bytes: u32) -> bool {
    let v = bytes as libc::c_int;
    // SAFETY: valid fd, constant level/option, live c_int of declared length.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &v as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    rc == 0
}

/// Return the interface carrying the first IPv4 default route from `/proc/net/route`.
///
/// Hardware receive timestamping is configured per network device, not per socket. Feed sockets use
/// this route in production; callers with policy routing or `SO_BINDTODEVICE` should instead pass
/// their known interface directly to [`configure_hardware_rx_timestamping`].
pub fn default_route_iface() -> Option<String> {
    let route = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in route.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let iface = fields.next()?;
        let destination = fields.next()?;
        if destination == "00000000" {
            return Some(iface.to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn hwtstamp_rx_enabled(config: &libc::hwtstamp_config) -> bool {
    config.rx_filter != libc::HWTSTAMP_FILTER_NONE as libc::c_int
}

#[cfg(target_os = "linux")]
fn hwtstamp_ioctl(
    fd: std::os::unix::io::RawFd,
    request: libc::c_ulong,
    iface: &str,
    config: &mut libc::hwtstamp_config,
) -> bool {
    let name = iface.as_bytes();
    if name.is_empty() || name.len() >= libc::IFNAMSIZ || name.contains(&0) {
        return false;
    }

    // SAFETY: `ifreq` is a C POD. The interface name is bounded to IFNAMSIZ-1 and the zeroed tail
    // supplies the required NUL terminator. `ifru_data` points to a live hwtstamp_config for the
    // complete ioctl call; the kernel copies/updates it synchronously.
    let rc = unsafe {
        let mut ifreq: libc::ifreq = std::mem::zeroed();
        for (dst, src) in ifreq.ifr_name.iter_mut().zip(name.iter().copied()) {
            *dst = src as libc::c_char;
        }
        ifreq.ifr_ifru.ifru_data = config as *mut libc::hwtstamp_config as *mut libc::c_char;
        libc::ioctl(fd, request, &mut ifreq)
    };
    rc == 0 && hwtstamp_rx_enabled(config)
}

/// Configure a Linux network device for hardware receive timestamps with `SIOCSHWTSTAMP`.
///
/// Requests `HWTSTAMP_TX_OFF` + `HWTSTAMP_FILTER_ALL`. The kernel/driver may widen the requested
/// receive filter; success is reported only when the ioctl succeeds and the returned effective
/// filter is not `HWTSTAMP_FILTER_NONE`. Changing this device-wide setting normally requires
/// `CAP_NET_ADMIN`. Failure is non-fatal so callers can retain kernel-software timestamps.
pub fn configure_hardware_rx_timestamping(fd: std::os::unix::io::RawFd, iface: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut config = libc::hwtstamp_config {
            flags: 0,
            tx_type: libc::HWTSTAMP_TX_OFF as libc::c_int,
            rx_filter: libc::HWTSTAMP_FILTER_ALL as libc::c_int,
        };
        hwtstamp_ioctl(fd, libc::SIOCSHWTSTAMP, iface, &mut config)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (fd, iface);
        false
    }
}

/// Configure `iface` with `SIOCSHWTSTAMP` using a short-lived control socket.
///
/// This is the process-local proof-producing form used by clock-discipline checks: `true` means
/// this call itself completed the set ioctl successfully and the driver returned a non-`NONE`
/// effective RX filter. A pre-existing configuration observed only through `SIOCGHWTSTAMP` is not
/// enough to make this function succeed.
pub fn configure_hardware_rx_timestamping_for_iface(iface: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: socket returns a new owned fd or -1. It is used only for the synchronous ioctl and
        // closed exactly once below.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return false;
        }
        let configured = configure_hardware_rx_timestamping(fd, iface);
        // SAFETY: fd was returned by socket above and has not otherwise been closed.
        unsafe { libc::close(fd) };
        configured
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        false
    }
}

/// Query whether a Linux network device is actually configured for hardware receive timestamps.
///
/// This uses `SIOCGHWTSTAMP`; an unsupported query, invalid interface, socket failure, or effective
/// `HWTSTAMP_FILTER_NONE` all fail closed to `false`. Thus NIC capability alone can never be used as
/// evidence that hardware timestamp generation was configured successfully.
pub fn hardware_rx_timestamping_configured(iface: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: socket returns a new owned fd or -1. It is used only for the synchronous ioctl and
        // closed exactly once below.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return false;
        }
        let mut config = libc::hwtstamp_config {
            flags: 0,
            tx_type: libc::HWTSTAMP_TX_OFF as libc::c_int,
            rx_filter: libc::HWTSTAMP_FILTER_NONE as libc::c_int,
        };
        let configured = hwtstamp_ioctl(fd, libc::SIOCGHWTSTAMP, iface, &mut config);
        // SAFETY: fd was returned by socket above and has not otherwise been closed.
        unsafe { libc::close(fd) };
        configured
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        false
    }
}

#[cfg(target_os = "linux")]
fn so_timestamping_flags(hardware_configured: bool) -> libc::c_int {
    let mut flags =
        (libc::SOF_TIMESTAMPING_RX_SOFTWARE | libc::SOF_TIMESTAMPING_SOFTWARE) as libc::c_int;
    if hardware_configured {
        flags |= (libc::SOF_TIMESTAMPING_RX_HARDWARE | libc::SOF_TIMESTAMPING_RAW_HARDWARE)
            as libc::c_int;
    }
    flags
}

/// Enable `SO_TIMESTAMPING` on a socket so the kernel stamps each inbound packet with its arrival
/// time. Kernel-software RX timestamps remain available even when device hardware configuration is
/// unavailable. Hardware request/reporting flags are included only when `SIOCGHWTSTAMP` proves the
/// default-route interface has a non-`NONE` RX filter (normally after a successful
/// [`configure_hardware_rx_timestamping`] call).
///
/// Best-effort; returns whether `setsockopt` succeeded. Reading either stamp requires `recvmsg` and
/// parsing the `SCM_TIMESTAMPING` ancillary message.
#[inline]
pub fn set_so_timestamping(fd: std::os::unix::io::RawFd) -> bool {
    #[cfg(target_os = "linux")]
    {
        // Always retain the software fallback. Merely setting SOF_TIMESTAMPING_RX_HARDWARE does not
        // configure the NIC; include HW flags only after the device configuration is observable.
        let hardware_configured = default_route_iface()
            .map(|iface| hardware_rx_timestamping_configured(&iface))
            .unwrap_or(false);
        let flags = so_timestamping_flags(hardware_configured);
        // SAFETY: valid fd, constant level/option, live c_int of declared length.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TIMESTAMPING,
                &flags as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        rc == 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        false
    }
}

/// The kernel's `struct scm_timestamping`: three timespecs carried in the `SCM_TIMESTAMPING`
/// control message — `ts[0]` software (kernel RX), `ts[1]` legacy transformed HW (deprecated),
/// `ts[2]` raw hardware (the NIC PHC; on AWS Nitro the true "frame left the wire" instant).
#[cfg(target_os = "linux")]
#[repr(C)]
struct ScmTimestamping {
    ts: [libc::timespec; 3],
}

/// Convert one kernel timestamp to CLOCK_REALTIME nanoseconds, rejecting malformed values rather
/// than wrapping them into a plausible-looking measurement.
#[cfg(target_os = "linux")]
fn timespec_ns(ts: &libc::timespec) -> Option<u64> {
    let sec = u64::try_from(ts.tv_sec).ok()?;
    let nsec = u64::try_from(ts.tv_nsec).ok()?;
    if nsec >= 1_000_000_000 {
        return None;
    }
    let ns = sec.checked_mul(1_000_000_000)?.checked_add(nsec)?;
    (ns != 0).then_some(ns)
}

/// Walk the `SCM_TIMESTAMPING` control message of a received `msghdr` and return the software
/// (`ts[0]`) and raw-hardware (`ts[2]`) CLOCK_REALTIME timestamps separately. A missing, malformed,
/// or undersized control message returns `(None, None)`. Split out from
/// [`recvmsg_timestamps_split`] so parsing is unit-testable with a synthetic control buffer.
///
/// # Safety
/// `msg` must have been populated by a successful `recvmsg`, or hand-built with a correctly-aligned
/// control buffer: `msg_control`/`msg_controllen` must describe a valid CMSG region.
#[cfg(target_os = "linux")]
unsafe fn scm_timestamps_from_msghdr(msg: &libc::msghdr) -> (Option<u64>, Option<u64>) {
    // SAFETY: guaranteed by this function's contract.
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(msg) };
    while !cmsg.is_null() {
        // SAFETY: CMSG_FIRSTHDR/CMSG_NXTHDR returned a header within msg's declared control region.
        let header = unsafe { &*cmsg };
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_TIMESTAMPING {
            // CMSG_FIRSTHDR validates only the header. Check the payload length before copying the
            // three timespecs so a truncated/malformed ancillary record cannot cause an OOB read.
            let needed =
                unsafe { libc::CMSG_LEN(std::mem::size_of::<ScmTimestamping>() as u32) as usize };
            if header.cmsg_len < needed {
                return (None, None);
            }
            let Some(control_end) = (msg.msg_control as usize).checked_add(msg.msg_controllen)
            else {
                return (None, None);
            };
            // SAFETY: cmsg is a header in msg's control region by the function contract.
            let data_start = unsafe { libc::CMSG_DATA(cmsg) as usize };
            let Some(data_end) = data_start.checked_add(std::mem::size_of::<ScmTimestamping>())
            else {
                return (None, None);
            };
            if data_end > control_end {
                return (None, None);
            }
            // CMSG_DATA is normally suitably aligned, but read_unaligned also makes the safety
            // independent of the caller's synthetic control-buffer alignment.
            let sts = unsafe {
                std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const ScmTimestamping)
            };
            return (timespec_ns(&sts.ts[0]), timespec_ns(&sts.ts[2]));
        }
        // SAFETY: cmsg belongs to msg's valid control-message chain by the function contract.
        cmsg = unsafe { libc::CMSG_NXTHDR(msg, cmsg) };
    }
    (None, None)
}

#[cfg(target_os = "linux")]
#[repr(align(16))]
struct AlignedControl([u8; 128]);

#[cfg(target_os = "linux")]
struct RecvmsgResult {
    bytes: usize,
    flags: libc::c_int,
    software_ns: Option<u64>,
    hardware_ns: Option<u64>,
}

#[cfg(target_os = "linux")]
fn recvmsg_raw(
    fd: std::os::unix::io::RawFd,
    buf: &mut [u8],
    collect_timestamping: bool,
) -> std::io::Result<RecvmsgResult> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    // 128 bytes exceeds CMSG_SPACE for three 64-bit timespecs. Explicit alignment is required:
    // a plain `[u8; N]` has alignment 1 and is not a valid backing store for `cmsghdr` accessors.
    let mut cbuf = AlignedControl([0u8; 128]);
    // SAFETY: msghdr is a C POD; zeroing then filling the fields we use is the documented idiom.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if collect_timestamping {
        msg.msg_control = cbuf.0.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cbuf.0.len() as _;
    }

    // SAFETY: msg owns valid iov/control pointers of the declared lengths for this call.
    let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // If ancillary data was truncated, do not inspect a partial cmsg. The payload remains valid and
    // the caller records this receive as missing timestamp coverage.
    let (software_ns, hardware_ns) =
        if collect_timestamping && msg.msg_flags & libc::MSG_CTRUNC == 0 {
            // SAFETY: msg was populated by successful recvmsg and cbuf remains live for the parse.
            unsafe { scm_timestamps_from_msghdr(&msg) }
        } else {
            (None, None)
        };
    Ok(RecvmsgResult {
        bytes: n as usize,
        flags: msg.msg_flags,
        software_ns,
        hardware_ns,
    })
}

/// Receive one socket record into `buf` and report whether the kernel set `MSG_TRUNC` because the
/// record exceeded the supplied buffer. This safe wrapper is intended for `SOCK_SEQPACKET` feed
/// frames: callers must reject `truncated == true` and must never decode the returned prefix.
#[cfg(target_os = "linux")]
pub fn recvmsg_packet(
    fd: std::os::unix::io::RawFd,
    buf: &mut [u8],
) -> std::io::Result<(usize, bool)> {
    let result = recvmsg_raw(fd, buf, false)?;
    Ok((result.bytes, result.flags & libc::MSG_TRUNC != 0))
}

/// Write one complete record to a connected message-oriented socket with exactly one syscall.
/// `SOCK_SEQPACKET` preserves this call as one record. The returned byte count must equal
/// `buf.len()`; callers must never retry a short result because that would create a second record.
#[cfg(target_os = "linux")]
pub fn send_packet_once(fd: std::os::unix::io::RawFd, buf: &[u8]) -> std::io::Result<usize> {
    // SAFETY: fd is supplied by the caller and buf is a live readable region for its exact length.
    let sent = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if sent < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(sent as usize)
    }
}

/// Block SIGINT/SIGTERM/SIGHUP on the current thread. Call before spawning worker threads so they
/// inherit the mask, then dedicate one ordinary Rust thread to [`wait_termination_signal`]. This
/// avoids async-signal handlers and lets safe Rust drain/join evidence writers during shutdown.
#[cfg(target_os = "linux")]
pub fn block_termination_signals() -> std::io::Result<()> {
    // SAFETY: sigset_t is a C POD initialized by sigemptyset/sigaddset; pthread_sigmask copies it.
    let rc = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut set) != 0
            || libc::sigaddset(&mut set, libc::SIGINT) != 0
            || libc::sigaddset(&mut set, libc::SIGTERM) != 0
            || libc::sigaddset(&mut set, libc::SIGHUP) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut())
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(rc))
    }
}

/// Synchronously wait for SIGINT/SIGTERM/SIGHUP on a thread whose termination signals are blocked.
/// Returns the received signal number; no async-signal context is involved.
#[cfg(target_os = "linux")]
pub fn wait_termination_signal() -> std::io::Result<libc::c_int> {
    // SAFETY: the set/result are live for sigwait, which writes one signal number before returning.
    let rc = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut set) != 0
            || libc::sigaddset(&mut set, libc::SIGINT) != 0
            || libc::sigaddset(&mut set, libc::SIGTERM) != 0
            || libc::sigaddset(&mut set, libc::SIGHUP) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut signal = 0;
        let rc = libc::sigwait(&set, &mut signal);
        (rc, signal)
    };
    if rc.0 == 0 {
        Ok(rc.1)
    } else {
        Err(std::io::Error::from_raw_os_error(rc.0))
    }
}

/// `recvmsg` on `fd` into `buf`, returning kernel-software (`ts[0]`) and NIC raw-hardware (`ts[2]`)
/// `SCM_TIMESTAMPING` values separately as CLOCK_REALTIME nanoseconds. A socket without stamps, a
/// missing cmsg, or truncated ancillary data returns `None` for the affected coverage sample. The
/// call honours the fd's blocking/non-blocking mode and performs no heap allocation.
#[cfg(target_os = "linux")]
pub fn recvmsg_timestamps_split(
    fd: std::os::unix::io::RawFd,
    buf: &mut [u8],
) -> std::io::Result<(usize, Option<u64>, Option<u64>)> {
    let result = recvmsg_raw(fd, buf, true)?;
    Ok((result.bytes, result.software_ns, result.hardware_ns))
}

/// Compatibility wrapper for callers that want one timestamp. Raw hardware remains preferred over
/// kernel software exactly as before; new feed instrumentation should use
/// [`recvmsg_timestamps_split`] so it can count and label both sources independently.
#[cfg(target_os = "linux")]
pub fn recvmsg_timestamp_ns(
    fd: std::os::unix::io::RawFd,
    buf: &mut [u8],
) -> std::io::Result<(usize, Option<u64>)> {
    let (bytes, software_ns, hardware_ns) = recvmsg_timestamps_split(fd, buf)?;
    Ok((bytes, hardware_ns.or(software_ns)))
}

/// Enable/disable `SO_KEEPALIVE` on a socket fd so an idle warm connection's liveness is probed and
/// the path stays open. Best-effort; portable across Unix.
#[inline]
pub fn set_so_keepalive(fd: std::os::unix::io::RawFd, on: bool) -> bool {
    let v: libc::c_int = on as libc::c_int;
    // SAFETY: valid fd, constant level/option, live c_int of declared length.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &v as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    rc == 0
}

/// Set `TCP_USER_TIMEOUT` (milliseconds) on a socket fd (Linux). This bounds how long transmitted
/// data may remain unacknowledged before the kernel forcibly errors the connection — turning a
/// silent blackhole (peer stops ACKing, no RST/FIN) from a multi-minute retransmit stall into a
/// bounded failure the submit path can classify. `ms == 0` leaves the kernel default (disabled).
/// Best-effort; returns whether the setsockopt succeeded. NOTE: `TCP_USER_TIMEOUT` bounds
/// unacknowledged *transmitted* data — it does NOT prove the peer application did not receive it, so
/// a timeout after a write begins is still delivery-ambiguous (P7B/P7C).
#[inline]
pub fn set_tcp_user_timeout(fd: std::os::unix::io::RawFd, ms: u32) -> bool {
    let v: libc::c_uint = ms;
    // SAFETY: valid fd, constant level/option, live c_uint of declared length.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_USER_TIMEOUT,
            &v as *const libc::c_uint as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    rc == 0
}

/// Read `CLOCK_MONOTONIC_RAW` as nanoseconds. Unlike `CLOCK_MONOTONIC` (what `std::time::Instant`
/// uses on Linux), the RAW clock is NOT slewed by NTP/adjtime, so a same-host duration measured
/// from two RAW reads cannot be stretched/compressed by clock discipline mid-measurement. Use it
/// for same-host stage durations; it is meaningless across hosts and has no epoch relation to UTC.
/// Falls back to 0 only if the syscall fails (never on a sane Linux).
#[inline]
pub fn clock_monotonic_raw_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live, correctly-sized timespec; CLOCK_MONOTONIC_RAW is a constant clockid.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec as u64)
}

// ---- internal best-effort syscall wrappers --------------------------------------------------

fn set_rlimit_infinity(resource: libc::__rlimit_resource_t, name: &str) -> bool {
    let lim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: `lim` is a valid initialized rlimit; resource is a constant.
    let ok = unsafe { libc::setrlimit(resource, &lim) == 0 };
    if !ok {
        let err = std::io::Error::last_os_error();
        warn!(%name, %err, "engine-sysopt: setrlimit(INFINITY) failed (need CAP_SYS_RESOURCE); continuing");
    }
    ok
}

fn raise_rlimit_nofile(want: u64) -> bool {
    // Read current limits; raise the soft limit up to the hard limit (or `want`, whichever is
    // lower) so we never exceed the hard cap (which would EPERM without CAP_SYS_RESOURCE).
    let mut cur = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid out-pointer; constant resource.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut cur) } != 0 {
        warn!("engine-sysopt: getrlimit(NOFILE) failed; leaving fd limit untouched");
        return false;
    }
    let target = want.min(cur.rlim_max);
    if cur.rlim_cur >= target {
        return true; // already high enough
    }
    let lim = libc::rlimit {
        rlim_cur: target as libc::rlim_t,
        rlim_max: cur.rlim_max,
    };
    // SAFETY: valid initialized rlimit; constant resource.
    let ok = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) == 0 };
    if !ok {
        let err = std::io::Error::last_os_error();
        warn!(%err, "engine-sysopt: setrlimit(NOFILE) failed; continuing");
    }
    ok
}

fn enable_transparent_hugepages() -> bool {
    // PR_SET_THP_DISABLE = 0 ensures THP is NOT disabled for this process (it does not force
    // 'always', but undoes any inherited disable). Best-effort; harmless if unsupported.
    // SAFETY: prctl with a constant option and zeroed unused args.
    let ok = unsafe { libc::prctl(libc::PR_SET_THP_DISABLE, 0, 0, 0, 0) == 0 };
    if !ok {
        let err = std::io::Error::last_os_error();
        warn!(%err, "engine-sysopt: prctl(PR_SET_THP_DISABLE,0) failed; continuing");
    }
    ok
}

/// B4: hint the kernel that this heap region should be backed by 2 MB huge pages.
///
/// Returns `true` if `madvise(MADV_HUGEPAGE)` succeeded. Best-effort — failure is logged at
/// debug (THP support varies by kernel + cgroup policy) and the caller still gets a working
/// region, just with 4 KB pages. Safe to call on a region the caller still uses afterwards;
/// `MADV_HUGEPAGE` is read-only advice (the kernel may or may not back this with huge pages,
/// but never invalidates the data).
///
/// Use on the largest hot tables: pool slot vec, route table, CSR `pool_to_hot_routes`,
/// `RouteId → Route` storage. On Graviton4 with 16-entry L1 dTLB, every 2 MB region promoted
/// from 4 KB → 2 MB pages collapses 512 TLB entries → 1 (sub-µs hot-path benefit on dense
/// scans). Default ON; `SYSOPT_HUGEPAGES=0` opts out.
#[cfg(target_os = "linux")]
pub fn madvise_hugepage_region(ptr: *const u8, len: usize) -> bool {
    if !env_flag_default_on("SYSOPT_HUGEPAGES") {
        return false;
    }
    if len == 0 || ptr.is_null() {
        return false;
    }
    // SAFETY: madvise with a non-null ptr + valid len. The kernel only reads the address
    // range to update its policy; it doesn't dereference or write to the data.
    let r = unsafe { libc::madvise(ptr as *mut libc::c_void, len, libc::MADV_HUGEPAGE) };
    if r != 0 {
        let err = std::io::Error::last_os_error();
        tracing::debug!(
            ?err,
            len,
            "engine-sysopt: madvise(MADV_HUGEPAGE) returned non-zero"
        );
        return false;
    }
    true
}

/// Stub for non-Linux platforms — huge-page advice is a Linux concept; always returns `false`.
#[cfg(not(target_os = "linux"))]
pub fn madvise_hugepage_region(_ptr: *const u8, _len: usize) -> bool {
    false
}

/// Same default-on env parsing as `engine` crate's helper — duplicated here so the sysopt
/// helpers can read flags without a circular dep.
#[inline]
fn env_flag_default_on(name: &str) -> bool {
    match std::env::var(name)
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
    {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no"),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affinity_rejects_an_out_of_range_core_without_a_syscall() {
        assert!(!pin_current_thread_to_core(usize::MAX));
    }

    #[test]
    fn process_opts_never_panic() {
        // On the sandbox most of these EPERM; the contract is "log + continue, never panic".
        let _r = apply_process_optimizations();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn poll_readable_wakes_on_data_and_times_out_when_idle() {
        // socketpair gives two connected fds without a NIC — enough to verify the contract the
        // dual-relay reader relies on: 0 on idle-timeout, >0 the INSTANT the peer writes (no fixed
        // sleep folding latency into the frame).
        let mut sv: [libc::c_int; 2] = [0; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair failed");
        let (rd, wr) = (sv[0], sv[1]);
        // No data yet → poll times out → 0.
        assert_eq!(poll_readable(&[rd], 5), 0, "idle poll must time out to 0");
        // Peer writes one byte → poll returns immediately with the fd ready (>0).
        let b = [1u8];
        assert_eq!(
            unsafe { libc::write(wr, b.as_ptr() as *const libc::c_void, 1) },
            1
        );
        assert!(poll_readable(&[rd], 100) > 0, "poll must wake on data");
        // Empty slice → 0 (no fds to wait on).
        assert_eq!(poll_readable(&[], 5), 0);
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn scm_timestamp_parse_preserves_sw_and_hw_from_synthetic_cmsg() {
        use std::mem::size_of;
        // Deterministic unit test of the cmsg parser: hand-build a control buffer holding one
        // SCM_TIMESTAMPING message and verify the helper keeps the software and raw-hardware slots
        // separate. Independent of whether the local NIC/loopback actually stamps — that is the
        // part the AWS NIC exercises in the dry-run.
        #[repr(align(16))]
        struct AlignedControl([u8; 128]);
        let mut cbuf = AlignedControl([0u8; 128]);
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_control = cbuf.0.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<ScmTimestamping>() as u32) } as _;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SO_TIMESTAMPING;
            (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<ScmTimestamping>() as u32) as _;
            let sts = libc::CMSG_DATA(cmsg) as *mut ScmTimestamping;
            (*sts).ts = std::mem::zeroed();
            // (a) only the software slot ts[0] set → parser returns it.
            (*sts).ts[0] = libc::timespec {
                tv_sec: 1_700_000_000,
                tv_nsec: 123_456_789,
            };
            assert_eq!(
                scm_timestamps_from_msghdr(&msg),
                (Some(1_700_000_000_u64 * 1_000_000_000 + 123_456_789), None)
            );
            // (b) raw-HW slot ts[2] also set → BOTH values survive separately.
            (*sts).ts[2] = libc::timespec {
                tv_sec: 1_700_000_001,
                tv_nsec: 5,
            };
            assert_eq!(
                scm_timestamps_from_msghdr(&msg),
                (
                    Some(1_700_000_000_u64 * 1_000_000_000 + 123_456_789),
                    Some(1_700_000_001_u64 * 1_000_000_000 + 5)
                ),
                "software ts[0] and raw hardware ts[2] must both survive"
            );

            // A short cmsg must be rejected before dereferencing its payload.
            (*cmsg).cmsg_len = libc::CMSG_LEN((size_of::<ScmTimestamping>() - 1) as u32) as _;
            assert_eq!(scm_timestamps_from_msghdr(&msg), (None, None));

            // A header that claims a complete payload cannot override the actual control-region
            // boundary. The parser must reject before reading beyond msg_controllen.
            (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<ScmTimestamping>() as u32) as _;
            msg.msg_controllen = libc::CMSG_LEN((size_of::<ScmTimestamping>() - 1) as u32) as _;
            assert_eq!(scm_timestamps_from_msghdr(&msg), (None, None));
        }
        // No control message → neither stamp (the graceful path on a non-stamping socket).
        let mut empty: libc::msghdr = unsafe { std::mem::zeroed() };
        empty.msg_controllen = 0;
        assert_eq!(unsafe { scm_timestamps_from_msghdr(&empty) }, (None, None));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn timestamp_conversion_rejects_invalid_or_overflowing_values() {
        assert_eq!(
            timespec_ns(&libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
            None,
            "zero pair denotes an unpopulated slot"
        );
        assert_eq!(
            timespec_ns(&libc::timespec {
                tv_sec: -1,
                tv_nsec: 0,
            }),
            None,
            "negative seconds"
        );
        assert_eq!(
            timespec_ns(&libc::timespec {
                tv_sec: 1,
                tv_nsec: 1_000_000_000,
            }),
            None,
            "nanoseconds must be normalized"
        );
        assert_eq!(
            timespec_ns(&libc::timespec {
                tv_sec: libc::time_t::MAX,
                tv_nsec: 0,
            }),
            None,
            "seconds-to-nanoseconds overflow"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn hardware_timestamp_configuration_evidence_is_fail_closed() {
        let none = libc::hwtstamp_config {
            flags: 0,
            tx_type: libc::HWTSTAMP_TX_OFF as libc::c_int,
            rx_filter: libc::HWTSTAMP_FILTER_NONE as libc::c_int,
        };
        let all = libc::hwtstamp_config {
            flags: 0,
            tx_type: libc::HWTSTAMP_TX_OFF as libc::c_int,
            rx_filter: libc::HWTSTAMP_FILTER_ALL as libc::c_int,
        };
        assert!(!hwtstamp_rx_enabled(&none));
        assert!(hwtstamp_rx_enabled(&all));
        let software_only = so_timestamping_flags(false);
        assert_ne!(
            software_only & libc::SOF_TIMESTAMPING_RX_SOFTWARE as libc::c_int,
            0
        );
        assert_eq!(
            software_only & libc::SOF_TIMESTAMPING_RX_HARDWARE as libc::c_int,
            0,
            "SO_TIMESTAMPING must not request HW generation without proven device configuration"
        );
        assert_eq!(
            software_only & libc::SOF_TIMESTAMPING_RAW_HARDWARE as libc::c_int,
            0,
            "SO_TIMESTAMPING must not report raw HW without proven device configuration"
        );
        let with_hardware = so_timestamping_flags(true);
        assert_ne!(
            with_hardware & libc::SOF_TIMESTAMPING_RX_HARDWARE as libc::c_int,
            0
        );
        assert_ne!(
            with_hardware & libc::SOF_TIMESTAMPING_RAW_HARDWARE as libc::c_int,
            0
        );
        assert!(
            !hardware_rx_timestamping_configured("not-a-real-interface"),
            "an unprovable device configuration must fail closed"
        );
        assert!(
            !configure_hardware_rx_timestamping(-1, "ens5"),
            "an unsuccessful SIOCSHWTSTAMP must never be treated as configured"
        );
        assert!(
            !configure_hardware_rx_timestamping_for_iface("not-a-real-interface"),
            "the proof-producing control-socket SIOCS path must fail closed"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn recvmsg_timestamp_round_trips_payload_and_is_graceful_without_stamp() {
        // Integration smoke for the recvmsg wrapper: it returns the exact payload and never panics
        // whether or not this kernel/interface attaches a timestamp (loopback software stamping is
        // environment-dependent; the HW path is exercised on the AWS NIC during the dry-run).
        use std::os::unix::io::AsRawFd;
        let rx = match std::net::UdpSocket::bind("127.0.0.1:0") {
            Ok(socket) => socket,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping loopback timestamp smoke: sandbox denied bind");
                return;
            }
            Err(err) => panic!("bind rx: {err}"),
        };
        let rx_addr = rx.local_addr().unwrap();
        let _ = set_so_timestamping(rx.as_raw_fd()); // best-effort; may be a no-op in a sandbox
        let tx = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind tx");
        tx.send_to(b"hello-stamp", rx_addr).expect("send");
        let mut buf = [0u8; 64];
        let (n, software, hardware) =
            recvmsg_timestamps_split(rx.as_raw_fd(), &mut buf).expect("recvmsg");
        assert_eq!(&buf[..n], b"hello-stamp", "payload must round-trip intact");
        // If stamps came through, each must be a plausible CLOCK_REALTIME ns; if not, None is fine.
        for ns in software.into_iter().chain(hardware) {
            assert!(
                ns > 1_500_000_000_000_000_000 && ns < 4_000_000_000_000_000_000,
                "implausible ns: {ns}"
            );
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn recvmsg_packet_reports_seqpacket_truncation() {
        // AF_UNIX socketpair is local-only and needs no bind/network permission. A sequenced-packet
        // receive must expose MSG_TRUNC so the engine can fail closed instead of decoding a prefix.
        let mut sv = [0; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, sv.as_mut_ptr()) },
            0,
            "seqpacket socketpair"
        );
        let payload = b"oversize";
        assert_eq!(
            unsafe {
                libc::write(
                    sv[0],
                    payload.as_ptr() as *const libc::c_void,
                    payload.len(),
                )
            },
            payload.len() as isize
        );
        let mut short = [0u8; 4];
        let (n, truncated) = recvmsg_packet(sv[1], &mut short).expect("recv truncated packet");
        assert_eq!(n, short.len());
        assert_eq!(&short, &payload[..short.len()]);
        assert!(truncated, "oversize seqpacket must expose MSG_TRUNC");

        let exact = b"okay";
        assert_eq!(
            unsafe { libc::write(sv[0], exact.as_ptr() as *const libc::c_void, exact.len()) },
            exact.len() as isize
        );
        let (n, truncated) = recvmsg_packet(sv[1], &mut short).expect("recv exact packet");
        assert_eq!(n, exact.len());
        assert_eq!(&short[..n], exact);
        assert!(!truncated, "exact-fit seqpacket must not report truncation");

        unsafe {
            libc::close(sv[0]);
            libc::close(sv[1]);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn madvise_hugepage_on_real_heap_region_is_safe() {
        // B4 contract: madvise on a live heap region returns truthfully and is safe to call
        // before, during, and after the caller continues to use the buffer. The kernel may
        // or may not back the range with huge pages depending on policy; either outcome is OK.
        let buf: Vec<u8> = vec![0xab; 4096 * 4]; // 16 KiB — small but valid
        let ok = madvise_hugepage_region(buf.as_ptr(), buf.len());
        // The data must be untouched after the advice call — proves madvise didn't corrupt.
        for b in &buf {
            assert_eq!(*b, 0xab);
        }
        // We don't assert ok=true because cgroups/policy may deny. We DO assert it doesn't panic.
        let _ = ok;
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn madvise_hugepage_handles_null_and_zero_len_safely() {
        // Defensive contract: invalid inputs return false without invoking madvise (so the
        // syscall isn't reached with bad args). Catches regressions where a caller passes a
        // dropped buffer's stale ptr or a zero-sized slot table.
        assert!(!madvise_hugepage_region(std::ptr::null(), 4096));
        let buf: Vec<u8> = Vec::with_capacity(64);
        assert!(!madvise_hugepage_region(buf.as_ptr(), 0));
    }

    #[test]
    fn madvise_hugepage_respects_opt_out_env() {
        // SYSOPT_HUGEPAGES=0 must short-circuit before the syscall. We verify this without
        // invoking the syscall (env-flag parse only).
        let prev = std::env::var("SYSOPT_HUGEPAGES").ok();
        // Safety: tests in this module are single-threaded by default; if parallelism is
        // enabled the env mutation could race. We restore at end.
        // SAFETY: we manage the lifetime of the env var ourselves; no concurrent readers.
        unsafe {
            std::env::set_var("SYSOPT_HUGEPAGES", "0");
        }
        let buf = vec![0u8; 4096];
        let r = madvise_hugepage_region(buf.as_ptr(), buf.len());
        assert!(!r, "SYSOPT_HUGEPAGES=0 must opt out");
        match prev {
            Some(v) => unsafe {
                std::env::set_var("SYSOPT_HUGEPAGES", v);
            },
            None => unsafe {
                std::env::remove_var("SYSOPT_HUGEPAGES");
            },
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn quickack_engages_on_real_socket() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        use std::os::unix::io::AsRawFd;
        // A connected TCP pair on loopback; TCP_QUICKACK is settable without privilege.
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping QUICKACK smoke: sandbox denied bind");
                return;
            }
            Err(err) => panic!("bind: {err}"),
        };
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).expect("connect");
        let (mut server, _) = listener.accept().expect("accept");
        server.write_all(b"x").ok();
        // The real assertion: the setsockopt actually succeeds on a live fd (not a no-op stub).
        assert!(
            set_tcp_quickack(client.as_raw_fd()),
            "TCP_QUICKACK must engage on a real socket"
        );
        // SO_BUSY_POLL needs CAP_NET_ADMIN; just assert it never panics (may be false unprivileged).
        let _ = set_so_busy_poll(client.as_raw_fd(), 50);
    }

    #[test]
    fn rt_default_on_guarded_and_explicit_off_respected() {
        // 2026-07-05: RT is DEFAULT-OFF again — live run dryrun-20260704-234044 proved
        // FIFO sim + TS sim-helpers on shared isolated cores live-lock the dispatch
        // barrier (sim_quotes=0 for 10 min). Explicit opt-in still honoured below.
        // unset → OFF everywhere, isolated or not:
        assert!(!rt_sched_decision(None, false, true));
        assert!(!rt_sched_decision(None, false, false));
        // explicit disable always wins, isolated or not
        assert!(!rt_sched_decision(Some("0"), false, true));
        assert!(!rt_sched_decision(Some("false"), true, true));
        // legacy explicit enable still works; force overrides missing isolation
        assert!(rt_sched_decision(Some("1"), false, true));
        assert!(rt_sched_decision(Some("1"), true, false));
        assert!(!rt_sched_decision(Some("1"), false, false));

        // With RT unset/disabled, RT is never applied — even for Engine/Submit roles.
        std::env::set_var("SYSOPT_RT_SCHED", "0");
        std::env::remove_var("SYSOPT_RT_FORCE");
        assert!(!apply_current_thread_rt(ThreadRole::Engine));
        assert!(!apply_current_thread_rt(ThreadRole::Submit));
        std::env::remove_var("SYSOPT_RT_SCHED");
        // Background is RT-exempt regardless.
        assert_eq!(ThreadRole::Background.fifo_priority(), None);
        assert_eq!(ThreadRole::Engine.fifo_priority(), Some(90));
        assert_eq!(ThreadRole::Submit.fifo_priority(), Some(89));

        // Footgun guard: on a host WITHOUT `isolcpus` (dev laptop / CI), requesting RT must be
        // REFUSED — SCHED_FIFO on non-isolated cores soft-locks the machine (SIGKILL at live).
        if !cores_isolated() {
            std::env::set_var("SYSOPT_RT_SCHED", "1");
            assert!(!rt_sched_enabled(), "RT must be refused without isolcpus");
            assert!(
                !apply_current_thread_rt(ThreadRole::Engine),
                "no SCHED_FIFO without isolcpus"
            );
            // An explicit operator override re-enables the request.
            std::env::set_var("SYSOPT_RT_FORCE", "1");
            assert!(rt_sched_enabled(), "SYSOPT_RT_FORCE=1 overrides the guard");
            std::env::remove_var("SYSOPT_RT_SCHED");
            std::env::remove_var("SYSOPT_RT_FORCE");
        }
    }

    #[test]
    fn cpu_plan_reserves_core0() {
        let p = build_cpu_plan(8);
        assert_eq!(p.os_core, 0);
        assert_eq!(p.hot_cores, vec![1, 2, 3, 4, 5, 6, 7]);
        // Single-core fallback still yields a usable core.
        assert_eq!(build_cpu_plan(1).hot_cores, vec![0]);
        assert!(online_cores() >= 1);
    }
}
