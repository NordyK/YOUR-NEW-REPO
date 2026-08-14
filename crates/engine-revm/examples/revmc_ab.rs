//! revmc A/B: interpreter vs JIT vs AOT interpret-latency on representative workloads.
//!
//! Runs the SAME bytecode through (1) plain revm interpreter, (2) `JitEvm` + background
//! (non-blocking JIT) backend, (3) `JitEvm` + blocking (AOT-style) backend, and reports
//! p50/p99 latency + the compiled-vs-interpreter speedup. Two workloads bracket a real arb
//! validation:
//!   * compute-bound loop  — cheap opcodes only (revmc's best case)
//!   * storage-bound loop  — SLOAD+SSTORE every iteration, like an AMM swap touching slots
//!     (revmc's realistic case — bridge crossings per SLOAD cap the gain)
//!
//! Byte-exactness is asserted every sample (a compiled answer that diverges is a hard error).
//! Build+run (needs LLVM 22): LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 \
//!   cargo run --release -p engine-revm --features revmc --example revmc_ab -- [iters]

#[cfg(not(feature = "revmc"))]
fn main() {
    eprintln!("build with --features revmc (needs LLVM 22): LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 \\");
    eprintln!("  cargo run --release -p engine-revm --features revmc --example revmc_ab");
}

#[cfg(feature = "revmc")]
mod ab {
use std::time::Instant;

use engine_revm::fork_db::{ForkDb, StateFetcher};
use revm::context::Context;
use revm::context::result::{ExecutionResult, Output};
use revm::database::CacheDB;
use revm::primitives::{Address, Bytes, TxKind, U256};
use revm::{ExecuteEvm, MainBuilder, MainContext};

/// Pure-compute loop (~26 gas/iter): PUSH2 n; JUMPDEST; PUSH1 1; SWAP1; SUB; DUP1; PUSH1 3; JUMPI; STOP.
fn compute_loop(iters: u16) -> Vec<u8> {
    let [hi, lo] = iters.to_be_bytes();
    vec![0x61, hi, lo, 0x5b, 0x60, 0x01, 0x90, 0x03, 0x80, 0x60, 0x03, 0x57, 0x00]
}

/// Storage loop: SLOAD(0)+ADD1+SSTORE(0) every iteration + counter — swap-representative.
fn storage_loop(iters: u16) -> Vec<u8> {
    let [hi, lo] = iters.to_be_bytes();
    vec![
        0x61, hi, lo, // PUSH2 n
        0x5b, // JUMPDEST (pc=3)
        0x60, 0x00, 0x54, // PUSH1 0; SLOAD
        0x60, 0x01, 0x01, // PUSH1 1; ADD
        0x60, 0x00, 0x55, // PUSH1 0; SSTORE
        0x60, 0x01, 0x90, 0x03, 0x80, // PUSH1 1; SWAP1; SUB; DUP1
        0x60, 0x03, 0x57, // PUSH1 3; JUMPI
        0x00, // STOP
    ]
}

struct CodeMock {
    addr: [u8; 20],
    code: Vec<u8>,
}
impl StateFetcher for CodeMock {
    fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
        if a == self.addr {
            (U256::ZERO, 0, Some(self.code.clone()))
        } else {
            (U256::ZERO, 0, None)
        }
    }
    fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
        U256::ZERO
    }
}

