//! Decode session core: owned GPU resources shared by both codecs.
//!
//! [`Session`] owns the video session, its parameters, the DPB array image,
//! the bitstream and readback buffers, and the per-slot decode metadata.
//! Per-frame command assembly is pure data over these resources; only the
//! final record-and-submit touches the device.

use ash::vk;

use crate::device::VulkanDevice;
use crate::dpb::SlotBook;
use crate::error::VulkanError;
use crate::raw::{
    DecodeQueue, ExportPair, ExtDevice, GpuBuffer, GpuImage, GpuView, PersistentMap,
    SessionParameters, VideoSession,
};
use crate::session::geometry::SessionGeometry;
use crate::session::slots::{SubmitSlots, MAX_SUBMIT_SLOTS};

use rust_video_caps::delivery::SubmitStats;

/// Bitstream staging size in bytes: the largest access unit the session takes.
pub(crate) const BITSTREAM_BYTES: u64 = 16 << 20;

/// Decoded-picture metadata of one DPB slot.
#[derive(Clone, Copy)]
pub(crate) enum DpbMeta {
    /// H.264 reference info of the slot's picture.
    H264(vk::native::StdVideoDecodeH264ReferenceInfo),
    /// H.265 reference info of the slot's picture.
    H265(vk::native::StdVideoDecodeH265ReferenceInfo),
    /// AV1 reference info of the slot's picture.
    Av1(vk::native::StdVideoDecodeAV1ReferenceInfo),
}

/// Exportable planes behind one GPU submit.
pub(crate) struct ExportTarget<'a> {
    /// Planes receiving the decoded picture.
    pair: &'a ExportPair,
    /// First submit into the planes since creation.
    first: bool,
}

impl<'a> ExportTarget<'a> {
    /// Target over `pair`, `first` on its first submit.
    #[must_use]
    pub fn new(pair: &'a ExportPair, first: bool) -> Self {
        Self { pair, first }
    }
}

/// Owned decode session over a negotiated device.
///
/// Field order is the drop order: the submit ring idles the device
/// before dependents free, session-life maps unmap before their
/// buffers free, parameters free before the session they belong to,
/// and GPU objects free before the device clone.
pub(crate) struct Session {
    /// Submit-slot ring; drops first, idling the device.
    submit: SubmitSlots,
    /// Session-life maps over the bitstream buffers, one per slot.
    bitstream_maps: Vec<PersistentMap>,
    /// Session-life maps over the readback buffers, one per slot.
    readback_maps: Vec<PersistentMap>,
    /// Logical device.
    device: ash::Device,
    /// Decode queue functions.
    decode: DecodeQueue,
    /// Session parameters; drops before `video` below.
    params: SessionParameters,
    /// Video session.
    video: VideoSession,
    /// Validated session sizing.
    geometry: SessionGeometry,
    /// One view per DPB layer; drops before `dpb` below.
    dpb_views: Vec<GpuView>,
    /// DPB array image, one layer per slot.
    dpb: GpuImage,
    /// Tracked layout per DPB layer.
    dpb_layouts: Vec<vk::ImageLayout>,
    /// Bitstream staging buffer per submit slot.
    bitstreams: Vec<GpuBuffer>,
    /// Tight-NV12 readback buffer per submit slot.
    readbacks: Vec<GpuBuffer>,
    /// Reference-id to slot map.
    slots: SlotBook,
    /// Decode metadata per slot.
    meta: Vec<Option<DpbMeta>>,
    /// Memory properties behind exportable-plane allocation.
    props: vk::PhysicalDeviceMemoryProperties,
    /// Platform export functions, absent without the extensions.
    ext: Option<ExtDevice>,
}

/// Head barriers plus layout commits of one submit.
type HeadBarriers = (
    Vec<vk::ImageMemoryBarrier2<'static>>,
    Vec<(usize, vk::ImageLayout)>,
);

