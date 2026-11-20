//! `MediaFoundation` encoder session behind the D3D11 encode path.
//!
//! <https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts>
//! <https://docs.rs/windows/0.62.2/windows/Win32/Media/MediaFoundation/>

// FFI boundary: Media Foundation transform calls live here.
#![allow(unsafe_code)]
// The implement macro expands to inline(always) shims with ref and pointer casts.
#![allow(
    clippy::borrow_as_ptr,
    clippy::inline_always,
    clippy::ptr_as_ptr,
    clippy::ref_as_ptr
)]

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use crate::device::D3d11Device;
use crate::error::{D3d11Error, UnsupportedReason};
use crate::guid::Codec;
use crate::submit::SubmitLock;

use bytes::Bytes;
use rust_video_caps::av1::EncodeProfile as Av1Profile;
use rust_video_caps::color::{
    ColorConfig, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics,
};
use rust_video_caps::common::ChromaSampling;
use rust_video_caps::encode::{
    Decline, EncodeConfig, Gop, Level, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::h264::{EncodeLevel as H264Level, EncodeProfile as H264Profile};
use rust_video_caps::h265::EncodeProfile as H265Profile;
use rust_video_caps::packet::Packet;
use rust_video_caps::reason::UnsupportedReason as StreamCore;
use rust_video_caps::vp9::EncodeProfile as Vp9Profile;
use windows::core::implement;
use windows::core::ComObject;
use windows::core::IUnknown;
use windows::core::Interface;
use windows::core::Ref;
use windows::core::GUID;
use windows::core::PWSTR;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
    eAVEncH264VProfile_Base, eAVEncH264VProfile_High, eAVEncH264VProfile_Main,
    CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonQualityVsSpeed,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncMPVGOPSizeMax, CODECAPI_AVEncMPVGOPSizeMin,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVEncVideoMaxNumRefFrame, ICodecAPI, IMFActivate,
    IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFAttributes, IMFDXGIDeviceManager,
    IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, METransformDrainComplete,
    METransformHaveOutput, METransformNeedInput, MFCreateAlignedMemoryBuffer,
    MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateSample,
    MFMediaType_Video, MFSampleExtension_CleanPoint, MFStartup, MFTEnumEx,
    MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_H265,
    MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_VP90, MFVideoInterlace_Progressive,
    MFASYNC_CALLBACK_QUEUE_MULTITHREADED, MFASYNC_FAST_IO_PROCESSING_CALLBACK, MFSTARTUP_FULL,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SYNCMFT, MFT_INPUT_STREAM_INFO, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_MESSAGE_TYPE,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_TYPE, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_LEVEL, MF_MT_VIDEO_PROFILE,
    MF_SA_D3D11_AWARE, MF_SA_D3D11_BINDFLAGS, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
    MF_VERSION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4, VT_UI4,
};

/// Output frame rate in frames per second.
const FRAME_RATE: u32 = 30;

/// Sample duration in 100ns units at the output frame rate.
const FRAME_HNS: i64 = 10_000_000 / 30;

/// Input buffer alignment in bytes without a stream report.
const SAMPLE_ALIGN: u32 = 16;

/// Bitrate floor in bits per second.
const MIN_BITRATE: u64 = 100_000;

/// Bitrate ceiling in bits per second.
const MAX_BITRATE: u64 = 50_000_000;

/// Landed VP9 quality-vs-speed level; 0 favors speed.
const VP9_QUALITY_VS_SPEED: u32 = 0;

/// Balanced quality-vs-speed level behind the balanced preset.
const BALANCED_QUALITY_VS_SPEED: u32 = 50;

/// Compression quality-vs-speed level behind the quality preset.
const QUALITY_QUALITY_VS_SPEED: u32 = 100;

/// Reference frames ceiling behind the GOP gate.
const MAX_REF_FRAMES: u8 = 16;

/// Consecutive B-frames ceiling behind the GOP gate.
const MAX_B_FRAMES: u8 = 2;

/// Catalog walk bound in entries.
const AVAILABLE_TYPES: u32 = 64;

/// Catalog entry reader behind the type walk.
type Available = unsafe fn(&IMFTransform, u32, u32) -> windows::core::Result<IMFMediaType>;

/// Resolved MF knobs behind one encode session.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EncodePlan {
    /// Codec the session encodes.
    codec: Codec,
    /// Target bitrate in bps, absent for the area-scaled recipe.
    bitrate: Option<u32>,
    /// Codec-API rate-control mode, absent when unset.
    rate_mode: Option<u32>,
    /// Peak bitrate in bps, absent when unset.
    max_bitrate: Option<u32>,
    /// Frame rate in frames per period.
    fps: (u32, u32),
    /// Sample duration in 100ns units.
    frame_hns: i64,
    /// Keyframe cadence in frames, absent when unset.
    gop: Option<u32>,
    /// Minimum keyframe cadence in frames, absent when unset.
    gop_min: Option<u32>,
    /// Maximum keyframe cadence in frames, absent when unset.
    gop_max: Option<u32>,
    /// Consecutive B-frames, absent when unset.
    b_frames: Option<u32>,
    /// Reference frames, absent when unset.
    max_refs: Option<u32>,
    /// Quality-vs-speed level, absent for legacy tuning.
    qvs: Option<u32>,
    /// H.264 profile idc, absent when unset.
    profile: Option<u32>,
    /// H.264 level idc, absent when unset.
    level: Option<u32>,
}

impl EncodePlan {
    /// MF knobs behind `config`.
    ///
    /// Automatic rate control resolves the legacy recipe: area-scaled
    /// bitrate, 30 fps, MFT-picked profile and level, VP9 speed
    /// tuning only. Explicit rate control maps every knob; the VBV
    /// buffer stays MFT-managed and non-H.264 levels stay MFT-picked.
    ///
    /// # Errors
    ///
    /// Returns the first [`Decline`] when `config` fails validation
    /// or exceeds the tier.
    ///
    /// # Arguments
    ///
    /// * `config` - rate, structure, quality, profile, level, depth,
    ///   chroma, frame rate, and color behind the session.
    pub(crate) fn resolve(config: &EncodeConfig) -> Result<Self, Decline> {
        config.validate()?;
        let codec = config.codec();
        let allowed = Self::check_profile(config.profile)?;
        Self::check_input(config.bit_depth, config.chroma)?;
        Self::check_color(config.color)?;
        let gop = config.gop;
        Self::check_gop(gop)?;
        let (bitrate, rate_mode, max_bitrate, legacy) = match config.rate_control {
            RateControl::Auto => (None, None, None, true),
            RateControl::Cbr {
                bitrate, maxrate, ..
            } => (
                Some(bitrate),
                Some(u32_flag(eAVEncCommonRateControlMode_CBR.0)),
                Some(maxrate),
                false,
            ),
            RateControl::Vbr { bitrate, maxrate } => (
                Some(bitrate),
                Some(u32_flag(eAVEncCommonRateControlMode_PeakConstrainedVBR.0)),
                Some(maxrate),
                false,
            ),
            RateControl::Cqp { .. } | RateControl::Crf { .. } => {
                return Err(Decline::RateControlUnsupported {
                    mode: config.rate_control,
                });
            }
        };
        if legacy {
            return Ok(Self {
                codec,
                bitrate: None,
                rate_mode: None,
                max_bitrate: None,
                fps: (FRAME_RATE, 1),
                frame_hns: FRAME_HNS,
                gop: None,
                gop_min: None,
                gop_max: None,
                b_frames: None,
                max_refs: None,
                qvs: None,
                profile: None,
                level: None,
            });
        }
        let level = match config.level {
            Level::H264(H264Level::Pinned(level)) => Some(u32::from(level.idc())),
            _ => None,
        };
        Ok(Self {
            codec,
            bitrate,
            rate_mode,
            max_bitrate,
            fps: (config.frame_rate.num, config.frame_rate.den),
            frame_hns: 10_000_000i64.saturating_mul(i64::from(config.frame_rate.den))
                / i64::from(config.frame_rate.num),
            gop: Some(gop.keyint),
            gop_min: Some(gop.min_keyint),
            gop_max: Some(gop.keyint),
            b_frames: Some(u32::from(gop.b_frames)),
            max_refs: Some(u32::from(gop.refs)),
            qvs: Some(match config.quality.preset {
                SpeedPreset::Realtime => VP9_QUALITY_VS_SPEED,
                SpeedPreset::Balanced => BALANCED_QUALITY_VS_SPEED,
                SpeedPreset::Quality => QUALITY_QUALITY_VS_SPEED,
            }),
            profile: allowed,
            level,
        })
    }

