//! W1.1 (FIX_PLAN_2026-07-10) — STATE-MATCHED PARITY MINER over a recorded candidates.jsonl
//! corpus: per recorded hop, at the validator pin block that validation ACTUALLY ran against,
//! compare the engine's integer AMM kernel against the chain's own pricing view (the family
//! Quoter / the pool's own query fn) on the SAME state and the SAME input.
//!
//! WHY: fl_disagree conflates "kernel computes wrong" with "state got stale between decision and
//! validation" (state_point_mode=LiveTipCurrent on all 14,772 records of the 2026-07-09 soak).
//! This harness removes staleness from the comparison entirely: both sides read block-pinned
//! archive state, so any surviving mismatch is a KERNEL defect — the promotion gate P2.1 needs
//! parity-mismatch = 0 per family before that family's fast-lane flip.
//!
//! GROUND TRUTH is the chain's quoter, NOT the run's recorded REVM verdicts: the 07-09 soak ran
//! with the state mirror enabled and its Gate-C audit measured 79% slot divergence before the
//! fail-safe demoted it — recorded verdicts are potentially mirror-poisoned, quoter calls at our
//! pinned block are not. Recorded `balance_delta_wei` successes are reported as a SECONDARY
//! cross-check distribution only.
//!
//! PIN RECOVERY: recorded snapshot_block/block_env were BOOT-frozen (OnceLock bug, fixed in
//! W1.1a) — the true pin timeline is reconstructed from engine.log repin/boot events and each
//! record is assigned the latest pin at its recv_ns, excluding records within a boundary margin
//! of a repin (ring-crossing ambiguity).
//!
//! Usage (archive RPC required; free endpoints work — drpc/1rpc/official all serve day-old state):
//!   ARBITRUM_RPC_URL=https://arbitrum.drpc.org \
//!   cargo run -p engine --release --example state_matched_parity -- \
//!     --candidates aws_dryrun_20260709/logs/candidates.jsonl \
//!     --engine-log aws_dryrun_20260709/logs/engine.log \
//!     [--max-per-family 220] [--sleep-ms 25] [--report parity_report.json] [--family all] \
//!     [--require-families uniswap_v3,pancake_v3] \
//!     [--generate-targeted-corpus targeted_parity_corpus.jsonl]
//!
//! Targeted generation selects real pools and a real recovered pin, emits six mechanically chosen
//! inputs in both directions for three pools per requested family, and only preflights the
//! authoritative reference transport. The generated corpus must then be consumed by a separate
//! strict run; generation never filters rows by comparison outcome.
//!
//! Exit code 0 iff every explicitly required family has strict zero-wei agreement, no unresolved
//! errors, and promotion-grade breadth (3 pools, 6 inputs per pool in both directions, 36
//! comparisons, and every topology-specific edge case). Without `--require-families`, a singular
//! `--family` is required exactly; `--family all` requires every family this harness supports. A
//! one-wei difference is a disagreement.

use alloy_primitives::{keccak256, U256};
use engine_amm::{TickInfo, V3Slot0};
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use std::collections::{HashMap, HashSet};
use std::io::BufRead;

// ───────────────────────────── verified quoter deployments (Arbitrum One) ──────────────────────
// Uniswap V3 QuoterV2 — docs.uniswap.org/contracts/v3/reference/deployments/arbitrum-deployments
const UNI_V3_QUOTER_V2: &str = "61fFE014bA17989E743c5F6cB21bF9697530B21e";
// PancakeSwap V3 QuoterV2 — developer.pancakeswap.finance/contracts/v3/addresses
const PANCAKE_QUOTER_V2: &str = "B048Bbc1Ee6b733FFfCFb9e9CeF7375518e25997";
// Camelot (Algebra V1.9) Quoter — docs.camelot.exchange/contracts/arbitrum/one-mainnet
const ALGEBRA_QUOTER: &str = "0Fc73040b26E9bC8514fA028D998E73A254Fa76E";
// Historical V4 quoter used by the pinned corpus; the strict report below, not this address/comment,
// decides byte-exact eligibility.
const V4_QUOTER: &str = "3972c00f7ed4885e145823eb7c655375d275a1c5";
// Multicall3 (canonical, all chains)
const MULTICALL3: &str = "cA11bde05977b3631167028862bE2a173976CA11";
const ARBITRUM_ONE_CHAIN_ID: u64 = 42_161;

const MIN_POOLS: usize = 3;
const MIN_DIRECTIONAL_INPUTS: usize = 36;
const MIN_INPUTS_PER_POOL_DIRECTION: usize = 6;
const MIN_COMPARISONS: u64 = 36;

const BASE_EDGE_CASES: &[&str] = &["dust", "nominal", "extreme", "near_empty_reserve"];
const CL_EDGE_CASES: &[&str] = &[
    "dust",
    "nominal",
    "extreme",
    "near_tick_boundary",
    "multi_tick_crossing",
];
const LB_EDGE_CASES: &[&str] = &[
    "dust",
    "nominal",
    "extreme",
    "near_bin_boundary",
    "multi_bin_crossing",
];

#[derive(Clone, Copy)]
struct FamilySpec {
    /// Canonical `fastlane_policy::family_token` and JSON object key.
    token: &'static str,
    /// `exchange_metadata[].family` spelling in the recorded candidate corpus.
    corpus_family: &'static str,
    authoritative_reference: &'static str,
    reference_kind: &'static str,
    current_status: &'static str,
    status_note: &'static str,
    policy_default: bool,
    /// True only where this example currently has both a kernel snapshot and a chain reference.
    harness_supported: bool,
    required_edge_cases: &'static [&'static str],
}

// Keep this in PoolKind discriminant/family_token order. Every row is emitted even when this
// example has no implementation or the corpus has no data; absent evidence must remain visible.
const FAMILY_SPECS: [FamilySpec; 18] = [
    FamilySpec {
        token: "uniswap_v2",
        corpus_family: "UniswapV2",
        authoritative_reference: "REVM execution of the deployed Uniswap V2 pair swap at the recovered pin (not wired in this example)",
        reference_kind: "revm_swap_execution",
        current_status: "tier2",
        status_note: "policy-default math family, currently demoted by the AWS parity intersection",
        policy_default: true,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "sushi_v2",
        corpus_family: "SushiV2",
        authoritative_reference: "REVM execution of the deployed Sushi V2 pair swap at the recovered pin (not wired in this example)",
        reference_kind: "revm_swap_execution",
        current_status: "tier2",
        status_note: "policy-default math family, currently demoted by the AWS parity intersection",
        policy_default: true,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "camelot_v2",
        corpus_family: "CamelotV2",
        authoritative_reference: "REVM execution of the deployed Camelot V2 pair swap at the recovered pin (not wired in this example)",
        reference_kind: "revm_swap_execution",
        current_status: "tier2",
        status_note: "policy-default math family, currently demoted by the AWS parity intersection",
        policy_default: true,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "uniswap_v3",
        corpus_family: "UniswapV3",
        authoritative_reference: "official Uniswap V3 QuoterV2 on Arbitrum at the recovered pin",
        reference_kind: "official_onchain_quoter",
        current_status: "promoted",
        status_note: "present in the current AWS FASTLANE_PARITY_PROVEN_FAMILIES intersection",
        policy_default: true,
        harness_supported: true,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "sushi_v3",
        corpus_family: "SushiV3",
        authoritative_reference: "official Sushi V3 on-chain quoter at the recovered pin (not wired in this example)",
        reference_kind: "official_onchain_quoter",
        current_status: "tier2",
        status_note: "policy-default math family, currently demoted by the AWS parity intersection",
        policy_default: true,
        harness_supported: false,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "pancake_v3",
        corpus_family: "PancakeV3",
        authoritative_reference: "official PancakeSwap V3 QuoterV2 on Arbitrum at the recovered pin",
        reference_kind: "official_onchain_quoter",
        current_status: "promoted",
        status_note: "present in the current AWS FASTLANE_PARITY_PROVEN_FAMILIES intersection",
        policy_default: true,
        harness_supported: true,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "algebra_v19",
        corpus_family: "AlgebraV19",
        authoritative_reference: "official Camelot Algebra V1.9 Quoter on Arbitrum at the recovered pin",
        reference_kind: "official_onchain_quoter",
        current_status: "tier2",
        status_note: "strict pinned evidence found two one-wei disagreements caused by Algebra raw 256-tick row segmentation; keep REVM-only",
        policy_default: true,
        harness_supported: true,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "curve_stableswap",
        corpus_family: "CurveStableSwapNg",
        authoritative_reference: "deployed Curve StableSwap/StableSwap-NG get_dy view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "live Tier-2 family; REVM remains authoritative",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "curve_cryptoswap",
        corpus_family: "CurveCryptoSwap",
        authoritative_reference: "deployed Curve CryptoSwap/Tricrypto get_dy view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "live Tier-2 family; known approximation requires REVM",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "trader_joe_lb",
        corpus_family: "TraderJoeLb",
        authoritative_reference: "deployed Trader Joe LB pair getSwapOut view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "live Tier-2 family; strict real-pool parity is still required",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: LB_EDGE_CASES,
    },
    FamilySpec {
        token: "maverick_v2",
        corpus_family: "MaverickV2",
        authoritative_reference: "official deployed Maverick V2 quoter at the recovered pin (not wired in this example)",
        reference_kind: "official_onchain_quoter",
        current_status: "unsupported",
        status_note: "actual-live gate has zero certified vectors",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "fluid",
        corpus_family: "FluidDex",
        authoritative_reference: "deployed Fluid resolver/pool quote view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "Fluid v1 is live Tier-2; Fluid v2 remains paused",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "dodo",
        corpus_family: "DodoV2",
        authoritative_reference: "deployed DODO V2 pool querySellBase/querySellQuote view at the recovered pin",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "live Tier-2 family; strict parity has not yet authorized promotion",
        policy_default: false,
        harness_supported: true,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "kyber_elastic",
        corpus_family: "KyberSwapElastic",
        authoritative_reference: "official KyberSwap Elastic quoter or REVM swap execution at the recovered pin (not wired in this example)",
        reference_kind: "revm_swap_execution",
        current_status: "unsupported",
        status_note: "startup drops Kyber pools from the actual-live surface",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "solidly_v3",
        corpus_family: "SolidlyV3",
        authoritative_reference: "deployed Solidly V3 pool quote view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "unsupported",
        status_note: "no executor exchange id; family is fail-closed",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "uniswap_v4",
        corpus_family: "UniswapV4",
        authoritative_reference: "official Uniswap V4 Quoter on Arbitrum at the recovered pin, hookless PoolKey self-checked",
        reference_kind: "official_onchain_quoter",
        current_status: "promoted",
        status_note: "present in the current AWS FASTLANE_PARITY_PROVEN_FAMILIES intersection (hookless only)",
        policy_default: true,
        harness_supported: true,
        required_edge_cases: CL_EDGE_CASES,
    },
    FamilySpec {
        token: "balancer_stable",
        corpus_family: "BalancerStable",
        authoritative_reference: "deployed Balancer Vault queryBatchSwap/querySwap view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "live Tier-2 family; REVM remains authoritative",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
    FamilySpec {
        token: "balancer_weighted",
        corpus_family: "BalancerWeighted",
        authoritative_reference: "deployed Balancer Vault queryBatchSwap/querySwap view at the recovered pin (not wired in this example)",
        reference_kind: "pool_quote_view",
        current_status: "tier2",
        status_note: "live Tier-2 family; known approximation requires REVM",
        policy_default: false,
        harness_supported: false,
        required_edge_cases: BASE_EDGE_CASES,
    },
];

fn family_spec_by_token(token: &str) -> Option<&'static FamilySpec> {
    FAMILY_SPECS.iter().find(|spec| spec.token == token)
}

fn family_spec_for_corpus(family: &str) -> Option<&'static FamilySpec> {
    FAMILY_SPECS
        .iter()
        .find(|spec| spec.corpus_family == family || spec.token == family)
}

fn required_variants(token: &str) -> &'static [&'static str] {
    match token {
        "curve_stableswap" => &["plain_ng"],
        "curve_cryptoswap" => &["twocrypto", "tricrypto"],
        "trader_joe_lb" => &["lb_v21", "lb_v22"],
        "fluid" => &["v1", "v2"],
        "dodo" => &["dvm", "dpp", "dsp"],
        "uniswap_v4" => &["hookless_erc20"],
        "balancer_stable" => &[
            "stable",
            "composable_stable",
            "meta_stable",
            "rate_provider",
        ],
        "balancer_weighted" => &["equal_weight", "unequal_weight", "canonical_codehash"],
        _ => &[],
    }
}

fn addr20(s: &str) -> [u8; 20] {
    let s = s.trim().trim_start_matches("0x");
    let mut a = [0u8; 20];
    for (k, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * k..2 * k + 2], 16).expect("hex addr");
    }
    a
}

fn sel(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

fn word_u256(x: U256) -> [u8; 32] {
    x.to_be_bytes::<32>()
}

fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

fn decode_word0(resp: &[u8]) -> Option<U256> {
    (resp.len() >= 32).then(|| U256::from_be_slice(&resp[..32]))
}

fn dodo_class_from_version(resp: &[u8]) -> Option<engine_amm::dodo_v2::DodoPoolClass> {
    let offset: usize = U256::from_be_slice(resp.get(..32)?).try_into().ok()?;
    let data_start = offset.checked_add(32)?;
    let len: usize = U256::from_be_slice(resp.get(offset..data_start)?)
        .try_into()
        .ok()?;
    let data_end = data_start.checked_add(len)?;
    let version = std::str::from_utf8(resp.get(data_start..data_end)?)
        .ok()?
        .to_ascii_uppercase();
    if version.contains("DVM") {
        Some(engine_amm::dodo_v2::DodoPoolClass::Dvm)
    } else if version.contains("DPP") {
        Some(engine_amm::dodo_v2::DodoPoolClass::Dpp)
    } else if version.contains("DSP") {
        Some(engine_amm::dodo_v2::DodoPoolClass::Dsp)
    } else {
        None
    }
}

// ───────────────────────────── RFC3339 → epoch ns (UTC, no external deps) ──────────────────────
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn rfc3339_to_epoch_ns(ts: &str) -> Option<u64> {
    // "2026-07-09T15:06:41.928511Z"
    let b = ts.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let y: i64 = ts.get(0..4)?.parse().ok()?;
    let mo: u32 = ts.get(5..7)?.parse().ok()?;
    let d: u32 = ts.get(8..10)?.parse().ok()?;
    let h: u64 = ts.get(11..13)?.parse().ok()?;
    let mi: u64 = ts.get(14..16)?.parse().ok()?;
    let s: u64 = ts.get(17..19)?.parse().ok()?;
    let mut frac_ns: u64 = 0;
    if let Some(rest) = ts.get(19..) {
        if let Some(stripped) = rest.strip_prefix('.') {
            let digits: String = stripped
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                let mut v: u64 = digits.parse().ok()?;
                for _ in digits.len()..9 {
                    v *= 10;
                }
                frac_ns = v;
            }
        }
    }
    let days = days_from_civil(y, mo, d);
    if days < 0 {
        return None;
    }
    Some((days as u64 * 86_400 + h * 3_600 + mi * 60 + s) * 1_000_000_000 + frac_ns)
}