/// One execution. `backend` = None → plain interpreter; Some(&b) → JitEvm sharing that
/// backend's compiled-fn cache (so a warm backend runs the COMPILED function, not a re-compile).
fn run_once(code: &[u8], backend: Option<&revmc::runtime::JitBackend>) -> (bool, u64, Vec<u8>) {
    let contract = [0xcc; 20];
    let db = CacheDB::new(ForkDb::new(CodeMock {
        addr: contract,
        code: code.to_vec(),
    }));
    let evm = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = 42161;
            cfg.disable_nonce_check = true;
            cfg.tx_gas_limit_cap = Some(u64::MAX);
        })
        .build_mainnet();
    let tx = revm::context::TxEnv::builder()
        .caller(Address::from([0xaa; 20]))
        .kind(TxKind::Call(Address::from(contract)))
        .value(U256::ZERO)
        .data(Bytes::new())
        .gas_limit(30_000_000)
        .gas_price(0)
        .chain_id(None)
        .build()
        .expect("tx");
    let t = Instant::now();
    let result = match backend {
        None => {
            let mut evm = evm;
            evm.transact(tx).expect("interp")
        }
        Some(b) => {
            let mut jit = revmc::revm_evm::JitEvm::new(evm, b.clone());
            jit.transact(tx).expect("jit")
        }
    };
    let ns = t.elapsed().as_nanos() as u64;
    let (ok, out) = match result.result {
        ExecutionResult::Success {
            output: Output::Call(b),
            ..
        } => (true, b.to_vec()),
        other => (false, format!("{other:?}").into_bytes()),
    };
    (ok, ns, out)
}

fn new_backend(blocking: bool) -> revmc::runtime::JitBackend {
    revmc::runtime::JitBackend::new(revmc::runtime::RuntimeConfig {
        blocking,
        tuning: revmc::runtime::RuntimeTuning {
            jit_worker_count: 1,
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("jit backend")
}

fn pct(v: &mut [u64], p: f64) -> f64 {
    v.sort_unstable();
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)] as f64 / 1000.0
}

fn bench(name: &str, code: &[u8], samples: usize) {
    let (_, _, base_out) = run_once(code, None);
    // JIT (non-blocking) and AOT (blocking) each share ONE persistent backend so the compile
    // happens ONCE; warm it until the compiled fn is live, THEN measure steady-state execution.
    let jit_b = new_backend(false);
    let aot_b = new_backend(true);
    // AOT: first call compiles synchronously → warm after 2. JIT: compile is backgrounded →
    // spin a warmup loop long enough for the worker to finish and the cache to populate.
    for _ in 0..3 {
        let (ok, _, out) = run_once(code, Some(&aot_b));
        assert!(ok && out == base_out, "{name}: AOT diverged");
    }
    for _ in 0..400 {
        let (ok, _, out) = run_once(code, Some(&jit_b));
        assert!(ok && out == base_out, "{name}: JIT diverged");
    }
    let measure = |b: Option<&revmc::runtime::JitBackend>| -> (f64, f64) {
        let mut v: Vec<u64> = (0..samples)
            .map(|_| {
                let (ok, ns, out) = run_once(code, b);
                assert!(ok);
                assert_eq!(out, base_out, "{name}: byte-exactness broke mid-run");
                ns
            })
            .collect();
        let p50 = pct(&mut v, 0.50);
        let p99 = pct(&mut v, 0.99);
        (p50, p99)
    };
    let (i50, i99) = measure(None);
    let (j50, j99) = measure(Some(&jit_b));
    let (a50, a99) = measure(Some(&aot_b));
    println!("\n== {name} (n={samples}) ==");
    println!("  interpreter : p50 {i50:8.2}us  p99 {i99:8.2}us");
    println!("  JIT (warm)  : p50 {j50:8.2}us  p99 {j99:8.2}us   ({:.2}x vs interp)", i50 / j50);
    println!("  AOT (warm)  : p50 {a50:8.2}us  p99 {a99:8.2}us   ({:.2}x vs interp)", i50 / a50);
}

// ===================== Stage 3 corpus byte-exactness replay =====================
// Replays the recorded-candidate JSONL corpus (produced by `write_candidate_record_jsonl`) and
// asserts interpreter == AOT == JIT on (accept/reject, gas, output) for EVERY record. The point is
// backend AGREEMENT on identical inputs, NOT reproducing the live result — a reverted replay is a
// valid identity case. State is seeded OFFLINE from each record's `changed_slots.original` (the
// pre-state REVM read) plus the MantisExecutor runtime code from `REVMC_MANTIS_CODE`; missing state
// serves as zero and is seen identically by all three backends. Run:
//   REVMC_CORPUS_JSONL=candidates.jsonl REVMC_MANTIS_CODE=MantisExecutor.runtime.hex \
//   LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 cargo run --release -p engine-revm --features revmc \
//     --example revmc_ab
// Exits non-zero on ANY divergence, so it doubles as a gate.

use std::collections::HashMap;
use std::io::BufRead;

/// Serves the injected MantisExecutor code at its address, a synthetic max balance to the caller,
/// and the recorded `changed_slots.original` pre-state; everything else is zero/empty.
#[derive(Clone)]
struct CorpusFetcher {
    executor: [u8; 20],
    code: Option<Vec<u8>>,
    caller: [u8; 20],
    storage: HashMap<([u8; 20], U256), U256>,
}
impl StateFetcher for CorpusFetcher {
    fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
        if a == self.executor {
            return (U256::ZERO, 0, self.code.clone());
        }
        if a == self.caller {
            return (U256::MAX, 0, None);
        }
        (U256::ZERO, 0, None)
    }
    fn fetch_storage(&self, a: [u8; 20], s: U256) -> U256 {
        self.storage.get(&(a, s)).copied().unwrap_or(U256::ZERO)
    }
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let s = s.trim().trim_start_matches("0x");
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}
fn hex_to_u256(s: &str) -> U256 {
    U256::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(U256::ZERO)
}
fn hex_to_addr(s: &str) -> [u8; 20] {
    let b = hex_to_bytes(s);
    let mut a = [0u8; 20];
    if b.len() >= 20 {
        a.copy_from_slice(&b[b.len() - 20..]);
    }
    a
}