    /// Signaled H.264 profile idc behind `profile`.
    ///
    /// # Errors
    ///
    /// Returns [`Decline::ProfileUnsupported`] for a profile outside
    /// the 8-bit 4:2:0 allowlist.
    ///
    /// # Arguments
    ///
    /// * `profile` - requested encode profile.
    fn check_profile(profile: Profile) -> Result<Option<u32>, Decline> {
        Ok(match profile {
            Profile::H264(H264Profile::Baseline) => Some(u32_flag(eAVEncH264VProfile_Base.0)),
            Profile::H264(H264Profile::Main) => Some(u32_flag(eAVEncH264VProfile_Main.0)),
            Profile::H264(H264Profile::High) => Some(u32_flag(eAVEncH264VProfile_High.0)),
            Profile::H265(H265Profile::Main)
            | Profile::Av1(Av1Profile::Main)
            | Profile::Vp9(Vp9Profile::P0) => None,
            _ => return Err(Decline::ProfileUnsupported { profile }),
        })
    }

    /// 8-bit 4:2:0 input behind `depth` and `chroma`.
    ///
    /// # Errors
    ///
    /// Returns [`Decline::BitDepthUnsupported`] above 8 bit or
    /// [`Decline::ChromaUnsupported`] outside 4:2:0.
    ///
    /// # Arguments
    ///
    /// * `depth` - requested bits per sample.
    /// * `chroma` - requested chroma sampling.
    fn check_input(depth: u8, chroma: ChromaSampling) -> Result<(), Decline> {
        if depth != 8 {
            return Err(Decline::BitDepthUnsupported {
                actual: depth,
                max: 8,
            });
        }
        if chroma != ChromaSampling::Yuv420 {
            return Err(Decline::ChromaUnsupported { chroma });
        }
        Ok(())
    }

    /// BT.709 SDR behind `color`.
    ///
    /// # Errors
    ///
    /// Returns [`Decline::HdrUnsupported`] with HDR metadata or the
    /// per-component decline off the BT.709 baseline.
    ///
    /// # Arguments
    ///
    /// * `color` - requested color description.
    fn check_color(color: ColorConfig) -> Result<(), Decline> {
        if color.is_hdr() {
            return Err(Decline::HdrUnsupported);
        }
        if color.primaries != ColorPrimaries::Bt709 {
            return Err(Decline::PrimariesUnsupported {
                primaries: color.primaries,
            });
        }
        if color.transfer != TransferCharacteristics::Bt709 {
            return Err(Decline::TransferUnsupported {
                transfer: color.transfer,
            });
        }
        if color.matrix != MatrixCoefficients::Bt709 {
            return Err(Decline::MatrixUnsupported {
                matrix: color.matrix,
            });
        }
        if color.range != ColorRange::Limited {
            return Err(Decline::RangeUnsupported { range: color.range });
        }
        Ok(())
    }

    /// Bounded prediction budget behind `gop`.
    ///
    /// # Errors
    ///
    /// Returns [`Decline::GopInvalid`] without references,
    /// [`Decline::RefsUnsupported`] or [`Decline::BFramesUnsupported`]
    /// past the tier ceilings.
    ///
    /// # Arguments
    ///
    /// * `gop` - requested keyframe cadence plus prediction budget.
    fn check_gop(gop: Gop) -> Result<(), Decline> {
        if gop.refs == 0 {
            return Err(Decline::GopInvalid { gop });
        }
        if gop.refs > MAX_REF_FRAMES {
            return Err(Decline::RefsUnsupported {
                actual: gop.refs,
                max: MAX_REF_FRAMES,
            });
        }
        if gop.b_frames > MAX_B_FRAMES {
            return Err(Decline::BFramesUnsupported {
                actual: gop.b_frames,
                max: MAX_B_FRAMES,
            });
        }
        Ok(())
    }
}

/// Negotiated encoder session over one `MediaFoundation` transform.
pub(crate) struct Session {
    /// Encoder transform.
    transform: IMFTransform,
    /// Input stream id.
    input: u32,
    /// Output stream id.
    output: u32,
    /// Transform allocates its own output samples.
    provides: bool,
    /// Output buffer length in bytes.
    out_size: u32,
    /// Output buffer alignment in bytes.
    out_align: u32,
    /// Input buffer alignment in bytes.
    in_align: u32,
    /// Async event pump, absent on synchronous transforms.
    pump: Option<EventPump>,
    /// Transform D3D11 awareness.
    d3d11_aware: bool,
    /// D3D11 bind behind GPU samples, absent without a bound transform.
    gpu: Option<GpuBind>,
    /// Serving encoder MFT name.
    name: String,
    /// Untried encoder activates in priority order.
    standby: VecDeque<IMFActivate>,
    /// Output subtype behind the negotiated media type.
    subtype: GUID,
    /// Codec the session encodes.
    codec: Codec,
    /// Resolved MF knobs behind the session.
    plan: EncodePlan,
    /// Applied VP9 speed tuning, absent off the VP9 path or without the property.
    vp9_tuning: Option<Vp9Tuning>,
    /// Negotiated extent, absent before the first frame.
    size: Option<(u32, u32)>,
    /// Next input timestamp in 100ns units.
    pts: i64,
    /// Queued input credits from the event pump.
    credits: u32,
    /// Consumed input samples for reuse at the negotiated extent.
    spare_inputs: Vec<IMFSample>,
}

impl Session {
    /// Session over `device` for `config` behind the first matching encoder MFT.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `config` fails validation or exceeds the
    /// tier, without `MediaFoundation`, or without a `config` codec
    /// encoder MFT on the machine.
    pub(crate) fn open(device: &D3d11Device, config: EncodeConfig) -> Result<Self, D3d11Error> {
        Self::open_windows(device, config)
    }

    /// Session over `device` for `config`.
    fn open_windows(device: &D3d11Device, config: EncodeConfig) -> Result<Self, D3d11Error> {
        let plan = EncodePlan::resolve(&config)?;
        let codec = plan.codec;
        let _com = ComGuard::enter();
        startup()?;
        let mut standby = VecDeque::new();
        let mut subtype = MFVideoFormat_H264;
        for candidate in subtypes(codec) {
            let matches = enumerate(candidate);
            if !matches.is_empty() {
                standby = matches.into_iter().collect();
                subtype = *candidate;
                break;
            }
        }
        let Some((opened, pump)) = elect_armed(&mut standby, codec) else {
            return Err(missing_encoder(codec));
        };
        let gpu = GpuBind::open(device, opened.d3d11_aware);
        Ok(Self {
            transform: opened.transform,
            input: opened.input,
            output: opened.output,
            provides: false,
            out_size: 0,
            out_align: 1,
            in_align: SAMPLE_ALIGN,
            pump,
            d3d11_aware: opened.d3d11_aware,
            gpu,
            name: opened.name,
            standby,
            subtype,
            codec,
            plan,
            vp9_tuning: None,
            size: None,
            pts: 0,
            credits: 0,
            spare_inputs: Vec::new(),
        })
    }

    /// Next arming transform from the standby queue.
    fn adopt(&mut self) -> Result<(), D3d11Error> {
        let Some((opened, pump)) = elect_armed(&mut self.standby, self.codec) else {
            return Err(missing_encoder(self.codec));
        };
        self.transform = opened.transform;
        self.pump = pump;
        self.d3d11_aware = opened.d3d11_aware;
        self.name = opened.name;
        self.input = opened.input;
        self.output = opened.output;
        self.vp9_tuning = None;
        self.in_align = SAMPLE_ALIGN;
        self.credits = 0;
        self.spare_inputs.clear();
        Ok(())
    }

    /// True when the transform reports D3D11 awareness.
    #[must_use]
    pub(crate) fn uses_d3d11(&self) -> bool {
        self.d3d11_aware
    }

    /// True when the session encodes from GPU textures.
    #[must_use]
    pub(crate) fn gpu_bound(&self) -> bool {
        self.gpu.is_some()
    }

    /// Name of the serving encoder MFT.
    #[must_use]
    pub(crate) fn mft_name(&self) -> &str {
        &self.name
    }

    /// Applied VP9 speed tuning, absent off the VP9 path or without the property.
    #[must_use]
    pub(crate) fn vp9_tuning(&self) -> Option<Vp9Tuning> {
        self.vp9_tuning
    }

