//! D3D12 video encode: headers plus GPU submission.
//!
//! Intra-only H.264; reference pictures stay pending.

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
    ID3D12Resource, D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE,
    D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_READBACK, D3D12_HEAP_TYPE_UPLOAD,
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL};
#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    ID3D12VideoDevice, ID3D12VideoDevice3, ID3D12VideoEncodeCommandList2, ID3D12VideoEncoder,
    ID3D12VideoEncoderHeap, D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
    D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_ENABLE_CABAC_ENCODING,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
    D3D12_VIDEO_ENCODER_CODEC_H264, D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM,
    D3D12_VIDEO_ENCODER_DESC, D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER, D3D12_VIDEO_ENCODER_FLAGS,
    D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
    D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME, D3D12_VIDEO_ENCODER_HEAP_DESC,
    D3D12_VIDEO_ENCODER_HEAP_FLAGS, D3D12_VIDEO_ENCODER_INTRA_REFRESH,
    D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE, D3D12_VIDEO_ENCODER_LEVELS_H264,
    D3D12_VIDEO_ENCODER_LEVELS_H264_1, D3D12_VIDEO_ENCODER_LEVELS_H264_11,
    D3D12_VIDEO_ENCODER_LEVELS_H264_12, D3D12_VIDEO_ENCODER_LEVELS_H264_13,
    D3D12_VIDEO_ENCODER_LEVELS_H264_2, D3D12_VIDEO_ENCODER_LEVELS_H264_21,
    D3D12_VIDEO_ENCODER_LEVELS_H264_22, D3D12_VIDEO_ENCODER_LEVELS_H264_3,
    D3D12_VIDEO_ENCODER_LEVELS_H264_31, D3D12_VIDEO_ENCODER_LEVELS_H264_32,
    D3D12_VIDEO_ENCODER_LEVELS_H264_4, D3D12_VIDEO_ENCODER_LEVELS_H264_41,
    D3D12_VIDEO_ENCODER_LEVELS_H264_42, D3D12_VIDEO_ENCODER_LEVELS_H264_5,
    D3D12_VIDEO_ENCODER_LEVELS_H264_51, D3D12_VIDEO_ENCODER_LEVELS_H264_52,
    D3D12_VIDEO_ENCODER_LEVEL_SETTING, D3D12_VIDEO_ENCODER_LEVEL_SETTING_0,
    D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_FULL_PIXEL,
    D3D12_VIDEO_ENCODER_OUTPUT_METADATA, D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
    D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA,
    D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC, D3D12_VIDEO_ENCODER_PROFILE_DESC,
    D3D12_VIDEO_ENCODER_PROFILE_DESC_0, D3D12_VIDEO_ENCODER_PROFILE_H264,
    D3D12_VIDEO_ENCODER_PROFILE_H264_HIGH, D3D12_VIDEO_ENCODER_RATE_CONTROL,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0, D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
    D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE, D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CQP,
    D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE,
    D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS,
    D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAGS,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE, D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0,
    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264, D3D12_VIDEO_ENCODE_REFERENCE_FRAMES,
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

/// Annex B sequence headers matching the encoder configuration.
pub(crate) struct H264Headers {
    /// Start-coded SPS plus PPS.
    bytes: Vec<u8>,
    /// Level idc behind the headers.
    level_idc: u8,
}

impl H264Headers {
    /// Headers for `width` by `height` on the macroblock grid.
    ///
    /// # Errors
    ///
    /// Returns `Err` off the macroblock grid or past level 5.2.
    pub(crate) fn build(width: u32, height: u32) -> Result<Self, D3d12Error> {
        if !width.is_multiple_of(MB) || !height.is_multiple_of(MB) {
            return Err(D3d12Error::Unsupported(
                "extent outside macroblock grid".to_owned(),
            ));
        }
        let mbs = width
            .checked_div(MB)
            .and_then(|w| height.checked_div(MB).and_then(|h| w.checked_mul(h)))
            .ok_or_else(|| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let level_idc = LEVELS.iter().find(|entry| mbs <= entry.0).map_or_else(
            || Err(D3d12Error::Unsupported("extent above level 5.2".to_owned())),
            |entry| Ok(entry.1),
        )?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x67]);
        bytes.extend_from_slice(&sps_body(width / MB - 1, height / MB - 1, level_idc));
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x68]);
        bytes.extend_from_slice(&pps_body());
        Ok(Self { bytes, level_idc })
    }

    /// Start-coded header bytes.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Level idc behind the headers.
    pub(crate) fn level_idc(&self) -> u8 {
        self.level_idc
    }
}

