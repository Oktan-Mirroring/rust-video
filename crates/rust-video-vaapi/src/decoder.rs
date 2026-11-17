//! H.264 decode behind the caps `Decoder` trait.

#![allow(unsafe_code)]

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

use crate::backend::{Backend, Pool, StatsInner};
use crate::buffer::bytes_of;
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi::{self, VaSliceParameterBufferH264};
use crate::frontend::{Frontend, PendingPicture, Picture, SurfaceSlot};
use crate::h264::parser::{SliceHeader, SliceType};
use crate::params::Params;
use crate::seam::{SurfaceId, VaSeam};
use crate::sys;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, DmaBufPlane, GpuExport, GpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Lease, SharedDevice};

/// H.264 decoder over a VA-API display.
///
/// Stays on its thread: the borrowed display is `Send` without
/// `Sync`, so the borrow keeps the decoder off other threads.
pub struct H264Decoder<'a> {
    /// Display owning the decode context.
    device: &'a VaApiDevice,
    /// Annex B frontend: pictures ordered against the DPB.
    frontend: Frontend,
    /// Live decode context, created from the first sequence set.
    backend: Option<Backend>,
    /// VRAM budget sized to the surface pool.
    shared: Option<SharedDevice>,
    /// Next frame generation number.
    generation: u64,
    /// Scratch slice parameters, reused across pictures.
    slice_params: Vec<VaSliceParameterBufferH264>,
    /// Scratch list-zero indices, reused across slices.
    list0: Vec<usize>,
    /// Scratch list-one indices, reused across slices.
    list1: Vec<usize>,
}

impl<'a> H264Decoder<'a> {
    /// Decoder over `device`; the context starts on the first sequence set.
    #[must_use]
    pub fn open(device: &'a VaApiDevice) -> Self {
        Self {
            device,
            frontend: Frontend::new(),
            backend: None,
            shared: None,
            generation: 0,
            slice_params: Vec::new(),
            list0: Vec::new(),
            list1: Vec::new(),
        }
    }

