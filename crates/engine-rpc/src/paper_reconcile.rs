//! Win-rate harness (part B) — offline reconciler logic.
//!
//! Given the paper-mode records the engine wrote in dry-run (one per would-submit decision), this
//! module decides, for each opportunity, whether a competitor took it on-chain and whether the
//! engine's projected submit time would have beaten that competitor's inclusion. The RPC block
//! fetch lives in the `paper_reconcile` example; everything here is PURE and unit-tested.

/// Does a competitor transaction's calldata embed any of `pools`? Heuristic pool fingerprint:
/// the 20-byte pool address appears verbatim in the tx input (routers/aggregators pass pool
/// addresses in calldata). Case-insensitive; `0x` prefixes are ignored on both sides. This is the
/// same approximate match the harness design calls out — a starting signal, not a proof of arb.
pub fn tx_touches_any_pool(tx_input_hex: &str, pools: &[String]) -> bool {
    let hay = tx_input_hex.trim_start_matches("0x").to_ascii_lowercase();
    pools.iter().any(|p| {
        let needle = p.trim_start_matches("0x").to_ascii_lowercase();
        !needle.is_empty() && hay.contains(&needle)
    })
}

/// One parsed paper-mode record (the fields the reconciler needs). Matches the JSONL shape
/// emitted by `engine::paper_mode::PaperRecord::to_json_line`. Extra fields are ignored.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PaperRecordInput {
    /// Wall-clock ns at the would-submit decision.
    pub timestamp_ns: u64,
    /// Engine route id.
    pub route_id: u32,
    /// Live L2 block number observed at detection.
    pub block_number_seen: u64,
    /// `0x`-prefixed pool addresses of the route's hops.
    pub pools: Vec<String>,
}

/// Earliest block number among `eth_getLogs` result entries (each a JSON object with a hex
/// `"blockNumber"`). `None` if empty / no parseable entry. Used to find the first block in which a
/// competitor emitted a swap log from one of the route's pools. Malformed entries are skipped.
pub fn earliest_log_block(logs: &[serde_json::Value]) -> Option<u64> {
    logs.iter()
        .filter_map(|l| {
            l.get("blockNumber")
                .and_then(|b| b.as_str())
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        })
        .min()
}

/// Parse one JSONL line into a [`PaperRecordInput`]. Returns `None` for blank/garbage lines so the
/// reader can skip them without aborting the whole reconcile.
pub fn parse_paper_record(line: &str) -> Option<PaperRecordInput> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// Outcome of reconciling one paper opportunity against on-chain reality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A competitor took a same-pool tx and the engine's projected submit would NOT have beaten it.
    Lost,
    /// A competitor took it, but the engine's projected submit beats their inclusion by the safety
    /// margin — the engine would have won the race.
    Won,
    /// No competitor touched these pools in the window — the engine would have taken it by default.
    Uncontested,
}

/// Decide a verdict. `engine_submit_ns` is the engine's projected submit time (record wall-clock +
/// estimated one-way latency to the sequencer). `competitor_inclusion_ns` is the earliest on-chain
/// competitor inclusion time (block timestamp), or `None` if uncontested. The engine only counts a
/// win if it beats the competitor by at least `safety_ns` (conservative margin for timing noise).
pub fn classify_verdict(
    engine_submit_ns: u128,
    competitor_inclusion_ns: Option<u128>,
    safety_ns: u128,
) -> Verdict {
    match competitor_inclusion_ns {
        None => Verdict::Uncontested,
        Some(competitor) => {
            if engine_submit_ns.saturating_add(safety_ns) < competitor {
                Verdict::Won
            } else {
                Verdict::Lost
            }
        }
    }
}

/// Aggregate verdict tally + capture-rate readouts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Opportunities the engine would have won against a competitor.
    pub won: u32,
    /// Opportunities a competitor would have beaten the engine on.
    pub lost: u32,
    /// Opportunities nobody contested (engine takes by default).
    pub uncontested: u32,
}

