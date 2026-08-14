//! §18.7 hot-path zero-allocation gate (check 11), measured on the REAL `ReplayDriver` per-frame
//! hot path — the same production replay path used for the zero-network (`unshare -rn`) and 2-run
//! determinism proofs. This integration test is its own crate, so it installs a counting global
//! allocator without touching the production binary.
//!
//! Method: materialize the corpus frames up front (all I/O allocation happens BEFORE `arm()`),
//! run one full warmup pass (which grows `ReplayDriver`'s reused `decode_scratch` buffer to its
//! high-water mark), then `arm()` the counter and run the steady-state pass. After warmup the
//! decode buffer is already large enough, so a zero-alloc hot path records zero allocations.

use engine_metrics::CountingAllocator;
use engine_replay::{FeedFrameSource, ReplayDriver};
use mimalloc::MiMalloc;

#[global_allocator]
static ALLOC: CountingAllocator<MiMalloc> = CountingAllocator::new(MiMalloc);

#[test]
fn replay_hot_path_zero_alloc_after_warmup() {
    let corpus = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../anvil/corpus/blk_465686729.jsonl"
    );
    // The corpus is a recorded-from-live artifact (not committed — regenerate via the corpus
    // capture runbook in TEST_PLAN.md, needs an Arbitrum RPC). Skip-with-notice when absent so a
    // fresh clone isn't permanently red; the gate is enforced wherever the corpus exists
    // (laptop/AWS runs).
    if !std::path::Path::new(corpus).exists() {
        eprintln!(
            "SKIP §18.7 zero-alloc gate: corpus not present at {corpus} — regenerate from live \
             RPC (TEST_PLAN.md) to enforce"
        );
        return;
    }

    // Materialize every frame BEFORE arming — frame I/O + Vec growth must not be measured.
    let frames: Vec<_> = FeedFrameSource::open(corpus)
        .expect("open corpus")
        .collect();
    assert!(
        frames.len() >= 2,
        "corpus must have frames (got {})",
        frames.len()
    );

    let mut driver = ReplayDriver::new();

    // Warmup pass: grows the reused decode_scratch to the largest frame's requirement.
    for f in &frames {
        let _ = driver.process(f);
    }

    // Steady-state pass under the armed counter.
    ALLOC.arm();
    for f in &frames {
        let _ = driver.process(f);
    }
    let (allocs, deallocs, bytes) = ALLOC.snapshot();
    ALLOC.disarm(); // stop counting before the (allocating) println/assert formatting.

    println!(
        "STEADY-STATE replay hot path over {} frames: allocs={allocs} deallocs={deallocs} bytes={bytes}",
        frames.len()
    );
    assert_eq!(
        allocs, 0,
        "hot-path zero-allocation gate (§18.7) failed: {allocs} alloc(s) after warmup"
    );
}
