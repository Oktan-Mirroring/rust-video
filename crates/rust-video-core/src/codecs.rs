//! Codec identities behind the profile probe.

use std::fmt;

/// Codec with a decoder profile family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Codec {
    /// H.264.
    H264,
    /// H.265.
    H265,
    /// VP8.
    Vp8,
    /// VP9.
    Vp9,
    /// AV1.
    Av1,
    /// AV2.
    Av2,
    /// MPEG-2.
    Mpeg2,
    /// VC-1.
    Vc1,
}

impl Codec {
    /// Profile families in probe order, most deployed first.
    pub const ALL: &'static [Self] = &[
        Self::H264,
        Self::H265,
        Self::Vp8,
        Self::Vp9,
        Self::Av1,
        Self::Av2,
        Self::Mpeg2,
        Self::Vc1,
    ];

    /// Codec name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::Av2 => "av2",
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

#[cfg(test)]
mod tests {
    use super::Codec;

    #[test]
    fn all_lists_eight_families() {
        assert_eq!(Codec::ALL.len(), 8);
        assert!(Codec::ALL.contains(&Codec::H264));
        assert!(Codec::ALL.contains(&Codec::Vc1));
    }

    #[test]
    fn names_match_spelling() {
        let names: Vec<&str> = Codec::ALL.iter().map(|codec| codec.name()).collect();
        assert_eq!(
            names,
            ["h264", "h265", "vp8", "vp9", "av1", "av2", "mpeg2", "vc1"]
        );
    }

    #[test]
    fn display_delegates_to_name() {
        assert_eq!(Codec::H265.to_string(), "h265");
    }
}
