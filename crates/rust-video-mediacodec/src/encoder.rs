//! Byte-buffer encode behind the caps `Encoder` trait.

#[cfg(target_os = "android")]
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(any(test, target_os = "android"))]
use std::time::Duration;

#[cfg(target_os = "android")]
use crate::bridge::{CodecEvent, EventQueue, PendingInputs};
#[cfg(target_os = "android")]
use crate::decoder::ranged_bytes;
use crate::error::MediaCodecError;
use crate::format::{pixel_format_for, FormatSpec};
#[cfg(target_os = "android")]
use crate::quirks::QuirkTable;
#[cfg(target_os = "android")]
use crate::sdk::ApiLevel;

use bytes::Bytes;
#[cfg(target_os = "android")]
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, InputBuffer, MediaCodec,
    MediaCodecDirection,
};
#[cfg(target_os = "android")]
use ndk::media::media_format::MediaFormat;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
#[cfg(any(test, target_os = "android"))]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "android")]
use rust_video_caps::traits::Encoder;

/// `MediaCodec#BUFFER_FLAG_KEY_FRAME`; `MediaCodecEncoder::packet` and `Inner` share it.
#[cfg(any(test, target_os = "android"))]
const BUFFER_FLAG_KEY_FRAME: u32 = 1;

/// `MediaCodec#BUFFER_FLAG_CODEC_CONFIG`; `MediaCodecEncoder::packet` and `Inner` share it.
#[cfg(any(test, target_os = "android"))]
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;

/// `MediaCodec#BUFFER_FLAG_END_OF_STREAM`; `Inner` drain loops share it.
#[cfg(target_os = "android")]
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;

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

/// Output arrival path.
#[cfg(target_os = "android")]
#[derive(Debug)]
enum DrainMode {
    /// Synchronous dequeue with timeouts.
    Sync,
    /// Async callbacks feeding a polled queue.
    Async(AsyncDrain),
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

/// Async callback state.
#[cfg(target_os = "android")]
#[derive(Debug)]
struct AsyncDrain {
    /// Callback-side event intake.
    queue: EventQueue,
    /// Input indices awaiting a frame.
    pending: PendingInputs,
}

#[cfg(target_os = "android")]
impl AsyncDrain {
    /// Drain over `queue` with an empty stash.
    fn new(queue: EventQueue) -> Self {
        Self {
            queue,
            pending: PendingInputs::new(),
        }
    }

    /// Frame queued; packets freed along the way.
    fn push(
        &mut self,
        codec: &MediaCodec,
        bytes: &[u8],
        pts: u64,
    ) -> Result<Vec<Packet>, MediaCodecError> {
        let mut packets = Vec::new();
        for _ in 0..Inner::INPUT_ATTEMPTS {
            if let Some(index) = self.pending.pop() {
                Inner::write_input(codec, index, bytes, pts)?;
                return Ok(packets);
            }
            let batch = self.queue.drain_blocking(Inner::INPUT_TIMEOUT);
            for event in self.pending.stash_inputs(batch) {
                match event {
                    CodecEvent::InputAvailable { index } => self.pending.push(index),
                    CodecEvent::OutputAvailable {
                        index,
                        offset,
                        size,
                        pts_us,
                        flags,
                        ..
                    } => {
                        if let Some(packet) =
                            Inner::pull_output(codec, index, offset, size, flags, pts_us)?
                        {
                            packets.push(packet);
                        }
                    }
                    CodecEvent::FormatChanged => {}
                    CodecEvent::Error {
                        recoverable,
                        transient,
                        kind,
                    } => {
                        return Err(MediaCodecError::CodecFault {
                            recoverable,
                            transient,
                            kind,
                        });
                    }
                }
            }
        }
        Err(MediaCodecError::InputStarved)
    }

