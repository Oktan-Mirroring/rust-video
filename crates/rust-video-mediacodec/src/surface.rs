//! Decoder-issued surfaces over byte-buffer output.
//!
//! A [`MediaCodecSurface`] is one decoded picture: owned CPU pixels plus the
//! canonical surface metadata (`size`, `format`, `slice`, `coded`, `origin`)
//! and an opaque export from [`MediaCodecSurface::export`]. Each live surface
//! pins one decoder slot; the pool is bounded, so backpressure is a loud
//! [`MediaCodecError::NoFreeSlot`], never silent corruption.
//!
//! Threading rule: single-threaded use needs nothing. Threads sharing one
//! decoder serialize through `&mut` (for example behind a `Mutex`), because
//! every native codec call happens inside an `&mut` decoder method. Surfaces
//! themselves are `Send + Sync` and may cross threads freely once issued.
//!
//! Lifetime rule: a surface owns its pixels and its slot lease. Holding a
//! surface pins its slot until the surface drops, even past decoder drop;
//! dropping the surface releases the slot. Decoder drop stops the codec and
//! frees codec buffers; already-issued surfaces keep valid pixels.
//!
//! Sync story: byte-buffer dequeue completes synchronously, so pixels are
//! complete at emission and [`MediaCodecSurface::try_wait`] observes
//! completion without blocking. There is no fence to poll; the NDK `sync`
//! module only matters to a future output-`Surface` path, which would hand
//! out `AImage`/`HardwareBuffer` objects instead of copies.

use std::sync::{Arc, Mutex};

use crate::error::MediaCodecError;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport, GpuFrame, PixelFormat, Surface};

/// Decoded picture pinning one decoder slot.
///
/// A live surface keeps its own resources alive: dropping the decoder never invalidates outstanding surfaces, and dropping a surface releases its slot.
#[derive(Debug)]
pub struct MediaCodecSurface {
    /// Owned picture pixels.
    frame: CpuFrame,
    /// Full buffer extent from stride by slice height.
    coded: PictureSize,
    /// Slot lease; drop releases the slot.
    slot: SlotGuard,
}

impl MediaCodecSurface {
    /// Surface over `frame` with `coded` extent and `slot` lease.
    pub(crate) fn new(frame: CpuFrame, coded: PictureSize, slot: SlotGuard) -> Self {
        Self { frame, coded, slot }
    }

    /// Visible picture extent in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.frame.size()
    }

    /// Pixel layout.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.frame.format()
    }

    /// Array layer of this picture; byte buffers carry one layer.
    #[must_use]
    pub fn slice(&self) -> u32 {
        0
    }

    /// Full buffer extent in luma samples, stride by slice height.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.coded
    }

    /// Crop left/top in luma samples; byte buffers report visible dims directly.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        (0, 0)
    }

    /// Opaque export behind the surface.
    ///
    /// The export is metadata only; pixels stay in the surface and cross to
    /// other consumers through [`MediaCodecSurface::frame`]. There is no
    /// re-import: byte-buffer mode never mints `AImage`/`HardwareBuffer`
    /// objects.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        GpuExport::Opaque { name: "mediacodec" }
    }

    /// Image pixels by reference.
    #[must_use]
    pub fn frame(&self) -> &CpuFrame {
        &self.frame
    }

    /// Device frame ticket behind this picture.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        GpuFrame::new(self.size(), self.format(), self.slot.generation)
    }

    /// Packed picture bytes behind the lease.
    #[must_use]
    pub fn lease_bytes(&self) -> u64 {
        self.frame
            .planes()
            .iter()
            .map(|plane| plane.data().len() as u64)
            .sum()
    }

    /// Blocking completion wait for external readers.
    ///
    /// Byte-buffer pixels complete synchronously at emission, so this
    /// observes completion without blocking. It stays in the contract so
    /// readers written against fence-backed tiers compile unchanged.
    ///
    /// # Errors
    ///
    /// Returns `Ok` always; byte-buffer pixels complete at emission.
    // Receiver is contract surface; completion needs no state read.
    #[allow(clippy::unused_self)]
    pub fn try_wait(&self) -> Result<(), MediaCodecError> {
        Ok(())
    }
}

impl Surface for MediaCodecSurface {
    /// Visible picture extent in luma samples.
    fn size(&self) -> PictureSize {
        self.size()
    }

    /// Pixel layout.
    fn format(&self) -> PixelFormat {
        self.format()
    }

    /// Array layer of this picture; byte buffers carry one layer.
    fn slice(&self) -> u32 {
        self.slice()
    }

    /// Full buffer extent in luma samples, stride by slice height.
    fn coded(&self) -> PictureSize {
        self.coded()
    }

    /// Crop left/top in luma samples; byte buffers report visible dims directly.
    fn origin(&self) -> (u32, u32) {
        self.origin()
    }

    /// Opaque export behind the surface.
    fn export(&self) -> GpuExport {
        self.export()
    }
}

