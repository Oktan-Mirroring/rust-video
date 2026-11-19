//! Saturating duration reads.

use std::time::Duration;

/// Whole microseconds, saturating.
#[must_use]
pub fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::micros;

    #[test]
    fn whole_microseconds_truncate_down() {
        assert_eq!(micros(Duration::from_millis(5)), 5_000);
        assert_eq!(micros(Duration::from_nanos(1_999)), 1);
        assert_eq!(micros(Duration::ZERO), 0);
    }

    #[test]
    fn overflow_saturates_at_max() {
        assert_eq!(micros(Duration::MAX), u64::MAX);
    }
}
