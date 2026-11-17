//! Decoder-issued surfaces over byte-buffer output.
//!
//! A [`MediaCodecSurface`] is one decoded picture: owned CPU pixels plus the
//! canonical surface metadata (`size`, `format`, `slice`, `coded`, `origin`)
//! and a native ticket from [`MediaCodecSurface::export`]. Each live surface
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
//! complete at emission and [`MediaCodecSurface::wait`] observes completion
//! without blocking. There is no fence to poll; the NDK `sync` module only
//! matters to a future output-`Surface` path, which would hand out
//! `AImage`/`HardwareBuffer` objects instead of copies.

use std::sync::{Arc, Mutex};

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat};

use crate::error::MediaCodecError;

/// Decoded picture pinning one decoder slot.
#[derive(Debug)]
pub struct MediaCodecSurface {
    /// Owned picture pixels.
    frame: CpuFrame,
    /// Full buffer extent from stride by slice height.
    coded: PictureSize,
    /// Decoder-issued sequence number plus geometry.
    ticket: SurfaceTicket,
    /// Slot lease; drop releases the slot.
    _slot: SlotGuard,
}

impl MediaCodecSurface {
    /// Surface over `frame` with `coded` extent, `slot` lease, and `generation`.
    #[cfg(any(test, target_os = "android"))]
    pub(crate) fn new(
        frame: CpuFrame,
        coded: PictureSize,
        slot: SlotGuard,
        generation: u64,
    ) -> Self {
        let ticket = SurfaceTicket {
            generation,
            size: frame.size(),
            format: frame.format(),
        };
        Self {
            frame,
            coded,
            ticket,
            _slot: slot,
        }
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

    /// Native ticket: decoder-issued sequence plus geometry.
    ///
    /// The ticket is metadata only; pixels stay in the surface and cross to
    /// other consumers through [`MediaCodecSurface::frame`]. There is no
    /// re-import: byte-buffer mode never mints `AImage`/`HardwareBuffer`
    /// objects.
    #[must_use]
    pub fn export(&self) -> SurfaceTicket {
        self.ticket
    }

    /// Image pixels by reference.
    #[must_use]
    pub fn frame(&self) -> &CpuFrame {
        &self.frame
    }

    /// Blocking completion wait for external readers.
    ///
    /// Byte-buffer pixels complete synchronously at emission, so this
    /// observes completion without blocking. It stays in the contract so
    /// readers written against fence-backed tiers compile unchanged.
    // Receiver is contract surface; completion needs no state read.
    #[allow(clippy::unused_self)]
    pub fn wait(&self) {}
}

/// Native ticket for a [`MediaCodecSurface`].
///
/// Copyable metadata: the decoder-issued sequence number plus the geometry
/// needed to describe the picture without touching its pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SurfaceTicket {
    /// Decoder-issued sequence number.
    generation: u64,
    /// Visible picture extent.
    size: PictureSize,
    /// Pixel layout.
    format: PixelFormat,
}

impl SurfaceTicket {
    /// Decoder-issued sequence number.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Visible picture extent in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.size
    }

    /// Pixel layout.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.format
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
            state: Arc::new(Mutex::new(SlotState { capacity, live: 0 })),
        }
    }

    /// Slot capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.lock().capacity
    }

    /// Slots currently pinned by live surfaces.
    #[must_use]
    pub fn live(&self) -> usize {
        self.lock().live
    }

    /// One slot lease for a new surface.
    ///
    /// # Errors
    ///
    /// Returns [`MediaCodecError::NoFreeSlot`] when live surfaces hold every
    /// slot.
    pub fn acquire(&self) -> Result<SlotGuard, MediaCodecError> {
        let mut state = self.lock();
        if state.live >= state.capacity {
            return Err(MediaCodecError::NoFreeSlot);
        }
        state.live += 1;
        Ok(SlotGuard {
            state: Arc::clone(&self.state),
        })
    }

    /// Pool state; a poisoned mutex still yields its counts.
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One pinned decoder slot; drop releases the slot.
#[derive(Debug)]
pub struct SlotGuard {
    /// Shared live count plus capacity.
    state: Arc<Mutex<SlotState>>,
}

impl Drop for SlotGuard {
    /// Slot released back to the pool.
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

    use super::{MediaCodecSurface, SurfaceSlots};
    use crate::error::MediaCodecError;

    fn frame_16x16_nv12() -> CpuFrame {
        let size = PictureSize::new(16, 16).expect("valid size");
        let y = Plane::new(Bytes::from(vec![3_u8; 256]), 16).expect("valid y");
        let uv = Plane::new(Bytes::from(vec![7_u8; 128]), 16).expect("valid uv");
        CpuFrame::new(size, PixelFormat::Nv12, vec![y, uv]).expect("valid frame")
    }

    fn surface(generation: u64) -> (MediaCodecSurface, SurfaceSlots) {
        let slots = SurfaceSlots::new(2);
        let coded = PictureSize::new(32, 16).expect("valid coded");
        let slot = slots.acquire().expect("free slot");
        (
            MediaCodecSurface::new(frame_16x16_nv12(), coded, slot, generation),
            slots,
        )
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn metadata_matches_frame_and_layout() {
        let (surface, _slots) = surface(9);
        assert_eq!(surface.size(), PictureSize::new(16, 16).expect("valid"));
        assert_eq!(surface.format(), PixelFormat::Nv12);
        assert_eq!(surface.slice(), 0);
        assert_eq!(surface.coded(), PictureSize::new(32, 16).expect("valid"));
        assert_eq!(surface.origin(), (0, 0));
        assert_eq!(surface.frame().planes().len(), 2);
    }

    #[test]
    fn export_carries_generation_and_geometry() {
        let (surface, _slots) = surface(9);
        let ticket = surface.export();
        assert_eq!(ticket.generation(), 9);
        assert_eq!(ticket.size(), surface.size());
        assert_eq!(ticket.format(), surface.format());
        let copy = ticket;
        assert_eq!(copy, ticket);
    }

    #[test]
    fn frame_shares_backing_without_copy() {
        let frame = frame_16x16_nv12();
        let base = frame.planes()[0].data().as_ptr();
        let slots = SurfaceSlots::new(1);
        let coded = PictureSize::new(16, 16).expect("valid coded");
        let slot = slots.acquire().expect("free slot");
        let surface = MediaCodecSurface::new(frame, coded, slot, 0);
        assert_eq!(surface.frame().planes()[0].data().as_ptr(), base);
    }

    #[test]
    fn wait_returns_on_complete_pixels() {
        let (surface, _slots) = surface(0);
        surface.wait();
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
        let surface = MediaCodecSurface::new(frame_16x16_nv12(), coded, slot, 0);
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
        assert_send_sync::<super::SurfaceTicket>();
    }
}