/// Bounded pool of decoder surface slots.
///
/// Each issued surface holds one [`SlotGuard`]; dropping the surface frees
/// its slot. Clones share one pool, so a guard outliving its decoder still
/// releases cleanly on drop.
#[derive(Clone, Debug)]
pub struct SurfaceSlots {
    /// Shared live count plus capacity.
    state: Arc<Mutex<SlotState>>,
}

impl SurfaceSlots {
    /// Decoder pool capacity: sixteen live surfaces before backpressure.
    pub const DEFAULT_CAPACITY: usize = 16;

    /// Pool holding `capacity` live surfaces.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(SlotState {
                capacity,
                live: 0,
                next: 0,
            })),
        }
    }

    /// Slot capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        SlotState::lock(&self.state).capacity
    }

    /// Slots currently pinned by live surfaces.
    #[must_use]
    pub fn live(&self) -> usize {
        SlotState::lock(&self.state).live
    }

    /// One slot lease for a new surface.
    ///
    /// # Errors
    ///
    /// Returns [`MediaCodecError::NoFreeSlot`] when live surfaces hold every
    /// slot.
    pub fn acquire(&self) -> Result<SlotGuard, MediaCodecError> {
        let generation = {
            let mut state = SlotState::lock(&self.state);
            if state.live >= state.capacity {
                return Err(MediaCodecError::NoFreeSlot);
            }
            state.live += 1;
            let generation = state.next;
            state.next += 1;
            generation
        };
        Ok(SlotGuard {
            state: Arc::clone(&self.state),
            generation,
        })
    }
}

/// One pinned decoder slot; drop releases the slot.
#[derive(Debug)]
pub struct SlotGuard {
    /// Shared live count plus capacity.
    state: Arc<Mutex<SlotState>>,
    /// Pool-issued sequence number.
    generation: u64,
}

impl SlotGuard {
    /// Slot capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        SlotState::lock(&self.state).capacity
    }

    /// Slots currently pinned by live surfaces.
    #[must_use]
    pub fn live(&self) -> usize {
        SlotState::lock(&self.state).live
    }
}

impl Drop for SlotGuard {
    /// Slot released back to the pool.
    fn drop(&mut self) {
        let mut state = SlotState::lock(&self.state);
        state.live = state.live.saturating_sub(1);
    }
}

/// Pool counts behind the mutex.
#[derive(Debug)]
struct SlotState {
    /// Slot capacity.
    capacity: usize,
    /// Slots currently pinned.
    live: usize,
    /// Next slot generation.
    next: u64,
}