// ───────────────────────────── pin timeline from engine.log ────────────────────────────────────
/// (wall_ns, pin_block) events: boot "REVM route validation ACTIVE" + every LIGHT/idle repin.
fn load_pin_timeline(path: &str) -> Vec<(u64, u64)> {
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut out: Vec<(u64, u64)> = Vec::new();
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v["schema"] == "mantis_pin_timeline_v1" {
            if let (Some(timestamp_ns), Some(block_number)) =
                (v["timestamp_ns"].as_u64(), v["block_number"].as_u64())
            {
                out.push((timestamp_ns, block_number));
            }
            continue;
        }
        if !(line.contains("repin") || line.contains("REVM route validation ACTIVE")) {
            continue;
        }
        let Some(ts) = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(rfc3339_to_epoch_ns)
        else {
            continue;
        };
        let fields = v.get("fields").unwrap_or(&serde_json::Value::Null);
        let msg = fields.get("message").and_then(|m| m.as_str()).unwrap_or("");
        // Repin events carry the numeric new_block directly.
        if msg.contains("repin") {
            if let Some(nb) = fields.get("new_block").and_then(|b| b.as_u64()) {
                out.push((ts, nb));
            }
            continue;
        }
        // Boot: "revm_loop: REVM route validation ACTIVE …" logs block = Number(4820…) (Debug).
        if msg.contains("REVM route validation ACTIVE") {
            if let Some(block_dbg) = fields.get("block").and_then(|b| b.as_str()) {
                if let Some(num) = block_dbg
                    .trim_start_matches("Number(")
                    .trim_end_matches(')')
                    .parse::<u64>()
                    .ok()
                {
                    out.push((ts, num));
                }
            }
        }
    }
    out.sort_unstable();
    out
}

fn write_portable_pin_timeline(
    path: &std::path::Path,
    timeline: &[(u64, u64)],
) -> Result<(), String> {
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(path)
            .map_err(|error| format!("create pin timeline {}: {error}", path.display()))?,
    );
    use std::io::Write as _;
    for (timestamp_ns, block_number) in timeline {
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({
                "schema": "mantis_pin_timeline_v1",
                "timestamp_ns": timestamp_ns,
                "block_number": block_number,
            }),
        )
        .map_err(|error| format!("serialize pin timeline row: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("write pin timeline row: {error}"))?;
    }
    file.flush()
        .map_err(|error| format!("flush pin timeline {}: {error}", path.display()))
}

fn write_jsonl(path: &std::path::Path, rows: &[serde_json::Value]) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create JSONL directory {}: {error}", parent.display()))?;
    }
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(path)
            .map_err(|error| format!("create JSONL {}: {error}", path.display()))?,
    );
    use std::io::Write as _;
    for row in rows {
        serde_json::to_writer(&mut file, row)
            .map_err(|error| format!("serialize JSONL row: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("write JSONL row: {error}"))?;
    }
    file.flush()
        .map_err(|error| format!("flush JSONL {}: {error}", path.display()))
}

/// Latest pin at `recv_ns`, excluding records within `margin_ns` of a boundary (either side):
/// validation runs shortly AFTER recv, so near a repin the assignment is ambiguous.
fn assign_pin(timeline: &[(u64, u64)], recv_ns: u64, margin_ns: u64) -> Option<u64> {
    let idx = timeline.partition_point(|(t, _)| *t <= recv_ns);
    if idx == 0 {
        return None; // before the first known pin
    }
    let (t_cur, pin) = timeline[idx - 1];
    if recv_ns.saturating_sub(t_cur) < margin_ns {
        return None; // just after a pin advance — validation may have used either side
    }
    if let Some((t_next, _)) = timeline.get(idx) {
        if t_next.saturating_sub(recv_ns) < margin_ns {
            return None; // validation may have crossed into the next pin
        }
    }
    Some(pin)
}

// ───────────────────────────── corpus records ──────────────────────────────────────────────────
#[derive(Clone)]
struct HopMeta {
    family: String,
    pool: [u8; 20],
    token_in: [u8; 20],
    token_out: [u8; 20],
    fee: u32,
    tick_spacing: i32,
    pool_id: Option<[u8; 32]>,
    state_view: Option<[u8; 20]>,
    /// Labels from a deterministic, deliberately targeted parity corpus. Automatically provable
    /// topology edges are added by `kernel_quote`; generic size classes must be corpus-authored.
    parity_edge_cases: Vec<String>,
}

fn pool_identity(hop: &HopMeta) -> [u8; 32] {
    if hop.family == "UniswapV4" {
        if let Some(pool_id) = hop.pool_id {
            return pool_id;
        }
    }
    let mut identity = [0u8; 32];
    identity[12..].copy_from_slice(&hop.pool);
    identity
}

fn parity_dedup_key(pin: u64, hop: &HopMeta, amount: U256) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::default();
    (pin, pool_identity(hop), hop.token_in, hop.token_out, amount).hash(&mut hasher);
    hasher.finish()
}

struct Record {
    candidate_id: String,
    recv_ns: u64,
    input_amount: U256,
    hops: Vec<HopMeta>,
    balance_delta_wei: Option<i128>,
    revm_result: String,
    /// (address, slot, original) from the record's changed_slots — the state-match anchor.
    changed_slots: Vec<([u8; 20], [u8; 32], [u8; 32])>,
    /// The in-fork executor address — its own storage is a fork fiction, excluded from anchoring.
    mantis_executor: Option<[u8; 20]>,
}

