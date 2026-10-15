//! D3D12 video decode: picture arguments plus GPU submission.
//!
//! H.264 decodes through the DPB; HEVC stays intra-only.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::ffi::c_void;
#[cfg(target_os = "windows")]
use std::mem::ManuallyDrop;

use crate::dxva::{
    PicEntryHevc, PicParamsH264, PicParamsHevc, QmatrixH264, QmatrixHevc, SliceShortH264,
    SliceShortHevc, HEVC_CHROMA_420, HEVC_IDR_PIC, HEVC_INTRA_PIC, HEVC_IRAP_PIC,
    HEVC_POC_LSB_SHIFT, HEVC_PROP_CHROMA_QP_OFFSETS, HEVC_PROP_CONSTRAINED_INTRA,
    HEVC_PROP_CU_QP_DELTA, HEVC_PROP_DEBLOCK_DISABLED, HEVC_PROP_DEBLOCK_OVERRIDE,
    HEVC_PROP_ENTROPY_SYNC, HEVC_PROP_LISTS_MOD, HEVC_PROP_LOOP_SLICES, HEVC_PROP_LOOP_TILES,
    HEVC_PROP_SLICE_EXT, HEVC_PROP_TILES, HEVC_PROP_TRANSFORM_SKIP, HEVC_PROP_TRANSQUANT_BYPASS,
    HEVC_PROP_UNIFORM_SPACING, HEVC_PROP_WEIGHTED_BIPRED, HEVC_PROP_WEIGHTED_PRED, HEVC_TOOL_AMP,
    HEVC_TOOL_CABAC_INIT, HEVC_TOOL_DEPENDENT_SLICES, HEVC_TOOL_EXTRA_BITS_SHIFT,
    HEVC_TOOL_OUTPUT_FLAG, HEVC_TOOL_SAO, HEVC_TOOL_SIGN_HIDING, HEVC_TOOL_STRONG_INTRA,
    HEVC_TOOL_TEMPORAL_MVP,
};
use crate::error::D3d12Error;
#[cfg(not(target_os = "windows"))]
use crate::error::SkipReason;
use crate::h264::Crop;
#[cfg(target_os = "windows")]
use crate::submit::{DeviceResources, Fence, Nv12Layout};

#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12GraphicsCommandList,
    ID3D12Resource, D3D12_BOX, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE, D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_READBACK,
    D3D12_HEAP_TYPE_UPLOAD, D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATE_VIDEO_DECODE_READ,
    D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE,
};
#[cfg(target_os = "windows")]
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

/// Output texture pool behind the DPB slots.
#[cfg(target_os = "windows")]
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
        return Err(D3d12Error::Unsupported("picture without slices".to_owned()));
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
pub(crate) struct HevcSubmission {
    /// Picture parameters.
    params: PicParamsHevc,
    /// Inverse quantization matrices.
    qmatrix: QmatrixHevc,
    /// Slice control entries.
    slices: Vec<SliceShortHevc>,
    /// Start-coded slices padded to 128 bytes.
    bitstream: Vec<u8>,
}