    /// Packets coded from one NV12 frame of `width` x `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the extent overflows the sample buffer, or when
    /// the transform refuses the frame.
    pub(crate) fn encode(
        &mut self,
        width: u32,
        height: u32,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Vec<Packet>, D3d11Error> {
        let _com = ComGuard::enter();
        let mut packets = self.ensure_size(width, height)?;
        let sample = self.take_input_sample(nv12_len(width, height), fill)?;
        self.submit_sample(&sample, &mut packets)?;
        Ok(packets)
    }

    /// Input sample of `len` bytes at the next stamp, filled by `fill`.
    ///
    /// Reuses a consumed sample when one waits; the pool only ever
    /// holds the negotiated extent.
    fn take_input_sample(
        &mut self,
        len: u64,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<IMFSample, D3d11Error> {
        let pts = self.pts;
        let hns = self.plan.frame_hns;
        if let Some(sample) = self.spare_inputs.pop() {
            let bytes = u32::try_from(len)
                .map_err(|_| D3d11Error::Unsupported(UnsupportedReason::FrameOver4Gib))?;
            fill_sample(&sample, bytes, pts, hns, fill)?;
            return Ok(sample);
        }
        input_sample(len, self.in_align, pts, hns, fill)
    }

    /// Drained packets across the size switch to `width` x `height`.
    fn ensure_size(&mut self, width: u32, height: u32) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        if self.size != Some((width, height)) {
            if self.size.is_some() {
                packets.append(&mut self.drain()?);
                self.message(MFT_MESSAGE_COMMAND_FLUSH)?;
            }
            self.spare_inputs.clear();
            self.configure(width, height)?;
        }
        Ok(packets)
    }

    /// `sample` through the sync or async submit path into `packets`.
    fn submit_sample(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        self.pts = self.pts.wrapping_add(self.plan.frame_hns);
        let outcome = if self.pump.is_some() {
            self.submit_async(sample, packets)
        } else {
            self.submit_sync(sample, packets)
        };
        if outcome.is_err() {
            self.reset_after_failure();
            return outcome;
        }
        let stamped = sample_time(sample);
        if packets.iter().any(|packet| packet.pts() == Some(stamped)) {
            self.spare_inputs.push(sample.clone());
        }
        outcome
    }

    /// Transform flushed plus the pump reset behind a submit failure.
    fn reset_after_failure(&self) {
        let _ = self.message(MFT_MESSAGE_COMMAND_FLUSH);
        if let Some(pump) = &self.pump {
            pump.reset();
        }
    }

    /// Packets coded from the `slice` decode-texture region at (`left`, `top`) sized `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a D3D11 bind, for an odd extent, or when the
    /// transform refuses the frame.
    pub(crate) fn encode_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<Packet>, D3d11Error> {
        let _com = ComGuard::enter();
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::OddNv12Extent,
            )));
        }
        let mut packets = self.ensure_size(width, height)?;
        let region = Region {
            slice,
            left,
            top,
            width,
            height,
        };
        let gpu = self.gpu.as_mut().ok_or(D3d11Error::Unsupported(
            UnsupportedReason::EncoderWithoutGpuImport,
        ))?;
        let sample = gpu.sample(&self.transform, self.input, texture, &region, self.pts)?;
        self.submit_sample(&sample, &mut packets)?;
        Ok(packets)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the transform refuses the drain.
    pub(crate) fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        let _com = ComGuard::enter();
        if self.size.is_none() {
            return Ok(Vec::new());
        }
        self.drain()
    }

    /// Next coded picture forced to IDR.
    ///
    /// One-shot nonzero
    /// [AVEncVideoForceKeyFrame](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideoforcekeyframe)
    /// through the codec API; stream start codes IDR, so an
    /// unconfigured session reports success without touching the
    /// transform.
    ///
    /// # Errors
    ///
    /// Returns `Declined(KeyframeUnsupported)` without the codec
    /// interface or when the transform refuses the property.
    pub(crate) fn request_keyframe(&mut self) -> Result<(), D3d11Error> {
        if self.size.is_none() {
            return Ok(());
        }
        let _com = ComGuard::enter();
        let Ok(codec) = self.codec_api() else {
            return Err(D3d11Error::Declined(Decline::KeyframeUnsupported));
        };
        if set_codec_u32(&codec, &CODECAPI_AVEncVideoForceKeyFrame, 1) {
            Ok(())
        } else {
            Err(D3d11Error::Declined(Decline::KeyframeUnsupported))
        }
    }

    /// Media types for `width` x `height`, falling back across MFTs.
    fn configure(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        loop {
            match self.configure_try(width, height) {
                Ok(()) => return Ok(()),
                Err(_) if !self.standby.is_empty() => self.adopt()?,
                Err(error) if self.size.is_some() => return Err(error),
                Err(_) => return Err(missing_encoder(self.codec)),
            }
        }
    }

    /// Media types for `width` x `height` plus streaming start.
    fn configure_try(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        self.set_output_type(width, height)?;
        self.set_input_type(width, height)?;
        self.tune();
        let bound = self
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.attach(&self.transform).is_ok());
        if !bound {
            self.gpu = None;
        }
        self.message(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)?;
        self.message(MFT_MESSAGE_NOTIFY_START_OF_STREAM)?;
        let mut info = MFT_INPUT_STREAM_INFO::default();
        // Sound: the info stays live for the call; failure keeps the default align.
        if unsafe {
            self.transform
                .GetInputStreamInfo(self.input, std::ptr::addr_of_mut!(info))
        }
        .is_ok()
        {
            self.in_align = info.cbAlignment.max(1);
        }
        self.refresh_output_info(width, height)?;
        self.size = Some((width, height));
        Ok(())
    }

    /// Output media type for `width` x `height` behind the target bitrate.
    fn set_output_type(&self, width: u32, height: u32) -> Result<(), D3d11Error> {
        let target = self.plan.bitrate.unwrap_or_else(|| bitrate(width, height));
        let output_type = find_available(
            &self.transform,
            self.output,
            &self.subtype,
            IMFTransform::GetOutputAvailableType,
        )
        .map_or_else(|| media_type(self.subtype), Ok)?;
        stamp_output(
            &output_type,
            width,
            height,
            self.plan.fps,
            target,
            self.plan.profile,
            self.plan.level,
        )?;
        // Sound: the transform and type are live.
        call(
            unsafe { self.transform.SetOutputType(self.output, &output_type, 0) },
            "SetOutputType",
        )
    }

    /// Input NV12 media type for `width` x `height`.
    fn set_input_type(&self, width: u32, height: u32) -> Result<(), D3d11Error> {
        let input_type = find_available(
            &self.transform,
            self.input,
            &MFVideoFormat_NV12,
            IMFTransform::GetInputAvailableType,
        )
        .map_or_else(|| media_type(MFVideoFormat_NV12), Ok)?;
        stamp_input(&input_type, width, height, self.plan.fps)?;
        // Sound: the transform and type are live.
        call(
            unsafe { self.transform.SetInputType(self.input, &input_type, 0) },
            "SetInputType",
        )
    }

    /// Codec-API knobs behind the plan, each property best-effort.
    fn tune(&mut self) {
        self.vp9_tuning = None;
        let Ok(codec) = self.codec_api() else {
            return;
        };
        if self.codec == Codec::Vp9 {
            self.vp9_tuning = match self.plan.qvs {
                Some(level) => Vp9Tuning::apply_level(&codec, level),
                None => Vp9Tuning::apply(&codec),
            };
        } else if let Some(level) = self.plan.qvs {
            set_codec_u32(&codec, &CODECAPI_AVEncCommonQualityVsSpeed, level);
        }
        if let Some(mode) = self.plan.rate_mode {
            set_codec_u32(&codec, &CODECAPI_AVEncCommonRateControlMode, mode);
        }
        if let Some(max) = self.plan.max_bitrate {
            set_codec_u32(&codec, &CODECAPI_AVEncCommonMaxBitRate, max);
        }
        if let Some(gop) = self.plan.gop {
            set_codec_u32(&codec, &CODECAPI_AVEncMPVGOPSize, gop);
        }
        if let Some(min) = self.plan.gop_min {
            set_codec_u32(&codec, &CODECAPI_AVEncMPVGOPSizeMin, min);
        }
        if let Some(max) = self.plan.gop_max {
            set_codec_u32(&codec, &CODECAPI_AVEncMPVGOPSizeMax, max);
        }
        if let Some(b) = self.plan.b_frames {
            set_codec_u32(&codec, &CODECAPI_AVEncMPVDefaultBPictureCount, b);
        }
        if let Some(refs) = self.plan.max_refs {
            set_codec_u32(&codec, &CODECAPI_AVEncVideoMaxNumRefFrame, refs);
        }
    }

    /// Codec API behind the transform, absent without the interface.
    fn codec_api(&self) -> Result<ICodecAPI, D3d11Error> {
        // Sound: the transform is live.
        call(self.transform.cast(), "QueryInterface")
    }

    /// Output buffer shape behind the current output type.
    fn refresh_output_info(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        // Sound: the transform is live past the output set.
        let info = call(
            unsafe { self.transform.GetOutputStreamInfo(self.output) },
            "GetOutputStreamInfo",
        )?;
        self.provides = info.dwFlags & u32_flag(MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0) != 0;
        let fallback = u32::try_from(nv12_len(width, height)).unwrap_or(u32::MAX);
        self.out_size = if info.cbSize == 0 {
            fallback
        } else {
            info.cbSize
        };
        self.out_align = info.cbAlignment.max(1);
        Ok(())
    }

    /// Output type behind a stream change.
    fn renegotiate(&mut self) -> Result<(), D3d11Error> {
        // Sound: the transform is live past the stream change.
        let media = call(
            unsafe { self.transform.GetOutputAvailableType(self.output, 0) },
            "GetOutputAvailableType",
        )?;
        call(
            unsafe { self.transform.SetOutputType(self.output, &media, 0) },
            "SetOutputType",
        )?;
        let (width, height) = self.size.unwrap_or((0, 0));
        self.refresh_output_info(width, height)
    }

    /// `message` through the transform.
    fn message(&self, message: MFT_MESSAGE_TYPE) -> Result<(), D3d11Error> {
        // Sound: the transform is live.
        call(
            unsafe { self.transform.ProcessMessage(message, 0) },
            "ProcessMessage",
        )
    }

    /// `sample` through a synchronous transform into `packets`.
    fn submit_sync(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        // Sound: the transform and sample are live.
        match unsafe { self.transform.ProcessInput(self.input, sample, 0) } {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_NOTACCEPTING => {
                self.collect_sync(packets)?;
                call(
                    unsafe { self.transform.ProcessInput(self.input, sample, 0) },
                    "ProcessInput",
                )?;
            }
            Err(error) => return call(Err(error), "ProcessInput"),
        }
        self.collect_sync(packets)
    }

    /// `sample` through an asynchronous transform into `packets`.
    fn submit_async(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        self.await_credit(packets)?;
        // Sound: the transform and sample are live past the input credit.
        call(
            unsafe { self.transform.ProcessInput(self.input, sample, 0) },
            "ProcessInput",
        )?;
        self.poll(packets)?;
        self.await_output(sample, packets)
    }

    /// Output stamped like `sample` into `packets`.
    ///
    /// The transform answers asynchronously; without this wait each call
    /// returns the previous frame's packet.
    fn await_output(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        let want = sample_time(sample);
        loop {
            if packets.iter().any(|packet| packet.pts() == Some(want)) {
                return Ok(());
            }
            let typ = self.async_pump()?.pop()?;
            if is_event(typ, METransformHaveOutput) {
                self.try_output(packets)?;
            } else if is_event(typ, METransformNeedInput) {
                self.credits += 1;
            }
        }
    }

    /// One input credit, collecting outputs queued ahead of it.
    fn await_credit(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        if self.credits > 0 {
            self.credits -= 1;
            return Ok(());
        }
        loop {
            let typ = self.async_pump()?.pop()?;
            if is_event(typ, METransformNeedInput) {
                return Ok(());
            }
            if is_event(typ, METransformHaveOutput) {
                self.try_output(packets)?;
            }
        }
    }

    /// Queued outputs into `packets` without blocking.
    fn poll(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        for typ in self.async_pump()?.drain()? {
            if is_event(typ, METransformHaveOutput) {
                self.try_output(packets)?;
            } else if is_event(typ, METransformNeedInput) {
                self.credits += 1;
            }
        }
        Ok(())
    }

    /// Async pump, absent on synchronous transforms.
    fn async_pump(&self) -> Result<&EventPump, D3d11Error> {
        self.pump.as_ref().ok_or(D3d11Error::Unsupported(
            UnsupportedReason::AsyncMftWithoutEventPump,
        ))
    }

    /// Drained outputs with the session left streaming.
    fn drain(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        self.message(MFT_MESSAGE_COMMAND_DRAIN)?;
        if self.pump.is_some() {
            loop {
                let typ = self.async_pump()?.pop()?;
                if is_event(typ, METransformDrainComplete) {
                    break;
                }
                if is_event(typ, METransformHaveOutput) {
                    self.try_output(&mut packets)?;
                } else if is_event(typ, METransformNeedInput) {
                    self.credits += 1;
                }
            }
            // Draining ends the streaming segment; re-arm input requests.
            self.message(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)?;
            self.message(MFT_MESSAGE_NOTIFY_START_OF_STREAM)?;
        } else {
            self.collect_sync(&mut packets)?;
        }
        Ok(packets)
    }

    /// Available outputs into `packets` until the transform starves.
    fn collect_sync(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        while self.try_output(packets)? {}
        Ok(())
    }

    /// One output attempt; true when a sample may follow.
    fn try_output(&mut self, packets: &mut Vec<Packet>) -> Result<bool, D3d11Error> {
        loop {
            let mut buffer = self.output_buffer()?;
            let mut status = 0u32;
            // Sound: the buffer and status stay live for the call.
            let result = unsafe {
                self.transform.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut buffer),
                    std::ptr::addr_of_mut!(status),
                )
            };
            // Sound: the buffer slot is ours in every outcome.
            let held = unsafe { ManuallyDrop::take(&mut buffer.pSample) };
            match result {
                Ok(()) => {
                    if let Some(sample) = held {
                        packets.push(extract(&sample)?);
                    }
                    if buffer.dwStatus & u32_flag(MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE.0) != 0 {
                        self.renegotiate()?;
                    }
                    return Ok(true);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Ok(false);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.renegotiate()?;
                }
                Err(error) => return call(Err(error), "ProcessOutput"),
            }
        }
    }

    /// Output buffer with a sample unless the transform provides.
    fn output_buffer(&self) -> Result<MFT_OUTPUT_DATA_BUFFER, D3d11Error> {
        let sample = if self.provides {
            None
        } else {
            // Sound: the sample and buffer are live for the output call.
            let sample = call(unsafe { MFCreateSample() }, "MFCreateSample")?;
            let buffer = call(
                unsafe { MFCreateAlignedMemoryBuffer(self.out_size, self.out_align) },
                "MFCreateAlignedMemoryBuffer",
            )?;
            call(unsafe { sample.AddBuffer(&buffer) }, "AddBuffer")?;
            Some(sample)
        };
        Ok(MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        })
    }
}