    /// Frames decoded from `packet` with device surfaces attached.
    ///
    /// The frames skip the NV12 readback: `nv12` is empty and `to_cpu`
    /// is absent. Each frame pins its pool slot until dropped, so a
    /// caller holding more frames than the pool holds back fails with
    /// `NoFreeSlot` when live frames pin every free slot, or `DpbFull`
    /// when the picture buffer itself is full; `decode` copies pixels
    /// and releases immediately.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet`.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.decode_inner(packet, false)
    }

    /// Device owning the decode context.
    #[must_use]
    pub fn device(&self) -> &VaApiDevice {
        self.device
    }

    /// Live decode context, absent before the first sequence set.
    ///
    /// The borrow ties the context to the decoder; see
    /// [`Backend`](crate::Backend) for the native handles behind it.
    #[must_use]
    pub fn context(&self) -> Option<&Backend> {
        self.backend.as_ref()
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.flush_inner(false)
    }

    /// Submit gauges of the live context, absent before the first sequence set.
    #[must_use]
    pub fn submit_stats(&self) -> Option<crate::backend::SubmitStats> {
        self.backend.as_ref().map(Backend::stats)
    }

    /// Decode `packet`, reading back pixels when `readback` holds.
    fn decode_inner(
        &mut self,
        packet: &Packet,
        readback: bool,
    ) -> Result<Vec<VaApiFrame>, VaApiError> {
        let owned = packet.bytes();
        let mut ingest = self.frontend.ingest(&owned)?;
        let mut frames = Vec::new();
        if ingest.reconfigured() {
            let drained = ingest.drained();
            match (ingest.drained_size(), ingest.drained_crop()) {
                (Some(size), Some(crop)) => {
                    self.read_cropped(drained, &mut frames, size, crop, readback)?;
                }
                _ if drained.is_empty() => {}
                _ => {
                    return Err(VaApiError::Parse(
                        "reconfig dropped decoded pictures".to_owned(),
                    ));
                }
            }
            self.maybe_drop_backend();
        }
        for pending in ingest.pictures() {
            self.decode_picture(pending, &mut frames, readback)?;
        }
        Ok(frames)
    }

    /// Flush held frames, reading back pixels when `readback` holds.
    fn flush_inner(&mut self, readback: bool) -> Result<Vec<VaApiFrame>, VaApiError> {
        let mut frames = Vec::new();
        if let Some(pending) = self.frontend.take_pending() {
            self.decode_picture(pending, &mut frames, readback)?;
        }
        let drained = self.frontend.drain();
        if !drained.is_empty() {
            self.read_slots(drained, &mut frames, readback)?;
        }
        Ok(frames)
    }

    /// Decode `pending` into `frames`.
    fn decode_picture(
        &mut self,
        pending: PendingPicture,
        frames: &mut Vec<VaApiFrame>,
        readback: bool,
    ) -> Result<(), VaApiError> {
        self.ensure_backend()?;
        let (picture, bumped) = self.frontend.begin(pending)?;
        self.read_slots(bumped, frames, readback)?;
        let slot = self.acquire_slot()?;
        self.submit_picture(&picture, slot)?;
        let outputs = self.frontend.commit(picture, slot)?;
        self.read_slots(outputs, frames, readback)
    }

    /// Live context, creating it from the active sequence set.
    fn ensure_backend(&mut self) -> Result<(), VaApiError> {
        if self.backend.is_some() {
            return Ok(());
        }
        let sps = self
            .frontend
            .sps()
            .cloned()
            .ok_or_else(|| VaApiError::Parse("picture before sequence set".to_owned()))?;
        let profile = Params::profile(sps.profile_idc)?;
        let (width, height) = self
            .frontend
            .coded_size()
            .ok_or_else(|| VaApiError::Parse("picture before sequence set".to_owned()))?;
        let count = self.frontend.max_dpb_frames().unwrap_or(16) + 4;
        let backend = Backend::open(self.device, profile, width, height, count)?;
        self.shared = Some(SharedDevice::new("vaapi", u64::MAX));
        self.backend = Some(backend);
        Ok(())
    }

    /// Drop the context unless the new sequence reuses its pool.
    fn maybe_drop_backend(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let reuse = self
            .frontend
            .sps()
            .cloned()
            .and_then(|sps| Params::profile(sps.profile_idc).ok())
            .is_some_and(|profile| {
                profile == backend.profile()
                    && self.frontend.coded_size() == Some(backend.coded_size())
                    && self.frontend.max_dpb_frames().unwrap_or(16) + 4 == backend.pool_size()
            });
        if reuse {
            log::debug!("va-api: reconfig reuses the surface pool");
        } else {
            self.backend = None;
            self.shared = None;
        }
    }

    /// Free pool slot: neither DPB-backed nor leased to a live frame.
    ///
    /// `NoFreeSlot` when live frames pin every DPB-free slot;
    /// `DpbFull` when no slot is DPB-free.
    fn acquire_slot(&self) -> Result<SurfaceSlot, VaApiError> {
        let pool = self.backend.as_ref().map_or(0, Backend::pool_size);
        let end = u32::try_from(pool).map_err(|_| VaApiError::NoFreeSlot)?;
        let mut pinned = false;
        for index in 0..end {
            let slot = SurfaceSlot::new(index);
            if self.frontend.slot_in_use(slot.index()) {
                continue;
            }
            if self
                .backend
                .as_ref()
                .is_some_and(|backend| backend.surface_live(slot.index()))
            {
                pinned = true;
                continue;
            }
            return Ok(slot);
        }
        Err(if pinned {
            VaApiError::NoFreeSlot
        } else {
            VaApiError::DpbFull
        })
    }

    /// Submit `picture` on `slot`.
    fn submit_picture(&mut self, picture: &Picture, slot: SurfaceSlot) -> Result<(), VaApiError> {
        let surface = self
            .backend
            .as_ref()
            .and_then(|backend| backend.surfaces().get(slot.index() as usize).copied())
            .ok_or_else(|| {
                VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
            })?;
        let first = picture
            .slices()
            .first()
            .ok_or_else(|| VaApiError::Parse("picture without slices".to_owned()))?;
        let max_pic_num = i32::try_from(picture.sps().max_frame_num())
            .map_err(|_| VaApiError::Parse("frame_num range".to_owned()))?;
        let backend = self
            .backend
            .as_ref()
            .ok_or_else(|| VaApiError::Parse("decode before sequence set".to_owned()))?;
        let params = Params::new(self.frontend.dpb(), backend.surfaces());
        let picture_params = params.picture(
            picture.sps(),
            picture.pps(),
            first.header(),
            picture.pic(),
            surface,
        )?;
        let matrix = Params::matrix(picture.pps());
        self.slice_params.clear();
        for slice in picture.slices() {
            Self::slice_lists(
                &self.frontend,
                &mut self.list0,
                &mut self.list1,
                picture,
                slice.header(),
                max_pic_num,
            )?;
            self.slice_params.push(params.slice(
                slice.header(),
                slice.data().len(),
                &self.list0,
                &self.list1,
                picture.sps(),
                picture.pps(),
            )?);
        }
        debug_assert_eq!(self.slice_params.len(), picture.slices().len());
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| VaApiError::Parse("decode before sequence set".to_owned()))?;
        backend.submit_slices(
            surface,
            bytes_of(&picture_params),
            Some(bytes_of(&matrix)),
            &self.slice_params,
            |index| picture.slices()[index].data(),
        )
    }

    /// Reference lists for one slice header, rebuilt in `list0`/`list1`.
    fn slice_lists(
        frontend: &Frontend,
        list0: &mut Vec<usize>,
        list1: &mut Vec<usize>,
        picture: &Picture,
        header: &SliceHeader,
        max_pic_num: i32,
    ) -> Result<(), VaApiError> {
        let initial = picture.initial();
        let curr_pic_num = picture.pic().pic_num;
        match header.slice_type {
            SliceType::P | SliceType::Sp => {
                frontend.modify_into(
                    header,
                    &initial.p_list,
                    false,
                    curr_pic_num,
                    max_pic_num,
                    list0,
                )?;
                list1.clear();
            }
            SliceType::B => {
                frontend.modify_into(
                    header,
                    &initial.b_list0,
                    false,
                    curr_pic_num,
                    max_pic_num,
                    list0,
                )?;
                frontend.modify_into(
                    header,
                    &initial.b_list1,
                    true,
                    curr_pic_num,
                    max_pic_num,
                    list1,
                )?;
            }
            SliceType::I | SliceType::Si => {
                list0.clear();
                list1.clear();
            }
        }
        Ok(())
    }

    /// Output frames for `slots` in order.
    fn read_slots(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        readback: bool,
    ) -> Result<(), VaApiError> {
        let crop = self.crop()?;
        let (_, _, width, height) = crop;
        self.read_cropped(slots, frames, (width, height), crop, readback)
    }

    /// Output frames for `slots` under an explicit crop rectangle.
    fn read_cropped(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
        readback: bool,
    ) -> Result<(), VaApiError> {
        let (backend, shared) = self.active()?;
        self.generation = VaApiFrame::emit_into(
            self.device,
            backend,
            shared,
            Emit::new(slots, size, crop, readback),
            frames,
            self.generation,
        )?;
        Ok(())
    }

    /// Visible rectangle of the active sequence.
    fn crop(&self) -> Result<(u32, u32, u32, u32), VaApiError> {
        self.frontend
            .visible_rect()
            .ok_or_else(|| VaApiError::Parse("picture before sequence set".to_owned()))
    }

    /// Live context and budget.
    fn active(&self) -> Result<(&Backend, &SharedDevice), VaApiError> {
        match (self.backend.as_ref(), self.shared.as_ref()) {
            (Some(backend), Some(shared)) => Ok((backend, shared)),
            _ => Err(VaApiError::Parse("decode before sequence set".to_owned())),
        }
    }
}