impl HevcSubmission {
    /// Arguments for `picture` under its parameter sets.
    ///
    /// # Errors
    ///
    /// Returns `Err` for inter pictures, non-IDR intra pictures, or a
    /// bitstream past the driver buffer.
    pub(crate) fn build(
        sps: &crate::hevc::Sps,
        pps: &crate::hevc::Pps,
        picture: &crate::hevc::Picture,
        feedback: u32,
    ) -> Result<Self, D3d12Error> {
        if !picture.intra() {
            return Err(D3d12Error::Unsupported(
                "inter picture needs references".to_owned(),
            ));
        }
        if !picture.idr {
            return Err(D3d12Error::Unsupported(
                "non-IDR intra needs POC tracking".to_owned(),
            ));
        }
        if picture.slices.is_empty() {
            return Err(D3d12Error::Unsupported("picture without slices".to_owned()));
        }
        let min_cb = sps.min_cb();
        let width_in_min_cbs = u16::try_from(sps.width().div_ceil(min_cb))
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let height_in_min_cbs = u16::try_from(sps.height().div_ceil(min_cb))
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let mut params = PicParamsHevc::empty();
        params.pic_width_in_min_cbs_y = width_in_min_cbs;
        params.pic_height_in_min_cbs_y = height_in_min_cbs;
        params.format_flags =
            HEVC_CHROMA_420 | (u16::from(sps.log2_max_poc_lsb_minus4) << HEVC_POC_LSB_SHIFT);
        params.curr_pic = PicEntryHevc::frame(0);
        params.sps_max_dec_pic_buffering_minus1 = sps.max_dec_pic_buffering_minus1;
        params.log2_min_luma_coding_block_size_minus3 = sps.log2_min_cb_minus3;
        params.log2_diff_max_min_luma_coding_block_size = sps.log2_diff_max_min_cb;
        params.log2_min_transform_block_size_minus2 = sps.log2_min_tb_minus2;
        params.log2_diff_max_min_transform_block_size = sps.log2_diff_max_min_tb;
        params.max_transform_hierarchy_depth_inter = sps.max_depth_inter;
        params.max_transform_hierarchy_depth_intra = sps.max_depth_intra;
        params.num_ref_idx_l0_default_active_minus1 = pps.num_ref_l0;
        params.num_ref_idx_l1_default_active_minus1 = pps.num_ref_l1;
        params.init_qp_minus26 = pps.init_qp_minus26;
        params.tool_flags = Self::tool_flags(sps, pps);
        params.property_flags = Self::property_flags(pps);
        params.pps_cb_qp_offset = pps.cb_qp_offset;
        params.pps_cr_qp_offset = pps.cr_qp_offset;
        params.num_tile_columns_minus1 = pps.tile_columns_minus1;
        params.num_tile_rows_minus1 = pps.tile_rows_minus1;
        params.column_width_minus1 = pps.column_width_minus1;
        params.row_height_minus1 = pps.row_height_minus1;
        params.diff_cu_qp_delta_depth = pps.diff_cu_qp_delta_depth;
        params.pps_beta_offset_div2 = pps.beta_offset_div2;
        params.pps_tc_offset_div2 = pps.tc_offset_div2;
        params.log2_parallel_merge_level_minus2 = pps.log2_parallel_merge_level_minus2;
        params.curr_pic_order_cnt_val = 0;
        params.status_report_feedback_number = feedback;
        let qmatrix = QmatrixHevc {
            lists0: [[16; 16]; 6],
            lists1: [[16; 64]; 6],
            lists2: [[16; 64]; 6],
            lists3: [[16; 64]; 2],
            dc2: [16; 6],
            dc3: [16; 2],
        };
        let nals: Vec<&[u8]> = picture
            .slices
            .iter()
            .map(|slice| slice.nal.as_slice())
            .collect();
        let (spans, bitstream) = start_coded_bitstream(&nals)?;
        let slices = spans
            .into_iter()
            .map(|(location, bytes)| SliceShortHevc {
                location,
                bytes,
                chopping: 0,
            })
            .collect();
        Ok(Self {
            params,
            qmatrix,
            slices,
            bitstream,
        })
    }

    /// Packed coding tool flags.
    fn tool_flags(sps: &crate::hevc::Sps, pps: &crate::hevc::Pps) -> u32 {
        let mut flags = 0;
        if sps.amp() {
            flags |= HEVC_TOOL_AMP;
        }
        if sps.sao() {
            flags |= HEVC_TOOL_SAO;
        }
        if sps.temporal_mvp() {
            flags |= HEVC_TOOL_TEMPORAL_MVP;
        }
        if sps.strong_intra_smoothing() {
            flags |= HEVC_TOOL_STRONG_INTRA;
        }
        if pps.dependent_slices() {
            flags |= HEVC_TOOL_DEPENDENT_SLICES;
        }
        if pps.output_flag_present() {
            flags |= HEVC_TOOL_OUTPUT_FLAG;
        }
        flags |= u32::from(pps.num_extra_slice_header_bits) << HEVC_TOOL_EXTRA_BITS_SHIFT;
        if pps.sign_data_hiding() {
            flags |= HEVC_TOOL_SIGN_HIDING;
        }
        if pps.cabac_init_present() {
            flags |= HEVC_TOOL_CABAC_INIT;
        }
        flags
    }

    /// Packed picture property flags.
    fn property_flags(pps: &crate::hevc::Pps) -> u32 {
        let mut flags = HEVC_IRAP_PIC | HEVC_IDR_PIC | HEVC_INTRA_PIC;
        if pps.constrained_intra() {
            flags |= HEVC_PROP_CONSTRAINED_INTRA;
        }
        if pps.transform_skip() {
            flags |= HEVC_PROP_TRANSFORM_SKIP;
        }
        if pps.cu_qp_delta() {
            flags |= HEVC_PROP_CU_QP_DELTA;
        }
        if pps.slice_chroma_qp_offsets_present() {
            flags |= HEVC_PROP_CHROMA_QP_OFFSETS;
        }
        if pps.weighted_pred() {
            flags |= HEVC_PROP_WEIGHTED_PRED;
        }
        if pps.weighted_bipred() {
            flags |= HEVC_PROP_WEIGHTED_BIPRED;
        }
        if pps.transquant_bypass() {
            flags |= HEVC_PROP_TRANSQUANT_BYPASS;
        }
        if pps.tiles() {
            flags |= HEVC_PROP_TILES;
        }
        if pps.entropy_sync() {
            flags |= HEVC_PROP_ENTROPY_SYNC;
        }
        if pps.uniform_spacing() {
            flags |= HEVC_PROP_UNIFORM_SPACING;
        }
        if pps.loop_across_tiles() {
            flags |= HEVC_PROP_LOOP_TILES;
        }
        if pps.loop_across_slices() {
            flags |= HEVC_PROP_LOOP_SLICES;
        }
        if pps.deblocking_override() {
            flags |= HEVC_PROP_DEBLOCK_OVERRIDE;
        }
        if pps.deblocking_disabled() {
            flags |= HEVC_PROP_DEBLOCK_DISABLED;
        }
        if pps.lists_modification_present() {
            flags |= HEVC_PROP_LISTS_MOD;
        }
        if pps.slice_ext_present() {
            flags |= HEVC_PROP_SLICE_EXT;
        }
        flags
    }
}

