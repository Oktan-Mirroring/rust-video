//! H.264 decode behind the facade `Decoder` trait.

use crate::backend::Backend;
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi::VaSliceParameterBufferH264;
use crate::frontend::{Frontend, PendingPicture, Picture, SurfaceSlot};
use crate::params::Params;
use crate::vendor::h264::parser::{SliceHeader, SliceType};

use bytes::Bytes;
use rust_video::{
    CpuFrame, Decoder, GpuExport, GpuFrame, Lease, Packet, PixelFormat, Plane, SharedDevice,
};
use rust_video_caps::common::PictureSize;

/// H.264 decoder over a VA-API display.
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
        }
    }

    /// Decode `pending` into `frames`.
    fn decode_picture(
        &mut self,
        pending: PendingPicture,
        frames: &mut Vec<VaApiFrame>,
    ) -> Result<(), VaApiError> {
        self.ensure_backend()?;
        let (picture, bumped) = self.frontend.begin(pending)?;
        self.read_slots(bumped, frames)?;
        let slot = self.acquire_slot()?;
        self.submit_picture(&picture, slot)?;
        let outputs = self.frontend.commit(picture, slot)?;
        self.read_slots(outputs, frames)
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

    /// Free pool slot.
    fn acquire_slot(&self) -> Result<SurfaceSlot, VaApiError> {
        let pool = self.backend.as_ref().map_or(0, Backend::pool_size);
        let end = u32::try_from(pool).map_err(|_| VaApiError::DpbFull)?;
        (0..end)
            .map(SurfaceSlot::new)
            .find(|slot| !self.frontend.slot_in_use(slot.index()))
            .ok_or(VaApiError::DpbFull)
    }

    /// Submit `picture` on `slot`.
    fn submit_picture(&self, picture: &Picture, slot: SurfaceSlot) -> Result<(), VaApiError> {
        let (backend, _) = self.active()?;
        let params = Params::new(self.frontend.dpb(), backend.surfaces());
        let surface = backend
            .surfaces()
            .get(slot.index() as usize)
            .copied()
            .ok_or_else(|| {
                VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
            })?;
        let first = picture
            .slices()
            .first()
            .ok_or_else(|| VaApiError::Parse("picture without slices".to_owned()))?;
        let picture_params = params.picture(
            picture.sps(),
            picture.pps(),
            first.header(),
            picture.pic(),
            surface,
        )?;
        let matrix = Params::matrix(picture.pps());
        let max_pic_num = i32::try_from(picture.sps().max_frame_num())
            .map_err(|_| VaApiError::Parse("frame_num range".to_owned()))?;
        let mut slice_params = Vec::with_capacity(picture.slices().len());
        for slice in picture.slices() {
            let (list0, list1) = self.slice_lists(picture, slice.header(), max_pic_num)?;
            slice_params.push(params.slice(
                slice.header(),
                slice.data().len(),
                &list0,
                &list1,
                picture.sps(),
                picture.pps(),
            )?);
        }
        let paired: Vec<(&VaSliceParameterBufferH264, &[u8])> = slice_params
            .iter()
            .zip(picture.slices().iter())
            .map(|(params, slice)| (params, slice.data()))
            .collect();
        backend.submit(surface, &picture_params, &matrix, &paired)
    }

    /// Reference lists for one slice header.
    fn slice_lists(
        &self,
        picture: &Picture,
        header: &SliceHeader,
        max_pic_num: i32,
    ) -> Result<(Vec<usize>, Vec<usize>), VaApiError> {
        let initial = picture.initial();
        let curr_pic_num = picture.pic().pic_num;
        match header.slice_type {
            SliceType::P | SliceType::Sp => {
                let list0 = self.frontend.modify(
                    header,
                    &initial.ref_pic_list_p0,
                    false,
                    curr_pic_num,
                    max_pic_num,
                )?;
                Ok((list0, Vec::new()))
            }
            SliceType::B => {
                let list0 = self.frontend.modify(
                    header,
                    &initial.ref_pic_list_b0,
                    false,
                    curr_pic_num,
                    max_pic_num,
                )?;
                let list1 = self.frontend.modify(
                    header,
                    &initial.ref_pic_list_b1,
                    true,
                    curr_pic_num,
                    max_pic_num,
                )?;
                Ok((list0, list1))
            }
            SliceType::I | SliceType::Si => Ok((Vec::new(), Vec::new())),
        }
    }

    /// Output frames for `slots` in order.
    fn read_slots(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
    ) -> Result<(), VaApiError> {
        let crop = self.crop()?;
        let (_, _, width, height) = crop;
        self.read_cropped(slots, frames, (width, height), crop)
    }

    /// Output frames for `slots` under an explicit crop rectangle.
    fn read_cropped(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
    ) -> Result<(), VaApiError> {
        let (backend, shared) = self.active()?;
        let frame_size = PictureSize::new(size.0, size.1).ok_or(VaApiError::BadExtent {
            width: size.0,
            height: size.1,
        })?;
        let mut generation = self.generation;
        for slot in slots {
            let surface = backend
                .surfaces()
                .get(slot.index() as usize)
                .copied()
                .ok_or_else(|| {
                    VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
                })?;
            let data = Bytes::from(backend.read_nv12(surface, crop)?);
            let lease = shared.try_acquire(data.len() as u64)?;
            frames.push(VaApiFrame {
                identity: GpuFrame::new(frame_size, PixelFormat::Nv12, generation),
                data,
                lease,
            });
            generation = generation.wrapping_add(1);
        }
        self.generation = generation;
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
        let mut ingest = self.frontend.ingest(packet.data())?;
        let mut frames = Vec::new();
        if ingest.reconfigured() {
            let drained = ingest.drained();
            if let (Some((width, height)), Some(crop)) =
                (ingest.drained_size(), ingest.drained_crop())
            {
                self.read_cropped(drained, &mut frames, (width, height), crop)?;
            }
            self.backend = None;
            self.shared = None;
        }
        for pending in ingest.pictures() {
            self.decode_picture(pending, &mut frames)?;
        }
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        let mut frames = Vec::new();
        if let Some(pending) = self.frontend.take_pending() {
            self.decode_picture(pending, &mut frames)?;
        }
        let drained = self.frontend.drain();
        if !drained.is_empty() {
            self.read_slots(drained, &mut frames)?;
        }
        Ok(frames)
    }
}

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct VaApiFrame {
    /// Facade identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
}

impl VaApiFrame {
    /// Facade identity of the frame.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        self.identity
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
    /// Opaque backend descriptor: the tier readbacks to CPU, so no
    /// platform handle exists to hand out. Infallible by shape, never
    /// `BackendAbsent`.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        GpuExport::Opaque { name: "vaapi" }
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

#[cfg(test)]
mod tests {
    use super::VaApiFrame;

    use bytes::Bytes;
    use rust_video::{GpuExport, GpuFrame, PixelFormat, SharedDevice};
    use rust_video_caps::common::PictureSize;

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
}
