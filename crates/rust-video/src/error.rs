//! Facade error vocabulary.

use std::fmt;

use crate::tier::TierError;

use rust_video_caps::common::Violation;

/// Facade failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Error {
    /// No tier can take the codec.
    TierUnavailable(TierError),
    /// Caps conformance failure.
    Caps(Violation),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TierUnavailable(err) => write!(f, "{err}"),
            Self::Caps(violation) => write!(f, "{violation}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TierUnavailable(err) => Some(err),
            Self::Caps(violation) => Some(violation),
        }
    }
}

impl From<Violation> for Error {
    fn from(violation: Violation) -> Self {
        Self::Caps(violation)
    }
}

impl From<TierError> for Error {
    fn from(err: TierError) -> Self {
        Self::TierUnavailable(err)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use super::Error;
    use crate::tier::{Tier, TierError, TierPreference, VideoCodec};

    use rust_video_caps::common::Violation;

    #[test]
    fn tier_unavailable_display_delegates() {
        let err = Error::from(TierError::new(
            VideoCodec::H264,
            TierPreference::Force(Tier::VaApi),
        ));
        assert_eq!(format!("{err}"), "tier VaApi unavailable for codec H264");
    }

    #[test]
    fn caps_display_delegates() {
        let err = Error::from(Violation::ChromaNotAllowed);
        assert_eq!(format!("{err}"), "chroma sampling not allowed");
        let err = Error::from(Violation::BitDepthNotAllowed { actual: 12, max: 8 });
        assert_eq!(format!("{err}"), "bit depth 12 above maximum 8");
    }

    #[test]
    fn from_impls_select_variants() {
        let tier = TierError::new(VideoCodec::H265, TierPreference::Auto);
        assert_eq!(Error::from(tier), Error::TierUnavailable(tier));
        assert_eq!(
            Error::from(Violation::ChromaNotAllowed),
            Error::Caps(Violation::ChromaNotAllowed)
        );
    }

    #[test]
    fn source_returns_cause() {
        let tier = TierError::new(VideoCodec::H265, TierPreference::Auto);
        let err = Error::TierUnavailable(tier);
        assert_eq!(
            err.source().expect("tier cause").to_string(),
            tier.to_string()
        );
        let err = Error::Caps(Violation::ChromaNotAllowed);
        assert_eq!(
            err.source().expect("caps cause").to_string(),
            Violation::ChromaNotAllowed.to_string()
        );
    }
}
