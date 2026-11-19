//! Byte-buffer decode behind the caps `Decoder` trait.

use std::time::Instant;

use crate::error::MediaCodecError;
use crate::format::{pixel_format_for, FormatSpec};
use crate::overlap::{PipelineLedger, SubmitOutcome};
use crate::seam::{CodecSeam, SeamItem};
use crate::surface::{MediaCodecSurface, SurfaceSlots};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;

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

/// Byte-buffer plane arrangement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layout {
    /// Semi-planar Y plus interleaved UV.
    Nv12,
    /// Planar Y, U, V.
    Planar,
}

impl Layout {
    /// Shared pixel layout.
    fn pixel_format(self) -> PixelFormat {
        match self {
            Self::Nv12 => PixelFormat::Nv12,
            Self::Planar => PixelFormat::Yuv420P,
        }
    }
}

/// `MediaCodec` decoder over byte buffers.
///
/// Threading rule: single-threaded use needs nothing. Threads sharing one
/// decoder serialize through `&mut` (for example behind a `Mutex`); every
/// native codec call happens inside an `&mut` method, and `decode` takes
/// `&mut` precisely so sharing cannot slip past the borrow checker.
///
/// Lifetime rule: each issued surface pins one decoder slot until it drops;
/// surfaces own their pixels and stay valid past decoder drop. Slot
/// exhaustion fails loudly with `NoFreeSlot`, never silent corruption.
#[derive(Debug)]
pub struct MediaCodecDecoder {
    /// Lifecycle gate.
    state: DecodeState,
    /// Negotiated output layout.
    output: OutputLayout,
    /// Bounded surface slot pool.
    slots: SurfaceSlots,
    /// In-flight bound plus phase timings.
    ledger: PipelineLedger,
    /// OS codec session.
    // Reached only from the Android module.
    #[allow(dead_code)]
    backend: Box<dyn CodecSeam>,
}

