use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn line_number_at(src: &str, byte_offset: usize) -> usize {
    src[..byte_offset].bytes().filter(|b| *b == b'\n').count() + 1
}

fn dirty_event_initializer_blocks(src: &str) -> Vec<(usize, &str)> {
    let mut blocks = Vec::new();
    let mut search_from = 0;

    while let Some(relative) = src[search_from..].find("DirtyEvent {") {
        let start = search_from + relative;
        let line_start = src[..start].rfind('\n').map_or(0, |idx| idx + 1);
        let line = src[line_start..src[start..].find('\n').map_or(src.len(), |idx| start + idx)]
            .trim_start();
        search_from = start + "DirtyEvent {".len();

        if line.starts_with("struct DirtyEvent") {
            continue;
        }

        let open_brace = src[start..].find('{').expect("initializer open brace") + start;
        let mut depth = 0usize;
        let mut end = None;
        for (idx, ch) in src[open_brace..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(open_brace + idx + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.expect("initializer closing brace");
        blocks.push((line_number_at(src, start), &src[start..end]));
    }

    blocks
}

fn has_tx_hash_initializer(block: &str) -> bool {
    block
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with("tx_hash:") || line == "tx_hash,")
}

#[test]
fn dirty_event_tx_hash_initializers_are_complete_in_current_pipeline_source() {
    let pipeline = repo_root().join("engine/crates/engine/src/pipeline.rs");
    let src = fs::read_to_string(&pipeline).expect("read pipeline.rs");

    assert!(
        src.contains("struct DirtyEvent") && src.contains("tx_hash: [u8; 32]"),
        "DirtyEvent must currently require tx_hash for this blocker to be accurate"
    );

    let blocks = dirty_event_initializer_blocks(&src);
    let missing: Vec<usize> = blocks
        .iter()
        .filter_map(|(line, block)| (!has_tx_hash_initializer(block)).then_some(*line))
        .collect();

    // 14 = the 13 long-standing sites + the WI-01d fail-dark harness emission (landed with the
    // campaign oracle rounds; this pin lagged because engine-feed --tests is outside the
    // campaign gate set). The load-bearing invariant is the `missing` assert below — every
    // initializer names tx_hash — which held throughout; the count only guards staleness.
    assert_eq!(blocks.len(), 14, "unexpected DirtyEvent initializer count");
    assert_eq!(
        missing,
        Vec::<usize>::new(),
        "DirtyEvent initializer(s) still missing tx_hash: {missing:?}"
    );
}

#[test]
#[ignore = "docs-lint: reads repository Markdown; excluded from correctness certification"]
fn report_records_dirty_event_build_blocker_as_cleared() {
    let report = repo_root().join("Codex/FRESH_MONEY_PATH_CERTIFICATION_TEST_REPORT_2026-06-14.md");
    let report = fs::read_to_string(&report).expect("read living report");

    assert!(report.contains("Cleared Production Build Blocker"));
    assert!(report.contains("DirtyEvent"));
    assert!(report.contains("tx_hash"));
    assert!(report.contains("every current `DirtyEvent` initializer includes `tx_hash`"));
    assert!(report.contains("compile blocker is cleared in the current worktree"));
}
