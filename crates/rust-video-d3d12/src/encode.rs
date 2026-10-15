//! D3D12 video encode: headers plus GPU submission.
//!
//! Intra-only H.264 and HEVC; reference pictures stay pending.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::mem::ManuallyDrop;

use crate::error::D3d12Error;
#[cfg(not(target_os = "windows"))]
use crate::error::SkipReason;
#[cfg(target_os = "windows")]
use crate::submit::{DeviceResources, Fence, Nv12Layout};

#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12GraphicsCommandList,
    ID3D12Resource, D3D12_BOX, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_READBACK,
    D3D12_HEAP_TYPE_UPLOAD, D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RESOURCE_STATES,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL};
#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_32x32,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_8x8,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_32x32,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_4x4, ID3D12VideoDevice, ID3D12VideoDevice3,
    ID3D12VideoEncodeCommandList2, ID3D12VideoEncoder, ID3D12VideoEncoderHeap,
    D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
    D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_ENABLE_CABAC_ENCODING,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_NONE, D3D12_VIDEO_ENCODER_CODEC_H264,
    D3D12_VIDEO_ENCODER_CODEC_HEVC, D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM,
    D3D12_VIDEO_ENCODER_DESC, D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER, D3D12_VIDEO_ENCODER_FLAGS,
    D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
    D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME, D3D12_VIDEO_ENCODER_FRAME_TYPE_HEVC_IDR_FRAME,
    D3D12_VIDEO_ENCODER_HEAP_DESC, D3D12_VIDEO_ENCODER_HEAP_FLAGS,
    D3D12_VIDEO_ENCODER_INTRA_REFRESH, D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
    D3D12_VIDEO_ENCODER_LEVELS_H264, D3D12_VIDEO_ENCODER_LEVELS_H264_1,
    D3D12_VIDEO_ENCODER_LEVELS_H264_11, D3D12_VIDEO_ENCODER_LEVELS_H264_12,
    D3D12_VIDEO_ENCODER_LEVELS_H264_13, D3D12_VIDEO_ENCODER_LEVELS_H264_2,
    D3D12_VIDEO_ENCODER_LEVELS_H264_21, D3D12_VIDEO_ENCODER_LEVELS_H264_22,
    D3D12_VIDEO_ENCODER_LEVELS_H264_3, D3D12_VIDEO_ENCODER_LEVELS_H264_31,
    D3D12_VIDEO_ENCODER_LEVELS_H264_32, D3D12_VIDEO_ENCODER_LEVELS_H264_4,
    D3D12_VIDEO_ENCODER_LEVELS_H264_41, D3D12_VIDEO_ENCODER_LEVELS_H264_42,
    D3D12_VIDEO_ENCODER_LEVELS_H264_5, D3D12_VIDEO_ENCODER_LEVELS_H264_51,
    D3D12_VIDEO_ENCODER_LEVELS_H264_52, D3D12_VIDEO_ENCODER_LEVELS_HEVC,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_1, D3D12_VIDEO_ENCODER_LEVELS_HEVC_2,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_21, D3D12_VIDEO_ENCODER_LEVELS_HEVC_3,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_31, D3D12_VIDEO_ENCODER_LEVELS_HEVC_4,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_41, D3D12_VIDEO_ENCODER_LEVELS_HEVC_5,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_51, D3D12_VIDEO_ENCODER_LEVELS_HEVC_52,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_6, D3D12_VIDEO_ENCODER_LEVELS_HEVC_61,
    D3D12_VIDEO_ENCODER_LEVELS_HEVC_62, D3D12_VIDEO_ENCODER_LEVEL_SETTING,
    D3D12_VIDEO_ENCODER_LEVEL_SETTING_0, D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC,
    D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_FULL_PIXEL,
    D3D12_VIDEO_ENCODER_OUTPUT_METADATA, D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC_FLAG_NONE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA,
    D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC, D3D12_VIDEO_ENCODER_PROFILE_DESC,
    D3D12_VIDEO_ENCODER_PROFILE_DESC_0, D3D12_VIDEO_ENCODER_PROFILE_H264,
    D3D12_VIDEO_ENCODER_PROFILE_H264_HIGH, D3D12_VIDEO_ENCODER_PROFILE_HEVC,
    D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN, D3D12_VIDEO_ENCODER_RATE_CONTROL,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0, D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE, D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CQP,
    D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE,
    D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAGS,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE, D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC, D3D12_VIDEO_ENCODER_TIER_HEVC_MAIN,
    D3D12_VIDEO_ENCODE_REFERENCE_FRAMES,
};

/// Macroblock edge in luma samples.
const MB: u32 = 16;

/// Metadata staging fallback size in bytes.
const META_CAP_FALLBACK: u64 = 128 * 1024;

/// Constant quantizer behind intra frames.
const INTRA_QP: u32 = 26;

/// High profile idc.
const PROFILE_IDC: u8 = 100;

/// Level ceiling per macroblock count, H.264 Table A-1.
const LEVELS: &[(u32, u8)] = &[
    (99, 10),
    (396, 11),
    (792, 21),
    (1620, 22),
    (3600, 31),
    (5120, 32),
    (8192, 40),
    (8704, 42),
    (22080, 50),
    (36864, 52),
];

/// Level ceiling per luma sample count, H.265 Table A-1.
const LEVELS_HEVC: &[(u32, u8)] = &[
    (36864, 30),
    (122_880, 60),
    (245_760, 63),
    (552_960, 90),
    (983_040, 93),
    (2_228_224, 120),
    (8_912_896, 150),
    (35_651_584, 180),
];

/// Coded extent for `visible` on the macroblock grid.
fn pad_grid(visible: u32) -> Result<u32, D3d12Error> {
    visible
        .div_ceil(MB)
        .checked_mul(MB)
        .ok_or_else(|| D3d12Error::Unsupported("picture too large".to_owned()))
}

/// Right and bottom pad in luma samples under `visible` in `coded`.
fn pad_crop(
    coded: (u32, u32),
    visible: (u32, u32),
) -> Result<Option<crate::h264::Crop>, D3d12Error> {
    let right = coded.0.saturating_sub(visible.0);
    let bottom = coded.1.saturating_sub(visible.1);
    if right == 0 && bottom == 0 {
        return Ok(None);
    }
    if !right.is_multiple_of(2) || !bottom.is_multiple_of(2) {
        return Err(D3d12Error::Unsupported("odd visible extent".to_owned()));
    }
    Ok(Some(crate::h264::Crop {
        left: 0,
        right,
        top: 0,
        bottom,
    }))
}

/// Annex B sequence headers matching the encoder configuration.
pub(crate) struct H264Headers {
    /// Start-coded SPS plus PPS.
    bytes: Vec<u8>,
    /// Level idc behind the headers.
    level_idc: u8,
    /// Coded extent on the macroblock grid.
    coded: (u32, u32),
}

