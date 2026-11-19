#![allow(unsafe_code)]
//! Android codec session behind the crate seam.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use crate::bridge::{CodecEvent, EventQueue, PendingInputs};
use crate::decoder::{ranged_bytes, DecodeState, MediaCodecDecoder, OutputLayout};
use crate::device::MediaCodecDevice;
use crate::encoder::{EncodeSpec, EncodeState, InputLayout, MediaCodecEncoder};
use crate::error::MediaCodecError;
use crate::format::FormatSpec;
use crate::pipeline::SubmitOutcome;
use crate::sdk::ApiLevel;
use crate::seam::{CodecSeam, SeamFormat, SeamItem, SeamOutput};
use crate::surface::{MediaCodecSurface, SurfaceSlots};

use ndk::media::media_codec::{
    AsyncNotifyCallback, DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, InputBuffer,
    MediaCodec, MediaCodecDirection,
};
use ndk::media::media_format::MediaFormat;
use ndk::media_error::MediaError;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::GpuCaps;

/// `MediaCodec#BUFFER_FLAG_DECODE_ONLY`; `AndroidSeam` and `AsyncDrain` share it.
const BUFFER_FLAG_DECODE_ONLY: u32 = 32;
/// `MediaCodec#BUFFER_FLAG_END_OF_STREAM`; `AndroidSeam` and `AsyncDrain` share it.
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// Input attempts per payload before starvation; `AndroidSeam` and `AsyncDrain` share it.
const INPUT_ATTEMPTS: u32 = 3;
/// Dequeue wait per attempt; `AndroidSeam` and `AsyncDrain` share it.
const INPUT_TIMEOUT: Duration = Duration::from_millis(10);

/// Output arrival path.
#[derive(Debug)]
enum DrainMode {
    /// Synchronous dequeue with timeouts.
    Sync,
    /// Async callbacks feeding a polled queue.
    Async(AsyncDrain),
}

/// Session direction behind one seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Role {
    /// Decode: packets in, frames out.
    Decode,
    /// Encode: frames in, packets out.
    Encode,
}

impl Role {
    /// Overflow error for a `len`-byte payload against `capacity`.
    fn overflow(self, len: usize, capacity: usize) -> MediaCodecError {
        match self {
            Self::Decode => MediaCodecError::InputTooLarge { len, capacity },
            Self::Encode => MediaCodecError::FrameTooLarge { len, capacity },
        }
    }

    /// True when an output carrying `flags` drops before release.
    fn skips(self, flags: u32) -> bool {
        match self {
            Self::Decode => flags & BUFFER_FLAG_DECODE_ONLY != 0,
            Self::Encode => flags & BUFFER_FLAG_END_OF_STREAM != 0,
        }
    }
}

/// NDK codec session behind the crate seam.
#[derive(Debug)]
pub(crate) struct AndroidSeam {
    /// Owned codec handle.
    codec: MediaCodec,
    /// Sync dequeue or async callbacks.
    mode: DrainMode,
    /// Session direction.
    role: Role,
    /// True after the first output format shipped.
    primed: bool,
}

impl AndroidSeam {
    /// Session over `codec` in `mode` for `role`.
    fn new(codec: MediaCodec, mode: DrainMode, role: Role) -> Self {
        Self {
            codec,
            mode,
            role,
            primed: false,
        }
    }

    /// Sync dequeue, or async callbacks where the device carries them.
    fn drain_mode(codec: &mut MediaCodec, level: ApiLevel) -> Result<DrainMode, MediaCodecError> {
        if level.supports_async_notify() {
            let queue = EventQueue::new();
            codec.set_async_notify_callback(Some(queue.async_callbacks()))?;
            Ok(DrainMode::Async(AsyncDrain::new(queue)))
        } else {
            Ok(DrainMode::Sync)
        }
    }

    /// Current output format reading.
    fn format_of(codec: &MediaCodec) -> SeamFormat {
        let format = codec.output_format();
        SeamFormat::new(
            format.i32(FormatSpec::KEY_WIDTH),
            format.i32(FormatSpec::KEY_HEIGHT),
            format.i32(FormatSpec::KEY_COLOR_FORMAT),
            format.i32(FormatSpec::KEY_STRIDE),
            format.i32(FormatSpec::KEY_SLICE_HEIGHT),
        )
    }

    /// First output format on decode sessions, once.
    fn primed_format(&mut self) -> Vec<SeamItem> {
        if self.role == Role::Decode && !self.primed {
            self.primed = true;
            vec![SeamItem::Format(Self::format_of(&self.codec))]
        } else {
            Vec::new()
        }
    }

