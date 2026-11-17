//! D3D12 video decode: picture arguments plus GPU submission.
//!
//! H.264, HEVC, and AV1 decode through their DPBs.
//!
//! <https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createsharedhandle>
//! <https://docs.rs/windows/0.62.2/windows/Win32/Graphics/Direct3D12/>

// FFI boundary: video-decode COM calls live here.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::time::Duration;

use crate::dxva::{
    PicParamsAv1, PicParamsH264, PicParamsHevc, QmatrixH264, QmatrixHevc, SliceShortH264,
    SliceShortHevc, TileAv1,
};
use crate::error::{BrokenReferenceReason, D3d12Error, UnsupportedReason};
use crate::h264::Crop;
use crate::submit::{CommandSlot, DeviceResources, Fence, Nv12Layout, Ticket};
use crate::texture::SurfaceWait;

use windows::core::Interface;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandQueue, ID3D12Device, ID3D12DeviceChild, ID3D12GraphicsCommandList, ID3D12Resource,
    D3D12_BOX, D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE,
    D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_READBACK, D3D12_HEAP_TYPE_UPLOAD,
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATE_VIDEO_DECODE_READ,
    D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE,
};
use windows::Win32::Media::MediaFoundation::{
    ID3D12VideoDecodeCommandList, ID3D12VideoDecoder, ID3D12VideoDecoderHeap,
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
    D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL, D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM,
    D3D12_VIDEO_DECODE_CONVERSION_ARGUMENTS, D3D12_VIDEO_DECODE_FRAME_ARGUMENT,
    D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS, D3D12_VIDEO_DECODE_OUTPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_DECODE_REFERENCE_FRAMES,
};

/// Bitstream upload capacity in bytes.
const BITSTREAM_CAP: u64 = 8 << 20;

/// Bitstream upload buffers behind pipelined submits.
const BITSTREAM_SLOTS: usize = 4;

/// Output texture pool behind the DPB slots.
const POOL: usize = 16;

/// H.264 picture arguments for one `DecodeFrame` call.
#[derive(Debug)]
pub(crate) struct H264Submission {
    /// Picture parameters.
    pub(crate) params: PicParamsH264,
    /// Inverse quantization matrices.
    pub(crate) qmatrix: QmatrixH264,
    /// Slice control entries.
    pub(crate) slices: Vec<SliceShortH264>,
    /// Start-coded slices padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output texture slot.
    pub(crate) slot: u8,
}

/// Span table plus the padded bitstream over NAL units.
type CodedBitstream = (Vec<(u32, u32)>, Vec<u8>);

/// Span table plus the padded start-coded bitstream over `nals`.
pub(crate) fn start_coded_bitstream(nals: &[&[u8]]) -> Result<CodedBitstream, D3d12Error> {
    let mut bitstream =
        Vec::with_capacity(nals.iter().map(|nal| nal.len() + 3).sum::<usize>() + 128);
    let mut spans = Vec::with_capacity(nals.len());
    for nal in nals {
        let location =
            u32::try_from(bitstream.len()).map_err(|_| D3d12Error::BitstreamTooLarge {
                len: bitstream.len(),
                capacity: u64::from(u32::MAX),
            })?;
        let bytes = nal
            .len()
            .checked_add(3)
            .and_then(|len| u32::try_from(len).ok())
            .ok_or(D3d12Error::BitstreamTooLarge {
                len: nal.len(),
                capacity: u64::from(u32::MAX),
            })?;
        bitstream.extend_from_slice(&[0, 0, 1]);
        bitstream.extend_from_slice(nal);
        spans.push((location, bytes));
    }
    if spans.is_empty() {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::PictureWithoutSlices,
        ));
    }
    let padding = 128 - (bitstream.len() % 128);
    bitstream.resize(bitstream.len() + padding, 0);
    if let Some(last) = spans.last_mut() {
        last.1 = last
            .1
            .checked_add(u32::try_from(padding).unwrap_or(0))
            .ok_or(D3d12Error::BitstreamTooLarge {
                len: bitstream.len(),
                capacity: u64::from(u32::MAX),
            })?;
    }
    Ok((spans, bitstream))
}

/// H.265 picture arguments for one `DecodeFrame` call.
#[derive(Debug)]
pub(crate) struct HevcSubmission {
    /// Picture parameters.
    pub(crate) params: PicParamsHevc,
    /// Inverse quantization matrices.
    pub(crate) qmatrix: QmatrixHevc,
    /// Slice control entries.
    pub(crate) slices: Vec<SliceShortHevc>,
    /// Start-coded slices padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output texture slot.
    pub(crate) slot: u8,
}

/// AV1 picture arguments for one `DecodeFrame` call.
#[derive(Debug)]
pub(crate) struct Av1Submission {
    /// Picture parameters.
    pub(crate) params: PicParamsAv1,
    /// Tile control entries.
    pub(crate) tiles: Vec<TileAv1>,
    /// Concatenated tile payloads padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output texture slot.
    pub(crate) slot: u8,
}

/// Pool texture tables behind one decode call.
struct RefTables {
    /// Pool textures in slot order.
    textures: [Option<ID3D12Resource>; POOL],
    /// Subresource per pool texture.
    subs: [u32; POOL],
}

/// Picture arguments behind one `DecodeFrame` call.
trait Submission {
    /// Picture parameters type.
    type Params;
    /// Inverse quantization matrix type.
    type Matrix;
    /// Slice control entry type.
    type Slice;
    /// Picture parameters.
    fn params(&self) -> &Self::Params;
    /// Inverse quantization matrices.
    fn matrix(&self) -> &Self::Matrix;
    /// Slice control entries.
    fn slices(&self) -> &[Self::Slice];
    /// Bitstream length in bytes.
    fn bitstream_len(&self) -> usize;
}

impl Submission for H264Submission {
    type Params = PicParamsH264;
    type Matrix = QmatrixH264;
    type Slice = SliceShortH264;

    fn params(&self) -> &Self::Params {
        &self.params
    }

    fn matrix(&self) -> &Self::Matrix {
        &self.qmatrix
    }

    fn slices(&self) -> &[Self::Slice] {
        &self.slices
    }

    fn bitstream_len(&self) -> usize {
        self.bitstream.len()
    }
}

impl Submission for HevcSubmission {
    type Params = PicParamsHevc;
    type Matrix = QmatrixHevc;
    type Slice = SliceShortHevc;

