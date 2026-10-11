//! Realtime video encode/decode toolkit.
//!
//! Pure-Rust-first codecs and platform tiers behind one facade.

/// Decode tier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DecodeTier {
    /// Vulkan video decode.
    Vulkan,
    /// VA-API decode.
    VaApi,
    /// Direct3D 11 decode.
    D3d11,
    /// Android `MediaCodec` decode.
    MediaCodec,
    /// Pure-Rust software decode.
    Software,
}

/// Encode tier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EncodeTier {
    /// Vulkan video encode.
    Vulkan,
    /// VA-API encode.
    VaApi,
    /// Direct3D 11 encode.
    D3d11,
    /// Android `MediaCodec` encode.
    MediaCodec,
    /// Pure-Rust software encode.
    Software,
}

/// Video codec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VideoCodec {
    /// H.264 codec.
    H264,
    /// H.265 codec.
    H265,
    /// AV1 codec.
    Av1,
    /// VP9 codec.
    Vp9,
    /// AV2 codec.
    Av2,
}