    /// Available outputs, without priming.
    fn read_inner(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
        match &mut self.mode {
            DrainMode::Sync => Self::read_sync(&self.codec, self.role),
            DrainMode::Async(drain) => drain.read(&self.codec, self.role),
        }
    }

    /// Payload queued through one zero-wait dequeue; outputs stay for the poll.
    fn queue_nowait_sync(
        codec: &MediaCodec,
        payload: &[u8],
        pts_us: u64,
        role: Role,
    ) -> Result<SubmitOutcome, MediaCodecError> {
        match codec.dequeue_input_buffer(Duration::ZERO)? {
            DequeuedInputBufferResult::Buffer(mut buffer) => {
                Self::fill_buffer(&mut buffer, payload, role)?;
                codec.queue_input_buffer(buffer, 0, payload.len(), pts_us, 0)?;
                Ok(SubmitOutcome::Queued)
            }
            DequeuedInputBufferResult::TryAgainLater => Ok(SubmitOutcome::Starved),
        }
    }

    /// Payload queued through sync dequeue; outputs freed along the way.
    fn submit_sync(
        codec: &MediaCodec,
        payload: &[u8],
        pts_us: u64,
        role: Role,
    ) -> Result<Vec<SeamItem>, MediaCodecError> {
        let mut items = Vec::new();
        for _ in 0..INPUT_ATTEMPTS {
            match codec.dequeue_input_buffer(INPUT_TIMEOUT)? {
                DequeuedInputBufferResult::Buffer(mut buffer) => {
                    Self::fill_buffer(&mut buffer, payload, role)?;
                    codec.queue_input_buffer(buffer, 0, payload.len(), pts_us, 0)?;
                    return Ok(items);
                }
                DequeuedInputBufferResult::TryAgainLater => {
                    items.extend(Self::read_sync(codec, role)?);
                }
            }
        }
        Err(MediaCodecError::InputStarved)
    }

    /// Outputs collected through sync dequeue.
    fn read_sync(codec: &MediaCodec, role: Role) -> Result<Vec<SeamItem>, MediaCodecError> {
        let mut items = Vec::new();
        loop {
            match codec.dequeue_output_buffer(Duration::ZERO)? {
                DequeuedOutputBufferInfoResult::Buffer(buffer) => {
                    let info = *buffer.info();
                    let flags = info.flags();
                    if role.skips(flags) {
                        codec.release_output_buffer(buffer, false)?;
                        continue;
                    }
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
                    let pts = info.presentation_time_us();
                    codec.release_output_buffer(buffer, false)?;
                    items.push(SeamItem::Output(SeamOutput::new(bytes, flags, pts)));
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                    if role == Role::Decode {
                        items.push(SeamItem::Format(Self::format_of(codec)));
                    }
                }
                DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
                DequeuedOutputBufferInfoResult::TryAgainLater => return Ok(items),
            }
        }
    }

    /// Payload bytes copied into one input buffer.
    fn fill_buffer(
        buffer: &mut InputBuffer<'_>,
        payload: &[u8],
        role: Role,
    ) -> Result<(), MediaCodecError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let dst = buffer.buffer_mut();
            if payload.len() > dst.len() {
                return Err(role.overflow(payload.len(), dst.len()));
            }
            for (slot, byte) in dst.iter_mut().zip(payload.iter()) {
                slot.write(*byte);
            }
            Ok(())
        }));
        match outcome {
            Ok(result) => result,
            Err(_) => Err(MediaCodecError::BufferLost),
        }
    }

    /// Payload bytes queued into input buffer `index`.
    fn write_input(
        codec: &MediaCodec,
        index: usize,
        payload: &[u8],
        pts_us: u64,
        role: Role,
    ) -> Result<(), MediaCodecError> {
        let Some(dst) = codec.input_buffer(index) else {
            return Err(MediaCodecError::BufferLost);
        };
        if payload.len() > dst.len() {
            return Err(role.overflow(payload.len(), dst.len()));
        }
        for (slot, byte) in dst.iter_mut().zip(payload.iter()) {
            slot.write(*byte);
        }
        codec.queue_input_buffer_by_index(index, 0, payload.len(), pts_us, 0)?;
        Ok(())
    }
}

