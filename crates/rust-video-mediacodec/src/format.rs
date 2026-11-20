//! Codec input formats and output layout mapping.

use rust_video_caps::frame::PixelFormat;

use crate::error::MediaCodecError;

/// Codec carried by a format.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Codec {
    /// H.264.
    H264,
    /// H.265.
    H265,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
    /// VP8.
    Vp8,
}

/// Decoder input format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatSpec {
    /// Codec identity.
    codec: Codec,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Declared frame rate.
    frame_rate: Option<u32>,
    /// First codec-specific blob.
    csd_0: Option<Vec<u8>>,
    /// Second codec-specific blob.
    csd_1: Option<Vec<u8>>,
}

impl FormatSpec {
    /// `MediaFormat#KEY_MIME`.
    pub const KEY_MIME: &'static str = "mime";
    /// `MediaFormat#KEY_WIDTH`.
    pub const KEY_WIDTH: &'static str = "width";
    /// `MediaFormat#KEY_HEIGHT`.
    pub const KEY_HEIGHT: &'static str = "height";
    /// `MediaFormat#KEY_FRAME_RATE`.
    pub const KEY_FRAME_RATE: &'static str = "frame-rate";
    /// First codec-specific data key.
    pub const KEY_CSD_0: &'static str = "csd-0";
    /// Second codec-specific data key.
    pub const KEY_CSD_1: &'static str = "csd-1";
    /// `MediaFormat#KEY_COLOR_FORMAT`.
    pub const KEY_COLOR_FORMAT: &'static str = "color-format";
    /// `MediaFormat#KEY_STRIDE`.
    pub const KEY_STRIDE: &'static str = "stride";
    /// `MediaFormat#KEY_SLICE_HEIGHT`.
    pub const KEY_SLICE_HEIGHT: &'static str = "slice-height";

    /// `MIMETYPE_VIDEO_AVC`.
    pub const MIME_H264: &'static str = "video/avc";
    /// `MIMETYPE_VIDEO_HEVC`.
    pub const MIME_H265: &'static str = "video/hevc";
    /// `MIMETYPE_VIDEO_AV1`.
    pub const MIME_AV1: &'static str = "video/av01";
    /// `MIMETYPE_VIDEO_VP9`.
    pub const MIME_VP9: &'static str = "video/x-vnd.on2.vp9";
    /// `MIMETYPE_VIDEO_VP8`.
    pub const MIME_VP8: &'static str = "video/x-vnd.on2.vp8";

    /// `COLOR_FormatYUV420Planar`.
    pub const COLOR_PLANAR: i32 = 19;
    /// `COLOR_FormatYUV420SemiPlanar`.
    pub const COLOR_SEMIPLANAR: i32 = 21;

    /// Device floor for AV1 decode; Android 10 carries the first AV1 codecs.
    pub const AV1_MIN_SDK: u32 = 29;

    /// Largest dimension the native format takes as `i32`.
    const MAX_DIMENSION: u32 = i32::MAX as u32;

    /// H.264 format over `width` by `height`; zero or oversize dims rejected.
    #[must_use]
    pub fn h264(width: u32, height: u32) -> Option<Self> {
        Self::coded(Codec::H264, width, height)
    }

    /// H.265 format over `width` by `height`; zero or oversize dims rejected.
    #[must_use]
    pub fn h265(width: u32, height: u32) -> Option<Self> {
        Self::coded(Codec::H265, width, height)
    }

    /// AV1 format over `width` by `height`; zero or oversize dims rejected.
    #[must_use]
    pub fn av1(width: u32, height: u32) -> Option<Self> {
        Self::coded(Codec::Av1, width, height)
    }

    /// VP9 format over `width` by `height`; zero or oversize dims rejected.
    #[must_use]
    pub fn vp9(width: u32, height: u32) -> Option<Self> {
        Self::coded(Codec::Vp9, width, height)
    }

    /// VP8 format over `width` by `height`; zero or oversize dims rejected.
    #[must_use]
    pub fn vp8(width: u32, height: u32) -> Option<Self> {
        Self::coded(Codec::Vp8, width, height)
    }

    /// Format for `codec` over `width` by `height`.
    fn coded(codec: Codec, width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 || width > Self::MAX_DIMENSION || height > Self::MAX_DIMENSION
        {
            return None;
        }
        Some(Self {
            codec,
            width,
            height,
            frame_rate: None,
            csd_0: None,
            csd_1: None,
        })
    }