// Constructors and directors run only behind the Android module.
#[allow(dead_code)]
impl MediaCodecDecoder {
    /// Decoder in `state` with `output` layout, `slots` pool, and `backend` session.
    pub(crate) fn from_backend(
        state: DecodeState,
        output: OutputLayout,
        slots: SurfaceSlots,
        backend: Box<dyn CodecSeam>,
    ) -> Self {
        Self {
            state,
            output,
            slots,
            ledger: PipelineLedger::new(PipelineLedger::DEFAULT_DEPTH),
            backend,
        }
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

    /// Surfaces currently pinning decoder slots.
    #[must_use]
    pub fn live_surfaces(&self) -> usize {
        self.slots.live()
    }

    /// In-flight bound plus phase timings.
    #[must_use]
    pub fn ledger(&self) -> &PipelineLedger {
        &self.ledger
    }

    /// Presentation time in microseconds; unstamped packets read zero.
    #[must_use]
    pub fn pts_us(packet: &Packet) -> u64 {
        packet
            .pts()
            .map_or(0, |pts| u64::try_from(pts.as_micros()).unwrap_or(u64::MAX))
    }

    /// Surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    pub(crate) fn push_packet(
        &mut self,
        packet: &Packet,
    ) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        self.state.ensure_started("decode")?;
        let pts = Self::pts_us(packet);
        let start = Instant::now();
        let mut items = self.backend.submit(packet.data(), pts)?;
        self.ledger
            .note_submitted(pts, PipelineLedger::elapsed_us(start));
        items.extend(self.backend.read()?);
        self.surfaces(items)
    }

    /// Surfaces decoded from `packet`, queued without blocking on its output.
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
    pub(crate) fn submit_packet_nowait(
        &mut self,
        packet: &Packet,
    ) -> Result<(SubmitOutcome, Vec<MediaCodecSurface>), MediaCodecError> {
        self.state.ensure_started("decode")?;
        if !self.ledger.can_submit() {
            return Ok((SubmitOutcome::Starved, Vec::new()));
        }
        let pts = Self::pts_us(packet);
        let start = Instant::now();
        let (outcome, items) = self.backend.submit_nowait(packet.data(), pts)?;
        if outcome == SubmitOutcome::Queued {
            self.ledger
                .note_submitted(pts, PipelineLedger::elapsed_us(start));
        }
        Ok((outcome, self.surfaces(items)?))
    }

    /// Surfaces decoded so far, draining every available output.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    pub(crate) fn poll_surfaces(&mut self) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        self.state.ensure_started("poll")?;
        let items = self.backend.read()?;
        self.surfaces(items)
    }

    /// Surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    pub(crate) fn drain_surfaces(&mut self) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        self.state.ensure_started("flush")?;
        let items = self.backend.read()?;
        self.surfaces(items)
    }

    /// Drained surfaces, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the codec faults, when a format
    /// reading rejects the output layout, or when live surfaces hold
    /// every slot (`NoFreeSlot`).
    pub(crate) fn reset_surfaces(&mut self) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        self.state.ensure_started("reset")?;
        let items = self.backend.reset()?;
        let surfaces = self.surfaces(items)?;
        self.ledger.discard_pending();
        Ok(surfaces)
    }

    /// Decoder marked released.
    pub(crate) fn mark_released(&mut self) {
        self.state = self.state.released().unwrap_or(DecodeState::Released);
    }

    /// Surfaces over drained `items`, formats applied in order.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a format reading rejects the output layout,
    /// when a payload falls short of the layout, or when live surfaces
    /// hold every slot (`NoFreeSlot`).
    fn surfaces(
        &mut self,
        items: Vec<SeamItem>,
    ) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        let mut frames = Vec::new();
        for item in items {
            match item {
                SeamItem::Format(format) => self.output.apply_format(
                    format.width(),
                    format.height(),
                    format.color(),
                    format.stride(),
                    format.slice_height(),
                )?,
                SeamItem::Output(output) => {
                    let pts = output.pts_us();
                    let start = Instant::now();
                    let frame = self.output.split(output.into_bytes())?;
                    self.ledger
                        .note_completed(pts, PipelineLedger::elapsed_us(start));
                    frames.push(frame);
                }
            }
        }
        self.wrap(frames)
    }

    /// Surfaces over `frames`, each pinning one decoder slot.
    ///
    /// # Errors
    ///
    /// Returns `NoFreeSlot` when live surfaces hold every slot; the frame
    /// that found no slot drops with its codec buffer already released, and
    /// surfaces wrapped earlier in this call keep their slots.
    fn wrap(&mut self, frames: Vec<CpuFrame>) -> Result<Vec<MediaCodecSurface>, MediaCodecError> {
        let mut surfaces = Vec::with_capacity(frames.len());
        for frame in frames {
            let slot = self.slots.acquire()?;
            let coded = PictureSize::new(self.output.stride(), self.output.slice_height())
                .unwrap_or_else(|| frame.size());
            surfaces.push(MediaCodecSurface::new(frame, coded, slot));
        }
        Ok(surfaces)
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
            _ => {
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

    /// Visible picture extent in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.size
    }

    /// Luma bytes per row.
    #[must_use]
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Luma row count.
    #[must_use]
    pub fn slice_height(&self) -> u32 {
        self.slice_height
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
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::{ranged_bytes, DecodeState, MediaCodecDecoder, MediaCodecError, OutputLayout};
    use crate::format::FormatSpec;
    use crate::overlap::SubmitOutcome;
    use crate::seam::{CodecSeam, SeamFormat, SeamItem, SeamOutput};
    use crate::surface::{MediaCodecSurface, SurfaceSlots};

    use bytes::Bytes;

    use rust_video_caps::frame::PixelFormat;
    use rust_video_caps::packet::Packet;

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
    fn layout_exposes_surface_geometry() {
        let layout = layout_16x16_nv12();
        assert_eq!(
            layout.size(),
            rust_video_caps::common::PictureSize::new(16, 16).expect("valid size")
        );
        assert_eq!(layout.stride(), 16);
        assert_eq!(layout.slice_height(), 16);
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

    /// Seam call observed by the script.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Call {
        /// Blocking queue.
        Submit,
        /// Non-blocking queue.
        SubmitNowait,
        /// Output drain.
        Read,
        /// Drain plus flush.
        Reset,
    }

    /// Scripted codec session; queued inputs release as outputs on drain.
    #[derive(Debug)]
    struct ScriptSeam {
        /// Calls in arrival order.
        log: Rc<RefCell<Vec<Call>>>,
        /// Inputs awaiting release, oldest first.
        held: VecDeque<(Vec<u8>, u64)>,
        /// Input buffer count; fuller queues starve nowait submits.
        capacity: usize,
        /// Drain latency modeling engine time.
        latency: Duration,
        /// True after the format shipped.
        primed: bool,
    }

    impl ScriptSeam {
        /// Seam holding `capacity` inputs, draining after `latency`.
        fn new(log: Rc<RefCell<Vec<Call>>>, capacity: usize, latency: Duration) -> Self {
            Self {
                log,
                held: VecDeque::new(),
                capacity,
                latency,
                primed: false,
            }
        }

        /// Call appended to the log.
        fn note(&self, call: Call) {
            self.log.borrow_mut().push(call);
        }

        /// Format once, then every held input as one output.
        fn release(&mut self) -> Vec<SeamItem> {
            let mut items = Vec::new();
            if !self.primed {
                self.primed = true;
                items.push(SeamItem::Format(SeamFormat::new(
                    Some(16),
                    Some(16),
                    Some(21),
                    Some(16),
                    Some(16),
                )));
            }
            while let Some((payload, pts)) = self.held.pop_front() {
                let marker = payload.first().copied().unwrap_or(0);
                let bytes = Bytes::from(vec![marker; 16 * 16 * 3 / 2]);
                items.push(SeamItem::Output(SeamOutput::new(
                    bytes,
                    0,
                    i64::try_from(pts).unwrap_or(i64::MAX),
                )));
            }
            items
        }
    }

    impl CodecSeam for ScriptSeam {
        fn submit(
            &mut self,
            payload: &[u8],
            pts_us: u64,
        ) -> Result<Vec<SeamItem>, MediaCodecError> {
            self.note(Call::Submit);
            self.held.push_back((payload.to_vec(), pts_us));
            Ok(Vec::new())
        }

        fn submit_nowait(
            &mut self,
            payload: &[u8],
            pts_us: u64,
        ) -> Result<(SubmitOutcome, Vec<SeamItem>), MediaCodecError> {
            self.note(Call::SubmitNowait);
            if self.held.len() >= self.capacity {
                return Ok((SubmitOutcome::Starved, Vec::new()));
            }
            self.held.push_back((payload.to_vec(), pts_us));
            Ok((SubmitOutcome::Queued, Vec::new()))
        }

        fn read(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
            self.note(Call::Read);
            std::thread::sleep(self.latency);
            Ok(self.release())
        }

        fn reset(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
            self.note(Call::Reset);
            Ok(self.release())
        }
    }

    fn script_decoder(seam: ScriptSeam) -> MediaCodecDecoder {
        let spec = FormatSpec::h264(16, 16).expect("valid dims");
        let layout = OutputLayout::tentative(&spec).expect("valid tentative");
        MediaCodecDecoder::from_backend(
            DecodeState::Started,
            layout,
            SurfaceSlots::new(16),
            Box::new(seam),
        )
    }

    fn script_packets(count: u8) -> Vec<Packet> {
        (0..count)
            .map(|i| {
                Packet::new(Bytes::from(vec![i; 8]))
                    .with_pts(Duration::from_micros(u64::from(i) * 1000))
            })
            .collect()
    }

    fn surface_bytes(surfaces: &[MediaCodecSurface]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for surface in surfaces {
            for plane in surface.frame().planes() {
                bytes.extend_from_slice(plane.data());
            }
        }
        bytes
    }

    fn drain_all(decoder: &mut MediaCodecDecoder) -> Vec<MediaCodecSurface> {
        let mut surfaces = Vec::new();
        loop {
            let batch = decoder.poll_surfaces().expect("drain");
            if batch.is_empty() {
                return surfaces;
            }
            surfaces.extend(batch);
        }
    }

    #[test]
    fn nowait_batches_four_inputs_before_first_drain() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut decoder = script_decoder(ScriptSeam::new(Rc::clone(&log), 8, Duration::ZERO));
        for packet in &script_packets(4) {
            let (outcome, surfaces) = decoder.submit_packet_nowait(packet).expect("queue");
            assert_eq!(outcome, SubmitOutcome::Queued);
            assert!(surfaces.is_empty());
        }
        assert_eq!(*log.borrow(), [Call::SubmitNowait; 4]);
        assert_eq!(decoder.ledger().in_flight(), 4);
        assert_eq!(decoder.ledger().max_in_flight(), 4);
        assert!(decoder.ledger().overlap_observed());
        let surfaces = decoder.poll_surfaces().expect("drain");
        assert_eq!(surfaces.len(), 4);
        assert_eq!(*log.borrow().last().expect("drain logged"), Call::Read);
        assert_eq!(decoder.ledger().in_flight(), 0);
        let timings = decoder.ledger().timings();
        assert_eq!(timings.len(), 4);
        for (n, timing) in timings.iter().enumerate() {
            assert_eq!(timing.seq(), u64::try_from(n).expect("small seq"));
            assert_eq!(
                timing.submitted_pts_us().expect("stamped"),
                u64::try_from(n).expect("small seq") * 1000
            );
            assert_eq!(
                timing.completed_pts_us().expect("stamped"),
                i64::try_from(n).expect("small seq") * 1000
            );
        }
    }

    #[test]
    fn fifth_submit_starves_at_depth_bound() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut decoder = script_decoder(ScriptSeam::new(Rc::clone(&log), 8, Duration::ZERO));
        for packet in &script_packets(4) {
            let (outcome, _) = decoder.submit_packet_nowait(packet).expect("queue");
            assert_eq!(outcome, SubmitOutcome::Queued);
        }
        assert!(!decoder.ledger().can_submit());
        let (outcome, surfaces) = decoder
            .submit_packet_nowait(&script_packets(1)[0])
            .expect("bound answers");
        assert_eq!(outcome, SubmitOutcome::Starved);
        assert!(surfaces.is_empty());
        assert_eq!(log.borrow().len(), 4);
        assert!(decoder.ledger().max_in_flight() <= decoder.ledger().depth());
    }

    #[test]
    fn pipelined_bytes_match_sync_decode() {
        let packets = script_packets(6);
        let mut sync = script_decoder(ScriptSeam::new(
            Rc::new(RefCell::new(Vec::new())),
            8,
            Duration::ZERO,
        ));
        let mut sync_surfaces = Vec::new();
        for packet in &packets {
            sync_surfaces.extend(sync.push_packet(packet).expect("sync decode"));
        }
        sync_surfaces.extend(drain_all(&mut sync));

        let mut pipelined = script_decoder(ScriptSeam::new(
            Rc::new(RefCell::new(Vec::new())),
            8,
            Duration::ZERO,
        ));
        let mut pipe_surfaces = Vec::new();
        for packet in &packets {
            loop {
                let (outcome, surfaces) = pipelined.submit_packet_nowait(packet).expect("queue");
                pipe_surfaces.extend(surfaces);
                if outcome == SubmitOutcome::Queued {
                    break;
                }
                pipe_surfaces.extend(drain_all(&mut pipelined));
            }
        }
        pipe_surfaces.extend(drain_all(&mut pipelined));

        assert_eq!(pipe_surfaces.len(), sync_surfaces.len());
        assert_eq!(surface_bytes(&pipe_surfaces), surface_bytes(&sync_surfaces));
        assert!(pipelined.ledger().overlap_observed());
    }

    #[test]
    fn pipelined_wall_beats_serial_wall_with_latency_parity() {
        let packets = script_packets(4);
        let latency = Duration::from_millis(15);
        let mut sync = script_decoder(ScriptSeam::new(
            Rc::new(RefCell::new(Vec::new())),
            8,
            latency,
        ));
        let start = Instant::now();
        for packet in &packets {
            sync.push_packet(packet).expect("sync decode");
        }
        let serial = start.elapsed();

        let mut pipelined = script_decoder(ScriptSeam::new(
            Rc::new(RefCell::new(Vec::new())),
            8,
            latency,
        ));
        let start = Instant::now();
        for packet in &packets {
            let (outcome, _) = pipelined.submit_packet_nowait(packet).expect("queue");
            assert_eq!(outcome, SubmitOutcome::Queued);
        }
        pipelined.poll_surfaces().expect("drain");
        let overlapped = start.elapsed();

        println!("serial {serial:?} vs overlapped {overlapped:?}");
        for timing in pipelined.ledger().timings() {
            println!(
                "frame {} submit {}us engine {}us copy {}us total {}us",
                timing.seq(),
                timing.submit_us(),
                timing.engine_wait_us(),
                timing.copy_us(),
                timing.total_us(),
            );
        }
        assert!(overlapped < serial / 2);
        let mean = |timings: &VecDeque<rust_video_caps::delivery::PhaseTiming>| {
            timings
                .iter()
                .map(rust_video_caps::delivery::PhaseTiming::total_us)
                .sum::<u64>()
                / 4
        };
        let sync_mean = mean(sync.ledger().timings());
        let pipe_mean = mean(pipelined.ledger().timings());
        let gap = sync_mean.abs_diff(pipe_mean);
        assert!(gap < 15_000, "per-frame means within one engine window");
        for timing in pipelined.ledger().timings() {
            assert!(timing.engine_wait_us() >= 5_000);
        }
    }

    #[test]
    fn decode_continues_past_reset() {
        let mut decoder = script_decoder(ScriptSeam::new(
            Rc::new(RefCell::new(Vec::new())),
            8,
            Duration::ZERO,
        ));
        for packet in &script_packets(2) {
            decoder.submit_packet_nowait(packet).expect("queue");
        }
        assert_eq!(drain_all(&mut decoder).len(), 2);
        assert!(decoder.reset_surfaces().expect("reset").is_empty());
        assert_eq!(decoder.ledger().in_flight(), 0);
        for packet in &script_packets(2) {
            decoder.submit_packet_nowait(packet).expect("queue");
        }
        assert_eq!(drain_all(&mut decoder).len(), 2);
        let timings = decoder.ledger().timings();
        assert_eq!(timings.len(), 4);
        assert_eq!(timings.back().expect("fourth timing").seq(), 3);
    }
}