impl H264Headers {
    /// Headers for `width` by `height`, padded to the macroblock grid.
    ///
    /// A padded extent carries a frame crop rectangle over the visible
    /// size; the encoder session codes the padded grid.
    ///
    /// # Errors
    ///
    /// Returns `Err` for an odd visible extent or past level 5.2.
    pub(crate) fn build(width: u32, height: u32) -> Result<Self, D3d12Error> {
        let coded = (pad_grid(width)?, pad_grid(height)?);
        let crop = pad_crop(coded, (width, height))?;
        let mbs = coded
            .0
            .checked_div(MB)
            .and_then(|w| coded.1.checked_div(MB).and_then(|h| w.checked_mul(h)))
            .ok_or_else(|| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let level_idc = LEVELS.iter().find(|entry| mbs <= entry.0).map_or_else(
            || Err(D3d12Error::Unsupported("extent above level 5.2".to_owned())),
            |entry| Ok(entry.1),
        )?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x67]);
        bytes.extend_from_slice(&sps_body(
            coded.0 / MB - 1,
            coded.1 / MB - 1,
            level_idc,
            crop,
        ));
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x68]);
        bytes.extend_from_slice(&pps_body());
        Ok(Self {
            bytes,
            level_idc,
            coded,
        })
    }

    /// Level idc behind the headers.
    pub(crate) fn level_idc(&self) -> u8 {
        self.level_idc
    }

    /// Coded extent on the macroblock grid.
    pub(crate) fn coded(&self) -> (u32, u32) {
        self.coded
    }

    /// Start-coded header bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Sequence set body for the encoder configuration.
fn sps_body(
    width_mbs_minus1: u32,
    height_mbs_minus1: u32,
    level_idc: u8,
    crop: Option<crate::h264::Crop>,
) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.bits(u32::from(PROFILE_IDC), 8);
    writer.bits(0, 8);
    writer.bits(u32::from(level_idc), 8);
    writer.ue(0);
    writer.ue(1);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(1);
    writer.bit(false);
    writer.ue(width_mbs_minus1);
    writer.ue(height_mbs_minus1);
    writer.bit(true);
    writer.bit(true);
    if let Some(crop) = crop {
        writer.bit(true);
        writer.ue(0);
        writer.ue(crop.right / 2);
        writer.ue(0);
        writer.ue(crop.bottom / 2);
    } else {
        writer.bit(false);
    }
    writer.bit(false);
    writer.finish()
}

/// Annex B parameter sets matching the HEVC encoder configuration.
pub(crate) struct HevcHeaders {
    /// Start-coded VPS plus SPS plus PPS.
    bytes: Vec<u8>,
    /// Level idc behind the headers.
    level_idc: u8,
    /// Coded extent on the macroblock grid.
    coded: (u32, u32),
}

impl HevcHeaders {
    /// Headers for `width` by `height`, padded to the macroblock grid.
    ///
    /// Block sizes match the encoder configuration: 8 to 32 luma
    /// coding blocks, 4 to 32 luma transform blocks, depth one. A
    /// padded extent carries a conformance window over the visible
    /// size; the encoder session codes the padded grid.
    ///
    /// # Errors
    ///
    /// Returns `Err` for an odd visible extent or past level 6.2.
    pub(crate) fn build(width: u32, height: u32) -> Result<Self, D3d12Error> {
        let coded = (pad_grid(width)?, pad_grid(height)?);
        let crop = pad_crop(coded, (width, height))?;
        let samples = coded
            .0
            .checked_mul(coded.1)
            .ok_or_else(|| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let level_idc = LEVELS_HEVC
            .iter()
            .find(|entry| samples <= entry.0)
            .map_or_else(
                || Err(D3d12Error::Unsupported("extent above level 6.2".to_owned())),
                |entry| Ok(entry.1),
            )?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x40, 0x01]);
        bytes.extend_from_slice(&hevc_vps_body(level_idc));
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x42, 0x01]);
        bytes.extend_from_slice(&hevc_sps_body(coded.0, coded.1, level_idc, crop));
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x44, 0x01]);
        bytes.extend_from_slice(&hevc_pps_body());
        Ok(Self {
            bytes,
            level_idc,
            coded,
        })
    }

    /// Level idc behind the headers.
    pub(crate) fn level_idc(&self) -> u8 {
        self.level_idc
    }

    /// Coded extent on the macroblock grid.
    pub(crate) fn coded(&self) -> (u32, u32) {
        self.coded
    }

    /// Start-coded header bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Video set body for the encoder configuration.
fn hevc_vps_body(level_idc: u8) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.bits(0, 4);
    writer.bit(true);
    writer.bit(true);
    writer.bits(0, 6);
    writer.bits(0, 3);
    writer.bit(true);
    writer.bits(0xFFFF, 16);
    hevc_profile_tier(&mut writer, level_idc);
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bits(0, 6);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// Sequence set body for the encoder configuration.
fn hevc_sps_body(
    width: u32,
    height: u32,
    level_idc: u8,
    crop: Option<crate::h264::Crop>,
) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    hevc_profile_tier(&mut writer, level_idc);
    writer.ue(0);
    writer.ue(1);
    writer.ue(width);
    writer.ue(height);
    if let Some(crop) = crop {
        writer.bit(true);
        writer.ue(0);
        writer.ue(crop.right / 2);
        writer.ue(0);
        writer.ue(crop.bottom / 2);
    } else {
        writer.bit(false);
    }
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(2);
    writer.ue(0);
    writer.ue(3);
    writer.ue(1);
    writer.ue(1);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// General profile, tier, and level for the encoder configuration.
fn hevc_profile_tier(writer: &mut BitWriter, level_idc: u8) {
    writer.bits(0, 2);
    writer.bit(false);
    writer.bits(1, 5);
    writer.bits(0x4000_0000, 32);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bits(0, 32);
    writer.bits(0, 11);
    writer.bit(false);
    writer.bits(u32::from(level_idc), 8);
}

/// Picture set body for the encoder configuration.
fn hevc_pps_body() -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.se(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// Picture set body for the encoder configuration.
fn pps_body() -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
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
    writer.bit(true);
    writer.bit(false);
    writer.se(0);
    writer.finish()
}

/// Tight NV12 bytes padded from `visible` to `coded` by edge replication.
///
/// Both extents are even; the chroma plane halves the row counts.
///
/// # Errors
///
/// Returns `Err` when `visible` bytes fall short of the visible extent.
fn pad_nv12(
    visible: &[u8],
    visible_extent: (u32, u32),
    coded_extent: (u32, u32),
) -> Result<Vec<u8>, D3d12Error> {
    let (vis_w, vis_h) = (visible_extent.0 as usize, visible_extent.1 as usize);
    let (coded_w, coded_h) = (coded_extent.0 as usize, coded_extent.1 as usize);
    let y_len = vis_w.saturating_mul(vis_h);
    if visible.len() < y_len + y_len / 2 {
        return Err(D3d12Error::Unsupported(
            "nv12 short of its extent".to_owned(),
        ));
    }
    let mut out = Vec::with_capacity(coded_w * coded_h * 3 / 2);
    pad_plane(&visible[..y_len], vis_w, vis_h, coded_w, coded_h, &mut out);
    pad_plane(
        &visible[y_len..y_len + y_len / 2],
        vis_w,
        vis_h / 2,
        coded_w,
        coded_h / 2,
        &mut out,
    );
    Ok(out)
}

/// `rows` of `width` bytes padded to `coded` geometry by edge replication.
fn pad_plane(
    plane: &[u8],
    width: usize,
    rows: usize,
    coded_width: usize,
    coded_rows: usize,
    out: &mut Vec<u8>,
) {
    for row in 0..coded_rows {
        let start = row
            .min(rows.saturating_sub(1))
            .saturating_mul(width)
            .min(plane.len());
        let end = start.saturating_add(width).min(plane.len());
        let src = &plane[start..end];
        let edge = src.last().copied().unwrap_or(0);
        out.extend_from_slice(src);
        out.resize(out.len() + coded_width.saturating_sub(width), edge);
    }
}

