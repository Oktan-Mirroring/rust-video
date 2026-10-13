//! Byte-buffer decode behind the facade `Decoder` trait.

#[cfg(target_os = "android")]
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(target_os = "android")]
use std::time::Duration;

#[cfg(target_os = "android")]
use crate::bridge::{CodecEvent, EventQueue};
use crate::error::MediaCodecError;
use crate::format::{pixel_format_for, FormatSpec};
use crate::quirks::QuirkTable;
#[cfg(target_os = "android")]
use crate::sdk::ApiLevel;

use bytes::Bytes;
#[cfg(target_os = "android")]
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, InputBuffer, MediaCodec,
    MediaCodecDirection,
};
use rust_video::{CpuFrame, Decoder, Packet, PixelFormat, Plane};
use rust_video_caps::common::PictureSize;

/// Decoder lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeState {
    /// Created, no format configured.
    Unconfigured,
    /// Format configured, not started.
    Configured,
    /// Started, accepts packets.
    Started,
    /// Released, unusable.
    Released,
}

impl DecodeState {
    /// State after `configure`.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless the decoder is unconfigured.
    pub fn configured(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Unconfigured => Ok(Self::Configured),
            other => Err(MediaCodecError::InvalidState {
                op: "configure",
                state: other,
            }),
        }
    }

    /// State after `start`.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless the decoder is configured.
    pub fn started(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Configured => Ok(Self::Started),
            other => Err(MediaCodecError::InvalidState {
                op: "start",
                state: other,
            }),
        }
    }

    /// State after `release`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the decoder is already released.
    pub fn released(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Released => Err(MediaCodecError::InvalidState {
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
            Err(MediaCodecError::InvalidState { op, state: self })
        }
    }
}

/// Output arrival path.
#[cfg(target_os = "android")]
#[derive(Debug)]
enum DrainMode {
    /// Synchronous dequeue with timeouts.
    Sync,
    /// Async callbacks feeding a polled queue.
    Async(EventQueue),
}

/// Byte-buffer plane arrangement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layout {
    /// Semi-planar Y plus interleaved UV.
    Nv12,
    /// Planar Y, U, V.
    Planar,
}