    /// Format carrying frame rate `rate`.
    #[must_use]
    pub fn with_frame_rate(mut self, rate: u32) -> Self {
        self.frame_rate = Some(rate);
        self
    }

    /// Format carrying first codec-specific blob `csd`.
    #[must_use]
    pub fn with_csd_0(mut self, csd: Vec<u8>) -> Self {
        self.csd_0 = Some(csd);
        self
    }

    /// Format carrying second codec-specific blob `csd`.
    #[must_use]
    pub fn with_csd_1(mut self, csd: Vec<u8>) -> Self {
        self.csd_1 = Some(csd);
        self
    }

    /// Codec mime type.
    #[must_use]
    pub fn mime(&self) -> &'static str {
        match self.codec {
            Codec::H264 => Self::MIME_H264,
            Codec::H265 => Self::MIME_H265,
            Codec::Av1 => Self::MIME_AV1,
            Codec::Vp9 => Self::MIME_VP9,
            Codec::Vp8 => Self::MIME_VP8,
        }
    }

    /// Lowest device API carrying this codec.
    #[must_use]
    pub fn min_sdk(&self) -> u32 {
        match self.codec {
            Codec::Av1 => Self::AV1_MIN_SDK,
            Codec::H264 | Codec::H265 | Codec::Vp9 | Codec::Vp8 => crate::sdk::MIN_SDK,
        }
    }

    /// Coded width in luma samples.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Coded height in luma samples.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Declared frame rate, if set.
    #[must_use]
    pub fn frame_rate(&self) -> Option<u32> {
        self.frame_rate
    }

    /// First codec-specific blob, if set.
    #[must_use]
    pub fn csd_0(&self) -> Option<&[u8]> {
        self.csd_0.as_deref()
    }

    /// Second codec-specific blob, if set.
    #[must_use]
    pub fn csd_1(&self) -> Option<&[u8]> {
        self.csd_1.as_deref()
    }

    /// Rejection unless H.264/H.265 carries both codec-specific blobs.
    ///
    /// # Errors
    ///
    /// Returns `MissingCsd` naming the first absent blob.
    pub fn check_csd(&self) -> Result<(), MediaCodecError> {
        match self.codec {
            Codec::H264 | Codec::H265 => {
                if Self::absent(self.csd_0.as_ref()) {
                    return Err(MediaCodecError::MissingCsd {
                        mime: self.mime(),
                        key: Self::KEY_CSD_0,
                    });
                }
                if Self::absent(self.csd_1.as_ref()) {
                    return Err(MediaCodecError::MissingCsd {
                        mime: self.mime(),
                        key: Self::KEY_CSD_1,
                    });
                }
                Ok(())
            }
            Codec::Av1 | Codec::Vp9 | Codec::Vp8 => Ok(()),
        }
    }

    /// True when the blob is unset or empty.
    fn absent(blob: Option<&Vec<u8>>) -> bool {
        blob.is_none_or(Vec::is_empty)
    }

    /// Pixel layout for an output `color-format`, if mappable.
    ///
    /// # Arguments
    ///
    /// * `color_format` - raw `color-format` value.
    #[must_use]
    pub fn pixel_format_for(color_format: i32) -> Option<PixelFormat> {
        match color_format {
            Self::COLOR_PLANAR => Some(PixelFormat::Yuv420P),
            Self::COLOR_SEMIPLANAR => Some(PixelFormat::Nv12),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FormatSpec;

    use crate::error::MediaCodecError;
    use rust_video_caps::frame::PixelFormat;

    #[test]
    fn builders_carry_mime_and_dims() {
        let spec = FormatSpec::h264(1920, 1080).expect("valid dims");
        assert_eq!(spec.mime(), "video/avc");
        assert_eq!((spec.width(), spec.height()), (1920, 1080));
        let spec = FormatSpec::h265(3840, 2160).expect("valid dims");
        assert_eq!(spec.mime(), "video/hevc");
        let spec = FormatSpec::av1(1280, 720).expect("valid dims");
        assert_eq!(spec.mime(), "video/av01");
        let spec = FormatSpec::vp9(640, 480).expect("valid dims");
        assert_eq!(spec.mime(), "video/x-vnd.on2.vp9");
        let spec = FormatSpec::vp8(320, 240).expect("valid dims");
        assert_eq!(spec.mime(), "video/x-vnd.on2.vp8");
    }

    #[test]
    fn builders_reject_bad_dims() {
        assert!(FormatSpec::h264(0, 1080).is_none());
        assert!(FormatSpec::h264(1920, 0).is_none());
        assert!(FormatSpec::h265(u32::MAX, 1080).is_none());
        assert!(FormatSpec::av1(1920, u32::MAX).is_none());
    }

    #[test]
    fn builders_chain_options() {
        let spec = FormatSpec::h264(16, 16)
            .expect("valid dims")
            .with_frame_rate(60)
            .with_csd_0(vec![0, 0, 1])
            .with_csd_1(vec![1]);
        assert_eq!(spec.frame_rate(), Some(60));
        assert_eq!(spec.csd_0(), Some([0, 0, 1].as_slice()));
        assert_eq!(spec.csd_1(), Some([1].as_slice()));
    }

    #[test]
    fn min_sdk_gates_av1() {
        let h264 = FormatSpec::h264(16, 16).expect("valid dims");
        let h265 = FormatSpec::h265(16, 16).expect("valid dims");
        let vp9 = FormatSpec::vp9(16, 16).expect("valid dims");
        let av1 = FormatSpec::av1(16, 16).expect("valid dims");
        let vp8 = FormatSpec::vp8(16, 16).expect("valid dims");
        assert_eq!(h264.min_sdk(), 21);
        assert_eq!(h265.min_sdk(), 21);
        assert_eq!(vp9.min_sdk(), 21);
        assert_eq!(av1.min_sdk(), 29);
        assert_eq!(vp8.min_sdk(), 21);
    }

    #[test]
    fn keys_match_platform_constants() {
        assert_eq!(FormatSpec::KEY_MIME, "mime");
        assert_eq!(FormatSpec::KEY_WIDTH, "width");
        assert_eq!(FormatSpec::KEY_HEIGHT, "height");
        assert_eq!(FormatSpec::KEY_FRAME_RATE, "frame-rate");
        assert_eq!(FormatSpec::KEY_CSD_0, "csd-0");
        assert_eq!(FormatSpec::KEY_CSD_1, "csd-1");
        assert_eq!(FormatSpec::KEY_COLOR_FORMAT, "color-format");
        assert_eq!(FormatSpec::KEY_STRIDE, "stride");
        assert_eq!(FormatSpec::KEY_SLICE_HEIGHT, "slice-height");
    }

    #[test]
    fn check_csd_demands_blobs_for_avc_and_hevc() {
        let bare = FormatSpec::h264(16, 16).expect("valid dims");
        assert_eq!(
            bare.check_csd(),
            Err(MediaCodecError::MissingCsd {
                mime: "video/avc",
                key: "csd-0",
            })
        );
        let half = FormatSpec::h264(16, 16)
            .expect("valid dims")
            .with_csd_0(vec![1]);
        assert_eq!(
            half.check_csd(),
            Err(MediaCodecError::MissingCsd {
                mime: "video/avc",
                key: "csd-1",
            })
        );
        let empty = FormatSpec::h265(16, 16)
            .expect("valid dims")
            .with_csd_0(Vec::new())
            .with_csd_1(vec![1]);
        assert_eq!(
            empty.check_csd(),
            Err(MediaCodecError::MissingCsd {
                mime: "video/hevc",
                key: "csd-0",
            })
        );
        let full = FormatSpec::h265(16, 16)
            .expect("valid dims")
            .with_csd_0(vec![1])
            .with_csd_1(vec![2]);
        assert!(full.check_csd().is_ok());
        for spec in [
            FormatSpec::av1(16, 16).expect("valid dims"),
            FormatSpec::vp9(16, 16).expect("valid dims"),
            FormatSpec::vp8(16, 16).expect("valid dims"),
        ] {
            assert!(spec.check_csd().is_ok());
        }
    }

    #[test]
    fn color_map_covers_byte_buffer_layouts() {
        assert_eq!(FormatSpec::pixel_format_for(19), Some(PixelFormat::Yuv420P));
        assert_eq!(FormatSpec::pixel_format_for(21), Some(PixelFormat::Nv12));
        assert_eq!(FormatSpec::pixel_format_for(2_135_033_992), None);
        assert_eq!(FormatSpec::pixel_format_for(2_130_708_361), None);
        assert_eq!(FormatSpec::pixel_format_for(0), None);
    }
}