impl Session {
    /// Session over `device` for `geometry`, `profile`, and `template`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the display window escapes the coded extent,
    /// when session, image, buffer, pool, or fence creation fails,
    /// or when a staging map fails.
    pub(crate) fn open(
        device: &VulkanDevice,
        geometry: &SessionGeometry,
        profile: &vk::VideoProfileInfoKHR<'_>,
        std_version: &vk::ExtensionProperties,
        template: &mut crate::session::params::ParamsTemplate<'_>,
        depth: usize,
    ) -> Result<Self, VulkanError> {
        check_crop_window(geometry)?;
        let depth = depth.clamp(1, MAX_SUBMIT_SLOTS);
        let raw = device.raw().device();
        let properties =
            crate::raw::RawDevice::memory_properties(device.backend(), device.adapter());
        let slots = geometry.dpb_slots() as usize;
        let desc = crate::raw::VideoSessionDesc {
            profile,
            queue_family: device.queue_family(),
            format: geometry.format(),
            extent: as_extent(geometry.coded()),
            dpb_slots: geometry.dpb_slots(),
            active_refs: geometry.active_refs(),
            std_version,
        };
        let session = VideoSession::create(raw, device.backend().instance(), &properties, &desc)?;
        let create = vk::VideoSessionParametersCreateInfoKHR {
            video_session: session.session(),
            ..Default::default()
        };
        let params = match template {
            crate::session::params::ParamsTemplate::H264(codec) => {
                SessionParameters::create(&session, &create.push_next(codec))?
            }
            crate::session::params::ParamsTemplate::H265(codec) => {
                SessionParameters::create(&session, &create.push_next(codec))?
            }
            crate::session::params::ParamsTemplate::Av1(codec) => {
                SessionParameters::create(&session, &create.push_next(codec))?
            }
            crate::session::params::ParamsTemplate::H264Encode(_)
            | crate::session::params::ParamsTemplate::H265Encode(_) => {
                return Err(VulkanError::OverCaps { what: "codec op" });
            }
        };
        let image = crate::raw::GpuImageDesc {
            format: geometry.format(),
            extent: as_extent(geometry.coded()),
            layers: geometry.dpb_slots(),
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
                | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR
                | vk::ImageUsageFlags::TRANSFER_SRC,
            flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
        };
        let profiles = std::slice::from_ref(profile);
        let dpb = GpuImage::create(raw, &properties, &image, profiles)?;
        let mut dpb_views = Vec::with_capacity(slots);
        for layer in 0..geometry.dpb_slots() {
            dpb_views.push(dpb.layer_view(layer)?);
        }
        let (bitstreams, readbacks) = Self::stage_buffers(
            raw,
            &properties,
            profiles,
            nv12_bytes(as_extent(geometry.coded())),
            depth,
        )?;
        let decode = DecodeQueue::new(device.backend().instance(), raw);
        let submit = SubmitSlots::open_depth(
            raw,
            device.queue_family(),
            device.queue(),
            device.queue_lock().clone(),
            depth,
        )?;
        let (bitstream_maps, readback_maps) = Self::stage_maps(&bitstreams, &readbacks)?;
        let ext = device.ext();
        Ok(Self {
            submit,
            bitstream_maps,
            readback_maps,
            device: raw.clone(),
            decode,
            params,
            video: session,
            geometry: *geometry,
            dpb_views,
            dpb,
            dpb_layouts: vec![vk::ImageLayout::UNDEFINED; slots],
            bitstreams,
            readbacks,
            slots: SlotBook::with_slots(slots),
            meta: vec![None; slots],
            props: properties,
            ext,
        })
    }