enum CorpusBackend<'a> {
    Interpreter,
    Compiled(&'a revmc::runtime::JitBackend),
}

/// One replay of `calldata` → `target` over a fresh clone of `fetcher`, through `backend`.
fn run_call(
    fetcher: &CorpusFetcher,
    target: [u8; 20],
    caller: [u8; 20],
    calldata: Vec<u8>,
    block_env: Option<(u64, u64)>,
    backend: CorpusBackend,
) -> (bool, u64, Vec<u8>) {
    let db = CacheDB::new(ForkDb::new(fetcher.clone()));
    let evm = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = 42161;
            cfg.disable_nonce_check = true;
            cfg.tx_gas_limit_cap = Some(u64::MAX);
        })
        .modify_block_chained(|b| {
            if let Some((n, t)) = block_env {
                b.number = U256::from(n);
                b.timestamp = U256::from(t);
            }
        })
        .build_mainnet();
    let tx = revm::context::TxEnv::builder()
        .caller(Address::from(caller))
        .kind(TxKind::Call(Address::from(target)))
        .value(U256::ZERO)
        .data(Bytes::from(calldata))
        .gas_limit(80_000_000)
        .gas_price(0)
        .chain_id(None)
        .build()
        .expect("tx");
    let result = match backend {
        CorpusBackend::Interpreter => {
            let mut evm = evm;
            evm.transact(tx).expect("interp transact")
        }
        CorpusBackend::Compiled(b) => {
            let mut jit = revmc::revm_evm::JitEvm::new(evm, b.clone());
            jit.transact(tx).expect("compiled transact")
        }
    };
    let gas = result.result.tx_gas_used();
    match result.result {
        ExecutionResult::Success { output: Output::Call(b), .. } => (true, gas, b.to_vec()),
        other => (false, gas, format!("{other:?}").into_bytes()),
    }
}