impl Stats {
    /// Tally a slice of verdicts.
    pub fn from_verdicts(verdicts: &[Verdict]) -> Self {
        let mut s = Stats::default();
        for v in verdicts {
            match v {
                Verdict::Won => s.won += 1,
                Verdict::Lost => s.lost += 1,
                Verdict::Uncontested => s.uncontested += 1,
            }
        }
        s
    }

    /// Total opportunities reconciled.
    pub fn total(&self) -> u32 {
        self.won + self.lost + self.uncontested
    }

    /// Capture rate over CONTESTED opportunities only: `won / (won + lost)`. The conservative
    /// headline number — "when someone else also wanted it, how often would we have won?". Zero
    /// when nothing was contested.
    pub fn capture_rate_contested(&self) -> f64 {
        let contested = self.won + self.lost;
        if contested == 0 {
            0.0
        } else {
            self.won as f64 / contested as f64
        }
    }

    /// Optimistic capture rate counting uncontested opportunities as wins:
    /// `(won + uncontested) / total`. Zero when there is no data.
    pub fn capture_rate_incl_uncontested(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            (self.won + self.uncontested) as f64 / total as f64
        }
    }
}

/// One "persistent uncontested net" group — the FORMAL MIRAGE TEST (FIX_PLAN_2026-07-10 W2.2 /
/// finding #9): a real dislocation is raced away in seconds; an "opportunity" on the same pool
/// set that stays uncontested across many records over a long window is a pricing artifact
/// (stale-`i` PMM class — the 07-09 corpus's 4.53-ETH DODO pair), NOT capturable profit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirageGroup {
    /// The route's pool set (sorted, lowercased) identifying the group.
    pub pools: Vec<String>,
    /// Records in the group.
    pub records: u32,
    /// Wall-clock span between the first and last record, nanoseconds.
    pub span_ns: u64,
    /// True when EVERY record in the group was `Uncontested` (nobody-took).
    pub all_uncontested: bool,
}