    /// Staging bitstream plus readback buffers, `depth` pairs of `frame` bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when buffer creation fails.
    fn stage_buffers(
        raw: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        profiles: &[vk::VideoProfileInfoKHR<'_>],
        frame: u64,
        depth: usize,
    ) -> Result<(Vec<GpuBuffer>, Vec<GpuBuffer>), VulkanError> {
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let mut bitstreams = Vec::with_capacity(depth);
        let mut readbacks = Vec::with_capacity(depth);
        for _ in 0..depth {
            bitstreams.push(GpuBuffer::create(
                raw,
                properties,
                BITSTREAM_BYTES,
                vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR,
                host,
                profiles,
            )?);
            readbacks.push(GpuBuffer::create_readback(
                raw,
                properties,
                frame,
                vk::BufferUsageFlags::TRANSFER_DST,
                host,
                &[],
            )?);
        }
        Ok((bitstreams, readbacks))
    }

    /// Session-life maps over staging `bitstreams` plus `readbacks`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a mapping fails.
    fn stage_maps(
        bitstreams: &[GpuBuffer],
        readbacks: &[GpuBuffer],
    ) -> Result<(Vec<PersistentMap>, Vec<PersistentMap>), VulkanError> {
        let mut bitstream_maps = Vec::with_capacity(bitstreams.len());
        let mut readback_maps = Vec::with_capacity(readbacks.len());
        for slot in 0..bitstreams.len() {
            bitstream_maps.push(bitstreams[slot].memory().map_persistent()?);
            readback_maps.push(readbacks[slot].memory().map_persistent()?);
        }
        Ok((bitstream_maps, readback_maps))
    }

    /// Validated session sizing.
    pub(crate) fn geometry(&self) -> &SessionGeometry {
        &self.geometry
    }

    /// Session parameters for set updates.
    pub(crate) fn params_mut(&mut self) -> &mut SessionParameters {
        &mut self.params
    }

    /// Reference-id to slot map.
    pub(crate) fn slots(&self) -> &SlotBook {
        &self.slots
    }

    /// Reference-id to slot map, mutably.
    pub(crate) fn slots_mut(&mut self) -> &mut SlotBook {
        &mut self.slots
    }

    /// Decode metadata of `slot`, if the slot holds a picture.
    pub(crate) fn meta(&self, slot: usize) -> Option<DpbMeta> {
        self.meta.get(slot).copied().flatten()
    }

    /// Record decode metadata of `slot`.
    pub(crate) fn set_meta(&mut self, slot: usize, meta: Option<DpbMeta>) {
        if let Some(entry) = self.meta.get_mut(slot) {
            *entry = meta;
        }
    }

    /// Clear every slot's metadata after a DPB reset.
    pub(crate) fn clear_meta(&mut self) {
        self.meta.fill(None);
    }

    /// DPB layer view of `slot`, if the slot exists.
    pub(crate) fn dpb_view(&self, slot: usize) -> Option<vk::ImageView> {
        self.dpb_views.get(slot).map(GpuView::view)
    }

    /// Handles behind exportable-plane allocation, absent without platform export.
    pub(crate) fn export_alloc(
        &self,
    ) -> Option<(
        &ash::Device,
        &ExtDevice,
        &vk::PhysicalDeviceMemoryProperties,
    )> {
        self.ext
            .as_ref()
            .map(|ext| (&self.device, ext, &self.props))
    }

    /// Upload `bytes` to the bitstream staging map of `slot`.
    ///
    /// Zero-pads through the aligned range the decode consumes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the aligned range exceeds the staging buffer.
    pub(crate) fn upload(&self, slot: usize, bytes: &[u8]) -> Result<u64, VulkanError> {
        let len = u64::try_from(bytes.len()).map_err(|_| VulkanError::OverCaps {
            what: "access unit",
        })?;
        let total = len.next_multiple_of(self.geometry.bitstream_align());
        if total > BITSTREAM_BYTES {
            return Err(VulkanError::OverCaps {
                what: "access unit",
            });
        }
        self.bitstream_maps[slot].write_padded(bytes, total)?;
        Ok(total)
    }

    /// Queue-submit one decode without waiting, into `export` when present.
    ///
    /// Records on a free ring slot and commits the planned layouts once
    /// the submit queues; same-queue submission order plus the recorded
    /// barriers carry the layout assumptions to the device. Wait the
    /// returned pending through `wait_pending`, then read its pixels
    /// through `copy_pending_into` on the readback path.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the upload, recording, or submission fails.
    pub(crate) fn submit_async(
        &mut self,
        sub: &Submission<'_>,
        export: Option<&ExportTarget<'_>>,
    ) -> Result<PendingDecode, VulkanError> {
        let index = self.submit.acquire().inspect_err(|_| self.reset_state())?;
        let range = match self.upload(index, sub.bytes) {
            Ok(range) => range,
            Err(error) => {
                self.submit.release(index);
                return Err(error);
            }
        };
        let cmd = self.submit.cmd(index);
        let planned = match self.record(index, cmd, sub, range, export) {
            Ok(planned) => planned,
            Err(error) => {
                self.submit.release(index);
                return Err(error);
            }
        };
        if let Err(error) = self.submit.submit(index) {
            self.reset_state();
            return Err(error);
        }
        for (slot, layout) in planned {
            self.dpb_layouts[slot] = layout;
        }
        Ok(PendingDecode { slot: index })
    }

    /// True once `pending` completes, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the fence query fails.
    pub(crate) fn is_ready(&self, pending: PendingDecode) -> Result<bool, VulkanError> {
        self.submit.signaled(pending.slot)
    }

    /// Wait `pending` out, freeing its ring slot.
    ///
    /// The slot frees for reuse on return; follow with
    /// `copy_pending_into` before the next submit reuses the slot.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the fence wait fails.
    pub(crate) fn wait_pending(&mut self, pending: PendingDecode) -> Result<(), VulkanError> {
        self.submit
            .poll(pending.slot)
            .inspect_err(|_| self.reset_state())
    }

    /// Wait `pending` out up to `timeout_ns`, freeing its ring slot.
    ///
    /// `Ok(false)` reports the lapsed budget with the ring intact; the
    /// completion stays pending for a later wait.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the fence wait fails.
    pub(crate) fn wait_pending_timeout(
        &mut self,
        pending: PendingDecode,
        timeout_ns: u64,
    ) -> Result<bool, VulkanError> {
        self.submit
            .try_poll_timeout(pending.slot, timeout_ns)
            .inspect_err(|_| self.reset_state())
    }

    /// Map the waited `pending` readback into `out`.
    ///
    /// The readback holds the display window, packed tightly by the copy.
    /// Call only after `wait_pending`; an unwaited slot reads stale bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the readback map fails.
    pub(crate) fn copy_pending_into(
        &self,
        pending: PendingDecode,
        out: &mut bytes::BytesMut,
    ) -> Result<(), VulkanError> {
        let display = as_extent(self.geometry.display());
        let len = usize::try_from(nv12_bytes(display)).map_err(|_| {
            crate::error::ParseError::OutOfRange {
                syntax: "readback size",
            }
        })?;
        self.readback_maps[pending.slot].read_into(out, len)
    }

    /// Session-life readback map behind `pending` for a worker copy.
    ///
    /// Call only after the slot's fence signaled; the map lives with
    /// the session, so no release runs after the copy lands.
    ///
    /// # Errors
    ///
    /// Returns `Err` when sizing overflows or overruns the readback.
    pub(crate) fn map_pending(
        &self,
        pending: PendingDecode,
    ) -> Result<(*const u8, usize), VulkanError> {
        let display = as_extent(self.geometry.display());
        let len = usize::try_from(nv12_bytes(display)).map_err(|_| {
            crate::error::ParseError::OutOfRange {
                syntax: "readback size",
            }
        })?;
        let map = &self.readback_maps[pending.slot];
        let want = u64::try_from(len).unwrap_or(u64::MAX);
        if want > map.size() {
            return Err(VulkanError::OutOfBounds {
                len: want,
                capacity: map.size(),
            });
        }
        Ok((map.ptr().cast_const(), len))
    }

    /// Submit-core gauges snapshot.
    #[must_use]
    pub(crate) fn submit_stats(&self) -> SubmitStats {
        self.submit.stats()
    }

    /// Submits since the last take, zeroing the counter.
    pub(crate) fn take_submitted(&mut self) -> u64 {
        self.submit.take_submitted()
    }

    /// Resets since the last take, zeroing the counter.
    pub(crate) fn take_resets(&mut self) -> u64 {
        self.submit.take_resets()
    }

    /// Driver creates since the last take, zeroing the counter.
    pub(crate) fn take_creates(&mut self) -> u64 {
        self.submit.take_creates()
    }

    /// Fail the next submit once, exercising the reset path.
    pub(crate) fn inject_fault(&mut self) {
        self.submit.inject_fault();
    }

    /// Forget DPB layouts and references after a submit-core reset.
    fn reset_state(&mut self) {
        self.dpb_layouts.fill(vk::ImageLayout::UNDEFINED);
        self.slots.release_all();
        self.clear_meta();
    }

    /// Record decode commands, returning the planned layout commits.
    /// Transition-or-follow barriers over the setup plus live DPB layers.
    ///
    /// Changed layers transition and commit; steady layers chain prior
    /// submits through a same-layout memory barrier.
    fn head_barriers(&self, setup_slot: usize) -> Result<HeadBarriers, VulkanError> {
        let mut planned = Vec::new();
        let mut barriers = Vec::new();
        for (slot, layout) in self.dpb_layouts.iter().enumerate() {
            if slot != setup_slot && self.meta[slot].is_none() {
                continue;
            }
            let target = vk::ImageLayout::VIDEO_DECODE_DPB_KHR;
            let layer = narrow_u32(slot, "dpb slot")?;
            if *layout == target {
                barriers.push(follow_barrier(
                    self.dpb.image(),
                    target,
                    vk::ImageAspectFlags::COLOR,
                    layer,
                    vk::PipelineStageFlags2::VIDEO_DECODE_KHR | vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR
                        | vk::AccessFlags2::VIDEO_DECODE_READ_KHR
                        | vk::AccessFlags2::TRANSFER_READ,
                ));
            } else {
                barriers.push(layout_barrier(
                    self.dpb.image(),
                    *layout,
                    target,
                    vk::ImageAspectFlags::COLOR,
                    layer,
                    1,
                ));
                planned.push((slot, target));
            }
        }
        Ok((barriers, planned))
    }

    fn record(
        &self,
        slot: usize,
        cmd: vk::CommandBuffer,
        sub: &Submission<'_>,
        range: u64,
        export: Option<&ExportTarget<'_>>,
    ) -> Result<Vec<(usize, vk::ImageLayout)>, VulkanError> {
        self.submit.pool().begin_buffer(cmd)?;
        let recorder = self.submit.pool().recorder(cmd);
        let (barriers, mut planned) = self.head_barriers(sub.setup_slot)?;
        if !barriers.is_empty() {
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&barriers));
        }
        let begin = vk::VideoBeginCodingInfoKHR {
            video_session: self.video.session(),
            video_session_parameters: self.params.params(),
            reference_slot_count: narrow_u32(sub.begin.len(), "begin slots")?,
            p_reference_slots: if sub.begin.is_empty() {
                std::ptr::null()
            } else {
                sub.begin.as_ptr()
            },
            ..Default::default()
        };
        self.video.begin_coding(cmd, &begin);
        if sub.reset {
            let control = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::RESET);
            self.video.control_coding(cmd, &control);
        }
        let decode = vk::VideoDecodeInfoKHR {
            src_buffer: self.bitstreams[slot].buffer(),
            src_buffer_offset: 0,
            src_buffer_range: range,
            dst_picture_resource: sub.dst,
            p_setup_reference_slot: std::ptr::from_ref(sub.setup),
            reference_slot_count: narrow_u32(sub.refs.len(), "decode refs")?,
            p_reference_slots: if sub.refs.is_empty() {
                std::ptr::null()
            } else {
                sub.refs.as_ptr()
            },
            p_next: sub.picture_next,
            ..Default::default()
        };
        self.decode.decode_video(cmd, &decode);
        self.video
            .end_coding(cmd, &vk::VideoEndCodingInfoKHR::default());
        let layer = narrow_u32(sub.setup_slot, "setup slot")?;
        let to_src = layout_barrier(
            self.dpb.image(),
            vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageAspectFlags::COLOR,
            layer,
            1,
        );
        recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[to_src]));
        if let Some(target) = export {
            self.record_export(&recorder, layer, target);
        } else {
            let regions = nv12_copy_regions(as_extent(self.geometry.display()), layer);
            recorder.copy_image_to_buffer(
                self.dpb.image(),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.readbacks[slot].buffer(),
                &regions,
            );
        }
        let to_dpb = layout_barrier(
            self.dpb.image(),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
            vk::ImageAspectFlags::COLOR,
            layer,
            1,
        );
        recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[to_dpb]));
        planned.push((sub.setup_slot, vk::ImageLayout::VIDEO_DECODE_DPB_KHR));
        self.submit.pool().end_buffer(cmd)?;
        Ok(planned)
    }

    /// Record the decoded `layer` into `target`, cropping to display extent.
    fn record_export(
        &self,
        recorder: &crate::raw::Recorder<'_>,
        layer: u32,
        target: &ExportTarget<'_>,
    ) {
        let old = if target.first {
            vk::ImageLayout::UNDEFINED
        } else {
            vk::ImageLayout::TRANSFER_DST_OPTIMAL
        };
        for image in [target.pair.y_image(), target.pair.uv_image()] {
            let barrier = layout_barrier(
                image,
                old,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            );
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[barrier]));
        }
        let (width, height) = self.geometry.display();
        let luma = vk::Extent2D { width, height };
        let chroma = vk::Extent2D {
            width: width / 2,
            height: height / 2,
        };
        let y_copy = plane_copy(
            vk::ImageAspectFlags::PLANE_0,
            layer,
            vk::ImageAspectFlags::COLOR,
            0,
            luma,
        );
        recorder.copy_image(
            self.dpb.image(),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            target.pair.y_image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[y_copy],
        );
        let uv_copy = plane_copy(
            vk::ImageAspectFlags::PLANE_1,
            layer,
            vk::ImageAspectFlags::COLOR,
            0,
            chroma,
        );
        recorder.copy_image(
            self.dpb.image(),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            target.pair.uv_image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[uv_copy],
        );
    }
}