impl Layout {
    /// Facade pixel layout.
    fn pixel_format(self) -> PixelFormat {
        match self {
            Self::Nv12 => PixelFormat::Nv12,
            Self::Planar => PixelFormat::Yuv420P,
        }
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
    /// Input attempts per packet before starvation.
    const INPUT_ATTEMPTS: u32 = 3;
    /// Dequeue wait per attempt.
    const INPUT_TIMEOUT: Duration = Duration::from_millis(10);
    /// `MediaCodec#BUFFER_FLAG_DECODE_ONLY`.
    const BUFFER_FLAG_DECODE_ONLY: u32 = 32;

    /// Packet queued; frames freed along the way.
    fn push_packet(
        &self,
        packet: &Packet,
        output: &mut OutputLayout,
    ) -> Result<Vec<CpuFrame>, MediaCodecError> {
        match &self.mode {
            DrainMode::Sync => self.push_sync(packet, output),
            DrainMode::Async(queue) => self.push_async(packet, queue, output),
        }
    }

    /// Packet queued through sync dequeue.
    fn push_sync(
        &self,
        packet: &Packet,
        output: &mut OutputLayout,
    ) -> Result<Vec<CpuFrame>, MediaCodecError> {
        let mut frames = Vec::new();
        for _ in 0..Self::INPUT_ATTEMPTS {
            match self.codec.dequeue_input_buffer(Self::INPUT_TIMEOUT)? {
                DequeuedInputBufferResult::Buffer(mut buffer) => {
                    Self::fill_buffer(&mut buffer, packet.data())?;
                    self.codec.queue_input_buffer(
                        buffer,
                        0,
                        packet.data().len(),
                        MediaCodecDecoder::pts_us(packet),
                        0,
                    )?;
                    return Ok(frames);
                }
                DequeuedInputBufferResult::TryAgainLater => {
                    frames.extend(self.drain_outputs(output)?);
                }
            }
        }
        Err(MediaCodecError::InputStarved)
    }

    /// Packet bytes copied into one input buffer.
    fn fill_buffer(buffer: &mut InputBuffer<'_>, data: &[u8]) -> Result<(), MediaCodecError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let dst = buffer.buffer_mut();
            if data.len() > dst.len() {
                return Err(MediaCodecError::InputTooLarge {
                    len: data.len(),
                    capacity: dst.len(),
                });
            }
            for (slot, byte) in dst.iter_mut().zip(data.iter()) {
                slot.write(*byte);
            }
            Ok(())
        }));
        match outcome {
            Ok(result) => result,
            Err(_) => Err(MediaCodecError::BufferLost),
        }
    }

    /// Packet queued through async input events.
    fn push_async(
        &self,
        packet: &Packet,
        queue: &EventQueue,
        output: &mut OutputLayout,
    ) -> Result<Vec<CpuFrame>, MediaCodecError> {
        let mut frames = Vec::new();
        let mut queued = false;
        for _ in 0..Self::INPUT_ATTEMPTS {
            for event in queue.drain_blocking(Self::INPUT_TIMEOUT) {
                match event {
                    CodecEvent::InputAvailable { index } if !queued => {
                        self.write_input(index, packet)?;
                        queued = true;
                    }
                    CodecEvent::InputAvailable { index } => {
                        let _sent = queue.sender().send(CodecEvent::InputAvailable { index });
                    }
                    CodecEvent::OutputAvailable {
                        index,
                        offset,
                        size,
                        flags,
                        ..
                    } => {
                        if let Some(frame) = self.pull_output(index, offset, size, flags, output)? {
                            frames.push(frame);
                        }
                    }
                    CodecEvent::FormatChanged => {
                        self.refresh_output(output)?;
                    }
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
            if queued {
                return Ok(frames);
            }
        }
        Err(MediaCodecError::InputStarved)
    }

    /// Packet bytes queued into input buffer `index`.
    fn write_input(&self, index: usize, packet: &Packet) -> Result<(), MediaCodecError> {
        let Some(dst) = self.codec.input_buffer(index) else {
            return Err(MediaCodecError::BufferLost);
        };
        if packet.data().len() > dst.len() {
            return Err(MediaCodecError::InputTooLarge {
                len: packet.data().len(),
                capacity: dst.len(),
            });
        }
        for (slot, byte) in dst.iter_mut().zip(packet.data().iter()) {
            slot.write(*byte);
        }
        self.codec.queue_input_buffer_by_index(
            index,
            0,
            packet.data().len(),
            MediaCodecDecoder::pts_us(packet),
            0,
        )?;
        Ok(())
    }

    /// Available outputs as frames.
    fn drain_outputs(&self, output: &mut OutputLayout) -> Result<Vec<CpuFrame>, MediaCodecError> {
        match &self.mode {
            DrainMode::Sync => self.drain_sync(output),
            DrainMode::Async(queue) => self.drain_async(queue, output),
        }
    }

    /// Outputs collected through sync dequeue.
    fn drain_sync(&self, output: &mut OutputLayout) -> Result<Vec<CpuFrame>, MediaCodecError> {
        let mut frames = Vec::new();
        loop {
            match self.codec.dequeue_output_buffer(Duration::ZERO)? {
                DequeuedOutputBufferInfoResult::Buffer(buffer) => {
                    let info = *buffer.info();
                    if info.flags() & Self::BUFFER_FLAG_DECODE_ONLY != 0 {
                        self.codec.release_output_buffer(buffer, false)?;
                        continue;
                    }
                    if !output.negotiated() {
                        self.refresh_output(output)?;
                    }
                    let caught = catch_unwind(AssertUnwindSafe(|| buffer.buffer()));
                    let Ok(slice) = caught else {
                        let _released = self.codec.release_output_buffer(buffer, false);
                        return Err(MediaCodecError::BufferLost);
                    };
                    let bytes = match ranged_bytes(slice, info.offset(), info.size()) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            let _released = self.codec.release_output_buffer(buffer, false);
                            return Err(error);
                        }
                    };
                    self.codec.release_output_buffer(buffer, false)?;
                    frames.push(output.split(bytes)?);
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    self.refresh_output(output)?;
                }
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
                DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(frames),
            }
        }
    }

    /// Outputs collected from queued async events.
    fn drain_async(
        &self,
        queue: &EventQueue,
        output: &mut OutputLayout,
    ) -> Result<Vec<CpuFrame>, MediaCodecError> {
        let mut frames = Vec::new();
        for event in queue.drain() {
            match event {
                CodecEvent::InputAvailable { index } => {
                    let _sent = queue.sender().send(CodecEvent::InputAvailable { index });
                }
                CodecEvent::OutputAvailable {
                    index,
                    offset,
                    size,
                    flags,
                    ..
                } => {
                    if let Some(frame) = self.pull_output(index, offset, size, flags, output)? {
                        frames.push(frame);
                    }
                }
                CodecEvent::FormatChanged => {
                    self.refresh_output(output)?;
                }
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
        Ok(frames)
    }

    /// Frame pulled from output buffer `index`, unless decode-only.
    fn pull_output(
        &self,
        index: usize,
        offset: i32,
        size: i32,
        flags: u32,
        output: &mut OutputLayout,
    ) -> Result<Option<CpuFrame>, MediaCodecError> {
        if flags & Self::BUFFER_FLAG_DECODE_ONLY != 0 {
            self.codec.release_output_buffer_by_index(index, false)?;
            return Ok(None);
        }
        if !output.negotiated() {
            self.refresh_output(output)?;
        }
        let Some(buffer) = self.codec.output_buffer(index) else {
            return Err(MediaCodecError::BufferLost);
        };
        let bytes = ranged_bytes(buffer, offset, size);
        let _released = self.codec.release_output_buffer_by_index(index, false);
        Ok(Some(output.split(bytes?)?))
    }

    /// Layout refreshed from the current output format.
    fn refresh_output(&self, output: &mut OutputLayout) -> Result<(), MediaCodecError> {
        let format = self.codec.output_format();
        output.apply_format(
            format.i32(FormatSpec::KEY_WIDTH),
            format.i32(FormatSpec::KEY_HEIGHT),
            format.i32(FormatSpec::KEY_COLOR_FORMAT),
            format.i32(FormatSpec::KEY_STRIDE),
            format.i32(FormatSpec::KEY_SLICE_HEIGHT),
        )
    }
}

/// `MediaCodec` decoder over byte buffers.
#[derive(Debug)]
pub struct MediaCodecDecoder {
    /// Lifecycle gate.
    state: DecodeState,
    /// Negotiated output layout.
    output: OutputLayout,
    /// Matched quirk identifiers.
    applied: Vec<String>,
    /// Native codec and drain mode.
    #[cfg(target_os = "android")]
    inner: Inner,
}

impl MediaCodecDecoder {
    /// Decoder for `spec` on device `soc` with `quirks`.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec input format.
    /// * `soc` - device `SoC` name for quirk matching.
    /// * `quirks` - quirk table.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Android, below the device floor, or when no decoder
    /// answers the mime.
    pub fn open(
        spec: &FormatSpec,
        soc: &str,
        quirks: &QuirkTable,
    ) -> Result<Self, MediaCodecError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (spec, soc, quirks);
            Err(MediaCodecError::BackendAbsent)
        }
        #[cfg(target_os = "android")]
        Self::open_android(spec, soc, quirks)
    }

    /// Native decoder after the SDK gates.
    #[cfg(target_os = "android")]
    fn open_android(
        spec: &FormatSpec,
        soc: &str,
        quirks: &QuirkTable,
    ) -> Result<Self, MediaCodecError> {
        let level = ApiLevel::current();
        if !level.meets_min() || !level.at_least(spec.min_sdk()) {
            return Err(MediaCodecError::BelowMinSdk { found: level.get() });
        }
        let mut codec = MediaCodec::from_decoder_type(spec.mime())
            .ok_or(MediaCodecError::NoDecoder { mime: spec.mime() })?;
        let state = DecodeState::Unconfigured.configured()?;
        codec.configure(&spec.materialize(), None, MediaCodecDirection::Decoder)?;
        let mode = if level.supports_async_notify() {
            let queue = EventQueue::new();
            codec.set_async_notify_callback(Some(queue.async_callbacks()))?;
            DrainMode::Async(queue)
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
        let output = OutputLayout::tentative(spec).ok_or(MediaCodecError::BadExtent {
            width: i32::try_from(spec.width()).unwrap_or(i32::MAX),
            height: i32::try_from(spec.height()).unwrap_or(i32::MAX),
        })?;
        Ok(Self {
            state,
            output,
            applied,
            inner: Inner { codec, mode },
        })
    }

    /// Lifecycle state.
    #[must_use]
    pub fn state(&self) -> DecodeState {
        self.state
    }

    /// Negotiated output layout.
    #[must_use]
    pub fn output_layout(&self) -> &OutputLayout {
        &self.output
    }

    /// Quirk identifiers matched at open.
    #[must_use]
    pub fn applied_quirks(&self) -> &[String] {
        &self.applied
    }

    /// Presentation time in microseconds; unstamped packets read zero.
    #[must_use]
    pub fn pts_us(packet: &Packet) -> u64 {
        packet
            .pts()
            .map_or(0, |pts| u64::try_from(pts.as_micros()).unwrap_or(u64::MAX))
    }

    /// Drained frames, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub fn reset(&mut self) -> Result<Vec<CpuFrame>, MediaCodecError> {
        self.state.ensure_started("reset")?;
        #[cfg(target_os = "android")]
        {
            let frames = self.inner.drain_outputs(&mut self.output)?;
            self.inner.codec.flush()?;
            if matches!(self.inner.mode, DrainMode::Async(_)) {
                self.inner.codec.start()?;
            }
            Ok(frames)
        }
        #[cfg(not(target_os = "android"))]
        Err(MediaCodecError::BackendAbsent)
    }
}

