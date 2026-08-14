//! Paper-mode candidate recorder (win-rate harness, part A).
//!
//! In dry-run the engine runs the entire detect → size → validate → would-submit path but never
//! POSTs. This module writes one structured JSONL row per would-submit decision so an offline
//! reconciler can later ask "was this opportunity taken by a competitor, and would we have beaten
//! their inclusion?" — yielding a capture-rate with ZERO money at risk.
//!
//! Distinct from `CANDIDATE_RECORD_JSONL` (a REVM-validation artifact keyed on replay `seq`): a
//! paper record carries the wall-clock `timestamp_ns` and the live L2 `block_number_seen` needed
//! to align against on-chain competitor txs.

/// One would-submit decision in dry-run, recorded for offline win-rate reconciliation. Large
/// integers (`amount_in`, profits) are carried as decimal strings so no precision is lost going
/// through JSON (wei values overflow f64 and u64).
#[derive(Debug, Clone)]
pub struct PaperRecord {
    /// Wall-clock nanoseconds when the engine decided it would submit (for timing vs competitors).
    pub timestamp_ns: u64,
    /// Engine route id.
    pub route_id: u32,
    /// The live L2 block number observed at detection (`seq + nitro genesis`).
    pub block_number_seen: u64,
    /// Input size in base-token wei (decimal string).
    pub amount_in: String,
    /// Predicted gross profit in base-token wei (decimal string).
    pub gross_profit_wei: String,
    /// Predicted net profit in base-token wei after gas/fees, if known (decimal string).
    pub net_profit_wei: Option<String>,
    /// `0x`-prefixed pool addresses of the route's hops, in order.
    pub pools: Vec<String>,
}

impl PaperRecord {
    /// Serialise to a single JSONL line (no trailing newline).
    pub fn to_json_line(&self) -> String {
        serde_json::json!({
            "timestamp_ns": self.timestamp_ns,
            "route_id": self.route_id,
            "block_number_seen": self.block_number_seen,
            "amount_in": self.amount_in,
            "gross_profit_wei": self.gross_profit_wei,
            "net_profit_wei": self.net_profit_wei,
            "pools": self.pools,
        })
        .to_string()
    }
}

/// Open the paper-mode JSONL sink from the `PAPER_MODE_LOG` env var. Returns `None` (recorder
/// disabled) when the var is unset or empty — paper mode is strictly opt-in, like the existing
/// `CANDIDATE_RECORD_JSONL`. On an open/mkdir failure it logs and disables (never aborts the run).
pub fn open_paper_log() -> Option<std::fs::File> {
    let path = std::env::var("PAPER_MODE_LOG").ok()?;
    if path.trim().is_empty() {
        return None;
    }
    open_paper_log_at(&path)
}

/// Open (APPENDING) the paper-mode JSONL file at `path`, making parent dirs as needed. Split
/// from [`open_paper_log`] so the file logic is testable without mutating process env.
/// Append, never truncate: the 24/7 watchdog chain restarts the engine unattended, and paper
/// records are the win-rate evidence — a truncating reopen erased a whole box night down to the
/// post-restart tail (2026-07-08). JSONL is self-delimiting, so appended runs stay parseable.
pub fn open_paper_log_at(path: &str) -> Option<std::fs::File> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(path = %path, error = ?e, "paper mode: failed to create parent dir");
                return None;
            }
        }
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => {
            tracing::info!(path = %path, "paper mode JSONL enabled");
            Some(f)
        }
        Err(e) => {
            tracing::warn!(path = %path, error = ?e, "paper mode: failed to open JSONL");
            None
        }
    }
}