/// Checked narrowing of an index to `u32`.
pub(crate) fn narrow_u32(value: usize, syntax: &'static str) -> Result<u32, VulkanError> {
    u32::try_from(value).map_err(|_| crate::error::ParseError::OutOfRange { syntax }.into())
}

/// `Err` when the display window escapes the coded extent.
///
/// # Errors
///
/// Returns `Err` when `geometry` displays outside its coded extent.
fn check_crop_window(geometry: &SessionGeometry) -> Result<(), VulkanError> {
    let (coded_w, coded_h) = geometry.coded();
    let (display_w, display_h) = geometry.display();
    if display_w > coded_w || display_h > coded_h {
        return Err(VulkanError::OverCaps {
            what: "crop window",
        });
    }
    Ok(())
}

/// `(u32, u32)` as a Vulkan extent.
pub(crate) fn as_extent(extent: (u32, u32)) -> vk::Extent2D {
    vk::Extent2D {
        width: extent.0,
        height: extent.1,
    }
}

/// Tight NV12 bytes of `extent`.
pub(crate) fn nv12_bytes(extent: vk::Extent2D) -> u64 {
    u64::from(extent.width) * u64::from(extent.height) * 3 / 2
}

/// Picture resource over `view` covering `extent` at `layer`.
pub(crate) fn picture_resource(
    view: vk::ImageView,
    extent: vk::Extent2D,
    layer: u32,
) -> vk::VideoPictureResourceInfoKHR<'static> {
    vk::VideoPictureResourceInfoKHR {
        coded_offset: vk::Offset2D::default(),
        coded_extent: extent,
        base_array_layer: layer,
        image_view_binding: view,
        ..Default::default()
    }
}