/// Applied VP9 encoder speed tuning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Vp9Tuning {
    /// Quality-vs-speed level read back from the MFT.
    quality_vs_speed: u32,
}

impl Vp9Tuning {
    /// Fastest tuning through `codec`, absent without the speed property.
    pub(crate) fn apply(codec: &ICodecAPI) -> Option<Self> {
        Self::apply_level(codec, VP9_QUALITY_VS_SPEED)
    }

    /// `level` through `codec`, absent without the speed property.
    ///
    /// 0 favors speed over quality per
    /// [AVEncCommonQualityVsSpeed](https://learn.microsoft.com/en-us/windows/win32/directshow/avenccommonqualityvsspeed-property).
    pub(crate) fn apply_level(codec: &ICodecAPI, level: u32) -> Option<Self> {
        if !set_codec_u32(codec, &CODECAPI_AVEncCommonQualityVsSpeed, level) {
            return None;
        }
        // Sound: the codec is live; the property was just set.
        let current = unsafe { codec.GetValue(&CODECAPI_AVEncCommonQualityVsSpeed) }.ok()?;
        read_u32(&current).map(|quality_vs_speed| Self { quality_vs_speed })
    }

    /// Quality-vs-speed level read back from the MFT.
    #[must_use]
    pub fn quality_vs_speed(&self) -> u32 {
        self.quality_vs_speed
    }
}

/// `value` behind `property` on `codec`; false when the MFT refuses it.
fn set_codec_u32(codec: &ICodecAPI, property: &GUID, value: u32) -> bool {
    let value = variant_u32(value);
    // Sound: the codec and value are live for the set.
    unsafe { codec.SetValue(property, &value) }.is_ok()
}

/// `VARIANT` holding `value` as `VT_UI4`.
fn variant_u32(value: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: value },
            }),
        },
    }
}

/// `VT_UI4`/`VT_I4` payload of `variant`, absent for other types.
fn read_u32(variant: &VARIANT) -> Option<u32> {
    // Sound: the union reads match the stored type tag.
    unsafe {
        let inner = &variant.Anonymous.Anonymous;
        if inner.vt == VT_UI4 {
            Some(inner.Anonymous.ulVal)
        } else if inner.vt == VT_I4 {
            u32::try_from(inner.Anonymous.lVal).ok()
        } else {
            None
        }
    }
}

/// Asynchronous event pump over one transform event source.
struct EventPump {
    /// Async event source.
    events: IMFMediaEventGenerator,
    /// Callback object behind outstanding requests.
    callback: IMFAsyncCallback,
    /// Next-event queue.
    queue: Arc<EventQueue>,
}

impl EventPump {
    /// Pump over `events` with one outstanding request.
    fn open(events: IMFMediaEventGenerator) -> Result<Self, D3d11Error> {
        let queue = Arc::new(EventQueue::new());
        let callback: IMFAsyncCallback = ComObject::new(PumpCallback {
            events: events.clone(),
            queue: queue.clone(),
        })
        .into_interface();
        let state: Option<&IUnknown> = None;
        // Sound: the source and callback are live; no state object.
        call(
            unsafe { events.BeginGetEvent(&callback, state) },
            "BeginGetEvent",
        )?;
        Ok(Self {
            events,
            callback,
            queue,
        })
    }

