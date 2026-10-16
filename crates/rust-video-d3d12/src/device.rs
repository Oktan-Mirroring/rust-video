//! D3D12 device with enumerated decoder profiles behind the caps `Device` trait.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::ffi::c_void;

#[cfg(target_os = "windows")]
use crate::error::D3d12Error;
#[cfg(target_os = "windows")]
use crate::error::SkipReason;
use crate::guid::{Codec, Profile};

use rust_video_caps::traits::Device;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::SharedDevice;
#[cfg(target_os = "windows")]
use windows::core::IUnknown;
#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::core::GUID;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D::{
    D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_12_0,
    D3D_FEATURE_LEVEL_12_1, D3D_FEATURE_LEVEL_12_2,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D12::{D3D12CreateDevice, ID3D12Device};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4};
#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    ID3D12VideoDecoder, ID3D12VideoDecoderHeap, ID3D12VideoDevice, ID3D12VideoDevice3,
    D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE, D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES,
    D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT, D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT,
    D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC, D3D12_FEATURE_VIDEO,
    D3D12_FEATURE_VIDEO_DECODE_PROFILES, D3D12_FEATURE_VIDEO_DECODE_PROFILE_COUNT,
    D3D12_FEATURE_VIDEO_DECODE_SUPPORT, D3D12_FEATURE_VIDEO_ENCODER_CODEC,
    D3D12_VIDEO_DECODER_DESC, D3D12_VIDEO_DECODER_HEAP_DESC, D3D12_VIDEO_DECODE_CONFIGURATION,
    D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS, D3D12_VIDEO_DECODE_PROFILE_AV1_12BIT_PROFILE2,
    D3D12_VIDEO_DECODE_PROFILE_AV1_12BIT_PROFILE2_420, D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE0,
    D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE1, D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE2,
    D3D12_VIDEO_DECODE_PROFILE_H264, D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN,
    D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN10, D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN12,
    D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN12_422, D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN12_444,
    D3D12_VIDEO_DECODE_PROFILE_MPEG1_AND_MPEG2, D3D12_VIDEO_DECODE_PROFILE_MPEG2,
    D3D12_VIDEO_DECODE_PROFILE_VC1, D3D12_VIDEO_DECODE_PROFILE_VC1_D2010,
    D3D12_VIDEO_DECODE_PROFILE_VP9, D3D12_VIDEO_DECODE_PROFILE_VP9_10BIT_PROFILE2,
    D3D12_VIDEO_DECODE_SUPPORT_FLAGS, D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED,
    D3D12_VIDEO_DECODE_TIER, D3D12_VIDEO_DECODE_TIER_2, D3D12_VIDEO_ENCODER_CODEC,
    D3D12_VIDEO_ENCODER_CODEC_AV1, D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0, D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC, D3D12_VIDEO_ENCODER_CODEC_H264,
    D3D12_VIDEO_ENCODER_CODEC_HEVC, D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
};

/// H.264 profile name the tier decodes.
#[cfg(target_os = "windows")]
const H264_PROFILE: &str = "h264";

/// Codec, GUID, and name behind profile matching.
#[cfg(target_os = "windows")]
const TABLE: &[(Codec, windows::core::GUID, &str)] = &[
    (Codec::H264, D3D12_VIDEO_DECODE_PROFILE_H264, H264_PROFILE),
    (
        Codec::Hevc,
        D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN,
        "hevc-main",
    ),
    (
        Codec::Hevc,
        D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN10,
        "hevc-main10",
    ),
    (
        Codec::Hevc,
        D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN12,
        "hevc-main12",
    ),
    (
        Codec::Hevc,
        D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN12_422,
        "hevc-main12-422",
    ),
    (
        Codec::Hevc,
        D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN12_444,
        "hevc-main12-444",
    ),
    (Codec::Vp9, D3D12_VIDEO_DECODE_PROFILE_VP9, "vp9-profile0"),
    (
        Codec::Vp9,
        D3D12_VIDEO_DECODE_PROFILE_VP9_10BIT_PROFILE2,
        "vp9-profile2-10bit",
    ),
    (
        Codec::Av1,
        D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE0,
        "av1-profile0",
    ),
    (
        Codec::Av1,
        D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE1,
        "av1-profile1",
    ),
    (
        Codec::Av1,
        D3D12_VIDEO_DECODE_PROFILE_AV1_PROFILE2,
        "av1-profile2",
    ),
    (
        Codec::Av1,
        D3D12_VIDEO_DECODE_PROFILE_AV1_12BIT_PROFILE2,
        "av1-profile2-12bit",
    ),
    (
        Codec::Av1,
        D3D12_VIDEO_DECODE_PROFILE_AV1_12BIT_PROFILE2_420,
        "av1-profile2-12bit-420",
    ),
    (Codec::Mpeg2, D3D12_VIDEO_DECODE_PROFILE_MPEG2, "mpeg2"),
    (
        Codec::Mpeg2,
        D3D12_VIDEO_DECODE_PROFILE_MPEG1_AND_MPEG2,
        "mpeg1-and-mpeg2",
    ),
    (Codec::Vc1, D3D12_VIDEO_DECODE_PROFILE_VC1, "vc1"),
    (
        Codec::Vc1,
        D3D12_VIDEO_DECODE_PROFILE_VC1_D2010,
        "vc1-d2010",
    ),
];

