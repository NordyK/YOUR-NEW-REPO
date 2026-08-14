//! WI-13 LEGACY DIAGNOSTIC — where each retired hardcoded gas constant actually survives.
//!
//! §3.4 retired-constant rule, AMENDED (fix round, codex NEEDS-FIX-FL14 correction 5). The
//! original claim — "old `16`/`140` constants remain ONLY in explicitly named legacy
//! diagnostic tests" — was FALSE and is replaced by this honest contract (mirrored in the
//! `engine_gas::authority` module docs):
//!
//! * `16`  — the pre-Dia `tokenGasUnits` — survives in the INERT boot-default
//!   `GasSnapshot` only. The boot default's `captured_at_ms == 0` sentinel is permanently
//!   Stale, and with the authority refresher as the engine's SOLE gas writer nothing can
//!   ever stamp it fresh: every fresh snapshot is a COMPLETE publication carrying the LIVE
//!   `getPricesInArbGasWithAggregator` value at the snapshot block.
//! * `140` — the empirical per-tx envelope pad — is a LIVE, POLICY-BOUND production
//!   default: `authority::DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES` flows through
//!   `GasAuthorityConfig::legacy_static_overhead_bytes` onto EVERY complete publication,
//!   is logged at refresher boot, and each published snapshot carries the value actually
//!   used (`static_overhead_bytes` = per-publication evidence). It has no on-chain source;
//!   over-stating a subtracted cost is safe by direction.
//! * `100_000` — the FL-14 fail-open floor the L1 leg used to substitute FRESH on a failed
//!   fetch — has NO production successor: a failed component keeps the old value and the
//!   old age.

use engine_gas::authority::{GasAuthorityConfig, DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES};
use engine_gas::{GasError, GasOracle, GasSnapshot};

/// Historical values, recorded for diagnostics/archaeology only.
const LEGACY_TOKEN_GAS_UNITS: u32 = 16;
const LEGACY_STATIC_OVERHEAD_BYTES: u32 = 140;
const LEGACY_FL14_L1_FLOOR_WEI: u128 = 100_000;

/// The boot-default `GasSnapshot` still ships the legacy placeholders — but it is
/// PERMANENTLY stale (`captured_at_ms == 0` is the never-refreshed sentinel), so they can
/// never gate a submission: only a COMPLETE authority refresh publishes a fresh snapshot,
/// and that refresh carries the live values. With the authority as the SOLE gas writer
/// (fix round; pinned by `engine`'s `wi13_gas_sole_writer_e2e`), no other worker can ever
/// stamp the sentinel fresh.
#[test]
fn legacy_hardcoded_16_and_140_survive_only_as_boot_placeholders_and_never_gate() {
    let default = GasSnapshot::default();
    assert_eq!(default.l1_gas_per_data_byte, LEGACY_TOKEN_GAS_UNITS);
    assert_eq!(default.static_overhead_bytes, LEGACY_STATIC_OVERHEAD_BYTES);
    assert_eq!(
        DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES,
        LEGACY_STATIC_OVERHEAD_BYTES,
        "the explicit config default replaces the buried 140 hardcode"
    );
    // §3.4 amended: the pad is policy-bound through the production config constructor and
    // published as per-snapshot evidence (`static_overhead_bytes`) — not a scattered
    // hardcode. The refresher always builds `arbitrum_one`.
    assert_eq!(
        GasAuthorityConfig::arbitrum_one([0u8; 20]).legacy_static_overhead_bytes,
        DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES,
        "production config must carry the pad as an explicit, policy-bound value"
    );

    // The placeholder snapshot is ALWAYS rejected by the freshness gate.
    let oracle = GasOracle::new(default, u64::MAX);
    assert!(
        matches!(oracle.load_fresh(), Err(GasError::Stale)),
        "the never-refreshed placeholder must be Stale under any max_age"
    );
    // And the typed authority does not exist at all before a complete refresh.
    assert!(oracle.authority().is_none());
    assert!(oracle.submit_authority_view().is_none());
}

/// The FL-14 floor value, recorded so its retirement is visible: it must not resurface as
/// any default the boot snapshot could publish.
#[test]
fn legacy_fl14_floor_value_is_archived_not_a_default() {
    let default = GasSnapshot::default();
    assert_ne!(
        default.l1_base_fee_per_gas_wei, LEGACY_FL14_L1_FLOOR_WEI,
        "the boot default must not carry the retired floor"
    );
    assert_eq!(
        default.l1_base_fee_per_gas_wei, 0,
        "boot default L1 fee is 0 — and the l1>0 pre-broadcast check plus the Stale \
         sentinel keep it from ever gating"
    );
}