/// Bit writer with emulation prevention.
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

/// Input, bitstream, and metadata buffers behind encode.
#[cfg(target_os = "windows")]
struct Buffers {
    /// NV12 input texture.
    input: ID3D12Resource,
    /// Reconstructed picture texture.
    recon: ID3D12Resource,
    /// Input upload buffer.
    upload: ID3D12Resource,
    /// Staging layout.
    layout: Nv12Layout,
    /// Driver footprints behind staging copies.
    footprints: [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; 2],
    /// Bitstream encoder buffer.
    bitstream: ID3D12Resource,
    /// Bitstream staging buffer.
    staging: ID3D12Resource,
    /// Bitstream capacity in bytes.
    bitstream_cap: u64,
    /// Encoder metadata buffer.
    meta: ID3D12Resource,
    /// Resolved metadata buffer.
    resolved: ID3D12Resource,
    /// Resolved staging buffer.
    resolved_staging: ID3D12Resource,
}

/// D3D12 video encode session over one codec geometry.
pub(crate) struct EncodeSession {
    /// Video encode queue.
    #[cfg(target_os = "windows")]
    video_queue: ID3D12CommandQueue,
    /// Video encode allocator.
    #[cfg(target_os = "windows")]
    video_alloc: ID3D12CommandAllocator,
    /// Video encode list.
    #[cfg(target_os = "windows")]
    video_list: ID3D12VideoEncodeCommandList2,
    /// Direct queue behind uploads.
    #[cfg(target_os = "windows")]
    copy_queue: ID3D12CommandQueue,
    /// Direct allocator behind uploads.
    #[cfg(target_os = "windows")]
    copy_alloc: ID3D12CommandAllocator,
    /// Direct list behind uploads.
    #[cfg(target_os = "windows")]
    copy_list: ID3D12GraphicsCommandList,
    /// Fence behind both queues.
    #[cfg(target_os = "windows")]
    fence: Fence,
    /// Encoder behind pictures.
    #[cfg(target_os = "windows")]
    encoder: ID3D12VideoEncoder,
    /// Encoder heap behind pictures.
    #[cfg(target_os = "windows")]
    heap: ID3D12VideoEncoderHeap,
    /// NV12 input texture.
    #[cfg(target_os = "windows")]
    input: ID3D12Resource,
    /// Reconstructed picture texture.
    #[cfg(target_os = "windows")]
    recon: ID3D12Resource,
    /// Reconstructed texture state.
    #[cfg(target_os = "windows")]
    recon_state: D3D12_RESOURCE_STATES,
    /// Input upload buffer.
    #[cfg(target_os = "windows")]
    upload: ID3D12Resource,
    /// Staging layout.
    #[cfg(target_os = "windows")]
    layout: Nv12Layout,
    /// Driver footprints behind staging copies.
    #[cfg(target_os = "windows")]
    footprints: [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; 2],
    /// Bitstream encoder buffer.
    #[cfg(target_os = "windows")]
    bitstream: ID3D12Resource,
    /// Bitstream buffer state.
    #[cfg(target_os = "windows")]
    bitstream_state: D3D12_RESOURCE_STATES,
    /// Bitstream staging buffer.
    #[cfg(target_os = "windows")]
    staging: ID3D12Resource,
    /// Bitstream capacity in bytes.
    #[cfg(target_os = "windows")]
    bitstream_cap: u64,
    /// Start-coded headers written before the slices.
    #[cfg(target_os = "windows")]
    headers: Vec<u8>,
    /// Encoder metadata buffer.
    #[cfg(target_os = "windows")]
    meta: ID3D12Resource,
    /// Metadata buffer state.
    #[cfg(target_os = "windows")]
    meta_state: D3D12_RESOURCE_STATES,
    /// Resolved metadata buffer.
    #[cfg(target_os = "windows")]
    resolved: ID3D12Resource,
    /// Resolved buffer state.
    #[cfg(target_os = "windows")]
    resolved_state: D3D12_RESOURCE_STATES,
    /// Resolved staging buffer.
    #[cfg(target_os = "windows")]
    resolved_staging: ID3D12Resource,
    /// Codec behind the session.
    #[cfg(target_os = "windows")]
    codec: crate::guid::Codec,
    /// Coded width in luma samples.
    #[cfg(target_os = "windows")]
    width: u32,
    /// Coded height in luma samples.
    #[cfg(target_os = "windows")]
    height: u32,
    /// Visible width in luma samples.
    #[cfg(target_os = "windows")]
    visible_width: u32,
    /// Visible height in luma samples.
    #[cfg(target_os = "windows")]
    visible_height: u32,
    /// Creation descriptors behind the encoder and heap.
    #[cfg(target_os = "windows")]
    descriptors: Box<EncoderDescriptors>,
    /// IDR id behind frames.
    #[cfg(target_os = "windows")]
    idr_pic_id: u16,
}

/// Encoder creation descriptors with a stable heap address.
#[cfg(target_os = "windows")]
enum EncoderDescriptors {
    /// H.264 descriptors.
    H264(H264Descriptors),
    /// HEVC descriptors.
    Hevc(HevcDescriptors),
}

/// H.264 creation descriptors with a stable heap address.
#[cfg(target_os = "windows")]
struct H264Descriptors {
    /// Encoder profile behind the encoder, heap, and resolve calls.
    profile: D3D12_VIDEO_ENCODER_PROFILE_H264,
    /// Encoder level behind the heap.
    level_setting: D3D12_VIDEO_ENCODER_LEVELS_H264,
    /// Codec configuration behind the encoder.
    codec_config: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
    /// Coded resolution behind the heap.
    resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
}

/// HEVC creation descriptors with a stable heap address.
#[cfg(target_os = "windows")]
struct HevcDescriptors {
    /// Encoder profile behind the encoder, heap, and resolve calls.
    profile: D3D12_VIDEO_ENCODER_PROFILE_HEVC,
    /// Encoder level behind the heap.
    level_setting: D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC,
    /// Codec configuration behind the encoder.
    codec_config: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
    /// Coded resolution behind the heap.
    resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
}