impl Decoder for H264Decoder<'_> {
    type Frame = VaApiFrame;
    type Error = VaApiError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.decode_inner(packet, true)
    }

    fn flush(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.flush_inner(true)
    }
}

/// Output-frame request: slots, geometry, and readback mode.
pub(crate) struct Emit {
    /// Pool slots to output, in order.
    slots: Vec<SurfaceSlot>,
    /// Frame size in luma samples.
    size: (u32, u32),
    /// Crop rectangle as `(x, y, width, height)`.
    crop: (u32, u32, u32, u32),
    /// True to read back pixels; false to attach device surfaces.
    readback: bool,
}

impl Emit {
    /// Request for `slots` at `size` cropped to `crop`.
    pub(crate) fn new(
        slots: Vec<SurfaceSlot>,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
        readback: bool,
    ) -> Self {
        Self {
            slots,
            size,
            crop,
            readback,
        }
    }
}

/// Decoded frame: NV12 bytes with identity, lease, and device surface.
///
/// A live surface keeps its own resources alive: dropping the decoder never invalidates outstanding surfaces, and dropping a surface releases its slot.
///
/// `Send`, never `Sync`: hand the frame to another thread when idle,
/// but export from one thread at a time. The surface handle behind
/// the frame is only dereferenced through driver calls the owner
/// serializes.
pub struct VaApiFrame {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
    /// Device surface behind the frame, absent without one.
    device: Option<DeviceSurface>,
    /// Full texture extent in luma samples.
    coded: PictureSize,
    /// Crop left/top in luma samples.
    origin: (u32, u32),
}

