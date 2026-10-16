//! D3D11 device with enumerated decoder profiles behind the caps `Device` trait.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::sync::Arc;

#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::core::GUID;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HMODULE;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11VideoContext,
    ID3D11VideoDevice, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

#[cfg(target_os = "windows")]
use crate::error::D3d11Error;
#[cfg(target_os = "windows")]
use crate::error::SkipReason;
use crate::guid::{Codec, Profile};
#[cfg(target_os = "windows")]
use crate::submit::SubmitLock;

use rust_video_caps::traits::Device;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::SharedDevice;

/// H.264 VLD profile name the tier decodes.
#[cfg(target_os = "windows")]
const H264_PROFILE: &str = "h264-vld-nofgt";

/// D3D11 device with video interfaces and a VRAM budget.
pub struct D3d11Device {
    /// Logical device.
    #[cfg(target_os = "windows")]
    device: ID3D11Device,
    /// Immediate context.
    #[cfg(target_os = "windows")]
    context: ID3D11DeviceContext,
    /// Video device behind decode creation.
    #[cfg(target_os = "windows")]
    video_device: ID3D11VideoDevice,
    /// Video context behind frame submission.
    #[cfg(target_os = "windows")]
    video_context: ID3D11VideoContext,
    /// Matched decoder profiles.
    profiles: Vec<Profile>,
    /// VRAM budget shared with decoders.
    #[cfg(target_os = "windows")]
    shared: SharedDevice,
    /// Single-submitter lock over the immediate context.
    #[cfg(target_os = "windows")]
    submit: Arc<SubmitLock>,
    /// Negotiated feature level.
    level: i32,
    /// Adapter description.
    name: String,
}

impl D3d11Device {
    /// Device over the default hardware adapter with H.264 decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a hardware device, without
    /// video interfaces, without decoder profiles, or without an
    /// NV12 H.264 profile.
    #[cfg(target_os = "windows")]
    pub fn open() -> Result<Self, D3d11Error> {
        Self::open_windows()
    }

    /// Device over the default hardware adapter.
    #[cfg(target_os = "windows")]
    fn open_windows() -> Result<Self, D3d11Error> {
        let (device, context, level) = create_device()?;
        // Sound: the context is live; protection precedes any shared use.
        let multithread: ID3D11Multithread = context
            .cast()
            .map_err(|_| D3d11Error::from(SkipReason::VideoSupportMissing))?;
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
        // Sound: each cast queries a live object for its video interface.
        let video_device: ID3D11VideoDevice = device
            .cast()
            .map_err(|_| D3d11Error::from(SkipReason::VideoSupportMissing))?;
        let video_context: ID3D11VideoContext = context
            .cast()
            .map_err(|_| D3d11Error::from(SkipReason::VideoSupportMissing))?;
        let profiles = enumerate(&video_device)?;
        if profiles.is_empty() {
            return Err(SkipReason::NoDecoderProfiles.into());
        }
        if !profiles
            .iter()
            .any(|profile| profile.name() == H264_PROFILE)
        {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::H264 }.into());
        }
        check_nv12(&video_device)?;
        let (name, budget) = adapter_info(&device)?;
        Ok(Self {
            device,
            context,
            video_device,
            video_context,
            profiles,
            shared: SharedDevice::new("d3d11", budget),
            submit: Arc::new(SubmitLock::new()),
            level: level.0,
            name,
        })
    }

    /// True when the adapter exposes a `codec` profile.
    #[must_use]
    pub fn supports(&self, codec: Codec) -> bool {
        self.profiles.iter().any(|profile| profile.codec() == codec)
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
    pub(crate) fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Immediate context behind copies and readback.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Video device behind decode creation.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn video_device(&self) -> &ID3D11VideoDevice {
        &self.video_device
    }

    /// Video context behind frame submission.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn video_context(&self) -> &ID3D11VideoContext {
        &self.video_context
    }

    /// Single-submitter lock over the immediate context.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn submit_lock(&self) -> &Arc<SubmitLock> {
        &self.submit
    }
}

impl Device for D3d11Device {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Hardware device with video support at feature level 11.
#[cfg(target_os = "windows")]
fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, D3D_FEATURE_LEVEL), D3d11Error> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let mut device = None;
    let mut context = None;
    let mut level = D3D_FEATURE_LEVEL_11_0;
    // Sound: the out-pointers stay live for the call; no adapter or software module.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE(std::ptr::null_mut()),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(std::ptr::addr_of_mut!(device)),
            Some(std::ptr::addr_of_mut!(level)),
            Some(std::ptr::addr_of_mut!(context)),
        )
    }
    .map_err(|error| SkipReason::NoHardwareDevice {
        hresult: error.code().0,
    })?;
    let (Some(device), Some(context)) = (device, context) else {
        return Err(D3d11Error::Call {
            call: "D3D11CreateDevice",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        });
    };
    Ok((device, context, level))
}

/// Decoder profiles the adapter reports, matched against the tier table.
#[cfg(target_os = "windows")]
fn enumerate(video_device: &ID3D11VideoDevice) -> Result<Vec<Profile>, D3d11Error> {
    // Sound: the count bounds every indexed read below.
    let count = unsafe { video_device.GetVideoDecoderProfileCount() };
    let mut profiles = Vec::new();
    for index in 0..count {
        // Sound: `index` is below the reported count.
        let guid = unsafe { video_device.GetVideoDecoderProfile(index) }.map_err(|error| {
            D3d11Error::Call {
                call: "GetVideoDecoderProfile",
                hresult: error.code().0,
            }
        })?;
        if let Some(profile) = match_profile(&guid) {
            profiles.push(profile);
        }
    }
    Ok(profiles)
}

