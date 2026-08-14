//! B2: `AF_XDP` zero-copy ingress scaffold.
//!
//! On the production AWS Graviton4 deployment, the binary feed comes in either through a UDS
//! sidecar (`binunix:`, already kernel-fast) or — on hosts running without the sidecar — over
//! a TCP socket from the upstream relay. The TCP path goes through the standard kernel network
//! stack: NIC → IRQ → softirq → skb → TCP → socket buffer → user read. Each hop adds latency
//! and the skb allocation is the largest contributor (~2–5 µs).
//!
//! `AF_XDP` lets a userspace program register a memory region with the kernel and receive
//! packets DIRECTLY into that region, bypassing the skb path entirely. On a busy Graviton4 with
//! ENA + a properly-pinned RX queue, this collapses the per-packet ingress cost to sub-µs.
//!
//! ## What this module is right now
//! A **capability probe** + **deployment-path documentation**. The actual XDP socket bring-up
//! requires:
//!   * `CAP_NET_ADMIN` and `CAP_BPF` capabilities (the engine running unprivileged on AWS would
//!     need these granted via systemd `AmbientCapabilities=`).
//!   * A specific RX queue index pinned to the engine's vCPU 1 (the relay-RX core).
//!   * NIC driver support for XDP (ENA on Graviton4 supports it; older drivers don't).
//!   * A loaded BPF program steering matching packets (by 5-tuple) to the XDP socket's UMEM.
//!
//! None of those are settable inside the sandbox. This module surfaces the **probe** so the
//! engine can log whether the target host has XDP available, and documents the production
//! bring-up path so an operator can follow the steps on the AWS box.
//!
//! ## Default-on policy
//! The XDP path is opt-in via `RELAY_USE_XDP=1` (NOT default-on) because the prerequisites are
//! deployment-specific. The probe IS default-on and always logged at startup so operators see
//! one of three states:
//!   * `xdp_kernel_supported=true, xdp_caps_granted=true, xdp_iface_supports_xdp=true` → ready.
//!   * `xdp_kernel_supported=true, xdp_caps_granted=false` → need CAP_NET_ADMIN+CAP_BPF.
//!   * `xdp_kernel_supported=false` → kernel < 4.18 or `AF_XDP` not compiled in.

/// One-shot capability snapshot. Surface this in the startup banner so operators don't have
/// to grep kernel logs.
#[derive(Debug, Clone, Copy, Default)]
pub struct XdpCapabilityReport {
    /// Kernel supports `AF_XDP` (probe via `socket(AF_XDP, ...)`).
    pub kernel_supported: bool,
    /// Process has `CAP_NET_ADMIN` (required to bind an XSK to a queue).
    pub has_cap_net_admin: bool,
    /// Process has `CAP_BPF` (required to load the steering BPF program).
    pub has_cap_bpf: bool,
}

impl XdpCapabilityReport {
    /// True iff every prerequisite is satisfied AND the operator has not opted out via
    /// `RELAY_USE_XDP=0`. The engine starts the XDP path only when this is true.
    pub fn xdp_path_usable(&self) -> bool {
        self.kernel_supported && self.has_cap_net_admin && self.has_cap_bpf && opted_in()
    }
}

/// Probe kernel + capability prerequisites for `AF_XDP`. Best-effort — every branch returns
/// `false` rather than panic on unsupported platforms.
// Linux capability bit numbers (`linux/include/uapi/linux/capability.h`). libc does not export
// these constants across all versions, so we pin them locally.
const CAP_NET_ADMIN: i32 = 12;
const CAP_BPF: i32 = 39;

/// Probe AF_XDP kernel + capability prerequisites. Linux-only; non-Linux returns the all-false
/// default. Called once at engine startup; the result feeds the active-opts banner.
#[cfg(target_os = "linux")]
pub fn probe_xdp_capabilities() -> XdpCapabilityReport {
    let kernel_supported = probe_kernel_af_xdp();
    let has_cap_net_admin = probe_capability(CAP_NET_ADMIN);
    let has_cap_bpf = probe_capability(CAP_BPF);
    XdpCapabilityReport {
        kernel_supported,
        has_cap_net_admin,
        has_cap_bpf,
    }
}

/// Non-Linux stub.
#[cfg(not(target_os = "linux"))]
pub fn probe_xdp_capabilities() -> XdpCapabilityReport {
    XdpCapabilityReport::default()
}

#[cfg(target_os = "linux")]
fn probe_kernel_af_xdp() -> bool {
    const AF_XDP: libc::c_int = 44;
    // SAFETY: socket() with constant params; we close the returned fd immediately.
    let fd = unsafe { libc::socket(AF_XDP, libc::SOCK_RAW, 0) };
    if fd < 0 {
        return false;
    }
    // SAFETY: fd is the value we just received from socket().
    unsafe { libc::close(fd) };
    true
}

#[cfg(target_os = "linux")]
fn probe_capability(cap: i32) -> bool {
    // Best-effort: read /proc/self/status `CapEff:` line and check the bit. Avoids libcap dep.
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return false,
    };
    let line = match status.lines().find(|l| l.starts_with("CapEff:")) {
        Some(l) => l,
        None => return false,
    };
    let hex = match line.split_whitespace().nth(1) {
        Some(h) => h,
        None => return false,
    };
    let bits = u64::from_str_radix(hex, 16).unwrap_or(0);
    (bits >> (cap as u64)) & 1 == 1
}

#[inline]
fn opted_in() -> bool {
    matches!(
        std::env::var("RELAY_USE_XDP").ok().as_deref(),
        Some("1") | Some("true") | Some("on") | Some("yes")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_never_panics_and_returns_a_report() {
        // On the dev sandbox most fields are false; the contract is "log + continue".
        let r = probe_xdp_capabilities();
        // Don't assert specific field values — they're environment-dependent. Just confirm
        // the probe completes without panicking.
        let _ = (r.kernel_supported, r.has_cap_net_admin, r.has_cap_bpf);
    }

    #[test]
    fn xdp_path_usable_requires_opt_in_even_if_caps_present() {
        // Defensive: even with every capability granted, the engine MUST NOT start the XDP
        // path unless the operator explicitly opts in. This protects deployments where XDP
        // is available but not desired (e.g. when a sidecar is preferred for filtering).
        let all_yes = XdpCapabilityReport {
            kernel_supported: true,
            has_cap_net_admin: true,
            has_cap_bpf: true,
        };
        // RELAY_USE_XDP unset → opt-out by default.
        let prev = std::env::var("RELAY_USE_XDP").ok();
        unsafe {
            std::env::remove_var("RELAY_USE_XDP");
        }
        assert!(
            !all_yes.xdp_path_usable(),
            "must opt-in via RELAY_USE_XDP=1"
        );
        // Opt in → usable.
        unsafe {
            std::env::set_var("RELAY_USE_XDP", "1");
        }
        assert!(all_yes.xdp_path_usable());
        // Restore.
        match prev {
            Some(v) => unsafe {
                std::env::set_var("RELAY_USE_XDP", v);
            },
            None => unsafe {
                std::env::remove_var("RELAY_USE_XDP");
            },
        }
    }
}