impl EncodeSession {
    /// Session over `device` for `codec` at `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, without a `codec` encoder, past the
    /// adapter encode caps, or when the driver refuses the session.
    pub(crate) fn open(
        device: &crate::device::D3d12Device,
        codec: crate::guid::Codec,
        width: u32,
        height: u32,
    ) -> Result<Self, D3d12Error> {
        #[cfg(target_os = "windows")]
        {
            Self::open_windows(device, codec, width, height)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device, codec, width, height);
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
    ) -> Result<Self, D3d12Error> {
        if codec != crate::guid::Codec::H264 && codec != crate::guid::Codec::Hevc {
            return Err(D3d12Error::Unsupported(format!(
                "{codec} encode args pending"
            )));
        }
        let (mut descriptors, header_bytes) = Self::descriptors(codec, width, height)?;
        let (coded_width, coded_height) = match descriptors.as_ref() {
            EncoderDescriptors::H264(h264) => (h264.resolution.Width, h264.resolution.Height),
            EncoderDescriptors::Hevc(hevc) => (hevc.resolution.Width, hevc.resolution.Height),
        };
        let video3 = device.video3()?;
        let (encoder, heap) = Self::create_codec(&video3, &mut descriptors)?;
        let resources = DeviceResources::new(device.device());
        let video_queue = resources.queue(D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)?;
        let video_alloc = resources.allocator(D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)?;
        let video_list: ID3D12VideoEncodeCommandList2 =
            resources.command_list(D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, &video_alloc)?;
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
        let meta_cap =
            Self::query_requirements(device.video(), &mut descriptors, coded_width, coded_height)?;
        let buffers = Self::open_buffers(&resources, coded_width, coded_height, meta_cap)?;
        Ok(Self {
            codec,
            video_queue,
            video_alloc,
            video_list,
            copy_queue,
            copy_alloc,
            copy_list,
            fence,
            encoder,
            heap,
            input: buffers.input,
            recon: buffers.recon,
            recon_state: D3D12_RESOURCE_STATE_COMMON,
            upload: buffers.upload,
            layout: buffers.layout,
            footprints: buffers.footprints,
            bitstream: buffers.bitstream,
            bitstream_state: D3D12_RESOURCE_STATE_COMMON,
            staging: buffers.staging,
            bitstream_cap: buffers.bitstream_cap,
            headers: header_bytes,
            meta: buffers.meta,
            meta_state: D3D12_RESOURCE_STATE_COMMON,
            resolved: buffers.resolved,
            resolved_state: D3D12_RESOURCE_STATE_COMMON,
            resolved_staging: buffers.resolved_staging,
            width: coded_width,
            height: coded_height,
            visible_width: width,
            visible_height: height,
            descriptors,
            idr_pic_id: 0,
        })
    }