/// Picture arguments behind one session submission.
pub(crate) trait PictureSubmission {
    /// NV12 bytes decoded from the arguments on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, past the bitstream buffer, or when the
    /// driver refuses the picture.
    fn submit(&self, session: &mut DecodeSession) -> Result<Vec<u8>, D3d12Error>;

    /// Output texture behind the arguments decode, without readback.
    ///
    /// # Errors
    ///
    /// Returns `Err` past the bitstream buffer or when the driver refuses
    /// the picture.
    #[cfg(target_os = "windows")]
    fn submit_texture(&self, session: &mut DecodeSession) -> Result<ID3D12Resource, D3d12Error>;
}

impl PictureSubmission for HevcSubmission {
    fn submit(&self, session: &mut DecodeSession) -> Result<Vec<u8>, D3d12Error> {
        session.decode_hevc(self)
    }

    #[cfg(target_os = "windows")]
    fn submit_texture(&self, session: &mut DecodeSession) -> Result<ID3D12Resource, D3d12Error> {
        session.decode_texture_hevc(self)
    }
}

/// Pool texture tables behind one decode call.
#[cfg(target_os = "windows")]
struct RefTables {
    /// Pool textures in slot order.
    textures: [Option<ID3D12Resource>; POOL],
    /// Subresource per pool texture.
    subs: [u32; POOL],
}

/// Visible extent plus per-plane source boxes under a crop.
#[cfg(target_os = "windows")]
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

/// Crop copy plan for `crop` in `width` by `height`, absent when uncropped.
#[cfg(target_os = "windows")]
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

/// D3D12 video decode session over one codec geometry.
pub(crate) struct DecodeSession {
    /// Video decode queue.
    #[cfg(target_os = "windows")]
    video_queue: ID3D12CommandQueue,
    /// Video decode allocator.
    #[cfg(target_os = "windows")]
    video_alloc: ID3D12CommandAllocator,
    /// Video decode list.
    #[cfg(target_os = "windows")]
    video_list: ID3D12VideoDecodeCommandList,
    /// Direct queue behind downloads.
    #[cfg(target_os = "windows")]
    copy_queue: ID3D12CommandQueue,
    /// Direct allocator behind downloads.
    #[cfg(target_os = "windows")]
    copy_alloc: ID3D12CommandAllocator,
    /// Direct list behind downloads.
    #[cfg(target_os = "windows")]
    copy_list: ID3D12GraphicsCommandList,
    /// Fence behind both queues.
    #[cfg(target_os = "windows")]
    fence: Fence,
    /// Decoder heap behind pictures.
    #[cfg(target_os = "windows")]
    heap: ID3D12VideoDecoderHeap,
    /// Decoder behind pictures.
    #[cfg(target_os = "windows")]
    decoder: ID3D12VideoDecoder,
    /// NV12 output texture pool, one per DPB slot.
    #[cfg(target_os = "windows")]
    outputs: Vec<ID3D12Resource>,
    /// Visible-size crop targets, one per pool slot, empty when uncropped.
    #[cfg(target_os = "windows")]
    crops: Vec<ID3D12Resource>,
    /// Bitstream upload buffer.
    #[cfg(target_os = "windows")]
    bitstream: ID3D12Resource,
    /// Download staging buffer.
    #[cfg(target_os = "windows")]
    staging: ID3D12Resource,
    /// Staging layout.
    #[cfg(target_os = "windows")]
    layout: Nv12Layout,
    /// Driver footprints behind staging copies.
    #[cfg(target_os = "windows")]
    footprints: [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; 2],
    /// Coded width in luma samples.
    #[cfg(target_os = "windows")]
    width: u32,
    /// Coded height in luma samples.
    #[cfg(target_os = "windows")]
    height: u32,
    /// Visible rectangle, absent when uncropped.
    #[cfg(target_os = "windows")]
    crop: Option<Crop>,
}

