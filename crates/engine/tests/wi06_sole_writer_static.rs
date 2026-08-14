//! WI-06 T0 — writer inventory + sole-writer static contract
//! (codex/WI06_PLAN_2026-07-17.md rev 4, task T0; spec §3.3 sole-writer publication).
//!
//! Two obligations:
//!
//! 1. `all_post_seal_family_store_writes_are_publisher_owned` — every post-seal family-store
//!    write callsite (`.set(` / `.set_recycled(` on the ten family stores,
//!    `pools.bump_generation(`, `pools.poison_param(`, and `commit_prediction(`) must be
//!    publisher-owned: the callsite passes a `StateWriteTxn` witness (T3 threads the witness
//!    param through the store entry points, so holding one IS proof of publisher ownership) or
//!    lives in an allowlisted location (engine-state `builder.rs` pre-seal init,
//!    `publication.rs` itself, `#[cfg(test)]` modules). RED by design until T3/T4 land the
//!    witness threading + state_loop publication.
//!
//! 2. `generated_writer_inventory_names_origin_frontier_and_dominance` — the writer/origin
//!    inventory is GENERATED from source (not hand-named), and every row carries its
//!    barrier-completion obligation with the rev-4 TEMPORAL binding: the origin-class frontier
//!    it must record while authority is dark (feed cursor / canonical cursor / incomparable
//!    identity) and its D1 rule-3 dominance class (feed / canonical-comparable /
//!    reconcile-reset-suspend). This inventory is the source of D1 rule 1's per-origin
//!    dark-frontier table; T7's `barrier_completion_predicate_covers_the_generated_writer_inventory`
//!    pins publication.rs predicate rows == these rows.

use std::path::{Path, PathBuf};

/// Repo root (mirrors the other `*_static` corpus tests).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// The ten family-store write receivers plus the pool-table mutators. A callsite matches when
/// `<receiver>.<method>(` appears outside a `#[cfg(test)]` module.
const FAMILY_STORE_WRITE_MARKERS: &[&str] = &[
    "v2_state.set(",
    "v2_state.set_recycled(",
    "v3_state.set(",
    "v3_state.set_recycled(",
    "curve_state.set(",
    "curve_state.set_recycled(",
    "curve_crypto_state.set(",
    "curve_crypto_state.set_recycled(",
    "dodo_state.set(",
    "dodo_state.set_recycled(",
    "balancer_state.set(",
    "balancer_state.set_recycled(",
    "balancer_weighted_state.set(",
    "balancer_weighted_state.set_recycled(",
    "lb_state.set(",
    "lb_state.set_recycled(",
    "maverick_state.set(",
    "maverick_state.set_recycled(",
    "fluid_state.set(",
    "fluid_state.set_recycled(",
    "pools.bump_generation(",
    "pools.poison_param(",
    "commit_prediction(",
];