    fn params(&self) -> &Self::Params {
        &self.params
    }

    fn matrix(&self) -> &Self::Matrix {
        &self.qmatrix
    }

    fn slices(&self) -> &[Self::Slice] {
        &self.slices
    }

    fn bitstream_len(&self) -> usize {
        self.bitstream.len()
    }
}

/// Visible extent plus per-plane source boxes under a crop.
struct CropPlan {
    /// Visible width in luma samples.
    width: u32,
    /// Visible height in luma samples.
    height: u32,
    /// Luma box edges: left, top, right, bottom.
    luma: [u32; 4],
    /// Chroma box edges in R8G8 texels: left, top, right, bottom.
    chroma: [u32; 4],
}

/// Crop target plan for `crop` in `width` by `height`, absent without an offset crop.
fn crop_copy_plan(width: u32, height: u32, crop: Option<Crop>) -> Option<CropPlan> {
    if crop.is_none_or(|crop| crop.left == 0 && crop.top == 0) {
        return None;
    }
    crop_plan(width, height, crop)
}

/// Crop copy plan for `crop` in `width` by `height`, absent when uncropped.
fn crop_plan(width: u32, height: u32, crop: Option<Crop>) -> Option<CropPlan> {
    let crop = crop?;
    let left = crop.left;
    let top = crop.top;
    let visible_width = width.saturating_sub(left.saturating_add(crop.right));
    let visible_height = height.saturating_sub(top.saturating_add(crop.bottom));
    if left == 0 && top == 0 && visible_width == width && visible_height == height {
        return None;
    }
    let chroma_left = left / 2;
    let chroma_top = top / 2;
    Some(CropPlan {
        width: visible_width,
        height: visible_height,
        luma: [
            left,
            top,
            left.saturating_add(visible_width),
            top.saturating_add(visible_height),
        ],
        chroma: [
            chroma_left,
            chroma_top,
            chroma_left.saturating_add(visible_width / 2),
            chroma_top.saturating_add(visible_height / 2),
        ],
    })
}

/// Owned NT handle behind one pool texture export.
pub(crate) struct SharedExport {
    /// NT handle value.
    handle: isize,
}

impl SharedExport {
    /// NT handle value.
    pub(crate) fn handle(&self) -> isize {
        self.handle
    }
}

impl Drop for SharedExport {
    fn drop(&mut self) {
        // Sound: the handle is live; the close ends the export.
        let _ = unsafe { CloseHandle(HANDLE(self.handle as *mut c_void)) };
    }
}

/// Submit queues plus slots reused across session rebuilds.
pub(crate) struct DecodeQueues {
    /// Video decode queue.
    video_queue: ID3D12CommandQueue,
    /// Video decode slots, one per pool texture.
    video_slots: Vec<CommandSlot<ID3D12VideoDecodeCommandList>>,
    /// Direct queue behind downloads.
    copy_queue: ID3D12CommandQueue,
    /// Download slots, one per pool texture.
    copy_slots: Vec<CommandSlot<ID3D12GraphicsCommandList>>,
}

impl DecodeQueues {
    /// Queues plus slots over `resources`.
    fn open(resources: &DeviceResources) -> Result<Self, D3d12Error> {
        let video_queue = resources.queue(D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE)?;
        let mut video_slots = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            video_slots.push(CommandSlot::open(
                resources,
                D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE,
            )?);
        }
        let copy_queue = resources.queue(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let mut copy_slots = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            copy_slots.push(CommandSlot::open(
                resources,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
            )?);
        }
        Ok(Self {
            video_queue,
            video_slots,
            copy_queue,
            copy_slots,
        })
    }
}

/// D3D12 video decode session over one codec geometry.
pub(crate) struct DecodeSession {
    /// Logical device behind shared-handle export.
    device: ID3D12Device,
    /// Video decode queue.
    video_queue: ID3D12CommandQueue,
    /// Video decode slots, one per pool texture.
    video_slots: Vec<CommandSlot<ID3D12VideoDecodeCommandList>>,
    /// In-flight video ticket per slot, waited before the slot re-records.
    video_busy: Vec<Option<Ticket>>,
    /// Direct queue behind downloads.
    copy_queue: ID3D12CommandQueue,
    /// Download slots, one per pool texture.
    copy_slots: Vec<CommandSlot<ID3D12GraphicsCommandList>>,
    /// In-flight copy ticket per slot, waited before the slot re-records.
    copy_busy: Vec<Option<Ticket>>,
    /// Fence behind the video queue.
    video_fence: Fence,
    /// Fence behind the direct queue.
    copy_fence: Fence,
    /// Decoder heap behind pictures.
    heap: ID3D12VideoDecoderHeap,
    /// Decoder behind pictures.
    decoder: ID3D12VideoDecoder,
    /// NV12 output texture pool, one per DPB slot.
    outputs: Vec<ID3D12Resource>,
    /// Visible-size crop targets, one per pool slot, empty without an offset crop.
    crops: Vec<ID3D12Resource>,
    /// Bitstream upload buffers, one per pipelined submit.
    bitstreams: Vec<ID3D12Resource>,
    /// In-flight video ticket per upload buffer.
    bitstream_busy: Vec<Option<Ticket>>,
    /// Next upload buffer behind a submit.
    bitstream_next: usize,
    /// Download staging buffers, one per pool slot.
    staging: Vec<ID3D12Resource>,
    /// Staging layout.
    layout: Nv12Layout,
    /// Driver footprints behind staging copies.
    footprints: [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; 2],
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Visible rectangle, absent when uncropped.
    crop: Option<Crop>,
}

impl DecodeSession {
    /// Session over `device` for `codec` at `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a `codec` profile, past the
    /// adapter decode caps, or when the driver refuses the session.
    ///
    /// # Arguments
    ///
    /// * `device` - D3D12 device behind the session.
    /// * `codec` - codec the session decodes.
    /// * `width` - coded width in luma samples.
    /// * `height` - coded height in luma samples.
    /// * `crop` - visible rectangle, absent when uncropped.
    /// * `queues` - submit queues from the dropped session, absent on first open.
    pub(crate) fn open(
        device: &crate::device::D3d12Device,
        codec: crate::guid::Codec,
        width: u32,
        height: u32,
        crop: Option<Crop>,
        queues: Option<DecodeQueues>,
    ) -> Result<Self, D3d12Error> {
        Self::open_windows(device, codec, width, height, crop, queues)
    }

