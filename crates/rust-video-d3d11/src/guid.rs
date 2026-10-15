//! Codec identities behind the profile probe.

use std::fmt;

/// Codec with a D3D11 decoder profile family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Codec {
    /// H.264.
    H264,
    /// H.265.
    Hevc,
    /// VP9.
    Vp9,
    /// AV1.
    Av1,
    /// MPEG-2.
    Mpeg2,
    /// VC-1.
    Vc1,
}

impl Codec {
    /// Profile families in probe order, most deployed first.
    pub const ALL: &'static [Self] = &[
        Self::H264,
        Self::Hevc,
        Self::Vp9,
        Self::Av1,
        Self::Mpeg2,
        Self::Vc1,
    ];

    /// Codec name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::Mpeg2 => "mpeg2",
            Self::Vc1 => "vc1",
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

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
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) const fn new(codec: Codec, name: &'static str) -> Self {
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
    #[cfg(target_os = "windows")]
    use super::Profile;

    #[test]
    fn all_lists_six_families() {
        assert_eq!(Codec::ALL.len(), 6);
        assert!(Codec::ALL.contains(&Codec::H264));
        assert!(Codec::ALL.contains(&Codec::Vc1));
    }

    #[test]
    fn names_match_spelling() {
        let names: Vec<&str> = Codec::ALL.iter().map(|codec| codec.name()).collect();
        assert_eq!(names, ["h264", "hevc", "vp9", "av1", "mpeg2", "vc1"]);
    }

    #[test]
    fn display_delegates_to_name() {
        assert_eq!(Codec::Hevc.to_string(), "hevc");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn profile_reports_fields() {
        let profile = Profile::new(Codec::Hevc, "hevc-main10");
        assert_eq!(profile.codec(), Codec::Hevc);
        assert_eq!(profile.name(), "hevc-main10");
    }
}
