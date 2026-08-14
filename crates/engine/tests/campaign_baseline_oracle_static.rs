//! WI-01 — campaign baseline oracle enforcement (codex plan §4 WI-01; master plan §D.1/§H).
//!
//! These are the WI-01 RED tests: they CREATE the missing regression-oracle enforcement.
//! Each test proves two things:
//!   1. the validator actually REJECTS bad input (an oracle that accepts anything is vacuous);
//!   2. the frozen Phase-0 baseline artifact exists under `codex/campaign_baseline_wi01/` and
//!      satisfies its own contract.
//!
//! Zero network. Zero paid RPC. Pure in-process validation over committed artifacts.

use engine::campaign_oracle::{
    self, ChangedRow, CorrectionManifestError, GoldenSchemaError, PhaseDiff, ProbeStatus,
    DECISION_CANDIDATE, DECISION_TYPED_DECLINE, POOL_KIND_CATALOG, REQUIRED_AUTHORITY_BOUNDARIES,
    REQUIRED_GOLDEN_DIMENSIONS, STRATEGY_KIND_CATALOG,
};
use engine::latency_budgets::{
    self, BudgetViolation, HotPayloadLayout, P999_MIN_CANDIDATE_CORRELATED_SAMPLES,
    ROUTE_AUTHORITY_CELL_BYTES, ROUTE_AUTHORITY_TABLE_MAX_BYTES, STATE_COMMIT_BASE_BUDGET_NS,
    STATE_COMMIT_PER_DIRTY_ROUTE_NS,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Repo root (mirrors the other `*_static` corpus tests).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// Mask Rust comments and literals without changing byte offsets or newlines.  The WI-06 gate
/// intentionally does not depend on a parser crate: Rust block comments nest, and a raw textual
/// `contains`/line count can therefore be spoofed by a comment or string literal.
fn mask_rust_source(source: &str) -> String {
    fn blank(bytes: &mut [u8], start: usize, end: usize) {
        for byte in &mut bytes[start..end] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }

    fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
        let mut i = start;
        if bytes.get(i) == Some(&b'b') {
            i += 1;
        }
        if bytes.get(i) != Some(&b'r') {
            return None;
        }
        i += 1;
        let hashes_start = i;
        while bytes.get(i) == Some(&b'#') {
            i += 1;
        }
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        let hash_count = i - hashes_start;
        i += 1;
        while i < bytes.len() {
            if bytes[i] == b'"'
                && bytes
                    .get(i + 1..i + 1 + hash_count)
                    .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
            {
                return Some(i + 1 + hash_count);
            }
            i += 1;
        }
        None
    }

    fn normal_literal_end(bytes: &[u8], start: usize, quote: u8) -> usize {
        let mut i = start + 1;
        let mut escaped = false;
        while i < bytes.len() {
            let byte = bytes[i];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote {
                return i + 1;
            } else if byte == b'\n' && quote == b'"' {
                return i;
            }
            i += 1;
        }
        bytes.len()
    }

    fn char_literal_end(bytes: &[u8], quote: usize) -> Option<usize> {
        let mut i = quote + 1;
        if i >= bytes.len() || bytes[i] == b'\n' || bytes[i] == b'\r' {
            return None;
        }
        if bytes[i] == b'\\' {
            i += 1;
            if i >= bytes.len() || bytes[i] == b'\n' || bytes[i] == b'\r' {
                return None;
            }
            // Escaped Unicode forms (for example `\\u{1f600}`) are consumed through their
            // closing brace; ordinary escapes consume the one escaped byte.
            if bytes[i] == b'u' && bytes.get(i + 1) == Some(&b'{') {
                i += 2;
                while i < bytes.len() && bytes[i] != b'}' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return None;
                }
            }
            i += 1;
        } else {
            let ch = std::str::from_utf8(&bytes[i..]).ok()?.chars().next()?;
            i += ch.len_utf8();
        }
        (bytes.get(i) == Some(&b'\'')).then_some(i + 1)
    }

    let bytes = source.as_bytes();
    let mut masked = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            blank(&mut masked, start, i);
            continue;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let start = i;
            let mut depth = 1usize;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank(&mut masked, start, i);
            continue;
        }
        if (bytes[i] == b'r' || (bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'r')))
            && raw_string_end(bytes, i).is_some()
        {
            let end = raw_string_end(bytes, i).unwrap();
            blank(&mut masked, i, end);
            i = end;
            continue;
        }
        if bytes[i] == b'"' || (bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'"')) {
            let start = i;
            if bytes[i] == b'b' {
                i += 1;
            }
            let end = normal_literal_end(bytes, i, b'"');
            blank(&mut masked, start, end);
            i = end;
            continue;
        }
        let char_quote = if bytes[i] == b'\'' {
            Some(i)
        } else if bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'\'') {
            Some(i + 1)
        } else {
            None
        };
        if let Some(quote) = char_quote {
            if let Some(end) = char_literal_end(bytes, quote) {
                blank(&mut masked, i, end);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    String::from_utf8(masked).expect("Rust source is UTF-8")
}

fn cfg_test_item_spans(masked_source: &str) -> Vec<(usize, usize)> {
    fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (idx, byte) in bytes.iter().enumerate().skip(open) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(idx);
                    }
                }
                _ => {}
            }
        }
        None
    }

    let bytes = masked_source.as_bytes();
    let mut spans = Vec::new();
    let marker = b"#[cfg(test)]";
    let mut cursor = 0;
    while let Some(relative) = bytes[cursor..]
        .windows(marker.len())
        .position(|window| window == marker)
    {
        let start = cursor + relative;
        let mut item = start + marker.len();
        while item < bytes.len() && bytes[item].is_ascii_whitespace() {
            item += 1;
        }
        let mut end = item;
        while end < bytes.len() && bytes[end] != b'{' && bytes[end] != b';' {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'{' {
            end = matching_brace(bytes, end).unwrap_or(bytes.len() - 1) + 1;
        } else if end < bytes.len() {
            end += 1;
        } else {
            end = bytes.len();
        }
        spans.push((start, end));
        cursor = end;
    }
    spans
}

fn strip_cfg_test_items(masked_source: &str) -> String {
    fn blank(bytes: &mut [u8], start: usize, end: usize) {
        for byte in &mut bytes[start..end] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }

    let mut out = masked_source.as_bytes().to_vec();
    for (start, end) in cfg_test_item_spans(masked_source) {
        blank(&mut out, start, end);
    }
    String::from_utf8(out).expect("masked Rust source is UTF-8")
}

/// Delete complete cfg(test) item spans while retaining every byte in every production span.  This
/// is the canonical comparison view used to prove that T6 changed only test-owned source.
fn canonical_production_view(raw_source: &str) -> String {
    let masked = mask_rust_source(raw_source);
    let mut out = String::with_capacity(raw_source.len());
    let mut cursor = 0usize;
    for (start, end) in cfg_test_item_spans(&masked) {
        out.push_str(&raw_source[cursor..start]);
        cursor = end;
    }
    out.push_str(&raw_source[cursor..]);
    // Removing a cfg(test) item can leave a different number of separator blank lines at its
    // boundary in two otherwise identical revisions. Keep all non-whitespace production bytes
    // and canonicalize only consecutive blank-line runs so the comparison remains about the
    // production token stream, not test-item placement whitespace.
    let mut normalized = String::with_capacity(out.len());
    let mut previous_blank = false;
    for line in out.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        normalized.push_str(line);
        previous_blank = blank;
    }
    normalized
}

/// T8-S permits only the reviewed FeedSniff ordinal bridge in production. Normalize the exact
/// three-point pass-through and its truthfulness-only comment repairs before the inherited
/// production-stream identity comparison; every anchor must occur exactly once in either the
/// frozen base spelling or the reviewed GREEN spelling.
fn canonical_t8_sniff_production_view(raw_source: &str) -> String {
    let mut out = canonical_production_view(raw_source);
    const BRIDGES: [(&str, &str); 9] = [
        (
            "    /// resync/receipt events with no single triggering leaf). Threaded to the emitted",
            "    /// synthetic non-feed events with no single triggering leaf). Threaded to the emitted",
        ),
        (
            "    /// Hash of the feed tx that dirtied these pools ([0;32] for resync/sniff events with no single",
            "    /// Hash of the feed tx that dirtied these pools ([0;32] for synthetic non-feed events with no single",
        ),
        (
            "/// the ordinal-0 sniff/resync/receipt sentinel — such a candidate services at `(seq, 0)` and never",
            "/// the ordinal-0 synthetic non-feed sentinel — such a candidate services at `(seq, 0)` and never",
        ),
        (
            "                            parsed.seq,\n                            parsed.tx_hash.unwrap_or([0u8; 32]),",
            "                            parsed.seq,\n                            parsed.inner_ordinal,\n                            parsed.tx_hash.unwrap_or([0u8; 32]),",
        ),
        (
            "fn emit_sniffed_dirty_event(\n    dirty_queue: &Arc<ArrayQueue<DirtyEvent>>,\n    dirty_events: &Arc<AtomicU64>,\n    recv_ns: u64,\n    recv_mono_raw_ns: u64,\n    seq: u64,\n    tx_hash: [u8; 32],",
            "fn emit_sniffed_dirty_event(\n    dirty_queue: &Arc<ArrayQueue<DirtyEvent>>,\n    dirty_events: &Arc<AtomicU64>,\n    recv_ns: u64,\n    recv_mono_raw_ns: u64,\n    seq: u64,\n    inner_ordinal: u32,\n    tx_hash: [u8; 32],",
        ),
        (
            "        seq,\n        // Sniff/receipt-replay events aren't bound to one canonical decoded leaf; ordinal 0 =\n        // \"start of this sequence\" (conservative for a Through cutoff — replays no later leaf).\n        inner_ordinal: 0,\n        // The sniffed/receipt-replay pools",
            "        seq,\n        inner_ordinal,\n        // The sniffed/receipt-replay pools",
        ),
        (
            "/// Build the minimal drain/resync/receipt `DirtyEvent` (no single triggering tx) and tee its pool",
            "/// Build the minimal synthetic non-feed `DirtyEvent` (no single triggering tx) and tee its pool",
        ),
        (
            "        inner_ordinal: 0,   // resync: not bound to one canonical leaf\n        tx_hash: [0u8; 32], // resync/targeted re-fetch: no single decoded triggering tx",
            "        inner_ordinal: 0,   // synthetic non-feed wake: not bound to one canonical leaf\n        tx_hash: [0u8; 32], // synthetic non-feed wake: no single decoded triggering tx",
        ),
        (
            "    // Standalone-replay anchor: the triggering feed tx. [0;32] (resync/sniff) → null.",
            "    // Standalone-replay anchor: the triggering feed tx. [0;32] (synthetic non-feed) → null.",
        ),
    ];
    for (idx, (base, green)) in BRIDGES.iter().enumerate() {
        let base_count = out.matches(base).count();
        let green_count = out.matches(green).count();
        assert_eq!(
            base_count + green_count,
            1,
            "[G-T8-SNIFF-PRODUCTION-DELTA] bridge anchor {idx} must occur once",
        );
        let needle = if base_count == 1 { *base } else { *green };
        out = out.replacen(needle, &format!("__T8_SNIFF_PRODUCTION_ANCHOR_{idx}__"), 1);
    }
    out
}

fn named_body(source: &str, needle: &str) -> Option<String> {
    let masked = mask_rust_source(source);
    let start = masked.find(needle)?;
    let open = masked[start..].find('{')? + start;
    let mut depth = 0usize;
    let close = masked
        .as_bytes()
        .iter()
        .enumerate()
        .skip(open)
        .find_map(|(idx, byte)| {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(idx);
                    }
                }
                _ => {}
            }
            None
        })?;
    Some(source[start..=close].to_string())
}

fn executable_statement_count(source: &str, statement: &str) -> usize {
    mask_rust_source(source)
        .lines()
        .map(str::trim)
        .filter(|line| *line == statement)
        .count()
}

fn positions(source: &str, needle: &str) -> Vec<usize> {
    mask_rust_source(source)
        .match_indices(needle)
        .map(|(idx, _)| idx)
        .collect()
}

fn baseline_dir() -> PathBuf {
    repo_root().join("codex/campaign_baseline_wi01")
}

fn read_baseline(name: &str) -> String {
    let path = baseline_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "WI-01 baseline artifact missing: {} ({err}) — the Phase-0 oracle is not frozen; \
             capture it with WI01_CAPTURE=1 before any later phase may diff against it",
            path.display()
        )
    })
}

fn read_baseline_json(name: &str) -> Value {
    serde_json::from_str(&read_baseline(name))
        .unwrap_or_else(|err| panic!("WI-01 baseline artifact {name} is invalid JSON: {err}"))
}

fn read_baseline_jsonl(name: &str) -> Vec<Value> {
    read_baseline(name)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(idx, line)| {
            serde_json::from_str(line).unwrap_or_else(|err| {
                panic!("WI-01 baseline {name} line {} invalid JSON: {err}", idx + 1)
            })
        })
        .collect()
}

/// A fully-populated golden record carrying every required dimension.
fn valid_golden_record() -> Value {
    json!({
        "trigger_tx_hash": format!("0x{}", "11".repeat(32)),
        "feed_sequence": 1_000u64,
        "inner_ordinal": 0u64,
        "route_static_hash": "0x1234abcd5678ef90",
        "strategy_kind": "TwoHop",
        "ordered_pool_kind_signature": "UniswapV3>PancakeV3",
        "direction": "01",
        "amount_bucket": 1u64,
        "decision": DECISION_CANDIDATE,
        "typed_decline_reason": "none",
        "risk_class": "Permissionless",
        "price_anchor_class": "stable_pair",
        "submit_eligibility": "mantis_executable",
        "amount_in": "1000000",
        "cycle_amount_out": "1020000",
    })
}

// ─────────────────────────────── RED 1: golden schema ───────────────────────────────

#[test]
fn campaign_golden_schema_rejects_missing_strategy_family_direction_and_decline_dimensions() {
    // A record accepted with ANY of the 13 dimensions absent makes every later phase diff
    // silently lossy — the validator must reject each omission, not skip the row.
    let valid = valid_golden_record();
    assert_eq!(
        campaign_oracle::validate_golden_record(0, &valid),
        Ok(()),
        "the canonical fully-populated record must validate"
    );
    for dim in REQUIRED_GOLDEN_DIMENSIONS {
        let mut record = valid.clone();
        record
            .as_object_mut()
            .expect("record is an object")
            .remove(dim);
        let verdict = campaign_oracle::validate_golden_record(7, &record);
        assert_eq!(
            verdict,
            Err(GoldenSchemaError::MissingDimension {
                row: 7,
                dimension: dim
            }),
            "a golden record missing `{dim}` must be REJECTED (got {verdict:?}) — \
             the schema enforcement is absent or vacuous"
        );
    }
    // Unknown vocabulary must be rejected too, not folded into a stringly bucket.
    for (dim, bad) in [
        ("strategy_kind", json!("WarpDrive")),
        ("ordered_pool_kind_signature", json!("NotAFamily>UniswapV3")),
        ("decision", json!("maybe")),
    ] {
        let mut record = valid_golden_record();
        record.as_object_mut().unwrap()[dim] = bad;
        assert!(
            campaign_oracle::validate_golden_record(0, &record).is_err(),
            "invalid `{dim}` vocabulary must be rejected"
        );
    }
    // A typed decline must carry a real reason, not the positive-row sentinel.
    let mut decline = valid_golden_record();
    decline.as_object_mut().unwrap()["decision"] = json!(DECISION_TYPED_DECLINE);
    assert!(
        campaign_oracle::validate_golden_record(0, &decline).is_err(),
        "decision=typed_decline with typed_decline_reason=none must be rejected"
    );
}

// ─────────────────────────────── RED 2: phase-diff anti-vacuity ───────────────────────────────

#[test]
fn phase_diff_rejects_vacuous_or_zero_positive_candidate_fixtures() {
    // An empty baseline compares vacuously — that is an ERROR, never a green diff.
    let verdict = campaign_oracle::phase_diff(&[], &[]);
    assert!(
        verdict.is_err(),
        "diffing an EMPTY baseline must be an error, got {verdict:?} — the phase gate would \
         pass while comparing nothing"
    );
    // A baseline with zero POSITIVE fixtures proves nothing about detection liveness.
    let mut decline = valid_golden_record();
    decline.as_object_mut().unwrap()["decision"] = json!(DECISION_TYPED_DECLINE);
    decline.as_object_mut().unwrap()["typed_decline_reason"] = json!("net_gate_fail");
    let all_declines = vec![decline.clone(), decline.clone()];
    let verdict = campaign_oracle::phase_diff(&all_declines, &all_declines);
    assert!(
        verdict.is_err(),
        "a zero-positive baseline must be rejected as vacuous, got {verdict:?}"
    );
    // A real baseline diffed against itself is clean: nonzero unchanged rows, no changes.
    let baseline = vec![valid_golden_record()];
    let diff = campaign_oracle::phase_diff(&baseline, &baseline)
        .expect("identical non-vacuous sets must diff cleanly");
    assert_eq!(diff.unchanged, 1, "self-diff must count the unchanged row");
    assert!(diff.changed.is_empty(), "self-diff must report no changes");
    // A candidate row silently flipping to a decline MUST surface as a changed row.
    let mut flipped = valid_golden_record();
    flipped.as_object_mut().unwrap()["decision"] = json!(DECISION_TYPED_DECLINE);
    flipped.as_object_mut().unwrap()["typed_decline_reason"] = json!("net_gate_fail");
    let diff = campaign_oracle::phase_diff(&baseline, &[flipped])
        .expect("a decision flip is a reportable diff, not an input error");
    assert_eq!(
        diff.changed.len(),
        1,
        "a positive→decline flip must be reported as exactly one changed row"
    );
}

