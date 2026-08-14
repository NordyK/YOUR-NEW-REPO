//! Variant submission policy (§10.11).

/// Decision: how many variants to submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantPolicy {
    /// EV threshold at which a candidate is submitted at all.
    pub submit_threshold_hot: i128,
    /// Multiplier above hot threshold required to submit a second variant.
    pub double_variant_multiplier: u32,
    /// Multiplier above hot threshold required to submit a third variant.
    pub triple_variant_multiplier: u32,
}

impl Default for VariantPolicy {
    fn default() -> Self {
        Self {
            submit_threshold_hot: 0,
            double_variant_multiplier: 3,
            triple_variant_multiplier: 10,
        }
    }
}

/// Compute the variant count for a candidate.
///
/// Returns 0 if EV is below the hot threshold, 1 for hot, 2 for `3×`+, 3 for `10×`+.
/// Variant overrides require historical competition data (passed in as a boolean) per
/// §10.11.
pub fn decide_variants(ev_wei: i128, historical_competition: bool, policy: &VariantPolicy) -> u8 {
    // AUD-VARIANTS-THRESH: a non-positive `submit_threshold_hot` (the unconfigured default is 0)
    // must NOT authorize break-even or multi-variant broadcasts. With threshold ≤ 0 the `threshold *
    // mult` escalation gates collapse to `ev >= 0`, which previously let a zero-EV candidate emit up
    // to 3 live variants. Require strictly-positive EV for a single variant and disable escalation
    // until production sets a positive threshold.
    if policy.submit_threshold_hot <= 0 {
        return u8::from(ev_wei > 0);
    }
    if ev_wei < policy.submit_threshold_hot {
        return 0;
    }
    if ev_wei
        >= policy
            .submit_threshold_hot
            .saturating_mul(policy.triple_variant_multiplier as i128)
        && historical_competition
    {
        return 3;
    }
    if ev_wei
        >= policy
            .submit_threshold_hot
            .saturating_mul(policy.double_variant_multiplier as i128)
        && historical_competition
    {
        return 2;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_submit_below_threshold() {
        let p = VariantPolicy {
            submit_threshold_hot: 100,
            ..VariantPolicy::default()
        };
        assert_eq!(decide_variants(50, true, &p), 0);
    }

    #[test]
    fn one_variant_default() {
        let p = VariantPolicy {
            submit_threshold_hot: 100,
            ..VariantPolicy::default()
        };
        assert_eq!(decide_variants(200, false, &p), 1);
    }

    #[test]
    fn two_variants_with_competition() {
        let p = VariantPolicy {
            submit_threshold_hot: 100,
            ..VariantPolicy::default()
        };
        assert_eq!(decide_variants(400, true, &p), 2);
    }

    #[test]
    fn three_variants_with_competition() {
        let p = VariantPolicy {
            submit_threshold_hot: 100,
            ..VariantPolicy::default()
        };
        assert_eq!(decide_variants(1_500, true, &p), 3);
    }

    #[test]
    fn nonpositive_threshold_never_broadcasts_breakeven_or_multivariant() {
        // AUD-VARIANTS-THRESH: with the default (0) threshold, a break-even candidate (ev==0) must
        // emit ZERO variants, and a positive-EV candidate must emit AT MOST ONE — never the 2×/3×
        // escalation (whose `threshold*mult` is also 0, which previously let ev>=0 emit 3 variants).
        let p = VariantPolicy::default(); // submit_threshold_hot == 0
        assert_eq!(
            decide_variants(0, true, &p),
            0,
            "break-even must not broadcast"
        );
        assert_eq!(
            decide_variants(-1, true, &p),
            0,
            "negative EV must not broadcast"
        );
        assert_eq!(
            decide_variants(1_000_000, true, &p),
            1,
            "positive EV with a non-positive threshold must cap at ONE variant"
        );
    }
}