fn load_records(path: &str) -> Vec<Record> {
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut out = Vec::new();
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let record_edge_cases: Vec<String> = v["parity_edge_cases"]
            .as_array()
            .map(|edges| {
                edges
                    .iter()
                    .filter_map(|edge| edge.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let hops: Vec<HopMeta> = v["exchange_metadata"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let mut parity_edge_cases = record_edge_cases.clone();
                        if let Some(edges) = m["parity_edge_cases"].as_array() {
                            parity_edge_cases.extend(
                                edges
                                    .iter()
                                    .filter_map(|edge| edge.as_str().map(str::to_owned)),
                            );
                        }
                        parity_edge_cases.sort();
                        parity_edge_cases.dedup();
                        Some(HopMeta {
                            family: m["family"].as_str()?.to_string(),
                            pool: addr20(m["pool"].as_str()?),
                            token_in: addr20(m["token_in"].as_str()?),
                            token_out: addr20(m["token_out"].as_str()?),
                            fee: m["fee_pips_or_bps"].as_u64().unwrap_or(0) as u32,
                            tick_spacing: m["tick_spacing"].as_i64().unwrap_or(0) as i32,
                            pool_id: m["pool_id"].as_str().map(|s| {
                                let s = s.trim_start_matches("0x");
                                let mut id = [0u8; 32];
                                for (k, b) in id.iter_mut().enumerate() {
                                    *b = u8::from_str_radix(&s[2 * k..2 * k + 2], 16).unwrap_or(0);
                                }
                                id
                            }),
                            state_view: m["state_view"].as_str().map(addr20),
                            parity_edge_cases,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if hops.is_empty() {
            continue;
        }
        let Some(input) = v["input_amount"]
            .as_str()
            .and_then(|s| U256::from_str_radix(s, 10).ok())
        else {
            continue;
        };
        let hex32 = |s: &str| -> [u8; 32] {
            let h = s.trim_start_matches("0x");
            let mut b = [0u8; 32];
            let padded = format!("{h:0>64}");
            for (k, byte) in b.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&padded[2 * k..2 * k + 2], 16).unwrap_or(0);
            }
            b
        };
        let changed_slots: Vec<([u8; 20], [u8; 32], [u8; 32])> = v["changed_slots"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|cs| {
                        Some((
                            addr20(cs["address"].as_str()?),
                            hex32(cs["slot"].as_str()?),
                            hex32(cs["original"].as_str()?),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(Record {
            candidate_id: v["candidate_id"].as_str().unwrap_or("?").to_string(),
            recv_ns: v["recv_ns"].as_u64().unwrap_or(0),
            input_amount: input,
            hops,
            balance_delta_wei: v["balance_delta_wei"]
                .as_str()
                .and_then(|s| s.parse::<i128>().ok()),
            revm_result: v["revm_result"].as_str().unwrap_or("?").to_string(),
            changed_slots,
            mantis_executor: v["mantis_executor"].as_str().map(addr20),
        });
    }
    out
}

// ─────────────── throttled RPC: transient-retry + endpoint rotation (free-tier reality) ────────
struct Throttled {
    clients: Vec<ArbitrumRpcClient>,
    cur: std::cell::Cell<usize>,
    sleep: std::time::Duration,
    errors_shown: std::cell::Cell<u32>,
}

impl Throttled {
    fn new(urls: &str, sleep: std::time::Duration) -> Self {
        let clients: Vec<ArbitrumRpcClient> = urls
            .split(',')
            .map(|u| ArbitrumRpcClient::new(u.trim().to_string()))
            .collect();
        assert!(!clients.is_empty(), "no RPC url");
        Self {
            clients,
            cur: std::cell::Cell::new(0),
            sleep,
            errors_shown: std::cell::Cell::new(0),
        }
    }

    fn client(&self) -> &ArbitrumRpcClient {
        &self.clients[self.cur.get() % self.clients.len()]
    }

    fn rotate(&self) {
        self.cur.set(self.cur.get() + 1);
    }

    fn verify_chain_id_on_every_endpoint(&self, expected: u64) -> Result<u64, String> {
        for (index, client) in self.clients.iter().enumerate() {
            let mut last_error = String::new();
            let mut verified = false;
            for attempt in 0..3u64 {
                std::thread::sleep(self.sleep);
                match client.chain_id() {
                    Ok(actual) if actual == expected => {
                        verified = true;
                        break;
                    }
                    Ok(actual) => {
                        return Err(format!(
                            "RPC endpoint index {index} serves chain_id {actual}, expected {expected}"
                        ));
                    }
                    Err(error) => {
                        last_error = format!("{error:?}");
                        std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
                    }
                }
            }
            if !verified {
                return Err(format!(
                    "RPC endpoint index {index} chain-id verification failed: {last_error}"
                ));
            }
        }
        Ok(expected)
    }

    /// Retry transient provider failures with rotation/backoff; a REVERT is a real answer and
    /// returns immediately (the caller counts it, no retry burn).
    fn eth_call(&self, to: &[u8; 20], data: &[u8], block: BlockTag) -> Result<Vec<u8>, String> {
        let mut last = String::new();
        for attempt in 0..6u64 {
            std::thread::sleep(self.sleep);
            match self.client().eth_call(to, data, block) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let es = format!("{e:?}");
                    if es.to_lowercase().contains("revert") {
                        return Err(es); // real EVM answer — do not retry
                    }
                    if self.errors_shown.get() < 5 {
                        self.errors_shown.set(self.errors_shown.get() + 1);
                        eprintln!("  rpc error (attempt {attempt}, rotating): {es}");
                    }
                    last = es;
                    self.rotate();
                    std::thread::sleep(std::time::Duration::from_millis(400 * (attempt + 1)));
                }
            }
        }
        Err(last)
    }
}

/// Route any engine-rpc fetcher through the same transient-retry + rotation policy.
fn with_retry<T>(
    th: &Throttled,
    mut f: impl FnMut(&ArbitrumRpcClient) -> Result<T, engine_rpc::RpcError>,
) -> Result<T, String> {
    let mut last = String::new();
    for attempt in 0..6u64 {
        std::thread::sleep(th.sleep);
        match f(th.client()) {
            Ok(v) => return Ok(v),
            Err(e) => {
                let es = format!("{e:?}");
                if es.to_lowercase().contains("revert") {
                    return Err(es);
                }
                if th.errors_shown.get() < 5 {
                    th.errors_shown.set(th.errors_shown.get() + 1);
                    eprintln!("  rpc error (attempt {attempt}, rotating): {es}");
                }
                last = es;
                th.rotate();
                std::thread::sleep(std::time::Duration::from_millis(400 * (attempt + 1)));
            }
        }
    }
    Err(last)
}

fn pinned_block_hash(th: &Throttled, block_number: u64) -> Result<String, String> {
    let block = with_retry(th, |client| {
        client.call(
            "eth_getBlockByNumber",
            serde_json::json!([format!("0x{block_number:x}"), false]),
        )
    })?;
    let hash = block["hash"]
        .as_str()
        .ok_or_else(|| format!("block {block_number} response omitted hash"))?
        .to_ascii_lowercase();
    if hash.len() != 66
        || !hash.starts_with("0x")
        || !hash[2..].as_bytes().iter().all(u8::is_ascii_hexdigit)
    {
        return Err(format!(
            "block {block_number} returned malformed hash {hash}"
        ));
    }
    Ok(hash)
}

fn static_authoritative_address(family: &str) -> Option<[u8; 20]> {
    match family {
        "uniswap_v3" => Some(addr20(UNI_V3_QUOTER_V2)),
        "pancake_v3" => Some(addr20(PANCAKE_QUOTER_V2)),
        "algebra_v19" => Some(addr20(ALGEBRA_QUOTER)),
        "uniswap_v4" => Some(addr20(V4_QUOTER)),
        _ => None,
    }
}

fn authoritative_address(hop: &HopMeta) -> Option<[u8; 20]> {
    family_spec_for_corpus(&hop.family)
        .and_then(|spec| static_authoritative_address(spec.token))
        .or_else(|| (hop.family == "DodoV2").then_some(hop.pool))
}

fn chain_provenance_value(
    th: &Throttled,
    chain_id: u64,
    pins: &HashSet<u64>,
    targets: &HashSet<(&'static str, u64, [u8; 20])>,
) -> Result<serde_json::Value, String> {
    if chain_id != ARBITRUM_ONE_CHAIN_ID {
        return Err(format!(
            "wrong RPC chain_id {chain_id}; expected Arbitrum One {ARBITRUM_ONE_CHAIN_ID}"
        ));
    }
    let mut sorted_pins: Vec<_> = pins.iter().copied().collect();
    sorted_pins.sort_unstable();
    let mut pinned_blocks = Vec::with_capacity(sorted_pins.len());
    for number in sorted_pins {
        pinned_blocks.push(serde_json::json!({
            "number": number,
            "hash": pinned_block_hash(th, number)?,
        }));
    }

    let mut sorted_targets: Vec<_> = targets.iter().copied().collect();
    sorted_targets.sort_unstable_by_key(|(family, pin, address)| (*pin, *family, *address));
    let mut quoters = Vec::with_capacity(sorted_targets.len());
    for (family, pin, address) in sorted_targets {
        let code = with_retry(th, |client| {
            client.get_code(&address, BlockTag::Number(pin))
        })?;
        if code.is_empty() {
            return Err(format!(
                "authoritative address {} has empty code at pin {pin}",
                hex_bytes(&address)
            ));
        }
        quoters.push(serde_json::json!({
            "family": family,
            "pin_block": pin,
            "address": hex_bytes(&address),
            "code_keccak256": hex_bytes(keccak256(code).as_slice()),
        }));
    }
    Ok(serde_json::json!({
        "chain_id": chain_id,
        "pinned_blocks": pinned_blocks,
        "quoters": quoters,
    }))
}

fn chain_provenance_matches_cached_blocks(
    chain: &serde_json::Value,
    expected: &HashMap<u64, String>,
) -> bool {
    let Some(rows) = chain["pinned_blocks"].as_array() else {
        return false;
    };
    expected.iter().all(|(number, hash)| {
        rows.iter().any(|row| {
            row["number"].as_u64() == Some(*number)
                && row["hash"]
                    .as_str()
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(hash))
        })
    }) && rows.len() == expected.len()
}

// ───────────────────────────── per-family kernel state at a pin ─────────────────────────────────
enum PoolState {
    V3 {
        slot0: V3Slot0,
        ticks: Vec<TickInfo>,
    },
    Dodo(engine_amm::dodo_v2::DodoState),
}

#[allow(clippy::too_many_arguments)]
fn snapshot_pool(
    th: &Throttled,
    hop: &HopMeta,
    block_num: u64,
    tick_width: i32,
) -> Result<PoolState, String> {
    let block = BlockTag::Number(block_num);
    let mc3 = addr20(MULTICALL3);
    match hop.family.as_str() {
        "UniswapV3" | "SushiV3" | "PancakeV3" => {
            let slot0 = with_retry(th, |c| engine_rpc::fetch_v3_slot0(c, &hop.pool, block))
                .map_err(|e| format!("slot0: {e}"))?;
            let liq = with_retry(th, |c| engine_rpc::fetch_v3_liquidity(c, &hop.pool, block))
                .map_err(|e| format!("liquidity: {e}"))?;
            let spacing = hop.tick_spacing.max(1);
            let win = with_retry(th, |c| {
                engine_rpc::fetch_v3_tick_window_multicall(
                    c, &mc3, &hop.pool, slot0.tick, spacing, tick_width, block,
                )
            })
            .map_err(|e| format!("ticks: {e}"))?;
            Ok(PoolState::V3 {
                slot0: V3Slot0 {
                    sqrt_price_x96: slot0.sqrt_price_x96,
                    tick: slot0.tick,
                    fee_pips: hop.fee,
                    liquidity: liq,
                    tick_spacing: spacing,
                },
                ticks: win
                    .into_iter()
                    .map(|t| TickInfo {
                        tick: t.tick,
                        liquidity_net: t.liquidity_net,
                    })
                    .collect(),
            })
        }
        "AlgebraV19" => {
            let gs = with_retry(th, |c| {
                engine_rpc::fetch_algebra_global_state(c, &hop.pool, block)
            })
            .map_err(|e| format!("globalState: {e}"))?;
            let liq = with_retry(th, |c| engine_rpc::fetch_v3_liquidity(c, &hop.pool, block))
                .map_err(|e| format!("liquidity: {e}"))?;
            // Algebra spacing is MUTABLE (admin setTickSpacing) — the record carries the
            // creation-time value; read the LIVE grid at the pin (root cause of the 28/220
            // size-growing mismatches: created 60, live 10 → ticks invisible to a 60-step walk).
            let spacing = with_retry(th, |c| {
                engine_rpc::fetch_algebra_tick_spacing(c, &hop.pool, block)
            })
            .map_err(|e| format!("tickSpacing: {e}"))?
            .max(1);
            let win = with_retry(th, |c| {
                engine_rpc::fetch_v3_tick_window_multicall(
                    c, &mc3, &hop.pool, gs.tick, spacing, tick_width, block,
                )
            })
            .map_err(|e| format!("ticks: {e}"))?;
            Ok(PoolState::V3 {
                slot0: V3Slot0 {
                    sqrt_price_x96: gs.sqrt_price_x96,
                    tick: gs.tick,
                    // Dynamic Algebra fee READ AT THE PIN — state-matched by construction
                    // (the plan's block_env caveat dissolves: both sides read the same state).
                    fee_pips: gs.fee_pips,
                    liquidity: liq,
                    tick_spacing: spacing,
                },
                ticks: win
                    .into_iter()
                    .map(|t| TickInfo {
                        tick: t.tick,
                        liquidity_net: t.liquidity_net,
                    })
                    .collect(),
            })
        }
        "UniswapV4" => {
            let (Some(pool_id), Some(state_view)) = (hop.pool_id, hop.state_view) else {
                return Err("v4 hop without pool_id/state_view".into());
            };
            let slot0 = with_retry(th, |c| {
                engine_rpc::fetch_v4_slot0(c, &state_view, &pool_id, block)
            })
            .map_err(|e| format!("v4 slot0: {e}"))?;
            let liq = with_retry(th, |c| {
                engine_rpc::fetch_v4_liquidity(c, &state_view, &pool_id, block)
            })
            .map_err(|e| format!("v4 liquidity: {e}"))?;
            let spacing = hop.tick_spacing.max(1);
            let win = with_retry(th, |c| {
                engine_rpc::fetch_v4_tick_window(
                    c,
                    &state_view,
                    &pool_id,
                    slot0.tick,
                    spacing,
                    tick_width,
                    block,
                )
            })
            .map_err(|e| format!("v4 ticks: {e}"))?;
            let fee_pips = if hop.fee == 0 || hop.fee > 1_000_000 {
                slot0.lp_fee
            } else {
                hop.fee
            };
            Ok(PoolState::V3 {
                slot0: V3Slot0 {
                    sqrt_price_x96: slot0.sqrt_price_x96,
                    tick: slot0.tick,
                    fee_pips,
                    liquidity: liq,
                    tick_spacing: spacing,
                },
                ticks: win
                    .into_iter()
                    .map(|t| TickInfo {
                        tick: t.tick,
                        liquidity_net: t.liquidity_net,
                    })
                    .collect(),
            })
        }
        "DodoV2" => {
            // A guessed fallback class is not promotion evidence. Require the pool's own version()
            // to identify DVM/DPP/DSP, then snapshot with that authoritative class.
            let version = th
                .eth_call(&hop.pool, &sel("version()"), block)
                .map_err(|e| format!("dodo version(): {e}"))?;
            let class = dodo_class_from_version(&version)
                .ok_or_else(|| "dodo version() did not identify DVM/DPP/DSP".to_string())?;
            with_retry(th, |c| {
                engine_rpc::fetch_dodo_state(c, &hop.pool, class, block, block_num)
            })
            .map(PoolState::Dodo)
            .map_err(|e| format!("dodo state: {e}"))
        }
        other => Err(format!("unsupported family {other}")),
    }
}

struct KernelObservation {
    amount_out: U256,
    edge_cases: Vec<String>,
    sqrt_price_x96_after: Option<U256>,
    initialized_ticks_crossed: usize,
}

fn mechanical_size_edge_cases(amount_in: U256, initialized_ticks_crossed: usize) -> Vec<String> {
    if amount_in == U256::from(1u8) {
        vec!["dust".to_string()]
    } else if initialized_ticks_crossed >= 2 {
        vec!["extreme".to_string()]
    } else {
        vec!["nominal".to_string()]
    }
}

fn quote_reaches_boundary(final_sqrt: U256, boundary: U256, zero_for_one: bool) -> bool {
    if zero_for_one {
        final_sqrt <= boundary
    } else {
        final_sqrt >= boundary
    }
}

/// Find the smallest exact-input amount that reaches or passes `boundary`, then inspect a narrow
/// neighbourhood to require the actual kernel transition to finish within one Q96 unit. Kernel
/// errors at the upper bound are treated as "too large" so a warmed-window exhaustion cannot be
/// mistaken for coverage.
fn targeted_boundary_amount(state: &PoolState, hop: &HopMeta, boundary: U256) -> Option<U256> {
    let zero_for_one = hop.token_in < hop.token_out;
    let mut low = U256::ZERO;
    let mut high = U256::from(1u8);
    let mut bracketed = false;
    for _ in 0..256 {
        match kernel_quote(state, hop, high) {
            Ok(observation) => {
                let final_sqrt = observation.sqrt_price_x96_after?;
                if quote_reaches_boundary(final_sqrt, boundary, zero_for_one) {
                    bracketed = true;
                    break;
                }
                low = high;
            }
            Err(_) => {
                bracketed = true;
                break;
            }
        }
        high = high.checked_mul(U256::from(2u8))?;
    }
    if !bracketed {
        return None;
    }

    while high > low + U256::from(1u8) {
        let mid = low + (high - low) / U256::from(2u8);
        match kernel_quote(state, hop, mid) {
            Ok(observation) => {
                let final_sqrt = observation.sqrt_price_x96_after?;
                if quote_reaches_boundary(final_sqrt, boundary, zero_for_one) {
                    high = mid;
                } else {
                    low = mid;
                }
            }
            Err(_) => high = mid,
        }
    }

    let start = high.saturating_sub(U256::from(4u8)).max(U256::from(1u8));
    for offset in 0..=8u8 {
        let amount = start + U256::from(offset);
        if kernel_quote(state, hop, amount).is_ok_and(|observation| {
            observation
                .sqrt_price_x96_after
                .is_some_and(|final_sqrt| final_sqrt.abs_diff(boundary) <= U256::from(1u8))
        }) {
            return Some(amount);
        }
    }
    None
}

/// Deterministically derive the six-input targeted suite used by generated strict evidence. The
/// suite includes an exact dust input, ordinary non-crossing inputs, a mechanically proved tick
/// boundary, and a mechanically proved two-or-more-initialized-tick crossing.
fn targeted_v3_amounts(state: &PoolState, hop: &HopMeta) -> Result<Vec<U256>, String> {
    let PoolState::V3 { slot0, ticks } = state else {
        return Err("targeted CL amount search requires V3-like state".to_string());
    };
    let zero_for_one = hop.token_in < hop.token_out;
    let mut boundaries: Vec<U256> = ticks
        .iter()
        .filter_map(|tick| engine_amm::sqrt_price_at_tick(tick.tick).ok())
        .filter(|boundary| {
            if zero_for_one {
                *boundary < slot0.sqrt_price_x96
            } else {
                *boundary > slot0.sqrt_price_x96
            }
        })
        .collect();
    if zero_for_one {
        boundaries.sort_unstable_by(|a, b| b.cmp(a));
    } else {
        boundaries.sort_unstable();
    }
    let near = boundaries
        .iter()
        .find_map(|boundary| targeted_boundary_amount(state, hop, *boundary))
        .ok_or_else(|| "no exact initialized-tick boundary amount in warmed state".to_string())?;

    // Finishing exactly at the third initialized boundary has mechanically traversed the first
    // two. Searching boundary targets avoids jumping straight from a one-cross quote to warmed
    // window exhaustion when adjacent ranges have very different liquidity.
    let multi = boundaries
        .iter()
        .skip(2)
        .filter_map(|boundary| targeted_boundary_amount(state, hop, *boundary))
        .find(|amount| {
            kernel_quote(state, hop, *amount)
                .is_ok_and(|observation| observation.initialized_ticks_crossed >= 2)
        })
        .ok_or_else(|| "no two-initialized-tick crossing inside warmed state".to_string())?;

    let mut amounts = vec![
        U256::from(1u8),
        U256::from(2u8),
        U256::from(3u8),
        U256::from(4u8),
        near,
        multi,
    ];
    amounts.sort_unstable();
    amounts.dedup();
    if amounts.len() != MIN_INPUTS_PER_POOL_DIRECTION {
        return Err(format!(
            "targeted amount suite collapsed to {} distinct inputs",
            amounts.len()
        ));
    }
    Ok(amounts)
}

/// Algebra V1.9's `TickTable.nextTickInTheSameRow` stops at a raw 256-tick bitmap row boundary
/// even when that boundary is not initialized. This diagnostic enumerates those exact boundaries;
/// unlike Uniswap V3, Algebra does not compress the tick by tick spacing before choosing a row.
fn algebra_row_boundaries(lo: i32, hi: i32, zero_for_one: bool) -> Vec<i32> {
    let residue = if zero_for_one { 0 } else { 255 };
    let mut tick = lo + (residue - lo).rem_euclid(256);
    let mut out = Vec::new();
    while tick <= hi {
        out.push(tick);
        let Some(next) = tick.checked_add(256) else {
            break;
        };
        tick = next;
    }
    out
}

/// Diagnostic alternate only: reproduce Algebra's bitmap-row segmentation by inserting zero-net
/// step boundaries before calling the same integer kernel. This result is printed for mismatches
/// but never participates in the strict verdict or promotion eligibility.
fn algebra_row_segmented_amount_out(
    state: &PoolState,
    hop: &HopMeta,
    amount_in: U256,
) -> Result<U256, String> {
    let PoolState::V3 { slot0, ticks } = state else {
        return Err("Algebra row diagnostic requires V3-like state".to_string());
    };
    let (Some(first), Some(last)) = (ticks.first(), ticks.last()) else {
        return Err("Algebra row diagnostic requires a nonempty tick window".to_string());
    };
    let zero_for_one = hop.token_in < hop.token_out;
    let mut segmented = ticks.clone();
    for tick in algebra_row_boundaries(first.tick, last.tick, zero_for_one) {
        if segmented
            .binary_search_by_key(&tick, |info| info.tick)
            .is_err()
        {
            segmented.push(TickInfo {
                tick,
                liquidity_net: 0,
            });
        }
    }
    segmented.sort_unstable_by_key(|info| info.tick);
    engine_amm::v3_get_amount_out(*slot0, amount_in, zero_for_one, &segmented)
        .map(|quote| quote.amount_out)
        .map_err(|error| format!("Algebra row-segmented diagnostic: {error:?}"))
}

fn select_target_pin(timeline: &[(u64, u64)], margin_ns: u64) -> Option<(u64, u64)> {
    timeline
        .windows(2)
        .filter_map(|window| {
            let (start_ns, pin) = window[0];
            let (end_ns, _) = window[1];
            let usable_start = start_ns.checked_add(margin_ns)?;
            let usable_end = end_ns.checked_sub(margin_ns)?;
            (usable_start <= usable_end).then(|| {
                let recv_ns = usable_start + (usable_end - usable_start) / 2;
                (end_ns - start_ns, recv_ns, pin)
            })
        })
        .max_by_key(|(gap, _, _)| *gap)
        .map(|(_, recv_ns, pin)| (recv_ns, pin))
}

fn basic_v3_amounts(state: &PoolState, hop: &HopMeta) -> Result<Vec<U256>, String> {
    let mut amounts = Vec::with_capacity(MIN_INPUTS_PER_POOL_DIRECTION);
    for raw in 1u64..=64 {
        let amount = U256::from(raw);
        if kernel_quote(state, hop, amount).is_ok() {
            amounts.push(amount);
        }
        if amounts.len() == MIN_INPUTS_PER_POOL_DIRECTION {
            break;
        }
    }
    if amounts.first() != Some(&U256::from(1u8)) {
        return Err("pool does not support the exact dust input amount=1".to_string());
    }
    if amounts.len() != MIN_INPUTS_PER_POOL_DIRECTION {
        return Err(format!(
            "only {} mechanically valid basic inputs",
            amounts.len()
        ));
    }
    Ok(amounts)
}

fn reversed_hop(hop: &HopMeta) -> HopMeta {
    let mut reverse = hop.clone();
    std::mem::swap(&mut reverse.token_in, &mut reverse.token_out);
    reverse.parity_edge_cases.clear();
    reverse
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write to String");
    }
    out
}

fn generated_candidate_id(
    family: &str,
    identity: &[u8; 32],
    direction: &str,
    index: usize,
) -> String {
    format!(
        "generated-parity:{family}:{}:{direction}:{index}",
        &hex_bytes(identity)[2..]
    )
}

fn successful_transcript_row(
    candidate_id: &str,
    family: &str,
    chain_id: u64,
    pin_block: u64,
    pin_block_hash: &str,
    pool_identity: &[u8; 32],
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    kernel_amount_out: U256,
    authoritative_amount_out: U256,
    edge_cases: &[String],
    variants: &[&str],
    authoritative_address: &[u8; 20],
) -> serde_json::Value {
    let diff = kernel_amount_out.abs_diff(authoritative_amount_out);
    serde_json::json!({
        "schema": "mantis_parity_comparison_v1",
        "candidate_id": candidate_id,
        "family": family,
        "chain_id": chain_id,
        "pin_block": pin_block,
        "pin_block_hash": pin_block_hash,
        "pool_identity": hex_bytes(pool_identity),
        "token_in": hex_bytes(token_in),
        "token_out": hex_bytes(token_out),
        "direction": if token_in < token_out { "zero_for_one" } else { "one_for_zero" },
        "amount_in": amount_in.to_string(),
        "kernel_amount_out": kernel_amount_out.to_string(),
        "authoritative_amount_out": authoritative_amount_out.to_string(),
        "absolute_difference_wei": diff.to_string(),
        "exact": diff.is_zero(),
        "mechanical_edge_cases": edge_cases,
        "mechanical_variants": variants,
        "authoritative_address": hex_bytes(authoritative_address),
        "outcome": "resolved",
    })
}

#[allow(clippy::too_many_arguments)]
fn error_transcript_row(
    candidate_id: &str,
    family: &str,
    chain_id: u64,
    pin_block: u64,
    pin_block_hash: &str,
    pool_identity: &[u8; 32],
    token_in: &[u8; 20],
    token_out: &[u8; 20],
    amount_in: U256,
    authoritative_address: &[u8; 20],
    stage: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "mantis_parity_comparison_v1",
        "candidate_id": candidate_id,
        "family": family,
        "chain_id": chain_id,
        "pin_block": pin_block,
        "pin_block_hash": pin_block_hash,
        "pool_identity": hex_bytes(pool_identity),
        "token_in": hex_bytes(token_in),
        "token_out": hex_bytes(token_out),
        "direction": if token_in < token_out { "zero_for_one" } else { "one_for_zero" },
        "amount_in": amount_in.to_string(),
        "authoritative_address": hex_bytes(authoritative_address),
        "outcome": "unresolved_error",
        "error_stage": stage,
        "error": error,
    })
}

fn campaign_manifest_value(
    source_head: String,
    source_tree_status: &str,
    source_manifest_sha256: String,
    producer_binary_sha256: String,
    chain: serde_json::Value,
    artifacts: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "mantis_parity_campaign_v1",
        "source": {
            "head": source_head,
            "tree_status": source_tree_status,
            "manifest_sha256": source_manifest_sha256,
        },
        "producer_binary_sha256": producer_binary_sha256.clone(),
        "chain": chain.clone(),
        "artifacts": artifacts,
    })
}