// ─────────────────────────────── RED 3: hot payload layout ───────────────────────────────

#[test]
fn hot_payload_layout_budget_is_machine_enforced() {
    if cfg!(feature = "flag-harness") {
        eprintln!("SKIP: layout baseline is frozen for the default feature set");
        return;
    }
    let actual = engine::pipeline::hot_payload_layouts();
    assert!(
        actual.iter().any(|l| l.type_name == "Candidate")
            && actual.iter().any(|l| l.type_name == "DirtyEvent")
            && actual.iter().any(|l| l.type_name == "ExpressCandidate"),
        "layout plumbing must report the hot ring payloads"
    );
    // The enforcement itself must reject drift: a baseline whose Candidate stride differs from
    // the live layout is a budget violation, not a warning.
    let mut drifted: Vec<HotPayloadLayout> = actual.clone();
    drifted[0].size_bytes += 8;
    let verdict = latency_budgets::validate_hot_payload_layout(&drifted, &actual);
    assert!(
        verdict.is_err(),
        "a drifted layout baseline must be rejected, got {verdict:?} — the layout budget is \
         not machine-enforced"
    );
    // A live payload type missing from the baseline is equally a violation (silent new ring).
    let truncated = actual[1..].to_vec();
    assert!(
        latency_budgets::validate_hot_payload_layout(&truncated, &actual).is_err(),
        "a live payload type absent from the baseline must be rejected"
    );
    // The frozen baseline artifact must exist and match the live layout EXACTLY.
    let baseline_rows: Vec<HotPayloadLayout> =
        serde_json::from_value(read_baseline_json("hot_payload_layout.json")["payloads"].clone())
            .expect("hot_payload_layout.json payloads decode");
    latency_budgets::validate_hot_payload_layout(&baseline_rows, &actual)
        .unwrap_or_else(|err| panic!("frozen layout baseline violated: {err}"));
}

// ─────────────────────── RED 4: allocation/syscall probe coverage ───────────────────────

#[test]
fn hot_path_allocation_and_syscall_probe_covers_authority_boundaries() {
    // Negative: a measured hot-path boundary reporting allocations must be rejected.
    let leaky = json!({
        "schema": "wi01_probe_coverage.v1",
        "boundaries": {
            "frame_decode_hot_loop": {
                "status": "measured",
                "allocs_after_warmup": 3,
                "ctx_switch_delta": 0
            }
        }
    });
    let verdict = campaign_oracle::alloc_syscall_probe_coverage(&leaky);
    assert!(
        matches!(
            verdict,
            Err(campaign_oracle::ProbeCoverageError::HotPathAllocation { .. })
        ),
        "a hot-path boundary that allocates after warm-up must be rejected, got {verdict:?}"
    );
    // The committed artifact must declare EVERY §10.3 authority boundary — measured now, or
    // explicitly pending its owning work item. Silence is a failure.
    let artifact = read_baseline_json("probe_coverage.json");
    let coverage = campaign_oracle::alloc_syscall_probe_coverage(&artifact)
        .unwrap_or_else(|err| panic!("probe-coverage artifact rejected: {err}"));
    for boundary in REQUIRED_AUTHORITY_BOUNDARIES {
        let status = coverage.boundaries.get(boundary).unwrap_or_else(|| {
            panic!(
                "authority boundary `{boundary}` has NO probe-coverage declaration — a later \
                 phase could add an allocating/blocking hot path there without any gate firing"
            )
        });
        if let ProbeStatus::Measured {
            allocs_after_warmup,
            zero_alloc_contract,
            ..
        } = status
        {
            assert!(
                !zero_alloc_contract || *allocs_after_warmup == 0,
                "zero-alloc-contract boundary `{boundary}` allocated after warm-up"
            );
        }
    }
    // The boundaries measurable TODAY must be measured, not deferred — and the frame-decode
    // loop must carry the zero-alloc CONTRACT (the express walk freezes its measured count
    // instead: it legitimately allocates its per-walk scratch today).
    for measurable_now in ["frame_decode_hot_loop", "express_walk_accept_prepare"] {
        assert!(
            matches!(
                coverage.boundaries.get(measurable_now),
                Some(ProbeStatus::Measured { .. })
            ),
            "boundary `{measurable_now}` exists today and must be MEASURED, not pending"
        );
    }
    assert!(
        matches!(
            coverage.boundaries.get("frame_decode_hot_loop"),
            Some(ProbeStatus::Measured {
                zero_alloc_contract: true,
                ..
            })
        ),
        "frame_decode_hot_loop must be under the zero-alloc contract"
    );
}

// ─────────────────────── RED 5: route-authority table budget ───────────────────────

#[test]
fn route_authority_table_enforces_cell_and_total_memory_budget() {
    // Cell spec: exactly 64 bytes (§10.2). Any other declared cell is a violation.
    let verdict = latency_budgets::route_authority_table_budget(1_000, 128, 8 << 30);
    assert!(
        matches!(verdict, Err(BudgetViolation::CellSize { .. })),
        "a non-64-byte route-authority cell must be rejected, got {verdict:?}"
    );
    // Absolute cap: 512 MiB. 10M routes × 64 B = 640 MB > cap.
    let verdict = latency_budgets::route_authority_table_budget(
        10_000_000,
        ROUTE_AUTHORITY_CELL_BYTES,
        1 << 40,
    );
    assert!(
        matches!(verdict, Err(BudgetViolation::TableOverBudget { .. })),
        "a table above the 512 MiB absolute cap must be rejected, got {verdict:?}"
    );
    // Relative cap: ≤5% of the declared process budget. 1M routes × 64 B = 64 MB, which is
    // over 5% of a 1 GiB process budget (51.2 MB).
    let verdict = latency_budgets::route_authority_table_budget(
        1_000_000,
        ROUTE_AUTHORITY_CELL_BYTES,
        1 << 30,
    );
    assert!(
        matches!(verdict, Err(BudgetViolation::TableOverBudget { .. })),
        "a table above 5% of the process budget must be rejected, got {verdict:?}"
    );
    // The frozen baseline (sealed-profile route count + declared process budget) must FIT.
    let counts = read_baseline_json("route_universe_counts.json");
    let route_count = counts["route_count"]
        .as_u64()
        .expect("route_universe_counts.json pins route_count");
    let process_budget = counts["declared_process_budget_bytes"]
        .as_u64()
        .expect("route_universe_counts.json pins declared_process_budget_bytes");
    let budget = latency_budgets::route_authority_table_budget(
        route_count,
        ROUTE_AUTHORITY_CELL_BYTES,
        process_budget,
    )
    .unwrap_or_else(|err| panic!("frozen route-authority table budget violated: {err}"));
    assert!(budget.table_bytes <= ROUTE_AUTHORITY_TABLE_MAX_BYTES);
    assert!(
        route_count > 0,
        "the frozen sealed-profile route count must be nonzero"
    );
}

// ─────────────────────── RED 6: max-degree dirty-fanout budget ───────────────────────

#[test]
fn maximum_degree_dirty_fanout_has_an_absolute_state_commit_budget() {
    // The absolute per-event budget is `500 ns + 60 ns × unique_dirty_route_count` (§10.3).
    assert_eq!(
        latency_budgets::state_commit_budget_ns(0),
        STATE_COMMIT_BASE_BUDGET_NS,
        "a zero-fanout event budget must equal the 500 ns base — no budget exists"
    );
    assert_eq!(
        latency_budgets::state_commit_budget_ns(100),
        STATE_COMMIT_BASE_BUDGET_NS + 100 * STATE_COMMIT_PER_DIRTY_ROUTE_NS,
        "the fanout budget must scale at 60 ns per unique dirty route"
    );
    // A maximum-degree event whose budget exceeds the frozen RX→state-ready budget must FAIL.
    let verdict = latency_budgets::validate_state_commit_budget(1_000_000, 1_000);
    assert!(
        verdict.is_err(),
        "a max-degree fanout budget larger than the frozen RX→state-ready budget must be \
         rejected, got {verdict:?}"
    );
    // The frozen baseline's real maximum CSR degree must carry a budget that FITS.
    let counts = read_baseline_json("route_universe_counts.json");
    let max_degree = counts["max_csr_degree"]
        .as_u64()
        .expect("route_universe_counts.json pins max_csr_degree");
    let rx_budget = counts["frozen_rx_to_state_ready_budget_ns"]
        .as_u64()
        .expect("route_universe_counts.json pins frozen_rx_to_state_ready_budget_ns");
    assert!(max_degree > 0, "frozen max CSR degree must be nonzero");
    latency_budgets::validate_state_commit_budget(max_degree, rx_budget).unwrap_or_else(|err| {
        panic!("frozen max-degree state-commit budget does not fit RX→state-ready: {err}")
    });
}

// ─────────────────────── RED 7: latency-gate row admissibility ───────────────────────

#[test]
fn latency_gate_rejects_uncorrelated_stage_rows_and_insufficient_tail_samples() {
    let good_row = json!({
        "stage": "express_walk_accept_prepare",
        "candidate_correlated": true,
        "correlation_key": "route_id+seq",
        "samples": P999_MIN_CANDIDATE_CORRELATED_SAMPLES,
        "p50_ns": 200, "p99_ns": 800, "p999_ns": 2_000
    });
    // Uncorrelated rows can compare unrelated populations — they must never gate a phase.
    let mut uncorrelated = good_row.clone();
    uncorrelated.as_object_mut().unwrap()["candidate_correlated"] = json!(false);
    let verdict = latency_budgets::validate_latency_gate_rows(&[uncorrelated]);
    assert!(
        verdict.is_err(),
        "an uncorrelated stage row must be rejected, got {verdict:?} — sums of unrelated \
         percentiles are not a gate"
    );
    let mut keyless = good_row.clone();
    keyless.as_object_mut().unwrap().remove("correlation_key");
    assert!(
        latency_budgets::validate_latency_gate_rows(&[keyless]).is_err(),
        "a row without its correlation key must be rejected"
    );
    // A p999 claim with fewer than 1M candidate-correlated samples is inadmissible (§10.3).
    let mut thin_tail = good_row.clone();
    thin_tail.as_object_mut().unwrap()["samples"] =
        json!(P999_MIN_CANDIDATE_CORRELATED_SAMPLES - 1);
    let verdict = latency_budgets::validate_latency_gate_rows(&[thin_tail]);
    assert!(
        verdict.is_err(),
        "a p999 claim under 1M samples must be rejected, got {verdict:?}"
    );
    // The good row is admissible and counted.
    let summary = latency_budgets::validate_latency_gate_rows(&[good_row])
        .expect("an admissible row must validate");
    assert_eq!(summary.rows, 1);
    assert_eq!(summary.p999_rows, 1);
    // The frozen Phase-0 latency baseline must exist, validate, and carry at least one
    // admissible p999 row (aggregated across ≤1h runs — never one >1h run).
    let baseline_rows = read_baseline_jsonl("latency_gate_baseline.jsonl");
    let summary = latency_budgets::validate_latency_gate_rows(&baseline_rows)
        .unwrap_or_else(|err| panic!("frozen latency baseline rejected: {err}"));
    assert!(
        summary.p999_rows >= 1,
        "the frozen baseline must carry at least one ≥1M-sample p999 row"
    );
}

// ─────────────────────── RED 8: correction-manifest proof duty ───────────────────────

#[test]
fn correction_manifest_requires_canonical_replay_proof_for_every_changed_row() {
    let changed = standard_output_change();
    // No manifest entry at all → the change is unauthorized.
    let verdict = campaign_oracle::validate_correction_manifest(&repo_root(), &[], &changed);
    assert!(
        matches!(
            verdict,
            Err(CorrectionManifestError::UnmanifestedChange { .. })
        ),
        "a changed row without a manifest entry must be rejected, got {verdict:?}"
    );
    // An entry WITHOUT canonical replay/Anvil proof must be rejected — the diff tool must never
    // incentivize preserving known-wrong output by making proof optional.
    let mut unproven = proven_correction_entry("k1");
    unproven.as_object_mut().unwrap().remove("proof");
    let verdict = campaign_oracle::validate_correction_manifest(&repo_root(), &[unproven], &changed);
    assert!(
        matches!(verdict, Err(CorrectionManifestError::MissingProof { .. })),
        "a correction without canonical replay/Anvil proof must be rejected, got {verdict:?}"
    );
    // A forbidden transition must be rejected even when proof is attached.
    let mut forbidden = proven_correction_entry("k1");
    forbidden["transition"] = json!("candidate->silently_dropped");
    assert!(
        matches!(
            campaign_oracle::validate_correction_manifest(&repo_root(), &[forbidden], &changed),
            Err(CorrectionManifestError::ForbiddenTransition { .. })
        ),
        "a transition outside the reviewed §11.2 set must be rejected"
    );
    // A row DISAPPEARING is never authorizable (§11.2: no row may disappear).
    let removed = PhaseDiff {
        unchanged: 10,
        changed: vec![ChangedRow {
            key: "k2".into(),
            change: "removed".into(),
            old_records: vec![valid_golden_record()],
            new_records: Vec::new(),
        }],
    };
    let covering = json!({
        "key": "k2",
        "issue_id": "FL-03",
        "transition": "candidate->typed_decline",
        "old_identity": {"route": 2}, "old_output": {"amount_out": "1"},
        "new_identity": {"route": 2}, "new_output": {"amount_out": "0"},
        "proof": {"kind": "anvil", "reference": "anvil/results/x.json"}
    });
    assert!(
        matches!(
            campaign_oracle::validate_correction_manifest(&repo_root(), &[covering], &removed),
            Err(CorrectionManifestError::RowDisappeared { .. })
        ),
        "row disappearance must be rejected even with a manifest entry"
    );
    // The fully-proven, allowed correction validates — proof = a PRESENT, hash-bound,
    // schema-valid committed artifact (never a fabricated path).
    let proven = proven_correction_entry("k1");
    campaign_oracle::validate_correction_manifest(&repo_root(), &[proven], &changed)
        .expect("a proven, allowed correction must validate");
}

// ───────────── WI-01 frozen baseline: golden fixtures + expected-red manifest ─────────────
// Not part of the 8 named enforcement REDs, but part of the same Phase-0 freeze: these fail
// until the GREEN capture lands, then pin the baseline forever.

#[test]
fn golden_baseline_fixture_set_is_present_and_non_vacuous() {
    let rows = read_baseline_jsonl("golden_candidate_records.jsonl");
    let summary = campaign_oracle::validate_golden_fixture_set(&rows)
        .unwrap_or_else(|err| panic!("frozen golden fixture set rejected: {err}"));
    assert!(
        summary.positives > 0,
        "the frozen baseline must carry positive candidate fixtures"
    );
    // Every one of the 18 families is accounted for: either it carries golden rows, or it is
    // EXPLICITLY recorded as blocked-on-seed / gated in baseline_manifest.json with a reason
    // (the WI-01 money brief forbids seeding and forbids inventing positives — but a family
    // silently absent from BOTH is a hole in the oracle).
    let manifest = read_baseline_json("baseline_manifest.json");
    let blocked = manifest["blocked_families"]
        .as_object()
        .expect("baseline_manifest.json blocked_families object");
    for family in POOL_KIND_CATALOG {
        let has_rows = summary.rows_by_family.get(family).copied().unwrap_or(0) > 0;
        let blocked_reason = blocked.get(family).and_then(Value::as_str);
        assert!(
            has_rows || blocked_reason.is_some_and(|r| !r.is_empty()),
            "family `{family}` has no golden rows AND no blocked-on-seed record — the oracle \
             is silently blind to it under ACTIVE_FAMILIES=all"
        );
        assert!(
            !(has_rows && blocked_reason.is_some()),
            "family `{family}` is both walked and marked blocked — the manifest is stale"
        );
    }
    for strategy in STRATEGY_KIND_CATALOG {
        assert!(
            summary
                .positives_by_strategy
                .get(strategy)
                .copied()
                .unwrap_or(0)
                > 0,
            "strategy `{strategy}` has zero positive fixtures — the oracle is vacuous for it"
        );
    }
    assert!(
        summary.positives_by_direction.len() >= 2,
        "both directions must carry positive fixtures"
    );
    assert!(
        summary.positives_by_amount_bucket.len() >= 2,
        "at least two amount buckets must carry positive fixtures"
    );
}