/// NV12 render target behind the H.264 profile.
#[cfg(target_os = "windows")]
fn check_nv12(video_device: &ID3D11VideoDevice) -> Result<(), D3d11Error> {
    use windows::Win32::Graphics::Direct3D11::D3D11_DECODER_PROFILE_H264_VLD_NOFGT;

    // Sound: the GUID and format are plain input values.
    let supported = unsafe {
        video_device
            .CheckVideoDecoderFormat(&D3D11_DECODER_PROFILE_H264_VLD_NOFGT, DXGI_FORMAT_NV12)
    }
    .map_err(|error| D3d11Error::Call {
        call: "CheckVideoDecoderFormat",
        hresult: error.code().0,
    })?;
    if supported.as_bool() {
        Ok(())
    } else {
        Err(SkipReason::NoNv12Target.into())
    }
}

/// Adapter description plus dedicated video memory in bytes.
#[cfg(target_os = "windows")]
fn adapter_info(device: &ID3D11Device) -> Result<(String, u64), D3d11Error> {
    // Sound: each cast queries a live object; the descriptor is plain data.
    let dxgi: IDXGIDevice = device.cast().map_err(|error| D3d11Error::Call {
        call: "QueryInterface",
        hresult: error.code().0,
    })?;
    let adapter = unsafe { dxgi.GetAdapter() }.map_err(|error| D3d11Error::Call {
        call: "GetAdapter",
        hresult: error.code().0,
    })?;
    let desc = unsafe { adapter.GetDesc() }.map_err(|error| D3d11Error::Call {
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
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_DECODER_PROFILE_MPEG2and1_VLD, D3D11_DECODER_PROFILE_AV1_VLD_12BIT_PROFILE2,
        D3D11_DECODER_PROFILE_AV1_VLD_12BIT_PROFILE2_420, D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0,
        D3D11_DECODER_PROFILE_AV1_VLD_PROFILE1, D3D11_DECODER_PROFILE_AV1_VLD_PROFILE2,
        D3D11_DECODER_PROFILE_H264_VLD_NOFGT, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
        D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10, D3D11_DECODER_PROFILE_MPEG1_VLD,
        D3D11_DECODER_PROFILE_MPEG2_VLD, D3D11_DECODER_PROFILE_VC1_D2010,
        D3D11_DECODER_PROFILE_VC1_VLD, D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2,
        D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0,
    };

    const TABLE: &[(Codec, GUID, &str)] = &[
        (
            Codec::H264,
            D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
            H264_PROFILE,
        ),
        (
            Codec::Hevc,
            D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
            "hevc-main",
        ),
        (
            Codec::Hevc,
            D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10,
            "hevc-main10",
        ),
        (
            Codec::Vp9,
            D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0,
            "vp9-profile0",
        ),
        (
            Codec::Vp9,
            D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2,
            "vp9-profile2-10bit",
        ),
        (
            Codec::Av1,
            D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0,
            "av1-profile0",
        ),
        (
            Codec::Av1,
            D3D11_DECODER_PROFILE_AV1_VLD_PROFILE1,
            "av1-profile1",
        ),
        (
            Codec::Av1,
            D3D11_DECODER_PROFILE_AV1_VLD_PROFILE2,
            "av1-profile2",
        ),
        (
            Codec::Av1,
            D3D11_DECODER_PROFILE_AV1_VLD_12BIT_PROFILE2,
            "av1-profile2-12bit",
        ),
        (
            Codec::Av1,
            D3D11_DECODER_PROFILE_AV1_VLD_12BIT_PROFILE2_420,
            "av1-profile2-12bit-420",
        ),
        (Codec::Mpeg2, D3D11_DECODER_PROFILE_MPEG2_VLD, "mpeg2-vld"),
        (
            Codec::Mpeg2,
            D3D11_DECODER_PROFILE_MPEG2and1_VLD,
            "mpeg2and1-vld",
        ),
        (Codec::Mpeg2, D3D11_DECODER_PROFILE_MPEG1_VLD, "mpeg1-vld"),
        (Codec::Vc1, D3D11_DECODER_PROFILE_VC1_VLD, "vc1-vld"),
        (Codec::Vc1, D3D11_DECODER_PROFILE_VC1_D2010, "vc1-d2010"),
    ];
    TABLE
        .iter()
        .find(|entry| entry.1 == *guid)
        .map(|entry| Profile::new(entry.0, entry.2))
}

/// H.264 profile GUID the tier decodes.
#[cfg(target_os = "windows")]
pub(crate) fn h264_guid() -> GUID {
    windows::Win32::Graphics::Direct3D11::D3D11_DECODER_PROFILE_H264_VLD_NOFGT
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use windows::core::GUID;
    #[cfg(target_os = "windows")]
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10, D3D11_DECODER_PROFILE_VC1_VLD,
    };

    #[cfg(target_os = "windows")]
    #[test]
    fn match_profile_names_table_entries() {
        let profile =
            super::match_profile(&D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10).expect("hevc main10");
        assert_eq!(profile.codec(), super::Codec::Hevc);
        assert_eq!(profile.name(), "hevc-main10");
        let profile = super::match_profile(&D3D11_DECODER_PROFILE_VC1_VLD).expect("vc1 vld");
        assert_eq!(profile.codec(), super::Codec::Vc1);
        assert!(super::match_profile(&GUID::zeroed()).is_none());
    }
}