fn campaign_artifact_value(
    campaign_manifest: &std::path::Path,
    artifact: &std::path::Path,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "path": portable_evidence_path(campaign_manifest, artifact)?,
        "sha256": file_sha256(artifact)?,
        "keccak256": file_keccak256(
            artifact
                .to_str()
                .ok_or_else(|| format!("non-UTF8 artifact path {}", artifact.display()))?
        ),
    }))
}

fn generated_hop_json(hop: &HopMeta) -> serde_json::Value {
    let mut value = serde_json::json!({
        "family": hop.family,
        "pool": hex_bytes(&hop.pool),
        "token_in": hex_bytes(&hop.token_in),
        "token_out": hex_bytes(&hop.token_out),
        "fee_pips_or_bps": hop.fee,
        "tick_spacing": hop.tick_spacing,
    });
    if let Some(pool_id) = hop.pool_id {
        value["pool_id"] = serde_json::Value::String(hex_bytes(&pool_id));
    }
    if let Some(state_view) = hop.state_view {
        value["state_view"] = serde_json::Value::String(hex_bytes(&state_view));
    }
    value
}

struct GeneratedPoolTarget {
    identity: [u8; 32],
    rows: Vec<serde_json::Value>,
    edges: HashSet<String>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_generated_pool(
    th: &Throttled,
    source_candidate_id: &str,
    source_corpus: &str,
    state_pin_source: &str,
    recv_ns: u64,
    pin: u64,
    tick_width: i32,
    template: &HopMeta,
) -> Result<GeneratedPoolTarget, String> {
    let mut forward = template.clone();
    forward.parity_edge_cases.clear();
    if forward.token_in > forward.token_out {
        std::mem::swap(&mut forward.token_in, &mut forward.token_out);
    }
    let reverse = reversed_hop(&forward);
    let state = snapshot_pool(th, &forward, pin, tick_width)?;
    let mut rows = Vec::with_capacity(MIN_INPUTS_PER_POOL_DIRECTION * 2);
    let mut pool_edges = HashSet::new();

    for hop in [&forward, &reverse] {
        let amounts =
            targeted_v3_amounts(&state, hop).or_else(|_| basic_v3_amounts(&state, hop))?;
        for (index, amount) in amounts.into_iter().enumerate() {
            let observation = kernel_quote(&state, hop, amount)?;
            // Transport/ABI preflight only. The returned amount is intentionally discarded and is
            // never used to select or remove a row; the later strict campaign compares every row.
            let _authoritative_response = quoter_quote(th, hop, amount, pin)?;
            pool_edges.extend(observation.edge_cases.iter().cloned());
            let direction = if hop.token_in < hop.token_out {
                "zero_for_one"
            } else {
                "one_for_zero"
            };
            rows.push(serde_json::json!({
                "candidate_id": generated_candidate_id(
                    family_spec_for_corpus(&hop.family).map_or("unknown", |spec| spec.token),
                    &pool_identity(hop),
                    direction,
                    index,
                ),
                "recv_ns": recv_ns,
                "input_amount": amount.to_string(),
                "exchange_metadata": [generated_hop_json(hop)],
                "balance_delta_wei": null,
                "revm_result": "not_applicable_generated_state_query",
                "changed_slots": [],
                "mantis_executor": null,
                "parity_provenance": {
                    "schema": "mantis_targeted_parity_row_v1",
                    "kind": "generated_targeted_input",
                    "source_candidate_id": source_candidate_id,
                    "source_corpus": source_corpus,
                    "state_pin_source": state_pin_source,
                    "pin_block": pin,
                    "amount_selection": "mechanical_integer_transition_search_v1",
                    "kernel_edges_at_generation": observation.edge_cases,
                    "authoritative_reference_preflight": "eth_call_succeeded_output_not_used_for_selection"
                }
            }));
        }
    }
    Ok(GeneratedPoolTarget {
        identity: pool_identity(&forward),
        rows,
        edges: pool_edges,
    })
}

fn write_targeted_corpus(
    th: &Throttled,
    chain_id: u64,
    output_path: &str,
    source_corpus_path: &str,
    state_pin_source_path: &str,
    records: &[Record],
    timeline: &[(u64, u64)],
    required_families: &[&str],
    margin_ns: u64,
    tick_width: i32,
) -> Result<(), String> {
    let (recv_ns, pin) = select_target_pin(timeline, margin_ns).ok_or_else(|| {
        "no recovered pin interval is wider than two boundary margins".to_string()
    })?;
    if assign_pin(timeline, recv_ns, margin_ns) != Some(pin) {
        return Err(
            "selected generated recv_ns does not round-trip to the recovered pin".to_string(),
        );
    }
    let output = std::path::Path::new(output_path);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create corpus output directory: {error}"))?;
    }
    let manifest_path = format!("{output_path}.manifest.json");
    let manifest_path_ref = std::path::Path::new(&manifest_path);
    let portable_pin_path = format!("{output_path}.pin-timeline.jsonl");
    write_portable_pin_timeline(std::path::Path::new(&portable_pin_path), timeline)?;
    let source_corpus =
        portable_evidence_path(manifest_path_ref, std::path::Path::new(source_corpus_path))?;
    let source_state_pin = portable_evidence_path(
        manifest_path_ref,
        std::path::Path::new(state_pin_source_path),
    )?;
    let state_pin_source =
        portable_evidence_path(manifest_path_ref, std::path::Path::new(&portable_pin_path))?;

