//! Byte-buffer encode behind the caps `Encoder` trait.

use std::time::Duration;

use crate::error::MediaCodecError;
use crate::format::{pixel_format_for, FormatSpec};
use crate::seam::{CodecSeam, SeamItem};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_core::planes;

/// Codec carried by an encode spec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Codec {
    /// H.264.
    H264,
    /// H.265.
    H265,
    /// AV1.
    Av1,
}

/// Encoder lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeState {
    /// Created, no format configured.
    Unconfigured,
    /// Format configured, not started.
    Configured,
    /// Started, accepts frames.
    Started,
    /// Released, unusable.
    Released,
}

impl EncodeState {
    /// State after `configure`.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless the encoder is unconfigured.
    pub fn configured(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Unconfigured => Ok(Self::Configured),
            other => Err(MediaCodecError::InvalidEncodeState {
                op: "configure",
                state: other,
            }),
        }
    }

    /// State after `start`.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless the encoder is configured.
    pub fn started(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Configured => Ok(Self::Started),
            other => Err(MediaCodecError::InvalidEncodeState {
                op: "start",
                state: other,
            }),
        }
    }

    /// State after `release`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the encoder is already released.
    pub fn released(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Released => Err(MediaCodecError::InvalidEncodeState {
                op: "release",
                state: self,
            }),
            Self::Unconfigured | Self::Configured | Self::Started => Ok(Self::Released),
        }
    }

    /// Rejection unless started.
    ///
    /// # Arguments
    ///
    /// * `op` - operation demanding the started state.
    ///
    /// # Errors
    ///
    /// Returns `Err` naming `op` and the current state.
    pub fn ensure_started(self, op: &'static str) -> Result<(), MediaCodecError> {
        if self == Self::Started {
            Ok(())
        } else {
            Err(MediaCodecError::InvalidEncodeState { op, state: self })
        }
    }
}

/// Byte-buffer plane arrangement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layout {
    /// Semi-planar Y plus interleaved UV.
    Nv12,
    /// Planar Y, U, V.
    Planar,
}

/// Encoder input format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodeSpec {
    /// Codec identity.
    codec: Codec,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Target bitrate in bits per second.
    bitrate: u32,
    /// Declared frame rate.
    frame_rate: u32,
    /// Seconds between keyframes.
    keyframe_interval: u32,
    /// Requested input `color-format`.
    color_format: i32,
}

impl EncodeSpec {
    /// `MediaFormat#KEY_BIT_RATE`.
    pub const KEY_BIT_RATE: &'static str = "bitrate";
    /// `MediaFormat#KEY_I_FRAME_INTERVAL`.
    pub const KEY_I_FRAME_INTERVAL: &'static str = "i-frame-interval";

    /// Largest dimension the native format takes as `i32`.
    const MAX_DIMENSION: u32 = i32::MAX as u32;
    /// Microseconds per second.
    const MICROS_PER_SECOND: u128 = 1_000_000;

    /// H.264 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn h264(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::H264,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// H.265 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn h265(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::H265,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// AV1 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn av1(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::Av1,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// Spec for `codec` over `width` by `height` at `bitrate` bps.
    fn coded(
        codec: Codec,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        if width == 0
            || height == 0
            || width > Self::MAX_DIMENSION
            || height > Self::MAX_DIMENSION
            || bitrate == 0
            || frame_rate == 0
        {
            return None;
        }
        Some(Self {
            codec,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
            color_format: FormatSpec::COLOR_PLANAR,
        })
    }

    /// Spec requesting input `color-format` `color`.
    #[must_use]
    pub fn with_color_format(mut self, color: i32) -> Self {
        self.color_format = color;
        self
    }