/// Group reconciled records by their (sorted) pool set and flag groups that satisfy the mirage
/// criterion: at least `min_records` records, spanning at least `min_span_ns`, ALL uncontested.
/// Contested groups (anyone took even one) are returned with `all_uncontested = false` so the
/// caller can print the full table; only `all_uncontested` groups are mirage-suspect.
pub fn mirage_groups(
    reconciled: &[(PaperRecordInput, Verdict)],
    min_records: u32,
    min_span_ns: u64,
) -> Vec<MirageGroup> {
    use std::collections::BTreeMap;
    let mut by_pools: BTreeMap<Vec<String>, Vec<&(PaperRecordInput, Verdict)>> = BTreeMap::new();
    for rv in reconciled {
        let mut key: Vec<String> = rv
            .0
            .pools
            .iter()
            .map(|p| p.trim_start_matches("0x").to_ascii_lowercase())
            .collect();
        key.sort();
        by_pools.entry(key).or_default().push(rv);
    }
    let mut out = Vec::new();
    for (pools, rows) in by_pools {
        let n = rows.len() as u32;
        let first = rows.iter().map(|r| r.0.timestamp_ns).min().unwrap_or(0);
        let last = rows.iter().map(|r| r.0.timestamp_ns).max().unwrap_or(0);
        let span_ns = last.saturating_sub(first);
        let all_uncontested = rows.iter().all(|r| r.1 == Verdict::Uncontested);
        if n >= min_records && span_ns >= min_span_ns {
            out.push(MirageGroup {
                pools,
                records: n,
                span_ns,
                all_uncontested,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts_ns: u64, pools: &[&str]) -> PaperRecordInput {
        PaperRecordInput {
            timestamp_ns: ts_ns,
            route_id: 1,
            block_number_seen: 1,
            pools: pools.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The formal DODO test: many records on one pool set, long span, nobody-took → mirage;
    /// the same set with even one contested record is NOT flagged; short/sparse groups skipped.
    #[test]
    fn mirage_groups_flag_persistent_uncontested_only() {
        const MIN: u64 = 600_000_000_000; // 10 min
        let mirage_pools = [
            "0x80c735c5a0222241f211b3edb8df2ccefad94553",
            "0x0d32a3bb0737b9ba62b0f9c5b7b1b9d3e0a2c111",
        ];
        let live_pools = ["0xc6962004f452be9203591991d15f6b388e09e8d0"];
        let mut reconciled: Vec<(PaperRecordInput, Verdict)> = Vec::new();
        // Mirage group: 4 uncontested records over 30 min (order shuffled, mixed 0x-casing).
        for (i, ts) in [0u64, 1_800_000_000_000, 600_000_000_000, 1_200_000_000_000]
            .into_iter()
            .enumerate()
        {
            let pools: Vec<&str> = if i % 2 == 0 {
                mirage_pools.to_vec()
            } else {
                vec![mirage_pools[1], mirage_pools[0]] // rotated order must group identically
            };
            reconciled.push((rec(ts, &pools), Verdict::Uncontested));
        }
        // Live group: 3 records over 30 min, one WON (contested) → not a mirage.
        reconciled.push((rec(0, &live_pools), Verdict::Uncontested));
        reconciled.push((rec(900_000_000_000, &live_pools), Verdict::Won));
        reconciled.push((rec(1_800_000_000_000, &live_pools), Verdict::Uncontested));
        // Sparse group: 2 records — below min_records, skipped entirely.
        reconciled.push((rec(0, &["0xaaaa"]), Verdict::Uncontested));
        reconciled.push((rec(1_800_000_000_000, &["0xaaaa"]), Verdict::Uncontested));

        let groups = mirage_groups(&reconciled, 3, MIN);
        assert_eq!(groups.len(), 2, "mirage + live groups qualify; sparse skipped");
        let mirage = groups
            .iter()
            .find(|g| g.pools.iter().any(|p| p.starts_with("80c735c5")))
            .expect("mirage group present");
        assert!(mirage.all_uncontested, "nobody-took with persistent net");
        assert_eq!(mirage.records, 4);
        assert!(mirage.span_ns >= MIN);
        let live = groups
            .iter()
            .find(|g| g.pools.iter().any(|p| p.starts_with("c6962004")))
            .expect("live group present");
        assert!(!live.all_uncontested, "a contested group is NOT a mirage");
    }

    #[test]
    fn tx_touches_any_pool_matches_case_insensitively_with_or_without_0x() {
        let pools = vec![
            "0xC6962004f452bE9203591991D15f6b388e09E8D0".to_string(), // mixed case + 0x
            "0x1666ea6e83bcc3473111732ef086ba47e6eddeb7".to_string(),
        ];
        // A competitor calldata embedding the (lowercased) second pool address.
        let input =
            "0xabcdef0000000000000000000000001666ea6e83bcc3473111732ef086ba47e6eddeb7deadbeef";
        assert!(tx_touches_any_pool(input, &pools));

        // Calldata embedding the FIRST pool, lowercased — must still match the mixed-case entry.
        let input2 = "0x000000000000000000000000c6962004f452be9203591991d15f6b388e09e8d0";
        assert!(tx_touches_any_pool(input2, &pools));

        // Unrelated calldata — no match.
        let input3 = "0x0000000000000000000000009999999999999999999999999999999999999999";
        assert!(!tx_touches_any_pool(input3, &pools));
    }

    const SAFETY_NS: u128 = 100_000_000; // 100 ms margin (matches harness design)

    #[test]
    fn classify_verdict_uncontested_when_no_competitor() {
        assert_eq!(
            classify_verdict(1_000, None, SAFETY_NS),
            Verdict::Uncontested
        );
    }

    #[test]
    fn classify_verdict_won_when_engine_beats_by_safety_margin() {
        // engine at t=1.000s, competitor included at t=2.000s → engine wins by >100ms.
        let engine = 1_000_000_000u128;
        let competitor = 2_000_000_000u128;
        assert_eq!(
            classify_verdict(engine, Some(competitor), SAFETY_NS),
            Verdict::Won
        );
    }

    #[test]
    fn classify_verdict_lost_when_engine_later() {
        let engine = 2_000_000_000u128;
        let competitor = 1_000_000_000u128;
        assert_eq!(
            classify_verdict(engine, Some(competitor), SAFETY_NS),
            Verdict::Lost
        );
    }

    #[test]
    fn classify_verdict_lost_when_inside_safety_margin() {
        // engine earlier by only 50ms (< 100ms safety) → conservatively a loss.
        let competitor = 1_000_000_000u128;
        let engine = competitor - 50_000_000;
        assert_eq!(
            classify_verdict(engine, Some(competitor), SAFETY_NS),
            Verdict::Lost
        );
    }

    #[test]
    fn stats_aggregate_capture_rates() {
        let verdicts = [
            Verdict::Won,
            Verdict::Won,
            Verdict::Lost,
            Verdict::Uncontested,
            Verdict::Uncontested,
            Verdict::Uncontested,
        ];
        let s = Stats::from_verdicts(&verdicts);
        assert_eq!((s.won, s.lost, s.uncontested), (2, 1, 3));
        assert_eq!(s.total(), 6);
        // contested capture rate = won / (won+lost) = 2/3.
        assert!((s.capture_rate_contested() - 2.0 / 3.0).abs() < 1e-9);
        // optimistic = (won + uncontested) / total = 5/6.
        assert!((s.capture_rate_incl_uncontested() - 5.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn stats_capture_rates_are_zero_with_no_data() {
        let s = Stats::from_verdicts(&[]);
        assert_eq!(s.total(), 0);
        assert_eq!(s.capture_rate_contested(), 0.0);
        assert_eq!(s.capture_rate_incl_uncontested(), 0.0);
    }

    #[test]
    fn earliest_log_block_picks_min_block_number() {
        let logs = vec![
            serde_json::json!({"blockNumber": "0x1bbf449", "address": "0xabc"}), // 29095497
            serde_json::json!({"blockNumber": "0x1bbf448", "address": "0xdef"}), // 29095496 (earliest)
            serde_json::json!({"blockNumber": "0x1bbf44a", "address": "0xabc"}),
        ];
        assert_eq!(earliest_log_block(&logs), Some(0x1bbf448));
        assert_eq!(earliest_log_block(&[]), None);
        // A malformed entry is skipped, not fatal.
        let messy = vec![
            serde_json::json!({"nope": 1}),
            serde_json::json!({"blockNumber": "0x5"}),
        ];
        assert_eq!(earliest_log_block(&messy), Some(5));
    }

    #[test]
    fn parse_paper_record_reads_engine_emitted_line() {
        // Exactly the shape engine::paper_mode::PaperRecord::to_json_line emits.
        let line = r#"{"timestamp_ns":1780000000000000000,"route_id":362,"block_number_seen":465686729,"amount_in":"11718742621174166240","gross_profit_wei":"2837294824172502","net_profit_wei":null,"pools":["0x1666ea6e83bcc3473111732ef086ba47e6eddeb7","0xc6962004f452be9203591991d15f6b388e09e8d0"]}"#;
        let rec = parse_paper_record(line).expect("parse");
        assert_eq!(rec.timestamp_ns, 1_780_000_000_000_000_000);
        assert_eq!(rec.route_id, 362);
        assert_eq!(rec.block_number_seen, 465_686_729);
        assert_eq!(rec.pools.len(), 2);
        assert_eq!(rec.pools[1], "0xc6962004f452be9203591991d15f6b388e09e8d0");

        // Blank lines / garbage return None (so the reader can skip them).
        assert!(parse_paper_record("").is_none());
        assert!(parse_paper_record("not json").is_none());
    }
}