    let mut all_rows = Vec::new();
    let mut manifest_families = serde_json::Map::new();
    for token in required_families {
        let spec = family_spec_by_token(token)
            .ok_or_else(|| format!("unknown targeted family {token}"))?;
        if !spec.harness_supported
            || !matches!(
                spec.token,
                "uniswap_v3" | "pancake_v3" | "algebra_v19" | "uniswap_v4"
            )
        {
            return Err(format!(
                "targeted generator supports only the four CL authoritative families; got {token}"
            ));
        }
        let mut seeds = Vec::new();
        let mut seen_pools = HashSet::new();
        for record in records {
            for hop in &record.hops {
                if family_spec_for_corpus(&hop.family).is_some_and(|got| got.token == spec.token)
                    && seen_pools.insert(pool_identity(hop))
                {
                    seeds.push((record.candidate_id.clone(), hop.clone()));
                }
            }
        }
        if seeds.len() < MIN_POOLS {
            return Err(format!(
                "{token}: source corpus has only {} distinct real pools",
                seeds.len()
            ));
        }

        let mut viable = Vec::new();
        let mut errors = Vec::new();
        for (source_candidate_id, hop) in seeds {
            match evaluate_generated_pool(
                th,
                &source_candidate_id,
                &source_corpus,
                &state_pin_source,
                recv_ns,
                pin,
                tick_width,
                &hop,
            ) {
                Ok(candidate) => viable.push(candidate),
                Err(error) => {
                    if errors.len() < 12 {
                        errors.push(format!(
                            "pool=0x{}: {error}",
                            &hex_bytes(&pool_identity(&hop))[2..]
                        ));
                    }
                }
            }
            let union: HashSet<&str> = viable
                .iter()
                .flat_map(|candidate| candidate.edges.iter().map(String::as_str))
                .collect();
            if viable.len() >= MIN_POOLS
                && spec
                    .required_edge_cases
                    .iter()
                    .all(|edge| union.contains(edge))
            {
                break;
            }
        }
        viable.sort_by_key(|candidate| std::cmp::Reverse(candidate.edges.len()));
        let chosen: Vec<_> = viable.into_iter().take(MIN_POOLS).collect();
        if chosen.len() < MIN_POOLS {
            return Err(format!(
                "{token}: only {} real pools passed kernel+authoritative transport preflight; samples={errors:?}",
                chosen.len()
            ));
        }
        let family_edges: HashSet<String> = chosen
            .iter()
            .flat_map(|candidate| candidate.edges.iter().cloned())
            .collect();
        let missing: Vec<_> = spec
            .required_edge_cases
            .iter()
            .filter(|edge| !family_edges.contains(**edge))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{token}: three-pool mechanical target set lacks edges {missing:?}; samples={errors:?}"
            ));
        }
        let mut pool_ids = Vec::new();
        let mut sorted_edges: Vec<_> = family_edges.into_iter().collect();
        sorted_edges.sort();
        for candidate in chosen {
            pool_ids.push(hex_bytes(&candidate.identity));
            all_rows.extend(candidate.rows);
        }
        manifest_families.insert(
            token.to_string(),
            serde_json::json!({
                "real_pinned_pools": pool_ids,
                "pool_count": MIN_POOLS,
                "inputs_per_pool_direction": MIN_INPUTS_PER_POOL_DIRECTION,
                "generated_comparisons": MIN_POOLS * 2 * MIN_INPUTS_PER_POOL_DIRECTION,
                "mechanical_edges": sorted_edges,
            }),
        );
    }

    let mut file = std::io::BufWriter::new(
        std::fs::File::create(output)
            .map_err(|error| format!("create targeted corpus {output_path}: {error}"))?,
    );
    for row in &all_rows {
        use std::io::Write as _;
        serde_json::to_writer(&mut file, row)
            .map_err(|error| format!("serialize targeted row: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("write targeted row: {error}"))?;
    }
    use std::io::Write as _;
    file.flush()
        .map_err(|error| format!("flush targeted corpus: {error}"))?;

    let generated_corpus = portable_evidence_path(manifest_path_ref, output)?;
    let source_manifest = source_manifest_sha256().ok_or_else(|| {
        "targeted evidence build must embed ENGINE_SOURCE_MANIFEST_SHA256".to_string()
    })?;
    let producer_binary =
        std::env::current_exe().map_err(|error| format!("locate producer binary: {error}"))?;
    let producer_binary_sha256 = file_sha256(&producer_binary)?;
    let mut pins = HashSet::new();
    pins.insert(pin);
    let mut chain_targets = HashSet::new();
    for token in required_families {
        let address = static_authoritative_address(token)
            .ok_or_else(|| format!("no static authoritative address for {token}"))?;
        let family = family_spec_by_token(token)
            .expect("validated targeted family")
            .token;
        chain_targets.insert((family, pin, address));
    }
    let chain = chain_provenance_value(th, chain_id, &pins, &chain_targets)?;
    let manifest = serde_json::json!({
        "schema": "mantis_targeted_parity_corpus_v1",
        "generated_by": "state_matched_parity --generate-targeted-corpus",
        "source_head": source_head(),
        "source_tree_status": source_tree_status(),
        "source_manifest_sha256": source_manifest,
        "producer_binary_sha256": producer_binary_sha256,
        "chain": chain,
        "generated_corpus": generated_corpus,
        "generated_corpus_sha256": file_sha256(output)?,
        "generated_corpus_keccak256": file_keccak256(output_path),
        "source_corpus": source_corpus,
        "source_corpus_keccak256": file_keccak256(source_corpus_path),
        "source_state_pin_source": source_state_pin,
        "source_state_pin_source_keccak256": file_keccak256(state_pin_source_path),
        "state_pin_source": state_pin_source,
        "state_pin_source_sha256": file_sha256(std::path::Path::new(&portable_pin_path))?,
        "state_pin_source_keccak256": file_keccak256(&portable_pin_path),
        "pin_block": pin,
        "synthetic_recv_ns": recv_ns,
        "boundary_margin_ns": margin_ns,
        "row_count": all_rows.len(),
        "families": manifest_families,
        "selection_invariant": "kernel transition and authoritative transport success only; authoritative amount was not compared or used to select rows",
        "promotion_authority": "the subsequent strict state_matched_parity report, never this generator manifest",
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize targeted manifest"),
    )
    .map_err(|error| format!("write targeted manifest: {error}"))?;
    eprintln!(
        "targeted corpus generated: rows={} pin={} corpus={} manifest={}",
        all_rows.len(),
        pin,
        output_path,
        manifest_path
    );
    Ok(())
}

/// Kernel quote for one hop on the pinned snapshot. Every promotion edge is derived from the
/// pinned state and actual integer transition; corpus-authored labels are deliberately ignored.
fn kernel_quote(
    state: &PoolState,
    hop: &HopMeta,
    amount_in: U256,
) -> Result<KernelObservation, String> {
    match state {
        PoolState::V3 { slot0, ticks } => {
            let zfo = hop.token_in < hop.token_out; // token0 = lexicographically smaller
            let quote = engine_amm::v3_get_amount_out(*slot0, amount_in, zfo, ticks)
                .map_err(|e| format!("kernel: {e:?}"))?;

            // A boundary case is mechanically proved only when the final sqrt price is within one
            // Q96 unit of an initialized tick. Do not infer it from a prose label or tolerance.
            let near_boundary = ticks.iter().any(|tick| {
                engine_amm::sqrt_price_at_tick(tick.tick).is_ok_and(|boundary| {
                    quote.sqrt_price_x96_after.abs_diff(boundary) <= U256::from(1u8)
                })
            });
            // Count initialized boundaries actually traversed between the pinned start price and
            // final price. Two or more is a genuine multi-tick traversal, independent of tick_after.
            let lo = slot0.sqrt_price_x96.min(quote.sqrt_price_x96_after);
            let hi = slot0.sqrt_price_x96.max(quote.sqrt_price_x96_after);
            let crossed = ticks
                .iter()
                .filter_map(|tick| engine_amm::sqrt_price_at_tick(tick.tick).ok())
                .filter(|boundary| *boundary > lo && *boundary <= hi)
                .take(2)
                .count();
            let mut edge_cases = mechanical_size_edge_cases(amount_in, crossed);
            if near_boundary {
                edge_cases.push("near_tick_boundary".to_string());
            }
            if crossed >= 2 {
                edge_cases.push("multi_tick_crossing".to_string());
            }
            edge_cases.sort();
            edge_cases.dedup();
            Ok(KernelObservation {
                amount_out: quote.amount_out,
                edge_cases,
                sqrt_price_x96_after: Some(quote.sqrt_price_x96_after),
                initialized_ticks_crossed: crossed,
            })
        }
        PoolState::Dodo(s) => {
            let quote = s
                .quote_for_tokens_exact_in(hop.token_in.into(), hop.token_out.into(), amount_in)
                .map_err(|e| format!("kernel: {e:?}"))?;
            let output_reserve = if s.base_token == hop.token_out {
                s.base_reserve
            } else {
                s.quote_reserve
            };
            let mut edge_cases = mechanical_size_edge_cases(amount_in, 0);
            if !output_reserve.is_zero()
                && quote.amount_out_native >= output_reserve - output_reserve / U256::from(10u8)
            {
                edge_cases.push("near_empty_reserve".to_string());
            }
            edge_cases.sort();
            edge_cases.dedup();
            Ok(KernelObservation {
                amount_out: quote.amount_out_native,
                edge_cases,
                sqrt_price_x96_after: None,
                initialized_ticks_crossed: 0,
            })
        }
    }
}

/// The chain's own answer for the hop at the same block.
fn quoter_quote(
    th: &Throttled,
    hop: &HopMeta,
    amount_in: U256,
    block_num: u64,
) -> Result<U256, String> {
    let block = BlockTag::Number(block_num);
    match hop.family.as_str() {
        "UniswapV3" | "PancakeV3" => {
            let quoter = if hop.family == "UniswapV3" {
                addr20(UNI_V3_QUOTER_V2)
            } else {
                addr20(PANCAKE_QUOTER_V2)
            };
            let mut cd = Vec::with_capacity(4 + 160);
            cd.extend_from_slice(&sel(
                "quoteExactInputSingle((address,address,uint256,uint24,uint160))",
            ));
            cd.extend_from_slice(&word_addr(&hop.token_in));
            cd.extend_from_slice(&word_addr(&hop.token_out));
            cd.extend_from_slice(&word_u256(amount_in));
            cd.extend_from_slice(&word_u256(U256::from(hop.fee)));
            cd.extend_from_slice(&word_u256(U256::ZERO));
            let resp = th.eth_call(&quoter, &cd, block)?;
            decode_word0(&resp).ok_or_else(|| "short quoter response".into())
        }
        "AlgebraV19" => {
            let quoter = addr20(ALGEBRA_QUOTER);
            let mut cd = Vec::with_capacity(4 + 128);
            cd.extend_from_slice(&sel(
                "quoteExactInputSingle(address,address,uint256,uint160)",
            ));
            cd.extend_from_slice(&word_addr(&hop.token_in));
            cd.extend_from_slice(&word_addr(&hop.token_out));
            cd.extend_from_slice(&word_u256(amount_in));
            cd.extend_from_slice(&word_u256(U256::ZERO));
            let resp = th.eth_call(&quoter, &cd, block)?;
            decode_word0(&resp).ok_or_else(|| "short quoter response".into())
        }
        "UniswapV4" => {
            let (c0, c1) = if hop.token_in < hop.token_out {
                (hop.token_in, hop.token_out)
            } else {
                (hop.token_out, hop.token_in)
            };
            let pk = encode_pool_key(&c0, &c1, hop.fee, hop.tick_spacing, &[0u8; 20]);
            // PoolKey self-check: derived poolId must equal the recorded one (proves hookless
            // + sorted-currency assumptions for THIS pool before trusting the quoter answer).
            if let Some(rec_id) = hop.pool_id {
                let derived: [u8; 32] = keccak256(pk).into();
                if derived != rec_id {
                    return Err("v4 PoolKey self-check failed (hooked or non-standard pool)".into());
                }
            }
            let zfo = hop.token_in == c0;
            let amt: u128 = amount_in
                .try_into()
                .map_err(|_| "v4 amount > u128".to_string())?;
            let cd = encode_v4_quoter_call(&pk, zfo, amt);
            let resp = th.eth_call(&addr20(V4_QUOTER), &cd, block)?;
            decode_word0(&resp).ok_or_else(|| "short quoter response".into())
        }
        "DodoV2" => {
            // The pool's own view: querySellBase/querySellQuote(trader, amount) →
            // (receiveAmount, mtFee). Which one depends on whether token_in is the base.
            let base_resp = th.eth_call(&hop.pool, &sel("_BASE_TOKEN_()").to_vec(), block)?;
            let base_token: [u8; 20] = base_resp
                .get(12..32)
                .map(|s| {
                    let mut a = [0u8; 20];
                    a.copy_from_slice(s);
                    a
                })
                .ok_or_else(|| "short _BASE_TOKEN_".to_string())?;
            let sig = if hop.token_in == base_token {
                "querySellBase(address,uint256)"
            } else {
                "querySellQuote(address,uint256)"
            };
            let mut cd = Vec::with_capacity(4 + 64);
            cd.extend_from_slice(&sel(sig));
            cd.extend_from_slice(&word_addr(&[0u8; 20]));
            cd.extend_from_slice(&word_u256(amount_in));
            let resp = th.eth_call(&hop.pool, &cd, block)?;
            decode_word0(&resp).ok_or_else(|| "short query response".into())
        }
        other => Err(format!("no quoter for {other}")),
    }
}

/// abi.encode(PoolKey{currency0,currency1,fee,tickSpacing,hooks}) — 5 static words.
fn encode_pool_key(
    c0: &[u8; 20],
    c1: &[u8; 20],
    fee: u32,
    tick_spacing: i32,
    hooks: &[u8; 20],
) -> [u8; 160] {
    let mut b = [0u8; 160];
    b[12..32].copy_from_slice(c0);
    b[44..64].copy_from_slice(c1);
    b[93..96].copy_from_slice(&fee.to_be_bytes()[1..4]);
    let ts = (tick_spacing as u32).to_be_bytes();
    b[125..128].copy_from_slice(&ts[1..4]);
    b[140..160].copy_from_slice(hooks);
    b
}

/// quoteExactInputSingle(QuoteExactSingleParams{poolKey,zeroForOne,exactAmount,hookData=empty}).
fn encode_v4_quoter_call(pool_key: &[u8; 160], zero_for_one: bool, exact_amount: u128) -> Vec<u8> {
    let selector =
        sel("quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))");
    let mut cd = Vec::with_capacity(4 + 32 + 288);
    cd.extend_from_slice(&selector);
    cd.extend_from_slice(&word_u256(U256::from(0x20u64))); // params tuple head offset
    cd.extend_from_slice(pool_key);
    cd.extend_from_slice(&word_u256(U256::from(zero_for_one as u64)));
    cd.extend_from_slice(&word_u256(U256::from(exact_amount)));
    cd.extend_from_slice(&word_u256(U256::from(0x100u64))); // hookData offset (from tuple start)
    cd.extend_from_slice(&word_u256(U256::ZERO)); // hookData len = 0
    cd
}

// ───────────────────────────── tally + main ────────────────────────────────────────────────────
#[derive(Default)]
struct FamilyTally {
    corpus_hops_seen: u64,
    hops_checked: u64,
    exact: u64,
    mismatch: u64,
    kernel_err: u64,
    quoter_err: u64,
    snapshot_err: u64,
    max_abs_disagreement_wei: U256,
    pools_checked: HashSet<[u8; 32]>,
    /// Distinct `(pool,direction,input)` observations; direction is part of proof breadth.
    inputs_checked: HashSet<([u8; 32], &'static str, U256)>,
    directions: HashSet<&'static str>,
    variants: HashSet<&'static str>,
    edge_cases: HashSet<String>,
    mismatch_samples: Vec<String>,
    error_samples: Vec<String>,
}

impl FamilyTally {
    fn unresolved_errors(&self) -> u64 {
        self.kernel_err + self.quoter_err + self.snapshot_err
    }

    fn pools_with_bidirectional_input_breadth(&self) -> usize {
        let mut counts: HashMap<([u8; 32], &'static str), usize> = HashMap::new();
        for (pool, direction, _) in &self.inputs_checked {
            *counts.entry((*pool, *direction)).or_default() += 1;
        }
        self.pools_checked
            .iter()
            .filter(|pool| {
                ["zero_for_one", "one_for_zero"].iter().all(|direction| {
                    counts.get(&(**pool, *direction)).copied().unwrap_or(0)
                        >= MIN_INPUTS_PER_POOL_DIRECTION
                })
            })
            .count()
    }
}

struct Assessment {
    byte_exact: bool,
    verdict: &'static str,
    missing_requirements: Vec<String>,
}

fn assess(spec: &FamilySpec, tally: &FamilyTally) -> Assessment {
    let mut missing = Vec::new();
    if !spec.harness_supported {
        missing.push("comparison_not_implemented".to_string());
    }
    if tally.hops_checked < MIN_COMPARISONS {
        missing.push(format!(
            "comparisons={}<{}",
            tally.hops_checked, MIN_COMPARISONS
        ));
    }
    if tally.pools_checked.len() < MIN_POOLS {
        missing.push(format!(
            "pools_checked={}<{}",
            tally.pools_checked.len(),
            MIN_POOLS
        ));
    }
    if tally.inputs_checked.len() < MIN_DIRECTIONAL_INPUTS {
        missing.push(format!(
            "inputs_checked={}<{}",
            tally.inputs_checked.len(),
            MIN_DIRECTIONAL_INPUTS
        ));
    }
    if tally.pools_with_bidirectional_input_breadth() < MIN_POOLS {
        missing.push(format!(
            "pools_with_{MIN_INPUTS_PER_POOL_DIRECTION}_inputs_each_direction={}<{}",
            tally.pools_with_bidirectional_input_breadth(),
            MIN_POOLS
        ));
    }
    if tally.directions.len() < 2 {
        missing.push(format!("directions_checked={}<2", tally.directions.len()));
    }
    for required in spec.required_edge_cases {
        if !tally.edge_cases.contains(*required) {
            missing.push(format!("missing_edge_case={required}"));
        }
    }
    for required in required_variants(spec.token) {
        if !tally.variants.contains(required) {
            missing.push(format!("missing_variant={required}"));
        }
    }
    if tally.unresolved_errors() > 0 {
        missing.push(format!("unresolved_errors={}", tally.unresolved_errors()));
    }
    if tally.exact + tally.mismatch != tally.hops_checked {
        missing.push("comparison_accounting_inconsistent".to_string());
    }

    // Any nonzero delta conclusively disproves byte-exactness, even when the sample is otherwise
    // thin. With no disagreement, incomplete breadth/errors remain unknown rather than passing.
    let verdict = if tally.mismatch > 0 || !tally.max_abs_disagreement_wei.is_zero() {
        "keep-tier2-REVM-only"
    } else if missing.is_empty() {
        "PROMOTE-ELIGIBLE"
    } else {
        "needs-more-coverage"
    };
    Assessment {
        byte_exact: verdict == "PROMOTE-ELIGIBLE",
        verdict,
        missing_requirements: missing,
    }
}

fn family_report_row(spec: &FamilySpec, tally: &FamilyTally) -> serde_json::Value {
    let assessment = assess(spec, tally);
    let mut directions: Vec<_> = tally.directions.iter().copied().collect();
    directions.sort_unstable();
    let mut edge_cases: Vec<_> = tally.edge_cases.iter().cloned().collect();
    edge_cases.sort();
    let mut variants: Vec<_> = tally.variants.iter().copied().collect();
    variants.sort_unstable();
    serde_json::json!({
        "authoritative_reference": spec.authoritative_reference,
        "reference_kind": spec.reference_kind,
        "state_pinned": tally.hops_checked > 0,
        "current_status": spec.current_status,
        "status_note": spec.status_note,
        "policy_default": spec.policy_default,
        "harness_supported": spec.harness_supported,
        "corpus_hops_seen": tally.corpus_hops_seen,
        "pools_checked": tally.pools_checked.len(),
        "inputs_checked": tally.inputs_checked.len(),
        "pools_with_bidirectional_input_breadth": tally.pools_with_bidirectional_input_breadth(),
        "min_inputs_per_pool_direction": MIN_INPUTS_PER_POOL_DIRECTION,
        "comparisons": tally.hops_checked,
        "exact": tally.exact,
        "max_abs_disagreement_wei": tally.max_abs_disagreement_wei.to_string(),
        "disagree": tally.mismatch,
        "unresolved_errors": tally.unresolved_errors(),
        "error_breakdown": {
            "snapshot": tally.snapshot_err,
            "kernel": tally.kernel_err,
            "quoter": tally.quoter_err,
        },
        "directions_checked": directions.len(),
        "directions": directions,
        "edge_cases": edge_cases,
        "required_edge_cases": spec.required_edge_cases,
        "variants": variants,
        "required_variants": required_variants(spec.token),
        "missing_requirements": assessment.missing_requirements,
        "byte_exact": assessment.byte_exact,
        "verdict": assessment.verdict,
        "mismatch_samples": tally.mismatch_samples,
        "error_samples": tally.error_samples,
    })
}

fn arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| default.to_string())
}

fn source_head() -> &'static str {
    // The runtime consumer binds the proof to this exact build identity. Reading shell HEAD here
    // could accidentally label a previously compiled binary with a newer checkout.
    env!("ENGINE_GIT_COMMIT")
}

fn source_tree_status() -> &'static str {
    env!("ENGINE_GIT_DIRTY")
}