    /// Codec mime type.
    #[must_use]
    pub fn mime(&self) -> &'static str {
        match self.codec {
            Codec::H264 => FormatSpec::MIME_H264,
            Codec::H265 => FormatSpec::MIME_H265,
            Codec::Av1 => FormatSpec::MIME_AV1,
        }
    }

    /// Lowest device API carrying this codec.
    #[must_use]
    pub fn min_sdk(&self) -> u32 {
        match self.codec {
            Codec::Av1 => FormatSpec::AV1_MIN_SDK,
            Codec::H264 | Codec::H265 => crate::sdk::MIN_SDK,
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

    /// Target bitrate in bits per second.
    #[must_use]
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Declared frame rate.
    #[must_use]
    pub fn frame_rate(&self) -> u32 {
        self.frame_rate
    }

    /// Seconds between keyframes.
    #[must_use]
    pub fn keyframe_interval(&self) -> u32 {
        self.keyframe_interval
    }

    /// Requested input `color-format`.
    #[must_use]
    pub fn color_format(&self) -> i32 {
        self.color_format
    }

    /// Presentation time of frame `index` in microseconds.
    ///
    /// # Arguments
    ///
    /// * `index` - zero-based frame count.
    #[must_use]
    pub fn pts_for(&self, index: u64) -> u64 {
        let pts = u128::from(index) * Self::MICROS_PER_SECOND / u128::from(self.frame_rate);
        u64::try_from(pts).unwrap_or(u64::MAX)
    }
}

/// Packed encoder input geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct InputLayout {
    /// Picture dimensions in luma samples.
    size: PictureSize,
    /// Plane arrangement.
    layout: Layout,
    /// Luma bytes per row.
    stride: u32,
    /// Luma rows.
    slice_height: u32,
    /// True after an input format landed.
    negotiated: bool,
}

impl InputLayout {
    /// Initial guess from `spec`, ahead of the input format.
    ///
    /// # Arguments
    ///
    /// * `spec` - encode spec carrying the coded dimensions.
    #[must_use]
    pub fn tentative(spec: &EncodeSpec) -> Option<Self> {
        PictureSize::new(spec.width(), spec.height()).map(|size| Self {
            size,
            layout: Layout::Planar,
            stride: spec.width(),
            slice_height: spec.height(),
            negotiated: false,
        })
    }

    /// Layout from raw input format readings.
    ///
    /// # Arguments
    ///
    /// * `color` - `color-format` value; unmapped values are rejected.
    /// * `stride` - row stride; missing or non-positive falls back to width.
    /// * `slice_height` - row count; missing or non-positive falls back to height.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the color has no plane layout.
    pub fn apply_format(
        &mut self,
        color: Option<i32>,
        stride: Option<i32>,
        slice_height: Option<i32>,
    ) -> Result<(), MediaCodecError> {
        let color = color.unwrap_or(-1);
        let format = pixel_format_for(color)
            .ok_or(MediaCodecError::UnsupportedColorFormat { value: color })?;
        let layout = match format {
            PixelFormat::Nv12 => Layout::Nv12,
            PixelFormat::Yuv420P => Layout::Planar,
            _ => {
                return Err(MediaCodecError::UnsupportedColorFormat { value: color });
            }
        };
        self.layout = layout;
        self.stride = Self::or_fallback(stride, self.size.width());
        self.slice_height = Self::or_fallback(slice_height, self.size.height());
        self.negotiated = true;
        Ok(())
    }

    /// True after an input format landed.
    #[must_use]
    pub fn negotiated(&self) -> bool {
        self.negotiated
    }

    /// Frame packed into one contiguous input buffer.
    ///
    /// # Arguments
    ///
    /// * `frame` - planar 4:2:0 input at the configured extent.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the layout is not planar 4:2:0, the extent
    /// mismatches, or a plane is short for its rows.
    pub fn pack(&self, frame: &CpuFrame) -> Result<Bytes, MediaCodecError> {
        if frame.format() != PixelFormat::Yuv420P {
            return Err(MediaCodecError::UnsupportedInput {
                format: frame.format(),
            });
        }
        if frame.size() != self.size {
            return Err(self.mismatch(frame));
        }
        let [y, u, v] = frame.planes() else {
            return Err(self.mismatch(frame));
        };
        let width = self.size.width();
        let height = self.size.height();
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        let mut packed = Vec::new();
        let _reserved = packed.try_reserve_exact(self.packed_len());
        match self.layout {
            Layout::Planar => {
                let half = self.stride / 2;
                Self::copy_rows(&mut packed, y, width, height, self.stride)?;
                Self::copy_rows(&mut packed, u, chroma_width, chroma_height, half)?;
                Self::copy_rows(&mut packed, v, chroma_width, chroma_height, half)?;
            }
            Layout::Nv12 => {
                Self::copy_rows(&mut packed, y, width, height, self.stride)?;
                Self::interleave_rows(&mut packed, u, v, chroma_width, chroma_height, self.stride)?;
            }
        }
        Ok(Bytes::from(packed))
    }