fn run_corpus(path: &str) {
    let spec = engine_revm::compiled::precompile_spec_id();
    let exec_code: Option<Vec<u8>> = std::env::var("REVMC_MANTIS_CODE")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| hex_to_bytes(&s))
        .filter(|c| !c.is_empty());
    if exec_code.is_none() {
        eprintln!(
            "WARN: REVMC_MANTIS_CODE unset — records replay against an EMPTY executor (trivial \
             success). Point it at the MantisExecutor runtime bytecode hex for a meaningful corpus."
        );
    }
    // Two persistent backends reused across records: AOT (blocking → compiled from first touch) and
    // JIT (non-blocking → we force each distinct code hash resident so its dispatch is deterministic).
    let aot_b = new_backend(true);
    let jit_b = new_backend(false);
    if let Some(code) = &exec_code {
        let h = alloy_primitives::keccak256(code);
        let _ = jit_b.compile_jit_sync(revmc::runtime::LookupRequest {
            key: revmc::runtime::RuntimeCacheKey { code_hash: h, spec_id: spec },
            code: Bytes::from(code.clone()),
        });
    }
    let file = std::fs::File::open(path).expect("open REVMC_CORPUS_JSONL");
    let (mut n, mut skipped, mut mismatches) = (0u64, 0u64, 0u64);
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            skipped += 1;
            continue;
        };
        let calldata_hex = v
            .get("executor_calldata")
            .or_else(|| v.get("mantis_calldata"))
            .or_else(|| v.get("calldata"))
            .and_then(|x| x.as_str());
        let (Some(cd), Some(ex)) =
            (calldata_hex, v.get("mantis_executor").and_then(|x| x.as_str()))
        else {
            skipped += 1;
            continue;
        };
        let calldata = hex_to_bytes(cd);
        if calldata.is_empty() {
            skipped += 1;
            continue;
        }
        let executor = hex_to_addr(ex);
        let caller = [0xaa; 20];
        let mut storage = HashMap::new();
        if let Some(cs) = v.get("changed_slots").and_then(|x| x.as_array()) {
            for slot in cs {
                if let (Some(a), Some(s), Some(o)) = (
                    slot.get("address").and_then(|x| x.as_str()),
                    slot.get("slot").and_then(|x| x.as_str()),
                    slot.get("original").and_then(|x| x.as_str()),
                ) {
                    storage.insert((hex_to_addr(a), hex_to_u256(s)), hex_to_u256(o));
                }
            }
        }
        let block_env = match (
            v.get("block_env").and_then(|x| x.as_u64()),
            v.get("block_timestamp").and_then(|x| x.as_u64()),
        ) {
            (Some(bn), Some(bt)) => Some((bn, bt)),
            _ => None,
        };
        let fetcher = CorpusFetcher { executor, code: exec_code.clone(), caller, storage };
        let i = run_call(&fetcher, executor, caller, calldata.clone(), block_env, CorpusBackend::Interpreter);
        let a = run_call(&fetcher, executor, caller, calldata.clone(), block_env, CorpusBackend::Compiled(&aot_b));
        let j = run_call(&fetcher, executor, caller, calldata, block_env, CorpusBackend::Compiled(&jit_b));
        n += 1;
        if i != a || i != j {
            mismatches += 1;
            eprintln!("MISMATCH record #{n}: interp={i:?}\n           aot={a:?}\n           jit={j:?}");
        }
    }
    println!("\ncorpus replay: {n} records, {skipped} skipped, {mismatches} MISMATCHES");
    if mismatches > 0 {
        std::process::exit(1);
    }
    println!("BYTE-EXACT: interpreter == AOT == JIT on all {n} records (accept/reject + gas + output)");
}

pub fn main() {
    // Corpus byte-exactness mode takes precedence when a corpus path is given.
    if let Ok(path) = std::env::var("REVMC_CORPUS_JSONL") {
        run_corpus(&path);
        return;
    }
    let samples: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    println!("revmc A/B — interpret latency, byte-exactness asserted every sample");
    // ~1000-iter loops ≈ a few hundred k gas, the arb-validation regime.
    bench("compute-bound (best case for revmc)", &compute_loop(2000), samples);
    bench("storage-bound (swap-representative)", &storage_loop(1000), samples);
}
}

#[cfg(feature = "revmc")]
fn main() {
    ab::main();
}