impl VaApiFrame {
    /// Output frames for `emit` appended to `frames`, returning the next generation.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a slot left the pool, the size is empty, or
    /// the budget refuses the bytes.
    pub(crate) fn emit_into(
        device: &VaApiDevice,
        backend: &Backend,
        shared: &SharedDevice,
        emit: Emit,
        frames: &mut Vec<Self>,
        generation: u64,
    ) -> Result<u64, VaApiError> {
        let frame_size =
            PictureSize::new(emit.size.0, emit.size.1).ok_or(VaApiError::BadExtent {
                width: emit.size.0,
                height: emit.size.1,
            })?;
        let (coded_width, coded_height) = backend.coded_size();
        let coded = PictureSize::new(coded_width, coded_height).ok_or(VaApiError::BadExtent {
            width: coded_width,
            height: coded_height,
        })?;
        let origin = (emit.crop.0, emit.crop.1);
        frames.reserve(emit.slots.len());
        let mut generation = generation;
        for slot in emit.slots {
            let surface = backend
                .surfaces()
                .get(slot.index() as usize)
                .copied()
                .ok_or_else(|| {
                    VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
                })?;
            let data = if emit.readback {
                Bytes::from(backend.read_nv12(surface, emit.crop)?)
            } else {
                Bytes::new()
            };
            let lease = shared.try_acquire(data.len() as u64)?;
            let device = if emit.readback {
                None
            } else {
                backend.surface_guard(slot.index()).map(|guard| {
                    DeviceSurface::new(
                        device.display(),
                        surface,
                        guard,
                        backend.pool(),
                        backend.gauges(),
                    )
                })
            };
            frames.push(Self {
                identity: GpuFrame::new(frame_size, PixelFormat::Nv12, generation),
                data,
                lease,
                device,
                coded,
                origin,
            });
            generation = generation.wrapping_add(1);
        }
        Ok(generation)
    }

    /// Frame identity.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        self.identity
    }

    /// Visible picture size in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.identity.size()
    }

    /// Pixel layout of the frame.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.identity.format()
    }

    /// Array layer behind the picture.
    ///
    /// VA-API surfaces carry a single layer.
    #[must_use]
    pub fn slice(&self) -> u32 {
        0
    }

    /// Full texture extent in luma samples.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.coded
    }

    /// Crop left/top in luma samples: the visible origin in the texture.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        self.origin
    }

    /// Native surface behind the frame, absent after a pixels-only readback.
    ///
    /// See [`SurfaceId`](crate::SurfaceId) for the threading and
    /// lifetime rules.
    #[must_use]
    pub fn surface(&self) -> Option<SurfaceId> {
        self.device.as_ref().map(|surface| surface.surface)
    }

    /// Block until the frame's decode finishes.
    ///
    /// Readback frames return immediately: their pixels were copied
    /// after a sync. External readers import the
    /// [`export`](Self::export) ticket only after this returns, so
    /// they never read a partial picture.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver reports the surface failed.
    pub fn wait(&self) -> Result<(), VaApiError> {
        match self.device.as_ref() {
            Some(surface) => surface.wait(),
            None => Ok(()),
        }
    }

    /// NV12 frame bytes in row-major order.
    #[must_use]
    pub fn nv12(&self) -> &[u8] {
        &self.data
    }

    /// Budget bytes the frame holds.
    #[must_use]
    pub fn lease_bytes(&self) -> u64 {
        self.lease.bytes()
    }

    /// Platform export handle for the frame.
    ///
    /// DMA-BUF planes while the device surface exports; an opaque
    /// backend descriptor without a surface, after a reconfig retired
    /// the pool, or when the driver refuses the export. Infallible by
    /// shape, never `BackendAbsent`.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        match self.device.as_ref().and_then(DeviceSurface::export) {
            Some(planes) => GpuExport::DmaBuf {
                format: PixelFormat::Nv12,
                planes,
            },
            None => GpuExport::Opaque { name: "vaapi" },
        }
    }

    /// Display and surface behind the frame, absent without a device surface.
    #[must_use]
    pub(crate) fn device_handle(&self) -> Option<(ffi::VaDisplay, ffi::VaSurfaceId)> {
        self.device
            .as_ref()
            .map(|surface| (surface.display, surface.surface))
    }

    /// CPU copy split into NV12 planes.
    #[must_use]
    pub fn to_cpu(&self) -> Option<CpuFrame> {
        let size = self.identity.size();
        let luma = u64::from(size.width()) * u64::from(size.height());
        let y_len = usize::try_from(luma).ok()?;
        let uv_len = y_len / 2;
        if self.data.len() < y_len + uv_len {
            return None;
        }
        let y = self.data.slice(..y_len);
        let uv = self.data.slice(y_len..y_len + uv_len);
        let stride = size.width();
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![Plane::new(y, stride)?, Plane::new(uv, stride)?],
        )
    }
}