fn source_manifest_sha256() -> Option<&'static str> {
    option_env!("ENGINE_SOURCE_MANIFEST_SHA256")
        .filter(|digest| digest.len() == 64 && digest.as_bytes().iter().all(u8::is_ascii_hexdigit))
}

fn file_keccak256(path: &str) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("hash {path}: {e}"));
    keccak256(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    let output = std::process::Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|error| format!("run sha256sum for {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "sha256sum failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let digest = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if digest.len() != 64 || !digest.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(format!(
            "sha256sum returned malformed digest for {}",
            path.display()
        ));
    }
    Ok(digest)
}

/// Return `target` relative to the directory containing `declaring_artifact`. Both arguments must
/// be on the same absolute filesystem root. Keeping this pure makes the portability invariant
/// directly unit-testable; `portable_evidence_path` canonicalizes real paths before calling it.
fn lexical_relative_path(
    declaring_artifact: &std::path::Path,
    target: &std::path::Path,
) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let base = declaring_artifact.parent()?;
    if base.is_absolute() != target.is_absolute() {
        return None;
    }
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    if matches!(base_components.first(), Some(Component::Prefix(_)))
        && base_components.first() != target_components.first()
    {
        return None;
    }
    if matches!(base_components.first(), Some(Component::RootDir))
        != matches!(target_components.first(), Some(Component::RootDir))
    {
        return None;
    }
    let common = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = std::path::PathBuf::new();
    for component in &base_components[common..] {
        if matches!(component, Component::Normal(_) | Component::ParentDir) {
            relative.push("..");
        }
    }
    for component in &target_components[common..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

fn portable_evidence_path(
    declaring_artifact: &std::path::Path,
    evidence_path: &std::path::Path,
) -> Result<String, String> {
    let evidence = std::fs::canonicalize(evidence_path).map_err(|error| {
        format!(
            "canonicalize evidence path {}: {error}",
            evidence_path.display()
        )
    })?;
    let parent = declaring_artifact
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let parent = std::fs::canonicalize(parent).map_err(|error| {
        format!(
            "canonicalize declaring directory {}: {error}",
            parent.display()
        )
    })?;
    let anchor = parent.join(declaring_artifact.file_name().ok_or_else(|| {
        format!(
            "artifact path {} has no file name",
            declaring_artifact.display()
        )
    })?);
    let relative = lexical_relative_path(&anchor, &evidence).ok_or_else(|| {
        format!(
            "evidence {} cannot be expressed relative to {}",
            evidence.display(),
            declaring_artifact.display()
        )
    })?;
    if relative.is_absolute() {
        return Err(format!(
            "portable evidence path unexpectedly absolute: {}",
            relative.display()
        ));
    }
    Ok(relative.display().to_string())
}

fn selected_family_token(filter: &str) -> Option<&'static str> {
    FAMILY_SPECS
        .iter()
        .find(|spec| spec.token == filter || spec.corpus_family == filter)
        .map(|spec| spec.token)
}

fn required_family_tokens(
    selected_token: Option<&'static str>,
    explicit: &str,
) -> Vec<&'static str> {
    let mut required = if explicit.trim().is_empty() {
        match selected_token {
            Some(token) => vec![token],
            None => FAMILY_SPECS
                .iter()
                .filter(|spec| spec.harness_supported)
                .map(|spec| spec.token)
                .collect(),
        }
    } else {
        explicit
            .split([',', ' ', '\t'])
            .filter(|token| !token.is_empty())
            .map(|token| {
                selected_family_token(token)
                    .unwrap_or_else(|| panic!("unknown --require-families token {token}"))
            })
            .collect()
    };
    required.sort_unstable();
    required.dedup();
    assert!(
        !required.is_empty(),
        "--require-families must name at least one family"
    );
    if let Some(selected) = selected_token {
        assert!(
            required.iter().all(|required| *required == selected),
            "--require-families must be a subset of the singular --family {selected} selection"
        );
    }
    required
}

fn required_families_pass(required: &[&str], tallies: &HashMap<&'static str, FamilyTally>) -> bool {
    required.iter().all(|token| {
        let spec = family_spec_by_token(token).expect("validated required family");
        assess(spec, tallies.get(token).expect("initialized family tally")).byte_exact
    })
}