#[test]
fn expected_red_manifest_is_frozen_and_signed() {
    let manifest = read_baseline_json("expected_red_manifest.json");
    // The exact five protected freeze-tripwires plus the dirty-tree red — never repinned,
    // never waived, and no new red may be reclassified into this set.
    let tripwires: Vec<&str> = manifest["freeze_tripwires"]
        .as_array()
        .expect("freeze_tripwires array")
        .iter()
        .map(|v| v.as_str().expect("tripwire name"))
        .collect();
    assert_eq!(
        tripwires,
        vec![
            "amm_family_detector_matrix_static",
            "gas_economics_matrix_static",
            "gas_economics_current_boundary_static",
            "laptop_only_certification_boundary_static",
            "engine-feed/engine_build_blocker_static",
        ],
        "the protected tripwire set is EXACTLY these five"
    );
    assert_eq!(
        manifest["dirty_tree_red"].as_str(),
        Some("deployment_atomicity_dirty_tree_static"),
        "the dirty-tree red is tracked separately from the five tripwires"
    );
    // Serial-flake allowlist. The codex §4 bar is exactly three current-baseline names, with
    // the third RECOVERED from authoritative baseline output — never invented. The recovery
    // campaign (AUDIT1 doc sweep + raw-log audit + 32 clean parallel suite runs at the RED
    // commit) could NOT surface a third name: the manifest therefore carries the two
    // source-confirmed names fully qualified, PLUS an explicit BLOCKED-UNRECOVERED record for
    // the third slot with the complete evidence trail. Flipping this test to three names
    // requires REAL recovered evidence (authoritative failing output + 3× isolated -j1
    // passes) — inventing or promoting a new failure into the slot is forbidden.
    let flakes = manifest["serial_flake_allowlist"]
        .as_array()
        .expect("serial_flake_allowlist array");
    assert_eq!(
        flakes.len(),
        2,
        "exactly the two source-confirmed serial flakes are qualified today; the third slot \
         is tracked in third_name_recovery (BLOCKED, never invented)"
    );
    for name in [
        "revm_fork::tests::learned_readset_converges_repeat_validation_without_probe_iterations",
        "shadow_revm::tests::close_block_full_resync_latch_blocks_heal_and_publish",
    ] {
        assert!(
            flakes.iter().any(|f| f["name"].as_str() == Some(name)),
            "source-confirmed serial flake `{name}` missing from the allowlist"
        );
    }
    for flake in flakes {
        assert!(
            flake["evidence"].as_str().is_some_and(|s| !s.is_empty()),
            "every allowlisted flake must carry recovery evidence: {flake}"
        );
        assert_eq!(
            flake["isolated_j1_consecutive_passes"].as_u64(),
            Some(3),
            "every allowlisted flake must have passed 3 consecutive isolated -j1 runs: {flake}"
        );
        assert!(
            flake["isolated_proof"].as_str().is_some_and(|s| !s.is_empty()),
            "every allowlisted flake must reference its isolated-run proof: {flake}"
        );
    }
    let recovery = &manifest["third_name_recovery"];
    // codex WI-01 review §5 formally endorsed TWO and corrected the "exactly three" premise
    // (plan update da7f12a): the third slot is retired as premise-corrected, never invented.
    assert_eq!(
        recovery["status"].as_str(),
        Some("PREMISE-CORRECTED-TWO"),
        "the third-name slot is retired per the codex §5 adjudication (plan commit da7f12a);          any future addition needs authoritative parallel-failure output + 3x isolated passes"
    );
    assert!(
        recovery["recovery_evidence"]
            .as_array()
            .is_some_and(|e| e.len() >= 3),
        "the BLOCKED third-name record must carry the full recovery-evidence trail"
    );
    assert!(
        recovery["resolution_path"].as_str().is_some_and(|s| !s.is_empty()),
        "the BLOCKED third-name record must state its resolution path"
    );
    // The manifest is signed with the exact tree state it froze.
    for key in ["head_commit", "git_status_sha256", "config_sha256"] {
        assert!(
            manifest[key].as_str().is_some_and(|s| !s.is_empty()),
            "expected-red manifest must be signed with `{key}`"
        );
    }
}

/// Guard: the baseline directory itself is committed alongside these tests.
#[test]
fn baseline_directory_exists() {
    let dir = baseline_dir();
    assert!(
        Path::new(&dir).exists(),
        "WI-01 baseline dir missing: {} — Phase 0 has not frozen its oracle",
        dir.display()
    );
}

// ═══════════════════════ WI-01b — oracle-completeness closure (codex NEEDS-FIX) ═══════════════════════
// codex/WI01_VERIFY_2026-07-17.md gaps 1/2/4/5 + extras. RED discipline: these compile and fail
// for the intended reason (enforcement absent / artifact field missing) until the GREEN lands.

/// sha256 of a committed proof-fixture file, via the same system tool the other static tests use.
fn sha256_of(path: &Path) -> String {
    let out = std::process::Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("run sha256sum");
    assert!(out.status.success(), "sha256sum failed for {}", path.display());
    String::from_utf8(out.stdout)
        .expect("utf8")
        .split_whitespace()
        .next()
        .expect("hash")
        .to_string()
}

/// A fully-bound correction entry for the given old/new records: manifest old/new fields are
/// the CANONICAL payloads (machine-derived, never hand-invented), the transition is derived
/// from the records, and the proof file (written under the proof root) carries the SAME
/// canonical replay payloads with typed source pins.
fn proven_entry_for(key: &str, tag: &str, old_records: &[Value], new_records: &[Value]) -> Value {
    let old_identity = campaign_oracle::canonical_identity_payload(old_records);
    let new_identity = campaign_oracle::canonical_identity_payload(new_records);
    let old_output = campaign_oracle::canonical_outcome_payload(old_records);
    let new_output = campaign_oracle::canonical_outcome_payload(new_records);
    let decision = |records: &[Value]| records[0]["decision"].as_str().unwrap().to_string();
    let transition = match (decision(old_records).as_str(), decision(new_records).as_str()) {
        ("candidate", "candidate") => "candidate->corrected_candidate",
        ("candidate", "typed_decline") => "candidate->typed_decline",
        _ => "typed_decline->proven_candidate",
    };
    let proof_doc = json!({
        "schema": "wi01_canonical_replay_proof.v1",
        "source": {
            "commit": "cb31077da689cfecdc6f203124b6bddcb319521c",
            "config_sha256": sha256_of(&repo_root().join("engine/Cargo.lock")),
            "corpus": "synthetic test fixture"
        },
        "replay": {
            "old_identity": old_identity, "old_output": old_output,
            "new_identity": new_identity, "new_output": new_output,
        }
    });
    let rel = format!(
        "codex/campaign_baseline_wi01/proof_fixtures/tmp_test_{}_{tag}.json",
        std::process::id()
    );
    let path = repo_root().join(&rel);
    std::fs::write(&path, serde_json::to_string_pretty(&proof_doc).unwrap())
        .expect("write test proof under the proof root");
    json!({
        "key": key,
        "issue_id": "FL-07",
        "transition": transition,
        "old_identity": old_identity, "old_output": old_output,
        "new_identity": new_identity, "new_output": new_output,
        "proof": {"kind": "canonical_replay", "reference": rel, "sha256": sha256_of(&path)}
    })
}

/// The standard entry matching [`standard_output_change`], backed by the COMMITTED fixture.
fn proven_correction_entry(key: &str) -> Value {
    let old_row = valid_golden_record();
    let mut new_row = valid_golden_record();
    new_row.as_object_mut().unwrap()["cycle_amount_out"] = json!("1030000");
    let rel = "codex/campaign_baseline_wi01/proof_fixtures/canonical_replay_example.json";
    json!({
        "key": key,
        "issue_id": "FL-07",
        "transition": "candidate->corrected_candidate",
        "old_identity": campaign_oracle::canonical_identity_payload(&[old_row.clone()]),
        "old_output": campaign_oracle::canonical_outcome_payload(&[old_row]),
        "new_identity": campaign_oracle::canonical_identity_payload(&[new_row.clone()]),
        "new_output": campaign_oracle::canonical_outcome_payload(&[new_row]),
        "proof": {"kind": "canonical_replay", "reference": rel,
                   "sha256": sha256_of(&repo_root().join(rel))}
    })
}

/// Gap 1 (CRITICAL): a changed candidate OUTPUT or grouping dimension must surface as a changed
/// row — today phase_diff compares only the 8-field identity + decision/reason and reports these
/// real regressions as all-clean.
#[test]
fn phase_diff_detects_output_and_grouping_field_drift() {
    let baseline = vec![valid_golden_record()];
    for (field, mutated) in [
        ("amount_in", json!("2000000")),
        ("cycle_amount_out", json!("999999")),
        ("submit_eligibility", json!("fail_closed_no_exchange_id")),
        ("risk_class", json!("CentralisedStable")),
        ("price_anchor_class", json!("lst_peg")),
    ] {
        let mut row = valid_golden_record();
        row.as_object_mut().unwrap()[field] = mutated;
        let diff = campaign_oracle::phase_diff(&baseline, &[row])
            .expect("a field mutation is a reportable diff, not an input error");
        assert_eq!(
            diff.changed.len(),
            1,
            "a mutated `{field}` MUST surface as exactly one changed row — got an all-clean \
             diff (unchanged={}): the oracle is blind to {field} drift",
            diff.unchanged
        );
    }
}

/// Gap 2a: a correction whose proof artifact does not exist, or whose hash does not match, is a
/// FABRICATED proof and must be rejected — proof metadata alone is not proof.
#[test]
fn correction_manifest_rejects_fabricated_or_unbound_proof_artifacts() {
    let changed = standard_output_change();
    // Nonexistent artifact path (everything else canonical — only the missing file rejects).
    let mut fabricated = proven_correction_entry("k1");
    fabricated["proof"]["reference"] =
        json!("codex/campaign_baseline_wi01/proof_fixtures/replay_1.json");
    let verdict = campaign_oracle::validate_correction_manifest(&repo_root(), &[fabricated], &changed);
    assert!(
        matches!(verdict, Err(CorrectionManifestError::MissingProof { .. })),
        "a proof reference that does not resolve to a real artifact must be rejected, got \
         {verdict:?} — fabricated references currently pass"
    );
    // Present artifact, wrong hash binding.
    let mut wrong_hash = proven_correction_entry("k1");
    wrong_hash["proof"]["sha256"] = json!("ab".repeat(32));
    let verdict =
        campaign_oracle::validate_correction_manifest(&repo_root(), &[wrong_hash], &changed);
    assert!(
        matches!(verdict, Err(CorrectionManifestError::MissingProof { .. })),
        "a proof whose sha256 does not match the artifact must be rejected, got {verdict:?}"
    );
    // Missing hash binding entirely.
    let mut unbound = proven_correction_entry("k1");
    unbound["proof"].as_object_mut().unwrap().remove("sha256");
    let verdict = campaign_oracle::validate_correction_manifest(&repo_root(), &[unbound], &changed);
    assert!(
        matches!(verdict, Err(CorrectionManifestError::MissingProof { .. })),
        "a proof without its hash binding must be rejected, got {verdict:?}"
    );
}

/// Gap 2b: the transition must be DERIVED from the actual old/new records; a forbidden actual
/// transition relabelled with an allowed string must be rejected.
#[test]
fn correction_manifest_derives_transition_from_actual_records() {
    // Actual: candidate -> typed_decline. Declared: candidate->corrected_candidate (mislabel).
    let old_row = valid_golden_record();
    let mut new_row = valid_golden_record();
    new_row.as_object_mut().unwrap()["decision"] = json!(DECISION_TYPED_DECLINE);
    new_row.as_object_mut().unwrap()["typed_decline_reason"] = json!("net_gate_fail");
    new_row.as_object_mut().unwrap()["cycle_amount_out"] = json!(null);
    let changed = PhaseDiff {
        unchanged: 0,
        changed: vec![ChangedRow {
            key: "k1".into(),
            change: "changed".into(),
            old_records: vec![old_row],
            new_records: vec![new_row],
        }],
    };
    let mislabelled = proven_correction_entry("k1"); // declares candidate->corrected_candidate
    let verdict =
        campaign_oracle::validate_correction_manifest(&repo_root(), &[mislabelled], &changed);
    assert!(
        matches!(
            verdict,
            Err(CorrectionManifestError::ForbiddenTransition { .. })
        ),
        "a declared transition contradicting the actual old/new records must be rejected, got \
         {verdict:?} — transition truth is currently unchecked"
    );
}

/// Gap 2c (inverse): the plan's OWN identity corrections (FL-03 inner_ordinal, FL-07 route) are
/// removed+added pairs — a manifest entry linking old/new keys with proof must LEGALIZE the pair
/// instead of dying on RowDisappeared.
#[test]
fn correction_manifest_represents_identity_corrections_via_linked_keys() {
    let old_row = valid_golden_record();
    let mut new_row = valid_golden_record();
    new_row.as_object_mut().unwrap()["inner_ordinal"] = json!(1u64); // FL-03 class
    let diff = campaign_oracle::phase_diff(&[old_row.clone()], &[new_row.clone()])
        .expect("identity correction diffs cleanly");
    assert_eq!(diff.changed.len(), 2, "identity change = removed + added pair");
    let old_key = diff
        .changed
        .iter()
        .find(|r| r.change == "removed")
        .expect("removed side")
        .key
        .clone();
    let new_key = diff
        .changed
        .iter()
        .find(|r| r.change == "added")
        .expect("added side")
        .key
        .clone();
    let mut entry =
        proven_entry_for(&old_key, "fl03_positive", &[old_row.clone()], &[new_row.clone()]);
    entry["issue_id"] = json!("FL-03");
    entry["identity_correction"] = json!({"old_key": old_key, "new_key": new_key});
    campaign_oracle::validate_correction_manifest(&repo_root(), &[entry], &diff).unwrap_or_else(
        |err| {
            panic!(
                "an FL-03-class identity correction with linked keys + present proof must \
                 validate, got: {err} — the plan's own corrections cannot be represented"
            )
        },
    );
}

/// Gap 4a: the §10.3 PHASE-ACCEPTANCE gate must enforce run protocol, not just row schema.
#[test]
fn phase_acceptance_gate_enforces_ab_protocol() {
    let compliant = json!({
        "build_profile": "release",
        "pinned_cores": true,
        "interleaved_repetitions": 30,
        "paired_baseline_candidate": true,
        "p50_threshold_pct": 1.0, "p99_threshold_pct": 3.0,
        "p999_upper_ci_threshold_pct": 5.0
    });
    latency_budgets::validate_phase_acceptance_run(&compliant)
        .expect("a compliant acceptance-run metadata doc must validate");
    for (field, bad) in [
        ("build_profile", json!("debug")),
        ("pinned_cores", json!(false)),
        ("interleaved_repetitions", json!(29)),
        ("paired_baseline_candidate", json!(false)),
    ] {
        let mut meta = compliant.clone();
        meta.as_object_mut().unwrap()[field] = bad;
        assert!(
            latency_budgets::validate_phase_acceptance_run(&meta).is_err(),
            "acceptance metadata with non-compliant `{field}` must be rejected — the §10.3 \
             gate is currently unenforced"
        );
    }
    let mut missing = compliant.clone();
    missing.as_object_mut().unwrap().remove("p999_upper_ci_threshold_pct");
    assert!(
        latency_budgets::validate_phase_acceptance_run(&missing).is_err(),
        "acceptance metadata missing a regression threshold must be rejected"
    );
}

/// Gap 4b: the WI-06/WI-08 pending placeholders must be ARMED tripwires — each pending probe
/// boundary declares its binding tripwire, and the moment the live type lands the pending status
/// becomes a failure.
#[test]
fn wi08_live_binding_tripwires_are_armed() {
    let artifact = read_baseline_json("probe_coverage.json");
    let boundaries = artifact["boundaries"].as_object().expect("boundaries");
    for (name, decl) in boundaries {
        if decl["status"].as_str() == Some("pending_work_item") {
            assert_eq!(
                decl["binding_tripwire"].as_bool(),
                Some(true),
                "pending boundary `{name}` must carry an ARMED binding_tripwire so the owing \
                 work item cannot land its live type while the probe stays pending"
            );
        }
    }
    // The tripwire itself: if the live RouteAuthorityCell type exists anywhere in engine
    // sources, pending is no longer acceptable — the cell must be measured and layout-frozen.
    let found_cell = source_tree_contains_route_authority_cell(&repo_root().join("engine/crates"));
    if found_cell {
        assert_eq!(
            boundaries["route_cell_rewrite_under_paused_reader"]["status"].as_str(),
            Some("measured"),
            "RouteAuthorityCell exists in source: the probe boundary must be MEASURED and the \
             64-byte cell layout must be live-bound (WI-08 landing duty)"
        );
    }
}

/// Gap 5: the baseline must be hash-bound to a clean committed capture — every artifact's
/// sha256 pinned in the manifest, captured from a clean tree at a named commit.
#[test]
fn baseline_artifacts_are_hash_bound_to_a_clean_committed_capture() {
    let manifest = read_baseline_json("baseline_manifest.json");
    assert_eq!(
        manifest["clean_tree"].as_bool(),
        Some(true),
        "the baseline must declare (and have been captured from) a CLEAN committed tree"
    );
    let hashes = manifest["artifact_sha256"]
        .as_object()
        .expect("baseline_manifest must pin artifact_sha256 for every capture artifact");
    for name in [
        "golden_candidate_records.jsonl",
        "hot_payload_layout.json",
        "route_universe_counts.json",
        "probe_coverage.json",
        "latency_gate_baseline.jsonl",
        "known_good_express_corpus.json",
        // WI-01d nit: the expected-red manifest is REQUIRED membership here so a future edit
        // cannot drop it from both the list and the hash map in tandem.
        "expected_red_manifest.json",
    ] {
        let pinned = hashes[name]
            .as_str()
            .unwrap_or_else(|| panic!("artifact_sha256 missing `{name}`"));
        let actual = sha256_of(&baseline_dir().join(name));
        assert_eq!(
            pinned, actual,
            "artifact `{name}` drifted from its manifest hash binding"
        );
    }
    // The config signature must RECOMPUTE, not merely exist (codex Gap 5: nonempty is not a
    // binding).
    let pinned_config = manifest["config_sha256"].as_str().expect("config_sha256");
    let actual_config = sha256_of(&repo_root().join("engine/Cargo.lock"));
    assert_eq!(
        pinned_config, actual_config,
        "config_sha256 must equal sha256(engine/Cargo.lock) at verification time"
    );
    // The capture commit must be a real commit object in this repository.
    let head = manifest["captured_at_commit"].as_str().expect("captured_at_commit");
    let ok = std::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{head}^{{commit}}")])
        .current_dir(repo_root())
        .status()
        .expect("git cat-file")
        .success();
    assert!(ok, "captured_at_commit `{head}` is not a commit in this repository");
}