    /// Next event type, re-arming the request chain.
    fn pop(&self) -> Result<u32, D3d11Error> {
        let event = self.queue.pop();
        self.rearm();
        event.map_err(|hresult| D3d11Error::Call {
            call: "GetEvent",
            hresult,
        })
    }

    /// Queued event types without waiting, re-arming per event.
    fn drain(&self) -> Result<Vec<u32>, D3d11Error> {
        let mut types = Vec::new();
        for event in self.queue.drain() {
            self.rearm();
            types.push(event.map_err(|hresult| D3d11Error::Call {
                call: "GetEvent",
                hresult,
            })?);
        }
        Ok(types)
    }

    /// Queue cleared plus a fresh request behind failures.
    fn reset(&self) {
        self.queue.reset();
        self.rearm();
    }

    /// One request behind the consumed event, failing the queue when refused.
    fn rearm(&self) {
        let state: Option<&IUnknown> = None;
        // Sound: the source and callback are live past the consumed request.
        if let Err(error) = unsafe { self.events.BeginGetEvent(&self.callback, state) } {
            self.queue.push(Err(error.code().0));
        }
    }
}

impl Drop for EventPump {
    // Outstanding request abandoned; blocked waiters wake with `E_ABORT`.
    fn drop(&mut self) {
        self.queue.push(Err(windows::Win32::Foundation::E_ABORT.0));
    }
}

/// Next-event queue behind one asynchronous transform.
struct EventQueue {
    /// Pending event types plus sticky failure.
    state: Mutex<QueueState>,
    /// Signalled on every push.
    ready: Condvar,
}

/// Pending event types plus sticky failure.
struct QueueState {
    /// Event types in arrival order.
    pending: VecDeque<u32>,
    /// Fatal pump failure, reported until reset.
    failed: Option<i32>,
}