impl Decoder for MediaCodecDecoder {
    type Frame = CpuFrame;
    type Error = MediaCodecError;

    /// Frames decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, off Android, or when the codec faults.
    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.state.ensure_started("decode")?;
        #[cfg(target_os = "android")]
        {
            let mut frames = self.inner.push_packet(packet, &mut self.output)?;
            frames.extend(self.inner.drain_outputs(&mut self.output)?);
            Ok(frames)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = packet;
            Err(MediaCodecError::BackendAbsent)
        }
    }

    /// Frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, off Android, or when the codec faults.
    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.state.ensure_started("flush")?;
        #[cfg(target_os = "android")]
        return self.inner.drain_outputs(&mut self.output);
        #[cfg(not(target_os = "android"))]
        Err(MediaCodecError::BackendAbsent)
    }
}

#[cfg(target_os = "android")]
impl Drop for MediaCodecDecoder {
    /// Codec stopped; buffers release with the handle.
    fn drop(&mut self) {
        let _stopped = self.inner.codec.stop();
        self.state = self.state.released().unwrap_or(DecodeState::Released);
    }
}

/// Negotiated output geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputLayout {
    /// Picture dimensions in luma samples.
    size: PictureSize,
    /// Plane arrangement.
    layout: Layout,
    /// Luma bytes per row.
    stride: u32,
    /// Luma rows.
    slice_height: u32,
    /// True after an output format landed.
    negotiated: bool,
}