/// Reference slot `index` over `resource`, without codec metadata.
///
/// A negative `index` binds the resource without claiming slot state.
pub(crate) fn reference_slot<'a>(
    index: i32,
    resource: &'a vk::VideoPictureResourceInfoKHR<'a>,
) -> vk::VideoReferenceSlotInfoKHR<'a> {
    vk::VideoReferenceSlotInfoKHR {
        slot_index: index,
        p_picture_resource: std::ptr::from_ref(resource),
        ..Default::default()
    }
}

/// Image barrier moving `image` from `old` to `new` inside one submit.
pub(crate) fn layout_barrier(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    aspect: vk::ImageAspectFlags,
    base_layer: u32,
    layers: u32,
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2 {
        src_stage_mask: stage_for(old),
        src_access_mask: access_for(old),
        dst_stage_mask: stage_for(new),
        dst_access_mask: access_for(new),
        old_layout: old,
        new_layout: new,
        image,
        subresource_range: vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: base_layer,
            layer_count: layers,
        },
        ..Default::default()
    }
}

/// Same-layout memory barrier chaining prior submits into this one.
///
/// Submission order alone leaves earlier writes invisible to later
/// submits on one queue. Emitted for every touched layer on every
/// submit whether or not its layout changes, so overlapped submits
/// stay ordered without CPU waits.
pub(crate) fn follow_barrier(
    image: vk::Image,
    layout: vk::ImageLayout,
    aspect: vk::ImageAspectFlags,
    base_layer: u32,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2 {
        src_stage_mask: src_stage,
        src_access_mask: src_access,
        dst_stage_mask: stage_for(layout),
        dst_access_mask: access_for(layout),
        old_layout: layout,
        new_layout: layout,
        image,
        subresource_range: vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: base_layer,
            layer_count: 1,
        },
        ..Default::default()
    }
}

