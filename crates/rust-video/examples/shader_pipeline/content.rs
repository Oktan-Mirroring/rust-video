//! Bench vectors as coded packets plus filter vocabulary.

use bytes::Bytes;
use rust_video::Packet;
use rust_video_harness::{bench_dir, AnnexB, Ivf};

/// Codec under test.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Codec {
    /// H.264.
    H264,
    /// H.265.
    H265,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
}

impl Codec {
    /// Every codec the bench vectors cover.
    pub const ALL: [Self; 4] = [Self::H264, Self::H265, Self::Av1, Self::Vp9];

    /// Lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Av1 => "av1",
            Self::Vp9 => "vp9",
        }
    }

    /// Parse a CLI codec filter.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|codec| codec.name() == text.to_lowercase())
    }
}

/// GPU filter operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Op {
    /// Desaturate to grey.
    Gray,
    /// Halve each extent.
    Down,
    /// Studio-to-full range convert plus halve each extent.
    Color,
    /// Center window at full resolution.
    Crop,
}

impl Op {
    /// Every filter operation.
    pub const ALL: [Self; 4] = [Self::Gray, Self::Down, Self::Color, Self::Crop];

    /// Lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Down => "down",
            Self::Color => "color",
            Self::Crop => "crop",
        }
    }

    /// Parse a CLI op filter.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|op| op.name() == text.to_lowercase())
    }
}

/// Coded pictures plus geometry for one codec.
pub struct Clip {
    /// Coded pictures in decode order.
    raws: Vec<Vec<u8>>,
    /// Luma width in samples.
    width: u32,
    /// Luma height in samples.
    height: u32,
}

impl Clip {
    /// Inter-picture 1080p vector for `codec`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the vector holds no pictures.
    ///
    /// # Panics
    ///
    /// Panics when the vector file is unreadable; the harness owns that path.
    pub fn load(codec: Codec) -> Result<Self, String> {
        let (file, raws) = match codec {
            Codec::H264 => (
                "h264-1080p_ip.h264",
                AnnexB::picture_packets(
                    &bench_dir().join("h264-1080p_ip.h264"),
                    AnnexB::is_h264_vcl,
                ),
            ),
            Codec::H265 => (
                "h265-1080p_ip.h265",
                AnnexB::picture_packets(
                    &bench_dir().join("h265-1080p_ip.h265"),
                    AnnexB::is_hevc_vcl,
                ),
            ),
            Codec::Av1 => (
                "av1-1080p_ip.ivf",
                Ivf::read(&bench_dir().join("av1-1080p_ip.ivf")).frames,
            ),
            Codec::Vp9 => (
                "vp9-1080p_ip.ivf",
                Ivf::read(&bench_dir().join("vp9-1080p_ip.ivf")).frames,
            ),
        };
        if raws.is_empty() {
            return Err(format!("{file} holds no pictures"));
        }
        Ok(Self {
            raws,
            width: 1920,
            height: 1080,
        })
    }

    /// Coded pictures in decode order.
    #[must_use]
    pub fn raws(&self) -> &[Vec<u8>] {
        &self.raws
    }

    /// Luma width in samples.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Luma height in samples.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Packets over the pictures, built once outside the timed loop.
    #[must_use]
    pub fn packets(&self) -> Vec<Packet> {
        self.raws
            .iter()
            .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
            .collect()
    }
}