/// Device surface behind a frame, leased against pool reuse.
///
/// A live surface keeps its own resources alive: dropping the decoder never invalidates outstanding surfaces, and dropping a surface releases its slot.
struct DeviceSurface {
    /// Display owning the surface.
    display: ffi::VaDisplay,
    /// Surface handle.
    surface: ffi::VaSurfaceId,
    /// Pool-slot lease; the slot stays out of the free set while live.
    _guard: Arc<()>,
    /// Decode pool behind the surface; the context and pool surfaces
    /// stay alive past decoder drop while held.
    _pool: Arc<Pool>,
    /// Submit gauges of the owning context.
    gauges: Arc<StatsInner>,
}

impl DeviceSurface {
    /// Surface on `display` leased by `guard`.
    fn new(
        display: ffi::VaDisplay,
        surface: ffi::VaSurfaceId,
        guard: Arc<()>,
        pool: Arc<Pool>,
        gauges: Arc<StatsInner>,
    ) -> Self {
        Self {
            display,
            surface,
            _guard: guard,
            _pool: pool,
            gauges,
        }
    }

    /// Block until the surface finishes decoding.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver reports the surface failed.
    fn wait(&self) -> Result<(), VaApiError> {
        // Sound: the display and surface are live behind the pool lease.
        let outcome = unsafe { sys::vaSyncSurface(self.display, self.surface) };
        self.gauges.note_synced();
        sys::status(outcome)
    }

    /// NV12 DMA-BUF planes for the surface, absent when the export fails.
    fn export(&self) -> Option<[DmaBufPlane; 2]> {
        // Sound: the display and surface are live behind the pool lease.
        let synced = unsafe { sys::vaSyncSurface(self.display, self.surface) };
        if synced != ffi::VA_STATUS_SUCCESS {
            return None;
        }
        self.gauges.note_synced();
        let mut descriptor = std::mem::MaybeUninit::<ffi::VaDrmPrimeDescriptor>::uninit();
        // Sound: the descriptor outlives the call.
        let status = unsafe {
            sys::vaExportSurfaceHandle(
                self.display,
                self.surface,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                ffi::VA_EXPORT_SURFACE_READ_ONLY | ffi::VA_EXPORT_SURFACE_SEPARATE_LAYERS,
                descriptor.as_mut_ptr().cast(),
            )
        };
        if status != ffi::VA_STATUS_SUCCESS {
            return None;
        }
        // Sound: the export succeeded, so the descriptor is initialized.
        let descriptor = unsafe { descriptor.assume_init() };
        Self::planes(&descriptor)
    }

