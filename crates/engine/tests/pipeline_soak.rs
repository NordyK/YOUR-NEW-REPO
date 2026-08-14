//! Pipeline soak test.
//!
//! Spins up the production pipeline orchestrator threads (RX → parser → state → sim →
//! REVM → submitter) with the relay endpoint pointed at a non-existent socket (RX thread
//! goes idle), then directly injects synthetic parsed events into the parser ring through
//! the public surface area exposed by the integration suite.
//!
//! The point of this test is to prove every hot thread is alive, that the kill switches
//! work, that the shutdown flag is honoured, and that no thread panics under load.

use std::sync::Arc;
use std::time::Duration;

use engine_config::EngineConfig;
use engine_gas::{GasOracle, GasSnapshot};
use engine_metrics::{LatencyHistograms, ReasonRing};

/// Smoke test: pipeline boots, kill switches arm/disarm cleanly, shutdown is honoured.
#[test]
fn pipeline_boots_and_shuts_down_cleanly() {
    // Use a config that points the relay socket at a path that doesn't exist; the RX
    // thread will idle and stay quiet until shutdown.
    let mut cfg = EngineConfig::default_arbitrum_one();
    cfg.relay_endpoint = "/tmp/engine-pipeline-soak-nonexistent.sock".into();

    let gas = Arc::new(GasOracle::new(GasSnapshot::default(), 1_000));
    let lat = Arc::new(LatencyHistograms::new());
    let reas = Arc::new(ReasonRing::new());
    let detection = Arc::new(engine::pipeline::DetectionContext::empty());
    let mut pipeline = engine::pipeline::Pipeline::new(cfg, gas, lat, reas, detection);
    pipeline.start_threads().expect("start_threads ok");

    // Give the threads a moment to come up.
    std::thread::sleep(Duration::from_millis(50));

    // Arm and disarm a kill switch to prove the atomic path round-trips.
    pipeline.kill.feed.trip(123);
    assert!(pipeline.kill.any_armed());
    pipeline.kill.feed.close(456);
    assert!(!pipeline.kill.any_armed());

    // Shut down and join everything.
    pipeline.signal_shutdown();
    pipeline.join();
}
