//! §10.5 confidence chain: five multiplicative factors, must clear 0.95.

/// Floor below which the engine rejects candidates outright.
pub const CONFIDENCE_FLOOR: f32 = 0.95;

/// The five confidence factors. Each is a fixed-point value in `[0, 1]`. We store them as
/// `f32` for arithmetic simplicity on the hot path; the calculation is five multiplies and
/// a compare, which is ~5 cycles even with floating point.
#[derive(Debug, Clone, Copy)]
pub struct ConfidenceFactors {
    /// State freshness factor.
    pub state_freshness: f32,
    /// Calldata decode confidence factor.
    pub decode_confidence: f32,
    /// Token allowlist / FoT / blacklist factor.
    pub token_quality: f32,
    /// Route heat / historical hit-rate factor.
    pub path_heat: f32,
    /// Inclusion probability factor.
    pub inclusion: f32,
}

impl ConfidenceFactors {
    /// Default = all factors at 1.0.
    pub fn full() -> Self {
        Self {
            state_freshness: 1.0,
            decode_confidence: 1.0,
            token_quality: 1.0,
            path_heat: 1.0,
            inclusion: 1.0,
        }
    }
}

/// Output of the confidence chain.
#[derive(Debug, Clone, Copy)]
pub struct Confidence {
    /// Final scalar confidence value.
    pub value: f32,
    /// Whether the candidate passes the floor.
    pub passes: bool,
}

impl Confidence {
    /// Compute the chain.
    #[inline]
    pub fn from_factors(f: ConfidenceFactors) -> Self {
        let v =
            f.state_freshness * f.decode_confidence * f.token_quality * f.path_heat * f.inclusion;
        Self {
            value: v,
            passes: v >= CONFIDENCE_FLOOR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_factors_pass() {
        let c = Confidence::from_factors(ConfidenceFactors::full());
        assert!(c.passes);
        assert!((c.value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fractional_decode_fails_floor() {
        let f = ConfidenceFactors {
            decode_confidence: 0.9,
            ..ConfidenceFactors::full()
        };
        let c = Confidence::from_factors(f);
        assert!(!c.passes);
        assert!((c.value - 0.9).abs() < 1e-6);
    }

    #[test]
    fn boundary_just_passes() {
        let f = ConfidenceFactors {
            state_freshness: 0.95,
            ..ConfidenceFactors::full()
        };
        let c = Confidence::from_factors(f);
        assert!(c.passes);
    }
}