impl OutputLayout {
    /// Initial guess from `spec`, ahead of the first output format.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec format carrying the coded dimensions.
    #[must_use]
    pub fn tentative(spec: &FormatSpec) -> Option<Self> {
        PictureSize::new(spec.width(), spec.height()).map(|size| Self {
            size,
            layout: Layout::Nv12,
            stride: spec.width(),
            slice_height: spec.height(),
            negotiated: false,
        })
    }

    /// Layout from raw output format readings.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or missing is rejected.
    /// * `height` - coded height; zero or missing is rejected.
    /// * `color` - `color-format` value; unmapped values are rejected.
    /// * `stride` - row stride; missing or non-positive falls back to width.
    /// * `slice_height` - row count; missing or non-positive falls back to height.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the extent or the color has no plane layout.
    pub fn apply_format(
        &mut self,
        width: Option<i32>,
        height: Option<i32>,
        color: Option<i32>,
        stride: Option<i32>,
        slice_height: Option<i32>,
    ) -> Result<(), MediaCodecError> {
        let width_raw = width.unwrap_or(0);
        let height_raw = height.unwrap_or(0);
        let Some(size) = PictureSize::new(
            u32::try_from(width_raw).unwrap_or(0),
            u32::try_from(height_raw).unwrap_or(0),
        ) else {
            return Err(MediaCodecError::BadExtent {
                width: width_raw,
                height: height_raw,
            });
        };
        let color = color.unwrap_or(-1);
        let format = pixel_format_for(color)
            .ok_or(MediaCodecError::UnsupportedColorFormat { value: color })?;
        let layout = match format {
            PixelFormat::Nv12 => Layout::Nv12,
            PixelFormat::Yuv420P => Layout::Planar,
            PixelFormat::Rgba8
            | PixelFormat::Bgra8
            | PixelFormat::Yuv422P
            | PixelFormat::Yuv444P
            | _ => {
                return Err(MediaCodecError::UnsupportedColorFormat { value: color });
            }
        };
        self.size = size;
        self.layout = layout;
        self.stride = positive_or(stride, size.width());
        self.slice_height = positive_or(slice_height, size.height());
        self.negotiated = true;
        Ok(())
    }