    /// Packets collected from queued async events.
    fn drain(&mut self, codec: &MediaCodec) -> Result<Vec<Packet>, MediaCodecError> {
        let mut packets = Vec::new();
        let batch = self.queue.drain();
        for event in self.pending.stash_inputs(batch) {
            match event {
                CodecEvent::InputAvailable { index } => self.pending.push(index),
                CodecEvent::OutputAvailable {
                    index,
                    offset,
                    size,
                    pts_us,
                    flags,
                    ..
                } => {
                    if let Some(packet) =
                        Inner::pull_output(codec, index, offset, size, flags, pts_us)?
                    {
                        packets.push(packet);
                    }
                }
                CodecEvent::FormatChanged => {}
                CodecEvent::Error {
                    recoverable,
                    transient,
                    kind,
                } => {
                    return Err(MediaCodecError::CodecFault {
                        recoverable,
                        transient,
                        kind,
                    });
                }
            }
        }
        Ok(packets)
    }
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

    /// Native format for `configure`.
    ///
    /// <https://docs.rs/ndk/0.9.0/ndk/media/media_format/struct.MediaFormat.html>
    #[cfg(target_os = "android")]
    #[must_use]
    pub fn materialize(&self) -> MediaFormat {
        let mut format = MediaFormat::new();
        format.set_str(FormatSpec::KEY_MIME, self.mime());
        format.set_i32(FormatSpec::KEY_WIDTH, self.width_as_i32());
        format.set_i32(FormatSpec::KEY_HEIGHT, self.height_as_i32());
        format.set_i32(
            Self::KEY_BIT_RATE,
            i32::try_from(self.bitrate).unwrap_or(i32::MAX),
        );
        format.set_i32(
            FormatSpec::KEY_FRAME_RATE,
            i32::try_from(self.frame_rate).unwrap_or(i32::MAX),
        );
        format.set_i32(
            Self::KEY_I_FRAME_INTERVAL,
            i32::try_from(self.keyframe_interval).unwrap_or(i32::MAX),
        );
        format.set_i32(FormatSpec::KEY_COLOR_FORMAT, self.color_format);
        format
    }

    /// Width as a native `i32`; construction bounds it.
    #[cfg(target_os = "android")]
    fn width_as_i32(&self) -> i32 {
        i32::try_from(self.width).unwrap_or(i32::MAX)
    }

    /// Height as a native `i32`; construction bounds it.
    #[cfg(target_os = "android")]
    fn height_as_i32(&self) -> i32 {
        i32::try_from(self.height).unwrap_or(i32::MAX)
    }
}

/// Native codec and drain mode.
#[cfg(target_os = "android")]
#[derive(Debug)]
struct Inner {
    /// Owned codec handle.
    codec: MediaCodec,
    /// Sync dequeue or async callbacks.
    mode: DrainMode,
}

#[cfg(target_os = "android")]
impl Inner {
    /// Input attempts per frame before starvation.
    const INPUT_ATTEMPTS: u32 = 3;
    /// Dequeue wait per attempt.
    const INPUT_TIMEOUT: Duration = Duration::from_millis(10);

    /// Frame queued; packets freed along the way.
    fn push_frame(&mut self, bytes: &[u8], pts: u64) -> Result<Vec<Packet>, MediaCodecError> {
        match &mut self.mode {
            DrainMode::Sync => Self::push_sync(&self.codec, bytes, pts),
            DrainMode::Async(drain) => drain.push(&self.codec, bytes, pts),
        }
    }

    /// Frame queued through sync dequeue.
    fn push_sync(
        codec: &MediaCodec,
        bytes: &[u8],
        pts: u64,
    ) -> Result<Vec<Packet>, MediaCodecError> {
        let mut packets = Vec::new();
        for _ in 0..Self::INPUT_ATTEMPTS {
            match codec.dequeue_input_buffer(Self::INPUT_TIMEOUT)? {
                DequeuedInputBufferResult::Buffer(mut buffer) => {
                    Self::fill_buffer(&mut buffer, bytes)?;
                    codec.queue_input_buffer(buffer, 0, bytes.len(), pts, 0)?;
                    return Ok(packets);
                }
                DequeuedInputBufferResult::TryAgainLater => {
                    packets.extend(Self::drain_sync(codec)?);
                }
            }
        }
        Err(MediaCodecError::InputStarved)
    }

