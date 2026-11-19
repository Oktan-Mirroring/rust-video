//! Codec identities behind the profile probe.

pub use rust_video_core::Codec;

/// Matched decoder profile on one adapter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Profile {
    /// Codec the profile decodes.
    pub(crate) codec: Codec,
    /// Profile name.
    pub(crate) name: &'static str,
}

impl Profile {
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