impl EventQueue {
    /// Empty queue.
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                pending: VecDeque::new(),
                failed: None,
            }),
            ready: Condvar::new(),
        }
    }

    /// `event` queued for the submitter thread.
    fn push(&self, event: Result<u32, i32>) {
        {
            let mut state = self.guard();
            match event {
                Ok(typ) => state.pending.push_back(typ),
                Err(hresult) => {
                    state.failed.get_or_insert(hresult);
                }
            }
        }
        self.ready.notify_one();
    }

    /// Next event type, waiting for arrival.
    fn pop(&self) -> Result<u32, i32> {
        let mut state = self.guard();
        loop {
            if let Some(hresult) = state.failed {
                return Err(hresult);
            }
            if let Some(typ) = state.pending.pop_front() {
                return Ok(typ);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Queued event types without waiting.
    fn drain(&self) -> Vec<Result<u32, i32>> {
        let mut state = self.guard();
        if let Some(hresult) = state.failed {
            return vec![Err(hresult)];
        }
        state.pending.drain(..).map(Ok).collect()
    }

    /// Pending types and failures cleared.
    fn reset(&self) {
        let mut state = self.guard();
        state.pending.clear();
        state.failed = None;
    }

    /// Queue state, recovered past a poisoned lock.
    fn guard(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Event callback behind the pump; signals only, never GPU work.
#[implement(IMFAsyncCallback)]
struct PumpCallback {
    /// Async event source behind completions.
    events: IMFMediaEventGenerator,
    /// Next-event queue.
    queue: Arc<EventQueue>,
}

impl IMFAsyncCallback_Impl for PumpCallback_Impl {
    fn GetParameters(&self, flags: *mut u32, queue: *mut u32) -> windows::core::Result<()> {
        // Sound: Media Foundation provides valid out-pointers.
        unsafe {
            *flags = MFASYNC_FAST_IO_PROCESSING_CALLBACK;
            *queue = MFASYNC_CALLBACK_QUEUE_MULTITHREADED;
        }
        Ok(())
    }

    fn Invoke(&self, result: Ref<'_, IMFAsyncResult>) -> windows::core::Result<()> {
        self.queue.push(self.completed(&result));
        Ok(())
    }
}

impl PumpCallback {
    /// Finished event behind `result`, without GPU work.
    fn completed(&self, result: &Ref<'_, IMFAsyncResult>) -> Result<u32, i32> {
        let completed = match result.ok() {
            Ok(completed) => completed,
            Err(error) => return Err(error.code().0),
        };
        // Sound: the source and result are live on the callback thread.
        let event = match unsafe { self.events.EndGetEvent(completed) } {
            Ok(event) => event,
            Err(error) => return Err(error.code().0),
        };
        // Sound: the event is live.
        let status = match unsafe { event.GetStatus() } {
            Ok(status) => status,
            Err(error) => return Err(error.code().0),
        };
        if status.is_err() {
            return Err(status.0);
        }
        // Sound: the event is live.
        unsafe { event.GetType() }.map_err(|error| error.code().0)
    }
}

/// COM apartment guard; uninitializes only a self-owned entry.
struct ComGuard {
    /// Thread entry owned by this guard.
    owned: bool,
}

impl ComGuard {
    /// Multithreaded entry for the calling thread.
    fn enter() -> Self {
        // Sound: no reserved context crosses the call.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            owned: entered == windows::Win32::Foundation::S_OK,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            // Sound: this guard owns the matching entry.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// Live parts behind one encoder activate.
struct Opened {
    /// Encoder transform.
    transform: IMFTransform,
    /// Async event source, absent on synchronous transforms.
    events: Option<IMFMediaEventGenerator>,
    /// Transform D3D11 awareness.
    d3d11_aware: bool,
    /// Serving encoder MFT name.
    name: String,
    /// Input stream id.
    input: u32,
    /// Output stream id.
    output: u32,
}

/// D3D11 bind behind GPU encoder samples.
struct GpuBind {
    /// DXGI device manager behind the bound transform.
    manager: IMFDXGIDeviceManager,
    /// Logical device behind input textures.
    device: ID3D11Device,
    /// Immediate context behind GPU copies.
    context: ID3D11DeviceContext,
    /// Single-submitter lock over the immediate context.
    submit: Arc<SubmitLock>,
    /// Input texture plus its extent, recreated per size.
    input: Option<(ID3D11Texture2D, u32, u32)>,
}

impl GpuBind {
    /// Manager over `device`, absent without D3D11 awareness.
    fn open(device: &D3d11Device, aware: bool) -> Option<Self> {
        if !aware {
            return None;
        }
        let mut token = 0u32;
        let mut manager = None;
        // Sound: the out-parameters stay live for the call.
        unsafe {
            MFCreateDXGIDeviceManager(
                std::ptr::addr_of_mut!(token),
                std::ptr::addr_of_mut!(manager),
            )
        }
        .ok()?;
        let manager: IMFDXGIDeviceManager = manager?;
        // Sound: the device is live; the token came from the create call.
        unsafe { manager.ResetDevice(device.device(), token) }.ok()?;
        Some(Self {
            manager,
            device: device.device().clone(),
            context: device.context().clone(),
            submit: device.submit_lock().clone(),
            input: None,
        })
    }

    /// Manager through `transform` as its D3D11 device manager.
    fn attach(&self, transform: &IMFTransform) -> Result<(), D3d11Error> {
        send_manager(transform, &self.manager)
    }

    /// Input sample over `region` of `texture`.
    fn sample(
        &mut self,
        transform: &IMFTransform,
        input: u32,
        texture: &ID3D11Texture2D,
        region: &Region,
        pts: i64,
    ) -> Result<IMFSample, D3d11Error> {
        let target = self.texture(transform, input, region.width, region.height)?;
        // 4:2:0 crop origins stay even; odd origins break planar copies.
        let src_box = D3D11_BOX {
            left: region.left,
            top: region.top,
            front: 0,
            right: region.left.saturating_add(region.width),
            bottom: region.top.saturating_add(region.height),
            back: 1,
        };
        let _submit = self.submit.enter()?;
        // Sound: both textures are live; the slice and box name the decode extent.
        unsafe {
            self.context.CopySubresourceRegion(
                &target,
                0,
                0,
                0,
                0,
                texture,
                region.slice,
                Some(std::ptr::from_ref(&src_box)),
            );
        }
        // Sound: the texture is live past the copy.
        let buffer = call(
            unsafe {
                MFCreateDXGISurfaceBuffer(
                    std::ptr::from_ref(&ID3D11Texture2D::IID),
                    &target,
                    0,
                    false,
                )
            },
            "MFCreateDXGISurfaceBuffer",
        )?;
        // Sound: the sample and buffer are live for every set below.
        let sample = call(unsafe { MFCreateSample() }, "MFCreateSample")?;
        call(unsafe { sample.AddBuffer(&buffer) }, "AddBuffer")?;
        call(unsafe { sample.SetSampleTime(pts) }, "SetSampleTime")?;
        call(
            unsafe { sample.SetSampleDuration(FRAME_HNS) },
            "SetSampleDuration",
        )?;
        Ok(sample)
    }

    /// Input texture at `width` by `height`, honoring the MFT bind flags.
    fn texture(
        &mut self,
        transform: &IMFTransform,
        input: u32,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D, D3d11Error> {
        if let Some((texture, cached_width, cached_height)) = &self.input {
            if (*cached_width, *cached_height) == (width, height) {
                return Ok(texture.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind_flags(transform, input),
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        // Sound: the descriptor stays live for the call.
        unsafe {
            self.device.CreateTexture2D(
                std::ptr::addr_of!(desc),
                None,
                Some(std::ptr::addr_of_mut!(texture)),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateTexture2D",
            hresult: error.code().0,
        })?;
        let texture: ID3D11Texture2D = texture.ok_or(D3d11Error::Call {
            call: "CreateTexture2D",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        })?;
        self.input = Some((texture.clone(), width, height));
        Ok(texture)
    }
}

/// Visible NV12 region inside a decode texture.
struct Region {
    /// Array slice behind the picture.
    slice: u32,
    /// Visible left offset in luma samples.
    left: u32,
    /// Visible top offset in luma samples.
    top: u32,
    /// Visible width in luma samples.
    width: u32,
    /// Visible height in luma samples.
    height: u32,
}

/// `manager` through `transform` as its D3D11 device manager.
fn send_manager(
    transform: &IMFTransform,
    manager: &IMFDXGIDeviceManager,
) -> Result<(), D3d11Error> {
    // Sound: the manager outlives the call; the transform AddRefs its copy.
    call(
        unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) },
        "ProcessMessage",
    )
}

/// MFT input bind flags behind `stream`, shader-resource without the hint.
fn bind_flags(transform: &IMFTransform, stream: u32) -> u32 {
    // Sound: the transform is live; absent keys read as unset.
    let hint = unsafe { transform.GetInputStreamAttributes(stream) }
        .ok()
        .and_then(|attrs| unsafe { attrs.GetUINT32(&MF_SA_D3D11_BINDFLAGS) }.ok())
        .unwrap_or(0);
    hint | u32_flag(D3D11_BIND_SHADER_RESOURCE.0)
}

/// `MediaFoundation` platform, started once per process.
fn startup() -> Result<(), D3d11Error> {
    // The platform starts once per process; per-instance shutdown races sibling encoders.
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    let status = STARTED.get_or_init(|| {
        // Sound: the version and flags are plain input values.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|error| error.code().0)
    });
    match status {
        Ok(()) => Ok(()),
        Err(hresult) => Err(D3d11Error::Call {
            call: "MFStartup",
            hresult: *hresult,
        }),
    }
}

/// Unsupported error naming the absent `codec` encoder MFT.
fn missing_encoder(codec: Codec) -> D3d11Error {
    D3d11Error::Unsupported(UnsupportedReason::NoEncoder { codec })
}

/// Output subtypes for `codec`, preferred first.
fn subtypes(codec: Codec) -> &'static [GUID] {
    match codec {
        Codec::H264 => &[MFVideoFormat_H264],
        Codec::H265 => &[MFVideoFormat_HEVC, MFVideoFormat_H265],
        Codec::Vp9 => &[MFVideoFormat_VP90],
        Codec::Av1 => &[MFVideoFormat_AV1],
        Codec::Vp8 | Codec::Av2 | Codec::Mpeg2 | Codec::Vc1 => &[],
    }
}

/// True when the catalog lists a `codec` encoder MFT, without activating.
pub(crate) fn has_encoder(codec: Codec) -> bool {
    let _com = ComGuard::enter();
    if startup().is_err() {
        return false;
    }
    subtypes(codec)
        .iter()
        .any(|subtype| !enumerate(subtype).is_empty())
}

/// Encoder activates for `subtype`, hardware before software.
fn enumerate(subtype: &GUID) -> Vec<IMFActivate> {
    let info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *subtype,
    };
    let mut found = Vec::new();
    for flags in [
        MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_SYNCMFT,
        MFT_ENUM_FLAG_ASYNCMFT,
    ] {
        found.append(&mut enumerate_flags(&info, flags));
    }
    found
}

/// Encoder activates for `info` under one flag set.
fn enumerate_flags(info: &MFT_REGISTER_TYPE_INFO, flags: MFT_ENUM_FLAG) -> Vec<IMFActivate> {
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // Sound: the info and out-parameters stay live for the call.
    let listed = unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            None,
            Some(std::ptr::from_ref(info)),
            std::ptr::addr_of_mut!(activates),
            std::ptr::addr_of_mut!(count),
        )
    };
    if listed.is_err() || count == 0 || activates.is_null() {
        free_catalog(activates);
        return Vec::new();
    }
    let count = usize::try_from(count).unwrap_or(0);
    let mut found = Vec::with_capacity(count);
    for index in 0..count {
        // Sound: the count bounds the array; each entry is a live activate moved out once.
        let entry = unsafe { activates.add(index).read() };
        if let Some(activate) = entry {
            found.push(activate);
        }
    }
    free_catalog(activates);
    found
}

/// Catalog array behind `activates` back to COM.
fn free_catalog(activates: *mut Option<IMFActivate>) {
    if !activates.is_null() {
        // Sound: the array came from the enum call.
        unsafe {
            CoTaskMemFree(Some(activates.cast_const().cast()));
        }
    }
}

/// First activating transform from `standby`, consumed in order.
fn elect(standby: &mut VecDeque<IMFActivate>, codec: Codec) -> Option<Opened> {
    while let Some(next) = standby.pop_front() {
        if let Ok(opened) = activate_one(&next, codec) {
            return Some(opened);
        }
    }
    None
}

/// First arming transform from `standby` plus its pump, consumed in order.
fn elect_armed(
    standby: &mut VecDeque<IMFActivate>,
    codec: Codec,
) -> Option<(Opened, Option<EventPump>)> {
    while let Some(opened) = elect(standby, codec) {
        match &opened.events {
            Some(source) => {
                if let Ok(armed) = EventPump::open(source.clone()) {
                    return Some((opened, Some(armed)));
                }
            }
            None => return Some((opened, None)),
        }
    }
    None
}

/// Live parts behind `activate`.
fn activate_one(activate: &IMFActivate, codec: Codec) -> Result<Opened, D3d11Error> {
    // Sound: the activate is live.
    let transform: IMFTransform = call(unsafe { activate.ActivateObject() }, "ActivateObject")?;
    let (store, async_mft, aware) = attributes(&transform);
    if async_mft {
        unlock(store.as_ref())?;
    }
    let events = if async_mft {
        Some(call(transform.cast(), "QueryInterface")?)
    } else {
        None
    };
    let (input, output) = stream_ids(&transform);
    let name = friendly(activate).unwrap_or_else(|| format!("{codec} encoder MFT"));
    Ok(Opened {
        transform,
        events,
        d3d11_aware: aware,
        name,
        input,
        output,
    })
}

/// Attribute store plus async and D3D11-aware flags.
fn attributes(transform: &IMFTransform) -> (Option<IMFAttributes>, bool, bool) {
    // Sound: the transform is live.
    let Ok(store) = (unsafe { transform.GetAttributes() }) else {
        return (None, false, false);
    };
    // Sound: the store is live; absent keys read as unset.
    let async_mft = unsafe { store.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
    let aware = unsafe { store.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) != 0;
    (Some(store), async_mft, aware)
}

/// Asynchronous unlock behind `store`.
fn unlock(store: Option<&IMFAttributes>) -> Result<(), D3d11Error> {
    let Some(store) = store else {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::AsyncMftWithoutAttributeStore,
        ));
    };
    // Sound: the store is live.
    call(
        unsafe { store.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) },
        "SetUINT32",
    )
}

/// Friendly name behind `activate`, absent without the attribute.
fn friendly(activate: &IMFActivate) -> Option<String> {
    let mut name = PWSTR::null();
    let mut len = 0u32;
    // Sound: the activate and out-parameters stay live for the call.
    unsafe {
        activate.GetAllocatedString(
            &MFT_FRIENDLY_NAME_Attribute,
            std::ptr::addr_of_mut!(name),
            std::ptr::addr_of_mut!(len),
        )
    }
    .ok()?;
    // Sound: the string came from the get call above.
    let text = unsafe { name.to_string() }.unwrap_or_default();
    // Sound: the string came from the get call above.
    unsafe {
        CoTaskMemFree(Some(name.as_ptr().cast()));
    }
    (!text.is_empty()).then_some(text)
}

/// Stream ids, zero when the transform numbers its streams.
fn stream_ids(transform: &IMFTransform) -> (u32, u32) {
    let (mut inputs, mut outputs) = ([0u32; 1], [0u32; 1]);
    // Sound: the arrays stay live for the call; fixed streams read as zero.
    if unsafe { transform.GetStreamIDs(&mut inputs, &mut outputs) }.is_ok() {
        (
            inputs.first().copied().unwrap_or(0),
            outputs.first().copied().unwrap_or(0),
        )
    } else {
        (0, 0)
    }
}

/// Catalog type for `subtype` on `stream`, walked through `get`.
fn find_available(
    transform: &IMFTransform,
    stream: u32,
    subtype: &GUID,
    get: Available,
) -> Option<IMFMediaType> {
    for index in 0..AVAILABLE_TYPES {
        // Sound: the transform is live; exhaustion ends the walk.
        let candidate = unsafe { get(transform, stream, index) };
        let Ok(candidate) = candidate else {
            break;
        };
        // Sound: the candidate store is live; a missing key is not a match.
        let found = unsafe { candidate.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or(GUID::zeroed());
        if &found == subtype {
            return Some(candidate);
        }
    }
    None
}

/// Video media type for `subtype`, stamped by the caller.
fn media_type(subtype: GUID) -> Result<IMFMediaType, D3d11Error> {
    // Sound: the created type is live for every set below.
    let media = call(unsafe { MFCreateMediaType() }, "MFCreateMediaType")?;
    call(
        unsafe { media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video) },
        "SetGUID",
    )?;
    call(
        unsafe { media.SetGUID(&MF_MT_SUBTYPE, &raw const subtype) },
        "SetGUID",
    )?;
    Ok(media)
}

/// Frame geometry plus rate on `media`.
fn stamp_input(
    media: &IMFMediaType,
    width: u32,
    height: u32,
    fps: (u32, u32),
) -> Result<(), D3d11Error> {
    // Sound: the borrowed type is live for every set below.
    call(
        unsafe { media.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height)) },
        "SetUINT64",
    )?;
    call(
        unsafe { media.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.0, fps.1)) },
        "SetUINT64",
    )?;
    call(
        unsafe {
            media.SetUINT32(
                &MF_MT_INTERLACE_MODE,
                u32_flag(MFVideoInterlace_Progressive.0),
            )
        },
        "SetUINT32",
    )?;
    call(
        unsafe { media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)) },
        "SetUINT64",
    )?;
    Ok(())
}