    /// Frame bytes copied into one input buffer.
    fn fill_buffer(buffer: &mut InputBuffer<'_>, bytes: &[u8]) -> Result<(), MediaCodecError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let dst = buffer.buffer_mut();
            if bytes.len() > dst.len() {
                return Err(MediaCodecError::FrameTooLarge {
                    len: bytes.len(),
                    capacity: dst.len(),
                });
            }
            for (slot, byte) in dst.iter_mut().zip(bytes.iter()) {
                slot.write(*byte);
            }
            Ok(())
        }));
        match outcome {
            Ok(result) => result,
            Err(_) => Err(MediaCodecError::BufferLost),
        }
    }

    /// Frame bytes queued into input buffer `index`.
    fn write_input(
        codec: &MediaCodec,
        index: usize,
        bytes: &[u8],
        pts: u64,
    ) -> Result<(), MediaCodecError> {
        let Some(dst) = codec.input_buffer(index) else {
            return Err(MediaCodecError::BufferLost);
        };
        if bytes.len() > dst.len() {
            return Err(MediaCodecError::FrameTooLarge {
                len: bytes.len(),
                capacity: dst.len(),
            });
        }
        for (slot, byte) in dst.iter_mut().zip(bytes.iter()) {
            slot.write(*byte);
        }
        codec.queue_input_buffer_by_index(index, 0, bytes.len(), pts, 0)?;
        Ok(())
    }

    /// Available outputs as packets.
    fn drain_packets(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        match &mut self.mode {
            DrainMode::Sync => Self::drain_sync(&self.codec),
            DrainMode::Async(drain) => drain.drain(&self.codec),
        }
    }

    /// Packets collected through sync dequeue.
    fn drain_sync(codec: &MediaCodec) -> Result<Vec<Packet>, MediaCodecError> {
        let mut packets = Vec::new();
        loop {
            match codec.dequeue_output_buffer(Duration::ZERO)? {
                DequeuedOutputBufferInfoResult::Buffer(buffer) => {
                    let info = *buffer.info();
                    let caught = catch_unwind(AssertUnwindSafe(|| buffer.buffer()));
                    let Ok(slice) = caught else {
                        let _released = codec.release_output_buffer(buffer, false);
                        return Err(MediaCodecError::BufferLost);
                    };
                    let bytes = match ranged_bytes(slice, info.offset(), info.size()) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            let _released = codec.release_output_buffer(buffer, false);
                            return Err(error);
                        }
                    };
                    let flags = info.flags();
                    let pts = info.presentation_time_us();
                    codec.release_output_buffer(buffer, false)?;
                    if flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                        continue;
                    }
                    if let Some(packet) = MediaCodecEncoder::packet(bytes, flags, pts) {
                        packets.push(packet);
                    }
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged
                | DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
                DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(packets),
            }
        }
    }

    /// Packet pulled from output buffer `index`, unless config-only.
    fn pull_output(
        codec: &MediaCodec,
        index: usize,
        offset: i32,
        size: i32,
        flags: u32,
        pts_us: i64,
    ) -> Result<Option<Packet>, MediaCodecError> {
        if flags & BUFFER_FLAG_END_OF_STREAM != 0 {
            codec.release_output_buffer_by_index(index, false)?;
            return Ok(None);
        }
        let Some(buffer) = codec.output_buffer(index) else {
            return Err(MediaCodecError::BufferLost);
        };
        let bytes = ranged_bytes(buffer, offset, size);
        codec.release_output_buffer_by_index(index, false)?;
        Ok(MediaCodecEncoder::packet(bytes?, flags, pts_us))
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
        let mut packed = Vec::new();
        match self.layout {
            Layout::Planar => {
                let half = self.stride / 2;
                Self::copy_rows(
                    &mut packed,
                    y,
                    self.size.width(),
                    self.size.height(),
                    self.stride,
                )?;
                Self::copy_rows(
                    &mut packed,
                    u,
                    self.size.width().div_ceil(2),
                    self.size.height().div_ceil(2),
                    half,
                )?;
                Self::copy_rows(
                    &mut packed,
                    v,
                    self.size.width().div_ceil(2),
                    self.size.height().div_ceil(2),
                    half,
                )?;
            }
            Layout::Nv12 => {
                Self::copy_rows(
                    &mut packed,
                    y,
                    self.size.width(),
                    self.size.height(),
                    self.stride,
                )?;
                Self::interleave_rows(
                    &mut packed,
                    u,
                    v,
                    self.size.width().div_ceil(2),
                    self.size.height().div_ceil(2),
                    self.stride,
                )?;
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
        for r in 0..rows {
            let start = r.saturating_mul(stride);
            let end = start.saturating_add(row);
            dst.extend_from_slice(&plane.data()[start..end]);
            dst.extend(std::iter::repeat_n(0, pitch.saturating_sub(row)));
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
        for r in 0..rows {
            let u_start = r.saturating_mul(u_stride);
            let v_start = r.saturating_mul(v_stride);
            let u_row = &u.data()[u_start..u_start.saturating_add(row)];
            let v_row = &v.data()[v_start..v_start.saturating_add(row)];
            for (u_byte, v_byte) in u_row.iter().zip(v_row.iter()) {
                dst.push(*u_byte);
                dst.push(*v_byte);
            }
            dst.extend(std::iter::repeat_n(
                0,
                pitch.saturating_sub(row.saturating_mul(2)),
            ));
        }
        Ok(())
    }

    /// Plane stride after the stride and length checks.
    fn checked_stride(plane: &Plane, width: u32, height: u32) -> Result<usize, MediaCodecError> {
        if plane.stride() < width {
            return Err(MediaCodecError::BufferRange {
                offset: 0,
                size: i32::try_from(width).unwrap_or(i32::MAX),
                len: usize::try_from(plane.stride()).unwrap_or(usize::MAX),
            });
        }
        let stride = usize::try_from(plane.stride()).unwrap_or(usize::MAX);
        let row = usize::try_from(width).unwrap_or(usize::MAX);
        let rows = usize::try_from(height).unwrap_or(usize::MAX);
        let needed = stride
            .saturating_mul(rows.saturating_sub(1))
            .saturating_add(row);
        if plane.data().len() < needed {
            return Err(MediaCodecError::BufferRange {
                offset: 0,
                size: i32::try_from(needed).unwrap_or(i32::MAX),
                len: plane.data().len(),
            });
        }
        Ok(stride)
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
    /// Matched quirk identifiers.
    applied: Vec<String>,
    /// Frames queued; stamps the next presentation time.
    #[cfg(target_os = "android")]
    frames: u64,
    /// Native codec and drain mode.
    #[cfg(target_os = "android")]
    inner: Inner,
}

impl MediaCodecEncoder {
    /// Encoder for `spec` on device `soc` with `quirks`.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec output format.
    /// * `soc` - device `SoC` name for quirk matching.
    /// * `quirks` - quirk table.
    ///
    /// # Errors
    ///
    /// Returns `Err` below the device floor, when the input color has no
    /// plane layout, or when no encoder answers the mime.
    #[cfg(target_os = "android")]
    pub fn open(
        spec: &EncodeSpec,
        soc: &str,
        quirks: &QuirkTable,
    ) -> Result<Self, MediaCodecError> {
        Self::open_android(spec, soc, quirks)
    }

    /// Native encoder after the SDK gates.
    #[cfg(target_os = "android")]
    fn open_android(
        spec: &EncodeSpec,
        soc: &str,
        quirks: &QuirkTable,
    ) -> Result<Self, MediaCodecError> {
        let level = ApiLevel::current();
        if !level.meets_min() || !level.at_least(spec.min_sdk()) {
            return Err(MediaCodecError::BelowMinSdk { found: level.get() });
        }
        let mut codec = MediaCodec::from_encoder_type(spec.mime())
            .ok_or(MediaCodecError::NoEncoder { mime: spec.mime() })?;
        let state = EncodeState::Unconfigured.configured()?;
        codec.configure(&spec.materialize(), None, MediaCodecDirection::Encoder)?;
        let mode = if level.supports_async_notify() {
            let queue = EventQueue::new();
            codec.set_async_notify_callback(Some(queue.async_callbacks()))?;
            DrainMode::Async(AsyncDrain::new(queue))
        } else {
            DrainMode::Sync
        };
        codec.start()?;
        let state = state.started()?;
        let applied = quirks
            .matching(soc, spec.mime())
            .iter()
            .map(|quirk| quirk.id().to_owned())
            .collect();
        let mut input = InputLayout::tentative(spec).ok_or(MediaCodecError::BadExtent {
            width: i32::try_from(spec.width()).unwrap_or(i32::MAX),
            height: i32::try_from(spec.height()).unwrap_or(i32::MAX),
        })?;
        let format = codec.input_format();
        input.apply_format(
            format.i32(FormatSpec::KEY_COLOR_FORMAT),
            format.i32(FormatSpec::KEY_STRIDE),
            format.i32(FormatSpec::KEY_SLICE_HEIGHT),
        )?;
        Ok(Self {
            state,
            spec: spec.clone(),
            input,
            applied,
            frames: 0,
            inner: Inner { codec, mode },
        })
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

    /// Quirk identifiers matched at open.
    #[must_use]
    pub fn applied_quirks(&self) -> &[String] {
        &self.applied
    }

    /// Packet from one output range, unless config-only.
    #[cfg(any(test, target_os = "android"))]
    fn packet(bytes: Bytes, flags: u32, pts_us: i64) -> Option<Packet> {
        if flags & BUFFER_FLAG_CODEC_CONFIG != 0 && flags & BUFFER_FLAG_KEY_FRAME == 0 {
            return None;
        }
        let mut packet =
            Packet::new(bytes).with_pts(Duration::from_micros(u64::try_from(pts_us).unwrap_or(0)));
        if flags & BUFFER_FLAG_KEY_FRAME != 0 {
            packet = packet.mark_keyframe();
        }
        Some(packet)
    }

    /// Drained packets, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    #[cfg(target_os = "android")]
    pub fn reset(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("reset")?;
        let packets = self.inner.drain_packets()?;
        if let DrainMode::Async(drain) = &mut self.inner.mode {
            drain.pending.clear();
        }
        self.inner.codec.flush()?;
        if matches!(self.inner.mode, DrainMode::Async(_)) {
            self.inner.codec.start()?;
        }
        Ok(packets)
    }
}

#[cfg(target_os = "android")]
impl Encoder for MediaCodecEncoder {
    type Error = MediaCodecError;

    /// Packets coded from `frame`, possibly none.
    ///
    /// # Arguments
    ///
    /// * `frame` - raw input frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the frame is not planar 4:2:0
    /// at the configured extent, or when the codec faults.
    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        self.state.ensure_started("encode")?;
        let bytes = self.input.pack(frame)?;
        let pts = self.spec.pts_for(self.frames);
        let mut packets = self.inner.push_frame(&bytes, pts)?;
        self.frames += 1;
        packets.extend(self.inner.drain_packets()?);
        Ok(packets)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        self.state.ensure_started("flush")?;
        self.inner.drain_packets()
    }
}

#[cfg(target_os = "android")]
impl Drop for MediaCodecEncoder {
    /// Codec stopped; buffers release with the handle.
    fn drop(&mut self) {
        let _stopped = self.inner.codec.stop();
        self.state = self.state.released().unwrap_or(EncodeState::Released);
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
