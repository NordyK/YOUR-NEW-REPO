use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

// Tier A of the two-tier money-path proof: the STRICT supported-family corpus (exchange ids
// {0,6,9,23,25}, hop 2-4) curated from the live gate9 capture. These rows are executable by the
// Solidity MantisDetectorVerdict harness (realized-net). Unsupported families (V4/Curve-NG/Fluid/
// LB/Maverick/Balancer/Camelot-V2/UniV2) are Tier B — validated by the engine's in-memory REVM
// (candidate records), since the Solidity harness cannot reconstruct their decoded/derived state.
const FIXTURE: &str = "../../contracts/test/fixtures/detect_routes_strict.txt";

#[derive(Debug)]
struct Hop {
    exchange_id: u8,
    pool: String,
}

#[derive(Debug)]
struct DetectRow {
    route_id: u64,
    block_number: u64,
    amount_in: u128,
    gross_profit: u128,
    hops: Vec<Hop>,
}

fn mantis_exchange_names() -> BTreeMap<u8, &'static str> {
    BTreeMap::from([
        (0, "UniswapV3"),
        (1, "CamelotV2"),
        (2, "SushiV2"),
        (6, "PancakeV3"),
        (7, "KyberSwapElastic"),
        (9, "AlgebraV19"),
        (12, "SolidlyV3"),
        (15, "TraderJoeLb"),
        (17, "CurveCryptoSwap"),
        (18, "CurveStableSwapNg"),
        (20, "UniswapV4"),
        (21, "FluidDex"),
        (22, "MaverickV2"),
        (23, "DodoV2"),
        (24, "UniswapV2"),
        (25, "SushiV3"),
        (26, "BalancerStable"),
        (27, "BalancerWeighted"),
    ])
}

fn strict_fork_verdict_supported_ids() -> BTreeSet<u8> {
    // Mirrors the current MantisDetectorVerdict.Fork.t.sol builder domain:
    // V3-like token0/token1 pools plus DODO's base/quote API. V4 and Curve
    // detections need dedicated metadata before they can be strict profit gates.
    BTreeSet::from([0, 6, 9, 23, 25])
}

fn is_hex_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn parse_u64(field: &str, label: &str, line_no: usize) -> u64 {
    field
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {field}: {err}"))
}

fn parse_u128(field: &str, label: &str, line_no: usize) -> u128 {
    field
        .parse::<u128>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {field}: {err}"))
}

fn parse_detect_line(line_no: usize, line: &str) -> DetectRow {
    let fields = line.split(',').collect::<Vec<_>>();
    assert_eq!(
        fields.first(),
        Some(&"DETECT"),
        "line {line_no}: expected DETECT row"
    );
    assert!(
        fields.len() >= 8,
        "line {line_no}: expected at least one hop, got {} fields",
        fields.len()
    );

    let hop_count = parse_u64(fields[5], "hop_count", line_no) as usize;
    assert_eq!(
        fields.len(),
        6 + hop_count * 2,
        "line {line_no}: field count does not match hop_count"
    );

    let mut hops = Vec::with_capacity(hop_count);
    for hop_idx in 0..hop_count {
        let exchange_idx = 6 + hop_idx * 2;
        let exchange_id = fields[exchange_idx]
            .parse::<u8>()
            .unwrap_or_else(|err| panic!("line {line_no}: invalid exchange id: {err}"));
        let pool = fields[exchange_idx + 1].to_ascii_lowercase();
        assert!(
            is_hex_address(&pool),
            "line {line_no}: invalid pool address {pool}"
        );
        hops.push(Hop { exchange_id, pool });
    }

    DetectRow {
        route_id: parse_u64(fields[1], "route_id", line_no),
        block_number: parse_u64(fields[2], "block_number", line_no),
        amount_in: parse_u128(fields[3], "amount_in", line_no),
        gross_profit: parse_u128(fields[4], "gross_profit", line_no),
        hops,
    }
}

fn load_fixture() -> Vec<DetectRow> {
    let raw = fs::read_to_string(Path::new(FIXTURE)).expect("read Gate 9 DETECT fixture");
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| parse_detect_line(idx + 1, line))
        .collect()
}

#[test]
fn gate9_detector_fixture_is_well_formed_and_uses_known_mantis_ids() {
    let rows = load_fixture();
    assert!(!rows.is_empty(), "Gate 9 DETECT fixture is empty");

    let names = mantis_exchange_names();
    let mut seen_ids = BTreeSet::new();
    let mut seen_pools = BTreeSet::new();

    for row in &rows {
        assert!(row.route_id > 0, "route_id must be nonzero");
        assert!(row.block_number > 0, "block_number must be nonzero");
        assert!(row.amount_in > 0, "amount_in must be nonzero");
        assert!(row.gross_profit > 0, "gross_profit must be nonzero");
        assert!(
            !row.hops.is_empty(),
            "every DETECT row needs at least one hop"
        );

        for hop in &row.hops {
            assert!(
                names.contains_key(&hop.exchange_id),
                "unknown Mantis exchange id {}",
                hop.exchange_id
            );
            seen_ids.insert(hop.exchange_id);
            seen_pools.insert(hop.pool.clone());
        }
    }

    assert!(
        seen_ids.len() >= 5,
        "fixture should exercise several AMM families, got {seen_ids:?}"
    );
    assert!(
        seen_pools.len() >= rows.len() / 2,
        "fixture has too little pool diversity: {} pools for {} rows",
        seen_pools.len(),
        rows.len()
    );
}