impl CodecSeam for AndroidSeam {
    /// One input payload queued; outputs freed along the way.
    fn submit(&mut self, payload: &[u8], pts_us: u64) -> Result<Vec<SeamItem>, MediaCodecError> {
        let mut items = self.primed_format();
        match &mut self.mode {
            DrainMode::Sync => {
                items.extend(Self::submit_sync(&self.codec, payload, pts_us, self.role)?);
            }
            DrainMode::Async(drain) => {
                items.extend(drain.submit(&self.codec, payload, pts_us, self.role)?);
            }
        }
        Ok(items)
    }

    /// One input payload queued without waiting for a buffer.
    fn submit_nowait(
        &mut self,
        payload: &[u8],
        pts_us: u64,
    ) -> Result<(SubmitOutcome, Vec<SeamItem>), MediaCodecError> {
        let mut items = self.primed_format();
        let outcome = match &mut self.mode {
            DrainMode::Sync => Self::queue_nowait_sync(&self.codec, payload, pts_us, self.role)?,
            DrainMode::Async(drain) => {
                let (outcome, mut drained) =
                    drain.queue_nowait(&self.codec, payload, pts_us, self.role)?;
                items.append(&mut drained);
                outcome
            }
        };
        Ok((outcome, items))
    }

    /// Available outputs.
    fn read(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
        let mut items = self.primed_format();
        items.extend(self.read_inner()?);
        Ok(items)
    }

    /// Drained outputs, then a codec flush; stays started.
    fn reset(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
        let items = self.read_inner()?;
        if let DrainMode::Async(drain) = &mut self.mode {
            drain.pending.clear();
        }
        self.codec.flush()?;
        if matches!(self.mode, DrainMode::Async(_)) {
            self.codec.start()?;
        }
        Ok(items)
    }
}

impl Drop for AndroidSeam {
    /// Codec stopped; buffers release with the handle.
    fn drop(&mut self) {
        let _stopped = self.codec.stop();
    }
}

/// Async callback state.
#[derive(Debug)]
struct AsyncDrain {
    /// Callback-side event intake.
    queue: EventQueue,
    /// Input indices awaiting a payload.
    pending: PendingInputs,
}

impl AsyncDrain {
    /// Drain over `queue` with an empty stash.
    fn new(queue: EventQueue) -> Self {
        Self {
            queue,
            pending: PendingInputs::new(),
        }
    }

    /// Payload queued; outputs freed along the way.
    fn submit(
        &mut self,
        codec: &MediaCodec,
        payload: &[u8],
        pts_us: u64,
        role: Role,
    ) -> Result<Vec<SeamItem>, MediaCodecError> {
        let mut items = Vec::new();
        for _ in 0..INPUT_ATTEMPTS {
            if let Some(index) = self.pending.pop() {
                AndroidSeam::write_input(codec, index, payload, pts_us, role)?;
                return Ok(items);
            }
            let batch = self.queue.drain_blocking(INPUT_TIMEOUT);
            self.absorb(codec, batch, role, &mut items)?;
        }
        Err(MediaCodecError::InputStarved)
    }

    /// Outputs collected from queued async events.
    fn read(&mut self, codec: &MediaCodec, role: Role) -> Result<Vec<SeamItem>, MediaCodecError> {
        let mut items = Vec::new();
        let batch = self.queue.drain();
        self.absorb(codec, batch, role, &mut items)?;
        Ok(items)
    }

    /// Payload queued without waiting; stashed inputs first, one drain after.
    fn queue_nowait(
        &mut self,
        codec: &MediaCodec,
        payload: &[u8],
        pts_us: u64,
        role: Role,
    ) -> Result<(SubmitOutcome, Vec<SeamItem>), MediaCodecError> {
        let mut items = Vec::new();
        if let Some(index) = self.pending.pop() {
            AndroidSeam::write_input(codec, index, payload, pts_us, role)?;
            return Ok((SubmitOutcome::Queued, items));
        }
        let batch = self.queue.drain();
        self.absorb(codec, batch, role, &mut items)?;
        if let Some(index) = self.pending.pop() {
            AndroidSeam::write_input(codec, index, payload, pts_us, role)?;
            return Ok((SubmitOutcome::Queued, items));
        }
        Ok((SubmitOutcome::Starved, items))
    }

