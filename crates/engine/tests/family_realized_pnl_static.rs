//! WI-6 wiring gate: the per-family realized-PnL fork test (MantisFamilyRealizedPnL.Fork.t.sol)
//! must never silently report a zero realized delta for a family. Every family is either
//! directly measured-positive OR an explicitly `opaque`-flagged exclusion that cites a dedicated,
//! independently-asserted money-proof. This static test enforces that contract WITHOUT an RPC fork:
//!   * exactly 16 families, each non-opaque (measured) or opaque-with-nonempty-proofRef;
//!   * the three known opaque-settlement families {solidly, sushiswap, fluid} are still opaque;
//!   * each opaque family's cited proof actually resolves (sibling-positive / fixture row / file);
//!   * the measured floor in the fork test is the raised value (>= 13), not the old >= 10.

use serde_json::Value;
use std::{fs, path::Path};

const SOL: &str = "../../contracts/test/MantisFamilyRealizedPnL.Fork.t.sol";
const ECON_FIXTURE: &str = "../../contracts/test/fixtures/candidate_records_economics_sample.jsonl";
const FLUID_PROOF: &str = "../../contracts/test/MantisGate5FluidV4Parent.Fork.t.sol";

#[derive(Debug)]
struct Family {
    name: String,
    opaque: bool,
    proof_ref: String,
}

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

/// Extract every double-quoted string literal in a block (our data has no escaped quotes).
fn quoted_strings(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = block.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                s.push(c);
            }
            out.push(s);
        }
    }
    out
}

/// Parse the `_arbs()` entries. Each entry is `... = Arb( ... );`; the proofRef string can contain
/// `)` (e.g. "ve(3,3)") so we delimit on the literal `);` terminator, which never appears in data.
fn parse_families(src: &str) -> Vec<Family> {
    let mut families = Vec::new();
    for chunk in src.split("= Arb(").skip(1) {
        let block = chunk
            .split_once(");")
            .unwrap_or_else(|| panic!("Arb( entry missing ); terminator"))
            .0;
        let strings = quoted_strings(block);
        assert!(
            !strings.is_empty(),
            "Arb entry has no family name string: {block}"
        );
        let name = strings[0].clone();
        let proof_ref = strings.last().cloned().unwrap_or_default();
        let opaque = block.lines().any(|line| line.trim() == "true,")
            && !block.lines().any(|line| line.trim() == "false,");
        families.push(Family {
            name,
            opaque,
            proof_ref,
        });
    }
    families
}

fn jsonl_rows(path: &str) -> Vec<Value> {
    read(path)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("valid candidate-record JSON line"))
        .collect()
}

/// Count REVM-success rows touching a given exchange id whose net_profit_wei is strictly positive.
fn net_positive_success_rows_for_exchange(rows: &[Value], exchange_id: u64) -> usize {
    rows.iter()
        .filter(|r| r.get("revm_result").and_then(Value::as_str) == Some("success"))
        .filter(|r| {
            r.get("exchange_ids")
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_u64() == Some(exchange_id)))
        })
        .filter(|r| {
            r.get("net_profit_wei")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<i128>().ok())
                .is_some_and(|n| n > 0)
        })
        .count()
}

#[test]
fn every_family_is_measured_or_a_documented_exclusion() {
    let families = parse_families(&read(SOL));
    assert_eq!(
        families.len(),
        16,
        "expected 16 families in MantisFamilyRealizedPnL, got {}",
        families.len()
    );

    for f in &families {
        if f.opaque {
            assert!(
                !f.proof_ref.trim().is_empty(),
                "opaque family `{}` must cite a dedicated money-proof (no silent zero)",
                f.name
            );
        } else {
            assert!(
                f.proof_ref.trim().is_empty(),
                "non-opaque family `{}` should not carry a proofRef (measured directly)",
                f.name
            );
        }
    }
}

#[test]
fn the_three_known_opaque_families_remain_flagged() {
    let families = parse_families(&read(SOL));
    for expected in ["solidly", "sushiswap", "fluid"] {
        let fam = families
            .iter()
            .find(|f| f.name == expected)
            .unwrap_or_else(|| panic!("family `{expected}` missing from MantisFamilyRealizedPnL"));
        assert!(
            fam.opaque,
            "family `{expected}` settles opaquely on-chain and must stay an opaque exclusion"
        );
    }
}

#[test]
fn opaque_family_proof_references_actually_resolve() {
    let families = parse_families(&read(SOL));
    let opaque = |name: &str| {
        families
            .iter()
            .find(|f| f.name == name && f.opaque)
            .unwrap_or_else(|| panic!("opaque family `{name}` not found"))
    };

    // solidly: self-contained proof — its cited ve(3,3) siblings exist as non-opaque (measured) rows.
    let solidly = opaque("solidly");
    for sib in ["chronos", "ramses", "auragi", "zyberswap"] {
        assert!(
            solidly.proof_ref.contains(sib),
            "solidly proofRef must cite ve(3,3) sibling `{sib}`"
        );
        let s = families
            .iter()
            .find(|f| f.name == sib)
            .unwrap_or_else(|| panic!("cited Solidly sibling `{sib}` missing"));
        assert!(
            !s.opaque,
            "Solidly sibling `{sib}` must be a measured (non-opaque) family to back the proof"
        );
    }

    // sushiswap: SushiV3 (exchange id 25) net-positive REVM-success rows in the gated economics fixture.
    let rows = jsonl_rows(ECON_FIXTURE);
    let sushi_rows = net_positive_success_rows_for_exchange(&rows, 25);
    assert!(
        sushi_rows >= 1,
        "sushiswap proof requires >=1 net-positive SushiV3(25) success row in {ECON_FIXTURE}, found {sushi_rows}"
    );
    assert!(
        opaque("sushiswap")
            .proof_ref
            .contains("candidate_records_economics_sample.jsonl"),
        "sushiswap proofRef must cite the economics fixture"
    );

    // fluid: the dedicated exact-prefix fork test file must exist (realized PnL AWS-capture-gated).
    assert!(
        Path::new(FLUID_PROOF).exists(),
        "fluid proof references {FLUID_PROOF}, which is missing"
    );
    assert!(
        opaque("fluid")
            .proof_ref
            .contains("MantisGate5FluidV4Parent"),
        "fluid proofRef must cite the dedicated Fluid fork test"
    );
}

#[test]
fn measured_floor_is_raised_to_thirteen() {
    let src = read(SOL);
    assert!(
        src.contains("assertGe(measured, 13"),
        "the fork test must assert the raised measured floor (>= 13), not the old >= 10"
    );
    assert!(
        src.contains("assertEq(measured + documented, arbs.length"),
        "the fork test must assert every family is accounted for (no silent zero)"
    );
    assert!(
        src.contains("solidlyClassPositive"),
        "the fork test must self-assert a measured-positive Solidly sibling"
    );
}