/// Pipeline stage producing `layout`.
fn stage_for(layout: vk::ImageLayout) -> vk::PipelineStageFlags2 {
    match layout {
        vk::ImageLayout::VIDEO_DECODE_DST_KHR | vk::ImageLayout::VIDEO_DECODE_DPB_KHR => {
            vk::PipelineStageFlags2::VIDEO_DECODE_KHR
        }
        vk::ImageLayout::VIDEO_ENCODE_SRC_KHR | vk::ImageLayout::VIDEO_ENCODE_DPB_KHR => {
            vk::PipelineStageFlags2::VIDEO_ENCODE_KHR
        }
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL | vk::ImageLayout::TRANSFER_DST_OPTIMAL => {
            vk::PipelineStageFlags2::TRANSFER
        }
        _ => vk::PipelineStageFlags2::TOP_OF_PIPE,
    }
}

/// Access mask producing `layout`.
fn access_for(layout: vk::ImageLayout) -> vk::AccessFlags2 {
    match layout {
        vk::ImageLayout::VIDEO_DECODE_DST_KHR => vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR,
        vk::ImageLayout::VIDEO_DECODE_DPB_KHR => {
            vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR | vk::AccessFlags2::VIDEO_DECODE_READ_KHR
        }
        vk::ImageLayout::VIDEO_ENCODE_SRC_KHR => vk::AccessFlags2::VIDEO_ENCODE_READ_KHR,
        vk::ImageLayout::VIDEO_ENCODE_DPB_KHR => {
            vk::AccessFlags2::VIDEO_ENCODE_WRITE_KHR | vk::AccessFlags2::VIDEO_ENCODE_READ_KHR
        }
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => vk::AccessFlags2::TRANSFER_READ,
        vk::ImageLayout::TRANSFER_DST_OPTIMAL => vk::AccessFlags2::TRANSFER_WRITE,
        _ => vk::AccessFlags2::empty(),
    }
}