    /// Descriptors plus header bytes for `codec` at visible `width` by `height`.
    #[cfg(target_os = "windows")]
    fn descriptors(
        codec: crate::guid::Codec,
        width: u32,
        height: u32,
    ) -> Result<(Box<EncoderDescriptors>, Vec<u8>), D3d12Error> {
        match codec {
            crate::guid::Codec::H264 => {
                let headers = H264Headers::build(width, height)?;
                let level = Self::level_h264(headers.level_idc())?;
                let (coded_width, coded_height) = headers.coded();
                let descriptors = EncoderDescriptors::H264(H264Descriptors {
                    profile: D3D12_VIDEO_ENCODER_PROFILE_H264_HIGH,
                    level_setting: level,
                    codec_config: Self::codec_config_h264(),
                    resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                        Width: coded_width,
                        Height: coded_height,
                    },
                });
                Ok((Box::new(descriptors), headers.into_bytes()))
            }
            crate::guid::Codec::Hevc => {
                let headers = HevcHeaders::build(width, height)?;
                let level = Self::level_hevc(headers.level_idc())?;
                let (coded_width, coded_height) = headers.coded();
                let descriptors = EncoderDescriptors::Hevc(HevcDescriptors {
                    profile: D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN,
                    level_setting: level,
                    codec_config: Self::codec_config_hevc(),
                    resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                        Width: coded_width,
                        Height: coded_height,
                    },
                });
                Ok((Box::new(descriptors), headers.into_bytes()))
            }
            _ => Err(D3d12Error::Unsupported(format!(
                "{codec} encode args pending"
            ))),
        }
    }

    /// Encoder level for `level_idc`.
    #[cfg(target_os = "windows")]
    fn level_h264(level_idc: u8) -> Result<D3D12_VIDEO_ENCODER_LEVELS_H264, D3d12Error> {
        match level_idc {
            10 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_1),
            11 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_11),
            12 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_12),
            13 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_13),
            20 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_2),
            21 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_21),
            22 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_22),
            30 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_3),
            31 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_31),
            32 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_32),
            40 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_4),
            41 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_41),
            42 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_42),
            50 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_5),
            51 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_51),
            52 => Ok(D3D12_VIDEO_ENCODER_LEVELS_H264_52),
            _ => Err(D3d12Error::Unsupported("level outside 5.2".to_owned())),
        }
    }

    /// Encoder level for `level_idc` on the main tier.
    #[cfg(target_os = "windows")]
    fn level_hevc(
        level_idc: u8,
    ) -> Result<D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC, D3d12Error> {
        let level: D3D12_VIDEO_ENCODER_LEVELS_HEVC = match level_idc {
            30 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_1,
            60 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_2,
            63 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_21,
            90 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_3,
            93 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_31,
            120 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_4,
            123 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_41,
            150 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_5,
            153 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_51,
            156 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_52,
            180 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_6,
            183 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_61,
            186 => D3D12_VIDEO_ENCODER_LEVELS_HEVC_62,
            _ => {
                return Err(D3d12Error::Unsupported("level outside 6.2".to_owned()));
            }
        };
        Ok(D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC {
            Level: level,
            Tier: D3D12_VIDEO_ENCODER_TIER_HEVC_MAIN,
        })
    }

    /// Intra-only codec configuration.
    #[cfg(target_os = "windows")]
    fn codec_config_h264() -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
        D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
            ConfigurationFlags:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_ENABLE_CABAC_ENCODING,
            DirectModeConfig:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
            DisableDeblockingFilterConfig:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
        }
    }

    /// Intra-only codec configuration matching the HEVC headers.
    ///
    /// <https://docs.rs/windows/0.62.2/windows/Win32/Media/MediaFoundation/>
    #[cfg(target_os = "windows")]
    fn codec_config_hevc() -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC {
        D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC {
            ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_NONE,
            MinLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_8x8,
            MaxLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_32x32,
            MinLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_4x4,
            MaxLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_32x32,
            max_transform_hierarchy_depth_inter: 1,
            max_transform_hierarchy_depth_intra: 1,
        }
    }

    /// Single-IDR group of pictures.
    #[cfg(target_os = "windows")]
    fn gop_h264() -> D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
        D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
            GOPLength: 1,
            PPicturePeriod: 1,
            pic_order_cnt_type: 0,
            log2_max_frame_num_minus4: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
        }
    }

    /// Single-IDR group of pictures.
    #[cfg(target_os = "windows")]
    fn gop_hevc() -> D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC {
        D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC {
            GOPLength: 1,
            PPicturePeriod: 1,
            log2_max_pic_order_cnt_lsb_minus4: 0,
        }
    }

    /// Constant quantizer behind every frame type.
    #[cfg(target_os = "windows")]
    fn cqp() -> D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP {
        D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP {
            ConstantQP_FullIntracodedFrame: INTRA_QP,
            ConstantQP_InterPredictedFrame_PrevRefOnly: INTRA_QP,
            ConstantQP_InterPredictedFrame_BiDirectionalRef: INTRA_QP,
        }
    }

    /// Rate control over `cqp`.
    #[cfg(target_os = "windows")]
    fn rate_control(
        cqp: &D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
    ) -> D3D12_VIDEO_ENCODER_RATE_CONTROL {
        D3D12_VIDEO_ENCODER_RATE_CONTROL {
            Mode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CQP,
            Flags: D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE,
            ConfigParams: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS {
                DataSize:
                    u32::try_from(std::mem::size_of::<D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP>())
                        .unwrap_or(u32::MAX),
                Anonymous: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0 {
                    pConfiguration_CQP: std::ptr::from_ref(cqp),
                },
            },
            TargetFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
        }
    }

    /// Encoder plus heap over the descriptors.
    #[cfg(target_os = "windows")]
    fn create_codec(
        video3: &ID3D12VideoDevice3,
        descriptors: &mut EncoderDescriptors,
    ) -> Result<(ID3D12VideoEncoder, ID3D12VideoEncoderHeap), D3d12Error> {
        // Sound: the boxed descriptors outlive the encoder and heap.
        let (mut encoder_desc, mut heap_desc) = match descriptors {
            EncoderDescriptors::H264(h264) => {
                let encoder_desc = D3D12_VIDEO_ENCODER_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_FLAGS(0),
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                    EncodeProfile: Self::profile_desc_h264(&mut h264.profile),
                    InputFormat: DXGI_FORMAT_NV12,
                    CodecConfiguration: crate::device::codec_configuration_h264(
                        &mut h264.codec_config,
                    ),
                    MaxMotionEstimationPrecision:
                        D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_FULL_PIXEL,
                };
                let heap_desc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_HEAP_FLAGS(0),
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                    EncodeProfile: Self::profile_desc_h264(&mut h264.profile),
                    EncodeLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING {
                        DataSize: u32::try_from(
                            std::mem::size_of::<D3D12_VIDEO_ENCODER_LEVELS_H264>(),
                        )
                        .unwrap_or(u32::MAX),
                        Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
                            pH264LevelSetting: std::ptr::from_mut(&mut h264.level_setting),
                        },
                    },
                    ResolutionsListCount: 1,
                    pResolutionList: std::ptr::from_ref(&h264.resolution),
                };
                (encoder_desc, heap_desc)
            }
            EncoderDescriptors::Hevc(hevc) => {
                let encoder_desc = D3D12_VIDEO_ENCODER_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_FLAGS(0),
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                    EncodeProfile: Self::profile_desc_hevc(&mut hevc.profile),
                    InputFormat: DXGI_FORMAT_NV12,
                    CodecConfiguration: crate::device::codec_configuration_hevc(
                        &mut hevc.codec_config,
                    ),
                    MaxMotionEstimationPrecision:
                        D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_FULL_PIXEL,
                };
                let heap_desc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_HEAP_FLAGS(0),
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                    EncodeProfile: Self::profile_desc_hevc(&mut hevc.profile),
                    EncodeLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING {
                        DataSize: u32::try_from(std::mem::size_of::<
                            D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC,
                        >())
                        .unwrap_or(u32::MAX),
                        Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
                            pHEVCLevelSetting: std::ptr::from_mut(&mut hevc.level_setting),
                        },
                    },
                    ResolutionsListCount: 1,
                    pResolutionList: std::ptr::from_ref(&hevc.resolution),
                };
                (encoder_desc, heap_desc)
            }
        };
        let encoder: ID3D12VideoEncoder =
            unsafe { video3.CreateVideoEncoder(std::ptr::from_mut(&mut encoder_desc)) }.map_err(
                |error| {
                    if error.code() == windows::Win32::Foundation::E_INVALIDARG {
                        return D3d12Error::OverCaps {
                            what: "encode configuration",
                        };
                    }
                    D3d12Error::Call {
                        call: "CreateVideoEncoder",
                        hresult: error.code().0,
                    }
                },
            )?;
        let heap: ID3D12VideoEncoderHeap =
            unsafe { video3.CreateVideoEncoderHeap(std::ptr::from_mut(&mut heap_desc)) }.map_err(
                |error| {
                    if error.code() == windows::Win32::Foundation::E_INVALIDARG {
                        return D3d12Error::OverCaps {
                            what: "encode extent",
                        };
                    }
                    D3d12Error::Call {
                        call: "CreateVideoEncoderHeap",
                        hresult: error.code().0,
                    }
                },
            )?;
        Ok((encoder, heap))
    }

    /// Buffer sizes behind `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the requirements query fails.
    #[cfg(target_os = "windows")]
    fn query_requirements(
        video: &ID3D12VideoDevice,
        descriptors: &mut EncoderDescriptors,
        width: u32,
        height: u32,
    ) -> Result<u64, D3d12Error> {
        let (codec, profile) = match descriptors {
            EncoderDescriptors::H264(h264) => (
                D3D12_VIDEO_ENCODER_CODEC_H264,
                Self::profile_desc_h264(&mut h264.profile),
            ),
            EncoderDescriptors::Hevc(hevc) => (
                D3D12_VIDEO_ENCODER_CODEC_HEVC,
                Self::profile_desc_hevc(&mut hevc.profile),
            ),
        };
        let mut requirements = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS {
            NodeIndex: 0,
            Codec: codec,
            Profile: profile,
            InputFormat: DXGI_FORMAT_NV12,
            PictureTargetResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            },
            ..Default::default()
        };
        crate::device::feature_support(
            video,
            D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
            &mut requirements,
            "EncoderRequirements",
        )?;
        if requirements.IsSupported.as_bool() {
            Ok(u64::from(
                requirements.MaxEncoderOutputMetadataBufferSize.max(4096),
            ))
        } else {
            Ok(META_CAP_FALLBACK)
        }
    }

    /// Buffers for `width` by `height` behind `meta_cap` metadata bytes.
    #[cfg(target_os = "windows")]
    fn open_buffers(
        resources: &DeviceResources,
        width: u32,
        height: u32,
        meta_cap: u64,
    ) -> Result<Buffers, D3d12Error> {
        let texture_desc = DeviceResources::texture_desc(width, height);
        let input = resources.committed(
            D3D12_HEAP_TYPE_DEFAULT,
            &texture_desc,
            D3D12_RESOURCE_STATE_COMMON,
            "CreateInput",
        )?;
        let recon = resources.committed(
            D3D12_HEAP_TYPE_DEFAULT,
            &texture_desc,
            D3D12_RESOURCE_STATE_COMMON,
            "CreateRecon",
        )?;
        let (layout, footprints) = resources.nv12_layout(width, height)?;
        let upload_desc = DeviceResources::buffer_desc(layout.total);
        let upload = resources.committed(
            D3D12_HEAP_TYPE_UPLOAD,
            &upload_desc,
            D3D12_RESOURCE_STATE_GENERIC_READ,
            "CreateUpload",
        )?;
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        let bitstream_cap = pixels
            .saturating_mul(3)
            .saturating_add(65536)
            .div_ceil(65536)
            * 65536;
        let stream_desc = DeviceResources::buffer_desc(bitstream_cap);
        let bitstream = resources.committed(
            D3D12_HEAP_TYPE_DEFAULT,
            &stream_desc,
            D3D12_RESOURCE_STATE_COMMON,
            "CreateBitstream",
        )?;
        let staging = resources.committed(
            D3D12_HEAP_TYPE_READBACK,
            &stream_desc,
            D3D12_RESOURCE_STATE_COPY_DEST,
            "CreateStaging",
        )?;
        let meta_desc = DeviceResources::buffer_desc(meta_cap);
        let meta = resources.committed(
            D3D12_HEAP_TYPE_DEFAULT,
            &meta_desc,
            D3D12_RESOURCE_STATE_COMMON,
            "CreateMeta",
        )?;
        let resolved = resources.committed(
            D3D12_HEAP_TYPE_DEFAULT,
            &meta_desc,
            D3D12_RESOURCE_STATE_COMMON,
            "CreateResolved",
        )?;
        let resolved_staging = resources.committed(
            D3D12_HEAP_TYPE_READBACK,
            &meta_desc,
            D3D12_RESOURCE_STATE_COPY_DEST,
            "CreateResolvedStaging",
        )?;
        Ok(Buffers {
            input,
            recon,
            upload,
            layout,
            footprints,
            bitstream,
            staging,
            bitstream_cap,
            meta,
            resolved,
            resolved_staging,
        })
    }

    /// Profile descriptor over `profile`.
    #[cfg(target_os = "windows")]
    fn profile_desc_h264(
        profile: &mut D3D12_VIDEO_ENCODER_PROFILE_H264,
    ) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
        D3D12_VIDEO_ENCODER_PROFILE_DESC {
            DataSize: u32::try_from(std::mem::size_of::<D3D12_VIDEO_ENCODER_PROFILE_H264>())
                .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
                pH264Profile: std::ptr::from_mut(profile),
            },
        }
    }

    /// Profile descriptor over `profile`.
    #[cfg(target_os = "windows")]
    fn profile_desc_hevc(
        profile: &mut D3D12_VIDEO_ENCODER_PROFILE_HEVC,
    ) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
        D3D12_VIDEO_ENCODER_PROFILE_DESC {
            DataSize: u32::try_from(std::mem::size_of::<D3D12_VIDEO_ENCODER_PROFILE_HEVC>())
                .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
                pHEVCProfile: std::ptr::from_mut(profile),
            },
        }
    }

    /// Profile descriptor over the session descriptors.
    #[cfg(target_os = "windows")]
    fn profile_desc(descriptors: &mut EncoderDescriptors) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
        match descriptors {
            EncoderDescriptors::H264(h264) => Self::profile_desc_h264(&mut h264.profile),
            EncoderDescriptors::Hevc(hevc) => Self::profile_desc_hevc(&mut hevc.profile),
        }
    }

    /// Bitstream bytes encoded from NV12 on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, for NV12 short of the extent, or when
    /// the driver refuses the picture.
    pub(crate) fn encode(&mut self, nv12: &[u8]) -> Result<Vec<u8>, D3d12Error> {
        #[cfg(target_os = "windows")]
        {
            self.encode_windows(nv12)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (nv12, self);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Bitstream bytes encoded from NV12 on the GPU.
    #[cfg(target_os = "windows")]
    fn encode_windows(&mut self, nv12: &[u8]) -> Result<Vec<u8>, D3d12Error> {
        let padded;
        let coded = if (self.visible_width, self.visible_height) == (self.width, self.height) {
            nv12
        } else {
            padded = pad_nv12(
                nv12,
                (self.visible_width, self.visible_height),
                (self.width, self.height),
            )?;
            &padded
        };
        let staged = self.layout.stride(coded, self.width, self.height)?;
        DeviceResources::write_upload(&self.upload, &staged)?;
        self.submit_copy()?;
        self.finish_picture()
    }

    /// Bitstream bytes encoded from the `src` texture on the GPU.
    #[cfg(target_os = "windows")]
    pub(crate) fn encode_texture(
        &mut self,
        src: &ID3D12Resource,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, D3d12Error> {
        if (width, height) != (self.visible_width, self.visible_height) {
            return Err(D3d12Error::Unsupported(
                "texture extent outside session".to_owned(),
            ));
        }
        self.submit_copy_from(src, width, height)?;
        self.finish_picture()
    }

    /// Picture through the video queue plus metadata into Annex B bytes.
    #[cfg(target_os = "windows")]
    fn finish_picture(&mut self) -> Result<Vec<u8>, D3d12Error> {
        self.submit_video()?;
        self.submit_download()?;
        let meta = self.read_metadata()?;
        if meta.EncodeErrorFlags != 0 {
            return Err(D3d12Error::Unsupported(format!(
                "encoder error flags {:#x}",
                meta.EncodeErrorFlags
            )));
        }
        let driver_len = usize::try_from(meta.EncodedBitstreamWrittenBytesCount)
            .map_err(|_| D3d12Error::Unsupported("bitstream too large".to_owned()))?;
        if u64::try_from(driver_len).unwrap_or(u64::MAX) > self.bitstream_cap || driver_len == 0 {
            return Err(D3d12Error::Unsupported(
                "driver bitstream outside buffer".to_owned(),
            ));
        }
        let buffer = DeviceResources::read_readback(&self.staging, driver_len)?;
        let mut out = Vec::with_capacity(self.headers.len() + driver_len);
        out.extend_from_slice(&self.headers);
        out.extend_from_slice(&buffer[..driver_len]);
        self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        Ok(out)
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

    /// NV12 upload through the direct queue into the input texture.
    #[cfg(target_os = "windows")]
    fn submit_copy(&mut self) -> Result<(), D3d12Error> {
        self.reset_copy()?;
        // Sound: the barriers name a live texture; the list is open.
        unsafe {
            self.copy_list.ResourceBarrier(&[DeviceResources::barrier(
                &self.input,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )]);
        }
        // Sound: the copy locations name live resources; the list is open.
        unsafe {
            for (index, placed) in [0u32, 1u32].into_iter().zip(self.footprints.iter()) {
                let dst = DeviceResources::subresource_location(&self.input, index);
                let src = DeviceResources::footprint_location(&self.upload, placed);
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
                &self.input,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        self.close_copy()
    }

    /// `src` visible planes through the direct queue into the input texture.
    ///
    /// Pad samples outside the visible rectangle stay stale; the crop
    /// rectangle hides them from decoders.
    #[cfg(target_os = "windows")]
    fn submit_copy_from(
        &mut self,
        src: &ID3D12Resource,
        width: u32,
        height: u32,
    ) -> Result<(), D3d12Error> {
        self.reset_copy()?;
        // Sound: the barriers name live textures; the list is open.
        unsafe {
            self.copy_list.ResourceBarrier(&[
                DeviceResources::barrier(
                    &self.input,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                ),
                DeviceResources::barrier(
                    src,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
            ]);
        }
        let boxes = [[0, 0, width, height], [0, 0, width / 2, height / 2]].map(|edges| D3D12_BOX {
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
                let dst = DeviceResources::subresource_location(&self.input, index);
                let src_loc = DeviceResources::subresource_location(src, index);
                self.copy_list.CopyTextureRegion(
                    std::ptr::from_ref(&dst),
                    0,
                    0,
                    0,
                    std::ptr::from_ref(&src_loc),
                    Some(std::ptr::from_ref(src_box)),
                );
            }
            self.copy_list.ResourceBarrier(&[
                DeviceResources::barrier(
                    &self.input,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
                DeviceResources::barrier(
                    src,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
            ]);
        }
        self.close_copy()
    }

    /// Picture control data over `pic_data`.
    #[cfg(target_os = "windows")]
    fn picture_data_h264(
        pic_data: &mut D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
    ) -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
            DataSize: u32::try_from(std::mem::size_of::<
                D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
            >())
            .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 {
                pH264PicData: std::ptr::from_mut(pic_data),
            },
        }
    }

    /// Picture control data over `pic_data`.
    #[cfg(target_os = "windows")]
    fn picture_data_hevc(
        pic_data: &mut D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC,
    ) -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
            DataSize: u32::try_from(std::mem::size_of::<
                D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC,
            >())
            .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 {
                pHEVCPicData: std::ptr::from_mut(pic_data),
            },
        }
    }

    /// Picture control over `codec_data`.
    #[cfg(target_os = "windows")]
    fn picture_control(
        codec_data: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA,
    ) -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
            IntraRefreshFrameIndex: 0,
            // The driver removes the device without a validation message when
            // the IDR picture is not marked as a reference picture.
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
            PictureControlCodecData: codec_data,
            ReferenceFrames: D3D12_VIDEO_ENCODE_REFERENCE_FRAMES::default(),
        }
    }

    /// Group-of-pictures structure over `gop`.
    #[cfg(target_os = "windows")]
    fn sequence_gop_h264(
        gop: &mut D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264,
    ) -> D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
        D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
            DataSize: u32::try_from(std::mem::size_of::<
                D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264,
            >())
            .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                pH264GroupOfPictures: std::ptr::from_mut(gop),
            },
        }
    }

    /// Group-of-pictures structure over `gop`.
    #[cfg(target_os = "windows")]
    fn sequence_gop_hevc(
        gop: &mut D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC,
    ) -> D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
        D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
            DataSize: u32::try_from(std::mem::size_of::<
                D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC,
            >())
            .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                pHEVCGroupOfPictures: std::ptr::from_mut(gop),
            },
        }
    }

    /// Sequence control over `cqp` and `gop` at `width` by `height`.
    #[cfg(target_os = "windows")]
    fn sequence_control(
        cqp: &D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
        gop: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE,
        width: u32,
        height: u32,
    ) -> D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC {
        D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC {
            Flags: D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAGS(0),
            IntraRefreshConfig: D3D12_VIDEO_ENCODER_INTRA_REFRESH {
                Mode: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
                IntraRefreshDuration: 0,
            },
            RateControl: Self::rate_control(cqp),
            PictureTargetResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            },
            SelectedLayoutMode: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
            FrameSubregionsLayoutData:
                D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA::default(),
            CodecGopSequence: gop,
        }
    }

    /// IDR picture data over `idr_pic_id`.
    #[cfg(target_os = "windows")]
    fn pic_h264(idr_pic_id: u16) -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264 {
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264 {
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
            FrameType: D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME,
            pic_parameter_set_id: 0,
            idr_pic_id: u32::from(idr_pic_id),
            PictureOrderCountNumber: 0,
            FrameDecodingOrderNumber: 0,
            ..Default::default()
        }
    }

    /// IDR picture data.
    #[cfg(target_os = "windows")]
    fn pic_hevc() -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC {
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC {
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC_FLAG_NONE,
            FrameType: D3D12_VIDEO_ENCODER_FRAME_TYPE_HEVC_IDR_FRAME,
            slice_pic_parameter_set_id: 0,
            PictureOrderCountNumber: 0,
            TemporalLayerIndex: 0,
            ..Default::default()
        }
    }

    /// `EncodeFrame` plus metadata resolve through the video queue.
    #[cfg(target_os = "windows")]
    fn submit_video(&mut self) -> Result<(), D3d12Error> {
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
        let hevc = matches!(self.codec, crate::guid::Codec::Hevc);
        let mut pic_h264 = Self::pic_h264(self.idr_pic_id);
        let mut pic_hevc = Self::pic_hevc();
        let codec_data = if hevc {
            Self::picture_data_hevc(&mut pic_hevc)
        } else {
            Self::picture_data_h264(&mut pic_h264)
        };
        let picture_control = Self::picture_control(codec_data);
        let cqp = Self::cqp();
        let mut gop_h264 = Self::gop_h264();
        let mut gop_hevc = Self::gop_hevc();
        let gop = if hevc {
            Self::sequence_gop_hevc(&mut gop_hevc)
        } else {
            Self::sequence_gop_h264(&mut gop_h264)
        };
        let sequence = Self::sequence_control(&cqp, gop, self.width, self.height);
        let input_args = D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS {
            SequenceControlDesc: sequence,
            PictureControlDesc: picture_control,
            pInputFrame: ManuallyDrop::new(Some(self.input.clone())),
            InputFrameSubresource: 0,
            CurrentFrameBitstreamMetadataSize: 0,
        };
        let output_args = D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS {
            Bitstream: D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM {
                pBuffer: ManuallyDrop::new(Some(self.bitstream.clone())),
                FrameStartOffset: 0,
            },
            ReconstructedPicture: D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE {
                pReconstructedPicture: ManuallyDrop::new(Some(self.recon.clone())),
                ReconstructedPictureSubresource: 0,
            },
            EncoderOutputMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                pBuffer: ManuallyDrop::new(Some(self.meta.clone())),
                Offset: 0,
            },
        };
        let resolve_in = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS {
            EncoderCodec: if hevc {
                D3D12_VIDEO_ENCODER_CODEC_HEVC
            } else {
                D3D12_VIDEO_ENCODER_CODEC_H264
            },
            EncoderProfile: Self::profile_desc(&mut self.descriptors),
            EncoderInputFormat: DXGI_FORMAT_NV12,
            EncodedPictureEffectiveResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: self.width,
                Height: self.height,
            },
            HWLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                pBuffer: ManuallyDrop::new(Some(self.meta.clone())),
                Offset: 0,
            },
        };
        let resolve_out = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS {
            ResolvedLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                pBuffer: ManuallyDrop::new(Some(self.resolved.clone())),
                Offset: 0,
            },
        };
        self.record_video(&input_args, &output_args, &resolve_in, &resolve_out);
        // Sound: the list holds only the encode and resolve above.
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
        self.recon_state = D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE;
        self.bitstream_state = D3D12_RESOURCE_STATE_COMMON;
        self.meta_state = D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ;
        self.resolved_state = D3D12_RESOURCE_STATE_COMMON;
        Ok(())
    }

    /// Encode plus resolve records behind the argument structs.
    #[cfg(target_os = "windows")]
    fn record_video(
        &self,
        input_args: &D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS,
        output_args: &D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS,
        resolve_in: &D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS,
        resolve_out: &D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS,
    ) {
        // Sound: the barriers name live resources; the list is open.
        // Sound: the argument structs outlive the fence wait below.
        unsafe {
            if self.recon_state != D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE {
                self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                    &self.recon,
                    self.recon_state,
                    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                )]);
            }
            self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                &self.input,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
            )]);
            if self.bitstream_state != D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE {
                self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                    &self.bitstream,
                    self.bitstream_state,
                    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                )]);
            }
            if self.meta_state != D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE {
                self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                    &self.meta,
                    self.meta_state,
                    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                )]);
            }
            if self.resolved_state != D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE {
                self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                    &self.resolved,
                    self.resolved_state,
                    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                )]);
            }
            self.video_list.EncodeFrame(
                &self.encoder,
                &self.heap,
                std::ptr::from_ref(input_args),
                std::ptr::from_ref(output_args),
            );
            self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                &self.meta,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
            )]);
            self.video_list.ResolveEncoderOutputMetadata(
                std::ptr::from_ref(resolve_in),
                std::ptr::from_ref(resolve_out),
            );
            self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                &self.input,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
            self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                &self.bitstream,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
            self.video_list.ResourceBarrier(&[DeviceResources::barrier(
                &self.resolved,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
    }

    /// Bitstream and metadata through the direct queue into staging.
    #[cfg(target_os = "windows")]
    fn submit_download(&mut self) -> Result<(), D3d12Error> {
        // Sound: the queue is idle; reset reuses the allocator and list.
        unsafe { self.copy_alloc.Reset() }.map_err(|error| D3d12Error::Call {
            call: "ResetAllocator",
            hresult: error.code().0,
        })?;
        // Sound: the allocator outlives the reset; no initial pipeline.
        unsafe { self.copy_list.Reset(&self.copy_alloc, None) }.map_err(|error| {
            D3d12Error::Call {
                call: "ResetList",
                hresult: error.code().0,
            }
        })?;
        // Sound: the buffers outlive the fence wait below.
        unsafe {
            self.copy_list.ResourceBarrier(&[
                DeviceResources::barrier(
                    &self.bitstream,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
                DeviceResources::barrier(
                    &self.resolved,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                ),
            ]);
            self.copy_list.CopyBufferRegion(
                &self.staging,
                0,
                &self.bitstream,
                0,
                self.bitstream_cap,
            );
            self.copy_list.CopyBufferRegion(
                &self.resolved_staging,
                0,
                &self.resolved,
                0,
                u64::try_from(std::mem::size_of::<D3D12_VIDEO_ENCODER_OUTPUT_METADATA>())
                    .unwrap_or(0),
            );
            self.copy_list.ResourceBarrier(&[
                DeviceResources::barrier(
                    &self.bitstream,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
                DeviceResources::barrier(
                    &self.resolved,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
            ]);
        }
        // Sound: the list holds only the copy above.
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

    /// Output metadata from the resolved staging buffer.
    #[cfg(target_os = "windows")]
    fn read_metadata(&self) -> Result<D3D12_VIDEO_ENCODER_OUTPUT_METADATA, D3d12Error> {
        let len = std::mem::size_of::<D3D12_VIDEO_ENCODER_OUTPUT_METADATA>();
        let bytes = DeviceResources::read_readback(&self.resolved_staging, len)?;
        if bytes.len() < len {
            return Err(D3d12Error::Unsupported(
                "resolved metadata short".to_owned(),
            ));
        }
        // Sound: the length is verified; the metadata is plain data.
        Ok(unsafe { std::ptr::read(bytes.as_ptr().cast()) })
    }
}

#[cfg(test)]
mod tests {
    use super::{pad_nv12, H264Headers, HevcHeaders};

    use crate::bits::rbsp;
    use crate::h264::{parse_pps, parse_sps, split_nals};
    use crate::hevc;

    #[test]
    fn headers_round_trip_through_parser() {
        let headers = H264Headers::build(64, 48).expect("headers");
        assert_eq!(headers.level_idc(), 10);
        let bytes = headers.into_bytes();
        let nals = split_nals(&bytes);
        assert_eq!(nals.len(), 2);
        let sps = parse_sps(&rbsp(nals[0])[1..]).expect("sps");
        assert_eq!((sps.width(), sps.height()), (64, 48));
        assert_eq!(sps.level_idc, 10);
        let pps = parse_pps(&rbsp(nals[1])[1..], &sps).expect("pps");
        assert!(pps.entropy());
        assert!(pps.transform_8x8());
    }

    #[test]
    fn headers_pick_level_by_extent() {
        assert_eq!(H264Headers::build(64, 48).expect("headers").level_idc(), 10);
        assert_eq!(
            H264Headers::build(1280, 720).expect("headers").level_idc(),
            31
        );
        assert_eq!(
            H264Headers::build(1920, 1088).expect("headers").level_idc(),
            40
        );
    }

    #[test]
    fn headers_reject_odd_grids() {
        assert!(H264Headers::build(65, 48).is_err());
        assert!(H264Headers::build(64, 47).is_err());
        assert!(H264Headers::build(8192, 8192).is_err());
    }

    #[test]
    fn headers_crop_360p_to_grid() {
        let headers = H264Headers::build(640, 360).expect("headers");
        assert_eq!(headers.coded(), (640, 368));
        assert_eq!(headers.level_idc(), 22);
        let bytes = headers.into_bytes();
        let nals = split_nals(&bytes);
        assert_eq!(nals.len(), 2);
        let sps = parse_sps(&rbsp(nals[0])[1..]).expect("sps");
        assert_eq!((sps.width(), sps.height()), (640, 368));
        assert_eq!((sps.visible_width(), sps.visible_height()), (640, 360));
    }

    #[test]
    fn headers_crop_1080p_to_grid() {
        let headers = H264Headers::build(1920, 1080).expect("headers");
        assert_eq!(headers.coded(), (1920, 1088));
        assert_eq!(headers.level_idc(), 40);
        let bytes = headers.into_bytes();
        let nals = split_nals(&bytes);
        assert_eq!(nals.len(), 2);
        let sps = parse_sps(&rbsp(nals[0])[1..]).expect("sps");
        assert_eq!((sps.width(), sps.height()), (1920, 1088));
        assert_eq!((sps.visible_width(), sps.visible_height()), (1920, 1080));
    }

    #[test]
    fn pad_replicates_edge() {
        let visible = vec![1, 2, 3, 4, 5, 6];
        let padded = pad_nv12(&visible, (2, 2), (4, 4)).expect("padded");
        assert_eq!(
            padded,
            vec![1, 2, 2, 2, 3, 4, 4, 4, 3, 4, 4, 4, 3, 4, 4, 4, 5, 6, 6, 6, 5, 6, 6, 6,]
        );
    }

    #[test]
    fn pad_rejects_short_input() {
        assert!(pad_nv12(&[0; 5], (2, 2), (4, 4)).is_err());
    }

    #[test]
    fn hevc_headers_round_trip_through_parser() {
        let headers = HevcHeaders::build(64, 48).expect("headers");
        assert_eq!(headers.level_idc(), 30);
        assert_eq!(headers.coded(), (64, 48));
        let bytes = headers.into_bytes();
        let nals = hevc::split_nals(&bytes);
        assert_eq!(nals.len(), 3);
        let kinds: Vec<u8> = nals.iter().map(|nal| nal[0] >> 1).collect();
        assert_eq!(kinds, vec![32, 33, 34]);
        let sps = hevc::parse_sps(&crate::bits::rbsp(nals[1])[2..]).expect("sps");
        assert_eq!((sps.width(), sps.height()), (64, 48));
        assert!(sps.crop.is_none());
        assert_eq!(sps.min_cb(), 8);
        assert_eq!(sps.log2_diff_max_min_cb, 2);
        assert_eq!(sps.log2_min_tb_minus2, 0);
        assert_eq!(sps.log2_diff_max_min_tb, 3);
        assert!(!sps.amp());
        assert!(!sps.sao());
        hevc::parse_pps(&crate::bits::rbsp(nals[2])[2..], &sps).expect("pps");
    }

    #[test]
    fn hevc_headers_crop_to_window() {
        let headers = HevcHeaders::build(640, 360).expect("headers");
        assert_eq!(headers.coded(), (640, 368));
        assert_eq!(headers.level_idc(), 63);
        let bytes = headers.into_bytes();
        let nals = hevc::split_nals(&bytes);
        assert_eq!(nals.len(), 3);
        let sps = hevc::parse_sps(&crate::bits::rbsp(nals[1])[2..]).expect("sps");
        assert_eq!((sps.width(), sps.height()), (640, 368));
        assert_eq!((sps.visible_width(), sps.visible_height()), (640, 360));
    }

    #[test]
    fn hevc_headers_pick_level_by_extent() {
        assert_eq!(HevcHeaders::build(64, 48).expect("headers").level_idc(), 30);
        assert_eq!(
            HevcHeaders::build(320, 240).expect("headers").level_idc(),
            60
        );
        assert_eq!(
            HevcHeaders::build(1920, 1080).expect("headers").level_idc(),
            120
        );
        assert!(HevcHeaders::build(65, 48).is_err());
        assert!(HevcHeaders::build(8192, 8192).is_err());
    }
}