    /// Queued `batch` absorbed: inputs stashed, outputs collected.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the codec faults or a pulled output range leaves
    /// its buffer.
    ///
    /// # Arguments
    ///
    /// * `codec` - session owning the buffers.
    /// * `batch` - drained queue events.
    /// * `role` - session direction.
    /// * `items` - drained outputs and formats, appended in arrival order.
    fn absorb(
        &mut self,
        codec: &MediaCodec,
        batch: Vec<CodecEvent>,
        role: Role,
        items: &mut Vec<SeamItem>,
    ) -> Result<(), MediaCodecError> {
        for event in self.pending.stash_inputs(batch) {
            match event {
                CodecEvent::InputAvailable { index } => self.pending.push(index),
                CodecEvent::OutputAvailable {
                    index,
                    offset,
                    size,
                    pts_us,
                    flags,
                } => {
                    if let Some(output) =
                        Self::pull_output(codec, index, offset, size, flags, pts_us, role)?
                    {
                        items.push(SeamItem::Output(output));
                    }
                }
                CodecEvent::FormatChanged => {
                    if role == Role::Decode {
                        items.push(SeamItem::Format(AndroidSeam::format_of(codec)));
                    }
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
        Ok(())
    }

    /// Output pulled from buffer `index`, unless dropped for `role`.
    fn pull_output(
        codec: &MediaCodec,
        index: usize,
        offset: i32,
        size: i32,
        flags: u32,
        pts_us: i64,
        role: Role,
    ) -> Result<Option<SeamOutput>, MediaCodecError> {
        if role.skips(flags) {
            codec.release_output_buffer_by_index(index, false)?;
            return Ok(None);
        }
        let Some(buffer) = codec.output_buffer(index) else {
            return Err(MediaCodecError::BufferLost);
        };
        let bytes = ranged_bytes(buffer, offset, size);
        codec.release_output_buffer_by_index(index, false)?;
        Ok(Some(SeamOutput::new(bytes?, flags, pts_us)))
    }
}

impl ApiLevel {
    /// Live device level.
    ///
    /// <https://docs.rs/ndk-sys/0.6.0/ndk_sys/fn.android_get_device_api_level.html>
    #[must_use]
    pub fn current() -> Self {
        // SAFETY: pure query without preconditions; any thread, any time.
        let level = unsafe { ndk_sys::android_get_device_api_level() };
        ApiLevel::new(u32::try_from(level).unwrap_or(0))
    }
}

impl EncodeSpec {
    /// Native format for `configure`.
    ///
    /// <https://docs.rs/ndk/0.9.0/ndk/media/media_format/struct.MediaFormat.html>
    #[must_use]
    pub fn materialize(&self) -> MediaFormat {
        let mut format = MediaFormat::new();
        format.set_str(FormatSpec::KEY_MIME, self.mime());
        format.set_i32(FormatSpec::KEY_WIDTH, self.width_as_i32());
        format.set_i32(FormatSpec::KEY_HEIGHT, self.height_as_i32());
        format.set_i32(
            Self::KEY_BIT_RATE,
            i32::try_from(self.bitrate()).unwrap_or(i32::MAX),
        );
        format.set_i32(
            FormatSpec::KEY_FRAME_RATE,
            i32::try_from(self.frame_rate()).unwrap_or(i32::MAX),
        );
        format.set_i32(
            Self::KEY_I_FRAME_INTERVAL,
            i32::try_from(self.keyframe_interval()).unwrap_or(i32::MAX),
        );
        format.set_i32(FormatSpec::KEY_COLOR_FORMAT, self.color_format());
        format
    }

    /// Width as a native `i32`; construction bounds it.
    fn width_as_i32(&self) -> i32 {
        i32::try_from(self.width()).unwrap_or(i32::MAX)
    }

    /// Height as a native `i32`; construction bounds it.
    fn height_as_i32(&self) -> i32 {
        i32::try_from(self.height()).unwrap_or(i32::MAX)
    }
}

impl EventQueue {
    /// Async callbacks pushing into this queue.
    ///
    /// All four slots stay set; unset slots fault on older devices.
    /// <https://docs.rs/ndk/0.9.0/ndk/media/media_codec/struct.MediaCodec.html#method.set_async_notify_callback>
    #[must_use]
    pub fn async_callbacks(&self) -> AsyncNotifyCallback {
        let input = self.sender();
        let output = self.sender();
        let format = self.sender();
        let error = self.sender();
        AsyncNotifyCallback {
            on_input_available: Some(Box::new(move |index| {
                let _sent = input.send(CodecEvent::InputAvailable { index });
            })),
            on_output_available: Some(Box::new(move |index, info| {
                let _sent = output.send(CodecEvent::OutputAvailable {
                    index,
                    offset: info.offset(),
                    size: info.size(),
                    pts_us: info.presentation_time_us(),
                    flags: info.flags(),
                });
            })),
            on_format_changed: Some(Box::new(move |_format| {
                let _sent = format.send(CodecEvent::FormatChanged);
            })),
            on_error: Some(Box::new(move |kind, action, _detail| {
                let _sent = error.send(CodecEvent::Error {
                    recoverable: action.is_recoverable(),
                    transient: action.is_transient(),
                    kind: format!("{kind:?}"),
                });
            })),
        }
    }
}

impl FormatSpec {
    /// Native format for `configure`.
    ///
    /// <https://docs.rs/ndk/0.9.0/ndk/media/media_format/struct.MediaFormat.html>
    #[must_use]
    pub fn materialize(&self) -> MediaFormat {
        let mut format = MediaFormat::new();
        format.set_str(Self::KEY_MIME, self.mime());
        format.set_i32(Self::KEY_WIDTH, self.width_as_i32());
        format.set_i32(Self::KEY_HEIGHT, self.height_as_i32());
        if let Some(rate) = self.frame_rate() {
            format.set_i32(
                Self::KEY_FRAME_RATE,
                i32::try_from(rate).unwrap_or(i32::MAX),
            );
        }
        if let Some(csd) = self.csd_0() {
            format.set_buffer(Self::KEY_CSD_0, csd);
        }
        if let Some(csd) = self.csd_1() {
            format.set_buffer(Self::KEY_CSD_1, csd);
        }
        format
    }

    /// Width as a native `i32`; construction bounds it.
    fn width_as_i32(&self) -> i32 {
        i32::try_from(self.width()).unwrap_or(i32::MAX)
    }

    /// Height as a native `i32`; construction bounds it.
    fn height_as_i32(&self) -> i32 {
        i32::try_from(self.height()).unwrap_or(i32::MAX)
    }
}

impl MediaCodecDecoder {
    /// Decoder for `spec`.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec input format.
    ///
    /// # Errors
    ///
    /// Returns `Err` below the device floor, without both `csd` blobs on
    /// H.264/H.265, or when no decoder answers the mime.
    pub fn open(spec: &FormatSpec) -> Result<Self, MediaCodecError> {
        spec.check_csd()?;
        let level = ApiLevel::current();
        if !level.meets_min() || !level.at_least(spec.min_sdk()) {
            return Err(MediaCodecError::BelowMinSdk { found: level.get() });
        }
        let mut codec = MediaCodec::from_decoder_type(spec.mime())
            .ok_or(MediaCodecError::NoDecoder { mime: spec.mime() })?;
        let state = DecodeState::Unconfigured.configured()?;
        codec.configure(&spec.materialize(), None, MediaCodecDirection::Decoder)?;
        let mode = AndroidSeam::drain_mode(&mut codec, level)?;
        codec.start()?;
        let state = state.started()?;
        let output = OutputLayout::tentative(spec).ok_or(MediaCodecError::BadExtent {
            width: i32::try_from(spec.width()).unwrap_or(i32::MAX),
            height: i32::try_from(spec.height()).unwrap_or(i32::MAX),
        })?;
        let backend = Box::new(AndroidSeam::new(codec, mode, Role::Decode));
        Ok(Self::from_backend(
            state,
            output,
            SurfaceSlots::new(SurfaceSlots::DEFAULT_CAPACITY),
            backend,
        ))
    }

    /// Drained surfaces, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    pub fn reset(&mut self) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        self.reset_surfaces()
    }

    /// One packet queued without blocking on its output.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`). A full bound or a busy codec answers
    /// `Starved`, never `Err`.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    ///
    /// # Returns
    ///
    /// Queue outcome plus surfaces freed along the way, if any.
    pub fn submit_nowait(
        &mut self,
        packet: &Packet,
    ) -> Result<(SubmitOutcome, Vec<MediaCodecSurface>), MediaCodecError> {
        self.submit_packet_nowait(packet)
    }

    /// Surfaces decoded so far, draining every available output.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    pub fn poll(&mut self) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        self.poll_surfaces()
    }
}

impl Decoder for MediaCodecDecoder {
    type Frame = MediaCodecSurface;
    type Error = MediaCodecError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    /// Surfaces decoded from `packet`, possibly none.
    ///
    /// Per-call emission: the packet queues and the drain runs in this same
    /// call, so a stream with no reordering emits each picture on its
    /// completing `decode` call. The client holds no decoded-picture buffer;
    /// whatever the codec holds back for reordering stays inside the codec.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.push_packet(packet)
    }