/// Image copy of one plane at `src_layer` into `dst_layer` over `extent`.
pub(crate) fn plane_copy(
    src_aspect: vk::ImageAspectFlags,
    src_layer: u32,
    dst_aspect: vk::ImageAspectFlags,
    dst_layer: u32,
    extent: vk::Extent2D,
) -> vk::ImageCopy {
    vk::ImageCopy {
        src_subresource: vk::ImageSubresourceLayers {
            aspect_mask: src_aspect,
            mip_level: 0,
            base_array_layer: src_layer,
            layer_count: 1,
        },
        src_offset: vk::Offset3D::default(),
        dst_subresource: vk::ImageSubresourceLayers {
            aspect_mask: dst_aspect,
            mip_level: 0,
            base_array_layer: dst_layer,
            layer_count: 1,
        },
        dst_offset: vk::Offset3D::default(),
        extent: vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        },
    }
}

/// Buffer copy regions packing NV12 `extent` tightly from `layer`.
pub(crate) fn nv12_copy_regions(extent: vk::Extent2D, layer: u32) -> [vk::BufferImageCopy; 2] {
    let luma = vk::BufferImageCopy {
        buffer_offset: 0,
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::PLANE_0,
            mip_level: 0,
            base_array_layer: layer,
            layer_count: 1,
        },
        image_offset: vk::Offset3D::default(),
        image_extent: vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        },
    };
    let chroma = vk::BufferImageCopy {
        buffer_offset: u64::from(extent.width) * u64::from(extent.height),
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::PLANE_1,
            mip_level: 0,
            base_array_layer: layer,
            layer_count: 1,
        },
        image_offset: vk::Offset3D::default(),
        image_extent: vk::Extent3D {
            width: extent.width / 2,
            height: extent.height / 2,
            depth: 1,
        },
    };
    [luma, chroma]
}

/// Submitted but uncollected decode.
#[derive(Clone, Copy)]
pub(crate) struct PendingDecode {
    /// Ring slot behind the submit.
    slot: usize,
}

/// One decoded picture's submission bundle.
///
/// All references borrow decoder-local chains that outlive the submit call.
pub(crate) struct Submission<'a> {
    /// Bitstream bytes of the picture.
    pub bytes: &'a [u8],
    /// Claimed slot the picture decodes into.
    pub setup_slot: usize,
    /// Issue a session reset before decoding.
    pub reset: bool,
    /// Active DPB slots at begin time, with codec metadata.
    pub begin: &'a [vk::VideoReferenceSlotInfoKHR<'a>],
    /// Active reference slots for the decode, with codec metadata.
    pub refs: &'a [vk::VideoReferenceSlotInfoKHR<'a>],
    /// Setup slot with codec metadata.
    pub setup: &'a vk::VideoReferenceSlotInfoKHR<'a>,
    /// Decode target resource: the setup slot's layer.
    pub dst: vk::VideoPictureResourceInfoKHR<'a>,
    /// Codec picture-info chain for the decode's `p_next`.
    pub picture_next: *const std::ffi::c_void,
}