impl DecodeSession {
    /// Session over `device` for `codec` at `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, without a `codec` profile, past the
    /// adapter decode caps, or when the driver refuses the session.
    pub(crate) fn open(
        device: &crate::device::D3d12Device,
        codec: crate::guid::Codec,
        width: u32,
        height: u32,
        crop: Option<Crop>,
    ) -> Result<Self, D3d12Error> {
        #[cfg(target_os = "windows")]
        {
            Self::open_windows(device, codec, width, height, crop)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device, codec, width, height, crop);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Session over `device` for `codec` at `width` by `height`.
    #[cfg(target_os = "windows")]
    fn open_windows(
        device: &crate::device::D3d12Device,
        codec: crate::guid::Codec,
        width: u32,
        height: u32,
        crop: Option<Crop>,
    ) -> Result<Self, D3d12Error> {
        if codec != crate::guid::Codec::H264 && codec != crate::guid::Codec::Hevc {
            return Err(D3d12Error::Unsupported(format!(
                "{codec} picture args pending"
            )));
        }
        let profile = crate::device::decode_profile(codec)
            .ok_or_else(|| D3d12Error::Unsupported(format!("no {codec} decode profile")))?;
        crate::device::check_decode_support(device.video(), profile, width, height)?;
        let resources = DeviceResources::new(device.device());
        let heap = crate::device::create_decoder_heap(device.video(), profile, width, height)?;
        let decoder = crate::device::create_decoder(device.video(), profile)?;
        let video_queue = resources.queue(D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE)?;
        let video_alloc = resources.allocator(D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE)?;
        let video_list: ID3D12VideoDecodeCommandList =
            resources.command_list(D3D12_COMMAND_LIST_TYPE_VIDEO_DECODE, &video_alloc)?;
        // Sound: the fresh list is open with no recorded work.
        unsafe { video_list.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })?;
        let copy_queue = resources.queue(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let copy_alloc = resources.allocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let copy_list: ID3D12GraphicsCommandList =
            resources.command_list(D3D12_COMMAND_LIST_TYPE_DIRECT, &copy_alloc)?;
        // Sound: the fresh list is open with no recorded work.
        unsafe { copy_list.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })?;
        let fence = Fence::open(device.device())?;
        let texture_desc = DeviceResources::texture_desc(width, height);
        let mut outputs = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            outputs.push(resources.committed(
                D3D12_HEAP_TYPE_DEFAULT,
                &texture_desc,
                D3D12_RESOURCE_STATE_COMMON,
                "CreateOutput",
            )?);
        }
        let mut crops = Vec::new();
        if let Some(plan) = crop_plan(width, height, crop) {
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
        let bitstream = resources.committed(
            D3D12_HEAP_TYPE_UPLOAD,
            &upload_desc,
            D3D12_RESOURCE_STATE_GENERIC_READ,
            "CreateBitstream",
        )?;
        let (layout, footprints) = resources.nv12_layout(width, height)?;
        let staging_desc = DeviceResources::buffer_desc(layout.total);
        let staging = resources.committed(
            D3D12_HEAP_TYPE_READBACK,
            &staging_desc,
            D3D12_RESOURCE_STATE_COPY_DEST,
            "CreateStaging",
        )?;
        Ok(Self {
            video_queue,
            video_alloc,
            video_list,
            copy_queue,
            copy_alloc,
            copy_list,
            fence,
            heap,
            decoder,
            outputs,
            crops,
            bitstream,
            staging,
            layout,
            footprints,
            width,
            height,
            crop,
        })
    }