    /// Surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.drain_surfaces()
    }
}

impl Drop for MediaCodecDecoder {
    /// State marked released; the session stops with its fields.
    fn drop(&mut self) {
        self.mark_released();
    }
}

impl MediaCodecDevice {
    /// Device probed on this machine.
    #[must_use]
    pub fn probe() -> Self {
        Self::from_level(ApiLevel::current())
    }
}

impl MediaCodecEncoder {
    /// Encoder for `spec`.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec output format.
    ///
    /// # Errors
    ///
    /// Returns `Err` below the device floor, when the input color has no
    /// plane layout, or when no encoder answers the mime.
    pub fn open(spec: &EncodeSpec) -> Result<Self, MediaCodecError> {
        let level = ApiLevel::current();
        if !level.meets_min() || !level.at_least(spec.min_sdk()) {
            return Err(MediaCodecError::BelowMinSdk { found: level.get() });
        }
        let mut codec = MediaCodec::from_encoder_type(spec.mime())
            .ok_or(MediaCodecError::NoEncoder { mime: spec.mime() })?;
        let state = EncodeState::Unconfigured.configured()?;
        codec.configure(&spec.materialize(), None, MediaCodecDirection::Encoder)?;
        let mode = AndroidSeam::drain_mode(&mut codec, level)?;
        codec.start()?;
        let state = state.started()?;
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
        let backend = Box::new(AndroidSeam::new(codec, mode, Role::Encode));
        Ok(Self::from_backend(state, spec.clone(), input, backend))
    }