#[cfg(test)]
mod tests {
    #[test]
    fn nv12_size_counts_three_planes_over_two() {
        let extent = ash::vk::Extent2D {
            width: 320,
            height: 240,
        };
        assert_eq!(super::nv12_bytes(extent), 320 * 240 * 3 / 2);
    }

    #[test]
    fn picture_resource_pins_layer_and_extent() {
        let view = ash::vk::ImageView::null();
        let extent = ash::vk::Extent2D {
            width: 64,
            height: 32,
        };
        let resource = super::picture_resource(view, extent, 3);
        assert!(resource.coded_extent == extent);
        assert_eq!(resource.base_array_layer, 3);
        assert_eq!(resource.image_view_binding, view);
        assert!(resource.coded_offset == ash::vk::Offset2D { x: 0, y: 0 });
    }

    #[test]
    fn barriers_carry_decode_and_transfer_stages() {
        let image = ash::vk::Image::null();
        let to_dst = super::layout_barrier(
            image,
            ash::vk::ImageLayout::UNDEFINED,
            ash::vk::ImageLayout::VIDEO_DECODE_DST_KHR,
            ash::vk::ImageAspectFlags::COLOR,
            1,
            1,
        );
        assert!(to_dst.dst_stage_mask == ash::vk::PipelineStageFlags2::VIDEO_DECODE_KHR);
        assert!(to_dst.dst_access_mask == ash::vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR);
        assert_eq!(to_dst.subresource_range.base_array_layer, 1);
        let to_read = super::layout_barrier(
            image,
            ash::vk::ImageLayout::VIDEO_DECODE_DST_KHR,
            ash::vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            ash::vk::ImageAspectFlags::COLOR,
            1,
            1,
        );
        assert!(to_read.src_stage_mask == ash::vk::PipelineStageFlags2::VIDEO_DECODE_KHR);
        assert!(to_read.dst_stage_mask == ash::vk::PipelineStageFlags2::TRANSFER);
        assert!(to_read.dst_access_mask == ash::vk::AccessFlags2::TRANSFER_READ);
        let to_dpb = super::layout_barrier(
            image,
            ash::vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            ash::vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
            ash::vk::ImageAspectFlags::COLOR,
            1,
            1,
        );
        assert!(
            to_dpb.dst_access_mask
                == ash::vk::AccessFlags2::VIDEO_DECODE_WRITE_KHR
                    | ash::vk::AccessFlags2::VIDEO_DECODE_READ_KHR
        );
    }

    #[test]
    fn plane_copy_pins_aspects_layers_and_extent() {
        let extent = ash::vk::Extent2D {
            width: 64,
            height: 32,
        };
        let copy = super::plane_copy(
            ash::vk::ImageAspectFlags::PLANE_0,
            2,
            ash::vk::ImageAspectFlags::COLOR,
            0,
            extent,
        );
        assert!(copy.src_subresource.aspect_mask == ash::vk::ImageAspectFlags::PLANE_0);
        assert_eq!(copy.src_subresource.base_array_layer, 2);
        assert!(copy.dst_subresource.aspect_mask == ash::vk::ImageAspectFlags::COLOR);
        assert_eq!(copy.dst_subresource.base_array_layer, 0);
        assert_eq!(copy.extent.width, 64);
        assert_eq!(copy.extent.height, 32);
        assert_eq!(copy.extent.depth, 1);
    }

    #[test]
    fn copy_regions_pack_planes_tightly() {
        let extent = ash::vk::Extent2D {
            width: 64,
            height: 32,
        };
        let [luma, chroma] = super::nv12_copy_regions(extent, 2);
        assert_eq!(luma.buffer_offset, 0);
        assert_eq!(luma.buffer_row_length, 0);
        assert!(luma.image_subresource.aspect_mask == ash::vk::ImageAspectFlags::PLANE_0);
        assert_eq!(luma.image_subresource.base_array_layer, 2);
        assert_eq!(luma.image_extent.width, 64);
        assert_eq!(luma.image_extent.height, 32);
        assert_eq!(chroma.buffer_offset, 64 * 32);
        assert!(chroma.image_subresource.aspect_mask == ash::vk::ImageAspectFlags::PLANE_1);
        assert_eq!(chroma.image_extent.width, 32);
        assert_eq!(chroma.image_extent.height, 16);
    }
}