/// D3D12 device with enumerated decoder profiles.
pub struct D3d12Device {
    /// Logical device.
    #[cfg(target_os = "windows")]
    device: ID3D12Device,
    /// Video device behind decode and encode queries.
    #[cfg(target_os = "windows")]
    video: ID3D12VideoDevice,
    /// Matched decoder profiles.
    profiles: Vec<Profile>,
    /// VRAM budget shared with decoders.
    #[cfg(target_os = "windows")]
    shared: SharedDevice,
    /// Negotiated feature level.
    level: i32,
    /// Adapter description.
    name: String,
}

impl D3d12Device {
    /// Device over the default hardware adapter with H.264 decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a hardware device, without
    /// video interfaces, without decoder profiles, or without an
    /// NV12 H.264 profile.
    #[cfg(target_os = "windows")]
    pub fn open() -> Result<Self, D3d12Error> {
        Self::open_windows()
    }

    /// Device over the default hardware adapter.
    #[cfg(target_os = "windows")]
    fn open_windows() -> Result<Self, D3d12Error> {
        let (device, level) = create_device()?;
        // Sound: the cast holds the device alive through the video interface.
        let video: ID3D12VideoDevice = device
            .cast()
            .map_err(|_| D3d12Error::from(SkipReason::VideoSupportMissing))?;
        let profiles = enumerate(&video)?;
        if profiles.is_empty() {
            return Err(SkipReason::NoDecoderProfiles.into());
        }
        if !profiles
            .iter()
            .any(|profile| profile.name() == H264_PROFILE)
        {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::H264 }.into());
        }
        check_nv12(&video)?;
        let (name, budget) = adapter_info(&device)?;
        Ok(Self {
            device,
            video,
            profiles,
            shared: SharedDevice::new("d3d12", budget),
            level: level.0,
            name,
        })
    }

    /// True when the adapter exposes a `codec` profile.
    #[must_use]
    pub fn supports(&self, codec: Codec) -> bool {
        self.profiles.iter().any(|profile| profile.codec() == codec)
    }

    /// True when the adapter encodes `codec` over the video encoder.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn supports_encode(&self, codec: Codec) -> bool {
        let Some(encoder) = encoder_codec(codec) else {
            return false;
        };
        let mut probe = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC {
            Codec: encoder,
            ..Default::default()
        };
        feature_support(
            &self.video,
            D3D12_FEATURE_VIDEO_ENCODER_CODEC,
            &mut probe,
            "EncoderCodec",
        )
        .is_ok()
            && probe.IsSupported.as_bool()
    }

    /// Matched decoder profiles on the adapter.
    #[must_use]
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Negotiated feature level.
    #[must_use]
    pub fn feature_level(&self) -> i32 {
        self.level
    }

    /// VRAM budget shared with decoders.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn shared(&self) -> &SharedDevice {
        &self.shared
    }

    /// Logical device behind the tier.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn device(&self) -> &ID3D12Device {
        &self.device
    }

    /// Video device behind decode and encode queries.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn video(&self) -> &ID3D12VideoDevice {
        &self.video
    }

    /// Video device behind encoder creation.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the device exposes no encoder interface.
    #[cfg(target_os = "windows")]
    pub(crate) fn video3(&self) -> Result<ID3D12VideoDevice3, D3d12Error> {
        // Sound: the cast holds the device alive through the video interface.
        self.video
            .cast()
            .map_err(|_| D3d12Error::from(SkipReason::VideoSupportMissing))
    }
}