    /// Positive `value`, else `fallback`.
    fn or_fallback(value: Option<i32>, fallback: u32) -> u32 {
        value
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(fallback)
    }

    /// Extent error carrying this layout against `frame`.
    fn mismatch(&self, frame: &CpuFrame) -> MediaCodecError {
        MediaCodecError::FrameMismatch {
            width: frame.size().width(),
            height: frame.size().height(),
            expected_width: self.size.width(),
            expected_height: self.size.height(),
        }
    }

    /// Packed byte count for this layout.
    fn packed_len(&self) -> usize {
        let width = usize::try_from(self.size.width()).unwrap_or(usize::MAX);
        let height = usize::try_from(self.size.height()).unwrap_or(usize::MAX);
        let chroma_width = usize::try_from(self.size.width().div_ceil(2)).unwrap_or(usize::MAX);
        let chroma_height = usize::try_from(self.size.height().div_ceil(2)).unwrap_or(usize::MAX);
        let stride = usize::try_from(self.stride).unwrap_or(usize::MAX);
        let y = width.max(stride).saturating_mul(height);
        let chroma = match self.layout {
            Layout::Planar => chroma_width
                .max(stride / 2)
                .saturating_mul(chroma_height)
                .saturating_mul(2),
            Layout::Nv12 => chroma_width
                .saturating_mul(2)
                .max(stride)
                .saturating_mul(chroma_height),
        };
        y.saturating_add(chroma)
    }

