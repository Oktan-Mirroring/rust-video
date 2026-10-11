#![allow(unsafe_code)]
//! Device API level and SDK gates.

/// Lowest supported device API; `AMediaCodec` starts here.
pub const MIN_SDK: u32 = 21;

/// API level carrying async notify callbacks.
pub const ASYNC_NOTIFY_SDK: u32 = 28;

/// Device API level.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ApiLevel(u32);

impl ApiLevel {
    /// Level from a raw SDK integer.
    #[must_use]
    pub fn new(level: u32) -> Self {
        Self(level)
    }

    /// Live device level.
    ///
    /// <https://docs.rs/ndk-sys/0.6.0/ndk_sys/fn.android_get_device_api_level.html>
    #[cfg(target_os = "android")]
    #[must_use]
    pub fn current() -> Self {
        // SAFETY: pure query without preconditions; any thread, any time.
        let level = unsafe { ndk_sys::android_get_device_api_level() };
        Self(u32::try_from(level).unwrap_or(0))
    }

    /// Raw SDK integer.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }

    /// True at `min` and above.
    #[must_use]
    pub fn at_least(self, min: u32) -> bool {
        self.0 >= min
    }

    /// True on the supported floor and above.
    #[must_use]
    pub fn meets_min(self) -> bool {
        self.at_least(MIN_SDK)
    }

    /// True where async notify callbacks exist.
    ///
    /// `ndk` 0.9.0 gates `set_async_notify_callback` behind `api-level-28`.
    /// <https://docs.rs/ndk/0.9.0/ndk/media/media_codec/struct.MediaCodec.html#method.set_async_notify_callback>
    #[must_use]
    pub fn supports_async_notify(self) -> bool {
        self.at_least(ASYNC_NOTIFY_SDK)
    }
}

#[cfg(test)]
mod tests {
    use super::{ApiLevel, ASYNC_NOTIFY_SDK, MIN_SDK};

    #[test]
    fn floors_match_ndk_history() {
        assert_eq!(MIN_SDK, 21);
        assert_eq!(ASYNC_NOTIFY_SDK, 28);
    }

    #[test]
    fn predicates_gate_levels() {
        assert!(!ApiLevel::new(19).meets_min());
        assert!(ApiLevel::new(21).meets_min());
        assert!(!ApiLevel::new(27).supports_async_notify());
        assert!(ApiLevel::new(28).supports_async_notify());
        assert!(ApiLevel::new(29).at_least(29));
        assert!(!ApiLevel::new(28).at_least(29));
    }

    #[test]
    fn round_trips_raw_value() {
        assert_eq!(ApiLevel::new(33).get(), 33);
    }
}
