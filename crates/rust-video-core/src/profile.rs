//! Adapter decoder profile behind the probe.

use crate::codecs::Codec;

/// Matched decoder profile on one adapter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Profile {
    /// Codec the profile decodes.
    codec: Codec,
    /// Profile name.
    name: &'static str,
}

impl Profile {
    /// Profile of `codec` named `name`.
    #[must_use]
    pub const fn new(codec: Codec, name: &'static str) -> Self {
        Self { codec, name }
    }

    /// Codec the profile decodes.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Profile name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

#[cfg(test)]
mod tests {
    use super::Codec;
    use super::Profile;

    #[test]
    fn profile_reports_fields() {
        let profile = Profile::new(Codec::H264, "h264-vld-nofgt");
        assert_eq!(profile.codec(), Codec::H264);
        assert_eq!(profile.name(), "h264-vld-nofgt");
    }
}
