//! 90 kHz and 27 MHz media clocks.
//!
//! [ITU-T H.222.0](https://www.itu.int/rec/T-REC-H.222.0) section 2.4.

use std::fmt;

/// Ticks per second of the system clock reference.
pub const TICKS_PER_SEC: u64 = 90_000;

/// Ticks per second of the program clock reference.
pub const PCR_TICKS_PER_SEC: u64 = 27_000_000;

/// PTS/DTS/PCR-base wrap interval in 90 kHz ticks.
pub const STC_WRAP: u64 = 1 << 33;

/// Maximum `PCR_extension` value; the extension counts 0..=299.
pub const PCR_EXT_MAX: u16 = 299;

/// 33-bit system time clock value.
///
/// Wire form: H.222.0 section 2.4.3.7 figure 2-7.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Ts90k(u64);

impl Ts90k {
    /// Clock value holding the low 33 bits of `ticks`.
    #[must_use]
    pub fn new(ticks: u64) -> Self {
        Self(ticks & (STC_WRAP - 1))
    }

    /// Raw 33-bit counter.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Clock advanced by `delta` ticks, wrapping at 2^33.
    #[must_use]
    pub fn wrapping_add(self, delta: u64) -> Self {
        Self::new(self.0.wrapping_add(delta))
    }

    /// Forward distance from `self` to `other` in ticks.
    #[must_use]
    pub fn forward_distance(self, other: Self) -> u64 {
        other.0.wrapping_sub(self.0) & (STC_WRAP - 1)
    }

    /// Five-byte PTS/DTS field with marker bits set.
    ///
    /// # Arguments
    ///
    /// * `prefix` - `0b0010` for PTS-only, `0b0001`/`0b0011` around DTS.
    #[must_use]
    pub fn field_bytes(self, prefix: u8) -> [u8; 5] {
        let v = self.0;
        [
            (prefix << 4) | (((v >> 30) & 0x07) as u8) << 1 | 1,
            ((v >> 22) & 0xFF) as u8,
            ((((v >> 15) & 0x7F) as u8) << 1) | 1,
            ((v >> 7) & 0xFF) as u8,
            (((v & 0x7F) as u8) << 1) | 1,
        ]
    }

    /// Clock parsed from a five-byte PTS/DTS field.
    ///
    /// # Returns
    ///
    /// `None` when any marker bit is clear.
    #[must_use]
    pub fn parse_field(bytes: [u8; 5]) -> Option<Self> {
        if bytes[0] & 1 == 0 || bytes[2] & 1 == 0 || bytes[4] & 1 == 0 {
            return None;
        }
        let v = (u64::from(bytes[0] >> 1 & 0x07) << 30)
            | (u64::from(bytes[1]) << 22)
            | (u64::from(bytes[2] >> 1) << 15)
            | (u64::from(bytes[3]) << 7)
            | u64::from(bytes[4] >> 1);
        Some(Self(v))
    }
}

impl fmt::Display for Ts90k {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Program clock reference: 33-bit 90 kHz base plus 9-bit extension.
///
/// Wire form: H.222.0 section 2.4.3.5; `PCR = base * 300 + ext`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Pcr {
    /// 90 kHz base.
    base: Ts90k,
    /// 27 MHz extension, 0..=299.
    ext: u16,
}

impl Pcr {
    /// Reference from a 90 kHz base and an extension clamped to 299.
    #[must_use]
    pub fn from_base_ext(base: Ts90k, ext: u16) -> Self {
        Self {
            base,
            ext: ext.min(PCR_EXT_MAX),
        }
    }

    /// 90 kHz base.
    #[must_use]
    pub fn base(self) -> Ts90k {
        self.base
    }

    /// 27 MHz extension.
    #[must_use]
    pub fn ext(self) -> u16 {
        self.ext
    }

    /// Full 27 MHz counter.
    #[must_use]
    pub fn total_27mhz(self) -> u64 {
        self.base.get() * 300 + u64::from(self.ext)
    }