/// Extras: every golden row carries its evidence class, and the Kyber rows are labelled as
/// synthetic KERNEL-capability evidence (production startup currently drops Kyber pools —
/// startup.rs exclusion + the strict parity artifact agree), setting up the WI-02 reviewed
/// candidate->typed-decline correction instead of a silent regression.
#[test]
fn golden_rows_carry_evidence_class_and_kyber_is_kernel_capability_only() {
    let rows = read_baseline_jsonl("golden_candidate_records.jsonl");
    for (idx, row) in rows.iter().enumerate() {
        let class = row["evidence_class"].as_str().unwrap_or_else(|| {
            panic!("golden row {idx} missing evidence_class — rows must state what they prove")
        });
        if row["ordered_pool_kind_signature"]
            .as_str()
            .unwrap_or("")
            .contains("KyberSwapElastic")
        {
            assert_eq!(
                class, "synthetic_kernel_capability_startup_excluded",
                "Kyber rows prove kernel capability ONLY (startup drops Kyber pools; parity \
                 artifact marks it unsupported) — row {idx} is mislabelled"
            );
        } else {
            assert_eq!(class, "synthetic_walk", "row {idx} evidence class");
        }
    }
}

// ═══════════════ WI-01c — oracle BINDING closure (codex NEEDS-FIX-ORACLE-BINDING) ═══════════════
// codex/WI01B_VERIFY_2026-07-17.md. RED discipline: compiling, failing at assertions.

/// The standard changed-row diff used by the binding tests: cycle_amount_out 1020000 -> 1030000.
fn standard_output_change() -> PhaseDiff {
    let old_row = valid_golden_record();
    let mut new_row = valid_golden_record();
    new_row.as_object_mut().unwrap()["cycle_amount_out"] = json!("1030000");
    PhaseDiff {
        unchanged: 0,
        changed: vec![ChangedRow {
            key: "k1".into(),
            change: "changed".into(),
            old_records: vec![old_row],
            new_records: vec![new_row],
        }],
    }
}

/// Gap 2a: a hash-valid but UNRELATED proof must not authorize a correction — the proof's
/// replay old/new payloads must EQUAL the canonical values derived from the changed records.
#[test]
fn correction_proof_must_bind_the_actual_old_new_values() {
    let diff = standard_output_change();
    // A hash-valid proof whose replay payloads describe a DIFFERENT change (amount_in
    // 999 -> 998) must not authorize the actual 1020000 -> 1030000 change: file identity is
    // not correction identity.
    let mut fab_old = valid_golden_record();
    fab_old.as_object_mut().unwrap()["amount_in"] = json!("999");
    let mut fab_new = valid_golden_record();
    fab_new.as_object_mut().unwrap()["amount_in"] = json!("998");
    let fabricated = proven_entry_for("k1", "fabricated", &[fab_old], &[fab_new]);
    let mut unrelated = proven_correction_entry("k1");
    unrelated["proof"] = fabricated["proof"].clone();
    let verdict =
        campaign_oracle::validate_correction_manifest(&repo_root(), &[unrelated], &diff);
    assert!(
        matches!(
            verdict,
            Err(CorrectionManifestError::ProofIdentityMismatch { .. })
        ),
        "a proof whose replay payloads do not equal the canonical old/new values derived from \
         ChangedRow must be rejected, got {verdict:?} — a hash-valid unrelated file currently \
         proves an arbitrary correction"
    );
}

/// Gap 2b: proof schemas are TYPED per kind and source pins are validated, not shape-checked.
#[test]
fn correction_proof_schemas_and_source_pins_are_typed() {
    let diff = standard_output_change();
    // Placeholder pins (the pre-WI-01c "example-fixture" class), a wrong schema, and a short
    // commit pin must each be rejected by the TYPED schema/pin checks — even when the file
    // exists and hash-matches.
    let fixture_doc: Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(
            "codex/campaign_baseline_wi01/proof_fixtures/canonical_replay_example.json",
        ))
        .expect("committed fixture"),
    )
    .expect("fixture json");
    let mutations: [(&str, fn(&mut Value)); 3] = [
        ("placeholder_pin", |d| d["source"]["config_sha256"] = json!("example-fixture")),
        ("wrong_schema", |d| d["schema"] = json!("some_other_schema.v9")),
        ("short_commit", |d| d["source"]["commit"] = json!("abc123")),
    ];
    for (tag, mutate) in mutations {
        let mut doc = fixture_doc.clone();
        mutate(&mut doc);
        let rel = format!(
            "codex/campaign_baseline_wi01/proof_fixtures/tmp_test_{}_{tag}.json",
            std::process::id()
        );
        let path = repo_root().join(&rel);
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).expect("write");
        let mut entry = proven_correction_entry("k1");
        entry["proof"]["reference"] = json!(rel);
        entry["proof"]["sha256"] = json!(sha256_of(&path));
        let verdict =
            campaign_oracle::validate_correction_manifest(&repo_root(), &[entry], &diff);
        std::fs::remove_file(&path).ok();
        assert!(
            matches!(verdict, Err(CorrectionManifestError::MissingProof { .. })),
            "proof with `{tag}` must be rejected by the typed schema/pin checks, got {verdict:?}"
        );
    }
}

/// Gap 2c: transition derivation is TOTAL over the record multiset.
#[test]
fn correction_transition_derivation_is_total() {
    // Mixed-decision old multiset must be rejected outright.
    let old_a = valid_golden_record();
    let mut old_b = valid_golden_record();
    old_b.as_object_mut().unwrap()["decision"] = json!(DECISION_TYPED_DECLINE);
    old_b.as_object_mut().unwrap()["typed_decline_reason"] = json!("net_gate_fail");
    old_b.as_object_mut().unwrap()["cycle_amount_out"] = json!(null);
    let mut new_row = valid_golden_record();
    new_row.as_object_mut().unwrap()["cycle_amount_out"] = json!("1030000");
    let mixed = PhaseDiff {
        unchanged: 0,
        changed: vec![ChangedRow {
            key: "k1".into(),
            change: "changed".into(),
            old_records: vec![old_a.clone(), old_b.clone()],
            new_records: vec![new_row.clone()],
        }],
    };
    let entry = proven_correction_entry("k1");
    assert!(
        campaign_oracle::validate_correction_manifest(&repo_root(), &[entry.clone()], &mixed)
            .is_err(),
        "a mixed-decision old multiset must be rejected — first-record-only derivation is not total"
    );
    // decline -> decline drift must NOT be authorizable as candidate->corrected_candidate.
    let mut old_decline = old_b.clone();
    old_decline.as_object_mut().unwrap()["typed_decline_reason"] = json!("net_gate_fail");
    let mut new_decline = old_b.clone();
    new_decline.as_object_mut().unwrap()["typed_decline_reason"] = json!("cycle_not_profitable");
    let decline_drift = PhaseDiff {
        unchanged: 0,
        changed: vec![ChangedRow {
            key: "k1".into(),
            change: "changed".into(),
            old_records: vec![old_decline],
            new_records: vec![new_decline],
        }],
    };
    let verdict = campaign_oracle::validate_correction_manifest(
        &repo_root(),
        &[proven_correction_entry("k1")],
        &decline_drift,
    );
    assert!(
        matches!(
            verdict,
            Err(CorrectionManifestError::ForbiddenTransition { .. })
        ),
        "decline->decline drift relabelled candidate->corrected_candidate must be rejected, \
         got {verdict:?}"
    );
    // A STANDALONE added row must not bypass the actual-transition check with an arbitrary
    // allowed label (its only legal class is typed_decline->proven_candidate... which requires
    // a matching removed decline via an identity link, not a bare addition).
    let added_only = PhaseDiff {
        unchanged: 0,
        changed: vec![ChangedRow {
            key: "k1".into(),
            change: "added".into(),
            old_records: Vec::new(),
            new_records: vec![valid_golden_record()],
        }],
    };
    let verdict = campaign_oracle::validate_correction_manifest(
        &repo_root(),
        &[proven_correction_entry("k1")],
        &added_only,
    );
    assert!(
        matches!(
            verdict,
            Err(CorrectionManifestError::ForbiddenTransition { .. })
        ),
        "a standalone added row with a corrected_candidate label must be rejected, got {verdict:?}"
    );
}

/// Gap 2d: identity links are BIJECTIVE, self-consistent, and restricted to the reviewed
/// FL-03/FL-07 identity fields.
#[test]
fn identity_links_are_bijective_and_restricted() {
    let old_row = valid_golden_record();
    let mut new_row = valid_golden_record();
    new_row.as_object_mut().unwrap()["inner_ordinal"] = json!(1u64);
    let diff = campaign_oracle::phase_diff(&[old_row.clone()], &[new_row.clone()])
        .expect("identity change diffs");
    let old_key = diff.changed.iter().find(|r| r.change == "removed").unwrap().key.clone();
    let new_key = diff.changed.iter().find(|r| r.change == "added").unwrap().key.clone();
    // (1) entry.key must equal the removed old_key — a link keyed elsewhere is inconsistent.
    let mut mis_keyed = proven_correction_entry("some_other_key");
    mis_keyed["issue_id"] = json!("FL-03");
    mis_keyed["identity_correction"] = json!({"old_key": old_key, "new_key": new_key});
    assert!(
        campaign_oracle::validate_correction_manifest(&repo_root(), &[mis_keyed], &diff).is_err(),
        "an identity link whose entry.key != old_key must be rejected"
    );
    // (2) two removals must not launder through ONE addition (many-to-one).
    let mut old2 = valid_golden_record();
    old2.as_object_mut().unwrap()["feed_sequence"] = json!(2_000u64);
    let diff2 = campaign_oracle::phase_diff(
        &[old_row.clone(), old2.clone()],
        &[new_row.clone()],
    )
    .expect("two removals + one addition diffs");
    let removed_keys: Vec<String> = diff2
        .changed
        .iter()
        .filter(|r| r.change == "removed")
        .map(|r| r.key.clone())
        .collect();
    assert_eq!(removed_keys.len(), 2, "fixture: two removals");
    let added_key = diff2
        .changed
        .iter()
        .find(|r| r.change == "added")
        .unwrap()
        .key
        .clone();
    let entries: Vec<Value> = removed_keys
        .iter()
        .map(|rk| {
            let mut e = proven_correction_entry(rk);
            e["issue_id"] = json!("FL-03");
            e["identity_correction"] = json!({"old_key": rk, "new_key": added_key});
            e
        })
        .collect();
    assert!(
        campaign_oracle::validate_correction_manifest(&repo_root(), &entries, &diff2).is_err(),
        "TWO removals linking to ONE addition must be rejected — the non-bijective link \
         launders a real disappearance"
    );
    // (3) a link whose old/new records differ in a NON-reviewed identity field (direction)
    // must be rejected: only inner_ordinal (FL-03) / route_static_hash (FL-07) may change.
    let mut dir_changed = valid_golden_record();
    dir_changed.as_object_mut().unwrap()["direction"] = json!("10");
    let diff3 = campaign_oracle::phase_diff(&[old_row.clone()], &[dir_changed])
        .expect("direction change diffs");
    let ok3 = diff3.changed.iter().find(|r| r.change == "removed").unwrap().key.clone();
    let nk3 = diff3.changed.iter().find(|r| r.change == "added").unwrap().key.clone();
    let mut dir_link = proven_correction_entry(&ok3);
    dir_link["issue_id"] = json!("FL-03");
    dir_link["identity_correction"] = json!({"old_key": ok3, "new_key": nk3});
    assert!(
        campaign_oracle::validate_correction_manifest(&repo_root(), &[dir_link], &diff3).is_err(),
        "an identity link changing DIRECTION (not a reviewed FL-03/FL-07 field) must be rejected"
    );
}

/// Gap 2e: proof references are committed artifacts under the proof root — no absolute paths,
/// no `..` escapes.
#[test]
fn proof_references_cannot_escape_the_proof_root() {
    let diff = standard_output_change();
    // An OUT-OF-ROOT file that is otherwise fully valid (same bytes as the committed proof
    // fixture): only the path policy can reject it — hash, schema, and source all validate.
    let outside = std::env::temp_dir().join(format!("wi01c_escape_{}.json", std::process::id()));
    std::fs::copy(
        repo_root().join("codex/campaign_baseline_wi01/proof_fixtures/canonical_replay_example.json"),
        &outside,
    )
    .expect("copy fixture outside the proof root");
    let escape_abs = outside.to_str().expect("utf8 path").to_string();
    let escape_rel = format!(
        "codex/campaign_baseline_wi01/proof_fixtures/../../../{}",
        "codex/campaign_baseline_wi01/proof_fixtures/canonical_replay_example.json"
    );
    for reference in [escape_abs.as_str(), escape_rel.as_str()] {
        let mut entry = proven_correction_entry("k1");
        entry["proof"]["reference"] = json!(reference);
        let verdict =
            campaign_oracle::validate_correction_manifest(&repo_root(), &[entry], &diff);
        assert!(
            matches!(verdict, Err(CorrectionManifestError::MissingProof { .. })),
            "proof reference `{reference}` escapes/dodges the proof root and must be rejected              by the PATH policy, got {verdict:?}"
        );
    }
    std::fs::remove_file(&outside).ok();
}

/// Gap 4: the §10.3 acceptance thresholds are FIXED (+1%/+3%/+5%) — weakened or absurd
/// declared thresholds are invalid runs.
#[test]
fn phase_acceptance_thresholds_are_the_fixed_10_3_limits() {
    let compliant = json!({
        "build_profile": "release", "pinned_cores": true,
        "interleaved_repetitions": 30, "paired_baseline_candidate": true,
        "p50_threshold_pct": 1.0, "p99_threshold_pct": 3.0,
        "p999_upper_ci_threshold_pct": 5.0
    });
    latency_budgets::validate_phase_acceptance_run(&compliant).expect("fixed limits validate");
    for (field, bad) in [
        ("p50_threshold_pct", json!(999.0)),
        ("p50_threshold_pct", json!(-1.0)),
        ("p99_threshold_pct", json!(30.0)),
        ("p999_upper_ci_threshold_pct", json!(50.0)),
    ] {
        let mut meta = compliant.clone();
        meta.as_object_mut().unwrap()[field] = bad.clone();
        assert!(
            latency_budgets::validate_phase_acceptance_run(&meta).is_err(),
            "declared {field}={bad} weaker than the fixed §10.3 limit must be rejected"
        );
    }
}