fn main() {
    let candidates_path = arg("--candidates", "aws_dryrun_20260709/logs/candidates.jsonl");
    let engine_log_path = arg("--engine-log", "aws_dryrun_20260709/logs/engine.log");
    let family_filter = arg("--family", "all");
    let required_family_filter = arg("--require-families", "");
    let max_per_family: u64 = arg("--max-per-family", "220").parse().unwrap_or(220);
    let sleep_ms: u64 = arg("--sleep-ms", "25").parse().unwrap_or(25);
    let tick_width: i32 = arg("--tick-width", "120").parse().unwrap_or(120);
    let margin_ns: u64 = arg("--boundary-margin-ms", "3000").parse().unwrap_or(3000) * 1_000_000;
    let report_path = arg("--report", "parity_report.json");
    let transcript_default = format!("{report_path}.transcript.jsonl");
    let transcript_path = arg("--transcript", &transcript_default);
    let campaign_default = format!("{report_path}.campaign.json");
    let campaign_manifest_path = arg("--campaign-manifest", &campaign_default);
    let targeted_manifest_arg = arg("--targeted-manifest", "");
    let rpc_url = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL (archive)");

    let th = Throttled::new(&rpc_url, std::time::Duration::from_millis(sleep_ms));
    let chain_id = th
        .verify_chain_id_on_every_endpoint(ARBITRUM_ONE_CHAIN_ID)
        .unwrap_or_else(|error| panic!("archive RPC identity check failed: {error}"));

    eprintln!("loading pin timeline from {engine_log_path} …");
    let timeline = load_pin_timeline(&engine_log_path);
    eprintln!("  {} pin events", timeline.len());
    assert!(
        !timeline.is_empty(),
        "no repin/boot events found — wrong engine.log?"
    );

    eprintln!("loading corpus from {candidates_path} …");
    let records = load_records(&candidates_path);
    eprintln!("  {} records", records.len());

    // ── ANCHOR MODE (--anchor-only true): changed_slots.original vs canonical @ recovered pin.
    // Validates the recv_ns→repin-timeline join AND measures per-record mirror poisoning (the
    // 07-09 run had the mirror ON until fail-safe demotions — originals served by a drifted
    // mirror will NOT match canonical chain state at the true pin).
    if arg("--anchor-only", "false") == "true" {
        let mut checked_records = 0u32;
        let mut clean_records = 0u32;
        let mut slots_total = 0u64;
        let mut slots_match = 0u64;
        let mut worst: Vec<(String, usize, usize)> = Vec::new();
        for rec in records.iter().filter(|r| !r.changed_slots.is_empty()) {
            let Some(pin) = assign_pin(&timeline, rec.recv_ns, margin_ns) else {
                continue;
            };
            // Exclude the in-fork executor's OWN storage (deploy fiction — never on-chain).
            // Token-contract slots holding the executor's balances/allowances remain included
            // (unidentifiable without the keccak preimage), so the dirty rate stays an UPPER
            // bound on mirror poisoning.
            let anchored: Vec<&([u8; 20], [u8; 32], [u8; 32])> = rec
                .changed_slots
                .iter()
                .filter(|(a, _, _)| Some(*a) != rec.mantis_executor)
                .collect();
            if anchored.is_empty() {
                continue;
            }
            let reads: Vec<([u8; 20], [u8; 32])> =
                anchored.iter().map(|(a, sl, _)| (*a, *sl)).collect();
            std::thread::sleep(th.sleep);
            let Ok(vals) = th.client().storage_batch(&reads, BlockTag::Number(pin)) else {
                th.rotate();
                continue;
            };
            let mut m = 0usize;
            let mut errs = 0usize;
            for ((_, _, original), got) in anchored.iter().zip(vals.iter()) {
                match got {
                    Ok(g) => {
                        if g == original {
                            m += 1;
                        }
                    }
                    Err(_) => errs += 1, // RPC error ≠ mismatch: excluded from the denominator
                }
            }
            let resolved = anchored.len() - errs;
            if resolved == 0 {
                continue;
            }
            checked_records += 1;
            slots_total += resolved as u64;
            slots_match += m as u64;
            if m == resolved {
                clean_records += 1;
            } else if worst.len() < 15 {
                worst.push((rec.candidate_id.clone(), m, resolved));
            }
            if checked_records.is_multiple_of(25) {
                eprintln!("  anchored {checked_records} records…");
            }
        }
        println!(
            "
══ STATE-MATCH ANCHOR (changed_slots.original vs canonical @ recovered pin) ══"
        );
        println!(
            "records_checked={checked_records} fully_clean={clean_records}              slot_match={slots_match}/{slots_total} ({:.1}%)",
            100.0 * slots_match as f64 / slots_total.max(1) as f64
        );
        println!(
            "interpretation: clean records validate the pin recovery; dirty ones quantify              mirror poisoning / boundary misassignment (exclude from delta cross-checks)"
        );
        for (id, m, n) in &worst {
            println!("  DIRTY {id}: {m}/{n} slots match");
        }
        return;
    }

    let selected_token = if family_filter == "all" {
        None
    } else {
        Some(selected_family_token(&family_filter).unwrap_or_else(|| {
            panic!("unknown --family {family_filter}; use a policy token or PoolKind name")
        }))
    };
    let selected = |token: &str| selected_token.is_none_or(|wanted| wanted == token);
    let required_families = required_family_tokens(selected_token, &required_family_filter);
    let targeted_corpus_output = arg("--generate-targeted-corpus", "");
    if !targeted_corpus_output.trim().is_empty() {
        write_targeted_corpus(
            &th,
            chain_id,
            &targeted_corpus_output,
            &candidates_path,
            &engine_log_path,
            &records,
            &timeline,
            &required_families,
            margin_ns,
            tick_width,
        )
        .unwrap_or_else(|error| panic!("targeted corpus generation failed: {error}"));
        return;
    }

    let mut tallies: HashMap<&'static str, FamilyTally> = FAMILY_SPECS
        .iter()
        .map(|spec| (spec.token, FamilyTally::default()))
        .collect();
    let mut unknown_family_hops = 0u64;
    for record in &records {
        for hop in &record.hops {
            if let Some(spec) = family_spec_for_corpus(&hop.family) {
                tallies.get_mut(spec.token).unwrap().corpus_hops_seen += 1;
            } else {
                unknown_family_hops += 1;
            }
        }
    }

    let mut per_family_used: HashMap<&'static str, u64> = HashMap::new();
    let mut skipped_boundary = 0u64;
    let mut skipped_family = 0u64;
    let mut seen_keys: HashSet<u64> = HashSet::new();
    let mut pins_with_comparisons: HashSet<u64> = HashSet::new();
    let mut pinned_block_hashes: HashMap<u64, String> = HashMap::new();
    let mut authoritative_targets: HashSet<(&'static str, u64, [u8; 20])> = HashSet::new();
    let mut comparison_transcript: Vec<serde_json::Value> = Vec::new();
    // Secondary cross-check accumulator: (candidate_id, kernel_route_delta, recorded_delta).
    let mut delta_checks: Vec<(String, i128, i128)> = Vec::new();

    // Snapshot cache keyed by (block, pool).
    let mut snap_cache: HashMap<(u64, [u8; 32]), Result<PoolState, String>> = HashMap::new();

    let mut scanned = 0u64;
    for rec in &records {
        scanned += 1;
        if scanned.is_multiple_of(500) {
            let done: u64 = per_family_used.values().sum();
            eprintln!(
                "  scanned {scanned}/{} records, {done} hop-checks, cache {} pools",
                records.len(),
                snap_cache.len()
            );
        }
        // Family targeting: a record contributes when it contains at least one hop of a family
        // still under its quota (and every hop is at least CHAINABLE through the kernel).
        if rec.hops.iter().any(|hop| {
            family_spec_for_corpus(&hop.family).is_none_or(|spec| !spec.harness_supported)
        }) {
            skipped_family += 1;
            continue;
        }
        let wants = rec.hops.iter().any(|hop| {
            let spec = family_spec_for_corpus(&hop.family).expect("supported family spec");
            selected(spec.token)
                && per_family_used.get(spec.token).copied().unwrap_or(0) < max_per_family
        });
        if !wants {
            continue;
        }
        let Some(pin) = assign_pin(&timeline, rec.recv_ns, margin_ns) else {
            skipped_boundary += 1;
            continue;
        };
        // Dedup identical work: same (pin, first pool, input) re-validations add no information.
        let hkey = parity_dedup_key(pin, &rec.hops[0], rec.input_amount);
        if !seen_keys.insert(hkey) {
            continue;
        }

        // Chain the hops: in[0] = recorded input; in[k+1] = kernel out of hop k. Any well-defined
        // input works for PARITY (both sides get the identical input); kernel-chaining keeps the
        // route sizes realistic without a quoter dependency for chaining.
        let mut amount = rec.input_amount;
        let mut route_ok = true;
        for hop in &rec.hops {
            let spec = family_spec_for_corpus(&hop.family).expect("supported family spec");
            let under_quota =
                per_family_used.get(spec.token).copied().unwrap_or(0) < max_per_family;
            let checked = selected(spec.token) && under_quota;
            let authoritative = authoritative_address(hop)
                .unwrap_or_else(|| panic!("{} has no authoritative address", spec.token));
            let pin_hash = if checked {
                if !pinned_block_hashes.contains_key(&pin) {
                    let hash = pinned_block_hash(&th, pin).unwrap_or_else(|error| {
                        panic!("cannot bind comparison to pinned block {pin}: {error}")
                    });
                    pinned_block_hashes.insert(pin, hash);
                }
                authoritative_targets.insert((spec.token, pin, authoritative));
                pinned_block_hashes
                    .get(&pin)
                    .expect("inserted pinned block hash")
                    .as_str()
            } else {
                ""
            };
            let tally = tallies
                .get_mut(spec.token)
                .expect("initialized family tally");
            let state = snap_cache
                .entry((pin, pool_identity(hop)))
                .or_insert_with(|| snapshot_pool(&th, hop, pin, tick_width));
            let state = match state {
                Ok(s) => s,
                Err(e) => {
                    tally.snapshot_err += 1;
                    if checked {
                        comparison_transcript.push(error_transcript_row(
                            &rec.candidate_id,
                            spec.token,
                            chain_id,
                            pin,
                            pin_hash,
                            &pool_identity(hop),
                            &hop.token_in,
                            &hop.token_out,
                            amount,
                            &authoritative,
                            "snapshot",
                            e,
                        ));
                    }
                    if tally.error_samples.len() < 20 {
                        tally.error_samples.push(format!(
                            "{} pin={} pool=0x{} snapshot: {}",
                            rec.candidate_id,
                            pin,
                            hop.pool
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>(),
                            e
                        ));
                    }
                    route_ok = false;
                    break;
                }
            };
            let kernel = match kernel_quote(state, hop, amount) {
                Ok(k) => k,
                Err(e) => {
                    tally.kernel_err += 1;
                    if checked {
                        comparison_transcript.push(error_transcript_row(
                            &rec.candidate_id,
                            spec.token,
                            chain_id,
                            pin,
                            pin_hash,
                            &pool_identity(hop),
                            &hop.token_in,
                            &hop.token_out,
                            amount,
                            &authoritative,
                            "kernel",
                            &e,
                        ));
                    }
                    if tally.error_samples.len() < 20 {
                        tally.error_samples.push(format!(
                            "{} pin={} pool=0x{} input={} kernel: {}",
                            rec.candidate_id,
                            pin,
                            hop.pool
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>(),
                            amount,
                            e
                        ));
                    }
                    route_ok = false;
                    break;
                }
            };
            if checked {
                match quoter_quote(&th, hop, amount, pin) {
                    Ok(onchain) => {
                        tally.hops_checked += 1;
                        *per_family_used.entry(spec.token).or_default() += 1;
                        pins_with_comparisons.insert(pin);
                        tally.pools_checked.insert(pool_identity(hop));
                        let direction = if hop.token_in < hop.token_out {
                            "zero_for_one"
                        } else {
                            "one_for_zero"
                        };
                        tally
                            .inputs_checked
                            .insert((pool_identity(hop), direction, amount));
                        tally.directions.insert(direction);
                        let mut comparison_variants = Vec::new();
                        if spec.token == "uniswap_v4" {
                            comparison_variants.push("hookless_erc20");
                        }
                        if let PoolState::Dodo(dodo) = state {
                            comparison_variants.push(match dodo.pool_class {
                                engine_amm::dodo_v2::DodoPoolClass::Dvm => "dvm",
                                engine_amm::dodo_v2::DodoPoolClass::Dpp => "dpp",
                                engine_amm::dodo_v2::DodoPoolClass::Dsp => "dsp",
                            });
                        }
                        tally.variants.extend(comparison_variants.iter().copied());
                        tally.edge_cases.extend(kernel.edge_cases.iter().cloned());
                        let diff = kernel.amount_out.abs_diff(onchain);
                        comparison_transcript.push(successful_transcript_row(
                            &rec.candidate_id,
                            spec.token,
                            chain_id,
                            pin,
                            pin_hash,
                            &pool_identity(hop),
                            &hop.token_in,
                            &hop.token_out,
                            amount,
                            kernel.amount_out,
                            onchain,
                            &kernel.edge_cases,
                            &comparison_variants,
                            &authoritative,
                        ));
                        tally.max_abs_disagreement_wei = tally.max_abs_disagreement_wei.max(diff);
                        if diff.is_zero() {
                            tally.exact += 1;
                        } else {
                            tally.mismatch += 1;
                            if tally.mismatch_samples.len() < 20 {
                                let diagnostic = if spec.token == "algebra_v19" {
                                    algebra_row_segmented_amount_out(state, hop, amount)
                                        .map(|alternate| {
                                            format!(
                                                " algebra_row_segmented={} alternate_diff={}",
                                                alternate,
                                                alternate.abs_diff(onchain)
                                            )
                                        })
                                        .unwrap_or_else(|error| {
                                            format!(" algebra_row_segmented_error={error}")
                                        })
                                } else {
                                    String::new()
                                };
                                tally.mismatch_samples.push(format!(
                                    "{} pin={} pool=0x{} in={} kernel={} onchain={} diff={}{}",
                                    rec.candidate_id,
                                    pin,
                                    hop.pool
                                        .iter()
                                        .map(|b| format!("{b:02x}"))
                                        .collect::<String>(),
                                    amount,
                                    kernel.amount_out,
                                    onchain,
                                    diff,
                                    diagnostic
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        tally.quoter_err += 1;
                        comparison_transcript.push(error_transcript_row(
                            &rec.candidate_id,
                            spec.token,
                            chain_id,
                            pin,
                            pin_hash,
                            &pool_identity(hop),
                            &hop.token_in,
                            &hop.token_out,
                            amount,
                            &authoritative,
                            "authoritative_reference",
                            &e,
                        ));
                        if tally.error_samples.len() < 20 {
                            tally.error_samples.push(format!(
                                "{} pin={} pool=0x{} input={} quoter: {}",
                                rec.candidate_id,
                                pin,
                                hop.pool
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<String>(),
                                amount,
                                e
                            ));
                        }
                    }
                }
            }
            amount = kernel.amount_out;
        }
        // Secondary: recorded REVM success deltas vs the state-matched kernel chain.
        if route_ok {
            if let Some(delta) = rec.balance_delta_wei {
                let kernel_delta = i128::try_from(amount.saturating_to::<u128>())
                    .unwrap_or(i128::MAX)
                    - i128::try_from(rec.input_amount.saturating_to::<u128>()).unwrap_or(0);
                delta_checks.push((rec.candidate_id.clone(), kernel_delta, delta));
            }
        }
    }

    // ── report ──
    println!("\n══ STATE-MATCHED PARITY (kernel vs chain quoter @ recovered pin) ══");
    println!(
        "records={} skipped_boundary={} skipped_unsupported_family={}",
        records.len(),
        skipped_boundary,
        skipped_family
    );
    let gate_pass = required_families_pass(&required_families, &tallies);
    for spec in &FAMILY_SPECS {
        let t = tallies.get(spec.token).expect("initialized family tally");
        let assessment = assess(spec, t);
        println!(
            "{:>20}: pools={:<3} inputs={:<4} comparisons={:<5} exact={:<5} disagree={:<4} \
             max_abs_wei={} directions={} unresolved={}  → {}",
            spec.token,
            t.pools_checked.len(),
            t.inputs_checked.len(),
            t.hops_checked,
            t.exact,
            t.mismatch,
            t.max_abs_disagreement_wei,
            t.directions.len(),
            t.unresolved_errors(),
            assessment.verdict,
        );
        for s in &t.mismatch_samples {
            println!("      MISMATCH {s}");
        }
        for requirement in &assessment.missing_requirements {
            println!("      MISSING {requirement}");
        }
    }
    if !delta_checks.is_empty() {
        let within_1pct = delta_checks
            .iter()
            .filter(|(_, k, r)| {
                let denom = r.abs().max(1);
                ((k - r).abs() as f64 / denom as f64) < 0.01
            })
            .count();
        println!(
            "\nsecondary (recorded REVM success deltas, mirror-poisoning expected): \
             {}/{} within 1% of state-matched kernel chain",
            within_1pct,
            delta_checks.len()
        );
    }
    let families: serde_json::Map<String, serde_json::Value> = FAMILY_SPECS
        .iter()
        .map(|spec| {
            (
                spec.token.to_string(),
                family_report_row(
                    spec,
                    tallies.get(spec.token).expect("initialized family tally"),
                ),
            )
        })
        .collect();
    let report_file = std::path::Path::new(&report_path);
    if let Some(parent) = report_file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!("create report directory {}: {error}", parent.display())
        });
    }
    write_jsonl(
        std::path::Path::new(&transcript_path),
        &comparison_transcript,
    )
    .unwrap_or_else(|error| panic!("write comparison transcript: {error}"));

    let chain = chain_provenance_value(
        &th,
        chain_id,
        &pins_with_comparisons,
        &authoritative_targets,
    )
    .unwrap_or_else(|error| panic!("capture chain provenance: {error}"));
    assert!(
        chain_provenance_matches_cached_blocks(&chain, &pinned_block_hashes),
        "pinned block hash changed between comparison and report binding"
    );
    let source_manifest = source_manifest_sha256().unwrap_or_else(|| {
        panic!("strict evidence build must embed ENGINE_SOURCE_MANIFEST_SHA256")
    });
    let producer_binary =
        std::env::current_exe().unwrap_or_else(|error| panic!("locate producer binary: {error}"));
    let producer_binary_sha256 = file_sha256(&producer_binary)
        .unwrap_or_else(|error| panic!("hash producer binary: {error}"));
    let corpus_keccak256 = file_keccak256(&candidates_path);
    let state_pin_source_keccak256 = file_keccak256(&engine_log_path);
    let corpus_path_portable =
        portable_evidence_path(report_file, std::path::Path::new(&candidates_path))
            .unwrap_or_else(|error| panic!("portable corpus path: {error}"));
    let state_pin_source_portable =
        portable_evidence_path(report_file, std::path::Path::new(&engine_log_path))
            .unwrap_or_else(|error| panic!("portable state-pin path: {error}"));
    let transcript_path_portable =
        portable_evidence_path(report_file, std::path::Path::new(&transcript_path))
            .unwrap_or_else(|error| panic!("portable transcript path: {error}"));
    let transcript_sha256 = file_sha256(std::path::Path::new(&transcript_path))
        .unwrap_or_else(|error| panic!("hash transcript: {error}"));
    let transcript_keccak256 = file_keccak256(&transcript_path);
    let inferred_targeted_manifest = format!("{candidates_path}.manifest.json");
    let targeted_manifest_path = if targeted_manifest_arg.trim().is_empty() {
        std::path::Path::new(&inferred_targeted_manifest)
            .is_file()
            .then_some(inferred_targeted_manifest)
    } else {
        assert!(
            std::path::Path::new(&targeted_manifest_arg).is_file(),
            "--targeted-manifest {} does not exist",
            targeted_manifest_arg
        );
        Some(targeted_manifest_arg)
    };
    let targeted_manifest_binding = targeted_manifest_path.as_ref().map(|path| {
        serde_json::json!({
            "path": portable_evidence_path(report_file, std::path::Path::new(path))
                .unwrap_or_else(|error| panic!("portable targeted manifest path: {error}")),
            "sha256": file_sha256(std::path::Path::new(path))
                .unwrap_or_else(|error| panic!("hash targeted manifest: {error}")),
            "keccak256": file_keccak256(path),
        })
    });
    let promotion_eligible_families: Vec<&str> = FAMILY_SPECS
        .iter()
        .filter(|spec| {
            assess(
                spec,
                tallies.get(spec.token).expect("initialized family tally"),
            )
            .byte_exact
        })
        .map(|spec| spec.token)
        .collect();
    let required_family_count = required_families.len();
    let promotion_eligible_family_count = promotion_eligible_families.len();
    let run_id = std::env::var("RUN_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let report = serde_json::json!({
        "schema_version": 2,
        "strict_byte_exact": true,
        "generated_by": "state_matched_parity",
        "run_id": run_id,
        "source_head": source_head(),
        "source_tree_status": source_tree_status(),
        "source_manifest_sha256": source_manifest,
        "producer_binary_sha256": producer_binary_sha256.clone(),
        "chain": chain.clone(),
        "corpus": corpus_path_portable,
        "corpus_keccak256": corpus_keccak256,
        "state_pin_source": state_pin_source_portable,
        "state_pin_source_keccak256": state_pin_source_keccak256,
        "comparison_transcript": transcript_path_portable,
        "comparison_transcript_sha256": transcript_sha256,
        "comparison_transcript_keccak256": transcript_keccak256,
        "comparison_transcript_rows": comparison_transcript.len(),
        "targeted_corpus_manifest": targeted_manifest_binding,
        "pin_events": timeline.len(),
        "pins_with_comparisons": pins_with_comparisons.len(),
        "selected_family": selected_token.unwrap_or("all"),
        "required_families": required_families,
        "required_family_count": required_family_count,
        "promotion_eligible_families": promotion_eligible_families,
        "promotion_eligible_family_count": promotion_eligible_family_count,
        "coverage_requirements": {
            "min_pools": MIN_POOLS,
            "min_distinct_pool_direction_inputs": MIN_DIRECTIONAL_INPUTS,
            "min_inputs_per_pool_direction": MIN_INPUTS_PER_POOL_DIRECTION,
            "min_comparisons": MIN_COMPARISONS,
            "directions": 2,
            "edge_cases_are_topology_specific": true,
        },
        "skipped_boundary": skipped_boundary,
        "skipped_unsupported_family": skipped_family,
        "unknown_family_hops": unknown_family_hops,
        "families": families,
        "gate_pass": gate_pass,
    });
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap())
        .unwrap_or_else(|e| panic!("report write failed for {report_path}: {e}"));

    let campaign_file = std::path::Path::new(&campaign_manifest_path);
    if let Some(parent) = campaign_file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!("create campaign directory {}: {error}", parent.display())
        });
    }
    let mut artifacts = serde_json::Map::new();
    for (name, path) in [
        ("strict_report", report_path.as_str()),
        ("targeted_corpus", candidates_path.as_str()),
        ("state_pin_source", engine_log_path.as_str()),
        ("comparison_transcript", transcript_path.as_str()),
    ] {
        artifacts.insert(
            name.to_string(),
            campaign_artifact_value(campaign_file, std::path::Path::new(path))
                .unwrap_or_else(|error| panic!("bind campaign artifact {name}: {error}")),
        );
    }
    if let Some(path) = targeted_manifest_path.as_ref() {
        artifacts.insert(
            "targeted_corpus_manifest".to_string(),
            campaign_artifact_value(campaign_file, std::path::Path::new(path)).unwrap_or_else(
                |error| panic!("bind campaign artifact targeted_corpus_manifest: {error}"),
            ),
        );
    }
    let campaign = campaign_manifest_value(
        source_head().to_string(),
        source_tree_status(),
        source_manifest.to_string(),
        producer_binary_sha256,
        chain,
        serde_json::Value::Object(artifacts),
    );
    std::fs::write(
        campaign_file,
        serde_json::to_vec_pretty(&campaign).expect("serialize campaign manifest"),
    )
    .unwrap_or_else(|error| {
        panic!(
            "campaign manifest write failed for {}: {error}",
            campaign_file.display()
        )
    });
    println!("\nreport → {report_path}   gate_pass={gate_pass}");
    println!("campaign → {campaign_manifest_path}");
    std::process::exit(if gate_pass { 0 } else { 1 });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promotion_grade_tally() -> FamilyTally {
        let mut tally = FamilyTally::default();
        tally.hops_checked = 36;
        tally.exact = 36;
        for pool_byte in 1..=3 {
            let pool = [pool_byte; 32];
            tally.pools_checked.insert(pool);
            for direction in ["zero_for_one", "one_for_zero"] {
                for amount in 1..=6 {
                    tally
                        .inputs_checked
                        .insert((pool, direction, U256::from(amount)));
                }
            }
        }
        tally.directions.insert("zero_for_one");
        tally.directions.insert("one_for_zero");
        for edge in [
            "dust",
            "nominal",
            "extreme",
            "near_tick_boundary",
            "multi_tick_crossing",
        ] {
            tally.edge_cases.insert(edge.to_string());
        }
        tally
    }

    #[test]
    fn family_specs_cover_the_exact_policy_matrix() {
        let got: Vec<_> = FAMILY_SPECS.iter().map(|spec| spec.token).collect();
        assert_eq!(got.len(), 18);
        assert_eq!(
            got,
            vec![
                "uniswap_v2",
                "sushi_v2",
                "camelot_v2",
                "uniswap_v3",
                "sushi_v3",
                "pancake_v3",
                "algebra_v19",
                "curve_stableswap",
                "curve_cryptoswap",
                "trader_joe_lb",
                "maverick_v2",
                "fluid",
                "dodo",
                "kyber_elastic",
                "solidly_v3",
                "uniswap_v4",
                "balancer_stable",
                "balancer_weighted",
            ]
        );
        let unique: std::collections::HashSet<_> = got.iter().copied().collect();
        assert_eq!(unique.len(), got.len(), "family tokens must be unique");
        assert!(FAMILY_SPECS
            .iter()
            .all(|spec| !spec.authoritative_reference.trim().is_empty()));
        let promoted: Vec<_> = FAMILY_SPECS
            .iter()
            .filter(|spec| spec.current_status == "promoted")
            .map(|spec| spec.token)
            .collect();
        assert_eq!(promoted, ["uniswap_v3", "pancake_v3", "uniswap_v4"]);
        assert_eq!(
            FAMILY_SPECS
                .iter()
                .filter(|spec| spec.policy_default)
                .count(),
            8
        );
    }

    #[test]
    fn dodo_variant_comes_from_the_pools_abi_version_string() {
        let mut response = Vec::new();
        response.extend_from_slice(&word_u256(U256::from(32u8)));
        response.extend_from_slice(&word_u256(U256::from(9u8)));
        response.extend_from_slice(b"DPP 1.0.0");
        response.resize(96, 0);
        assert_eq!(
            dodo_class_from_version(&response),
            Some(engine_amm::dodo_v2::DodoPoolClass::Dpp)
        );
        assert!(dodo_class_from_version(&[0u8; 31]).is_none());
    }

    #[test]
    fn exact_zero_with_full_breadth_is_promotion_eligible() {
        let spec = family_spec_by_token("uniswap_v3").unwrap();
        let tally = promotion_grade_tally();
        let assessment = assess(spec, &tally);
        assert!(assessment.byte_exact);
        assert_eq!(assessment.verdict, "PROMOTE-ELIGIBLE");
        assert!(assessment.missing_requirements.is_empty());
    }

    #[test]
    fn one_wei_is_a_disagreement_without_tolerance() {
        let spec = family_spec_by_token("uniswap_v3").unwrap();
        let mut tally = promotion_grade_tally();
        tally.exact -= 1;
        tally.mismatch = 1;
        tally.max_abs_disagreement_wei = U256::from(1u8);
        let assessment = assess(spec, &tally);
        assert!(!assessment.byte_exact);
        assert_eq!(assessment.verdict, "keep-tier2-REVM-only");
    }

    #[test]
    fn zero_data_errors_and_thin_breadth_never_exit_green() {
        let spec = family_spec_by_token("uniswap_v3").unwrap();
        let empty = assess(spec, &FamilyTally::default());
        assert!(!empty.byte_exact);
        assert_eq!(empty.verdict, "needs-more-coverage");

        let mut with_error = promotion_grade_tally();
        with_error.quoter_err = 1;
        let errored = assess(spec, &with_error);
        assert!(!errored.byte_exact);
        assert_eq!(errored.verdict, "needs-more-coverage");

        let mut one_direction = promotion_grade_tally();
        one_direction.directions.remove("one_for_zero");
        let thin = assess(spec, &one_direction);
        assert!(!thin.byte_exact);
        assert_eq!(thin.verdict, "needs-more-coverage");
    }

    #[test]
    fn report_row_exposes_the_strict_consumer_schema() {
        let spec = family_spec_by_token("uniswap_v3").unwrap();
        let row = family_report_row(spec, &promotion_grade_tally());
        for field in [
            "authoritative_reference",
            "reference_kind",
            "state_pinned",
            "current_status",
            "pools_checked",
            "inputs_checked",
            "comparisons",
            "directions_checked",
            "max_abs_disagreement_wei",
            "disagree",
            "unresolved_errors",
            "edge_cases",
            "variants",
            "byte_exact",
            "verdict",
        ] {
            assert!(!row[field].is_null(), "missing report field {field}");
        }
        assert_eq!(row["max_abs_disagreement_wei"], "0");
        assert_eq!(row["byte_exact"], true);
        assert_eq!(row["verdict"], "PROMOTE-ELIGIBLE");

        let head = source_head();
        assert_eq!(head.len(), 40, "ENGINE_GIT_COMMIT must be a full SHA-1");
        assert!(head.as_bytes().iter().all(u8::is_ascii_hexdigit));
    }

    #[test]
    fn gate_exit_is_scoped_to_the_exact_requested_families() {
        let mut tallies: HashMap<&'static str, FamilyTally> = FAMILY_SPECS
            .iter()
            .map(|spec| (spec.token, FamilyTally::default()))
            .collect();
        tallies.insert("uniswap_v3", promotion_grade_tally());

        assert!(required_families_pass(&["uniswap_v3"], &tallies));
        assert!(
            !required_families_pass(&["uniswap_v3", "pancake_v3"], &tallies),
            "an unproven requested family must make the process exit non-zero"
        );
    }

    #[test]
    fn all_mode_defaults_to_harness_supported_families_not_unimplemented_rows() {
        let required = required_family_tokens(None, "");
        let expected: Vec<_> = FAMILY_SPECS
            .iter()
            .filter(|spec| spec.harness_supported)
            .map(|spec| spec.token)
            .collect();
        let mut got = required;
        got.sort_unstable();
        let mut expected = expected;
        expected.sort_unstable();
        assert_eq!(got, expected);
        assert!(!got.contains(&"uniswap_v2"), "unimplemented rows remain visible but do not make an all-supported campaign impossible");
    }

    #[test]
    fn generic_edge_labels_are_derived_from_the_transition_not_trusted_from_corpus_text() {
        assert_eq!(mechanical_size_edge_cases(U256::from(1u8), 0), ["dust"]);
        assert_eq!(mechanical_size_edge_cases(U256::from(2u8), 0), ["nominal"]);
        assert_eq!(
            mechanical_size_edge_cases(U256::from(10_000u64), 2),
            ["extreme"]
        );
    }

    #[test]
    fn targeted_amount_search_mechanically_covers_cl_edges_with_six_distinct_inputs() {
        let ticks = [-180, -120, -60, 60, 120, 180]
            .into_iter()
            .map(|tick| TickInfo {
                tick,
                liquidity_net: 0,
            })
            .collect();
        let state = PoolState::V3 {
            slot0: V3Slot0 {
                sqrt_price_x96: engine_amm::sqrt_price_at_tick(0).unwrap(),
                tick: 0,
                fee_pips: 500,
                liquidity: 1_000_000_000_000_000_000,
                tick_spacing: 60,
            },
            ticks,
        };
        let hop = HopMeta {
            family: "UniswapV3".to_string(),
            pool: [7u8; 20],
            token_in: [1u8; 20],
            token_out: [2u8; 20],
            fee: 500,
            tick_spacing: 60,
            pool_id: None,
            state_view: None,
            parity_edge_cases: Vec::new(),
        };

        let amounts = targeted_v3_amounts(&state, &hop).expect("targeted amount search");
        assert_eq!(amounts.len(), 6);
        assert_eq!(amounts.iter().copied().collect::<HashSet<_>>().len(), 6);
        let mut edges = HashSet::new();
        for amount in amounts {
            edges.extend(kernel_quote(&state, &hop, amount).unwrap().edge_cases);
        }
        for required in CL_EDGE_CASES {
            assert!(
                edges.contains(*required),
                "missing mechanical edge {required}"
            );
        }
    }

    #[test]
    fn parity_dedup_identity_keeps_opposite_pool_directions_distinct() {
        let mut reverse = HopMeta {
            family: "UniswapV3".to_string(),
            pool: [9u8; 20],
            token_in: [1u8; 20],
            token_out: [2u8; 20],
            fee: 500,
            tick_spacing: 60,
            pool_id: None,
            state_view: None,
            parity_edge_cases: Vec::new(),
        };
        let forward = reverse.clone();
        std::mem::swap(&mut reverse.token_in, &mut reverse.token_out);
        assert_ne!(
            parity_dedup_key(123, &forward, U256::from(1u8)),
            parity_dedup_key(123, &reverse, U256::from(1u8)),
            "opposite directions are separate required proof observations"
        );
    }

    #[test]
    fn generated_corpus_pin_is_inside_a_real_recovered_pin_interval() {
        let timeline = [(100u64, 10u64), (1_000, 11), (1_100, 12)];
        let (recv_ns, pin) = select_target_pin(&timeline, 100).expect("usable pin interval");
        assert_eq!(pin, 10);
        assert!(recv_ns >= 200 && recv_ns <= 900);
        assert_eq!(assign_pin(&timeline, recv_ns, 100), Some(pin));
    }

    #[test]
    fn portable_pin_timeline_round_trips_without_the_original_engine_log() {
        let expected = vec![(100u64, 10u64), (1_000, 11), (1_100, 12)];
        let path = std::env::temp_dir().join(format!(
            "mantis-parity-pin-timeline-{}.jsonl",
            std::process::id()
        ));
        write_portable_pin_timeline(&path, &expected).expect("write portable pin timeline");
        let actual = load_pin_timeline(path.to_str().expect("UTF-8 temp path"));
        std::fs::remove_file(&path).expect("remove test pin timeline");
        assert_eq!(actual, expected);
    }

    #[test]
    fn algebra_diagnostic_segments_at_raw_256_tick_row_boundaries() {
        assert_eq!(
            algebra_row_boundaries(-600, 600, true),
            vec![-512, -256, 0, 256, 512]
        );
        assert_eq!(
            algebra_row_boundaries(-600, 600, false),
            vec![-513, -257, -1, 255, 511]
        );
    }

    #[test]
    fn evidence_paths_are_portable_relative_to_the_declaring_artifact() {
        let declared = lexical_relative_path(
            std::path::Path::new("/repo/evidence/final/report.json"),
            std::path::Path::new("/repo/evidence/final/transcript.jsonl"),
        )
        .expect("same filesystem");
        assert_eq!(declared, std::path::PathBuf::from("transcript.jsonl"));
        assert!(!declared.is_absolute());

        let sibling = lexical_relative_path(
            std::path::Path::new("/repo/evidence/final/report.json"),
            std::path::Path::new("/repo/source/pins.jsonl"),
        )
        .expect("same filesystem");
        assert_eq!(sibling, std::path::PathBuf::from("../../source/pins.jsonl"));
        assert!(!sibling.is_absolute());
    }

    #[test]
    fn generated_candidate_ids_bind_the_full_pool_identity() {
        let mut first = [0u8; 32];
        first[31] = 1;
        let mut second = [0u8; 32];
        second[31] = 2;
        let a = generated_candidate_id("uniswap_v3", &first, "zero_for_one", 0);
        let b = generated_candidate_id("uniswap_v3", &second, "zero_for_one", 0);
        assert_ne!(a, b);
        assert!(a.contains(&hex_bytes(&first)[2..]));
        assert!(b.contains(&hex_bytes(&second)[2..]));
    }

    #[test]
    fn offline_evidence_schema_binds_chain_binary_transcript_and_artifacts() {
        let transcript = successful_transcript_row(
            "candidate-1",
            "uniswap_v3",
            42161,
            482_086_269,
            &format!("0x{}", "11".repeat(32)),
            &[7u8; 32],
            &[1u8; 20],
            &[2u8; 20],
            U256::from(3u8),
            U256::from(4u8),
            U256::from(4u8),
            &["nominal".to_string()],
            &[],
            &[9u8; 20],
        );
        for field in [
            "candidate_id",
            "family",
            "chain_id",
            "pin_block",
            "pin_block_hash",
            "pool_identity",
            "amount_in",
            "kernel_amount_out",
            "authoritative_amount_out",
            "authoritative_address",
            "exact",
            "mechanical_variants",
        ] {
            assert!(!transcript[field].is_null(), "missing transcript {field}");
        }

        let campaign = campaign_manifest_value(
            "11".repeat(20),
            "dirty",
            "22".repeat(32),
            "33".repeat(32),
            serde_json::json!({"chain_id": 42161, "pinned_blocks": [], "quoters": []}),
            serde_json::json!({
                "strict_report": {"path": "report.json", "sha256": "44".repeat(32)},
                "targeted_corpus": {"path": "corpus.jsonl", "sha256": "55".repeat(32)},
                "targeted_corpus_manifest": {"path": "corpus.jsonl.manifest.json", "sha256": "66".repeat(32)},
                "state_pin_source": {"path": "pins.jsonl", "sha256": "77".repeat(32)},
                "comparison_transcript": {"path": "transcript.jsonl", "sha256": "88".repeat(32)}
            }),
        );
        assert_eq!(campaign["schema"], "mantis_parity_campaign_v1");
        assert_eq!(campaign["chain"]["chain_id"], 42161);
        assert_eq!(campaign["producer_binary_sha256"], "33".repeat(32));
        for required in [
            "strict_report",
            "targeted_corpus",
            "targeted_corpus_manifest",
            "state_pin_source",
            "comparison_transcript",
        ] {
            assert!(!campaign["artifacts"][required].is_null());
        }
    }

    #[test]
    fn report_chain_binding_must_match_the_hash_used_by_each_comparison() {
        let mut expected = HashMap::new();
        expected.insert(482_086_269, format!("0x{}", "11".repeat(32)));
        let matching = serde_json::json!({
            "pinned_blocks": [{
                "number": 482_086_269u64,
                "hash": format!("0x{}", "11".repeat(32)),
            }]
        });
        assert!(chain_provenance_matches_cached_blocks(&matching, &expected));
        let different = serde_json::json!({
            "pinned_blocks": [{
                "number": 482_086_269u64,
                "hash": format!("0x{}", "22".repeat(32)),
            }]
        });
        assert!(!chain_provenance_matches_cached_blocks(
            &different, &expected
        ));
    }
}
