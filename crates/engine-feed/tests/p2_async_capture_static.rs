use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn read_repo(path: &str) -> String {
    fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn code_only(source: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        Line,
        Block(usize),
        String { escaped: bool },
        RawString { hashes: usize },
    }

    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut state = State::Code;
    let mut i = 0;
    while i < bytes.len() {
        match state {
            State::Code if bytes.get(i..i + 2) == Some(b"//") => {
                out.extend_from_slice(b"  ");
                state = State::Line;
                i += 2;
            }
            State::Code if bytes.get(i..i + 2) == Some(b"/*") => {
                out.extend_from_slice(b"  ");
                state = State::Block(1);
                i += 2;
            }
            State::Code if bytes[i] == b'\'' => {
                // Distinguish short char/byte-char literals from Rust lifetimes (`'a`, `'_`). A
                // real char literal has a closing quote within one escaped/unicode scalar.
                let limit = (i + 12).min(bytes.len());
                let mut closing = None;
                let mut escaped = false;
                for (offset, byte) in bytes[i + 1..limit].iter().enumerate() {
                    if !escaped && *byte == b'\'' {
                        closing = Some(i + 1 + offset);
                        break;
                    }
                    escaped = !escaped && *byte == b'\\';
                    if *byte != b'\\' {
                        escaped = false;
                    }
                }
                if let Some(closing) = closing {
                    out.resize(out.len() + closing - i + 1, b' ');
                    i = closing + 1;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            State::Code if bytes[i] == b'r' => {
                let mut quote = i + 1;
                while bytes.get(quote) == Some(&b'#') {
                    quote += 1;
                }
                if bytes.get(quote) == Some(&b'"') {
                    let hashes = quote - i - 1;
                    out.resize(out.len() + quote - i + 1, b' ');
                    state = State::RawString { hashes };
                    i = quote + 1;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            State::Code if bytes[i] == b'"' => {
                out.push(b' ');
                state = State::String { escaped: false };
                i += 1;
            }
            State::Code => {
                out.push(bytes[i]);
                i += 1;
            }
            State::Line => {
                if bytes[i] == b'\n' {
                    out.push(b'\n');
                    state = State::Code;
                } else {
                    out.push(b' ');
                }
                i += 1;
            }
            State::Block(depth) if bytes.get(i..i + 2) == Some(b"/*") => {
                out.extend_from_slice(b"  ");
                state = State::Block(depth + 1);
                i += 2;
            }
            State::Block(depth) if bytes.get(i..i + 2) == Some(b"*/") => {
                out.extend_from_slice(b"  ");
                state = if depth == 1 {
                    State::Code
                } else {
                    State::Block(depth - 1)
                };
                i += 2;
            }
            State::Block(depth) => {
                out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                state = State::Block(depth);
                i += 1;
            }
            State::String { escaped } => {
                let byte = bytes[i];
                out.push(if byte == b'\n' { b'\n' } else { b' ' });
                state = if escaped {
                    State::String { escaped: false }
                } else if byte == b'\\' {
                    State::String { escaped: true }
                } else if byte == b'"' {
                    State::Code
                } else {
                    State::String { escaped: false }
                };
                i += 1;
            }
            State::RawString { hashes } => {
                let closes = bytes[i] == b'"'
                    && (0..hashes).all(|offset| bytes.get(i + 1 + offset) == Some(&b'#'));
                if closes {
                    out.resize(out.len() + hashes + 1, b' ');
                    i += hashes + 1;
                    state = State::Code;
                } else {
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
        }
    }
    String::from_utf8(out).expect("source was UTF-8")
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    let from = source
        .find(start)
        .ok_or_else(|| format!("missing structural anchor {start:?}"))?;
    let tail = &source[from..];
    let to = tail
        .find(end)
        .ok_or_else(|| format!("missing structural boundary {end:?}"))?;
    Ok(&tail[..to])
}

fn require_ordered(section: &str, tokens: &[&str]) -> Result<(), String> {
    let mut cursor = 0;
    for token in tokens {
        let relative = section[cursor..]
            .find(token)
            .ok_or_else(|| format!("missing ordered code token {token:?}"))?;
        cursor += relative + token.len();
    }
    Ok(())
}

fn assert_bounded_async_capture_structure(source: &str) -> Result<(), String> {
    // Comments are deliberately removed first: the old guard accepted a comment containing ten
    // magic words. These checks instead bind the ownership, publication, drain and invalidation
    // operations to the production method bodies that execute them.
    let code = code_only(source);
    let acquire = section(
        &code,
        "fn capture_raw_frame(&mut self, frame: &[u8])",
        "fn capture_freshness(",
    )?;
    require_ordered(
        acquire,
        &[
            "Self::Async(capture)",
            "frame.len() > CAPTURE_SLOT_BYTES",
            "capture.invalidate_one()",
            "capture.free.pop()",
            "capture.invalidate_one()",
            "slot.raw.extend_from_slice(frame)",
            "capture.pending = Some(slot)",
        ],
    )?;

    let async_impl = section(&code, "impl AsyncCapture", "impl Drop for AsyncCapture")?;
    require_ordered(
        async_impl,
        &[
            "fn finish_frame(&mut self)",
            "self.pending.take()",
            "self.stats.queue_depth.fetch_add(1",
            "self.ready.push(slot)",
            "self.stats.enqueued.fetch_add(1",
            "rtrb::PushError::Full(slot)",
            "self.stats.queue_depth.fetch_sub(1",
            "self.invalidate_one()",
        ],
    )?;

    let drop_impl = section(&code, "impl Drop for AsyncCapture", "enum CaptureSink")?;
    require_ordered(
        drop_impl,
        &[
            "self.finish_frame()",
            "self.shutdown.store(true, Ordering::Release)",
            "self.writer_thread.unpark()",
            "self.writer.take()",
            "writer.join()",
            "self.stats.invalidate_one()",
        ],
    )?;

    let writer = section(
        &code,
        "fn capture_writer_loop(",
        "fn drain_async_capture_before_watchdog_exit(",
    )?;
    require_ordered(
        writer,
        &[
            "while let Ok(mut slot) = ready.pop()",
            "raw.write_all(&slot.raw)",
            "write_freshness_row(&mut freshness, *row)",
            "stats.invalidate_one()",
            "free.push(slot)",
            "stats.invalidate_one()",
            "shutdown.load(Ordering::Acquire) && ready.is_empty()",
            "raw.flush()",
            "freshness.flush()",
            "persist_invalid_marker(&invalid_path, &stats)",
            "done.store(true, Ordering::Release)",
        ],
    )?;

    for runtime_test in [
        "fn async_capture_matches_sync_artifacts_byte_for_byte_when_lossless()",
        "fn async_capture_oversize_is_nonblocking_and_durably_invalidates_evidence()",
        "fn full_free_slot_ring_drops_capture_without_overwriting_inflight_slot()",
    ] {
        if !code.contains(runtime_test) {
            return Err(format!("missing runtime proof {runtime_test}"));
        }
    }
    Ok(())
}

#[test]
fn sidecar_has_bounded_async_capture_with_explicit_evidence_invalidation() {
    let sidecar = read_repo("engine/crates/engine/src/bin/relay_sidecar.rs");
    assert_bounded_async_capture_structure(&sidecar).expect("sidecar async capture structure");

    let marker_spoof = r#"
        // CAPTURE_MODE CaptureMode::Sync CaptureMode::Async CaptureMode::Off
        // RingBuffer::<CaptureSlot>::new capture_enqueued_total capture_dropped_total
        // capture_queue_depth capture_writer_stall_ns .invalid
    "#;
    assert!(
        assert_bounded_async_capture_structure(marker_spoof).is_err(),
        "comments containing the old marker list must not satisfy the structural guard"
    );

    let string_literal_spoof = r####"
        const SPOOF: &str = r###"
            fn capture_raw_frame(&mut self, frame: &[u8])
            Self::Async(capture) frame.len() > CAPTURE_SLOT_BYTES capture.invalidate_one()
            capture.free.pop() slot.raw.extend_from_slice(frame) capture.pending = Some(slot)
            fn capture_freshness(
        "###;
    "####;
    let stripped = code_only(string_literal_spoof);
    assert!(
        !stripped.contains("fn capture_raw_frame")
            && !stripped.contains("capture.pending = Some(slot)"),
        "raw/ordinary string contents must be removed before structural checks"
    );
    assert!(
        assert_bounded_async_capture_structure(string_literal_spoof).is_err(),
        "a raw string containing executable-looking tokens must not satisfy the guard"
    );
}

#[test]
fn sidecar_auxiliary_threads_are_kept_off_feed_decode_core() {
    let sidecar = read_repo("engine/crates/engine/src/bin/relay_sidecar.rs");
    let service = read_repo("engine/deploy/mantis-relay-sidecar.service");
    let verifier = read_repo("engine/deploy/aws/verify-pinning.sh");

    assert!(
        sidecar.contains("pin_to_core(2, \"relay-sidecar-feed\")"),
        "sidecar main/feed thread must be pinned to core 2"
    );
    assert!(
        sidecar.contains("pin_to_core(0, \"relay-watchdog\")")
            && sidecar.contains("pin_to_core(0, \"capture-writer\")"),
        "watchdog and capture writer must be pinned to housekeeping core 0"
    );
    assert!(
        service.contains("CPUAffinity=0 2"),
        "systemd outer affinity must allow both housekeeping core 0 and feed core 2"
    );
    assert!(
        verifier.contains("relay-watchdog") && verifier.contains("capture-writer"),
        "pin verifier must inspect named sidecar auxiliary threads"
    );
}
