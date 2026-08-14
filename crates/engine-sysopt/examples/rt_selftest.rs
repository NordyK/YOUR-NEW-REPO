//! RT / OS-optimization self-test. Applies the engine's process + thread hardening and reports
//! exactly what the kernel granted — so you can verify SCHED_FIFO / mlockall / THP actually engage
//! under sudo, before deploying.
//!
//! Run (needs privilege for RT + mlockall):
//!   sudo -E SYSOPT_RT_SCHED=1 \
//!     ~/.cargo/bin/cargo run -p engine-sysopt --example rt_selftest --release
//! or build once and run the binary:
//!   cargo build -p engine-sysopt --example rt_selftest --release
//!   sudo SYSOPT_RT_SCHED=1 target/release/examples/rt_selftest

use engine_sysopt::{
    apply_current_thread_rt, apply_process_optimizations, build_cpu_plan, online_cores, ThreadRole,
};

fn main() {
    println!("== engine-sysopt RT self-test ==");
    let uid = unsafe { libc::geteuid() };
    println!(
        "euid = {uid} ({})",
        if uid == 0 {
            "root — RT/mlockall can engage"
        } else {
            "non-root — most will EPERM"
        }
    );
    println!(
        "SYSOPT_RT_SCHED = {:?}",
        std::env::var("SYSOPT_RT_SCHED").ok()
    );

    let r = apply_process_optimizations();
    println!("\nprocess optimizations:");
    println!("  RLIMIT_MEMLOCK = infinity : {}", r.memlock_unlimited);
    println!("  RLIMIT_NOFILE raised      : {}", r.nofile_raised);
    println!("  THP not-disabled (prctl)  : {}", r.thp_enabled);

    let applied = apply_current_thread_rt(ThreadRole::Engine);
    // Read back the actual scheduler policy + priority the kernel assigned.
    let policy = unsafe { libc::sched_getscheduler(0) };
    let mut param = libc::sched_param { sched_priority: 0 };
    unsafe {
        libc::sched_getparam(0, &mut param);
    }
    let polname = match policy {
        libc::SCHED_FIFO => "SCHED_FIFO",
        libc::SCHED_RR => "SCHED_RR",
        libc::SCHED_OTHER => "SCHED_OTHER (CFS)",
        _ => "other",
    };
    println!("\nthread RT (Engine role):");
    println!("  apply_current_thread_rt returned: {applied}");
    println!(
        "  kernel scheduler now            : {polname} (prio {})",
        param.sched_priority
    );

    let plan = build_cpu_plan(online_cores());
    println!(
        "\nCPU plan: {} logical cores, os_core={}, hot_cores={:?}",
        plan.logical, plan.os_core, plan.hot_cores
    );

    println!(
        "\nVERDICT: {}",
        if applied && policy == libc::SCHED_FIFO {
            "RT scheduling ENGAGED (SCHED_FIFO) — full optimizations active."
        } else if uid != 0 {
            "RT NOT engaged (need root). Re-run with: sudo -E SYSOPT_RT_SCHED=1 <this binary>"
        } else {
            "RT NOT engaged though root — check SYSOPT_RT_SCHED=1 is set and not SYSOPT_DISABLE."
        }
    );
}