/// Gap 5 completion: the artifact list and hash map must be EXACTLY equal (expected_red
/// included), the status/config signatures must recompute, and flake proofs must reference
/// committed hash-checked logs.
#[test]
fn provenance_covers_every_listed_artifact_and_verifies_signatures() {
    let manifest = read_baseline_json("baseline_manifest.json");
    let listed: std::collections::BTreeSet<String> = manifest["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .map(|v| v.as_str().expect("artifact name").to_string())
        .collect();
    let hashed: std::collections::BTreeSet<String> = manifest["artifact_sha256"]
        .as_object()
        .expect("artifact_sha256")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        listed, hashed,
        "every listed artifact (incl. expected_red_manifest.json) must be hash-pinned — \
         list/hash-map drift is a provenance hole"
    );
    for name in &hashed {
        assert_eq!(
            manifest["artifact_sha256"][name].as_str().expect("hash"),
            sha256_of(&baseline_dir().join(name)),
            "artifact `{name}` hash drift"
        );
    }
    // The empty-porcelain signature must BE the SHA-256 of the empty string, recomputed.
    assert_eq!(
        manifest["git_status_sha256"].as_str(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        "git_status_sha256 must equal SHA256(empty) — a clean-tree capture, verified not trusted"
    );
    // Flake proof references must point at COMMITTED logs and hash-verify.
    let red = read_baseline_json("expected_red_manifest.json");
    for flake in red["serial_flake_allowlist"].as_array().expect("allowlist") {
        let proof_ref = flake["isolated_proof"].as_str().expect("isolated_proof");
        assert!(
            proof_ref.contains("codex/campaign_baseline_wi01/logs/"),
            "flake proof must reference the COMMITTED log, got `{proof_ref}` (obsolete \
             scratchpad reference)"
        );
    }
    assert!(
        !red["third_name_recovery"]["rule"]
            .as_str()
            .unwrap_or("")
            .contains("exactly three"),
        "third_name_recovery.rule still carries the obsolete exactly-three premise prose"
    );
}

/// NIT: one parameterized phase-diff mutation per required field (+ a future field): outcome
/// drift ⇒ exactly one changed row; identity drift ⇒ removed+added pair.
#[test]
fn phase_diff_mutation_matrix_covers_every_required_field() {
    let identity_fields = [
        "trigger_tx_hash", "feed_sequence", "inner_ordinal", "route_static_hash",
        "strategy_kind", "ordered_pool_kind_signature", "direction", "amount_bucket",
    ];
    let baseline = vec![valid_golden_record()];
    for field in REQUIRED_GOLDEN_DIMENSIONS {
        let mut row = valid_golden_record();
        let obj = row.as_object_mut().unwrap();
        match field {
            "trigger_tx_hash" => {
                obj[field] = json!(format!("0x{}", "22".repeat(32)));
            }
            "feed_sequence" | "amount_bucket" | "inner_ordinal" => {
                let v = obj[field].as_u64().unwrap();
                obj[field] = json!(v + 1);
            }
            "route_static_hash" => obj[field] = json!("0xfeedfeedfeedfeed"),
            "strategy_kind" => obj[field] = json!("Triangle"),
            "ordered_pool_kind_signature" => obj[field] = json!("UniswapV3>UniswapV3"),
            "direction" => obj[field] = json!("10"),
            "decision" => {
                obj["decision"] = json!(DECISION_TYPED_DECLINE);
                obj["typed_decline_reason"] = json!("net_gate_fail");
                obj["cycle_amount_out"] = json!(null);
            }
            "typed_decline_reason" => {
                // covered via decision flip above; standalone reason flip is invalid schema for
                // a candidate row — mutate through a decline pair instead.
                continue;
            }
            "amount_in" => obj[field] = json!("2000000"),
            "cycle_amount_out" => obj[field] = json!("1090000"),
            _ => obj[field] = json!("mutated"),
        }
        let diff = campaign_oracle::phase_diff(&baseline, &[row])
            .unwrap_or_else(|e| panic!("mutating `{field}` must diff, got {e}"));
        if identity_fields.contains(&field) {
            assert_eq!(
                diff.changed.len(),
                2,
                "identity-field `{field}` drift must surface as removed+added"
            );
        } else {
            assert_eq!(
                diff.changed.len(),
                1,
                "outcome-field `{field}` drift must surface as exactly one changed row"
            );
        }
    }
    // A FUTURE (non-required) field must also be caught by the full-record comparator.
    let mut future = valid_golden_record();
    future.as_object_mut().unwrap().insert("future_field".into(), json!("v2"));
    let diff = campaign_oracle::phase_diff(&baseline, &[future]).expect("future-field diffs");
    assert_eq!(diff.changed.len(), 1, "a future field's drift must be a changed row");
}

/// The WI-08 canary scan. WI-01c Gap 4: must be FULLY recursive — a module nested anywhere
/// below src/ must not evade it.
fn source_tree_contains_route_authority_cell(root: &Path) -> bool {
    source_tree_contains(root, concat!("struct ", "RouteAuthorityCell"))
}

/// Generic live-source canary scan (WI-01d): fully recursive over every directory (skipping
/// `target/`), matching only Rust files inside a `/src/` tree so test files quoting a type
/// name never trip it.
fn source_tree_contains(root: &Path, needle: &str) -> bool {
    // FULLY recursive (WI-01c Gap 4): every directory below the root is scanned — a module
    // nested anywhere under src/ cannot evade the canary. `target/` is skipped (build output).
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                if path.file_name().and_then(|n| n.to_str()) != Some("target") {
                    stack.push(path);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                // Only LIVE source trees (…/src/…) count — test files quoting the type name
                // (like this one's planted fixture) must not trip the canary.
                && path.to_string_lossy().contains("/src/")
                && std::fs::read_to_string(&path).unwrap_or_default().contains(needle)
            {
                return true;
            }
        }
    }
    false
}

/// Gap 4: the canary must catch a type planted in a NESTED module directory below src/.
#[test]
fn route_authority_cell_canary_is_recursive() {
    let scratch = std::env::temp_dir().join(format!("wi01c_canary_{}", std::process::id()));
    let nested = scratch.join("somecrate/src/authority/inner");
    std::fs::create_dir_all(&nested).expect("scratch tree");
    std::fs::write(
        nested.join("cell.rs"),
        format!("pub {} {{ _x: u64 }}\n", concat!("struct ", "RouteAuthorityCell")),
    )
    .expect("plant nested type");
    let found = source_tree_contains_route_authority_cell(&scratch);
    std::fs::remove_dir_all(&scratch).ok();
    assert!(
        found,
        "a RouteAuthorityCell nested below src/ evaded the canary — the WI-08 tripwire is \
         not recursive"
    );
}

// ═══════════════ WI-01d — FINAL oracle round (codex WI01C_VERIFY: 3 false-greens) ═══════════════

/// (1) The correction proof must bind the COMPLETE record multiset — the exact object
/// phase_diff compares — so a legitimate output correction can never piggyback an accidental
/// regression in any other (or future) field.
#[test]
fn correction_proof_binds_the_complete_record_multiset() {
    let old_row = valid_golden_record();
    let mut corrected = valid_golden_record();
    corrected.as_object_mut().unwrap()["cycle_amount_out"] = json!("1030000");
    // Entry + proof authorize ONLY the output correction (complete-record payloads of
    // old_row -> corrected).
    let entry = proven_entry_for("k1", "complete_bind", &[old_row.clone()], &[corrected.clone()]);
    for (field, bad) in [
        ("submit_eligibility", json!("fail_closed_no_exchange_id")),
        ("risk_class", json!("CentralisedStable")),
        ("price_anchor_class", json!("lst_peg")),
        ("evidence_class", json!("laundered")),
        ("future_field", json!("v2")),
    ] {
        let mut piggybacked = corrected.clone();
        piggybacked.as_object_mut().unwrap().insert(field.into(), bad.clone());
        let diff = PhaseDiff {
            unchanged: 0,
            changed: vec![ChangedRow {
                key: "k1".into(),
                change: "changed".into(),
                old_records: vec![old_row.clone()],
                new_records: vec![piggybacked],
            }],
        };
        let verdict = campaign_oracle::validate_correction_manifest(
            &repo_root(),
            &[entry.clone()],
            &diff,
        );
        assert!(
            matches!(
                verdict,
                Err(CorrectionManifestError::ProofIdentityMismatch { .. })
            ),
            "an approved output correction carrying a PIGGYBACKED `{field}` regression must \
             be rejected, got {verdict:?} — the proof binds a lossy projection, not the \
             complete record"
        );
    }
}

/// (2) Identity links must CONSERVE record multiplicity: two records at one old key may never
/// collapse into one record at the linked new key (a disappearance inside one link).
#[test]
fn identity_links_conserve_record_multiplicity() {
    let mut old_a = valid_golden_record();
    old_a.as_object_mut().unwrap()["amount_in"] = json!("1000000");
    let mut old_b = valid_golden_record();
    old_b.as_object_mut().unwrap()["amount_in"] = json!("3000000");
    old_b.as_object_mut().unwrap()["cycle_amount_out"] = json!("3060000");
    let mut new_one = valid_golden_record();
    new_one.as_object_mut().unwrap()["inner_ordinal"] = json!(1u64); // FL-03 identity change
    let diff = campaign_oracle::phase_diff(
        &[old_a.clone(), old_b.clone()],
        &[new_one.clone()],
    )
    .expect("multiplicity-collapsing identity change diffs");
    let old_key = diff.changed.iter().find(|r| r.change == "removed").unwrap().key.clone();
    let new_key = diff.changed.iter().find(|r| r.change == "added").unwrap().key.clone();
    // The entry/proof are ACCURATE (two complete old records -> one complete new record).
    let mut entry = proven_entry_for(
        &old_key,
        "multiplicity",
        &[old_a.clone(), old_b.clone()],
        &[new_one.clone()],
    );
    entry["issue_id"] = json!("FL-03");
    entry["identity_correction"] = json!({"old_key": old_key, "new_key": new_key});
    let verdict = campaign_oracle::validate_correction_manifest(&repo_root(), &[entry], &diff);
    assert!(
        verdict.is_err(),
        "TWO records at one old key collapsing into ONE record at the linked new key is a \
         disappearance and must be rejected, got {verdict:?}"
    );
}

/// (3) The fail-dark partition must be TERMINAL-IDENTITY bound: a route dying after express
/// preparation but before sign+envelope, or a route in the REVM ring without a sampler
/// decision, must FAIL the partition.
#[test]
fn fail_dark_partition_is_terminal_identity_bound() {
    let corpus: std::collections::BTreeSet<u32> = [0u32, 1, 2].into();
    // A clean run partitions exactly.
    let terminal: std::collections::BTreeSet<u32> = [0u32, 1].into();
    let sampled: std::collections::BTreeSet<u32> = [2u32].into();
    campaign_oracle::assert_corpus_partition(&corpus, &terminal, &sampled, &sampled)
        .expect("a clean terminal+sampled partition must validate");
    // Post-preparation terminal loss: route 2 prepared (would bump EXPRESS_SUBMITS) but never
    // crossed sign+envelope, and was NOT sampler-diverted.
    let empty: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    let verdict = campaign_oracle::assert_corpus_partition(&corpus, &terminal, &empty, &empty);
    assert!(
        matches!(verdict, Err(campaign_oracle::TerminalBoundaryFailure::FailDark { .. })),
        "a route lost AFTER preparation (terminal={{0,1}}, no sampler diversion) must FAIL \
         the partition, got {verdict:?} — the pre-terminal counter hid it"
    );
    // Misroute: route 2 sits in the REVM ring with NO sampler decision.
    let drained: std::collections::BTreeSet<u32> = [2u32].into();
    let verdict = campaign_oracle::assert_corpus_partition(&corpus, &terminal, &empty, &drained);
    assert!(
        verdict.is_err(),
        "a REVM-ring occupant WITHOUT a sampler decision is a misroute, not a permitted \
         diversion — got {verdict:?}"
    );
    // A sampler decision that never reached the ring (lost in the divert path) also fails.
    let verdict = campaign_oracle::assert_corpus_partition(&corpus, &terminal, &sampled, &empty);
    assert!(
        verdict.is_err(),
        "a sampler-diverted route missing from the REVM ring must fail, got {verdict:?}"
    );
}

/// The 7 families whose golden-baseline seeding requires REAL frozen-state snapshots — the
/// strategy-genesis boundary's SEEDING milestone. While the genesis boundary is re-gated
/// (pending), none of these may gain positive golden rows or leave `blocked_families`.
const GENESIS_SEED_GATED_FAMILIES: [&str; 7] = [
    "CurveStableSwapNg",
    "CurveCryptoSwap",
    "TraderJoeLb",
    "DodoV2",
    "BalancerStable",
    "BalancerWeighted",
    "MaverickV2",
];

/// WI-02 re-gate guardrail: the reassigned tripwire stays ARMED at the SEEDING milestone.
/// A still-pending genesis boundary permits the baseline ONLY while every seed-gated family
/// is still recorded blocked AND has zero positive golden rows; the moment the seeding WI
/// lands real family rows (or drops a blocked entry) without measuring genesis, this
/// rejects and the suite fails — the tripwire's target moved forward, it was not removed.
fn genesis_regate_permits(
    still_blocked: &[&str],
    positive_families: &[&str],
) -> Result<(), String> {
    for fam in GENESIS_SEED_GATED_FAMILIES {
        let blocked = still_blocked.contains(&fam);
        let seeded = positive_families.contains(&fam);
        if !blocked || seeded {
            return Err(format!(
                "seed-gated family `{fam}` reached the seeding milestone \
                 (blocked_entry_present={blocked}, positive_golden_rows={seeded}) while \
                 strategy_genesis_all_families is still pending"
            ));
        }
    }
    Ok(())
}

/// (4) The strategy-genesis deferral must be a MACHINE-FIRING tripwire, not a data label.
#[test]
fn strategy_genesis_tripwire_is_machine_wired() {
    // The boundary is REQUIRED — its declaration can never silently vanish from the artifact.
    assert!(
        REQUIRED_AUTHORITY_BOUNDARIES.contains(&"strategy_genesis_all_families"),
        "strategy_genesis_all_families must be a REQUIRED authority boundary"
    );
    // The parser must MACHINE-READ binding_tripwire: a pending boundary without an armed
    // tripwire is malformed (labels alone arm nothing).
    let unarmed = json!({
        "schema": "wi01_probe_coverage.v1",
        "boundaries": {
            "strategy_genesis_all_families": {
                "status": "pending_work_item",
                "work_item": "WI-02",
                "reason": "unarmed"
            }
        }
    });
    let verdict = campaign_oracle::alloc_syscall_probe_coverage(&unarmed);
    assert!(
        matches!(
            verdict,
            Err(campaign_oracle::ProbeCoverageError::Malformed { .. })
        ),
        "a pending boundary WITHOUT binding_tripwire=true must be rejected by the parser, \
         got {verdict:?}"
    );
    // The live WI-02 canary fires: a `struct RuntimePolicy` planted in a nested src tree is
    // found by the same recursive scanner class as the RouteAuthorityCell canary.
    let scratch = std::env::temp_dir().join(format!("wi01d_genesis_{}", std::process::id()));
    let nested = scratch.join("somecrate/src/policy/inner");
    std::fs::create_dir_all(&nested).expect("scratch tree");
    std::fs::write(
        nested.join("policy.rs"),
        format!("pub {} {{ _x: u64 }}\n", concat!("struct ", "RuntimePolicy")),
    )
    .expect("plant nested type");
    let found = source_tree_contains(&scratch, concat!("struct ", "RuntimePolicy"));
    std::fs::remove_dir_all(&scratch).ok();
    assert!(found, "the WI-02 RuntimePolicy canary must find a nested planted type");
    // And on the REAL tree: once WI-02's RuntimePolicy exists, the genesis boundary may no
    // longer sit silently pending-on-WI-02. WI-02 landed GATING-ONLY (it seals the policy; it
    // does NOT seed the 7 blocked families — their kernels need REAL frozen-state snapshots,
    // which are money-firewalled until the Phase-6 logged Lava cache-seed). So the landing
    // forces an EXPLICIT re-adjudication: the boundary is either measured, or re-gated onto
    // the SEEDING milestone with the binding tripwire STILL armed and the re-adjudication
    // recorded. WI-02b (codex blocker 3): the re-gate `work_item` names
    // `family-genesis-seeding` — the SAME milestone the machine trigger below detects
    // (a seed-gated family leaving blocked_families or gaining a positive golden row), so
    // target and trigger cannot diverge: the seeding work cannot land anywhere without
    // firing this test while genesis is still pending. A pending-on-WI-02 artifact (stale),
    // an unarmed re-gate, or a re-gate naming any other milestone fails the suite.
    if source_tree_contains(
        &repo_root().join("engine/crates"),
        concat!("struct ", "RuntimePolicy"),
    ) {
        let artifact = read_baseline_json("probe_coverage.json");
        let b = &artifact["boundaries"]["strategy_genesis_all_families"];
        let measured = b["status"].as_str() == Some("measured");
        let regated = b["status"].as_str() == Some("pending_work_item")
            && b["binding_tripwire"].as_bool() == Some(true)
            && b["work_item"].as_str() == Some("family-genesis-seeding")
            && b["re_adjudicated_by"].as_str() == Some("WI-02")
            && b["reason"].as_str().is_some_and(|r| !r.is_empty());
        assert!(
            measured || regated,
            "WI-02's RuntimePolicy landed: the strategy-genesis boundary must be measured OR \
             explicitly re-gated onto the machine-detected seeding milestone (pending, \
             tripwire armed, work_item=family-genesis-seeding, re_adjudicated_by=WI-02) — \
             got {b}"
        );
        // The re-gate guard itself must FIRE at the seeding milestone (class negatives): a
        // blocked family gaining a positive row, or leaving blocked_families, while genesis
        // is still pending must reject.
        assert!(
            genesis_regate_permits(&GENESIS_SEED_GATED_FAMILIES, &[]).is_ok(),
            "all-blocked, none-seeded is the legitimate re-gated state"
        );
        assert!(
            genesis_regate_permits(&GENESIS_SEED_GATED_FAMILIES, &["CurveStableSwapNg"]).is_err(),
            "a seed-gated family with POSITIVE golden rows while genesis is pending must \
             reject — the reassigned tripwire fired dark"
        );
        let missing_blocked: Vec<&str> = GENESIS_SEED_GATED_FAMILIES
            .iter()
            .copied()
            .filter(|f| *f != "DodoV2")
            .collect();
        assert!(
            genesis_regate_permits(&missing_blocked, &[]).is_err(),
            "a seed-gated family LEAVING blocked_families while genesis is pending must \
             reject — the reassigned tripwire fired dark"
        );
        // And on the REAL artifacts: while re-gated (not measured), the seeding milestone
        // must not have landed.
        if !measured {
            let manifest = read_baseline_json("baseline_manifest.json");
            let still_blocked: Vec<&str> = manifest["blocked_families"]
                .as_object()
                .expect("blocked_families object")
                .keys()
                .map(String::as_str)
                .collect();
            let golden = std::fs::read_to_string(
                repo_root().join("codex/campaign_baseline_wi01/golden_candidate_records.jsonl"),
            )
            .expect("golden rows");
            let mut positive_families: Vec<String> = Vec::new();
            for line in golden.lines().filter(|l| !l.trim().is_empty()) {
                let row: serde_json::Value = serde_json::from_str(line).expect("golden row");
                if row["decision"].as_str() == Some("candidate") {
                    let sig = row["ordered_pool_kind_signature"].as_str().unwrap_or("");
                    for fam in sig.split('>') {
                        positive_families.push(fam.to_string());
                    }
                }
            }
            let positive_refs: Vec<&str> =
                positive_families.iter().map(String::as_str).collect();
            genesis_regate_permits(&still_blocked, &positive_refs).unwrap_or_else(|err| {
                panic!(
                    "family-genesis SEEDING landed while strategy_genesis_all_families is \
                     still pending — measure the genesis boundary (or explicitly \
                     re-adjudicate) before seeding the blocked families: {err}"
                )
            });
        }
    }
}