impl Device for D3d12Device {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Hardware device at the highest feature level the adapter takes.
#[cfg(target_os = "windows")]
fn create_device() -> Result<(ID3D12Device, D3D_FEATURE_LEVEL), D3d12Error> {
    let levels = [
        D3D_FEATURE_LEVEL_12_2,
        D3D_FEATURE_LEVEL_12_1,
        D3D_FEATURE_LEVEL_12_0,
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
    ];
    let adapter: Option<&IUnknown> = None;
    let mut hresult = windows::Win32::Foundation::E_FAIL.0;
    for level in levels {
        let mut device = None;
        // Sound: the out-pointer stays live for the call; no adapter input.
        match unsafe { D3D12CreateDevice(adapter, level, std::ptr::addr_of_mut!(device)) } {
            Ok(()) => {
                if let Some(device) = device {
                    return Ok((device, level));
                }
            }
            Err(error) => hresult = error.code().0,
        }
    }
    Err(SkipReason::NoHardwareDevice { hresult }.into())
}

/// `feature` query over `data` against the video device.
#[cfg(target_os = "windows")]
pub(crate) fn feature_support<T>(
    video: &ID3D12VideoDevice,
    feature: D3D12_FEATURE_VIDEO,
    data: &mut T,
    call: &'static str,
) -> Result<(), D3d12Error> {
    let size = u32::try_from(std::mem::size_of::<T>()).unwrap_or(u32::MAX);
    // Sound: `data` outlives the call; the size matches its type.
    unsafe {
        video.CheckFeatureSupport(
            feature,
            std::ptr::from_mut::<T>(data).cast::<c_void>(),
            size,
        )
    }
    .map_err(|error| D3d12Error::Call {
        call,
        hresult: error.code().0,
    })
}

/// Decoder profiles the adapter reports, matched against the tier table.
#[cfg(target_os = "windows")]
fn enumerate(video: &ID3D12VideoDevice) -> Result<Vec<Profile>, D3d12Error> {
    let mut count = D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILE_COUNT {
        NodeIndex: 0,
        ProfileCount: 0,
    };
    feature_support(
        video,
        D3D12_FEATURE_VIDEO_DECODE_PROFILE_COUNT,
        &mut count,
        "DecodeProfileCount",
    )?;
    let len = usize::try_from(count.ProfileCount).unwrap_or(0);
    let mut guids = vec![GUID::zeroed(); len];
    // Sound: `guids` outlives the query below; the count bounds the writes.
    let mut list = D3D12_FEATURE_DATA_VIDEO_DECODE_PROFILES {
        NodeIndex: 0,
        ProfileCount: count.ProfileCount,
        pProfiles: guids.as_mut_ptr(),
    };
    feature_support(
        video,
        D3D12_FEATURE_VIDEO_DECODE_PROFILES,
        &mut list,
        "DecodeProfiles",
    )?;
    let len = usize::try_from(list.ProfileCount)
        .unwrap_or(0)
        .min(guids.len());
    guids.truncate(len);
    Ok(guids.iter().filter_map(match_profile).collect())
}

/// NV12 render target behind the H.264 profile at 1080p30.
#[cfg(target_os = "windows")]
fn check_nv12(video: &ID3D12VideoDevice) -> Result<(), D3d12Error> {
    let mut support = D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT {
        NodeIndex: 0,
        Configuration: D3D12_VIDEO_DECODE_CONFIGURATION {
            DecodeProfile: h264_guid(),
            BitstreamEncryption: D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
            InterlaceType: D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
        },
        Width: 1920,
        Height: 1080,
        DecodeFormat: DXGI_FORMAT_NV12,
        FrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        BitRate: 10_000_000,
        SupportFlags: D3D12_VIDEO_DECODE_SUPPORT_FLAGS(0),
        ConfigurationFlags: D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS(0),
        DecodeTier: D3D12_VIDEO_DECODE_TIER(0),
    };
    feature_support(
        video,
        D3D12_FEATURE_VIDEO_DECODE_SUPPORT,
        &mut support,
        "DecodeSupport",
    )?;
    if support
        .SupportFlags
        .contains(D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED)
    {
        Ok(())
    } else {
        Err(SkipReason::NoNv12Target.into())
    }
}

/// Video encoder codec for `codec`, if D3D12 encodes it.
#[cfg(target_os = "windows")]
fn encoder_codec(codec: Codec) -> Option<D3D12_VIDEO_ENCODER_CODEC> {
    match codec {
        Codec::H264 => Some(D3D12_VIDEO_ENCODER_CODEC_H264),
        Codec::Hevc => Some(D3D12_VIDEO_ENCODER_CODEC_HEVC),
        Codec::Av1 => Some(D3D12_VIDEO_ENCODER_CODEC_AV1),
        Codec::Vp9 | Codec::Mpeg2 | Codec::Vc1 => None,
    }
}

/// Adapter description plus dedicated video memory in bytes.
#[cfg(target_os = "windows")]
fn adapter_info(device: &ID3D12Device) -> Result<(String, u64), D3d12Error> {
    // Sound: the LUID is plain data; the factory call takes no inputs.
    let luid = unsafe { device.GetAdapterLuid() };
    let factory: IDXGIFactory4 =
        unsafe { CreateDXGIFactory1() }.map_err(|error| D3d12Error::Call {
            call: "CreateDXGIFactory1",
            hresult: error.code().0,
        })?;
    // Sound: the LUID came from the live device above.
    let adapter: IDXGIAdapter =
        unsafe { factory.EnumAdapterByLuid(luid) }.map_err(|error| D3d12Error::Call {
            call: "EnumAdapterByLuid",
            hresult: error.code().0,
        })?;
    // Sound: the descriptor is plain data behind a live adapter.
    let desc = unsafe { adapter.GetDesc() }.map_err(|error| D3d12Error::Call {
        call: "GetDesc",
        hresult: error.code().0,
    })?;
    let end = desc
        .Description
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(desc.Description.len());
    let name = String::from_utf16_lossy(&desc.Description[..end]);
    let budget = u64::try_from(desc.DedicatedVideoMemory).unwrap_or(u64::MAX);
    Ok((name, budget))
}

/// Profile for `guid`, if the tier drives it.
#[cfg(target_os = "windows")]
fn match_profile(guid: &GUID) -> Option<Profile> {
    TABLE
        .iter()
        .find(|entry| entry.1 == *guid)
        .map(|entry| Profile::new(entry.0, entry.2))
}

/// H.264 profile GUID the tier decodes.
#[cfg(target_os = "windows")]
pub(crate) fn h264_guid() -> GUID {
    D3D12_VIDEO_DECODE_PROFILE_H264
}

/// Decode profile GUID for `codec`, if the tier drives it.
#[cfg(target_os = "windows")]
pub(crate) fn decode_profile(codec: Codec) -> Option<GUID> {
    TABLE
        .iter()
        .find(|entry| entry.0 == codec)
        .map(|entry| entry.1)
}

/// NV12 decode behind `profile` at `width` by `height` on tier 2 or later.
#[cfg(target_os = "windows")]
pub(crate) fn check_decode_support(
    video: &ID3D12VideoDevice,
    profile: GUID,
    width: u32,
    height: u32,
) -> Result<(), D3d12Error> {
    let mut support = D3D12_FEATURE_DATA_VIDEO_DECODE_SUPPORT {
        NodeIndex: 0,
        Configuration: D3D12_VIDEO_DECODE_CONFIGURATION {
            DecodeProfile: profile,
            BitstreamEncryption: D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
            InterlaceType: D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
        },
        Width: width,
        Height: height,
        DecodeFormat: DXGI_FORMAT_NV12,
        FrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        BitRate: 10_000_000,
        SupportFlags: D3D12_VIDEO_DECODE_SUPPORT_FLAGS(0),
        ConfigurationFlags: D3D12_VIDEO_DECODE_CONFIGURATION_FLAGS(0),
        DecodeTier: D3D12_VIDEO_DECODE_TIER(0),
    };
    feature_support(
        video,
        D3D12_FEATURE_VIDEO_DECODE_SUPPORT,
        &mut support,
        "DecodeSupport",
    )?;
    if !support
        .SupportFlags
        .contains(D3D12_VIDEO_DECODE_SUPPORT_FLAG_SUPPORTED)
    {
        return Err(D3d12Error::OverCaps {
            what: "decode resolution",
        });
    }
    if support.DecodeTier.0 < D3D12_VIDEO_DECODE_TIER_2.0 {
        return Err(D3d12Error::Unsupported("decode tier 1".to_owned()));
    }
    Ok(())
}

/// Decoder heap behind `profile` at `width` by `height`.
#[cfg(target_os = "windows")]
pub(crate) fn create_decoder_heap(
    video: &ID3D12VideoDevice,
    profile: GUID,
    width: u32,
    height: u32,
) -> Result<ID3D12VideoDecoderHeap, D3d12Error> {
    let desc = D3D12_VIDEO_DECODER_HEAP_DESC {
        NodeMask: 0,
        Configuration: D3D12_VIDEO_DECODE_CONFIGURATION {
            DecodeProfile: profile,
            BitstreamEncryption: D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
            InterlaceType: D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
        },
        DecodeWidth: width,
        DecodeHeight: height,
        Format: DXGI_FORMAT_NV12,
        FrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        BitRate: 10_000_000,
        MaxDecodePictureBufferCount: 16,
    };
    // Sound: the descriptor outlives the call.
    unsafe { video.CreateVideoDecoderHeap(std::ptr::addr_of!(desc)) }.map_err(|error| {
        D3d12Error::Call {
            call: "CreateVideoDecoderHeap",
            hresult: error.code().0,
        }
    })
}

/// Decoder behind `profile`.
#[cfg(target_os = "windows")]
pub(crate) fn create_decoder(
    video: &ID3D12VideoDevice,
    profile: GUID,
) -> Result<ID3D12VideoDecoder, D3d12Error> {
    let desc = D3D12_VIDEO_DECODER_DESC {
        NodeMask: 0,
        Configuration: D3D12_VIDEO_DECODE_CONFIGURATION {
            DecodeProfile: profile,
            BitstreamEncryption: D3D12_BITSTREAM_ENCRYPTION_TYPE_NONE,
            InterlaceType: D3D12_VIDEO_FRAME_CODED_INTERLACE_TYPE_NONE,
        },
    };
    // Sound: the descriptor outlives the call.
    unsafe { video.CreateVideoDecoder(std::ptr::addr_of!(desc)) }.map_err(|error| {
        D3d12Error::Call {
            call: "CreateVideoDecoder",
            hresult: error.code().0,
        }
    })
}

/// H.264 codec configuration over `config`.
#[cfg(target_os = "windows")]
pub(crate) fn codec_configuration_h264(
    config: &mut D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
) -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
        DataSize: u32::try_from(std::mem::size_of::<
            D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264,
        >())
        .unwrap_or(u32::MAX),
        Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
            pH264Config: std::ptr::from_mut(config),
        },
    }
}

/// HEVC codec configuration over `config`.
#[cfg(target_os = "windows")]
pub(crate) fn codec_configuration_hevc(
    config: &mut D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
) -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
        DataSize: u32::try_from(std::mem::size_of::<
            D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
        >())
        .unwrap_or(u32::MAX),
        Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
            pHEVCConfig: std::ptr::from_mut(config),
        },
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use windows::core::GUID;
    #[cfg(target_os = "windows")]
    use windows::Win32::Media::MediaFoundation::{
        D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN10, D3D12_VIDEO_DECODE_PROFILE_VC1,
    };

    #[cfg(target_os = "windows")]
    #[test]
    fn match_profile_names_table_entries() {
        let profile =
            super::match_profile(&D3D12_VIDEO_DECODE_PROFILE_HEVC_MAIN10).expect("hevc main10");
        assert_eq!(profile.codec(), super::Codec::Hevc);
        assert_eq!(profile.name(), "hevc-main10");
        let profile = super::match_profile(&D3D12_VIDEO_DECODE_PROFILE_VC1).expect("vc1");
        assert_eq!(profile.codec(), super::Codec::Vc1);
        assert!(super::match_profile(&GUID::zeroed()).is_none());
    }
}
