use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
fn sidecar_propagates_hidden_relay_gaps_before_any_more_feed_bytes() {
    let path = repo_root().join("engine/crates/engine/src/bin/relay_sidecar.rs");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));

    let pump_start = source.find("fn pump_feed(").expect("pump_feed");
    let pump = &source[pump_start..];
    let install = pump
        .find("with_degradation_callback")
        .expect("relay degradation callback must be installed");
    let gate = pump
        .find("sidecar_feed_delivery_allowed")
        .expect("every post-gap frame must be gated");
    let first_send = pump.find("out.send_frame").expect("feed write path");
    assert!(
        install < gate && gate < first_send,
        "degradation callback and authority gate must precede every feed write"
    );
    assert!(
        pump.contains("capture.invalidate_evidence()"),
        "a relay gap must invalidate capture evidence as well as submission authority"
    );
}

/// C3b (WI-03/04 fix round): a base64-UNDECODABLE `l2Msg` is a state-bearing feed message
/// the engine cannot see into. Every live pre-normalizer decode arm must TAINT (advance the
/// feed taint watermark / emit a V4 taint marker) instead of a bare silent `continue`, and
/// the engine parser must not pre-skip empty payloads before the normalizer types them.
/// Source-armed so a refactor reintroducing the silent drop fails this test.
#[test]
fn undecodable_feed_payload_is_tainted_not_silently_skipped() {
    let window_after = |source: &str, needle: &str, label: &str| -> Vec<String> {
        let mut windows = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = source[from..].find(needle) {
            let start = from + rel;
            let end = (start + 600).min(source.len());
            windows.push(source[start..end].to_string());
            from = start + needle.len();
        }
        assert!(!windows.is_empty(), "{label}: no `{needle}` site found");
        windows
    };

    // Engine JSON parser: the live arm sits in parser_loop; the other in-crate match on the
    // same helper is a corpus TEST (byte-exactness fixture) and is exempt.
    let pipeline = fs::read_to_string(repo_root().join("engine/crates/engine/src/pipeline.rs"))
        .expect("read pipeline.rs");
    let live_windows: Vec<String> =
        window_after(&pipeline, "forgiving_decode_inplace(&mut b64_scratch)", "parser_loop");
    for window in &live_windows {
        assert!(
            !window.contains("Err(_) => continue"),
            "parser_loop must not silently drop an undecodable l2Msg (bare `Err(_) => continue`)"
        );
        assert!(
            window.contains("record_feed_taint"),
            "parser_loop's undecodable-l2Msg arm must advance the feed taint watermark"
        );
    }
    assert!(
        !pipeline.contains("if payload.is_empty() {\n                        continue;"),
        "parser_loop must route empty payloads through the normalizer (typed Malformed + \
         watermark), not pre-skip them silently"
    );

    // Sidecar: BOTH pumps (live WS + corpus replay) must ship a typed V4 taint marker for an
    // undecodable l2Msg so the ENGINE watermark moves.
    let sidecar =
        fs::read_to_string(repo_root().join("engine/crates/engine/src/bin/relay_sidecar.rs"))
            .expect("read relay_sidecar.rs");
    let sidecar_windows = window_after(&sidecar, "forgiving_decode_inplace(&mut b64)", "sidecar");
    assert_eq!(
        sidecar_windows.len(),
        2,
        "expected exactly the pump_file + pump_feed decode arms"
    );
    for window in &sidecar_windows {
        assert!(
            !window.contains("Err(_) => continue"),
            "sidecar must not silently drop an undecodable l2Msg (bare `Err(_) => continue`)"
        );
        assert!(
            window.contains("TAINT_CODE_UNDECODABLE_PAYLOAD"),
            "sidecar's undecodable-l2Msg arm must emit the typed V4 taint marker"
        );
    }
}

/// C2b (WI-03/04 fix round): the ONLY in-repo producer of binary feed frames is the sidecar,
/// and it emits V4 exclusively — pre-V4 binary frames (which cannot carry the canonical
/// `inner_ordinal`) enter an engine only from an OLD sidecar binary during a deploy version
/// skew, where they keep exactly the pre-WI-03 legacy same-sequence semantics. Source-armed:
/// reintroducing a V1/V2/V3 emission into the sidecar fails this test.
#[test]
fn sidecar_binary_emission_is_v4_only() {
    let sidecar =
        fs::read_to_string(repo_root().join("engine/crates/engine/src/bin/relay_sidecar.rs"))
            .expect("read relay_sidecar.rs");
    for legacy in ["encode_frame(", "encode_frame_v2(", "encode_frame_v3("] {
        assert!(
            !sidecar.contains(legacy),
            "relay_sidecar must not emit legacy binary frames ({legacy} found) — only \
             encode_frame_v4 carries the canonical inner_ordinal + taint markers"
        );
    }
    assert!(
        sidecar.contains("encode_frame_v4("),
        "relay_sidecar must emit ordinal-bearing V4 frames"
    );
}

#[test]
fn sidecar_emits_one_v4_ignorable_marker_for_every_unsupported_leaf() {
    let sidecar =
        fs::read_to_string(repo_root().join("engine/crates/engine/src/bin/relay_sidecar.rs"))
            .expect("read relay_sidecar.rs");
    let needle = "engine_feed::NormalizedItem::Unsupported { cursor, .. } =>";
    let mut windows = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = sidecar[from..].find(needle) {
        let start = from + relative;
        windows.push(sidecar[start..].chars().take(1_200).collect::<String>());
        from = start + needle.len();
    }

    assert_eq!(
        windows.len(),
        2,
        "file and live pumps must each handle Unsupported explicitly"
    );
    for window in &windows {
        assert!(
            window.contains("encode_frame_v4("),
            "Unsupported must emit a V4 record"
        );
        assert!(
            window.contains("cursor.inner_ordinal"),
            "Unsupported marker must carry its canonical ordinal"
        );
        assert!(
            window.contains("FeedFrameMarker::IgnorableOrdinal"),
            "Unsupported must use the typed ignorable marker"
        );
    }
    assert!(
        windows[0].contains("out.send_frame"),
        "file pump must send its ignorable marker directly"
    );
    assert!(
        windows[1].contains("sent && out.send_frame"),
        "live pump must reach its common ignorable-marker send path"
    );
    assert!(
        !sidecar.contains("Unsupported { .. } => false"),
        "the live Unsupported arm must not silently suppress its ordinal"
    );
    assert!(
        !sidecar.contains("ships nothing"),
        "sidecar documentation must not claim ignored leaves vanish on the wire"
    );
}
