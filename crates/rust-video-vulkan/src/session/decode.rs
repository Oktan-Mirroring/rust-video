//! Decode session core: owned GPU resources shared by both codecs.
//!
//! [`Session`] owns the video session, its parameters, the DPB array image,
//! the bitstream and readback buffers, and the per-slot decode metadata.
//! Per-frame command assembly is pure data over these resources; only the
//! final record-and-submit touches the device.

use ash::vk;

use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::inflight::Fence;
use crate::parse::refs::SlotBook;
use crate::raw::{
    CommandPool, DecodeQueue, GpuBuffer, GpuImage, GpuView, SessionParameters, VideoSession,
    VkFence,
};
use crate::session::geometry::SessionGeometry;

/// Bitstream staging size in bytes: the largest access unit the session takes.
pub(crate) const BITSTREAM_BYTES: u64 = 16 << 20;

/// Fence wait per submission in nanoseconds.
const SUBMIT_TIMEOUT_NS: u64 = 5_000_000_000;

/// Decoded-picture metadata of one DPB slot.
#[derive(Clone, Copy)]
pub(crate) enum DpbMeta {
    /// H.264 reference info of the slot's picture.
    H264(vk::native::StdVideoDecodeH264ReferenceInfo),
    /// H.265 reference info of the slot's picture.
    H265(vk::native::StdVideoDecodeH265ReferenceInfo),
}