    /// True after an output format landed.
    #[must_use]
    pub fn negotiated(&self) -> bool {
        self.negotiated
    }

    /// Frame split from one contiguous output buffer.
    ///
    /// # Arguments
    ///
    /// * `bytes` - full output buffer in row-major order.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the buffer is short for the layout.
    pub fn split(&self, bytes: Bytes) -> Result<CpuFrame, MediaCodecError> {
        let y_len = u64::from(self.stride) * u64::from(self.slice_height);
        let planes = match self.layout {
            Layout::Nv12 => {
                let uv_len = y_len / 2;
                Self::check_len(bytes.len(), y_len + uv_len)?;
                let mut rest = bytes;
                let y = rest.split_to(fit(y_len, rest.len()));
                let uv = rest.split_to(fit(uv_len, rest.len()));
                vec![self.plane(y, self.stride)?, self.plane(uv, self.stride)?]
            }
            Layout::Planar => {
                let chroma_len = y_len / 4;
                Self::check_len(bytes.len(), y_len + chroma_len * 2)?;
                let mut rest = bytes;
                let y = rest.split_to(fit(y_len, rest.len()));
                let u = rest.split_to(fit(chroma_len, rest.len()));
                let v = rest.split_to(fit(chroma_len, rest.len()));
                let half = self.stride / 2;
                vec![
                    self.plane(y, self.stride)?,
                    self.plane(u, half)?,
                    self.plane(v, half)?,
                ]
            }
        };
        CpuFrame::new(self.size, self.layout.pixel_format(), planes)
            .ok_or_else(|| self.bad_extent())
    }

    /// Rejection when `len` falls short of `total` bytes.
    fn check_len(len: usize, total: u64) -> Result<(), MediaCodecError> {
        if u64::try_from(len).unwrap_or(u64::MAX) < total {
            return Err(MediaCodecError::BufferRange {
                offset: 0,
                size: i32::try_from(total).unwrap_or(i32::MAX),
                len,
            });
        }
        Ok(())
    }

    /// Plane over `data` with `stride`, or the layout extent error.
    fn plane(&self, data: Bytes, stride: u32) -> Result<Plane, MediaCodecError> {
        Plane::new(data, stride).ok_or_else(|| self.bad_extent())
    }

