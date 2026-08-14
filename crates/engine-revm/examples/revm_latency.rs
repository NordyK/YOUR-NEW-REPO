//! Realistic in-memory REVM validation latency.
//!
//! Executes real `revm` bytecode against a warm in-memory `CacheDB` (NO RPC, NO fork) — the
//! engine's hot-path validation model. Two workloads bracket a real arb:
//!   * compute-bound loop  (cheap opcodes only)
//!   * storage-bound loop   (SLOAD+SSTORE every iteration — like AMM swaps touching slots)
//! Each is calibrated to typical arb gas (~250k for a 2-hop, ~500k for a 4-hop), run many
//! times, and reported as latency percentiles (us + ms) plus ns/gas throughput.
//!
//! Run: cargo run --release -p engine-revm --example revm_latency -- [iterations]

use std::time::Instant;

use alloy_primitives::U256;
use engine_revm::revm_validator::RevmValidator;

/// Pure-compute loop: each iteration ~26 gas (PUSH/SWAP/SUB/DUP/PUSH/JUMPI/JUMPDEST).
/// runtime: PUSH2 n; JUMPDEST; PUSH1 1; SWAP1; SUB; DUP1; PUSH1 3; JUMPI; STOP
fn compute_loop(iters: u16) -> Vec<u8> {
    let [hi, lo] = iters.to_be_bytes();
    vec![
        0x61, hi, lo, 0x5b, 0x60, 0x01, 0x90, 0x03, 0x80, 0x60, 0x03, 0x57, 0x00,
    ]
}

/// Storage loop: each iteration does SLOAD(slot)+SSTORE(slot) on a rotating slot, plus the
/// counter decrement — storage-heavy, like a swap mutating reserves/slot0.
/// runtime:
///   PUSH2 n                 [n]
/// L:JUMPDEST (pc=3)
///   DUP1                    [n,n]      ; slot = n
///   DUP1 SLOAD              [n,n,v]    ; read slot n
///   PUSH1 1 ADD             [n,n,v+1]
///   DUP3                    [n,n,v+1,n]; slot
///   SSTORE? need (slot,val) order: SSTORE pops slot then val.
/// To keep it correct and simple we use: PUSH1 slot0const ; the value loop.
fn storage_loop(iters: u16) -> Vec<u8> {
    let [hi, lo] = iters.to_be_bytes();
    // [n]
    // L: JUMPDEST(pc3)
    //   PUSH1 0  DUP1 SLOAD          ; [n, 0, v]
    //   PUSH1 1 ADD                  ; [n, 0, v+1]   (slot=0 still under)
    //   PUSH1 0 SSTORE               ; SSTORE(slot=0,val=v+1) pops 0 then v+1 -> [n]
    //   PUSH1 1 SWAP1 SUB            ; [n-1]
    //   DUP1 PUSH1 3 JUMPI           ; loop
    //   STOP
    vec![
        0x61, hi, lo,   // PUSH2 n
        0x5b, // JUMPDEST (pc 3)
        0x60, 0x00, 0x80, 0x54, // PUSH1 0; DUP1; SLOAD  -> [n,0,v]
        0x60, 0x01, 0x01, // PUSH1 1; ADD -> [n,0,v+1]
        0x90, 0x55, // SWAP1; SSTORE (slot=0,val=v+1) -> [n]
        0x60, 0x01, 0x90, 0x03, // PUSH1 1; SWAP1; SUB -> [n-1]
        0x80, 0x60, 0x03, 0x57, // DUP1; PUSH1 3; JUMPI
        0x00, // STOP
    ]
}

struct H {
    hist: hdrhistogram::Histogram<u64>,
}
impl H {
    fn new() -> Self {
        Self {
            hist: hdrhistogram::Histogram::new_with_bounds(1, 60_000_000_000, 3).unwrap(),
        }
    }
    fn rec(&mut self, ns: u64) {
        let _ = self.hist.record(ns.max(1));
    }
    fn p(&self, q: f64) -> u64 {
        self.hist.value_at_quantile(q / 100.0)
    }
}

fn run(label: &str, code: Vec<u8>, iters: usize) {
    let from = [0x11u8; 20];
    let to = [0xC0u8; 20];
    let mut v = RevmValidator::new();
    v.preload_account(from, U256::from(10u128).pow(U256::from(24u64)), 0, None);
    v.preload_account(to, U256::ZERO, 0, Some(code));

    // Warmup
    let mut gas_used = 0u64;
    for _ in 0..2000 {
        let r = v
            .call(
                from,
                to,
                U256::ZERO,
                vec![],
                30_000_000,
                U256::from(100_000_000u64),
            )
            .unwrap();
        gas_used = r.gas_used;
    }
    let mut h = H::new();
    for _ in 0..iters {
        let t = Instant::now();
        let r = v
            .call(
                from,
                to,
                U256::ZERO,
                vec![],
                30_000_000,
                U256::from(100_000_000u64),
            )
            .unwrap();
        h.rec(t.elapsed().as_nanos() as u64);
        gas_used = r.gas_used;
    }
    let us = |ns: u64| ns as f64 / 1_000.0;
    let ms = |ns: u64| ns as f64 / 1_000_000.0;
    let p50 = h.p(50.0);
    let ns_per_gas = p50 as f64 / gas_used.max(1) as f64;
    println!(
        "{label:<22} gas_used={gas_used:>7}  |  p50 {:>8.3}us  p90 {:>8.3}us  p99 {:>8.3}us  p99.9 {:>8.3}us  max {:>8.3}us",
        us(p50), us(h.p(90.0)), us(h.p(99.0)), us(h.p(99.9)), us(h.hist.max())
    );
    println!(
        "{:<22} ns/gas={ns_per_gas:>6.3}  |  p50 {:>8.5}ms  p90 {:>8.5}ms  p99 {:>8.5}ms  p99.9 {:>8.5}ms  max {:>8.5}ms",
        "", ms(p50), ms(h.p(90.0)), ms(h.p(99.0)), ms(h.p(99.9)), ms(h.hist.max())
    );
}

fn main() {
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::args().nth(1).and_then(|s| s.parse().ok()))
        .unwrap_or(50_000);
    println!(
        "# Realistic in-memory REVM validation latency (warm CacheDB, NO RPC)  iters={iters}\n"
    );
    // compute-bound, ~250k and ~500k gas (2-hop / 4-hop arb compute budget)
    run("compute ~250k gas", compute_loop(9600), iters); // ~26 gas * 9600 + overhead
    run("compute ~500k gas", compute_loop(19200), iters);
    // storage-bound, calibrated to similar gas (storage ops dominate real swaps)
    run("storage ~250k gas", storage_loop(2700), iters); // ~ (warm SLOAD+SSTORE+~) per iter
    run("storage ~500k gas", storage_loop(5400), iters);
    println!(
        "\n# NOTE: cold fork-DB REVM (RPC-backed first touch) is RPC-bound: tens-hundreds of ms,"
    );
    println!("#       dominated by eth_getStorageAt/eth_getCode round-trips, NOT EVM execution.");
}