    /// Drained packets, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub fn reset(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.reset_packets()
    }
}

impl Encoder for MediaCodecEncoder {
    type Error = MediaCodecError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

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
        self.push_frame(frame)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        self.drain_packets()
    }
}

impl Drop for MediaCodecEncoder {
    /// State marked released; the session stops with its fields.
    fn drop(&mut self) {
        self.mark_released();
    }
}

impl From<MediaError> for MediaCodecError {
    /// Codec fault carrying the native failure text.
    fn from(error: MediaError) -> Self {
        Self::CodecFault {
            recoverable: false,
            transient: false,
            kind: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MediaCodecDevice;

    use rust_video_caps::traits::Device;

    #[test]
    fn probe_names_tier() {
        assert_eq!(MediaCodecDevice::probe().name(), "mediacodec");
    }

    #[test]
    fn caps_agree_with_surface_exports() {
        use bytes::Bytes;
        use rust_video_caps::frame::GpuExport;
        use rust_video_caps::packet::Packet;
        use rust_video_caps::traits::Decoder;
        use rust_video_caps::transcode::GpuCaps;

        use super::MediaCodecDecoder;
        use crate::error::MediaCodecError;
        use crate::format::FormatSpec;

        let spec = FormatSpec::h264(64, 64)
            .expect("valid dims")
            .with_csd_0(vec![0, 0, 1])
            .with_csd_1(vec![0, 0, 1]);
        let mut decoder = match MediaCodecDecoder::open(&spec) {
            Ok(decoder) => decoder,
            Err(
                error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. }),
            ) => {
                println!("SKIP mediacodec caps on this device: {error}");
                return;
            }
            Err(error) => panic!("unexpected open failure: {error}"),
        };
        assert_eq!(decoder.caps(), GpuCaps::new(None, false, false));
        let empty = Packet::new(Bytes::new());
        let mut surfaces = decoder.decode(&empty).expect("empty packet queues");
        surfaces.extend(decoder.flush().expect("flush drains"));
        for surface in &surfaces {
            assert!(matches!(surface.export(), GpuExport::Opaque { .. }));
        }
    }

    #[test]
    fn encoder_caps_report_no_share() {
        use rust_video_caps::traits::Encoder;
        use rust_video_caps::transcode::GpuCaps;

        use super::MediaCodecEncoder;
        use crate::encoder::EncodeSpec;
        use crate::error::MediaCodecError;

        let spec = EncodeSpec::h264(64, 64, 1_000_000, 30, 1).expect("valid spec");
        let encoder = match MediaCodecEncoder::open(&spec) {
            Ok(encoder) => encoder,
            Err(
                error @ (MediaCodecError::NoEncoder { .. } | MediaCodecError::BelowMinSdk { .. }),
            ) => {
                println!("SKIP mediacodec caps on this device: {error}");
                return;
            }
            Err(error) => panic!("unexpected open failure: {error}"),
        };
        assert_eq!(encoder.caps(), GpuCaps::new(None, false, false));
    }
}