    /// Extent error carrying this layout.
    fn bad_extent(&self) -> MediaCodecError {
        MediaCodecError::BadExtent {
            width: i32::try_from(self.size.width()).unwrap_or(i32::MAX),
            height: i32::try_from(self.size.height()).unwrap_or(i32::MAX),
        }
    }
}

/// Positive `value`, else `fallback`.
fn positive_or(value: Option<i32>, fallback: u32) -> u32 {
    value
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(fallback)
}

/// `value` narrowed to `usize`; `len` caps the unreachable fallback.
fn fit(value: u64, len: usize) -> usize {
    usize::try_from(value).unwrap_or(len)
}

/// Valid range of `buffer` copied out.
///
/// # Arguments
///
/// * `buffer` - full codec buffer.
/// * `offset` - valid range start.
/// * `size` - valid range length.
///
/// # Errors
///
/// Returns `Err` when the range leaves the buffer.
pub fn ranged_bytes(buffer: &[u8], offset: i32, size: i32) -> Result<Bytes, MediaCodecError> {
    let range = || MediaCodecError::BufferRange {
        offset,
        size,
        len: buffer.len(),
    };
    let start = usize::try_from(offset).map_err(|_| range())?;
    let len = usize::try_from(size).map_err(|_| range())?;
    let end = start.checked_add(len).ok_or_else(range)?;
    if end > buffer.len() {
        return Err(range());
    }
    Ok(Bytes::copy_from_slice(&buffer[start..end]))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use rust_video::{Packet, PixelFormat};

    use super::{ranged_bytes, DecodeState, MediaCodecDecoder, MediaCodecError, OutputLayout};

    use crate::format::FormatSpec;
    #[cfg(not(target_os = "android"))]
    use crate::quirks::QuirkTable;

    fn layout_16x16_nv12() -> OutputLayout {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let mut layout = OutputLayout::tentative(&spec).expect("valid tentative");
        layout
            .apply_format(Some(16), Some(16), Some(21), Some(16), Some(16))
            .expect("valid format");
        layout
    }

    #[test]
    fn transitions_walk_the_lifecycle() {
        let state = DecodeState::Unconfigured
            .configured()
            .expect("configure")
            .started()
            .expect("start");
        assert_eq!(state, DecodeState::Started);
        state.ensure_started("decode").expect("started accepts");
        let state = state.released().expect("release");
        assert_eq!(state, DecodeState::Released);
    }

    #[test]
    fn transitions_reject_skips_and_repeats() {
        assert_eq!(
            DecodeState::Unconfigured.started(),
            Err(MediaCodecError::InvalidState {
                op: "start",
                state: DecodeState::Unconfigured,
            })
        );
        assert_eq!(
            DecodeState::Started.configured(),
            Err(MediaCodecError::InvalidState {
                op: "configure",
                state: DecodeState::Started,
            })
        );
        assert_eq!(
            DecodeState::Released.released(),
            Err(MediaCodecError::InvalidState {
                op: "release",
                state: DecodeState::Released,
            })
        );
        assert_eq!(
            DecodeState::Configured.ensure_started("flush"),
            Err(MediaCodecError::InvalidState {
                op: "flush",
                state: DecodeState::Configured,
            })
        );
    }

    #[test]
    fn tentative_not_negotiated() {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let layout = OutputLayout::tentative(&spec).expect("valid tentative");
        assert!(!layout.negotiated());
    }

    #[test]
    fn apply_format_honors_stride_padding() {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let mut layout = OutputLayout::tentative(&spec).expect("valid tentative");
        layout
            .apply_format(Some(16), Some(16), Some(21), Some(32), Some(16))
            .expect("valid format");
        assert!(layout.negotiated());
        let bytes = Bytes::from(vec![7_u8; 32 * 16 + 16 * 16]);
        let frame = layout.split(bytes).expect("padded split");
        assert_eq!(frame.format(), PixelFormat::Nv12);
        assert_eq!(frame.planes()[0].stride(), 32);
        assert_eq!(frame.planes()[1].stride(), 32);
    }

    #[test]
    fn apply_format_falls_back_to_dims() {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let mut layout = OutputLayout::tentative(&spec).expect("valid tentative");
        layout
            .apply_format(Some(16), Some(16), Some(19), None, Some(0))
            .expect("valid format");
        let bytes = Bytes::from(vec![7_u8; 16 * 16 * 3 / 2]);
        let frame = layout.split(bytes).expect("fallback split");
        assert_eq!(frame.format(), PixelFormat::Yuv420P);
        assert_eq!(frame.planes()[0].stride(), 16);
        assert_eq!(frame.planes()[1].stride(), 8);
    }

    #[test]
    fn apply_format_rejects_bad_extent_and_color() {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let mut layout = OutputLayout::tentative(&spec).expect("valid tentative");
        assert_eq!(
            layout.apply_format(Some(0), Some(16), Some(21), None, None),
            Err(MediaCodecError::BadExtent {
                width: 0,
                height: 16
            })
        );
        assert_eq!(
            layout.apply_format(None, None, Some(21), None, None),
            Err(MediaCodecError::BadExtent {
                width: 0,
                height: 0
            })
        );
        assert_eq!(
            layout.apply_format(Some(16), Some(16), Some(7), None, None),
            Err(MediaCodecError::UnsupportedColorFormat { value: 7 })
        );
        assert!(!layout.negotiated());
    }

    #[test]
    fn split_rejects_short_buffers() {
        let layout = layout_16x16_nv12();
        let bytes = Bytes::from(vec![0_u8; 10]);
        assert_eq!(
            layout.split(bytes),
            Err(MediaCodecError::BufferRange {
                offset: 0,
                size: 384,
                len: 10,
            })
        );
    }

    #[test]
    fn split_shares_backing_without_copy() {
        let layout = layout_16x16_nv12();
        let bytes = Bytes::from(vec![3_u8; 16 * 16 * 3 / 2]);
        let base = bytes.as_ptr();
        let frame = layout.split(bytes).expect("exact split");
        assert_eq!(frame.planes().len(), 2);
        assert_eq!(frame.planes()[0].data().as_ptr(), base);
    }

    #[test]
    fn ranged_bytes_clips_valid_ranges() {
        let buffer = [0_u8, 1, 2, 3, 4, 5];
        assert_eq!(
            ranged_bytes(&buffer, 2, 3).expect("inside").as_ref(),
            &[2, 3, 4]
        );
        assert_eq!(
            ranged_bytes(&buffer, -1, 2),
            Err(MediaCodecError::BufferRange {
                offset: -1,
                size: 2,
                len: 6,
            })
        );
        assert_eq!(
            ranged_bytes(&buffer, 4, 3),
            Err(MediaCodecError::BufferRange {
                offset: 4,
                size: 3,
                len: 6,
            })
        );
        assert_eq!(
            ranged_bytes(&buffer, 0, -2),
            Err(MediaCodecError::BufferRange {
                offset: 0,
                size: -2,
                len: 6,
            })
        );
    }

    #[test]
    fn pts_converts_and_defaults() {
        let stamped = Packet::new(Bytes::from_static(&[0])).with_pts(Duration::from_millis(40));
        assert_eq!(MediaCodecDecoder::pts_us(&stamped), 40_000);
        let bare = Packet::new(Bytes::from_static(&[0]));
        assert_eq!(MediaCodecDecoder::pts_us(&bare), 0);
        let huge = Packet::new(Bytes::from_static(&[0])).with_pts(Duration::from_secs(u64::MAX));
        assert_eq!(MediaCodecDecoder::pts_us(&huge), u64::MAX);
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn open_reports_absent_off_android() {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let quirks = QuirkTable::load().expect("embedded table parses");
        assert!(matches!(
            MediaCodecDecoder::open(&spec, "test", &quirks),
            Err(MediaCodecError::BackendAbsent)
        ));
    }
}