impl SlotState {
    /// Pool state; a poisoned mutex still yields its counts.
    fn lock(pool: &Arc<Mutex<Self>>) -> std::sync::MutexGuard<'_, Self> {
        pool.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, GpuExport, PixelFormat, Plane, Surface};

    use super::{MediaCodecSurface, SurfaceSlots};
    use crate::error::MediaCodecError;

    fn frame_16x16_nv12() -> CpuFrame {
        let size = PictureSize::new(16, 16).expect("valid size");
        let y = Plane::new(Bytes::from(vec![3_u8; 256]), 16).expect("valid y");
        let uv = Plane::new(Bytes::from(vec![7_u8; 128]), 16).expect("valid uv");
        CpuFrame::new(size, PixelFormat::Nv12, vec![y, uv]).expect("valid frame")
    }

    fn surface() -> (MediaCodecSurface, SurfaceSlots) {
        let slots = SurfaceSlots::new(2);
        let coded = PictureSize::new(32, 16).expect("valid coded");
        let slot = slots.acquire().expect("free slot");
        (
            MediaCodecSurface::new(frame_16x16_nv12(), coded, slot),
            slots,
        )
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn metadata_matches_frame_and_layout() {
        let (surface, _slots) = surface();
        assert_eq!(surface.size(), PictureSize::new(16, 16).expect("valid"));
        assert_eq!(surface.format(), PixelFormat::Nv12);
        assert_eq!(surface.slice(), 0);
        assert_eq!(surface.coded(), PictureSize::new(32, 16).expect("valid"));
        assert_eq!(surface.origin(), (0, 0));
        assert_eq!(surface.frame().planes().len(), 2);
    }

    #[test]
    fn export_answers_opaque() {
        let (surface, _slots) = surface();
        assert!(matches!(
            surface.export(),
            GpuExport::Opaque { name: "mediacodec" }
        ));
    }

    #[test]
    fn frame_shares_backing_without_copy() {
        let frame = frame_16x16_nv12();
        let base = frame.planes()[0].data().as_ptr();
        let slots = SurfaceSlots::new(1);
        let coded = PictureSize::new(16, 16).expect("valid coded");
        let slot = slots.acquire().expect("free slot");
        let surface = MediaCodecSurface::new(frame, coded, slot);
        assert_eq!(surface.frame().planes()[0].data().as_ptr(), base);
    }

    #[test]
    fn try_wait_returns_on_complete_pixels() {
        let (surface, _slots) = surface();
        surface.try_wait().expect("wait completes");
    }

    #[test]
    fn surface_trait_delegates_to_inherent_accessors() {
        let (surface, _slots) = surface();
        assert_eq!(
            Surface::size(&surface),
            PictureSize::new(16, 16).expect("valid")
        );
        assert_eq!(Surface::format(&surface), PixelFormat::Nv12);
        assert_eq!(Surface::slice(&surface), 0);
        assert_eq!(
            Surface::coded(&surface),
            PictureSize::new(32, 16).expect("valid")
        );
        assert_eq!(Surface::origin(&surface), (0, 0));
        assert!(matches!(
            Surface::export(&surface),
            GpuExport::Opaque { name: "mediacodec" }
        ));
    }

    #[test]
    fn identity_carries_size_format_and_generation() {
        let slots = SurfaceSlots::new(2);
        let coded = PictureSize::new(16, 16).expect("valid coded");
        let first = MediaCodecSurface::new(
            frame_16x16_nv12(),
            coded,
            slots.acquire().expect("free slot"),
        );
        let second = MediaCodecSurface::new(
            frame_16x16_nv12(),
            coded,
            slots.acquire().expect("free slot"),
        );
        let first_id = first.identity();
        let second_id = second.identity();
        assert_eq!(first_id.size(), PictureSize::new(16, 16).expect("valid"));
        assert_eq!(first_id.format(), PixelFormat::Nv12);
        assert_eq!(first_id.generation(), 0);
        assert_eq!(second_id.generation(), 1);
    }

    #[test]
    fn generations_never_reused_after_release() {
        let slots = SurfaceSlots::new(1);
        let coded = PictureSize::new(16, 16).expect("valid coded");
        let first = MediaCodecSurface::new(
            frame_16x16_nv12(),
            coded,
            slots.acquire().expect("free slot"),
        );
        assert_eq!(first.identity().generation(), 0);
        drop(first);
        let second = MediaCodecSurface::new(
            frame_16x16_nv12(),
            coded,
            slots.acquire().expect("free slot"),
        );
        assert_eq!(second.identity().generation(), 1);
    }

    #[test]
    fn lease_bytes_sums_plane_bytes() {
        let (surface, _slots) = surface();
        assert_eq!(surface.lease_bytes(), 256 + 128);
    }

    #[test]
    fn guard_reports_pool_counts() {
        let slots = SurfaceSlots::new(2);
        let first = slots.acquire().expect("slot one");
        assert_eq!(first.capacity(), 2);
        assert_eq!(first.live(), 1);
        let _second = slots.acquire().expect("slot two");
        assert_eq!(first.live(), 2);
        assert_eq!(slots.live(), 2);
    }

    #[test]
    fn pool_exhaustion_is_loud() {
        let slots = SurfaceSlots::new(2);
        assert_eq!(slots.capacity(), 2);
        assert_eq!(slots.live(), 0);
        let first = slots.acquire().expect("slot one");
        let _second = slots.acquire().expect("slot two");
        assert_eq!(slots.live(), 2);
        assert_eq!(slots.acquire().unwrap_err(), MediaCodecError::NoFreeSlot);
        drop(first);
        assert_eq!(slots.live(), 1);
        let _third = slots.acquire().expect("slot freed");
        assert_eq!(slots.live(), 2);
    }

    #[test]
    fn surface_drop_releases_its_slot() {
        let slots = SurfaceSlots::new(1);
        let coded = PictureSize::new(16, 16).expect("valid coded");
        let slot = slots.acquire().expect("free slot");
        let surface = MediaCodecSurface::new(frame_16x16_nv12(), coded, slot);
        assert_eq!(slots.live(), 1);
        drop(surface);
        assert_eq!(slots.live(), 0);
        slots.acquire().expect("slot freed");
    }

    #[test]
    fn zero_capacity_never_acquires() {
        let slots = SurfaceSlots::new(0);
        assert_eq!(slots.acquire().unwrap_err(), MediaCodecError::NoFreeSlot);
    }

    #[test]
    fn guard_outliving_pool_still_releases() {
        let slots = SurfaceSlots::new(1);
        let mirror = slots.clone();
        let guard = slots.acquire().expect("free slot");
        assert_eq!(mirror.live(), 1);
        drop(slots);
        drop(guard);
        assert_eq!(mirror.live(), 0);
        mirror.acquire().expect("slot freed");
    }

    #[test]
    fn default_capacity_is_sixteen() {
        assert_eq!(SurfaceSlots::DEFAULT_CAPACITY, 16);
        assert_eq!(
            SurfaceSlots::new(SurfaceSlots::DEFAULT_CAPACITY).capacity(),
            16
        );
    }

    #[test]
    fn surfaces_are_send_sync() {
        assert_send_sync::<MediaCodecSurface>();
        assert_send_sync::<SurfaceSlots>();
        assert_send_sync::<super::SlotGuard>();
    }
}