/// Output geometry, rate, bitrate, profile, and level on `media`.
fn stamp_output(
    media: &IMFMediaType,
    width: u32,
    height: u32,
    fps: (u32, u32),
    bitrate: u32,
    profile: Option<u32>,
    level: Option<u32>,
) -> Result<(), D3d11Error> {
    stamp_input(media, width, height, fps)?;
    // Sound: the borrowed type is live for every set below.
    call(
        unsafe { media.SetUINT32(&MF_MT_AVG_BITRATE, bitrate) },
        "SetUINT32",
    )?;
    if let Some(profile) = profile {
        call(
            unsafe { media.SetUINT32(&MF_MT_VIDEO_PROFILE, profile) },
            "SetUINT32",
        )?;
    }
    if let Some(level) = level {
        call(
            unsafe { media.SetUINT32(&MF_MT_VIDEO_LEVEL, level) },
            "SetUINT32",
        )?;
    }
    Ok(())
}

/// Two 32-bit halves packed into one 64-bit attribute.
fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Packed NV12 byte length for `width` x `height`.
fn nv12_len(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(3)
        / 2
}

/// Bitrate in bits per second scaled to the frame area.
fn bitrate(width: u32, height: u32) -> u32 {
    let estimate = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(3);
    u32::try_from(estimate.clamp(MIN_BITRATE, MAX_BITRATE)).unwrap_or(u32::MAX)
}

/// Flag bits as the comparison word.
fn u32_flag(bits: i32) -> u32 {
    u32::try_from(bits).unwrap_or(0)
}

/// Input sample over `len` bytes stamped at `pts`, filled by `fill`.
fn input_sample(
    len: u64,
    align: u32,
    pts: i64,
    hns: i64,
    fill: impl FnOnce(&mut [u8]),
) -> Result<IMFSample, D3d11Error> {
    let len = u32::try_from(len)
        .map_err(|_| D3d11Error::Unsupported(UnsupportedReason::FrameOver4Gib))?;
    // Sound: the sample and buffer are live for every set below.
    let sample = call(unsafe { MFCreateSample() }, "MFCreateSample")?;
    let buffer = call(
        unsafe { MFCreateAlignedMemoryBuffer(len, align) },
        "MFCreateAlignedMemoryBuffer",
    )?;
    call(unsafe { sample.AddBuffer(&buffer) }, "AddBuffer")?;
    fill_sample(&sample, len, pts, hns, fill)?;
    Ok(sample)
}

/// `len` bytes of `sample` stamped at `pts`, filled by `fill`.
fn fill_sample(
    sample: &IMFSample,
    len: u32,
    pts: i64,
    hns: i64,
    fill: impl FnOnce(&mut [u8]),
) -> Result<(), D3d11Error> {
    // Sound: the sample is live past output.
    let buffer = call(unsafe { sample.GetBufferByIndex(0) }, "GetBufferByIndex")?;
    let mut data: *mut u8 = std::ptr::null_mut();
    call(
        unsafe { buffer.Lock(std::ptr::addr_of_mut!(data), None, None) },
        "Lock",
    )?;
    // Sound: the lock spans `len` bytes.
    fill(unsafe { std::slice::from_raw_parts_mut(data, usize::try_from(len).unwrap_or(0)) });
    call(unsafe { buffer.Unlock() }, "Unlock")?;
    call(unsafe { buffer.SetCurrentLength(len) }, "SetCurrentLength")?;
    call(unsafe { sample.SetSampleTime(pts) }, "SetSampleTime")?;
    call(
        unsafe { sample.SetSampleDuration(hns) },
        "SetSampleDuration",
    )?;
    Ok(())
}

/// Packet behind one output sample.
fn extract(sample: &IMFSample) -> Result<Packet, D3d11Error> {
    // Sound: the sample is live past output.
    let buffer = call(unsafe { sample.GetBufferByIndex(0) }, "GetBufferByIndex")?;
    let len = call(unsafe { buffer.GetCurrentLength() }, "GetCurrentLength")?;
    let len = usize::try_from(len).unwrap_or(0);
    let mut data: *mut u8 = std::ptr::null_mut();
    call(
        unsafe { buffer.Lock(std::ptr::addr_of_mut!(data), None, None) },
        "Lock",
    )?;
    // Sound: the lock spans the current length.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast_const(), len) }.to_vec();
    call(unsafe { buffer.Unlock() }, "Unlock")?;
    // Sound: the sample store is live; a missing key is not a keyframe.
    let keyframe = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
    // Sound: the sample is live; a missing stamp reads as zero.
    let pts = unsafe { sample.GetSampleTime() }.map_or(Duration::ZERO, pts_duration);
    let mut packet = Packet::new(Bytes::from(bytes)).with_pts(pts);
    if keyframe {
        packet = packet.mark_keyframe();
    }
    Ok(packet)
}

/// Stamp of `sample`, zero when the transform left none.
fn sample_time(sample: &IMFSample) -> Duration {
    // Sound: the sample is live; a missing stamp reads as zero.
    unsafe { sample.GetSampleTime() }.map_or(Duration::ZERO, pts_duration)
}

/// Duration behind `hns` 100ns units.
fn pts_duration(hns: i64) -> Duration {
    let hns = u64::try_from(hns).unwrap_or(0);
    Duration::from_nanos(hns.saturating_mul(100))
}

/// True when `typ` names `want`.
fn is_event(typ: u32, want: MF_EVENT_TYPE) -> bool {
    u32::try_from(want.0).is_ok_and(|want| typ == want)
}

/// Windows result behind `result` as a tier failure naming `call`.
fn call<T>(result: windows::core::Result<T>, call: &'static str) -> Result<T, D3d11Error> {
    result.map_err(|error| D3d11Error::Call {
        call,
        hresult: error.code().0,
    })
}

#[cfg(test)]
mod tests {
    use super::EventQueue;

    #[test]
    fn pop_returns_pushed_type() {
        let queue = EventQueue::new();
        queue.push(Ok(7));
        assert_eq!(queue.pop(), Ok(7));
    }

    #[test]
    fn pop_surfaces_failure() {
        let queue = EventQueue::new();
        queue.push(Err(-1));
        assert_eq!(queue.pop(), Err(-1));
    }

    #[test]
    fn failure_sticks_until_reset() {
        let queue = EventQueue::new();
        queue.push(Err(-1));
        assert_eq!(queue.pop(), Err(-1));
        assert_eq!(queue.pop(), Err(-1));
        queue.reset();
        queue.push(Ok(7));
        assert_eq!(queue.pop(), Ok(7));
    }