/// WI-02c 4B deferral (codex WI02B_VERIFY): worker-enforcement of the sealed
/// `RuntimePolicy.proven_families` (the OBJECT handoff into `Pipeline` — no second
/// re-resolution in `start_threads` — plus equality/refuse-mismatch on the worker) is
/// WI-16's authority domain. SAFE to defer: no live capital runs before WI-16 (dry-run,
/// counting-fake transport, no seeded state). The deferral is tripwire-protected: while
/// `start_threads` still re-resolves via `FastlaneRuntimePolicy::from_env()`, the boundary
/// must sit as the exact ARMED pending tuple; the moment WI-16 lands its rewire (the
/// second-resolution marker disappears from pipeline.rs), a still-pending boundary FAILS
/// the suite — WI-16 cannot land the authority work without wiring + measuring it.
fn sealed_authority_deferral_permits(
    second_resolution_present: bool,
    status_measured: bool,
    armed_pending_tuple: bool,
) -> Result<(), String> {
    if status_measured {
        return Ok(());
    }
    if !second_resolution_present {
        return Err(
            "WI-16's sealed-policy worker-authority rewire LANDED (the start_threads \
             second-resolution marker is gone) while the boundary is still pending — \
             measure it (object handoff + worker equality) or the deferral is being \
             silently forgotten"
                .to_string(),
        );
    }
    if !armed_pending_tuple {
        return Err(
            "the WI-16 deferral must be recorded as the exact ARMED pending tuple \
             (binding_tripwire=true, work_item=WI-16-sealed-policy-worker-authority, \
             deferred_by=WI-02c, nonempty reason)"
                .to_string(),
        );
    }
    Ok(())
}

/// WI-02c: the deferred sealed-authority boundary is declared, armed, and machine-fires at
/// WI-16's landing.
#[test]
fn wi16_sealed_authority_tripwire_is_armed() {
    // The boundary is REQUIRED — its declaration can never silently vanish.
    assert!(
        REQUIRED_AUTHORITY_BOUNDARIES.contains(&"sealed_policy_worker_authority"),
        "sealed_policy_worker_authority must be a REQUIRED authority boundary (WI-02c \
         defers worker enforcement of the sealed proven set to WI-16 — the deferral must \
         be machine-tracked, not a prose note)"
    );
    // Class negatives: the deferral cannot be forgotten or silently recorded.
    assert!(sealed_authority_deferral_permits(true, false, true).is_ok());
    assert!(sealed_authority_deferral_permits(true, true, false).is_ok());
    assert!(sealed_authority_deferral_permits(false, true, false).is_ok());
    assert!(
        sealed_authority_deferral_permits(false, false, true).is_err(),
        "WI-16 landing its rewire while the boundary stays pending must FIRE"
    );
    assert!(
        sealed_authority_deferral_permits(true, false, false).is_err(),
        "an unrecorded/unarmed deferral must FIRE"
    );
    // The REAL artifacts: the boundary exists with the exact armed tuple, and the live
    // pipeline state is adjudicated by the same predicate.
    let artifact = read_baseline_json("probe_coverage.json");
    let b = &artifact["boundaries"]["sealed_policy_worker_authority"];
    let armed = b["status"].as_str() == Some("pending_work_item")
        && b["binding_tripwire"].as_bool() == Some(true)
        && b["work_item"].as_str() == Some("WI-16-sealed-policy-worker-authority")
        && b["deferred_by"].as_str() == Some("WI-02c")
        && b["reason"].as_str().is_some_and(|r| !r.is_empty());
    let measured = b["status"].as_str() == Some("measured");
    let pipeline_src = std::fs::read_to_string(
        repo_root().join("engine/crates/engine/src/pipeline.rs"),
    )
    .expect("read pipeline.rs");
    let second_resolution_present = pipeline_src.contains("FastlaneRuntimePolicy::from_env()");
    sealed_authority_deferral_permits(second_resolution_present, measured, armed)
        .unwrap_or_else(|err| {
            panic!("sealed-policy worker-authority deferral violated: {err} — got {b}")
        });
    assert!(
        measured || armed,
        "the boundary must be measured or the exact armed WI-16 deferral tuple, got {b}"
    );
}

/// (5) Provenance preservation: the flake proof log is itself hash-pinned and present, and
/// the expected-red manifest stays in the REQUIRED artifact set.
#[test]
fn provenance_preserves_expected_red_and_flake_log_bindings() {
    let manifest = read_baseline_json("baseline_manifest.json");
    let hashes = manifest["artifact_sha256"].as_object().expect("artifact_sha256");
    for required in ["expected_red_manifest.json", "logs/isolated_qualification.txt"] {
        let pinned = hashes
            .get(required)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("artifact_sha256 must pin `{required}`"));
        assert_eq!(
            pinned,
            sha256_of(&baseline_dir().join(required)),
            "`{required}` hash drift"
        );
    }
    // The flake proof references parse to an existing, hash-pinned committed file.
    let red = read_baseline_json("expected_red_manifest.json");
    for flake in red["serial_flake_allowlist"].as_array().expect("allowlist") {
        let proof = flake["isolated_proof"].as_str().expect("isolated_proof");
        let path_token = proof.split(" (").next().expect("path token");
        assert!(
            path_token.starts_with("codex/campaign_baseline_wi01/logs/"),
            "flake proof must lead with the committed log path, got `{path_token}`"
        );
        assert!(
            repo_root().join(path_token).exists(),
            "flake proof log `{path_token}` must exist"
        );
    }
}