    /// Plane rows copied into `dst`, padded to `pitch`.
    fn copy_rows(
        dst: &mut Vec<u8>,
        plane: &Plane,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> Result<(), MediaCodecError> {
        let stride = Self::checked_stride(plane, width, height)?;
        let row = usize::try_from(width).unwrap_or(usize::MAX);
        let rows = usize::try_from(height).unwrap_or(usize::MAX);
        let pitch = usize::try_from(pitch).unwrap_or(usize::MAX);
        let pad = pitch.saturating_sub(row);
        let src = plane.data();
        for r in 0..rows {
            let start = r.saturating_mul(stride);
            let end = start.saturating_add(row);
            dst.extend_from_slice(&src[start..end]);
            dst.extend(std::iter::repeat_n(0, pad));
        }
        Ok(())
    }

    /// Chroma rows interleaved into `dst`, padded to `pitch`.
    fn interleave_rows(
        dst: &mut Vec<u8>,
        u: &Plane,
        v: &Plane,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> Result<(), MediaCodecError> {
        let u_stride = Self::checked_stride(u, width, height)?;
        let v_stride = Self::checked_stride(v, width, height)?;
        let row = usize::try_from(width).unwrap_or(usize::MAX);
        let rows = usize::try_from(height).unwrap_or(usize::MAX);
        let pitch = usize::try_from(pitch).unwrap_or(usize::MAX);
        let pad = pitch.saturating_sub(row.saturating_mul(2));
        let u_src = u.data();
        let v_src = v.data();
        for r in 0..rows {
            let u_start = r.saturating_mul(u_stride);
            let v_start = r.saturating_mul(v_stride);
            let u_row = &u_src[u_start..u_start.saturating_add(row)];
            let v_row = &v_src[v_start..v_start.saturating_add(row)];
            for (u_byte, v_byte) in u_row.iter().zip(v_row.iter()) {
                dst.push(*u_byte);
                dst.push(*v_byte);
            }
            dst.extend(std::iter::repeat_n(0, pad));
        }
        Ok(())
    }

    /// Plane stride after the stride and length checks.
    fn checked_stride(plane: &Plane, width: u32, height: u32) -> Result<usize, MediaCodecError> {
        planes::check_plane(plane.data(), plane.stride(), width, height).map_err(|fault| {
            match fault {
                planes::PlaneFault::StrideBelowWidth { stride, width } => {
                    MediaCodecError::BufferRange {
                        offset: 0,
                        size: i32::try_from(width).unwrap_or(i32::MAX),
                        len: usize::try_from(stride).unwrap_or(usize::MAX),
                    }
                }
                planes::PlaneFault::ShortOfStride { need, len } => MediaCodecError::BufferRange {
                    offset: 0,
                    size: i32::try_from(need).unwrap_or(i32::MAX),
                    len,
                },
            }
        })?;
        Ok(usize::try_from(plane.stride()).unwrap_or(usize::MAX))
    }
}

/// `MediaCodec` encoder over byte buffers.
#[derive(Debug)]
pub struct MediaCodecEncoder {
    /// Lifecycle gate.
    state: EncodeState,
    /// Configured format.
    spec: EncodeSpec,
    /// Packed input layout.
    input: InputLayout,
    /// Frames queued; stamps the next presentation time.
    // Reached only from the Android module.
    #[allow(dead_code)]
    frames: u64,
    /// OS codec session.
    // Reached only from the Android module.
    #[allow(dead_code)]
    backend: Box<dyn CodecSeam>,
}

// Constructors and directors run only behind the Android module.
#[allow(dead_code)]
impl MediaCodecEncoder {
    /// `MediaCodec#BUFFER_FLAG_KEY_FRAME`.
    const BUFFER_FLAG_KEY_FRAME: u32 = 1;
    /// `MediaCodec#BUFFER_FLAG_CODEC_CONFIG`.
    const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;

    /// Encoder in `state` for `spec` with `input` layout and `backend` session.
    pub(crate) fn from_backend(
        state: EncodeState,
        spec: EncodeSpec,
        input: InputLayout,
        backend: Box<dyn CodecSeam>,
    ) -> Self {
        Self {
            state,
            spec,
            input,
            frames: 0,
            backend,
        }
    }

    /// Lifecycle state.
    #[must_use]
    pub fn state(&self) -> EncodeState {
        self.state
    }

    /// Configured format.
    #[must_use]
    pub fn spec(&self) -> &EncodeSpec {
        &self.spec
    }

    /// Packed input layout.
    #[must_use]
    pub fn input_layout(&self) -> &InputLayout {
        &self.input
    }

    /// Packet from one output range, unless config-only.
    fn packet(bytes: Bytes, flags: u32, pts_us: i64) -> Option<Packet> {
        if flags & Self::BUFFER_FLAG_CODEC_CONFIG != 0 && flags & Self::BUFFER_FLAG_KEY_FRAME == 0 {
            return None;
        }
        let mut packet =
            Packet::new(bytes).with_pts(Duration::from_micros(u64::try_from(pts_us).unwrap_or(0)));
        if flags & Self::BUFFER_FLAG_KEY_FRAME != 0 {
            packet = packet.mark_keyframe();
        }
        Some(packet)
    }

    /// Packets coded from `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the frame is not planar 4:2:0
    /// at the configured extent, when no input buffer frees inside the
    /// queue budget, or when the codec faults.
    pub(crate) fn push_frame(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("encode")?;
        let bytes = self.input.pack(frame)?;
        let pts = self.spec.pts_for(self.frames);
        let mut items = self.backend.submit(&bytes, pts)?;
        self.frames += 1;
        items.extend(self.backend.read()?);
        Ok(Self::packets(items))
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub(crate) fn drain_packets(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("flush")?;
        let items = self.backend.read()?;
        Ok(Self::packets(items))
    }

    /// Drained packets, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub(crate) fn reset_packets(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("reset")?;
        let items = self.backend.reset()?;
        Ok(Self::packets(items))
    }

    /// Encoder marked released.
    pub(crate) fn mark_released(&mut self) {
        self.state = self.state.released().unwrap_or(EncodeState::Released);
    }

    /// Packets over drained `items`, config-only outputs dropped.
    fn packets(items: Vec<SeamItem>) -> Vec<Packet> {
        items
            .into_iter()
            .filter_map(|item| match item {
                SeamItem::Output(output) => {
                    let flags = output.flags();
                    let pts_us = output.pts_us();
                    Self::packet(output.into_bytes(), flags, pts_us)
                }
                SeamItem::Format(_) => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

    use super::{EncodeSpec, EncodeState, InputLayout, MediaCodecEncoder, MediaCodecError};

    fn spec_16x16() -> EncodeSpec {
        EncodeSpec::h264(16, 16, 1_000_000, 30, 1).expect("valid spec")
    }

    fn frame_16x16() -> CpuFrame {
        CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![1_u8; 256]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![2_u8; 64]), 8).expect("valid plane"),
                Plane::new(Bytes::from(vec![3_u8; 64]), 8).expect("valid plane"),
            ],
        )
        .expect("three planes")
    }

    fn layout_16x16_planar() -> InputLayout {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        layout
            .apply_format(Some(19), Some(16), Some(16))
            .expect("valid format");
        layout
    }

    #[test]
    fn builders_carry_codec_and_rates() {
        let spec = EncodeSpec::h264(1920, 1080, 8_000_000, 60, 2).expect("valid spec");
        assert_eq!(spec.mime(), "video/avc");
        assert_eq!((spec.width(), spec.height()), (1920, 1080));
        assert_eq!((spec.bitrate(), spec.frame_rate()), (8_000_000, 60));
        assert_eq!(spec.keyframe_interval(), 2);
        assert_eq!(spec.color_format(), 19);
        let spec = EncodeSpec::h265(3840, 2160, 20_000_000, 30, 1).expect("valid spec");
        assert_eq!(spec.mime(), "video/hevc");
        let spec = EncodeSpec::av1(1280, 720, 4_000_000, 30, 0).expect("valid spec");
        assert_eq!(spec.mime(), "video/av01");
        assert_eq!(spec.keyframe_interval(), 0);
        let spec = spec.with_color_format(21);
        assert_eq!(spec.color_format(), 21);
    }

    #[test]
    fn builders_reject_bad_dims_and_rates() {
        assert!(EncodeSpec::h264(0, 1080, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::h264(1920, 0, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::h265(u32::MAX, 1080, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::av1(1920, u32::MAX, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::h264(1920, 1080, 0, 30, 1).is_none());
        assert!(EncodeSpec::h264(1920, 1080, 1_000_000, 0, 1).is_none());
    }

    #[test]
    fn keys_match_platform_constants() {
        assert_eq!(EncodeSpec::KEY_BIT_RATE, "bitrate");
        assert_eq!(EncodeSpec::KEY_I_FRAME_INTERVAL, "i-frame-interval");
    }

    #[test]
    fn min_sdk_gates_av1() {
        let h264 = EncodeSpec::h264(16, 16, 1_000_000, 30, 1).expect("valid spec");
        let h265 = EncodeSpec::h265(16, 16, 1_000_000, 30, 1).expect("valid spec");
        let av1 = EncodeSpec::av1(16, 16, 1_000_000, 30, 1).expect("valid spec");
        assert_eq!(h264.min_sdk(), 21);
        assert_eq!(h265.min_sdk(), 21);
        assert_eq!(av1.min_sdk(), 29);
    }

    #[test]
    fn pts_for_scales_frames() {
        let spec = spec_16x16();
        assert_eq!(spec.pts_for(0), 0);
        assert_eq!(spec.pts_for(30), 1_000_000);
        assert_eq!(spec.pts_for(60), 2_000_000);
        assert_eq!(spec.pts_for(u64::MAX), u64::MAX);
    }

    #[test]
    fn transitions_walk_the_lifecycle() {
        let state = EncodeState::Unconfigured
            .configured()
            .expect("configure")
            .started()
            .expect("start");
        assert_eq!(state, EncodeState::Started);
        state.ensure_started("encode").expect("started accepts");
        let state = state.released().expect("release");
        assert_eq!(state, EncodeState::Released);
    }

    #[test]
    fn transitions_reject_skips_and_repeats() {
        assert_eq!(
            EncodeState::Unconfigured.started(),
            Err(MediaCodecError::InvalidEncodeState {
                op: "start",
                state: EncodeState::Unconfigured,
            })
        );
        assert_eq!(
            EncodeState::Started.configured(),
            Err(MediaCodecError::InvalidEncodeState {
                op: "configure",
                state: EncodeState::Started,
            })
        );
        assert_eq!(
            EncodeState::Released.released(),
            Err(MediaCodecError::InvalidEncodeState {
                op: "release",
                state: EncodeState::Released,
            })
        );
        assert_eq!(
            EncodeState::Configured.ensure_started("flush"),
            Err(MediaCodecError::InvalidEncodeState {
                op: "flush",
                state: EncodeState::Configured,
            })
        );
    }

    #[test]
    fn tentative_not_negotiated() {
        let layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        assert!(!layout.negotiated());
    }

    #[test]
    fn apply_format_negotiates_layouts() {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        layout
            .apply_format(Some(21), Some(16), Some(16))
            .expect("semiplanar maps");
        assert!(layout.negotiated());
        let packed = layout.pack(&frame_16x16()).expect("semiplanar packs");
        assert_eq!(packed.len(), 384);
        assert_eq!(&packed[256..260], &[2, 3, 2, 3]);
        layout
            .apply_format(Some(19), None, Some(0))
            .expect("planar maps with fallback");
        let packed = layout.pack(&frame_16x16()).expect("planar packs");
        assert_eq!(packed.len(), 384);
        assert_eq!(&packed[256..258], &[2, 2]);
        assert_eq!(&packed[320..322], &[3, 3]);
    }

    #[test]
    fn apply_format_rejects_bad_color() {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        assert_eq!(
            layout.apply_format(Some(7), None, None),
            Err(MediaCodecError::UnsupportedColorFormat { value: 7 })
        );
        assert!(!layout.negotiated());
    }

    #[test]
    fn pack_honors_stride_padding() {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        layout
            .apply_format(Some(19), Some(32), Some(16))
            .expect("valid format");
        let packed = layout.pack(&frame_16x16()).expect("padded pack");
        assert_eq!(packed.len(), 32 * 16 + 2 * 16 * 8);
        assert_eq!(&packed[16..32], &[0_u8; 16]);
        assert_eq!(&packed[32..48], &[1_u8; 16]);
    }

    #[test]
    fn pack_declines_other_layouts() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(vec![0_u8; 256]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![0_u8; 128]), 16).expect("valid plane"),
            ],
        )
        .expect("two planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::UnsupportedInput {
                format: PixelFormat::Nv12,
            })
        );
    }

    #[test]
    fn pack_declines_extent_mismatch() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(32, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![0_u8; 512]), 32).expect("valid plane"),
                Plane::new(Bytes::from(vec![0_u8; 128]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![0_u8; 128]), 16).expect("valid plane"),
            ],
        )
        .expect("three planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::FrameMismatch {
                width: 32,
                height: 16,
                expected_width: 16,
                expected_height: 16,
            })
        );
    }

    #[test]
    fn pack_declines_short_planes() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![1_u8; 256]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![2_u8; 10]), 8).expect("valid plane"),
                Plane::new(Bytes::from(vec![3_u8; 64]), 8).expect("valid plane"),
            ],
        )
        .expect("three planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::BufferRange {
                offset: 0,
                size: 64,
                len: 10,
            })
        );
    }

    #[test]
    fn pack_declines_narrow_stride() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![1_u8; 256]), 4).expect("valid plane"),
                Plane::new(Bytes::from(vec![2_u8; 64]), 8).expect("valid plane"),
                Plane::new(Bytes::from(vec![3_u8; 64]), 8).expect("valid plane"),
            ],
        )
        .expect("three planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::BufferRange {
                offset: 0,
                size: 16,
                len: 4,
            })
        );
    }

    #[test]
    fn packet_flags_keyframes_and_config() {
        let bytes = Bytes::from_static(&[0x65, 0x88]);
        let packet = MediaCodecEncoder::packet(bytes.clone(), 1, 40_000).expect("keyframe packet");
        assert!(packet.is_keyframe());
        assert_eq!(packet.pts(), Some(Duration::from_millis(40)));
        let packet = MediaCodecEncoder::packet(bytes.clone(), 0, 80_000).expect("plain packet");
        assert!(!packet.is_keyframe());
        assert_eq!(packet.pts(), Some(Duration::from_millis(80)));
        assert_eq!(MediaCodecEncoder::packet(bytes.clone(), 2, 0), None);
        let packet =
            MediaCodecEncoder::packet(bytes.clone(), 3, -7).expect("config keyframe packet");
        assert!(packet.is_keyframe());
        assert_eq!(packet.pts(), Some(Duration::ZERO));
        let packet = MediaCodecEncoder::packet(bytes, 0, -1).expect("negative stamp packet");
        assert_eq!(packet.pts(), Some(Duration::ZERO));
    }
}