    #[test]
    fn drain_collects_without_blocking() {
        let queue = EventQueue::new();
        assert_eq!(queue.drain(), [] as [std::result::Result<u32, i32>; 0]);
        queue.push(Ok(1));
        queue.push(Ok(2));
        assert_eq!(queue.drain(), vec![Ok(1), Ok(2)]);
        assert_eq!(queue.drain(), [] as [std::result::Result<u32, i32>; 0]);
    }

    #[test]
    fn waiting_pop_wakes_on_push() {
        let queue = std::sync::Arc::new(EventQueue::new());
        let pushed = queue.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            pushed.push(Ok(9));
        });
        assert_eq!(queue.pop(), Ok(9));
        worker.join().expect("worker joins");
    }

    #[test]
    fn async_session_opens_and_drops() {
        use crate::device::D3d11Device;
        use crate::encoder::D3d11Encoder;
        use crate::guid::Codec;

        use super::Session;

        let queue = std::sync::Arc::new(EventQueue::new());
        let waiter = queue.clone();
        let worker = std::thread::spawn(move || waiter.pop());
        queue.push(Err(windows::Win32::Foundation::E_ABORT.0));
        assert_eq!(
            worker.join().expect("waiter joins"),
            Err(windows::Win32::Foundation::E_ABORT.0)
        );

        let device = match D3d11Device::open() {
            Ok(device) => device,
            Err(error) => {
                println!("d3d11 skipped: {error}");
                return;
            }
        };
        let config = D3d11Encoder::auto_config(Codec::H264).expect("h264 auto");
        let session = match Session::open(&device, config) {
            Ok(session) => session,
            Err(error) => {
                println!("d3d11 skipped: {error}");
                return;
            }
        };
        let name = session.mft_name().to_owned();
        let path = if session.pump.is_some() {
            "async"
        } else {
            "sync"
        };
        println!("d3d11 session: {name} ({path})");
    }

    /// Resolved plan behind `config`, failing loud on decline.
    fn plan_of(config: &rust_video_caps::encode::EncodeConfig) -> super::EncodePlan {
        super::EncodePlan::resolve(config).expect("plan resolves")
    }

    #[test]
    fn auto_resolves_legacy_recipe() {
        use rust_video_caps::encode::{EncodeConfig, RateControl};

        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            ..EncodeConfig::default()
        };
        let plan = plan_of(&config);
        assert_eq!(plan.codec, crate::guid::Codec::H264);
        assert_eq!(plan.bitrate, None);
        assert_eq!(plan.rate_mode, None);
        assert_eq!(plan.max_bitrate, None);
        assert_eq!(plan.fps, (30, 1));
        assert_eq!(plan.frame_hns, super::FRAME_HNS);
        assert_eq!(plan.gop, None);
        assert_eq!(plan.qvs, None);
        assert_eq!(plan.profile, None);
        assert_eq!(plan.level, None);
    }

    #[test]
    fn default_cbr_maps_bitrate_and_profile() {
        use rust_video_caps::encode::EncodeConfig;

        let plan = plan_of(&EncodeConfig::default());
        assert_eq!(plan.codec, crate::guid::Codec::H264);
        assert_eq!(plan.bitrate, Some(5_000_000));
        assert_eq!(plan.rate_mode, Some(0));
        assert_eq!(plan.max_bitrate, Some(5_000_000));
        assert_eq!(plan.fps, (30, 1));
        assert_eq!(plan.gop, Some(60));
        assert_eq!(plan.gop_min, Some(60));
        assert_eq!(plan.gop_max, Some(60));
        assert_eq!(plan.b_frames, Some(0));
        assert_eq!(plan.max_refs, Some(1));
        assert_eq!(plan.qvs, Some(0));
        assert_eq!(plan.profile, Some(77));
        assert_eq!(plan.level, None);
    }

    #[test]
    fn vbr_maps_peak_constrained_mode() {
        use rust_video_caps::encode::{EncodeConfig, RateControl};

        let config = EncodeConfig {
            rate_control: RateControl::Vbr {
                bitrate: 2_000_000,
                maxrate: 4_000_000,
            },
            ..EncodeConfig::default()
        };
        let plan = plan_of(&config);
        assert_eq!(plan.bitrate, Some(2_000_000));
        assert_eq!(plan.rate_mode, Some(1));
        assert_eq!(plan.max_bitrate, Some(4_000_000));
    }

    #[test]
    fn fps_maps_rate_and_sample_duration() {
        use rust_video_caps::encode::{EncodeConfig, FrameRate};

        let config = EncodeConfig {
            frame_rate: FrameRate { num: 60, den: 1 },
            ..EncodeConfig::default()
        };
        let plan = plan_of(&config);
        assert_eq!(plan.fps, (60, 1));
        assert_eq!(plan.frame_hns, super::FRAME_HNS / 2);
        let config = EncodeConfig {
            frame_rate: FrameRate {
                num: 30_000,
                den: 1001,
            },
            ..EncodeConfig::default()
        };
        let plan = plan_of(&config);
        assert_eq!(plan.frame_hns, 333_666);
    }

    #[test]
    fn presets_map_quality_vs_speed() {
        use rust_video_caps::encode::{EncodeConfig, Quality, SpeedPreset};

        for (preset, qvs) in [
            (SpeedPreset::Realtime, 0),
            (SpeedPreset::Balanced, 50),
            (SpeedPreset::Quality, 100),
        ] {
            let config = EncodeConfig {
                quality: Quality { preset },
                ..EncodeConfig::default()
            };
            assert_eq!(plan_of(&config).qvs, Some(qvs));
        }
    }

    #[test]
    fn pinned_h264_level_maps_idc() {
        use rust_video_caps::encode::{EncodeConfig, Level};
        use rust_video_caps::h264;

        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L4)),
            ..EncodeConfig::default()
        };
        assert_eq!(plan_of(&config).level, Some(40));
        let config = EncodeConfig {
            profile: super::Profile::H265(super::H265Profile::Main),
            level: Level::H265(rust_video_caps::h265::EncodeLevel::Pinned(
                rust_video_caps::h265::Level::L4,
            )),
            ..EncodeConfig::default()
        };
        assert_eq!(plan_of(&config).level, None);
    }

    #[test]
    fn unsupported_knobs_decline_typed() {
        use rust_video_caps::color::{ColorPrimaries, ContentLight};
        use rust_video_caps::common::ChromaSampling;
        use rust_video_caps::encode::{Decline, EncodeConfig, Gop, Level, Profile, RateControl};

        // VP8 names no encoder MFT.
        let config = EncodeConfig {
            profile: Profile::Vp8,
            level: Level::Vp8,
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::ProfileUnsupported { .. })
        ));
        // 10-bit profiles decline.
        let config = EncodeConfig {
            profile: Profile::H264(super::H264Profile::High10),
            bit_depth: 10,
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::ProfileUnsupported { .. })
        ));
        // 10-bit depth declines under an allowlisted 10-bit profile.
        let config = EncodeConfig {
            profile: Profile::Av1(super::Av1Profile::Main),
            level: Level::Av1(rust_video_caps::av1::EncodeLevel::Auto),
            bit_depth: 10,
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::BitDepthUnsupported { .. })
        ));
        // Off-420 chroma declines.
        let config = EncodeConfig {
            profile: Profile::H264(super::H264Profile::High),
            chroma: ChromaSampling::Yuv422,
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::ChromaUnsupported { .. })
        ));
        // HDR metadata declines.
        let mut config = EncodeConfig::default();
        config.color.light = Some(ContentLight {
            max_cll: 1000,
            max_fall: 400,
        });
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::HdrUnsupported)
        ));
        // Off-baseline primaries decline.
        let mut config = EncodeConfig::default();
        config.color.primaries = ColorPrimaries::Bt2020;
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::PrimariesUnsupported { .. })
        ));
        // CQP and CRF decline.
        for mode in [RateControl::Cqp { qp: 23 }, RateControl::Crf { value: 23 }] {
            let config = EncodeConfig {
                rate_control: mode,
                ..EncodeConfig::default()
            };
            assert!(matches!(
                super::EncodePlan::resolve(&config),
                Err(Decline::RateControlUnsupported { .. })
            ));
        }
        // Prediction budget overruns decline.
        let config = EncodeConfig {
            gop: Gop {
                b_frames: 3,
                ..Gop::default()
            },
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::BFramesUnsupported { .. })
        ));
        let config = EncodeConfig {
            gop: Gop {
                refs: 17,
                ..Gop::default()
            },
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::RefsUnsupported { .. })
        ));
        // Structural breaks surface first.
        let config = EncodeConfig {
            profile: Profile::H264(super::H264Profile::Main),
            level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
            ..EncodeConfig::default()
        };
        assert!(matches!(
            super::EncodePlan::resolve(&config),
            Err(Decline::ProfileLevelMismatch { .. })
        ));
    }
}
