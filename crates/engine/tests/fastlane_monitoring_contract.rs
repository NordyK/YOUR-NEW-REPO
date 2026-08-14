const PIPELINE_SOURCE: &str = include_str!("../src/pipeline.rs");

#[test]
fn pipeline_heartbeat_surfaces_all_express_spec_sign_counters() {
    let heartbeat_start = PIPELINE_SOURCE
        .find("if heartbeat_due.fire(elapsed)")
        .expect("pipeline source must retain the periodic heartbeat");
    let heartbeat_end = PIPELINE_SOURCE[heartbeat_start..]
        .find("\"pipeline heartbeat\"")
        .map(|offset| heartbeat_start + offset)
        .expect("pipeline heartbeat must retain its stable message");
    let heartbeat = &PIPELINE_SOURCE[heartbeat_start..heartbeat_end];

    let required = [
        (
            "express_spec_sign_job_overflow",
            "EXPRESS_SPEC_SIGN_JOB_OVERFLOW.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_sign_result_overflow",
            "EXPRESS_SPEC_SIGN_RESULT_OVERFLOW.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_sign_cancelled",
            "EXPRESS_SPEC_SIGN_CANCELLED.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_sign_stale",
            "EXPRESS_SPEC_SIGN_STALE.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_sign_parity_miss",
            "EXPRESS_SPEC_SIGN_PARITY_MISS.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_sign_hit",
            "EXPRESS_SPEC_SIGN_HIT.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_sign_miss",
            "EXPRESS_SPEC_SIGN_MISS.load(Ordering::Relaxed)",
        ),
        (
            "express_spec_signer_threads_spawned",
            "EXPRESS_SPEC_SIGNER_THREADS_SPAWNED.load(Ordering::Relaxed)",
        ),
    ];

    let missing = required
        .into_iter()
        .filter_map(|(field, counter)| {
            (!heartbeat.contains(field) || !heartbeat.contains(counter)).then_some(field)
        })
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "pipeline heartbeat is missing express speculative-sign observability fields: {}",
        missing.join(", ")
    );
}
