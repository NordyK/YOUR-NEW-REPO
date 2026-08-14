//! Kill switches per §14.1.
//!
//! Each level (global, strategy, pool, token, gas, feed, inclusion, nonce, AMM-family)
//! owns an `AtomicBool`. Hot threads check the cheap atomic before submitting; background
//! threads flip the atomic when an observed condition exceeds the threshold.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crossbeam_utils::CachePadded;

/// One named kill switch. Cheap atomic read on the hot path.
pub struct KillSwitch {
    flag: CachePadded<AtomicBool>,
    /// Monotonic timestamp when this switch was tripped (ns since unix epoch). Zero if
    /// never tripped.
    tripped_at_ns: CachePadded<AtomicU64>,
}

impl KillSwitch {
    /// New (closed) switch.
    pub const fn new() -> Self {
        Self {
            flag: CachePadded::new(AtomicBool::new(false)),
            tripped_at_ns: CachePadded::new(AtomicU64::new(0)),
        }
    }

    /// Returns `true` if the switch is currently armed.
    #[inline]
    pub fn is_armed(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Trip the switch.
    pub fn trip(&self, now_ns: u64) {
        self.flag.store(true, Ordering::Release);
        self.tripped_at_ns.store(now_ns, Ordering::Release);
    }

    /// Re-arm (close) the switch. Returns the elapsed time-armed in nanoseconds.
    pub fn close(&self, now_ns: u64) -> u64 {
        self.flag.store(false, Ordering::Release);
        let then = self.tripped_at_ns.swap(0, Ordering::AcqRel);
        if then == 0 {
            0
        } else {
            now_ns.saturating_sub(then)
        }
    }

    /// Read the time the switch was tripped (0 if never).
    pub fn tripped_at_ns(&self) -> u64 {
        self.tripped_at_ns.load(Ordering::Acquire)
    }
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}

/// All §14.1 levels in one struct so the hot path can read every relevant switch with one
/// pointer.
pub struct KillSwitches {
    /// Global halt.
    pub global: KillSwitch,
    /// Gas-staleness halt.
    pub gas: KillSwitch,
    /// Feed sequence-gap halt.
    pub feed: KillSwitch,
    /// Inclusion-rate halt.
    pub inclusion: KillSwitch,
}

impl KillSwitches {
    /// New, all closed.
    pub const fn new() -> Self {
        Self {
            global: KillSwitch::new(),
            gas: KillSwitch::new(),
            feed: KillSwitch::new(),
            inclusion: KillSwitch::new(),
        }
    }

    /// Returns `true` if **any** switch is armed. Hot path bail.
    #[inline]
    pub fn any_armed(&self) -> bool {
        self.global.is_armed()
            || self.gas.is_armed()
            || self.feed.is_armed()
            || self.inclusion.is_armed()
    }
}

impl Default for KillSwitches {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trip_and_close() {
        let s = KillSwitch::new();
        assert!(!s.is_armed());
        s.trip(100);
        assert!(s.is_armed());
        let elapsed = s.close(150);
        assert_eq!(elapsed, 50);
        assert!(!s.is_armed());
    }

    #[test]
    fn aggregate_any_armed() {
        let k = KillSwitches::new();
        assert!(!k.any_armed());
        k.feed.trip(1);
        assert!(k.any_armed());
    }
}