    /// Owned planes from an exported descriptor.
    ///
    /// The descriptors transfer from the transient export: the caller
    /// never touches them again after this returns.
    fn planes(descriptor: &ffi::VaDrmPrimeDescriptor) -> Option<[DmaBufPlane; 2]> {
        let count = usize::try_from(descriptor.num_objects)
            .unwrap_or(usize::MAX)
            .min(4);
        let mut owned: [Option<OwnedFd>; 4] = Default::default();
        for (slot, object) in owned.iter_mut().zip(descriptor.objects.iter()).take(count) {
            if object.fd >= 0 {
                // Sound: the export hands ownership of each descriptor to the caller.
                *slot = Some(unsafe { OwnedFd::from_raw_fd(object.fd) });
            }
        }
        if descriptor.num_objects == 0
            || descriptor.num_objects > 4
            || descriptor.fourcc != ffi::VA_FOURCC_NV12
            || descriptor.num_layers != 2
        {
            return None;
        }
        let mut layout = [(0u32, 0u32, 0u64, 0usize); 2];
        for (slot, layer) in layout.iter_mut().zip(descriptor.layers.iter()).take(2) {
            if layer.num_planes != 1 {
                return None;
            }
            let object = usize::try_from(layer.object_index[0]).ok()?;
            if object >= count || owned[object].is_none() {
                return None;
            }
            slot.0 = layer.pitch[0];
            slot.1 = layer.offset[0];
            slot.2 = descriptor.objects[object].drm_format_modifier;
            slot.3 = object;
        }
        let (stride0, offset0, modifier0, object0) = layout[0];
        let (stride1, offset1, modifier1, object1) = layout[1];
        let fd0 = owned[object0].take()?;
        let fd1 = owned[object1].take().or_else(|| fd0.try_clone().ok())?;
        Some([
            DmaBufPlane::new(fd0, stride0, offset0, modifier0),
            DmaBufPlane::new(fd1, stride1, offset1, modifier1),
        ])
    }
}

// Sound: a move hands exclusive ownership to the new thread; the
// surface has no drop effects, and the owner serializes calls.
unsafe impl Send for DeviceSurface {}

#[cfg(test)]
mod tests {
    use super::VaApiFrame;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
    use rust_video_caps::traits::SharedDevice;