/// Append one paper record as a JSONL line (record + `\n`) to any writer. Generic over the sink
/// so it is testable against an in-memory buffer and used against a `File` in production.
pub fn write_paper_record<W: std::io::Write>(w: &mut W, rec: &PaperRecord) -> std::io::Result<()> {
    let line = rec.to_json_line();
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_json_line_has_reconciler_fields() {
        let rec = PaperRecord {
            timestamp_ns: 1_780_000_000_000_000_000,
            route_id: 362,
            block_number_seen: 465_686_729,
            amount_in: "11718742621174166240".to_string(),
            gross_profit_wei: "2837294824172502".to_string(),
            net_profit_wei: Some("1736990135153531".to_string()),
            pools: vec![
                "0x1666ea6e83bcc3473111732ef086ba47e6eddeb7".to_string(),
                "0xc6962004f452be9203591991d15f6b388e09e8d0".to_string(),
            ],
        };
        let line = rec.to_json_line();
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(v["timestamp_ns"], 1_780_000_000_000_000_000u64);
        assert_eq!(v["route_id"], 362);
        assert_eq!(v["block_number_seen"], 465_686_729u64);
        assert_eq!(v["amount_in"], "11718742621174166240");
        assert_eq!(v["gross_profit_wei"], "2837294824172502");
        assert_eq!(v["net_profit_wei"], "1736990135153531");
        assert_eq!(v["pools"][0], "0x1666ea6e83bcc3473111732ef086ba47e6eddeb7");
        assert_eq!(v["pools"][1], "0xc6962004f452be9203591991d15f6b388e09e8d0");
        // One line, no embedded newline (JSONL invariant).
        assert!(!line.contains('\n'));
    }

    fn sample(route_id: u32) -> PaperRecord {
        PaperRecord {
            timestamp_ns: 1_780_000_000_000_000_000,
            route_id,
            block_number_seen: 465_686_729,
            amount_in: "1000".to_string(),
            gross_profit_wei: "5".to_string(),
            net_profit_wei: None,
            pools: vec!["0xabc".to_string()],
        }
    }

    #[test]
    fn write_paper_record_appends_newline_terminated_jsonl() {
        let mut buf: Vec<u8> = Vec::new();
        write_paper_record(&mut buf, &sample(1)).unwrap();
        write_paper_record(&mut buf, &sample(2)).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one line per record");
        let v0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let v1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v0["route_id"], 1);
        assert_eq!(v1["route_id"], 2);
        // net_profit_wei None serialises as JSON null.
        assert!(v0["net_profit_wei"].is_null());
        assert!(
            text.ends_with('\n'),
            "trailing newline so appends stay line-delimited"
        );
    }

    #[test]
    fn open_paper_log_at_creates_parent_dirs_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("paper_mode_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/paper.jsonl");
        let path_str = path.to_str().unwrap().to_string();

        let mut f = open_paper_log_at(&path_str).expect("opener creates file + parent dirs");
        write_paper_record(&mut f, &sample(7)).unwrap();
        write_paper_record(&mut f, &sample(8)).unwrap();
        drop(f);

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["route_id"], 7);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopen_appends_across_restarts_never_truncates() {
        // [P1.2] The 24/7 watchdog chain restarts the engine unattended (proven 2026-07-08:
        // sidecar stall → systemd relaunch). A truncating opener erased the whole paper-mode
        // history on every restart — the box night ended with 2 records. Reopening MUST append:
        // paper.jsonl is the P2.2 win-rate evidence and must survive self-heal cycles.
        let dir = std::env::temp_dir().join(format!("paper_mode_append_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("paper.jsonl");
        let path_str = path.to_str().unwrap().to_string();

        let mut run1 = open_paper_log_at(&path_str).expect("first open");
        write_paper_record(&mut run1, &sample(7)).unwrap();
        drop(run1); // engine dies (watchdog kill)…

        let mut run2 = open_paper_log_at(&path_str).expect("reopen after restart");
        write_paper_record(&mut run2, &sample(8)).unwrap();
        drop(run2);

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "restart must APPEND, never erase prior records: {content:?}"
        );
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["route_id"], 7, "pre-restart record survives");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_paper_log_is_none_without_env() {
        // The recorder is OFF unless PAPER_MODE_LOG is set (no env mutation in the test).
        assert!(
            std::env::var("PAPER_MODE_LOG").is_err(),
            "test env must not set PAPER_MODE_LOG"
        );
        assert!(open_paper_log().is_none());
    }
}