/// Sequence set body for the encoder configuration.
fn sps_body(width_mbs_minus1: u32, height_mbs_minus1: u32, level_idc: u8) -> Vec<u8> {
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

/// Test bit writer with emulation prevention.
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
    /// Coded width in luma samples.
    #[cfg(target_os = "windows")]
    width: u32,
    /// Coded height in luma samples.
    #[cfg(target_os = "windows")]
    height: u32,
    /// Creation descriptors behind the encoder and heap.
    #[cfg(target_os = "windows")]
    descriptors: Box<EncoderDescriptors>,
    /// IDR id behind frames.
    #[cfg(target_os = "windows")]
    idr_pic_id: u16,
}

/// Encoder creation descriptors with a stable heap address.
#[cfg(target_os = "windows")]
struct EncoderDescriptors {
    /// Encoder profile behind the encoder, heap, and resolve calls.
    profile: D3D12_VIDEO_ENCODER_PROFILE_H264,
    /// Encoder level behind the heap.
    level_setting: D3D12_VIDEO_ENCODER_LEVELS_H264,
    /// Codec configuration behind the encoder.
    codec_config: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
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
        if codec != crate::guid::Codec::H264 {
            return Err(D3d12Error::Unsupported(format!(
                "{codec} encode args pending"
            )));
        }
        let headers = H264Headers::build(width, height)?;
        let level = Self::level(headers.level_idc())?;
        let video3 = device.video3()?;
        let mut descriptors = Box::new(EncoderDescriptors {
            profile: D3D12_VIDEO_ENCODER_PROFILE_H264_HIGH,
            level_setting: level,
            codec_config: Self::codec_config(),
            resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            },
        });
        let (encoder, heap) = Self::create_codec(
            &video3,
            &mut descriptors.profile,
            &mut descriptors.level_setting,
            &mut descriptors.codec_config,
            &descriptors.resolution,
        )?;
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
            Self::query_requirements(device.video(), &mut descriptors.profile, width, height);
        let buffers = Self::open_buffers(&resources, width, height, meta_cap)?;
        Ok(Self {
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
            headers: headers.bytes,
            meta: buffers.meta,
            meta_state: D3D12_RESOURCE_STATE_COMMON,
            resolved: buffers.resolved,
            resolved_state: D3D12_RESOURCE_STATE_COMMON,
            resolved_staging: buffers.resolved_staging,
            width,
            height,
            descriptors,
            idr_pic_id: 0,
        })
    }

    /// Encoder level for `level_idc`.
    #[cfg(target_os = "windows")]
    fn level(level_idc: u8) -> Result<D3D12_VIDEO_ENCODER_LEVELS_H264, D3d12Error> {
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

    /// Intra-only codec configuration.
    #[cfg(target_os = "windows")]
    fn codec_config() -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
        D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
            ConfigurationFlags:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_ENABLE_CABAC_ENCODING,
            DirectModeConfig:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
            DisableDeblockingFilterConfig:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
        }
    }

    /// Single-IDR group of pictures.
    #[cfg(target_os = "windows")]
    fn gop() -> D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
        D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
            GOPLength: 1,
            PPicturePeriod: 1,
            pic_order_cnt_type: 0,
            log2_max_frame_num_minus4: 0,
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
        profile: &mut D3D12_VIDEO_ENCODER_PROFILE_H264,
        level_setting: &mut D3D12_VIDEO_ENCODER_LEVELS_H264,
        codec_config: &mut D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
        resolution: &D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    ) -> Result<(ID3D12VideoEncoder, ID3D12VideoEncoderHeap), D3d12Error> {
        // Sound: the boxed descriptors outlive the encoder and heap.
        let mut encoder_desc = D3D12_VIDEO_ENCODER_DESC {
            NodeMask: 0,
            Flags: D3D12_VIDEO_ENCODER_FLAGS(0),
            EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
            EncodeProfile: Self::profile_desc(profile),
            InputFormat: DXGI_FORMAT_NV12,
            CodecConfiguration: crate::device::codec_configuration_h264(codec_config),
            MaxMotionEstimationPrecision:
                D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_FULL_PIXEL,
        };
        let mut heap_desc = D3D12_VIDEO_ENCODER_HEAP_DESC {
            NodeMask: 0,
            Flags: D3D12_VIDEO_ENCODER_HEAP_FLAGS(0),
            EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
            EncodeProfile: Self::profile_desc(profile),
            EncodeLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING {
                DataSize: u32::try_from(std::mem::size_of::<D3D12_VIDEO_ENCODER_LEVELS_H264>())
                    .unwrap_or(u32::MAX),
                Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
                    pH264LevelSetting: std::ptr::from_mut(level_setting),
                },
            },
            ResolutionsListCount: 1,
            pResolutionList: std::ptr::from_ref(resolution),
        };
        let encoder: ID3D12VideoEncoder =
            unsafe { video3.CreateVideoEncoder(std::ptr::from_mut(&mut encoder_desc)) }.map_err(
                |error| D3d12Error::Call {
                    call: "CreateVideoEncoder",
                    hresult: error.code().0,
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
    #[cfg(target_os = "windows")]
    fn query_requirements(
        video: &ID3D12VideoDevice,
        profile: &mut D3D12_VIDEO_ENCODER_PROFILE_H264,
        width: u32,
        height: u32,
    ) -> u64 {
        let mut requirements = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            Profile: Self::profile_desc(profile),
            InputFormat: DXGI_FORMAT_NV12,
            PictureTargetResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            },
            ..Default::default()
        };
        match crate::device::feature_support(
            video,
            D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
            &mut requirements,
            "EncoderRequirements",
        ) {
            Ok(()) if requirements.IsSupported.as_bool() => {
                u64::from(requirements.MaxEncoderOutputMetadataBufferSize.max(4096))
            }
            _ => META_CAP_FALLBACK,
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
    fn profile_desc(
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
        let staged = self.layout.stride(nv12, self.width, self.height)?;
        DeviceResources::write_upload(&self.upload, &staged)?;
        self.submit_copy()?;
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

    /// NV12 upload through the direct queue into the input texture.
    #[cfg(target_os = "windows")]
    fn submit_copy(&mut self) -> Result<(), D3d12Error> {
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
        // Sound: the list holds only the barriers and copies above.
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

    /// Picture control over `pic_data`.
    #[cfg(target_os = "windows")]
    fn picture_control(
        pic_data: &mut D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
    ) -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
        let codec_data = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
            DataSize: u32::try_from(std::mem::size_of::<
                D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264,
            >())
            .unwrap_or(u32::MAX),
            Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 {
                pH264PicData: std::ptr::from_mut(pic_data),
            },
        };
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
            IntraRefreshFrameIndex: 0,
            // The driver removes the device without a validation message when
            // the IDR picture is not marked as a reference picture.
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
            PictureControlCodecData: codec_data,
            ReferenceFrames: D3D12_VIDEO_ENCODE_REFERENCE_FRAMES::default(),
        }
    }

    /// Sequence control over `cqp` and `gop` at `width` by `height`.
    #[cfg(target_os = "windows")]
    fn sequence_control(
        cqp: &D3D12_VIDEO_ENCODER_RATE_CONTROL_CQP,
        gop: &mut D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264,
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
            CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
                DataSize: u32::try_from(std::mem::size_of::<
                    D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264,
                >())
                .unwrap_or(u32::MAX),
                Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                    pH264GroupOfPictures: std::ptr::from_mut(gop),
                },
            },
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
        let mut pic_data = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264 {
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
            FrameType: D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME,
            pic_parameter_set_id: 0,
            idr_pic_id: u32::from(self.idr_pic_id),
            PictureOrderCountNumber: 0,
            FrameDecodingOrderNumber: 0,
            ..Default::default()
        };
        let picture_control = Self::picture_control(&mut pic_data);
        let cqp = Self::cqp();
        let mut gop = Self::gop();
        let sequence = Self::sequence_control(&cqp, &mut gop, self.width, self.height);
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
            EncoderCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
            EncoderProfile: Self::profile_desc(&mut self.descriptors.profile),
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
    use super::H264Headers;

    use crate::h264::{parse_pps, parse_sps, rbsp, split_nals};

    #[test]
    fn headers_round_trip_through_parser() {
        let headers = H264Headers::build(64, 48).expect("headers");
        assert_eq!(headers.level_idc(), 10);
        let nals = split_nals(headers.bytes());
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
}