    /// Frame over `data` leased against `shared`.
    fn frame(shared: &SharedDevice, width: u32, height: u32, data: Vec<u8>) -> VaApiFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let identity = GpuFrame::new(size, PixelFormat::Nv12, 3);
        let bytes = Bytes::from(data);
        let lease = shared
            .try_acquire(bytes.len() as u64)
            .expect("within budget");
        VaApiFrame {
            identity,
            data: bytes,
            lease,
            device: None,
            coded: size,
            origin: (0, 0),
        }
    }

    #[test]
    fn frame_holds_lease_against_budget() {
        let shared = SharedDevice::new("vaapi", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.lease_bytes(), 12);
        assert_eq!(shared.used(), 12);
        assert_eq!(held.nv12(), &[7; 12]);
        assert_eq!(held.identity().generation(), 3);
    }

    #[test]
    fn dropped_frame_releases_budget() {
        let shared = SharedDevice::new("vaapi", 100);
        {
            let _held = frame(&shared, 4, 2, vec![0; 12]);
            assert_eq!(shared.used(), 12);
        }
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn export_names_backend_without_handle() {
        let shared = SharedDevice::new("vaapi", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert!(matches!(held.export(), GpuExport::Opaque { name: "vaapi" }));
    }

    #[test]
    fn to_cpu_splits_nv12_planes() {
        let shared = SharedDevice::new("vaapi", 100);
        let mut data = vec![1; 8];
        data.extend_from_slice(&[2; 4]);
        let held = frame(&shared, 4, 2, data);
        let cpu = held.to_cpu().expect("full nv12");
        assert_eq!(cpu.format(), PixelFormat::Nv12);
        assert_eq!(cpu.planes().len(), 2);
        assert_eq!(cpu.planes()[0].data(), &[1; 8]);
        assert_eq!(cpu.planes()[0].stride(), 4);
        assert_eq!(cpu.planes()[1].data(), &[2; 4]);
        assert_eq!(cpu.planes()[1].stride(), 4);
    }

    #[test]
    fn to_cpu_rejects_truncated_bytes() {
        let shared = SharedDevice::new("vaapi", 100);
        let held = frame(&shared, 4, 2, vec![0; 11]);
        assert!(held.to_cpu().is_none());
    }

    #[test]
    fn frame_moves_across_threads() {
        fn is_send<T: Send>() {}
        is_send::<VaApiFrame>();
    }

    #[test]
    fn surface_contract_names_geometry_and_provenance() {
        let shared = SharedDevice::new("vaapi", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert_eq!(held.size(), PictureSize::new(4, 2).expect("extent"));
        assert_eq!(held.format(), PixelFormat::Nv12);
        assert_eq!(held.slice(), 0);
        assert_eq!(held.coded(), PictureSize::new(4, 2).expect("extent"));
        assert_eq!(held.origin(), (0, 0));
    }

    #[test]
    fn readback_frame_has_no_surface_but_waits_clean() {
        let shared = SharedDevice::new("vaapi", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert_eq!(held.surface(), None);
        assert!(held.wait().is_ok());
    }

    #[test]
    fn planes_rejects_non_nv12() {
        use std::os::fd::IntoRawFd;

        use crate::ffi::{VaDrmPrimeDescriptor, VaDrmPrimeLayer, VaDrmPrimeObject};

        let fd = std::fs::File::open("/dev/null")
            .expect("null fd")
            .into_raw_fd();
        let descriptor = VaDrmPrimeDescriptor {
            fourcc: 0,
            width: 4,
            height: 2,
            num_objects: 1,
            objects: [
                VaDrmPrimeObject {
                    fd,
                    size: 12,
                    drm_format_modifier: 0,
                },
                VaDrmPrimeObject {
                    fd: -1,
                    size: 0,
                    drm_format_modifier: 0,
                },
                VaDrmPrimeObject {
                    fd: -1,
                    size: 0,
                    drm_format_modifier: 0,
                },
                VaDrmPrimeObject {
                    fd: -1,
                    size: 0,
                    drm_format_modifier: 0,
                },
            ],
            num_layers: 2,
            layers: [VaDrmPrimeLayer {
                drm_format: 0,
                num_planes: 1,
                object_index: [0; 4],
                offset: [0; 4],
                pitch: [4; 4],
            }; 4],
        };
        assert!(super::DeviceSurface::planes(&descriptor).is_none());
    }

    #[test]
    fn planes_maps_shared_and_split_objects() {
        use std::os::fd::{AsRawFd, IntoRawFd};

        use crate::ffi::{VaDrmPrimeDescriptor, VaDrmPrimeLayer, VaDrmPrimeObject};

        fn layer(object: u32, offset: u32, pitch: u32) -> VaDrmPrimeLayer {
            VaDrmPrimeLayer {
                drm_format: 0,
                num_planes: 1,
                object_index: [object, 0, 0, 0],
                offset: [offset, 0, 0, 0],
                pitch: [pitch, 0, 0, 0],
            }
        }

        fn object(fd: i32, modifier: u64) -> VaDrmPrimeObject {
            VaDrmPrimeObject {
                fd,
                size: 12,
                drm_format_modifier: modifier,
            }
        }

        let null = || {
            std::fs::File::open("/dev/null")
                .expect("null fd")
                .into_raw_fd()
        };
        let shared = VaDrmPrimeDescriptor {
            fourcc: crate::ffi::VA_FOURCC_NV12,
            width: 4,
            height: 2,
            num_objects: 1,
            objects: [
                object(null(), 7),
                object(-1, 0),
                object(-1, 0),
                object(-1, 0),
            ],
            num_layers: 2,
            layers: [
                layer(0, 0, 4),
                layer(0, 8, 4),
                layer(0, 0, 0),
                layer(0, 0, 0),
            ],
        };
        let planes = super::DeviceSurface::planes(&shared).expect("shared object");
        assert_ne!(planes[0].fd().as_raw_fd(), planes[1].fd().as_raw_fd());
        assert_eq!((planes[0].stride(), planes[0].offset()), (4, 0));
        assert_eq!((planes[1].stride(), planes[1].offset()), (4, 8));
        assert_eq!(planes[0].modifier(), 7);

        let split = VaDrmPrimeDescriptor {
            fourcc: crate::ffi::VA_FOURCC_NV12,
            width: 4,
            height: 2,
            num_objects: 2,
            objects: [
                object(null(), 0),
                object(null(), 9),
                object(-1, 0),
                object(-1, 0),
            ],
            num_layers: 2,
            layers: [
                layer(0, 0, 4),
                layer(1, 0, 4),
                layer(0, 0, 0),
                layer(0, 0, 0),
            ],
        };
        let planes = super::DeviceSurface::planes(&split).expect("split objects");
        assert_eq!(planes[1].modifier(), 9);
    }
}