/// WI-06 T6 v2: one consolidated, comment-immune live-source gate.
#[test]
fn wi06_frontier_integrity_gate_is_wired() {
    const CASE_TESTS: [&str; 10] = [
        "service_order_fuzz_preserves_exact_applied_frontier_at_every_validation_stop",
        "duplicates_apply_at_most_once_and_dedup_overflow_poisons_the_pin",
        "multi_loss_records_earliest_hole_and_whole_pin_poison_declines_every_feed_candidate",
        "pending_overflow_poisons_the_whole_pin",
        "replay_failure_poisons_the_whole_pin",
        "budget_stop_defers_without_authorizing_then_is_re_served_without_poison",
        "ordinal_gap_disarms_without_restoration",
        "every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare",
        "replay_fault_after_authorization_before_socket_write_refuses_the_wire_in_both_lanes",
        "packed_generation_change_refuses_old_candidates_in_both_lanes",
    ];
    const CASE_ROWS: [&str; 15] = [
        "case=1 | scope=cursor-exact-safety | status=CLOSED-BY-GATE | owner=WI-06-D2-follow-up | evidence=service_order_fuzz_preserves_exact_applied_frontier_at_every_validation_stop",
        "case=2 | scope=dedup-safety | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=duplicates_apply_at_most_once_and_dedup_overflow_poisons_the_pin",
        "case=3 | scope=enqueue-loss-safety | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=multi_loss_records_earliest_hole_and_whole_pin_poison_declines_every_feed_candidate",
        "case=4 | scope=pending-overflow-safety | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=pending_overflow_poisons_the_whole_pin",
        "case=5 | scope=replay-failure-safety | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=replay_failure_poisons_the_whole_pin",
        "case=6 | scope=budget-stop-safety | status=CLOSED-BY-GATE | owner=WI-06-D2-follow-up | evidence=budget_stop_defers_without_authorizing_then_is_re_served_without_poison",
        "case=7 | scope=production-restoration-floor-sliver | status=DEFERRED | owner=T7b | evidence=NONE",
        "case=8 | scope=typed-origins-origin-neutrality | status=DEFERRED | owner=T7b | evidence=NONE",
        "case=9 | scope=forward-gap-safety | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=ordinal_gap_disarms_without_restoration",
        "case=9-restoration | scope=d1-authority-rearm | status=DEFERRED | owner=T7b | evidence=NONE",
        "case=10 | scope=built-fault-rows-F01-F07 | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare",
        "case=11 | scope=trusted-mirror-canonical-adjudication | status=DEFERRED | owner=WI-09/WI-11 | evidence=NONE",
        "case=12 | scope=post-accept-fault-to-wire | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=replay_fault_after_authorization_before_socket_write_refuses_the_wire_in_both_lanes",
        "case=13 | scope=packed-generation-safety | status=CLOSED-BY-GATE | owner=WI-06-T6 | evidence=packed_generation_change_refuses_old_candidates_in_both_lanes",
        "case=13-restoration | scope=production-poison-repin-barrier-clear | status=DEFERRED | owner=T7b | evidence=NONE",
    ];
    const FAULT_ROWS: [&str; 9] = [
        "D2-F01-replay-queue-enqueue-loss | status=CLOSED-BY-GATE | evidence=multi_loss_records_earliest_hole_and_whole_pin_poison_declines_every_feed_candidate,every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F02-pending-buffer-overflow | status=CLOSED-BY-GATE | evidence=pending_overflow_poisons_the_whole_pin,every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F03-replay-execution-failure | status=CLOSED-BY-GATE | evidence=replay_failure_poisons_the_whole_pin,every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F04-dedup-window-overflow | status=CLOSED-BY-GATE | evidence=duplicates_apply_at_most_once_and_dedup_overflow_poisons_the_pin,every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F05-budget-stop-below-trigger | status=CLOSED-BY-GATE | evidence=budget_stop_defers_without_authorizing_then_is_re_served_without_poison,every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F06-stream-contiguity-disorder | status=CLOSED-BY-GATE | evidence=ordinal_gap_disarms_without_restoration,every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F07-equal-cursor-different-identity | status=CLOSED-BY-GATE | evidence=every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare | repair_owner=WI-09",
        "D2-F08-repin-head-lag-sliver | status=DEFERRED | evidence=NONE | owner=T7b",
        "D2-F09-trusted-mirror-bypass | status=DEFERRED | evidence=NONE | owner=WI-09/WI-11",
    ];

    let root = repo_root();
    let pipeline_path = root.join("engine/crates/engine/src/pipeline.rs");
    let pipeline_raw = std::fs::read_to_string(&pipeline_path).expect("read pipeline.rs");
    let pipeline = mask_rust_source(&pipeline_raw);
    let production = strip_cfg_test_items(&pipeline);
    let state_raw = std::fs::read_to_string(
        root.join("engine/crates/engine-state/src/publication.rs"),
    )
    .expect("read publication.rs");
    let state = mask_rust_source(&state_raw);
    let feed = mask_rust_source(
        &std::fs::read_to_string(root.join("engine/crates/engine-feed/src/feed_frame.rs"))
            .expect("read feed_frame.rs"),
    );
    let ledger = std::fs::read_to_string(
        root.join("codex/PHASE6_DEFERRAL_LEDGER_2026-07-17.md"),
    )
    .expect("read ledger");
    let oracle = mask_rust_source(
        &std::fs::read_to_string(
            root.join("engine/crates/engine/tests/campaign_baseline_oracle_static.rs"),
        )
        .expect("read oracle source"),
    );

    const T8_SNIFF_ROW: &str = "D2-T8-SNIFF-ORDINAL | scope=feed-sniff-v4-ordinal-pass-through | status=CLOSED-BY-GATE | owner=WI-06-T8-S | evidence=sniff_fallback_preserves_parsed_inner_ordinal_at_dirty_terminal";
    let sniff_test_name = "sniff_fallback_preserves_parsed_inner_ordinal_at_dirty_terminal";
    assert_eq!(
        pipeline.matches(&format!("fn {sniff_test_name}(")).count(),
        1,
        "[G-T8-SNIFF-LEDGER] T8-S test must be defined exactly once",
    );
    let sniff_test_body_raw = named_body(&pipeline_raw, &format!("fn {sniff_test_name}"))
        .expect("[G-T8-SNIFF-REAL-TEST-SEAM] T8-S test body");
    let sniff_test_body = mask_rust_source(&sniff_test_body_raw);
    assert!(
        sniff_test_body.contains("push_parsed_state_bounded")
            && sniff_test_body.contains("state_loop(")
            && sniff_test_body.contains("dirty_queue")
            && sniff_test_body.contains(".pop()")
            && sniff_test_body_raw.contains("[T8-SNIFF-ORDINAL-STATE-TO-DIRTY]"),
        "[G-T8-SNIFF-REAL-TEST-SEAM] test must drive state_loop through the real dirty terminal",
    );
    assert_eq!(
        production.matches("let _ = emit_sniffed_dirty_event(").count(),
        1,
        "[G-T8-SNIFF-SOLE-LIVE-CALLER] exactly one production FeedSniff caller",
    );
    let state_loop = mask_rust_source(
        &named_body(&pipeline_raw, "fn state_loop")
            .expect("[G-T8-SNIFF-SOLE-LIVE-CALLER] state_loop body"),
    );
    let sniff_call = state_loop
        .find("let _ = emit_sniffed_dirty_event(")
        .expect("[G-T8-SNIFF-SOLE-LIVE-CALLER] state_loop FeedSniff caller");
    let sniff_call = &state_loop[sniff_call..];
    let seq_pos = sniff_call
        .find("parsed.seq,")
        .expect("[G-T8-SNIFF-ORDINAL-LIVE-CALLER] parsed seq argument");
    let ordinal_pos = sniff_call
        .find("parsed.inner_ordinal,")
        .expect("[G-T8-SNIFF-ORDINAL-LIVE-CALLER] parsed ordinal argument");
    let hash_pos = sniff_call
        .find("parsed.tx_hash.unwrap_or([0u8; 32]),")
        .expect("[G-T8-SNIFF-ORDINAL-LIVE-CALLER] parsed hash argument");
    assert!(
        seq_pos < ordinal_pos && ordinal_pos < hash_pos,
        "[G-T8-SNIFF-ORDINAL-LIVE-CALLER] caller order must be seq, parsed ordinal, tx hash",
    );
    let sniff_helper = mask_rust_source(
        &named_body(&pipeline_raw, "fn emit_sniffed_dirty_event")
            .expect("[G-T8-SNIFF-HELPER-PASSTHROUGH] helper body"),
    );
    assert!(
        sniff_helper.contains(
            "seq: u64,\n    inner_ordinal: u32,\n    tx_hash: [u8; 32],"
        ),
        "[G-T8-SNIFF-HELPER-PASSTHROUGH] helper accepts the ordinal between seq and hash",
    );
    let event_start = sniff_helper
        .find("let event = DirtyEvent")
        .expect("[G-T8-SNIFF-HELPER-PASSTHROUGH] DirtyEvent initializer");
    let event_end = event_start
        + sniff_helper[event_start..]
            .find("    };\n")
            .expect("[G-T8-SNIFF-HELPER-PASSTHROUGH] DirtyEvent initializer end")
        + 6;
    let event_init = &sniff_helper[event_start..event_end];
    assert!(
        event_init.contains("        inner_ordinal,")
            && !event_init.contains("inner_ordinal: 0"),
        "[G-T8-SNIFF-HELPER-PASSTHROUGH] helper initializer uses the parameter",
    );
    let drain_helper = mask_rust_source(
        &named_body(&pipeline_raw, "fn build_drain_dirty_event")
            .expect("[G-T8-NONFEED-ZERO-CONTROL] drain helper body"),
    );
    assert!(
        drain_helper.contains("inner_ordinal: 0,")
            && drain_helper.contains("tx_hash: [0u8; 32],"),
        "[G-T8-NONFEED-ZERO-CONTROL] synthetic drain wake retains zero ordinal/hash",
    );
    assert_eq!(
        ledger.matches(T8_SNIFF_ROW).count(),
        1,
        "[G-T8-SNIFF-LEDGER] exactly one separate T8-S closure row",
    );

    let module_marker = "#[cfg(test)]\nmod wi06_frontier_integrity";
    assert_eq!(
        pipeline.matches(module_marker).count(),
        1,
        "[G-HARNESS-EXACT-TEST-SET] one scoped module",
    );
    let module = named_body(&pipeline_raw, "mod wi06_frontier_integrity")
        .expect("[G-HARNESS-EXACT-TEST-SET] module body");
    let module_masked = mask_rust_source(&module);
    assert!(
        !module_masked.contains(&format!("fn {sniff_test_name}(")),
        "[G-T8-SNIFF-LEDGER] T8-S test must remain outside the exact-ten T6 module",
    );
    assert_eq!(
        module_masked.matches("#[test]").count(),
        CASE_TESTS.len(),
        "[G-HARNESS-EXACT-TEST-SET] exactly ten executable tests",
    );
    for name in CASE_TESTS {
        assert_eq!(
            module_masked.matches(&format!("fn {name}(")).count(),
            1,
            "[G-HARNESS-EXACT-TEST-SET] case must appear once",
        );
        assert_eq!(
            pipeline.matches(&format!("fn {name}(")).count(),
            1,
            "[G-HARNESS-NO-DUPLICATE-WRAPPERS] case must have one global definition",
        );
    }
    assert!(
        !module_masked.contains("fn trusted_mirror")
            && !module_masked.contains("fn canonical_mirror")
            && !module_masked.contains("fn typed_origin")
            && !module_masked.contains("fn repin_clear"),
        "[G-HARNESS-EXACT-TEST-SET] deferred cases are non-executable",
    );
    let case10 = named_body(
        &module,
        "fn every_built_replay_fault_refuses_sim_to_express_and_express_pop_prepare",
    )
    .expect("[G-HARNESS-CASE8-NO-TEST-ORIGIN] case 10 wrapper");
    let case10_masked = mask_rust_source(&case10);
    assert!(
        !case10_masked.contains("DirtyOrigin")
            && !case10_masked.contains("origin:")
            && !case10_masked.contains("test_only"),
        "[G-HARNESS-CASE8-NO-TEST-ORIGIN] no test-only origin fabrication",
    );
    assert!(
        module_masked.contains("push_validated_candidate")
            && module_masked.contains("express_pop_prepare")
            && module_masked.contains("HotSpare<CountingConn>")
            && module_masked.contains("rx_loop_unix_binary"),
        "[G-HARNESS-REAL-SEAMS] shared harness reaches real seams",
    );
    let case1_body = named_body(&pipeline_raw, "fn wi06_case1_body")
        .expect("[G-HARNESS-REAL-SEAMS] case 1 body missing");
    let case1 = mask_rust_source(&case1_body);
    assert!(
        case1.contains("ForkExecutor") && case1.contains("drain_revm_state_replay_queue"),
        "[G-HARNESS-REAL-SEAMS] cases 1-6 must use the real ForkExecutor/drain terminal",
    );
    assert!(
        module_masked.contains("OwnedStream")
            && module_masked.contains("SlabStream")
            && module_masked.contains("OwnedSeqpacket")
            && module_masked.contains("SlabSeqpacket")
            && module_masked.contains("FEED_PAYLOAD_CONFLICT_SENTINEL")
            && module_masked.contains("near_miss"),
        "[G-F07-EXACT-SENTINEL-FOUR-RX] four RX forms and near miss",
    );
    assert!(
        oracle.matches("fn wi06_taint_consumer_tripwire_is_armed").count() == 0
            && oracle.matches("fn wi06_budget_cursor_gates_are_wired").count() == 0
            && oracle.matches("fn wi06_frontier_integrity_gate_is_wired").count() == 1,
        "[G-NARROW-GUARDS-SUBSUMED] one consolidated executable gate",
    );

    assert!(ledger.contains("wi06_frontier_integrity_gate_is_wired"),
        "[G-LEDGER-BIDIRECTIONAL] ledger must name gate");
    assert!(ledger.contains("status=CLOSED-BY-GATE | owner=WI-06-D2-follow-up | t6=case1"),
        "[G-LEDGER-CASE1] case 1 closed");
    assert!(ledger.contains("status=CLOSED-BY-GATE | owner=WI-06-D2-follow-up | t6=case6"),
        "[G-LEDGER-CASE6] case 6 closed");
    assert!(ledger.contains("feed taint watermark consumer")
        && ledger.contains("same-sequence batch semantics"),
        "[G-TAINT-LEDGER-DEFERRED] taint contract remains explicit");
    for row in CASE_ROWS {
        assert_eq!(ledger.matches(row).count(), 1,
            "[G-LEDGER-EXACT-CASE-SET] exact case row: {row}");
    }
    for row in FAULT_ROWS {
        assert_eq!(ledger.matches(row).count(), 1,
            "[G-LEDGER-EXACT-FAULT-SET] exact fault row: {row}");
    }
    assert_eq!(ledger.matches("case=").count(), CASE_ROWS.len(),
        "[G-LEDGER-EXACT-CASE-SET] no extra case rows");
    assert_eq!(ledger.matches("D2-F").count(), FAULT_ROWS.len(),
        "[G-LEDGER-EXACT-FAULT-SET] no extra fault rows");
    for deferred in [
        "case=7 | scope=production-restoration-floor-sliver | status=DEFERRED | owner=T7b | evidence=NONE",
        "case=8 | scope=typed-origins-origin-neutrality | status=DEFERRED | owner=T7b | evidence=NONE",
        "case=9-restoration | scope=d1-authority-rearm | status=DEFERRED | owner=T7b | evidence=NONE",
        "case=11 | scope=trusted-mirror-canonical-adjudication | status=DEFERRED | owner=WI-09/WI-11 | evidence=NONE",
        "case=13-restoration | scope=production-poison-repin-barrier-clear | status=DEFERRED | owner=T7b | evidence=NONE",
    ] {
        assert!(ledger.contains(deferred),
            "[G-DEFERRAL-OWNER] deferred row owner/evidence: {deferred}");
    }
    assert!(
        !ledger.contains("wi06_taint_consumer_tripwire_is_armed")
            && !ledger.contains("wi06_budget_cursor_gates_are_wired"),
        "[G-NARROW-GUARDS-SUBSUMED] stale narrow backlinks",
    );

    let nested_comments = "/* outer continue;\n /* nested continue; */\n*/\ncontinue;\n";
    assert_eq!(executable_statement_count(nested_comments, "continue;"), 1,
        "[G-EXTRACTOR-BLOCK-COMMENT] nested comments cannot spoof count");
    let labels = "'reconnect: loop { break 'reconnect; }";
    let labels_masked = mask_rust_source(labels);
    assert!(labels_masked.contains("'reconnect: loop")
        && labels_masked.contains("break 'reconnect"),
        "[G-EXTRACTOR-LABEL] loop labels remain visible");
    let spoof_fixture = r#"
        // reserve_replay_service_cursor(0);
        /* outer /* candidate_after_replay_drain(0); */ */
        let text = "push_validated_candidate(0);";
        #[cfg(test)]
        fn hidden_test() { reserve_replay_service_cursor(0); }
        fn live_test() { reserve_replay_service_cursor(0); }
    "#;
    let spoof_production = strip_cfg_test_items(&mask_rust_source(spoof_fixture));
    assert_eq!(positions(&spoof_production, "reserve_replay_service_cursor(").len(), 1,
        "[G-EXTRACTOR-COMMENT-STRING] only live call survives");
    assert!(!spoof_production.contains("hidden_test")
        && !spoof_production.contains("push_validated_candidate(0)"),
        "[G-EXTRACTOR-CFG-TEST] cfg(test)/strings are stripped");
    let good_order = mask_rust_source("fn live() { reserve_replay_service_cursor(); submit(); }");
    let bad_order = mask_rust_source("fn live() { submit(); reserve_replay_service_cursor(); }");
    assert!(good_order.find("reserve_replay_service_cursor()") < good_order.find("submit()"),
        "[G-EXTRACTOR-ORDER] good live order");
    assert!(bad_order.find("reserve_replay_service_cursor()") > bad_order.find("submit()"),
        "[G-EXTRACTOR-ORDER] moved-after-submit order is rejected");

    // Inherited #69/#70 live caller and disposition teeth.
    assert!(
        ledger.contains("service_order_fuzz_preserves_exact_applied_frontier_at_every_validation_stop")
            && ledger.contains("budget_stop_defers_without_authorizing_then_is_re_served_without_poison"),
        "[G-BEHAVIORAL-NAMES] case 1 and case 6 names must stay ledger-backed",
    );
    assert!(
        pipeline.contains("fn service_order_fuzz_preserves_exact_applied_frontier_at_every_validation_stop"),
        "[G-C1-TEST] case 1 behavioral test is present",
    );
    assert!(
        pipeline.contains("fn budget_stop_defers_without_authorizing_then_is_re_served_without_poison"),
        "[G-C6-TEST] case 6 behavioral test is present",
    );
    assert!(
        pipeline.contains("fn pop_next(&mut self) -> Option<(u128, Candidate)>"),
        "[G-POP-SIGNATURE] EvBacklog::pop_next returns EV and Candidate without watermark",
    );
    assert!(
        pipeline.contains(".cmp(&candidate_replay_cursor(b))"),
        "[G-POP-CURSOR-PRIMARY] pop_next compares canonical replay cursor",
    );
    assert!(
        pipeline.contains(".then_with(|| ev_b.cmp(ev_a))"),
        "[G-POP-EV-TIEBREAK] EV is only the exact-cursor tie-break",
    );
    assert!(
        pipeline.contains("let mut replay_service_floor: Option<FeedReplayCursor> = None;"),
        "[G-FLOOR-DECL] replay service floor declaration is present",
    );
    assert!(
        pipeline.contains("u32::from(cd.inner_ordinal)"),
        "[G-ORDINAL-ADJUDICATION] ordinal narrowing is explicit",
    );
    assert!(
        !pipeline.contains("fn pop_next(&mut self, watermark: u64)")
            && !pipeline.contains("seq_watermark"),
        "[G-NO-SEQUENCE-POP] superseded sequence-only cursor machinery is absent",
    );

    let live_start = pipeline
        .find("\nfn revm_loop(")
        .expect("[G-LIVE-LOOP] production revm_loop missing");
    let live_end = live_start
        + pipeline[live_start..]
            .find("\n#[cfg(test)]\nmod curve_predict_tests")
            .expect("[G-LIVE-LOOP] production revm_loop end missing");
    let live = &pipeline[live_start..live_end];
    assert_eq!(
        live.matches("reserve_replay_service_cursor(").count(),
        1,
        "[G-LIVE-CURSOR-CALL] one production reservation call",
    );
    assert_eq!(
        live.matches("candidate_after_replay_drain(").count(),
        1,
        "[G-LIVE-BUDGET-CALL] one production budget disposition",
    );
    assert_eq!(
        live.matches("push_validated_candidate(").count(),
        1,
        "[G-LIVE-VC-CALL] one production validated-candidate push",
    );
    let reserve_if = live
        .find("if !reserve_replay_service_cursor(&mut replay_service_floor, cd_cursor) {")
        .expect("[G-LIVE-CURSOR-CALL] live cursor gate missing");
    let pin_capture = live
        .find("let validation_pin_generation =")
        .expect("[G-PIN-CAPTURE] validation pin capture missing");
    let candidate_drain = live
        .find("let drain = if trigger_drain_enabled() || !revm_state_trusts_mirror() {")
        .expect("[G-LIVE-DRAIN] replay drain missing");
    let budget_let = live
        .find("let Some(cd) = candidate_after_replay_drain(")
        .expect("[G-LIVE-BUDGET-CALL] live budget gate missing");
    let sweep = live
        .find("if sweep_cursor < prewarm_routes {")
        .expect("[G-SWEEP] sweep missing");
    let validation = live
        .find("let pass = if fastlane_eligible")
        .expect("[G-VALIDATION] validation boundary missing");
    let enqueue = live
        .find("let enqueue_failed = push_validated_candidate(")
        .expect("[G-ENQUEUE] enqueue missing");
    assert!(
        reserve_if < pin_capture
            && pin_capture < candidate_drain
            && candidate_drain < budget_let
            && budget_let < sweep
            && sweep < validation
            && validation < enqueue,
        "[G-LIVE-GATE-ORDER] cursor, pin, drain, budget, sweep, validation, enqueue order",
    );
    assert_eq!(
        executable_statement_count(&live[reserve_if..pin_capture], "continue;"),
        1,
        "[G-CURSOR-CONTINUE] cursor decline has one executable continue",
    );
    let budget_arm = &live[budget_let..sweep];
    assert!(
        budget_arm.contains(") else {") && budget_arm.contains("StageLatency::RevmFull"),
        "[G-BUDGET-ELSE] budget let-else decline arm remains",
    );
    assert_eq!(
        executable_statement_count(budget_arm, "continue;"),
        1,
        "[G-BUDGET-CONTINUE] budget defer has one executable continue",
    );
    let reset_positions = positions(live, "reset_replay_state_for_new_pin(");
    assert_eq!(
        reset_positions.len(),
        3,
        "[G-RESET-COUNT] exactly three production replay resets",
    );
    for (reset_pos, assertion_id) in reset_positions.iter().copied().zip([
        "[G-RESET-STAGED]",
        "[G-RESET-LIGHT]",
        "[G-RESET-IDLE]",
    ]) {
        let tail = &live[reset_pos..];
        let end = tail
            .find("last_repin = Instant::now();")
            .unwrap_or_else(|| panic!("{assertion_id} reset lacks last_repin"));
        assert_eq!(
            executable_statement_count(&tail[..end], "replay_service_floor = None;"),
            1,
            "{assertion_id} reset must clear the floor once before last_repin",
        );
    }
    assert_eq!(
        executable_statement_count(live, "replay_service_floor = None;"),
        3,
        "[G-FLOOR-CLEAR-COUNT] only three repins clear the floor",
    );
    let post_drain = live
        .find("if drain.budget_hit {")
        .expect("[G-BUDGET-REGION] budget region missing");
    let post_drain_region = &live[post_drain..sweep];
    for (needle, assertion_id) in [
        ("publish_fault(", "[G-BUDGET-NO-PUBLISH-FAULT]"),
        ("repin_clear(", "[G-BUDGET-NO-FAULT-CLEAR]"),
        ("reset_replay_state_for_new_pin(", "[G-BUDGET-NO-REPLAY-RESET]"),
        ("note_reject(", "[G-BUDGET-NO-COOLDOWN]"),
    ] {
        assert!(
            !post_drain_region.contains(needle),
            "{assertion_id} budget-stop caller must not contain {needle}",
        );
    }
    assert_eq!(
        executable_statement_count(post_drain_region, "replay_service_floor = None;"),
        0,
        "[G-BUDGET-NO-FLOOR-CLEAR] budget defer retains the floor",
    );
    assert_eq!(
        live.matches("repin_clear(").count(),
        0,
        "[G-NO-PRODUCTION-REPIN-CLEAR] no production repin_clear in T6",
    );

    // Taint readers remain test-only and exactly two plus two.
    assert_eq!(
        production.matches("feed_taint_watermark(").count(),
        0,
        "[G-TAINT-ZERO-PRODUCTION-READERS] zero production taint readers",
    );
    assert_eq!(
        pipeline.matches("feed_taint_watermark(").count(),
        4,
        "[G-TAINT-FOUR-TEST-READERS] exactly four test readers",
    );
    for function in [
        "fn binary_v4_taint_marker_advances_engine_watermark_without_state",
        "fn legacy_binary_frames_are_quarantined_from_authoritative_state",
    ] {
        let body = named_body(&pipeline_raw, function)
            .unwrap_or_else(|| panic!("[G-TAINT-TWO-PLUS-TWO-TEST-PLACEMENT] missing {function}"));
        assert_eq!(
            mask_rust_source(&body).matches("feed_taint_watermark(").count(),
            2,
            "[G-TAINT-TWO-PLUS-TWO-TEST-PLACEMENT] {function} owns two reads",
        );
    }
    assert!(
        ledger.contains("feed taint watermark consumer")
            && ledger.contains("same-sequence batch semantics")
            && ledger.contains("wi06_frontier_integrity_gate_is_wired"),
        "[G-TAINT-SAME-SEQUENCE-OBLIGATION] taint and batch semantics remain ledger-bound",
    );

    // One packed AtomicU64 is the sole replay authority; all lanes alias that Arc.
    let replay_struct = named_body(&state_raw, "struct ReplayIntegrityShared")
        .expect("[G-PACKED-AUTHORITY-ONE-WORD] ReplayIntegrityShared struct missing");
    assert_eq!(
        mask_rust_source(&replay_struct)
            .matches("packed: AtomicU64")
            .count(),
        1,
        "[G-PACKED-AUTHORITY-ONE-WORD] exactly one packed AtomicU64 in the shared authority",
    );
    for declaration in [
        "pub const REPLAY_FLAG_POISONED: u64 = 1 << 0;",
        "pub const REPLAY_FLAG_DISORDER: u64 = 1 << 1;",
        "pub const REPLAY_FLAG_CONFLICT: u64 = 1 << 2;",
    ] {
        assert!(
            state.contains(declaration),
            "[G-PACKED-AUTHORITY-ONE-WORD] missing flag declaration {declaration}",
        );
    }
    let replay_impl = named_body(&state_raw, "impl ReplayIntegrityShared")
        .expect("[G-PACKED-AUTHORITY-ONE-WORD] ReplayIntegrityShared impl missing");
    let replay_impl = mask_rust_source(&replay_impl);
    assert!(
        replay_impl.contains("self.packed.load(Ordering::Acquire)")
            && replay_impl.contains("self.packed.fetch_or(flag, Ordering::Release)")
            && replay_impl.contains("Ordering::AcqRel")
            && replay_impl.contains("generation == expected_generation && flags == 0"),
        "[G-PACKED-AUTHORITY-ONE-WORD] packed load/publish/repin/current predicate",
    );
    let publisher_impl = named_body(&state_raw, "impl StatePublisher")
        .expect("[G-ONE-SHARED-INTEGRITY-ARC] StatePublisher impl missing");
    assert_eq!(
        mask_rust_source(&publisher_impl)
            .matches("replay_integrity: Arc::new(ReplayIntegrityShared::new())")
            .count(),
        1,
        "[G-ONE-SHARED-INTEGRITY-ARC] one StatePublisher integrity Arc",
    );
    assert_eq!(
        production.matches("ReplayIntegrityShared::new()").count(),
        0,
        "[G-ONE-SHARED-INTEGRITY-ARC] pipeline production cannot construct another authority",
    );
    let start_threads = named_body(&pipeline_raw, "pub fn start_threads")
        .expect("[G-ONE-SHARED-INTEGRITY-ARC] start_threads missing");
    let start_threads = mask_rust_source(&start_threads);
    let wiring_end = start_threads
        .find("let ps_p = if binary_mode")
        .expect("[G-ONE-SHARED-INTEGRITY-ARC] wiring boundary missing");
    assert_eq!(
        start_threads[..wiring_end]
            .matches("Arc::clone(state_publisher.replay_integrity())")
            .count(),
        4,
        "[G-ONE-SHARED-INTEGRITY-ARC] revm/submit/rx/sim share the one Arc",
    );

    let wire_body = named_body(&pipeline_raw, "fn wire_authorization_current")
        .expect("[G-FINAL-WIRE-CLOSURE-BOTH-APIS] wire closure missing");
    let wire = mask_rust_source(&wire_body);
    assert!(
        wire.contains("feed_permit.is_current()")
            && wire.contains("replay_integrity.replay_integrity_current(candidate_pin_generation)")
            && wire.contains("&&"),
        "[G-FINAL-WIRE-CLOSURE-BOTH-APIS] permit and packed generation are both required",
    );
    let submit_body = named_body(&pipeline_raw, "fn submit_loop")
        .expect("[G-FINAL-WIRE-CLOSURE-BOTH-APIS] submit loop missing");
    let submit = mask_rust_source(&submit_body);
    assert!(
        submit.matches("submit_observed_authorized(").count() >= 1
            && submit.matches("submit_authorized(").count() >= 1
            && submit.matches("let mut authorize_attempt =").count() == 1
            && submit.contains("vc.integrity_pin_generation"),
        "[G-FINAL-WIRE-CLOSURE-BOTH-APIS] both lanes use one generation-bound closure",
    );
    let express_body = named_body(&pipeline_raw, "fn express_pop_prepare(")
        .expect("[G-EXPRESS-STORED-GENERATION] express pop missing");
    let express = mask_rust_source(&express_body);
    let stored = express
        .find("express.admission.integrity_pin_generation")
        .expect("[G-EXPRESS-STORED-GENERATION] stored generation check missing");
    let cooldown = express
        .find("let gen_at_start")
        .expect("[G-EXPRESS-STORED-GENERATION] cooldown region missing");
    assert!(
        stored < cooldown
            && express.contains("integrity_pin_generation: validation_pin_generation")
            && !express.contains("current_pin_generation()"),
        "[G-EXPRESS-STORED-GENERATION] stored generation is checked and carried unchanged",
    );

    // F01-F07 are pinned at their actual production seams.
    let enqueue_body = named_body(&pipeline_raw, "fn enqueue_revm_state_replay")
        .expect("[G-F01-POISON-BEFORE-ENQUEUE-LOSS-RETURN] enqueue seam missing");
    let enqueue = mask_rust_source(&enqueue_body);
    let holes = positions(&enqueue, "record_replay_hole(");
    assert_eq!(
        holes.len(),
        2,
        "[G-F01-POISON-BEFORE-ENQUEUE-LOSS-RETURN] both loss exits record the earliest hole",
    );
    for hole in holes {
        let false_after = enqueue[hole..].find("false");
        assert!(
            false_after.is_some_and(|pos| pos > 0),
            "[G-F01-POISON-BEFORE-ENQUEUE-LOSS-RETURN] hole record precedes loss return",
        );
    }
    let drain_body = named_body(&pipeline_raw, "fn drain_revm_state_replay_queue")
        .expect("[G-F02-POISON-BEFORE-POP] drain seam missing");
    let drain = mask_rust_source(&drain_body);
    let overflow = drain
        .find("if pending.len() > RING_CAPACITY")
        .expect("[G-F02-POISON-BEFORE-POP] overflow predicate missing");
    let overflow_fault = drain[overflow..]
        .find("replay_integrity.publish_fault(engine_state::publication::REPLAY_FLAG_POISONED)")
        .map(|p| p + overflow)
        .expect("[G-F02-POISON-BEFORE-POP] overflow poison missing");
    let overflow_pop = drain[overflow..]
        .find("pending.pop_front()")
        .map(|p| p + overflow)
        .expect("[G-F02-POISON-BEFORE-POP] overflow pop missing");
    assert!(
        overflow < overflow_fault && overflow_fault < overflow_pop,
        "[G-F02-POISON-BEFORE-POP] overflow poison precedes pop",
    );
    assert_eq!(
        drain.matches("replay_integrity.publish_fault(engine_state::publication::REPLAY_FLAG_POISONED)").count(),
        5,
        "[G-F03-ALL-FAILURE-EXITS-POISON] every built drain failure exit poisons",
    );
    let eviction = "if !dedup.contains(&event.tx_hash) && dedup.len() == dedup.capacity()";
    let eviction_pos = drain
        .find(eviction)
        .expect("[G-F04-EVICTION-RISK-PREDICATE] eviction-risk predicate missing");
    let eviction_fault = drain[eviction_pos..]
        .find("replay_integrity.publish_fault(engine_state::publication::REPLAY_FLAG_POISONED)")
        .map(|p| p + eviction_pos)
        .expect("[G-F04-EVICTION-RISK-PREDICATE] eviction poison missing");
    let insert = drain[eviction_pos..]
        .find("dedup.insert(event.tx_hash)")
        .map(|p| p + eviction_pos)
        .expect("[G-F04-EVICTION-RISK-PREDICATE] dedup insert missing");
    assert!(
        eviction_pos < eviction_fault && eviction_fault < insert,
        "[G-F04-EVICTION-RISK-PREDICATE] poison precedes a capacity eviction-risk insert",
    );

    let publish_body = named_body(&pipeline_raw, "fn publish_binary_frame")
        .expect("[G-F06-OBSERVE-BEFORE-EARLY-RETURN] publish seam missing");
    let publish = mask_rust_source(&publish_body);
    let observe = publish
        .find("let _ = observe_feed_contiguity(")
        .expect("[G-F06-OBSERVE-BEFORE-EARLY-RETURN] contiguity observation missing");
    for early in [
        "if v.is_taint_marker()",
        "if v.is_ignorable_ordinal_marker()",
        "if v.authority() == engine_feed::FeedFrameAuthority::NonAuthoritative",
        "let parsed = match slab_slot.take()",
    ] {
        let at = publish
            .find(early)
            .unwrap_or_else(|| panic!("[G-F06-OBSERVE-BEFORE-EARLY-RETURN] missing branch {early}"));
        assert!(
            observe < at,
            "[G-F06-OBSERVE-BEFORE-EARLY-RETURN] observe precedes {early}",
        );
    }

    assert_eq!(
        production.matches("engine_feed::feed_frame::is_feed_payload_conflict_sentinel(").count(),
        4,
        "[G-F07-EXACT-SENTINEL-FOUR-RX] four production classifiers",
    );
    assert!(
        feed.contains("pub const FEED_PAYLOAD_CONFLICT_SENTINEL_MAGIC: u32 = 0xD2C0_0F07;")
            && feed.contains("pub const FEED_PAYLOAD_CONFLICT_SENTINEL: [u8; FEED_FRAME_HEADER_LEN]")
            && feed.contains("buf == FEED_PAYLOAD_CONFLICT_SENTINEL.as_slice()"),
        "[G-F07-EXACT-SENTINEL-FOUR-RX] exact 49-byte sentinel comparison",
    );
    let latch_body = named_body(&pipeline_raw, "fn latch_binary_feed_payload_conflict")
        .expect("[G-F07-EXACT-SENTINEL-FOUR-RX] latch seam missing");
    let latch = mask_rust_source(&latch_body);
    let fault = latch
        .find("replay_integrity.publish_fault(engine_state::publication::REPLAY_FLAG_CONFLICT)")
        .expect("[G-F07-EXACT-SENTINEL-FOUR-RX] conflict poison missing");
    let dark = latch
        .find("latch_feed_state_degraded(kill,")
        .expect("[G-F07-EXACT-SENTINEL-FOUR-RX] legacy latch missing");
    assert!(
        fault < dark,
        "[G-F07-EXACT-SENTINEL-FOUR-RX] conflict is published before legacy latch",
    );

    let layout = read_baseline_json("hot_payload_layout.json");
    let payloads = layout["payloads"]
        .as_array()
        .expect("[G-VALIDATED-CANDIDATE-208B] payload rows");
    let vc = payloads
        .iter()
        .find(|row| row["type_name"].as_str() == Some("ValidatedCandidate"))
        .expect("[G-VALIDATED-CANDIDATE-208B] VC row");
    assert_eq!(
        vc["size_bytes"].as_u64(),
        Some(208),
        "[G-VALIDATED-CANDIDATE-208B] ValidatedCandidate remains 208B",
    );
    let vc_struct = named_body(&pipeline_raw, "struct ValidatedCandidate")
        .expect("[G-VALIDATED-CANDIDATE-208B] VC struct");
    assert_eq!(
        mask_rust_source(&vc_struct).matches("integrity_pin_generation: u64").count(),
        1,
        "[G-VALIDATED-CANDIDATE-208B] one generation field",
    );

    for assertion in [
        "[C1-EXECUTOR-IS-APPLIED-ORACLE]",
        "[C6-BUDGET-DISPOSITION-NONE]",
        "[C2-DUPLICATE-AT-MOST-ONCE]",
        "[C2-DUPLICATE-FRONTIER-UNCHANGED]",
        "[C2-DUPLICATE-AT-CAPACITY-CLEAN]",
        "[C2-FIRST-EVICTION-RISK-POISONS]",
        "[C2-REAL-VC-STAMP]",
        "[C2-AUTHORITY-REVOKED]",
        "[C2-DEFINITELY-NOT-WRITTEN]",
        "[C2-ZERO-RAW-POST]",
        "[C2-CLEAN-CONTROL-ONE-RAW]",
        "[C3-EARLIEST-HOLE-MIN]",
        "[C3-POISONED-AT-REAL-ENQUEUE-SEAM]",
        "[C3-PIN-NONCURRENT]",
        "[C3-WHOLE-PIN-NOT-SUFFIX]",
        "[C3-BELOW-ZERO-RAW]",
        "[C3-BETWEEN-ZERO-RAW]",
        "[C3-ABOVE-ZERO-RAW]",
        "[C3-CLEAN-ENQUEUE-ONE-RAW]",
        "[C4-EXACT-BOUND-CLEAN]",
        "[C4-DROP-COUNT-EXACT]",
        "[C4-FIRST-DROPPED-EXACT]",
        "[C4-WHOLE-PIN-POISONED]",
        "[C4-REAL-VC-STAMP]",
        "[C4-AUTHORITY-REVOKED]",
        "[C4-DEFINITELY-NOT-WRITTEN]",
        "[C4-ZERO-RAW-POST]",
        "[C5-RECOVERY-FAILURE-POISONS]",
        "[C5-DECODE-FAILURE-POISONS]",
        "[C5-EXECUTION-FAILURE-POISONS]",
        "[C5-FIRST-FAILED-EVENT-EXACT]",
        "[C5-BELOW-SUSPECT-THRESHOLD]",
        "[C5-EXECUTOR-STOPS-BEFORE-FAILURE]",
        "[C5-REAL-VC-STAMP]",
        "[C5-AUTHORITY-REVOKED]",
        "[C5-DEFINITELY-NOT-WRITTEN]",
        "[C5-ZERO-RAW-POST]",
        "[C5-SUCCESS-CONTROL-ONE-RAW]",
        "[C9-DENSE-CONTROL-CLEAN]",
        "[C9-REAL-V4-WIRE-ORDINALS]",
        "[C9-GAP-COUNT-EXACT]",
        "[C9-DISORDER-AT-INGRESS]",
        "[C9-NO-HIGHER-LEAF-VC]",
        "[C9-VALIDATOR-ZERO-RAW]",
        "[C9-EXPRESS-POP-DECLINE]",
        "[C9-STICKY-NO-SELF-RESTORE]",
        "[C10-F01-REAL-SEAM]",
        "[C10-F01-POISONED]",
        "[C10-F01-SIM-DECLINE]",
        "[C10-F01-POP-DECLINE]",
        "[C10-F01-ZERO-RAW]",
        "[C10-F02-REAL-SEAM]",
        "[C10-F02-POISONED]",
        "[C10-F02-SIM-DECLINE]",
        "[C10-F02-POP-DECLINE]",
        "[C10-F02-ZERO-RAW]",
        "[C10-F03-REAL-SEAM]",
        "[C10-F03-POISONED]",
        "[C10-F03-SIM-DECLINE]",
        "[C10-F03-POP-DECLINE]",
        "[C10-F03-ZERO-RAW]",
        "[C10-F04-REAL-SEAM]",
        "[C10-F04-POISONED]",
        "[C10-F04-SIM-DECLINE]",
        "[C10-F04-POP-DECLINE]",
        "[C10-F04-ZERO-RAW]",
        "[C10-F05-REAL-SEAM]",
        "[C10-F05-CLEAN-NONFAULT]",
        "[C10-F05-NO-VC]",
        "[C10-F05-NO-EXPRESS-ENTRY]",
        "[C10-F05-ZERO-RAW]",
        "[C10-F05-CLEAN-RESERVICE]",
        "[C10-F06-REAL-SEAM]",
        "[C10-F06-DISORDER]",
        "[C10-F06-SIM-DECLINE]",
        "[C10-F06-POP-DECLINE]",
        "[C10-F06-ZERO-RAW]",
        "[C10-F07-REAL-SEAM]",
        "[C10-F07-CONFLICT]",
        "[C10-F07-SIM-DECLINE]",
        "[C10-F07-POP-DECLINE]",
        "[C10-F07-ZERO-RAW]",
        "[C10-F07-OWNED-STREAM-CONFLICT]",
        "[C10-F07-SLAB-STREAM-CONFLICT]",
        "[C10-F07-OWNED-SEQPACKET-CONFLICT]",
        "[C10-F07-SLAB-SEQPACKET-CONFLICT]",
        "[C10-F07-NO-PARSED-ITEM]",
        "[C10-F07-NEAR-MISS-NOT-TYPED]",
        "[C12-AUTHORITY-REVOKED]",
        "[C12-VALIDATOR-REAL-ACCEPT-STAMP]",
        "[C12-EXPRESS-REAL-ACCEPT-STAMP]",
        "[C12-FAULT-AFTER-ACCEPT]",
        "[C12-DEFINITELY-NOT-WRITTEN]",
        "[C12-WARM-PRIMARY-ZERO-RAW]",
        "[C12-WARM-SPARE-ZERO-RAW]",
        "[C12-RECONNECT-ZERO-RAW]",
        "[C12-OBSERVED-ZERO-RAW]",
        "[C12-CLEAN-CONTROL-ONE-RAW]",
        "[C13-VALIDATOR-REAL-STAMP-N]",
        "[C13-EXPRESS-ADMISSION-STAMP-N]",
        "[C13-EXPRESS-ACCEPT-STAMP-N]",
        "[C13-TEST-ONLY-ATOMIC-N-PLUS-ONE]",
        "[C13-OLD-EXPRESS-POP-DECLINE]",
        "[C13-OLD-VALIDATOR-AUTHORITY-REVOKED]",
        "[C13-OLD-EXPRESS-AUTHORITY-REVOKED]",
        "[C13-DEFINITELY-NOT-WRITTEN]",
        "[C13-OLD-N-ZERO-RAW]",
        "[C13-FRESH-N-PLUS-ONE-ONE-RAW]",
        "[G-F02-POISON-BEFORE-POP]",
    ] {
        assert!(
            pipeline_raw.contains(assertion),
            "[G-NAMED-ASSERTIONS] required assertion ID is missing: {assertion}",
        );
    }

    // Keep the retained production stream comparison last so a compile-preserving source mutant
    // is diagnosed by its specific named structural tooth first (for example F02 placement),
    // while a clean checkout still proves canonical production identity before the gate returns.
    let base_output = std::process::Command::new("git")
        .args(["show", "259c551:engine/crates/engine/src/pipeline.rs"])
        .current_dir(&root)
        .output()
        .expect("[G-PRODUCTION-BYTE-IDENTITY] git show base");
    assert!(base_output.status.success(),
        "[G-PRODUCTION-BYTE-IDENTITY] base source available");
    let base_pipeline = String::from_utf8(base_output.stdout)
        .expect("[G-PRODUCTION-BYTE-IDENTITY] base UTF-8");
    let current_production = canonical_t8_sniff_production_view(&pipeline_raw);
    let base_production = canonical_t8_sniff_production_view(&base_pipeline);
    let first_difference = current_production
        .bytes()
        .zip(base_production.bytes())
        .position(|(left, right)| left != right)
        .or_else(|| (current_production.len() != base_production.len()).then_some(
            current_production.len().min(base_production.len()),
        ));
    let diff_context = first_difference.map(|pos| {
        let start = pos.saturating_sub(80);
        let end_current = (pos + 120).min(current_production.len());
        let end_base = (pos + 120).min(base_production.len());
        format!(
            "current={:?} base={:?}",
            String::from_utf8_lossy(&current_production.as_bytes()[start..end_current]),
            String::from_utf8_lossy(&base_production.as_bytes()[start..end_base]),
        )
    });
    assert!(
        first_difference.is_none(),
        "[G-PRODUCTION-BYTE-IDENTITY] production bytes changed at {:?} (current_len={}, base_len={}): {}",
        first_difference,
        current_production.len(),
        base_production.len(),
        diff_context.unwrap_or_default(),
    );
}