    /// Submit queues plus slots for the next session rebuild.
    ///
    /// Call past the session waits only; in-flight slots reset under the GPU.
    pub(crate) fn into_queues(self) -> DecodeQueues {
        DecodeQueues {
            video_queue: self.video_queue,
            video_slots: self.video_slots,
            copy_queue: self.copy_queue,
            copy_slots: self.copy_slots,
        }
    }

    /// Session over `device` for `codec` at `width` by `height`.
    fn open_windows(
        device: &crate::device::D3d12Device,
        codec: crate::guid::Codec,
        width: u32,
        height: u32,
        crop: Option<Crop>,
        queues: Option<DecodeQueues>,
    ) -> Result<Self, D3d12Error> {
        if codec != crate::guid::Codec::H264
            && codec != crate::guid::Codec::Hevc
            && codec != crate::guid::Codec::Av1
        {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::PictureArgsPending { codec },
            ));
        }
        let profile = crate::device_os::decode_profile(codec).ok_or(D3d12Error::Unsupported(
            UnsupportedReason::NoDecodeProfile { codec },
        ))?;
        crate::device_os::check_decode_support(device.video(), profile, width, height)?;
        let resources = DeviceResources::new(device.device());
        let heap = crate::device_os::create_decoder_heap(device.video(), profile, width, height)?;
        let decoder = crate::device_os::create_decoder(device.video(), profile)?;
        let queues = match queues {
            Some(queues) => queues,
            None => DecodeQueues::open(&resources)?,
        };
        let DecodeQueues {
            video_queue,
            video_slots,
            copy_queue,
            copy_slots,
        } = queues;
        let video_fence = Fence::open(device.device())?;
        let copy_fence = Fence::open(device.device())?;
        let texture_desc = DeviceResources::texture_desc(width, height);
        let mut outputs = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            outputs.push(resources.committed_shared(
                D3D12_HEAP_TYPE_DEFAULT,
                &texture_desc,
                D3D12_RESOURCE_STATE_COMMON,
                "CreateOutput",
            )?);
        }
        let mut crops = Vec::new();
        if let Some(plan) = crop_copy_plan(width, height, crop) {
            let crop_desc = DeviceResources::texture_desc(plan.width, plan.height);
            crops.reserve(POOL);
            for _ in 0..POOL {
                crops.push(resources.committed(
                    D3D12_HEAP_TYPE_DEFAULT,
                    &crop_desc,
                    D3D12_RESOURCE_STATE_COMMON,
                    "CreateCrop",
                )?);
            }
        }
        let upload_desc = DeviceResources::buffer_desc(BITSTREAM_CAP);
        let mut bitstreams = Vec::with_capacity(BITSTREAM_SLOTS);
        for _ in 0..BITSTREAM_SLOTS {
            bitstreams.push(resources.committed(
                D3D12_HEAP_TYPE_UPLOAD,
                &upload_desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                "CreateBitstream",
            )?);
        }
        let (layout, footprints) = resources.nv12_layout(width, height)?;
        let staging_desc = DeviceResources::buffer_desc(layout.total);
        let mut staging = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            staging.push(resources.committed(
                D3D12_HEAP_TYPE_READBACK,
                &staging_desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                "CreateStaging",
            )?);
        }
        Ok(Self {
            device: device.device().clone(),
            video_queue,
            video_slots,
            video_busy: vec![None; POOL],
            copy_queue,
            copy_slots,
            copy_busy: vec![None; POOL],
            video_fence,
            copy_fence,
            heap,
            decoder,
            outputs,
            crops,
            bitstreams,
            bitstream_busy: vec![None; BITSTREAM_SLOTS],
            bitstream_next: 0,
            staging,
            layout,
            footprints,
            width,
            height,
            crop,
        })
    }

    /// `submission` through the video queue, without waiting.
    ///
    /// The ticket completes before the next submit reuses the pool barriers.
    ///
    /// # Errors
    ///
    /// Returns `Err` past the bitstream buffer, or when the
    /// driver refuses the picture.
    pub(crate) fn submit(&mut self, submission: &H264Submission) -> Result<Ticket, D3d12Error> {
        self.submit_h264(submission)
    }

    /// Video `ticket` completion through the fence event.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget.
    pub(crate) fn wait_video(&self, ticket: Ticket) -> Result<(), D3d12Error> {
        self.video_fence.wait(ticket)
    }

    /// True once the GPU signals video `ticket`.
    pub(crate) fn video_signaled(&self, ticket: Ticket) -> bool {
        self.video_fence.poll(ticket)
    }

    /// Video `ticket` completion through the fence event before `timeout`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the event call fails.
    ///
    /// # Arguments
    ///
    /// * `ticket` - fence value the video queue signals.
    /// * `timeout` - wait budget.
    ///
    /// # Returns
    ///
    /// True when the GPU signaled `ticket` before `timeout` lapsed.
    pub(crate) fn wait_video_timeout(
        &self,
        ticket: Ticket,
        timeout: Duration,
    ) -> Result<bool, D3d12Error> {
        self.video_fence.wait_timeout(ticket, timeout)
    }

    /// Copy `ticket` completion through the fence event.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget.
    pub(crate) fn wait_copy(&self, ticket: Ticket) -> Result<(), D3d12Error> {
        self.copy_fence.wait(ticket)
    }

    /// True once the GPU signals copy `ticket`.
    pub(crate) fn copy_signaled(&self, ticket: Ticket) -> bool {
        self.copy_fence.poll(ticket)
    }

    /// Copy `ticket` completion through the fence event before `timeout`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the event call fails.
    ///
    /// # Arguments
    ///
    /// * `ticket` - fence value the copy queue signals.
    /// * `timeout` - wait budget.
    ///
    /// # Returns
    ///
    /// True when the GPU signaled `ticket` before `timeout` lapsed.
    pub(crate) fn wait_copy_timeout(
        &self,
        ticket: Ticket,
        timeout: Duration,
    ) -> Result<bool, D3d12Error> {
        self.copy_fence.wait_timeout(ticket, timeout)
    }

    /// Video decode queue behind submissions.
    pub(crate) fn video_queue(&self) -> &ID3D12CommandQueue {
        &self.video_queue
    }

    /// Direct queue behind downloads.
    pub(crate) fn copy_queue(&self) -> &ID3D12CommandQueue {
        &self.copy_queue
    }

    /// Completion wait behind a video `ticket`.
    pub(crate) fn video_wait(&self, ticket: Ticket) -> SurfaceWait {
        SurfaceWait::new(self.video_fence.handle(), ticket.value())
    }

    /// Completion wait behind a copy `ticket`.
    pub(crate) fn copy_wait(&self, ticket: Ticket) -> SurfaceWait {
        SurfaceWait::new(self.copy_fence.handle(), ticket.value())
    }

    /// Pool `slot` planes through the direct queue, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, or when
    /// the copy fails.
    pub(crate) fn start_download(&mut self, slot: u8) -> Result<Ticket, D3d12Error> {
        let index = self.record_download(slot)?;
        self.execute_download(index)
    }

    /// Pool `slot` planes through the direct queue after video `ticket`, without waiting.
    ///
    /// The copy queue waits on the video fence; the copy runs beside the
    /// next video submit on its own queue.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, or when
    /// the copy fails.
    pub(crate) fn start_download_after(
        &mut self,
        slot: u8,
        video: Ticket,
    ) -> Result<Ticket, D3d12Error> {
        let index = self.record_download(slot)?;
        let fence = self.video_fence.handle();
        // Sound: the fence and queue are live; the value is a submitted ticket.
        unsafe { self.copy_queue.Wait(&fence, video.value()) }.map_err(|error| {
            D3d12Error::Call {
                call: "QueueWait",
                hresult: error.code().0,
            }
        })?;
        self.execute_download(index)
    }

    /// Download list for pool `slot`, recorded and open.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, or when
    /// the copy fails.
    fn record_download(&mut self, slot: u8) -> Result<usize, D3d12Error> {
        let index = usize::from(slot);
        let source = self
            .outputs
            .get(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?
            .clone();
        self.wait_copy_slot(index)?;
        let pair = self
            .copy_slots
            .get_mut(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?;
        pair.reset()?;
        let list = pair.list();
        // Sound: the barriers name a live texture; the list is open.
        unsafe {
            list.ResourceBarrier(&[DeviceResources::barrier(
                &source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
        }
        let staging = self
            .staging
            .get(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?
            .clone();
        // Sound: the copy locations name live resources; the list is open.
        unsafe {
            for (plane, placed) in [0u32, 1u32].into_iter().zip(self.footprints.iter()) {
                let dst = DeviceResources::footprint_location(&staging, placed);
                let src = DeviceResources::subresource_location(&source, plane);
                list.CopyTextureRegion(
                    std::ptr::from_ref(&dst),
                    0,
                    0,
                    0,
                    std::ptr::from_ref(&src),
                    None,
                );
            }
            list.ResourceBarrier(&[DeviceResources::barrier(
                &source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        Ok(index)
    }

    /// Recorded download list for pool `index` through the direct queue, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` for an index outside the pool, or when
    /// the copy fails.
    fn execute_download(&mut self, index: usize) -> Result<Ticket, D3d12Error> {
        let pair = self
            .copy_slots
            .get_mut(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?;
        let ticket = pair.close_execute(&self.copy_queue, &mut self.copy_fence)?;
        self.copy_busy[index] = Some(ticket);
        Ok(ticket)
    }

    /// Tight NV12 bytes for a started `slot` once `ticket` completes.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, when the GPU misses
    /// the wait budget, or when staging falls short of the layout.
    pub(crate) fn finish_download(&self, slot: u8, ticket: Ticket) -> Result<Vec<u8>, D3d12Error> {
        self.copy_fence.wait(ticket)?;
        self.readback(slot)
    }

    /// Tight NV12 bytes for a started `slot` when `ticket` signaled, absent while the copy runs.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, or when
    /// staging falls short of the layout.
    pub(crate) fn poll_download(
        &self,
        slot: u8,
        ticket: Ticket,
    ) -> Result<Option<Vec<u8>>, D3d12Error> {
        if !self.copy_fence.poll(ticket) {
            return Ok(None);
        }
        self.readback(slot).map(Some)
    }

    /// Tight NV12 bytes mapped from the staging buffer behind `slot`.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, or when
    /// staging falls short of the layout.
    fn readback(&self, slot: u8) -> Result<Vec<u8>, D3d12Error> {
        let index = usize::from(slot);
        let staging = self.staging.get(index).ok_or(D3d12Error::BrokenReference(
            BrokenReferenceReason::SlotOutsidePool,
        ))?;
        let total = usize::try_from(self.layout.total)
            .unwrap_or(usize::MAX)
            .min(isize::MAX as usize);
        DeviceResources::with_readback(staging, total, |mapped| {
            self.layout
                .tighten(mapped, self.width, self.height, self.crop)
        })
    }

    /// Prior copy ticket on `index` completion.
    fn wait_copy_slot(&self, index: usize) -> Result<(), D3d12Error> {
        if let Some(Some(ticket)) = self.copy_busy.get(index) {
            self.copy_fence.wait(*ticket)?;
        }
        Ok(())
    }

    /// NT-handle export for pool `slot`, absent when cropped or refused.
    pub(crate) fn shared_handle(&self, slot: u8) -> Option<SharedExport> {
        if crop_plan(self.width, self.height, self.crop).is_some() {
            return None;
        }
        let resource = self.outputs.get(usize::from(slot))?;
        Self::share_resource(&self.device, resource)
    }

    /// NT-handle export for crop `slot`, absent without a crop target or refused.
    pub(crate) fn shared_handle_crop(&self, slot: u8) -> Option<SharedExport> {
        let resource = self.crops.get(usize::from(slot))?;
        Self::share_resource(&self.device, resource)
    }

    /// NT-handle export for `resource`, absent when the driver declines sharing.
    fn share_resource(device: &ID3D12Device, resource: &ID3D12Resource) -> Option<SharedExport> {
        // Sound: the resource is live; every resource exposes the device-child face.
        let child: ID3D12DeviceChild = resource.cast().ok()?;
        // Sound: the device and resource are live; default security, unnamed.
        let handle =
            unsafe { device.CreateSharedHandle(&child, None, GENERIC_ALL.0, PCWSTR::null()) }
                .ok()?;
        (!handle.is_invalid()).then(|| SharedExport {
            handle: handle.0 as isize,
        })
    }

    /// Pool texture behind `slot`, without readback.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool.
    pub(crate) fn texture(&self, slot: u8) -> Result<ID3D12Resource, D3d12Error> {
        self.outputs
            .get(usize::from(slot))
            .cloned()
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))
    }

    /// True with per-slot crop targets behind the session.
    pub(crate) fn cropped(&self) -> bool {
        !self.crops.is_empty()
    }

    /// Pool `slot` visible rectangle through the direct queue, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool, without a crop plan,
    /// or when the copy fails.
    pub(crate) fn start_crop(&mut self, slot: u8) -> Result<Ticket, D3d12Error> {
        let index = usize::from(slot);
        let source = self
            .outputs
            .get(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?
            .clone();
        let target = self
            .crops
            .get(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?
            .clone();
        let plan = crop_plan(self.width, self.height, self.crop).ok_or(
            D3d12Error::BrokenReference(BrokenReferenceReason::CropTargetWithoutCrop),
        )?;
        self.wait_copy_slot(index)?;
        let pair = self
            .copy_slots
            .get_mut(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?;
        pair.reset()?;
        let list = pair.list();
        // Sound: the barriers name live textures; the list is open.
        unsafe {
            list.ResourceBarrier(&[
                DeviceResources::barrier(
                    &source,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
                DeviceResources::barrier(
                    &target,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                ),
            ]);
        }
        let boxes = [plan.luma, plan.chroma].map(|edges| D3D12_BOX {
            left: edges[0],
            top: edges[1],
            front: 0,
            right: edges[2],
            bottom: edges[3],
            back: 1,
        });
        // Sound: the copy locations name live resources; the list is open.
        unsafe {
            for (plane, src_box) in [0u32, 1u32].into_iter().zip(boxes.iter()) {
                let dst = DeviceResources::subresource_location(&target, plane);
                let src = DeviceResources::subresource_location(&source, plane);
                list.CopyTextureRegion(
                    std::ptr::from_ref(&dst),
                    0,
                    0,
                    0,
                    std::ptr::from_ref(&src),
                    Some(std::ptr::from_ref(src_box)),
                );
            }
            list.ResourceBarrier(&[
                DeviceResources::barrier(
                    &source,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
                DeviceResources::barrier(
                    &target,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
            ]);
        }
        let pair = self
            .copy_slots
            .get_mut(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?;
        let ticket = pair.close_execute(&self.copy_queue, &mut self.copy_fence)?;
        self.copy_busy[index] = Some(ticket);
        Ok(ticket)
    }

    /// Crop target for a started `slot` once `ticket` completes.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool or when the GPU
    /// misses the wait budget.
    pub(crate) fn finish_crop(
        &self,
        slot: u8,
        ticket: Ticket,
    ) -> Result<ID3D12Resource, D3d12Error> {
        self.copy_fence.wait(ticket)?;
        self.crops
            .get(usize::from(slot))
            .cloned()
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))
    }

    /// `submission` through the video queue into its pool slot.
    fn submit_h264(&mut self, submission: &H264Submission) -> Result<Ticket, D3d12Error> {
        Self::check_bitstream_len(submission.bitstream.len())?;
        let (upload, upload_index) = self.acquire_bitstream()?;
        DeviceResources::write_upload(&upload, &submission.bitstream)?;
        let mut refs = self.ref_tables();
        let input = self.input_args(submission, Some(&mut refs), &upload)?;
        let ticket = self.submit_video(&input, submission.slot)?;
        self.bitstream_busy[upload_index] = Some(ticket);
        Ok(ticket)
    }

    /// Upload buffer plus index behind the next submit.
    ///
    /// Scans past buffers whose video ticket signaled; waits the next one
    /// only when every buffer is in flight.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget.
    fn acquire_bitstream(&mut self) -> Result<(ID3D12Resource, usize), D3d12Error> {
        for _ in 0..BITSTREAM_SLOTS {
            let index = self.bitstream_next;
            self.bitstream_next = (self.bitstream_next + 1) % BITSTREAM_SLOTS;
            let busy =
                self.bitstream_busy[index].is_some_and(|ticket| !self.video_fence.poll(ticket));
            if !busy {
                return Ok((self.bitstreams[index].clone(), index));
            }
        }
        let index = self.bitstream_next;
        if let Some(ticket) = self.bitstream_busy[index] {
            self.video_fence.wait(ticket)?;
        }
        Ok((self.bitstreams[index].clone(), index))
    }

    /// `submission` through the video queue into its pool slot.
    ///
    /// The ticket completes before the next submit reuses the pool barriers.
    ///
    /// # Errors
    ///
    /// Returns `Err` past the bitstream buffer, or when the
    /// driver refuses the picture.
    pub(crate) fn submit_hevc(
        &mut self,
        submission: &HevcSubmission,
    ) -> Result<Ticket, D3d12Error> {
        Self::check_bitstream_len(submission.bitstream.len())?;
        let (upload, upload_index) = self.acquire_bitstream()?;
        DeviceResources::write_upload(&upload, &submission.bitstream)?;
        let mut refs = self.ref_tables();
        let input = self.input_args(submission, Some(&mut refs), &upload)?;
        let ticket = self.submit_video(&input, submission.slot)?;
        self.bitstream_busy[upload_index] = Some(ticket);
        Ok(ticket)
    }

    /// `submission` through the video queue into its pool slot.
    ///
    /// The ticket completes before the next submit reuses the pool barriers.
    ///
    /// # Errors
    ///
    /// Returns `Err` past the bitstream buffer, or when the
    /// driver refuses the picture.
    pub(crate) fn submit_av1(&mut self, submission: &Av1Submission) -> Result<Ticket, D3d12Error> {
        Self::check_bitstream_len(submission.bitstream.len())?;
        let (upload, upload_index) = self.acquire_bitstream()?;
        DeviceResources::write_upload(&upload, &submission.bitstream)?;
        let mut refs = self.ref_tables();
        let input = self.input_args_av1(submission, &mut refs, &upload)?;
        let ticket = self.submit_video(&input, submission.slot)?;
        self.bitstream_busy[upload_index] = Some(ticket);
        Ok(ticket)
    }

    /// Bitstream length against the upload capacity.
    fn check_bitstream_len(len: usize) -> Result<(), D3d12Error> {
        if u64::try_from(len).unwrap_or(u64::MAX) > BITSTREAM_CAP {
            return Err(D3d12Error::BitstreamTooLarge {
                len,
                capacity: BITSTREAM_CAP,
            });
        }
        Ok(())
    }

    /// Pool tables behind the reference frame arguments.
    fn ref_tables(&self) -> RefTables {
        let mut textures: [Option<ID3D12Resource>; POOL] = core::array::from_fn(|_| None);
        for (slot, texture) in textures.iter_mut().zip(self.outputs.iter()) {
            *slot = Some(texture.clone());
        }
        RefTables {
            textures,
            subs: [0; POOL],
        }
    }

    /// `DecodeFrame` for `input` through the video queue into pool `slot`.
    ///
    /// Submits without waiting on other slots; only the target slot
    /// re-record waits its own prior ticket.
    fn submit_video(
        &mut self,
        input: &D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS,
        slot: u8,
    ) -> Result<Ticket, D3d12Error> {
        let index = usize::from(slot);
        if let Some(ticket) = self.video_busy.get(index).copied().flatten() {
            self.video_fence.wait(ticket)?;
        }
        let target = self
            .outputs
            .get(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?
            .clone();
        let pair = self
            .video_slots
            .get_mut(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?;
        pair.reset()?;
        let list = pair.list();
        let output = D3D12_VIDEO_DECODE_OUTPUT_STREAM_ARGUMENTS {
            pOutputTexture2D: ManuallyDrop::new(Some(target.clone())),
            OutputSubresource: 0,
            ConversionArguments: D3D12_VIDEO_DECODE_CONVERSION_ARGUMENTS::default(),
        };
        debug_assert_eq!(self.outputs.len(), POOL);
        let enter: [D3D12_RESOURCE_BARRIER; POOL] = core::array::from_fn(|index| {
            let after = if u8::try_from(index).is_ok_and(|at| at == slot) {
                D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE
            } else {
                D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
            };
            DeviceResources::barrier(&self.outputs[index], D3D12_RESOURCE_STATE_COMMON, after)
        });
        let leave: [D3D12_RESOURCE_BARRIER; POOL] = core::array::from_fn(|index| {
            let before = if u8::try_from(index).is_ok_and(|at| at == slot) {
                D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE
            } else {
                D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
            };
            DeviceResources::barrier(&self.outputs[index], before, D3D12_RESOURCE_STATE_COMMON)
        });
        // Sound: the barriers name live textures; the list is open.
        // Sound: the argument structs outlive the fence wait below.
        unsafe {
            list.ResourceBarrier(&enter);
            list.DecodeFrame(
                &self.decoder,
                std::ptr::from_ref(&output),
                std::ptr::from_ref(input),
            );
            list.ResourceBarrier(&leave);
        }
        let pair = self
            .video_slots
            .get_mut(index)
            .ok_or(D3d12Error::BrokenReference(
                BrokenReferenceReason::SlotOutsidePool,
            ))?;
        let ticket = pair.close_execute(&self.video_queue, &mut self.video_fence)?;
        self.video_busy[index] = Some(ticket);
        Ok(ticket)
    }

    /// Decode input arguments borrowing `submission`, plus `refs` when present.
    fn input_args<S: Submission>(
        &self,
        submission: &S,
        refs: Option<&mut RefTables>,
        bitstream: &ID3D12Resource,
    ) -> Result<D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS, D3d12Error> {
        let params_len = u32::try_from(std::mem::size_of::<S::Params>())
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureArgsTooLarge))?;
        let matrix_len = u32::try_from(std::mem::size_of::<S::Matrix>())
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureArgsTooLarge))?;
        let slices_len = u32::try_from(
            std::mem::size_of::<S::Slice>().saturating_mul(submission.slices().len()),
        )
        .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureArgsTooLarge))?;
        let bitstream_len = u64::try_from(submission.bitstream_len())
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        let mut args = [D3D12_VIDEO_DECODE_FRAME_ARGUMENT::default(); 10];
        args[0] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
            Size: params_len,
            pData: std::ptr::from_ref(submission.params())
                .cast_mut()
                .cast::<c_void>(),
        };
        args[1] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
            Size: matrix_len,
            pData: std::ptr::from_ref(submission.matrix())
                .cast_mut()
                .cast::<c_void>(),
        };
        args[2] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
            Size: slices_len,
            pData: submission.slices().as_ptr().cast_mut().cast::<c_void>(),
        };
        let references = match refs {
            Some(refs) => D3D12_VIDEO_DECODE_REFERENCE_FRAMES {
                NumTexture2Ds: u32::try_from(POOL).unwrap_or(u32::MAX),
                ppTexture2Ds: refs.textures.as_mut_ptr(),
                pSubresources: refs.subs.as_mut_ptr(),
                ppHeaps: std::ptr::null_mut(),
            },
            None => D3D12_VIDEO_DECODE_REFERENCE_FRAMES::default(),
        };
        Ok(D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS {
            NumFrameArguments: 3,
            FrameArguments: args,
            ReferenceFrames: references,
            CompressedBitstream: D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM {
                pBuffer: ManuallyDrop::new(Some(bitstream.clone())),
                Offset: 0,
                Size: bitstream_len,
            },
            pHeap: ManuallyDrop::new(Some(self.heap.clone())),
        })
    }

    /// AV1 decode input arguments borrowing `submission` plus `refs`.
    ///
    /// Two frame arguments: AV1 carries no inverse quantization matrix.
    fn input_args_av1(
        &self,
        submission: &Av1Submission,
        refs: &mut RefTables,
        bitstream: &ID3D12Resource,
    ) -> Result<D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS, D3d12Error> {
        let params_len = u32::try_from(std::mem::size_of::<PicParamsAv1>())
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureArgsTooLarge))?;
        let tiles_len =
            u32::try_from(std::mem::size_of::<TileAv1>().saturating_mul(submission.tiles.len()))
                .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureArgsTooLarge))?;
        let bitstream_len = u64::try_from(submission.bitstream.len())
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        let mut args = [D3D12_VIDEO_DECODE_FRAME_ARGUMENT::default(); 10];
        args[0] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
            Size: params_len,
            pData: std::ptr::from_ref(&submission.params)
                .cast_mut()
                .cast::<c_void>(),
        };
        args[1] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
            Size: tiles_len,
            pData: submission.tiles.as_ptr().cast_mut().cast::<c_void>(),
        };
        Ok(D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS {
            NumFrameArguments: 2,
            FrameArguments: args,
            ReferenceFrames: D3D12_VIDEO_DECODE_REFERENCE_FRAMES {
                NumTexture2Ds: u32::try_from(POOL).unwrap_or(u32::MAX),
                ppTexture2Ds: refs.textures.as_mut_ptr(),
                pSubresources: refs.subs.as_mut_ptr(),
                ppHeaps: std::ptr::null_mut(),
            },
            CompressedBitstream: D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM {
                pBuffer: ManuallyDrop::new(Some(bitstream.clone())),
                Offset: 0,
                Size: bitstream_len,
            },
            pHeap: ManuallyDrop::new(Some(self.heap.clone())),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::h264::Crop;
    use crate::h264::{parse_pps, parse_sps, Ingest};

    #[test]
    fn crop_plan_absent_when_uncropped() {
        assert!(super::crop_plan(640, 368, None).is_none());
        let zero = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        };
        assert!(super::crop_plan(640, 368, Some(zero)).is_none());
    }

    #[test]
    fn crop_plan_maps_360p_chroma_half_height() {
        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        let plan = super::crop_plan(640, 368, Some(crop)).expect("cropped");
        assert_eq!((plan.width, plan.height), (640, 360));
        assert_eq!(plan.luma, [0, 0, 640, 360]);
        assert_eq!(plan.chroma, [0, 0, 320, 180]);
    }

    #[test]
    fn crop_plan_maps_1080p_chroma_half_height() {
        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        let plan = super::crop_plan(1920, 1088, Some(crop)).expect("cropped");
        assert_eq!((plan.width, plan.height), (1920, 1080));
        assert_eq!(plan.luma, [0, 0, 1920, 1080]);
        assert_eq!(plan.chroma, [0, 0, 960, 540]);
    }

    #[test]
    fn crop_plan_offsets_symmetric_crop() {
        let crop = Crop {
            left: 8,
            right: 8,
            top: 8,
            bottom: 8,
        };
        let plan = super::crop_plan(336, 256, Some(crop)).expect("cropped");
        assert_eq!((plan.width, plan.height), (320, 240));
        assert_eq!(plan.luma, [8, 8, 328, 248]);
        assert_eq!(plan.chroma, [4, 4, 164, 124]);
    }

    /// Test bit writer.
    struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        fn bit(&mut self, set: bool) {
            if set {
                self.current |= 1 << (7 - self.filled);
            }
            self.filled += 1;
            if self.filled == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.filled = 0;
            }
        }

        /// Low `count` bits of `value`, most significant first.
        fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Unsigned Exp-Golomb code.
        fn ue(&mut self, value: u32) {
            let code = value + 1;
            let width = 32 - code.leading_zeros();
            for _ in 1..width {
                self.bit(false);
            }
            self.bits(code, width);
        }

        /// Signed Exp-Golomb code.
        fn se(&mut self, value: i32) {
            let code = if value > 0 {
                u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
            } else {
                u32::try_from(-value).unwrap_or(u32::MAX) * 2
            };
            self.ue(code);
        }

        /// Finished RBSP with emulation prevention inserted.
        fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            while self.filled != 0 {
                self.bit(false);
            }
            let mut out = Vec::new();
            let mut zeros = 0;
            for byte in self.bytes {
                if zeros == 2 && byte <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                out.push(byte);
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            out
        }
    }

    /// Baseline 64x48 sequence set body.
    fn sps_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(66, 8);
        writer.bits(0, 8);
        writer.bits(30, 8);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(1);
        writer.bit(false);
        writer.ue(3);
        writer.ue(2);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.finish()
    }

    /// Picture set body over sequence set zero.
    fn pps_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bits(0, 2);
        writer.se(0);
        writer.se(0);
        writer.se(0);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// NAL unit with header over the body.
    fn nal(nal_type: u8, ref_idc: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![ref_idc << 5 | nal_type];
        nal.extend_from_slice(body);
        nal
    }

    /// Packet of NAL units with four-byte start codes.
    fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut packet = Vec::new();
        for nal in nals {
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(nal);
        }
        packet
    }

    /// IDR slice body over the test sets.
    fn idr_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(2);
        writer.ue(0);
        writer.bits(0, 4);
        writer.ue(0);
        writer.bits(0, 4);
        writer.bit(false);
        writer.bit(false);
        writer.se(0);
        writer.ue(0);
        writer.se(0);
        writer.se(0);
        writer.finish()
    }

    /// P slice body over the test sets.
    fn p_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bits(1, 4);
        writer.bits(2, 4);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.ue(0);
        writer.se(0);
        writer.se(0);
        writer.finish()
    }

    /// Ingested pictures from one packet.
    fn ingest(data: &[u8]) -> Vec<crate::h264::Picture> {
        let mut ingest = Ingest::new();
        ingest.ingest_packet(data).expect("ingest")
    }

    /// Empty pin table behind buffer tests.
    fn pins() -> std::sync::Arc<crate::pins::SlotPins> {
        std::sync::Arc::new(crate::pins::SlotPins::new())
    }

    #[test]
    fn submission_carries_params_and_start_codes() {
        let data = packet(&[
            nal(7, 3, &sps_body()),
            nal(8, 3, &pps_body()),
            nal(5, 3, &idr_body()),
        ]);
        let pictures = ingest(&data);
        assert_eq!(pictures.len(), 1);
        let sps = parse_sps(&crate::bits::rbsp(&nal(7, 3, &sps_body()))[1..]).expect("sps");
        let pps = parse_pps(&crate::bits::rbsp(&nal(8, 3, &pps_body()))[1..], &sps).expect("pps");
        let mut dpb = crate::dpb::Dpb::new(&sps, pins());
        let decoded = dpb.decode(&pictures[0], &sps, &pps).expect("submission");
        assert!(decoded.outputs.is_empty());
        assert_eq!(decoded.submission.slot, 0);
        assert_eq!(decoded.submission.params.frame_width_in_mbs_minus1, 3);
        assert_eq!(decoded.submission.params.frame_height_in_mbs_minus1, 2);
        assert_eq!(decoded.submission.params.status_report_feedback_number, 1);
        assert_eq!(decoded.submission.params.curr_field_order_cnt, [0, 0]);
        assert_eq!(decoded.submission.slices.len(), 1);
        assert_eq!(decoded.submission.slices[0].location, 0);
        assert_eq!(&decoded.submission.bitstream[..4], &[0, 0, 1, 0x65]);
        assert_eq!(decoded.submission.bitstream.len() % 128, 0);
    }

    #[test]
    fn submission_matches_captured_baseline_idr() {
        let sps_nal = hex("6742c00dda0507ec0440000003004000000f03c50aa8");
        let pps_nal = hex("68ce0fc8");
        let slice_nal = hex(
            "6588840bfffffc3d140004517c9ff87fe08c140a0004bc40022c4000100700688829a381fffc8b84\
             01470064628cd0f81c010aa6371808f4007ff4bd",
        );
        let data = packet(&[sps_nal.clone(), pps_nal.clone(), slice_nal]);
        let pictures = ingest(&data);
        assert_eq!(pictures.len(), 1);
        let sps = parse_sps(&crate::bits::rbsp(&sps_nal)[1..]).expect("sps");
        let pps = parse_pps(&crate::bits::rbsp(&pps_nal)[1..], &sps).expect("pps");
        assert_eq!((sps.visible_width(), sps.visible_height()), (320, 240));
        assert_eq!(pps.pic_init_qp, -3);
        let picture = &pictures[0];
        assert!(picture.intra());
        assert!(picture.idr);
        assert_eq!(picture.frame_num(), 0);
        let header = &picture.slices[0].header;
        assert_eq!(header.first_mb, 0);
        assert!(header.slice_type.is_intra());
        assert_eq!(header.qp_delta, -11);
        let mut dpb = crate::dpb::Dpb::new(&sps, pins());
        let decoded = dpb.decode(picture, &sps, &pps).expect("submission");
        let params = &decoded.submission.params;
        assert_eq!(params.frame_width_in_mbs_minus1, 19);
        assert_eq!(params.frame_height_in_mbs_minus1, 14);
        assert_eq!(params.num_ref_frames, 1);
        assert_eq!(params.pic_init_qp_minus26, -3);
        assert_eq!(params.num_ref_idx_l0_active_minus1, 0);
        assert_eq!(params.num_ref_idx_l1_active_minus1, 0);
        assert_eq!(params.frame_num, 0);
        assert_eq!(params.pic_order_cnt_type, 2);
        assert_eq!(params.curr_field_order_cnt, [0, 0]);
        assert_eq!(params.direct_8x8_inference_flag, 1);
        assert_eq!(params.entropy_coding_mode_flag, 0);
        assert_eq!(params.deblocking_filter_control_present_flag, 1);
        assert_eq!(params.chroma_qp_index_offset, 0);
        assert_eq!(params.second_chroma_qp_index_offset, 0);
        assert_eq!(decoded.submission.qmatrix.lists_4x4[0], [16; 16]);
        assert_eq!(decoded.submission.qmatrix.lists_4x4[3], [16; 16]);
        assert_eq!(decoded.submission.qmatrix.lists_8x8[0], [16; 64]);
    }

    #[test]
    fn submission_matches_captured_high_8x8_idr() {
        let sps_nal = hex("6764000dacd94141fb0110000003001000000303c0f1429960");
        let pps_nal = hex("68cf8fcb");
        let slice_nal = hex(
            "658884002feffff87a280008a2f93df87fe08c130a0005bc40022c4000100b0037f82204e28f8e0\
             0046381dffe443840",
        );
        let data = packet(&[sps_nal.clone(), pps_nal.clone(), slice_nal]);
        let pictures = ingest(&data);
        assert_eq!(pictures.len(), 1);
        let sps = parse_sps(&crate::bits::rbsp(&sps_nal)[1..]).expect("sps");
        let pps = parse_pps(&crate::bits::rbsp(&pps_nal)[1..], &sps).expect("pps");
        assert_eq!((sps.visible_width(), sps.visible_height()), (320, 240));
        assert!(pps.transform_8x8());
        assert!(!pps.entropy());
        assert_eq!(pps.pic_init_qp, -3);
        assert_eq!(pps.second_chroma_qp, 0);
        let mut dpb = crate::dpb::Dpb::new(&sps, pins());
        let decoded = dpb.decode(&pictures[0], &sps, &pps).expect("submission");
        let params = &decoded.submission.params;
        assert_eq!(params.bit_fields, 0xBD50);
        assert_eq!(params.pic_order_cnt_type, 0);
        assert_eq!(params.log2_max_pic_order_cnt_lsb_minus4, 2);
        assert_eq!(params.curr_field_order_cnt, [0, 0]);
        assert_eq!(params.num_ref_frames, 4);
        assert_eq!(decoded.submission.qmatrix.lists_4x4[0], [16; 16]);
        assert_eq!(decoded.submission.qmatrix.lists_4x4[3], [16; 16]);
        assert_eq!(decoded.submission.qmatrix.lists_8x8[0], [16; 64]);
        assert_eq!(decoded.submission.qmatrix.lists_8x8[1], [16; 64]);
    }

    /// Bytes from even-length hex `text`.
    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        digits
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(core::str::from_utf8(pair).expect("hex"), 16).expect("hex")
            })
            .collect()
    }

    #[test]
    fn submission_accepts_p_with_references() {
        let idr = packet(&[
            nal(7, 3, &sps_body()),
            nal(8, 3, &pps_body()),
            nal(5, 3, &idr_body()),
        ]);
        let next = packet(&[nal(1, 3, &p_body())]);
        let mut live = Ingest::new();
        let idr = live.ingest_packet(&idr).expect("idr");
        assert_eq!(idr.len(), 1);
        let sps = parse_sps(&crate::bits::rbsp(&nal(7, 3, &sps_body()))[1..]).expect("sps");
        let pps = parse_pps(&crate::bits::rbsp(&nal(8, 3, &pps_body()))[1..], &sps).expect("pps");
        let mut dpb = crate::dpb::Dpb::new(&sps, pins());
        dpb.decode(&idr[0], &sps, &pps).expect("idr");
        let pictures = live.ingest_packet(&next).expect("p");
        assert_eq!(pictures.len(), 1);
        assert!(!pictures[0].intra());
        let decoded = dpb.decode(&pictures[0], &sps, &pps).expect("p");
        assert_eq!(decoded.submission.slot, 1);
        assert_eq!(decoded.submission.params.ref_frame_list[0].entry, 0);
        assert_eq!(decoded.submission.params.used_for_reference_flags, 0b11);
        assert_eq!(decoded.outputs.len(), 1);
        assert_eq!(decoded.outputs[0].slot(), 0);
    }
}
