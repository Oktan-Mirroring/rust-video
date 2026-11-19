//! D3D11 device with enumerated decoder profiles behind the caps `Device` trait.

use crate::guid::{Codec, Profile};
use crate::DeviceOs;

use rust_video_caps::traits::Device;

/// D3D11 device with enumerated decoder profiles.
pub struct D3d11Device {
    /// Matched decoder profiles.
    pub(crate) profiles: Vec<Profile>,
    /// Negotiated feature level.
    pub(crate) level: i32,
    /// Adapter description.
    pub(crate) name: String,
    // Unread off-target: only the Windows module constructs or reads it.
    #[allow(dead_code)]
    /// Windows device state.
    pub(crate) os: DeviceOs,
}

impl D3d11Device {
    /// True when the adapter exposes a `codec` profile.
    #[must_use]
    pub fn supports(&self, codec: Codec) -> bool {
        self.profiles.iter().any(|profile| profile.codec() == codec)
    }

    /// Matched decoder profiles on the adapter.
    #[must_use]
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Negotiated feature level.
    #[must_use]
    pub fn feature_level(&self) -> i32 {
        self.level
    }
}

impl Device for D3d11Device {
    fn name(&self) -> &str {
        &self.name
    }
}