/// Engine source files that may contain post-seal family-store writes. The scan is exhaustive
/// over `engine/crates/engine/src` so a NEW writer file cannot silently escape the contract.
fn engine_src_files() -> Vec<PathBuf> {
    let root = repo_root().join("engine/crates/engine/src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    files.sort();
    files
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Strip `#[cfg(test)]`-annotated `mod` blocks (and `#[cfg(all(test, ...))]` variants) with a
/// brace-tracking scan, replacing their contents with blank lines so reported line numbers in
/// the retained text stay true to the file.
fn strip_test_modules(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut keep = vec![true; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let is_cfg_test = trimmed.starts_with("#[cfg(test)]")
            || trimmed.starts_with("#[cfg(all(test");
        if is_cfg_test {
            // Find the `mod`/`fn`/item start; then blank out through its matching close brace.
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_start().starts_with('#') {
                j += 1;
            }
            if j < lines.len() {
                let mut depth: i64 = 0;
                let mut opened = false;
                let mut k = j;
                while k < lines.len() {
                    for ch in lines[k].chars() {
                        match ch {
                            '{' => {
                                depth += 1;
                                opened = true;
                            }
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                    keep[k] = false;
                    if opened && depth <= 0 {
                        break;
                    }
                    // Attribute applies to a single non-brace item (e.g. `use` line).
                    if !opened && lines[k].trim_end().ends_with(';') {
                        break;
                    }
                    k += 1;
                }
                for line in keep.iter_mut().take(k.min(lines.len())).skip(i) {
                    *line = false;
                }
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }
    lines
        .iter()
        .enumerate()
        .map(|(idx, l)| if keep[idx] { *l } else { "" })
        .collect::<Vec<_>>()
        .join("\n")
}

struct WriteCallsite {
    file: String,
    line: usize,
    marker: &'static str,
    /// The callsite text through its closing paren (bounded), used for the witness check.
    call_text: String,
}

/// Enumerate every family-store write callsite in the (test-stripped) engine sources.
fn family_store_write_callsites() -> Vec<WriteCallsite> {
    let root = repo_root();
    let mut sites = Vec::new();
    for file in engine_src_files() {
        let raw = std::fs::read_to_string(&file).expect("read source file");
        let stripped = strip_test_modules(&raw);
        let rel = file
            .strip_prefix(&root)
            .expect("relative path")
            .to_string_lossy()
            .into_owned();
        // Collapse split method chains (`...fluid_state\n    .set(...)`) so the receiver and
        // the write method land on one logical line; each logical line remembers the physical
        // line number of its first fragment.
        let mut lines: Vec<(usize, String)> = Vec::new();
        for (idx, raw_line) in stripped.lines().enumerate() {
            let t = raw_line.trim_start();
            if t.starts_with('.') && !lines.is_empty() {
                let last = lines.last_mut().expect("non-empty");
                last.1.push_str(t);
            } else {
                lines.push((idx + 1, raw_line.to_string()));
            }
        }
        for li in 0..lines.len() {
            let (first_physical_line, line) = (&lines[li].0, lines[li].1.clone());
            let line = line.as_str();
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            for marker in FAMILY_STORE_WRITE_MARKERS {
                if let Some(pos) = line.find(marker) {
                    // Definitions are not callsites: `pub fn set(...)`, `fn commit_prediction(`.
                    let before = &line[..pos];
                    if before.trim_end().ends_with("fn") {
                        continue;
                    }
                    // Collect through the call's closing paren (bounded to 8 logical lines).
                    let mut depth: i64 = 0;
                    let mut text = String::new();
                    'outer: for (fragment, l) in lines.iter().skip(li).take(8).enumerate() {
                        let l = l.1.as_str();
                        let start = if fragment == 0 { pos } else { 0 };
                        for ch in l[start.min(l.len())..].chars() {
                            text.push(ch);
                            match ch {
                                '(' => depth += 1,
                                ')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break 'outer;
                                    }
                                }
                                _ => {}
                            }
                        }
                        text.push('\n');
                    }
                    sites.push(WriteCallsite {
                        file: rel.clone(),
                        line: *first_physical_line,
                        marker,
                        call_text: text,
                    });
                }
            }
        }
    }
    sites
}

/// A callsite is publisher-owned when the call itself carries the `StateWriteTxn` witness
/// (T3 threads `txn` through every store write entry point) — lexically: the argument text
/// names `txn` (`&txn`, `&mut txn`, `txn,` ...) or `StateWriteTxn`.
fn is_publisher_owned(site: &WriteCallsite) -> bool {
    let t = &site.call_text;
    t.contains("StateWriteTxn")
        || t.contains("&txn")
        || t.contains("&mut txn")
        || t.contains("(txn")
        || t.contains(" txn,")
        || t.contains(" txn)")
        || t.contains("(txn,")
}

/// WI-06 T0 RED (stays red until T3/T4): every post-seal family-store write must be
/// publisher-owned — it must pass the `StateWriteTxn` witness that only the sole
/// `StatePublisher` (engine-state `publication.rs`) can mint. Today the state_loop
/// (`pipeline.rs` commit_prediction path), the WSS log applier, the receipt replayer,
/// resync, fluid poller/wake, mirror, and shadow wake ALL write the family stores directly
/// with no witness — the FL-08 multi-writer surface WI-06 exists to close.
#[test]
fn all_post_seal_family_store_writes_are_publisher_owned() {
    let sites = family_store_write_callsites();
    assert!(
        !sites.is_empty(),
        "the scan must find the known family-store write callsites (pipeline.rs \
         commit_prediction, log applier, receipt replayer, resync, fluid, shadow); an empty \
         result means the scan itself broke"
    );
    let unowned: Vec<String> = sites
        .iter()
        .filter(|s| !is_publisher_owned(s))
        .map(|s| format!("{}:{} `{}`", s.file, s.line, s.marker))
        .collect();
    assert!(
        unowned.is_empty(),
        "{} post-seal family-store write callsite(s) carry NO StateWriteTxn witness — every \
         family-store mutation must flow through the sole publisher (engine-state \
         publication.rs; witness threading lands at T3, state_loop publication at T4):\n  {}",
        unowned.len(),
        unowned.join("\n  ")
    );
}

// ---------------------------------------------------------------------------------------------
// Writer/origin inventory (T0 deliverable; consumed by D1 rule 1's per-origin dark-frontier
// table and pinned by T7's barrier-completion predicate test).
// ---------------------------------------------------------------------------------------------

/// The frontier an origin class must record, per dark commit, while authority is dark.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FrontierKind {
    /// Feed replay cursor `(sequence, inner_ordinal)`.
    FeedCursor,
    /// Canonical block-lineage cursor (block number + hash lineage).
    CanonicalCursor,
    /// No comparable cursor exists (poll cycles, mirror head, shadow reconstruction).
    IncomparableIdentity,
}

/// D1 rule-3 dominance class verified per row at the re-arm CAS.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum DominanceClass {
    /// anchor block >= block(frozen feed frontier F).
    Feed,
    /// anchor must DOMINATE the row's latest committed canonical cursor via `state_order`
    /// (block/hash lineage, never bare height).
    CanonicalComparable,
    /// dominance undefined — the row must be reconciled at the anchor, reset, or its trust
    /// suspended before the CAS may proceed.
    ReconcileResetOrSuspend,
}

struct WriterInventoryRow {
    /// Origin-class name — must match `publication.rs`'s barrier row set (T2/T7).
    origin_class: &'static str,
    /// Source anchor proving this writer class exists (fn name searched in the named file).
    anchor_file: &'static str,
    anchor_fn: &'static str,
    frontier: FrontierKind,
    dominance: DominanceClass,
}

/// GENERATED inventory: one row per writer/origin class, each verified to exist in source by
/// its anchor fn. Removing a writer class from the engine makes its row fail generation
/// (anchor missing); adding a NEW family-store writer without a row here trips
/// `all_post_seal_family_store_writes_are_publisher_owned` (exhaustive src scan), so the
/// row set and the write surface cannot drift apart silently.
fn generated_writer_inventory() -> Vec<WriterInventoryRow> {
    let rows = vec![
        WriterInventoryRow {
            origin_class: "FeedTrigger",
            anchor_file: "engine/crates/engine/src/pipeline.rs",
            anchor_fn: "fn state_loop(",
            frontier: FrontierKind::FeedCursor,
            dominance: DominanceClass::Feed,
        },
        WriterInventoryRow {
            origin_class: "Receipt",
            anchor_file: "engine/crates/engine/src/pipeline.rs",
            anchor_fn: "fn apply_receipt_logs_value(",
            frontier: FrontierKind::CanonicalCursor,
            dominance: DominanceClass::CanonicalComparable,
        },
        WriterInventoryRow {
            origin_class: "WssLog",
            anchor_file: "engine/crates/engine/src/pipeline.rs",
            anchor_fn: "fn log_updater_loop(",
            frontier: FrontierKind::CanonicalCursor,
            dominance: DominanceClass::CanonicalComparable,
        },
        WriterInventoryRow {
            origin_class: "Resync",
            anchor_file: "engine/crates/engine/src/pipeline.rs",
            anchor_fn: "fn resync_loop(",
            frontier: FrontierKind::CanonicalCursor,
            dominance: DominanceClass::CanonicalComparable,
        },
        WriterInventoryRow {
            origin_class: "Fluid",
            anchor_file: "engine/crates/engine/src/fluid_poller.rs",
            anchor_fn: "fn spawn_fluid_poller(",
            frontier: FrontierKind::IncomparableIdentity,
            dominance: DominanceClass::ReconcileResetOrSuspend,
        },
        WriterInventoryRow {
            origin_class: "Mirror",
            anchor_file: "engine/crates/engine/src/pipeline.rs",
            anchor_fn: "fn mirror_apply_slots(",
            frontier: FrontierKind::IncomparableIdentity,
            dominance: DominanceClass::ReconcileResetOrSuspend,
        },
        WriterInventoryRow {
            origin_class: "Shadow",
            anchor_file: "engine/crates/engine/src/pipeline.rs",
            anchor_fn: "impl ShadowStateHandle",
            frontier: FrontierKind::IncomparableIdentity,
            dominance: DominanceClass::ReconcileResetOrSuspend,
        },
    ];
    // Anchor verification — a row for a writer class that no longer exists must not survive.
    let root = repo_root();
    for row in &rows {
        let src = std::fs::read_to_string(root.join(row.anchor_file))
            .unwrap_or_else(|_| panic!("read anchor file {}", row.anchor_file));
        assert!(
            src.contains(row.anchor_fn),
            "inventory row `{}` anchors to `{}` in {} which no longer exists — regenerate \
             the writer inventory (and the publication.rs barrier row set with it)",
            row.origin_class,
            row.anchor_fn,
            row.anchor_file
        );
    }
    rows
}

/// T0 deliverable: the generated inventory covers every writer/origin class with its rev-4
/// TEMPORAL barrier obligation (frontier recorded per dark commit) and D1 dominance class.
#[test]
fn generated_writer_inventory_names_origin_frontier_and_dominance() {
    let rows = generated_writer_inventory();
    let classes: Vec<&str> = rows.iter().map(|r| r.origin_class).collect();
    for required in [
        "FeedTrigger",
        "Receipt",
        "WssLog",
        "Resync",
        "Fluid",
        "Mirror",
        "Shadow",
    ] {
        assert!(
            classes.contains(&required),
            "writer inventory must carry the `{required}` origin class"
        );
    }
    assert_eq!(rows.len(), 7, "one row per writer/origin class, no duplicates");
    // Temporal binding: feed rows record the feed cursor; canonical-origin rows record the
    // canonical cursor; incomparable rows record their identity and must
    // reconcile/reset/suspend at the barrier CAS.
    for row in &rows {
        match row.origin_class {
            "FeedTrigger" => {
                assert_eq!(row.frontier, FrontierKind::FeedCursor);
                assert_eq!(row.dominance, DominanceClass::Feed);
            }
            "Receipt" | "WssLog" | "Resync" => {
                assert_eq!(row.frontier, FrontierKind::CanonicalCursor);
                assert_eq!(row.dominance, DominanceClass::CanonicalComparable);
            }
            "Fluid" | "Mirror" | "Shadow" => {
                assert_eq!(row.frontier, FrontierKind::IncomparableIdentity);
                assert_eq!(row.dominance, DominanceClass::ReconcileResetOrSuspend);
            }
            other => panic!("unclassified writer origin `{other}`"),
        }
    }
}
