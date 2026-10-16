//! Decode session core: owned GPU resources shared by both codecs.
//!
//! [`Session`] owns the video session, its parameters, the DPB array image,
//! the bitstream and readback buffers, and the per-slot decode metadata.
//! Per-frame command assembly is pure data over these resources; only the
//! final record-and-submit touches the device.

use ash::{khr, vk};

use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::parse::refs::SlotBook;
use crate::raw::{
    CommandPool, DecodeQueue, ExportPair, GpuBuffer, GpuImage, GpuView, SessionParameters,
    VideoSession, VkFence,
};
use crate::session::geometry::SessionGeometry;

/// Bitstream staging size in bytes: the largest access unit the session takes.
pub(crate) const BITSTREAM_BYTES: u64 = 16 << 20;

/// Decoded-picture metadata of one DPB slot.
#[derive(Clone, Copy)]
pub(crate) enum DpbMeta {
    /// H.264 reference info of the slot's picture.
    H264(vk::native::StdVideoDecodeH264ReferenceInfo),
    /// H.265 reference info of the slot's picture.
    H265(vk::native::StdVideoDecodeH265ReferenceInfo),
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
/// Field order is the drop order: parameters free before the session they
/// belong to, and GPU objects free before the pool and device clones.
pub(crate) struct Session {
    /// Logical device.
    device: ash::Device,
    /// Video queue.
    queue: vk::Queue,
    /// Queue submission mutex, shared with the device.
    queue_lock: std::sync::Arc<std::sync::Mutex<()>>,
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
    /// Bitstream staging buffer.
    bitstream: GpuBuffer,
    /// Tight-NV12 readback buffer.
    readback: GpuBuffer,
    /// Command pool behind the submit slot.
    pool: CommandPool,
    /// Command buffer, allocated once and reset per submit.
    cmd: vk::CommandBuffer,
    /// Submission fence, reset per submit.
    fence: VkFence,
    /// True after a submit failure; the fence state is unknown.
    wedged: bool,
    /// Reference-id to slot map.
    slots: SlotBook,
    /// Decode metadata per slot.
    meta: Vec<Option<DpbMeta>>,
    /// Memory properties behind exportable-plane allocation.
    props: vk::PhysicalDeviceMemoryProperties,
    /// DMA-BUF export functions, absent without the extensions.
    ext: Option<khr::external_memory_fd::Device>,
}

impl Session {
    /// Session over `device` for `geometry`, `profile`, and `template`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when session, image, buffer, pool, or fence creation fails.
    pub(crate) fn open(
        device: &VulkanDevice,
        geometry: &SessionGeometry,
        profile: &vk::VideoProfileInfoKHR<'_>,
        std_version: &vk::ExtensionProperties,
        template: &mut crate::session::params::ParamsTemplate,
    ) -> Result<Self, VulkanError> {
        let raw = device.raw().device();
        let properties =
            crate::raw::RawDevice::memory_properties(device.backend(), device.adapter());
        let slots = geometry.dpb_slots() as usize;
        let desc = crate::raw::VideoSessionDesc {
            profile,
            queue_family: device.queue_family(),
            format: geometry.format(),
            extent: coded_extent(geometry.coded()),
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
            crate::session::params::ParamsTemplate::H264Encode(_)
            | crate::session::params::ParamsTemplate::H265Encode(_) => {
                return Err(VulkanError::OverCaps { what: "codec op" });
            }
        };
        let image = crate::raw::GpuImageDesc {
            format: geometry.format(),
            extent: coded_extent(geometry.coded()),
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
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let bitstream = GpuBuffer::create(
            raw,
            &properties,
            BITSTREAM_BYTES,
            vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR,
            host,
            profiles,
        )?;
        let readback = GpuBuffer::create(
            raw,
            &properties,
            nv12_bytes(coded_extent(geometry.coded())),
            vk::BufferUsageFlags::TRANSFER_DST,
            host,
            &[],
        )?;
        let pool = CommandPool::create(raw, device.queue_family())?;
        let decode = DecodeQueue::new(device.backend().instance(), raw);
        let cmd = pool.allocate()?;
        let fence = VkFence::new(raw, false)?;
        let ext = device.external_fd();
        Ok(Self {
            device: raw.clone(),
            queue: device.queue(),
            queue_lock: device.queue_lock().clone(),
            decode,
            params,
            video: session,
            geometry: *geometry,
            dpb_views,
            dpb,
            dpb_layouts: vec![vk::ImageLayout::UNDEFINED; slots],
            bitstream,
            readback,
            pool,
            cmd,
            fence,
            wedged: false,
            slots: SlotBook::with_slots(slots),
            meta: vec![None; slots],
            props: properties,
            ext,
        })
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

    /// Handles behind exportable-plane allocation, absent without fd export.
    pub(crate) fn export_alloc(
        &self,
    ) -> Option<(
        &ash::Device,
        &khr::external_memory_fd::Device,
        &vk::PhysicalDeviceMemoryProperties,
    )> {
        self.ext
            .as_ref()
            .map(|ext| (&self.device, ext, &self.props))
    }

    /// Upload `bytes` to the bitstream staging buffer.
    ///
    /// Zero-pads through the aligned range the decode consumes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the aligned range exceeds the staging buffer.
    pub(crate) fn upload(&self, bytes: &[u8]) -> Result<u64, VulkanError> {
        let len = u64::try_from(bytes.len()).map_err(|_| VulkanError::OverCaps {
            what: "access unit",
        })?;
        let total = len.next_multiple_of(self.geometry.bitstream_align());
        if total > BITSTREAM_BYTES {
            return Err(VulkanError::OverCaps {
                what: "access unit",
            });
        }
        self.bitstream.memory().fill_padded(bytes, total)?;
        Ok(total)
    }

    /// Decode one submission into `out`, reusing its allocation.
    ///
    /// # Errors
    ///
    /// Returns `Err` when recording, submission, or the fence wait fails.
    pub(crate) fn submit_into(
        &mut self,
        sub: &Submission<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), VulkanError> {
        let range = self.upload(sub.bytes)?;
        self.pool.reset_buffer(self.cmd)?;
        let planned = self.record(self.cmd, sub, range, None)?;
        self.dispatch(self.cmd, planned, out)
    }

    /// Decode one submission into `target`, leaving pixels on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` when recording, submission, or the fence wait fails.
    pub(crate) fn submit_gpu(
        &mut self,
        sub: &Submission<'_>,
        target: &ExportTarget<'_>,
    ) -> Result<(), VulkanError> {
        let range = self.upload(sub.bytes)?;
        self.pool.reset_buffer(self.cmd)?;
        let planned = self.record(self.cmd, sub, range, Some(target))?;
        self.dispatch_gpu(self.cmd, planned)
    }

    /// Record decode commands, returning the planned layout commits.
    fn record(
        &self,
        cmd: vk::CommandBuffer,
        sub: &Submission<'_>,
        range: u64,
        export: Option<&ExportTarget<'_>>,
    ) -> Result<Vec<(usize, vk::ImageLayout)>, VulkanError> {
        self.pool.begin_buffer(cmd)?;
        let recorder = self.pool.recorder(cmd);
        let mut planned = Vec::new();
        let mut barriers = Vec::new();
        for (slot, layout) in self.dpb_layouts.iter().enumerate() {
            if slot != sub.setup_slot && self.meta[slot].is_none() {
                continue;
            }
            let target = vk::ImageLayout::VIDEO_DECODE_DPB_KHR;
            if *layout != target {
                barriers.push(layout_barrier(
                    self.dpb.image(),
                    *layout,
                    target,
                    vk::ImageAspectFlags::COLOR,
                    narrow_u32(slot, "dpb slot")?,
                    1,
                ));
                planned.push((slot, target));
            }
        }
        if !barriers.is_empty() {
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&barriers));
        }
        let begin = vk::VideoBeginCodingInfoKHR {
            video_session: self.video.session(),
            video_session_parameters: self.params.params(),
            reference_slot_count: narrow_u32(sub.begin.len(), "begin slots")?,
            p_reference_slots: sub.begin.as_ptr(),
            ..Default::default()
        };
        self.video.begin_coding(cmd, &begin);
        if sub.reset {
            let control = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::RESET);
            self.video.control_coding(cmd, &control);
        }
        let decode = vk::VideoDecodeInfoKHR {
            src_buffer: self.bitstream.buffer(),
            src_buffer_offset: 0,
            src_buffer_range: range,
            dst_picture_resource: sub.dst,
            p_setup_reference_slot: std::ptr::from_ref(sub.setup),
            reference_slot_count: narrow_u32(sub.refs.len(), "decode refs")?,
            p_reference_slots: sub.refs.as_ptr(),
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
            let regions = nv12_copy_regions(coded_extent(self.geometry.coded()), layer);
            recorder.copy_image_to_buffer(
                self.dpb.image(),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.readback.buffer(),
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
        self.pool.end_buffer(cmd)?;
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

    /// Submit recorded commands and wait for the fence.
    ///
    /// A failure wedges the session: the fence may still be submitted,
    /// so later submits fail without touching the queue.
    ///
    /// # Errors
    ///
    /// Returns `Err` when submission or the fence wait fails, or when a
    /// past submit wedged the session.
    fn submit_wait(&mut self, cmd: vk::CommandBuffer) -> Result<(), VulkanError> {
        if self.wedged {
            return Err(VulkanError::Timeout);
        }
        if let Err(error) = crate::raw::submit_and_wait(
            &self.device,
            &self.queue_lock,
            self.queue,
            cmd,
            &self.fence,
            crate::raw::SUBMIT_TIMEOUT_NS,
        ) {
            self.wedged = true;
            return Err(error);
        }
        Ok(())
    }

    /// Submit recorded commands, wait, and map the readback buffer.
    fn dispatch(
        &mut self,
        cmd: vk::CommandBuffer,
        planned: Vec<(usize, vk::ImageLayout)>,
        out: &mut Vec<u8>,
    ) -> Result<(), VulkanError> {
        self.submit_wait(cmd)?;
        for (slot, layout) in planned {
            self.dpb_layouts[slot] = layout;
        }
        let len =
            usize::try_from(nv12_bytes(coded_extent(self.geometry.coded()))).map_err(|_| {
                crate::error::ParseError::OutOfRange {
                    syntax: "readback size",
                }
            })?;
        self.readback.memory().read_to_vec(out, len)?;
        Ok(())
    }

    /// Submit recorded commands and wait, leaving pixels on the GPU.
    fn dispatch_gpu(
        &mut self,
        cmd: vk::CommandBuffer,
        planned: Vec<(usize, vk::ImageLayout)>,
    ) -> Result<(), VulkanError> {
        self.submit_wait(cmd)?;
        for (slot, layout) in planned {
            self.dpb_layouts[slot] = layout;
        }
        Ok(())
    }
}

/// Checked narrowing of an index to `u32`.
fn narrow_u32(value: usize, syntax: &'static str) -> Result<u32, VulkanError> {
    u32::try_from(value).map_err(|_| crate::error::ParseError::OutOfRange { syntax }.into())
}

/// Coded extent as a Vulkan extent.
fn coded_extent(coded: (u32, u32)) -> vk::Extent2D {
    vk::Extent2D {
        width: coded.0,
        height: coded.1,
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

/// Crop tight coded NV12 `bytes` down to `display`, appended to `out`.
///
/// # Errors
///
/// Returns `Err` when the display window escapes the coded extent or
/// `bytes` is shorter than the coded frame.
pub(crate) fn crop_nv12_extend(
    coded: vk::Extent2D,
    display: vk::Extent2D,
    bytes: &[u8],
    out: &mut bytes::BytesMut,
) -> Result<(), VulkanError> {
    use bytes::BufMut;
    let window = crop_window(coded, display, bytes)?;
    if coded == display {
        out.extend_from_slice(&bytes[..window.coded_len]);
        return Ok(());
    }
    out.reserve(window.display_len);
    copy_crop_rows(&window, bytes, &mut |row| out.put_slice(row));
    Ok(())
}

/// Validated crop geometry of one frame.
struct CropWindow {
    /// Coded luma row width in bytes.
    coded_w: usize,
    /// Display width in bytes.
    display_w: usize,
    /// Display height in rows.
    display_h: usize,
    /// Coded chroma plane base in bytes.
    chroma_base: usize,
    /// Coded frame length in bytes.
    coded_len: usize,
    /// Display frame length in bytes.
    display_len: usize,
}

/// Validated crop geometry, or the bound it breaks.
///
/// # Errors
///
/// Returns `Err` when the display window escapes the coded extent or
/// `bytes` is shorter than the coded frame.
fn crop_window(
    coded: vk::Extent2D,
    display: vk::Extent2D,
    bytes: &[u8],
) -> Result<CropWindow, VulkanError> {
    let coded_w = coded.width as usize;
    let coded_h = coded.height as usize;
    let display_w = display.width as usize;
    let display_h = display.height as usize;
    if display_w > coded_w || display_h > coded_h {
        return Err(VulkanError::OverCaps {
            what: "crop window",
        });
    }
    let coded_len = coded_w * coded_h * 3 / 2;
    if bytes.len() < coded_len {
        return Err(VulkanError::ShortReadback {
            expected: coded_len,
            found: bytes.len(),
        });
    }
    Ok(CropWindow {
        coded_w,
        display_w,
        display_h,
        chroma_base: coded_w * coded_h,
        coded_len,
        display_len: display_w * display_h * 3 / 2,
    })
}

/// Visible luma rows then chroma rows of `bytes` into `push`.
fn copy_crop_rows(window: &CropWindow, bytes: &[u8], push: &mut impl FnMut(&[u8])) {
    for row in 0..window.display_h {
        let start = row * window.coded_w;
        push(&bytes[start..start + window.display_w]);
    }
    for row in 0..window.display_h / 2 {
        let start = window.chroma_base + row * window.coded_w;
        push(&bytes[start..start + window.display_w]);
    }
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
        assert!(resource.image_view_binding == view);
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

    #[test]
    fn crop_copies_visible_rows_only() {
        let coded = ash::vk::Extent2D {
            width: 4,
            height: 4,
        };
        let display = ash::vk::Extent2D {
            width: 2,
            height: 2,
        };
        let bytes: Vec<u8> = (0..24).collect();
        let mut pooled = bytes::BytesMut::new();
        super::crop_nv12_extend(coded, display, &bytes, &mut pooled).expect("in bounds");
        assert_eq!(pooled.freeze().as_ref(), &[0, 1, 4, 5, 16, 17]);
    }

    #[test]
    fn crop_without_window_returns_input() {
        let extent = ash::vk::Extent2D {
            width: 4,
            height: 4,
        };
        let bytes: Vec<u8> = (0..24).collect();
        let mut pooled = bytes::BytesMut::new();
        super::crop_nv12_extend(extent, extent, &bytes, &mut pooled).expect("in bounds");
        assert_eq!(pooled.freeze().as_ref(), bytes.as_slice());
    }

    #[test]
    fn crop_rejects_window_escape_and_short_bytes() {
        let coded = ash::vk::Extent2D {
            width: 4,
            height: 4,
        };
        let wide = ash::vk::Extent2D {
            width: 8,
            height: 4,
        };
        let bytes: Vec<u8> = (0..24).collect();
        let mut pooled = bytes::BytesMut::new();
        assert!(matches!(
            super::crop_nv12_extend(coded, wide, &bytes, &mut pooled),
            Err(super::VulkanError::OverCaps { .. })
        ));
        assert!(matches!(
            super::crop_nv12_extend(coded, coded, &bytes[..23], &mut pooled),
            Err(super::VulkanError::ShortReadback { .. })
        ));
    }
}