/// Owned decode session over a negotiated device.
pub(crate) struct Session {
    /// Logical device.
    device: ash::Device,
    /// Video queue.
    queue: vk::Queue,
    /// Decode queue functions.
    decode: DecodeQueue,
    /// Video session.
    video: VideoSession,
    /// Session parameters.
    params: SessionParameters,
    /// Validated session sizing.
    geometry: SessionGeometry,
    /// DPB array image, one layer per slot.
    dpb: GpuImage,
    /// One view per DPB layer.
    dpb_views: Vec<GpuView>,
    /// Tracked layout per DPB layer.
    dpb_layouts: Vec<vk::ImageLayout>,
    /// Bitstream staging buffer.
    bitstream: GpuBuffer,
    /// Tight-NV12 readback buffer.
    readback: GpuBuffer,
    /// One-shot command pool.
    pool: CommandPool,
    /// Submission fence, reset per frame.
    fence: VkFence,
    /// Reference-id to slot map.
    slots: SlotBook,
    /// Decode metadata per slot.
    meta: Vec<Option<DpbMeta>>,
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
        let dpb = GpuImage::create(raw, &properties, &image)?;
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
        )?;
        let readback = GpuBuffer::create(
            raw,
            &properties,
            nv12_bytes(coded_extent(geometry.coded())),
            vk::BufferUsageFlags::TRANSFER_DST,
            host,
        )?;
        let pool = CommandPool::create(raw, device.queue_family())?;
        let fence = VkFence::new(raw, false)?;
        let decode = DecodeQueue::new(device.backend().instance(), raw);
        Ok(Self {
            device: raw.clone(),
            queue: device.queue(),
            decode,
            video: session,
            params,
            geometry: *geometry,
            dpb,
            dpb_views,
            dpb_layouts: vec![vk::ImageLayout::UNDEFINED; slots],
            bitstream,
            readback,
            pool,
            fence,
            slots: SlotBook::with_slots(slots),
            meta: vec![None; slots],
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

    /// Upload `bytes` to the bitstream staging buffer.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `bytes` exceeds the staging buffer.
    pub(crate) fn upload(&self, bytes: &[u8]) -> Result<(), VulkanError> {
        let len = u64::try_from(bytes.len()).map_err(|_| VulkanError::OverCaps {
            what: "access unit",
        })?;
        if len > BITSTREAM_BYTES {
            return Err(VulkanError::OverCaps {
                what: "access unit",
            });
        }
        self.bitstream.memory().fill(bytes)?;
        if std::env::var("VKPROBE_ZEROPAD").is_ok() {
            let pad = vec![0u8; 65536];
            let _ = self.bitstream.memory().fill_offset(bytes.len(), &pad);
        }
        if std::env::var("VKPROBE_FFPAD").is_ok() {
            let pad = vec![0xFFu8; 65536];
            let _ = self.bitstream.memory().fill_offset(bytes.len(), &pad);
        }
        Ok(())
    }

    /// Decode one submission, returning tight coded-extent NV12 bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when recording, submission, or the fence wait fails.
    pub(crate) fn submit(&mut self, sub: &Submission<'_>) -> Result<Vec<u8>, VulkanError> {
        self.upload(sub.bytes)?;
        let cmd = self.pool.allocate()?;
        let recorded = self.record(cmd, sub);
        let result = recorded.and_then(|planned| self.dispatch(cmd, planned));
        self.pool.free(cmd);
        result
    }

    /// Record decode commands, returning the planned layout commits.
    fn record(
        &self,
        cmd: vk::CommandBuffer,
        sub: &Submission<'_>,
    ) -> Result<Vec<(usize, vk::ImageLayout)>, VulkanError> {
        self.pool.begin_buffer(cmd)?;
        let recorder = self.pool.recorder(cmd);
        let mut planned = Vec::new();
        let mut barriers = Vec::new();
        for (slot, layout) in self.dpb_layouts.iter().enumerate() {
            let target = if slot == sub.setup_slot {
                vk::ImageLayout::VIDEO_DECODE_DST_KHR
            } else if self.meta[slot].is_some() {
                vk::ImageLayout::VIDEO_DECODE_DPB_KHR
            } else {
                continue;
            };
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
            src_buffer_range: u64::try_from(sub.bytes.len()).map_err(|_| {
                crate::error::ParseError::OutOfRange {
                    syntax: "bitstream range",
                }
            })?,
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
        for (old, new) in [
            (
                vk::ImageLayout::VIDEO_DECODE_DST_KHR,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            ),
            (
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
            ),
        ] {
            let barrier = layout_barrier(
                self.dpb.image(),
                old,
                new,
                vk::ImageAspectFlags::COLOR,
                layer,
                1,
            );
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[barrier]));
            if new == vk::ImageLayout::TRANSFER_SRC_OPTIMAL {
                let regions = nv12_copy_regions(coded_extent(self.geometry.coded()), layer);
                recorder.copy_image_to_buffer(
                    self.dpb.image(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    self.readback.buffer(),
                    &regions,
                );
            }
        }
        planned.push((sub.setup_slot, vk::ImageLayout::VIDEO_DECODE_DPB_KHR));
        self.pool.end_buffer(cmd)?;
        Ok(planned)
    }

    /// Submit recorded commands, wait, and map the readback buffer.
    fn dispatch(
        &mut self,
        cmd: vk::CommandBuffer,
        planned: Vec<(usize, vk::ImageLayout)>,
    ) -> Result<Vec<u8>, VulkanError> {
        let buffers = [cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&buffers);
        crate::raw::submit_to_queue(&self.device, self.queue, &[submit], self.fence.fence())?;
        if !self.fence.wait(SUBMIT_TIMEOUT_NS)? {
            return Err(VulkanError::Timeout);
        }
        self.fence.reset()?;
        for (slot, layout) in planned {
            self.dpb_layouts[slot] = layout;
        }
        let len =
            usize::try_from(nv12_bytes(coded_extent(self.geometry.coded()))).map_err(|_| {
                crate::error::ParseError::OutOfRange {
                    syntax: "readback size",
                }
            })?;
        let mut bytes = vec![0_u8; len];
        self.readback.memory().read_into(&mut bytes)?;
        Ok(bytes)
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
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => vk::PipelineStageFlags2::TRANSFER,
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
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => vk::AccessFlags2::TRANSFER_READ,
        _ => vk::AccessFlags2::empty(),
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

/// Crop tight coded NV12 `bytes` down to `display`.
///
/// # Panics
///
/// Panics when `bytes` is shorter than the coded frame.
pub(crate) fn crop_nv12(coded: vk::Extent2D, display: vk::Extent2D, bytes: &[u8]) -> Vec<u8> {
    if coded == display {
        return bytes.to_vec();
    }
    let coded_w = coded.width as usize;
    let display_w = display.width as usize;
    let display_h = display.height as usize;
    let mut out = Vec::with_capacity(display_w * display_h * 3 / 2);
    for row in 0..display_h {
        let start = row * coded_w;
        out.extend_from_slice(&bytes[start..start + display_w]);
    }
    let chroma_base = coded_w * coded.height as usize;
    for row in 0..display_h / 2 {
        let start = chroma_base + row * coded_w;
        out.extend_from_slice(&bytes[start..start + display_w]);
    }
    out
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
        let cropped = super::crop_nv12(coded, display, &bytes);
        assert_eq!(cropped, vec![0, 1, 4, 5, 16, 17]);
    }

    #[test]
    fn crop_without_window_returns_input() {
        let extent = ash::vk::Extent2D {
            width: 4,
            height: 4,
        };
        let bytes: Vec<u8> = (0..24).collect();
        assert_eq!(super::crop_nv12(extent, extent, &bytes), bytes);
    }
}