    /// Decode `submission` onto its pool slot.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, past the bitstream buffer, or when the
    /// driver refuses the picture.
    pub(crate) fn submit(&mut self, submission: &H264Submission) -> Result<(), D3d12Error> {
        #[cfg(target_os = "windows")]
        {
            self.submit_h264(submission)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (
                self,
                &submission.params,
                &submission.qmatrix,
                &submission.slices,
                &submission.bitstream,
                submission.slot,
            );
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// NV12 bytes for pool `slot` cropped to the visible rectangle.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, for a slot outside the pool, or when
    /// the copy fails.
    pub(crate) fn download(&mut self, slot: u8) -> Result<Vec<u8>, D3d12Error> {
        #[cfg(target_os = "windows")]
        {
            self.download_windows(slot)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self, slot);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Pool texture behind `slot`, without readback.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool.
    #[cfg(target_os = "windows")]
    pub(crate) fn texture(&self, slot: u8) -> Result<ID3D12Resource, D3d12Error> {
        self.outputs
            .get(usize::from(slot))
            .cloned()
            .ok_or_else(|| D3d12Error::BrokenReference("slot outside pool".to_owned()))
    }

    /// Pool `slot` cropped to the visible rectangle on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the pool or when the copy fails.
    #[cfg(target_os = "windows")]
    pub(crate) fn texture_cropped(&mut self, slot: u8) -> Result<ID3D12Resource, D3d12Error> {
        if self.crops.is_empty() {
            return self.texture(slot);
        }
        self.submit_crop(slot)?;
        self.crops
            .get(usize::from(slot))
            .cloned()
            .ok_or_else(|| D3d12Error::BrokenReference("slot outside pool".to_owned()))
    }

    /// NV12 bytes decoded from `submission` on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, past the bitstream buffer, or when the
    /// driver refuses the picture.
    pub(crate) fn decode_hevc(
        &mut self,
        submission: &HevcSubmission,
    ) -> Result<Vec<u8>, D3d12Error> {
        #[cfg(target_os = "windows")]
        {
            self.decode_hevc_windows(submission)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (submission, self);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// `submission` through the video queue into its pool slot.
    #[cfg(target_os = "windows")]
    fn submit_h264(&mut self, submission: &H264Submission) -> Result<(), D3d12Error> {
        Self::check_bitstream_len(submission.bitstream.len())?;
        DeviceResources::write_upload(&self.bitstream, &submission.bitstream)?;
        let mut refs = self.ref_tables();
        let input = self.input_args(submission, &mut refs)?;
        self.submit_video(&input, submission.slot)
    }

    /// NV12 bytes decoded from `submission` on the GPU.
    #[cfg(target_os = "windows")]
    fn decode_hevc_windows(&mut self, submission: &HevcSubmission) -> Result<Vec<u8>, D3d12Error> {
        self.submit_hevc(submission)?;
        self.download(0)
    }

    /// Output texture behind `submission` decode, without readback.
    #[cfg(target_os = "windows")]
    pub(crate) fn decode_texture_hevc(
        &mut self,
        submission: &HevcSubmission,
    ) -> Result<ID3D12Resource, D3d12Error> {
        self.submit_hevc(submission)?;
        self.texture_cropped(0)
    }

    /// `submission` through the video queue into pool slot zero.
    #[cfg(target_os = "windows")]
    fn submit_hevc(&mut self, submission: &HevcSubmission) -> Result<(), D3d12Error> {
        Self::check_bitstream_len(submission.bitstream.len())?;
        DeviceResources::write_upload(&self.bitstream, &submission.bitstream)?;
        let input = self.input_args_hevc(submission)?;
        self.submit_video(&input, 0)
    }

    /// Bitstream length against the upload capacity.
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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

    /// Pool `slot` through staging into tight NV12 bytes.
    #[cfg(target_os = "windows")]
    fn download_windows(&mut self, slot: u8) -> Result<Vec<u8>, D3d12Error> {
        self.submit_copy(slot)?;
        let total = usize::try_from(self.layout.total)
            .unwrap_or(usize::MAX)
            .min(isize::MAX as usize);
        let staged = DeviceResources::read_readback(&self.staging, total)?;
        self.layout
            .tighten(&staged, self.width, self.height, self.crop)
    }

    /// `DecodeFrame` for `input` through the video queue into pool `slot`.
    #[cfg(target_os = "windows")]
    fn submit_video(
        &mut self,
        input: &D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS,
        slot: u8,
    ) -> Result<(), D3d12Error> {
        let target = self
            .outputs
            .get(usize::from(slot))
            .ok_or_else(|| D3d12Error::BrokenReference("slot outside pool".to_owned()))?
            .clone();
        // Sound: the queue is idle; reset reuses the allocator and list.
        unsafe { self.video_alloc.Reset() }.map_err(|error| D3d12Error::Call {
            call: "ResetAllocator",
            hresult: error.code().0,
        })?;
        // Sound: the allocator outlives the reset.
        unsafe { self.video_list.Reset(&self.video_alloc) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })?;
        let output = D3D12_VIDEO_DECODE_OUTPUT_STREAM_ARGUMENTS {
            pOutputTexture2D: ManuallyDrop::new(Some(target.clone())),
            OutputSubresource: 0,
            ConversionArguments: D3D12_VIDEO_DECODE_CONVERSION_ARGUMENTS::default(),
        };
        let enter: Vec<_> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(index, texture)| {
                let after = if u8::try_from(index).is_ok_and(|at| at == slot) {
                    D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE
                } else {
                    D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
                };
                DeviceResources::barrier(texture, D3D12_RESOURCE_STATE_COMMON, after)
            })
            .collect();
        let leave: Vec<_> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(index, texture)| {
                let before = if u8::try_from(index).is_ok_and(|at| at == slot) {
                    D3D12_RESOURCE_STATE_VIDEO_DECODE_WRITE
                } else {
                    D3D12_RESOURCE_STATE_VIDEO_DECODE_READ
                };
                DeviceResources::barrier(texture, before, D3D12_RESOURCE_STATE_COMMON)
            })
            .collect();
        // Sound: the barriers name live textures; the list is open.
        // Sound: the argument structs outlive the fence wait below.
        unsafe {
            self.video_list.ResourceBarrier(&enter);
            self.video_list.DecodeFrame(
                &self.decoder,
                std::ptr::from_ref(&output),
                std::ptr::from_ref(input),
            );
            self.video_list.ResourceBarrier(&leave);
        }
        // Sound: the list holds only the barriers and decode above.
        unsafe { self.video_list.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })?;
        // Sound: the list is closed; the cast holds the live list.
        let command: ID3D12CommandList =
            self.video_list.cast().map_err(|error| D3d12Error::Call {
                call: "CastCommandList",
                hresult: error.code().0,
            })?;
        self.fence.submit(&self.video_queue, &command)?;
        Ok(())
    }

    /// Decode input arguments borrowing `submission` and `refs`.
    #[cfg(target_os = "windows")]
    fn input_args(
        &self,
        submission: &H264Submission,
        refs: &mut RefTables,
    ) -> Result<D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS, D3d12Error> {
        let params_len = u32::try_from(std::mem::size_of::<PicParamsH264>())
            .map_err(|_| D3d12Error::Unsupported("picture args too large".to_owned()))?;
        let matrix_len = u32::try_from(std::mem::size_of::<QmatrixH264>())
            .map_err(|_| D3d12Error::Unsupported("picture args too large".to_owned()))?;
        let slices_len = u32::try_from(
            std::mem::size_of::<SliceShortH264>().saturating_mul(submission.slices.len()),
        )
        .map_err(|_| D3d12Error::Unsupported("picture args too large".to_owned()))?;
        let bitstream_len = u64::try_from(submission.bitstream.len())
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let mut args = [D3D12_VIDEO_DECODE_FRAME_ARGUMENT::default(); 10];
        args[0] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
            Size: params_len,
            pData: std::ptr::from_ref(&submission.params)
                .cast_mut()
                .cast::<c_void>(),
        };
        args[1] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
            Size: matrix_len,
            pData: std::ptr::from_ref(&submission.qmatrix)
                .cast_mut()
                .cast::<c_void>(),
        };
        args[2] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
            Size: slices_len,
            pData: submission.slices.as_ptr().cast_mut().cast::<c_void>(),
        };
        let count = u32::try_from(POOL).unwrap_or(u32::MAX);
        Ok(D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS {
            NumFrameArguments: 3,
            FrameArguments: args,
            ReferenceFrames: D3D12_VIDEO_DECODE_REFERENCE_FRAMES {
                NumTexture2Ds: count,
                ppTexture2Ds: refs.textures.as_mut_ptr(),
                pSubresources: refs.subs.as_mut_ptr(),
                ppHeaps: std::ptr::null_mut(),
            },
            CompressedBitstream: D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM {
                pBuffer: ManuallyDrop::new(Some(self.bitstream.clone())),
                Offset: 0,
                Size: bitstream_len,
            },
            pHeap: ManuallyDrop::new(Some(self.heap.clone())),
        })
    }

    /// Decode input arguments borrowing `submission`.
    #[cfg(target_os = "windows")]
    fn input_args_hevc(
        &self,
        submission: &HevcSubmission,
    ) -> Result<D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS, D3d12Error> {
        let params_len = u32::try_from(std::mem::size_of::<PicParamsHevc>())
            .map_err(|_| D3d12Error::Unsupported("picture args too large".to_owned()))?;
        let matrix_len = u32::try_from(std::mem::size_of::<QmatrixHevc>())
            .map_err(|_| D3d12Error::Unsupported("picture args too large".to_owned()))?;
        let slices_len = u32::try_from(
            std::mem::size_of::<SliceShortHevc>().saturating_mul(submission.slices.len()),
        )
        .map_err(|_| D3d12Error::Unsupported("picture args too large".to_owned()))?;
        let bitstream_len = u64::try_from(submission.bitstream.len())
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let mut args = [D3D12_VIDEO_DECODE_FRAME_ARGUMENT::default(); 10];
        args[0] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_PICTURE_PARAMETERS,
            Size: params_len,
            pData: std::ptr::from_ref(&submission.params)
                .cast_mut()
                .cast::<c_void>(),
        };
        args[1] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_INVERSE_QUANTIZATION_MATRIX,
            Size: matrix_len,
            pData: std::ptr::from_ref(&submission.qmatrix)
                .cast_mut()
                .cast::<c_void>(),
        };
        args[2] = D3D12_VIDEO_DECODE_FRAME_ARGUMENT {
            Type: D3D12_VIDEO_DECODE_ARGUMENT_TYPE_SLICE_CONTROL,
            Size: slices_len,
            pData: submission.slices.as_ptr().cast_mut().cast::<c_void>(),
        };
        Ok(D3D12_VIDEO_DECODE_INPUT_STREAM_ARGUMENTS {
            NumFrameArguments: 3,
            FrameArguments: args,
            ReferenceFrames: D3D12_VIDEO_DECODE_REFERENCE_FRAMES::default(),
            CompressedBitstream: D3D12_VIDEO_DECODE_COMPRESSED_BITSTREAM {
                pBuffer: ManuallyDrop::new(Some(self.bitstream.clone())),
                Offset: 0,
                Size: bitstream_len,
            },
            pHeap: ManuallyDrop::new(Some(self.heap.clone())),
        })
    }

    /// Copy allocator plus list reset for one submission.
    #[cfg(target_os = "windows")]
    fn reset_copy(&mut self) -> Result<(), D3d12Error> {
        // Sound: the queue is idle; reset reuses the allocator and list.
        unsafe { self.copy_alloc.Reset() }.map_err(|error| D3d12Error::Call {
            call: "ResetAllocator",
            hresult: error.code().0,
        })?;
        // Sound: the allocator outlives the reset; no initial pipeline.
        unsafe { self.copy_list.Reset(&self.copy_alloc, None) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })
    }

    /// Closed copy list through the direct queue.
    #[cfg(target_os = "windows")]
    fn close_copy(&mut self) -> Result<(), D3d12Error> {
        // Sound: the list holds the caller's barriers and copies.
        unsafe { self.copy_list.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })?;
        // Sound: the list is closed; the cast holds the live list.
        let command: ID3D12CommandList =
            self.copy_list.cast().map_err(|error| D3d12Error::Call {
                call: "CastCommandList",
                hresult: error.code().0,
            })?;
        self.fence.submit(&self.copy_queue, &command)
    }

    /// Pool `slot` planes through the direct queue into staging.
    #[cfg(target_os = "windows")]
    fn submit_copy(&mut self, slot: u8) -> Result<(), D3d12Error> {
        let source = self
            .outputs
            .get(usize::from(slot))
            .ok_or_else(|| D3d12Error::BrokenReference("slot outside pool".to_owned()))?
            .clone();
        self.reset_copy()?;
        // Sound: the barriers name a live texture; the list is open.
        unsafe {
            self.copy_list.ResourceBarrier(&[DeviceResources::barrier(
                &source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
        }
        // Sound: the copy locations name live resources; the list is open.
        unsafe {
            for (index, placed) in [0u32, 1u32].into_iter().zip(self.footprints.iter()) {
                let dst = DeviceResources::footprint_location(&self.staging, placed);
                let src = DeviceResources::subresource_location(&source, index);
                self.copy_list.CopyTextureRegion(
                    std::ptr::from_ref(&dst),
                    0,
                    0,
                    0,
                    std::ptr::from_ref(&src),
                    None,
                );
            }
            self.copy_list.ResourceBarrier(&[DeviceResources::barrier(
                &source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        self.close_copy()
    }

    /// Pool `slot` visible rectangle through the direct queue into its crop target.
    #[cfg(target_os = "windows")]
    fn submit_crop(&mut self, slot: u8) -> Result<(), D3d12Error> {
        let source = self
            .outputs
            .get(usize::from(slot))
            .ok_or_else(|| D3d12Error::BrokenReference("slot outside pool".to_owned()))?
            .clone();
        let target = self
            .crops
            .get(usize::from(slot))
            .ok_or_else(|| D3d12Error::BrokenReference("slot outside pool".to_owned()))?
            .clone();
        let plan = crop_plan(self.width, self.height, self.crop)
            .ok_or_else(|| D3d12Error::BrokenReference("crop target without crop".to_owned()))?;
        self.reset_copy()?;
        // Sound: the barriers name live textures; the list is open.
        unsafe {
            self.copy_list.ResourceBarrier(&[
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
            for (index, src_box) in [0u32, 1u32].into_iter().zip(boxes.iter()) {
                let dst = DeviceResources::subresource_location(&target, index);
                let src = DeviceResources::subresource_location(&source, index);
                self.copy_list.CopyTextureRegion(
                    std::ptr::from_ref(&dst),
                    0,
                    0,
                    0,
                    std::ptr::from_ref(&src),
                    Some(std::ptr::from_ref(src_box)),
                );
            }
            self.copy_list.ResourceBarrier(&[
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
        self.close_copy()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use crate::h264::Crop;
    use crate::h264::{parse_pps, parse_sps, Ingest};

    #[cfg(target_os = "windows")]
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

    #[cfg(target_os = "windows")]
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

    #[cfg(target_os = "windows")]
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

    #[cfg(target_os = "windows")]
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

    #[test]
    fn submission_carries_params_and_start_codes() {
        let data = packet(&[
            nal(7, 3, &sps_body()),
            nal(8, 3, &pps_body()),
            nal(5, 3, &idr_body()),
        ]);
        let pictures = ingest(&data);
        assert_eq!(pictures.len(), 1);
        let sps = parse_sps(&crate::h264::rbsp(&nal(7, 3, &sps_body()))[1..]).expect("sps");
        let pps = parse_pps(&crate::h264::rbsp(&nal(8, 3, &pps_body()))[1..], &sps).expect("pps");
        let mut dpb = crate::dpb::Dpb::new(&sps);
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
        let sps = parse_sps(&crate::h264::rbsp(&sps_nal)[1..]).expect("sps");
        let pps = parse_pps(&crate::h264::rbsp(&pps_nal)[1..], &sps).expect("pps");
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
        let mut dpb = crate::dpb::Dpb::new(&sps);
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
        let sps = parse_sps(&crate::h264::rbsp(&sps_nal)[1..]).expect("sps");
        let pps = parse_pps(&crate::h264::rbsp(&pps_nal)[1..], &sps).expect("pps");
        assert_eq!((sps.visible_width(), sps.visible_height()), (320, 240));
        assert!(pps.transform_8x8());
        assert!(!pps.entropy());
        assert_eq!(pps.pic_init_qp, -3);
        assert_eq!(pps.second_chroma_qp, 0);
        let mut dpb = crate::dpb::Dpb::new(&sps);
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
        let sps = parse_sps(&crate::h264::rbsp(&nal(7, 3, &sps_body()))[1..]).expect("sps");
        let pps = parse_pps(&crate::h264::rbsp(&nal(8, 3, &pps_body()))[1..], &sps).expect("pps");
        let mut dpb = crate::dpb::Dpb::new(&sps);
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

    #[test]
    fn hevc_submission_carries_params_and_start_codes() {
        use super::HevcSubmission;
        use crate::hevc::fixtures::{idr_body, nal, p_body, packet, pps_body, sps_body};
        use crate::hevc::{
            parse_pps as parse_hevc_pps, parse_sps as parse_hevc_sps, Ingest as HevcIngest,
        };

        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
        ]);
        let mut ingest = HevcIngest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 1);
        let sps = parse_hevc_sps(&crate::hevc::rbsp(&nal(33, &sps_body()))[2..]).expect("sps");
        let pps =
            parse_hevc_pps(&crate::hevc::rbsp(&nal(34, &pps_body()))[2..], &sps).expect("pps");
        let submission = HevcSubmission::build(&sps, &pps, &pictures[0], 7).expect("submission");
        assert_eq!(submission.params.pic_width_in_min_cbs_y, 8);
        assert_eq!(submission.params.pic_height_in_min_cbs_y, 6);
        assert_eq!(submission.params.status_report_feedback_number, 7);
        assert_eq!(submission.params.curr_pic_order_cnt_val, 0);
        assert_eq!(submission.slices.len(), 1);
        assert_eq!(submission.slices[0].location, 0);
        assert_eq!(&submission.bitstream[..5], &[0, 0, 1, 0x26, 0x01]);
        assert_eq!(submission.bitstream.len() % 128, 0);

        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(1, &p_body()),
        ]);
        let mut ingest = HevcIngest::new();
        assert!(ingest.ingest_packet(&data).is_err());
    }

    #[test]
    fn hevc_submission_rejects_inter_and_empty() {
        use super::HevcSubmission;
        use crate::hevc::fixtures::{nal, pps_body, sps_body};
        use crate::hevc::{
            parse_pps as parse_hevc_pps, parse_sps as parse_hevc_sps, PendingSlice,
            Picture as HevcPicture, SliceHeader, SliceType,
        };

        let sps = parse_hevc_sps(&crate::hevc::rbsp(&nal(33, &sps_body()))[2..]).expect("sps");
        let pps =
            parse_hevc_pps(&crate::hevc::rbsp(&nal(34, &pps_body()))[2..], &sps).expect("pps");
        let inter = HevcPicture {
            slices: vec![PendingSlice {
                header: SliceHeader {
                    pps_id: 0,
                    slice_type: SliceType::P,
                    address: 0,
                    first: true,
                },
                nal: Vec::new(),
                idr: false,
            }],
            idr: false,
        };
        assert!(HevcSubmission::build(&sps, &pps, &inter, 1).is_err());
        let empty = HevcPicture {
            slices: Vec::new(),
            idr: true,
        };
        assert!(HevcSubmission::build(&sps, &pps, &empty, 1).is_err());
    }
}