    /// Six-byte adaptation-field form.
    #[must_use]
    pub fn bytes(self) -> [u8; 6] {
        let b = self.base.get();
        let e = u32::from(self.ext);
        [
            ((b >> 25) & 0xFF) as u8,
            ((b >> 17) & 0xFF) as u8,
            ((b >> 9) & 0xFF) as u8,
            ((b >> 1) & 0xFF) as u8,
            (((b & 1) as u8) << 7) | 0x7E | (((e >> 8) & 0x01) as u8),
            (e & 0xFF) as u8,
        ]
    }

    /// Reference parsed from six adaptation-field bytes.
    #[must_use]
    pub fn parse(bytes: [u8; 6]) -> Self {
        let b = (u64::from(bytes[0]) << 25)
            | (u64::from(bytes[1]) << 17)
            | (u64::from(bytes[2]) << 9)
            | (u64::from(bytes[3]) << 1)
            | u64::from(bytes[4] >> 7);
        let e = (u16::from(bytes[4] & 1) << 8) | u16::from(bytes[5]);
        Self::from_base_ext(Ts90k::new(b), e)
    }
}

/// 33-bit clock unwrapped onto a monotonic 64-bit line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClockTracker {
    /// Extended base added to observations.
    base: u64,
    /// Previous raw observation.
    last: Option<u64>,
}

impl ClockTracker {
    /// Tracker with no observations yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Extended timestamp for a raw 33-bit observation.
    ///
    /// A backward jump past half the wrap interval counts as a wrap;
    /// smaller backward steps are treated as reorder, not time travel.
    pub fn observe(&mut self, raw: u64) -> u64 {
        let raw = raw & (STC_WRAP - 1);
        if let Some(last) = self.last {
            if last > raw && last - raw > STC_WRAP / 2 {
                self.base += STC_WRAP;
            }
        }
        self.last = Some(raw);
        self.base + raw
    }
}

#[cfg(test)]
mod tests {
    use super::{ClockTracker, Pcr, Ts90k, STC_WRAP};

    #[test]
    fn field_round_trips_with_markers() {
        for (prefix, ticks) in [(0x2, 0), (0x2, 1), (0x3, 90_000), (0x1, STC_WRAP - 1)] {
            let bytes = Ts90k::new(ticks).field_bytes(prefix);
            assert_eq!(bytes[0] & 1, 1);
            assert_eq!(bytes[2] & 1, 1);
            assert_eq!(bytes[4] & 1, 1);
            assert_eq!(bytes[0] >> 4, prefix);
            assert_eq!(Ts90k::parse_field(bytes), Some(Ts90k::new(ticks)));
        }
    }

    #[test]
    fn field_rejects_clear_marker() {
        let mut bytes = Ts90k::new(7).field_bytes(0x2);
        bytes[2] &= 0xFE;
        assert_eq!(Ts90k::parse_field(bytes), None);
    }

    #[test]
    fn distance_wraps_forward() {
        assert_eq!(Ts90k::new(10).forward_distance(Ts90k::new(15)), 5);
        assert_eq!(Ts90k::new(STC_WRAP - 1).forward_distance(Ts90k::new(0)), 1);
    }

    #[test]
    fn pcr_round_trips() {
        let pcr = Pcr::from_base_ext(Ts90k::new(STC_WRAP - 1), 299);
        assert_eq!(Pcr::parse(pcr.bytes()), pcr);
        assert_eq!(pcr.total_27mhz(), (STC_WRAP - 1) * 300 + 299);
    }

    #[test]
    fn pcr_clamps_extension() {
        assert_eq!(
            Pcr::from_base_ext(Ts90k::new(0), 999).ext(),
            super::PCR_EXT_MAX
        );
    }

    #[test]
    fn tracker_unwraps_once() {
        let mut tracker = ClockTracker::new();
        assert_eq!(tracker.observe(STC_WRAP - 2), STC_WRAP - 2);
        assert_eq!(tracker.observe(3), STC_WRAP + 3);
        assert_eq!(tracker.observe(9), STC_WRAP + 9);
    }

    #[test]
    fn tracker_tolerates_small_reorder() {
        let mut tracker = ClockTracker::new();
        assert_eq!(tracker.observe(1000), 1000);
        assert_eq!(tracker.observe(990), 990);
    }
}
