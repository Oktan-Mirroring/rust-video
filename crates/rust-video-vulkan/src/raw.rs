//! Owned Vulkan handles behind safe methods.
//!
//! Sole `unsafe` module: every Vulkan call lands here, each block
//! carries its own justification, and no raw handle escapes.
#![allow(unsafe_code)]
//! Justification: Vulkan is a C API; ash marks every call `unsafe`.
//! This module owns each handle from creation to `Drop`, validates
//! extension gates before touching extension function pointers, and
//! exposes only safe methods. No other module uses `unsafe`.

use std::ffi::{c_char, CString};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;

use crate::error::{SkipReason, VulkanError};
use crate::inflight::Fence;
use crate::ladder::{
    AdapterFacts, AdapterFingerprint, Backend, CodecOp, DecodeFacts, EncodeFacts, LadderError,
};

use ash::{khr, vk};
use rust_video_caps::frame::GpuExport;
#[cfg(unix)]
use rust_video_caps::frame::{DmaBufPlane, PixelFormat};

/// Loaded Vulkan entry plus a 1.3 instance for negotiation.
pub(crate) struct AshBackend {
    /// Dlopened loader entry.
    entry: ash::Entry,
    /// Instance at API 1.0 for maximum driver inclusion.
    instance: ash::Instance,
    /// Video-queue function pointers, gated on extension strings.
    video: khr::video_queue::Instance,
}

/// H.264 profiles the caps rung probes, most capable first.
const H264_PROFILES: [vk::native::StdVideoH264ProfileIdc; 3] = [
    vk::native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
    vk::native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
    vk::native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
];

/// H.265 profiles the caps rung probes, most capable first.
const H265_PROFILES: [vk::native::StdVideoH265ProfileIdc; 2] = [
    vk::native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN_10,
    vk::native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN,
];

impl AshBackend {
    /// Entry plus 1.3 instance over the system loader.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when the loader is absent or backs no
    /// driver, or on hard failure when the instance fails to create.
    pub fn load() -> Result<Self, LadderError> {
        // SAFETY: loads the system loader; the entry owns the handle.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|_| LadderError::Skip(SkipReason::LoaderAbsent))?;
        let app_info = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
        let create_info = vk::InstanceCreateInfo::default().application_info(&app_info);
        // SAFETY: descriptors point at live info structs, no extensions.
        let instance = match unsafe { entry.create_instance(&create_info, None) } {
            Ok(instance) => instance,
            Err(vk::Result::ERROR_INCOMPATIBLE_DRIVER) => {
                return Err(LadderError::Skip(SkipReason::NoPhysicalDevice));
            }
            Err(error) => return Err(VulkanError::Vulkan(error).into()),
        };
        let video = khr::video_queue::Instance::new(&entry, &instance);
        Ok(Self {
            entry,
            instance,
            video,
        })
    }

    /// Device-local heap bytes behind `adapter`.
    pub fn device_local_bytes(&self, adapter: vk::PhysicalDevice) -> u64 {
        // SAFETY: adapter came from this instance's enumeration.
        let memory = unsafe { self.instance.get_physical_device_memory_properties(adapter) };
        let heaps = &memory.memory_heaps[..memory.memory_heap_count as usize];
        heaps
            .iter()
            .filter(|heap| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
            .map(|heap| heap.size)
            .fold(0, u64::saturating_add)
    }

    /// Facts for one enumerated adapter.
    fn facts(
        &self,
        adapter: vk::PhysicalDevice,
    ) -> Result<AdapterFacts<vk::PhysicalDevice>, LadderError> {
        // SAFETY: adapter came from this instance's enumeration.
        let properties = unsafe { self.instance.get_physical_device_properties(adapter) };
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        // SAFETY: the properties chain is plain descriptors.
        unsafe {
            self.instance
                .get_physical_device_properties2(adapter, &mut properties2);
        }
        let luid = if id.device_luid_valid == vk::TRUE {
            id.device_luid
        } else {
            [0; 8]
        };
        // SAFETY: extension list query writes into ash-owned storage.
        let extensions = unsafe { self.instance.enumerate_device_extension_properties(adapter) }?;
        let mut names = Vec::with_capacity(extensions.len());
        for extension in &extensions {
            names.push(CString::from(extension.extension_name_as_c_str()?));
        }
        // SAFETY: queue-family query writes into ash-owned storage.
        let families = unsafe {
            self.instance
                .get_physical_device_queue_family_properties(adapter)
        };
        let mut decode = Vec::new();
        let mut encode = Vec::new();
        for (index, family) in families.iter().enumerate() {
            let Ok(index) = u32::try_from(index) else {
                continue;
            };
            if family
                .queue_flags
                .contains(vk::QueueFlags::VIDEO_DECODE_KHR)
            {
                decode.push(index);
            }
            if family
                .queue_flags
                .contains(vk::QueueFlags::VIDEO_ENCODE_KHR)
            {
                encode.push(index);
            }
        }
        let h264 = self.h264_caps(adapter, &names);
        let h265 = self.h265_caps(adapter, &names);
        let h264_encode = self.h264_encode_caps(adapter, &names);
        let h265_encode = self.h265_encode_caps(adapter, &names);
        Ok(AdapterFacts {
            handle: adapter,
            fingerprint: AdapterFingerprint {
                vendor: properties.vendor_id,
                device: properties.device_id,
                luid,
            },
            api_version: properties.api_version,
            extensions: names,
            decode_queue_families: decode,
            encode_queue_families: encode,
            h264,
            h265,
            h264_encode,
            h265_encode,
        })
    }

    /// Every extension string of `op` present on the adapter.
    fn gated(op: CodecOp, extensions: &[CString]) -> bool {
        op.extensions()
            .iter()
            .all(|need| extensions.iter().any(|have| have.as_c_str() == *need))
    }

    /// Instance behind the backend.
    pub(crate) fn instance(&self) -> &ash::Instance {
        &self.instance
    }

    /// DMA-BUF export extensions, each present on `adapter` or none at all.
    fn export_extensions(&self, adapter: vk::PhysicalDevice) -> Vec<&'static std::ffi::CStr> {
        // SAFETY: enumeration writes into ash-owned storage.
        let list = unsafe { self.instance.enumerate_device_extension_properties(adapter) }
            .unwrap_or_default();
        let names: Vec<CString> = list
            .iter()
            .filter_map(|entry| entry.extension_name_as_c_str().ok().map(CString::from))
            .collect();
        if external_memory_present(&names) {
            export_extension_names()
        } else {
            Vec::new()
        }
    }

    /// Video picture formats for `profile`, if the query runs.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the format query fails.
    pub(crate) fn video_formats(
        &self,
        adapter: vk::PhysicalDevice,
        profile: &vk::VideoProfileInfoKHR<'_>,
        usage: vk::ImageUsageFlags,
    ) -> Result<Vec<vk::VideoFormatPropertiesKHR<'static>>, VulkanError> {
        let mut list = vk::VideoProfileListInfoKHR {
            p_profiles: std::ptr::from_ref(profile),
            profile_count: 1,
            ..Default::default()
        };
        let mut info = vk::PhysicalDeviceVideoFormatInfoKHR::default().image_usage(usage);
        info.p_next = std::ptr::from_mut(&mut list).cast();
        // SAFETY: gated on the open device's extensions; structs outlive the call.
        let mut count = 0;
        let status = unsafe {
            (self
                .video
                .fp()
                .get_physical_device_video_format_properties_khr)(
                adapter,
                &raw const info,
                &raw mut count,
                std::ptr::null_mut(),
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        let mut formats = vec![vk::VideoFormatPropertiesKHR::default(); count as usize];
        // SAFETY: structs outlive the call; the driver fills each entry.
        let status = unsafe {
            (self
                .video
                .fp()
                .get_physical_device_video_format_properties_khr)(
                adapter,
                &raw const info,
                &raw mut count,
                formats.as_mut_ptr(),
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        formats.truncate(count as usize);
        Ok(formats)
    }

    /// H.264 caps from the first supported profile, if any.
    ///
    /// Skips the query unless every video extension string is present:
    /// ash loads a panicking stub for missing extension entry points.
    fn h264_caps(
        &self,
        adapter: vk::PhysicalDevice,
        extensions: &[CString],
    ) -> Option<DecodeFacts> {
        if !Self::gated(CodecOp::H264Decode, extensions) {
            return None;
        }
        H264_PROFILES.iter().find_map(|profile| {
            let mut h264 = vk::VideoDecodeH264ProfileInfoKHR::default()
                .std_profile_idc(*profile)
                .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);
            let query = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h264);
            let mut codec_caps = vk::VideoDecodeH264CapabilitiesKHR::default();
            let mut decode_caps = vk::VideoDecodeCapabilitiesKHR::default();
            let (status, facts) = {
                let mut caps = vk::VideoCapabilitiesKHR::default()
                    .push_next(&mut codec_caps)
                    .push_next(&mut decode_caps);
                // SAFETY: gated on extension strings; structs outlive the call.
                let status = unsafe {
                    (self.video.fp().get_physical_device_video_capabilities_khr)(
                        adapter,
                        &raw const query,
                        &raw mut caps,
                    )
                };
                let facts = (
                    caps.max_coded_extent.width,
                    caps.max_coded_extent.height,
                    caps.max_dpb_slots,
                    caps.max_active_reference_pictures,
                    caps.std_header_version.spec_version,
                    caps.min_bitstream_buffer_size_alignment,
                );
                (status, facts)
            };
            if status == vk::Result::SUCCESS {
                Some(DecodeFacts {
                    coded_extent: (facts.0, facts.1),
                    dpb_slots: facts.2,
                    active_refs: facts.3,
                    max_level: crate::session::geometry::h264_std_to_wire(codec_caps.max_level_idc),
                    std_version: facts.4,
                    min_bitstream_align: facts.5,
                })
            } else {
                None
            }
        })
    }

    /// H.265 caps from the first supported profile, if any.
    ///
    /// Skips the query unless every video extension string is present:
    /// ash loads a panicking stub for missing extension entry points.
    fn h265_caps(
        &self,
        adapter: vk::PhysicalDevice,
        extensions: &[CString],
    ) -> Option<DecodeFacts> {
        if !Self::gated(CodecOp::H265Decode, extensions) {
            return None;
        }
        H265_PROFILES.iter().find_map(|profile| {
            let mut h265 = vk::VideoDecodeH265ProfileInfoKHR::default().std_profile_idc(*profile);
            let query = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_H265)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h265);
            let mut codec_caps = vk::VideoDecodeH265CapabilitiesKHR::default();
            let mut decode_caps = vk::VideoDecodeCapabilitiesKHR::default();
            let (status, facts) = {
                let mut caps = vk::VideoCapabilitiesKHR::default()
                    .push_next(&mut codec_caps)
                    .push_next(&mut decode_caps);
                // SAFETY: gated on extension strings; structs outlive the call.
                let status = unsafe {
                    (self.video.fp().get_physical_device_video_capabilities_khr)(
                        adapter,
                        &raw const query,
                        &raw mut caps,
                    )
                };
                let facts = (
                    caps.max_coded_extent.width,
                    caps.max_coded_extent.height,
                    caps.max_dpb_slots,
                    caps.max_active_reference_pictures,
                    caps.std_header_version.spec_version,
                    caps.min_bitstream_buffer_size_alignment,
                );
                (status, facts)
            };
            if status == vk::Result::SUCCESS {
                Some(DecodeFacts {
                    coded_extent: (facts.0, facts.1),
                    dpb_slots: facts.2,
                    active_refs: facts.3,
                    max_level: crate::session::geometry::h265_std_to_wire(codec_caps.max_level_idc),
                    std_version: facts.4,
                    min_bitstream_align: facts.5,
                })
            } else {
                None
            }
        })
    }

    /// H.264 encode caps from the first supported profile, if any.
    ///
    /// Skips the query unless every video extension string is present:
    /// ash loads a panicking stub for missing extension entry points.
    fn h264_encode_caps(
        &self,
        adapter: vk::PhysicalDevice,
        extensions: &[CString],
    ) -> Option<EncodeFacts> {
        if !Self::gated(CodecOp::H264Encode, extensions) {
            return None;
        }
        H264_PROFILES.iter().find_map(|profile| {
            let mut h264 = vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(*profile);
            let query = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h264);
            let mut codec_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
            let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
            let (status, facts) = {
                let mut caps = vk::VideoCapabilitiesKHR::default()
                    .push_next(&mut codec_caps)
                    .push_next(&mut encode_caps);
                // SAFETY: gated on extension strings; structs outlive the call.
                let status = unsafe {
                    (self.video.fp().get_physical_device_video_capabilities_khr)(
                        adapter,
                        &raw const query,
                        &raw mut caps,
                    )
                };
                let facts = (
                    caps.max_coded_extent.width,
                    caps.max_coded_extent.height,
                    caps.std_header_version.spec_version,
                    encode_caps.supported_encode_feedback_flags,
                    encode_caps.rate_control_modes,
                );
                (status, facts)
            };
            if status == vk::Result::SUCCESS {
                Some(EncodeFacts {
                    coded_extent: (facts.0, facts.1),
                    quality_levels: encode_caps.max_quality_levels,
                    max_bitrate: encode_caps.max_bitrate,
                    std_version: facts.2,
                    profile: *profile,
                    feedback: facts.3,
                    rate_control_modes: facts.4,
                })
            } else {
                None
            }
        })
    }

    /// H.265 encode caps from the first supported profile, if any.
    ///
    /// Skips the query unless every video extension string is present:
    /// ash loads a panicking stub for missing extension entry points.
    fn h265_encode_caps(
        &self,
        adapter: vk::PhysicalDevice,
        extensions: &[CString],
    ) -> Option<EncodeFacts> {
        if !Self::gated(CodecOp::H265Encode, extensions) {
            return None;
        }
        H265_PROFILES.iter().find_map(|profile| {
            let mut h265 = vk::VideoEncodeH265ProfileInfoKHR::default().std_profile_idc(*profile);
            let query = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h265);
            let mut codec_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
            let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
            let (status, facts) = {
                let mut caps = vk::VideoCapabilitiesKHR::default()
                    .push_next(&mut codec_caps)
                    .push_next(&mut encode_caps);
                // SAFETY: gated on extension strings; structs outlive the call.
                let status = unsafe {
                    (self.video.fp().get_physical_device_video_capabilities_khr)(
                        adapter,
                        &raw const query,
                        &raw mut caps,
                    )
                };
                let facts = (
                    caps.max_coded_extent.width,
                    caps.max_coded_extent.height,
                    caps.std_header_version.spec_version,
                    encode_caps.supported_encode_feedback_flags,
                    encode_caps.rate_control_modes,
                );
                (status, facts)
            };
            if status == vk::Result::SUCCESS {
                Some(EncodeFacts {
                    coded_extent: (facts.0, facts.1),
                    quality_levels: encode_caps.max_quality_levels,
                    max_bitrate: encode_caps.max_bitrate,
                    std_version: facts.2,
                    profile: *profile,
                    feedback: facts.3,
                    rate_control_modes: facts.4,
                })
            } else {
                None
            }
        })
    }
}

impl Drop for AshBackend {
    fn drop(&mut self) {
        // SAFETY: instance is live and owned; no device outlives it.
        unsafe { self.instance.destroy_instance(None) };
    }
}

impl Backend for AshBackend {
    type Adapter = vk::PhysicalDevice;
    type Device = RawDevice;

    fn instance_version(&self) -> Result<Option<u32>, LadderError> {
        // SAFETY: version query touches no instance state.
        let version = unsafe { self.entry.try_enumerate_instance_version() }?;
        Ok(version)
    }

    fn adapters(&self) -> Result<Vec<AdapterFacts<vk::PhysicalDevice>>, LadderError> {
        // SAFETY: enumeration writes into ash-owned storage.
        let adapters = unsafe { self.instance.enumerate_physical_devices() }?;
        adapters
            .iter()
            .map(|adapter| self.facts(*adapter))
            .collect()
    }

    fn open_device(
        &self,
        adapter: &vk::PhysicalDevice,
        op: CodecOp,
        queues: &[u32],
    ) -> Result<RawDevice, LadderError> {
        let priorities = [1.0_f32];
        let mut infos = Vec::with_capacity(queues.len());
        for family in queues {
            infos.push(
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(*family)
                    .queue_priorities(&priorities),
            );
        }
        let mut names: Vec<*const c_char> =
            op.extensions().iter().map(|name| name.as_ptr()).collect();
        let extra = self.export_extensions(*adapter);
        names.extend(extra.iter().map(|name| name.as_ptr()));
        let mut sync2 =
            vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);
        let create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&infos)
            .enabled_extension_names(&names)
            .push_next(&mut sync2);
        // SAFETY: families passed the ladder; names point at static strings.
        let device = unsafe { self.instance.create_device(*adapter, &create_info, None) }?;
        Ok(RawDevice {
            device,
            external: !extra.is_empty(),
        })
    }
}

impl Backend for &AshBackend {
    type Adapter = vk::PhysicalDevice;
    type Device = RawDevice;

    fn instance_version(&self) -> Result<Option<u32>, LadderError> {
        (*self).instance_version()
    }

    fn adapters(&self) -> Result<Vec<AdapterFacts<vk::PhysicalDevice>>, LadderError> {
        (*self).adapters()
    }

    fn open_device(
        &self,
        adapter: &vk::PhysicalDevice,
        op: CodecOp,
        queues: &[u32],
    ) -> Result<RawDevice, LadderError> {
        (*self).open_device(adapter, op, queues)
    }
}

/// Owned logical device with its negotiated queues.
pub(crate) struct RawDevice {
    /// Logical device handle.
    device: ash::Device,
    /// DMA-BUF export extensions enabled.
    external: bool,
}

/// DMA-BUF export extensions for this platform.
#[cfg(unix)]
fn export_extension_names() -> Vec<&'static std::ffi::CStr> {
    vec![
        vk::KHR_EXTERNAL_MEMORY_NAME,
        vk::KHR_EXTERNAL_MEMORY_FD_NAME,
    ]
}

/// DMA-BUF export extensions for this platform.
#[cfg(not(unix))]
fn export_extension_names() -> Vec<&'static std::ffi::CStr> {
    Vec::new()
}

/// True when `extensions` carry the DMA-BUF export pair.
#[cfg(unix)]
#[must_use]
pub(crate) fn external_memory_present(extensions: &[CString]) -> bool {
    use std::ffi::CStr;
    let present = |want: &CStr| extensions.iter().any(|have| have.as_c_str() == want);
    present(vk::KHR_EXTERNAL_MEMORY_NAME) && present(vk::KHR_EXTERNAL_MEMORY_FD_NAME)
}

/// True when `extensions` carry the DMA-BUF export pair.
#[cfg(not(unix))]
#[must_use]
pub(crate) fn external_memory_present(_extensions: &[CString]) -> bool {
    false
}

impl RawDevice {
    /// Borrowed logical device for fence creation.
    #[must_use]
    pub fn device(&self) -> &ash::Device {
        &self.device
    }

    /// DMA-BUF export extensions enabled.
    #[must_use]
    pub(crate) fn external_memory(&self) -> bool {
        self.external
    }

    /// Queue of `family` at index zero.
    pub(crate) fn queue(&self, family: u32) -> vk::Queue {
        // SAFETY: the ladder requested this family at device creation.
        unsafe { self.device.get_device_queue(family, 0) }
    }

    /// Memory properties of `adapter` for type picking.
    pub(crate) fn memory_properties(
        backend: &AshBackend,
        adapter: vk::PhysicalDevice,
    ) -> vk::PhysicalDeviceMemoryProperties {
        // SAFETY: query writes into ash-owned storage.
        unsafe {
            backend
                .instance()
                .get_physical_device_memory_properties(adapter)
        }
    }
}

/// Owned logical device plus the instance behind it, shared with escaped frames.
///
/// Field order is the drop order: the device destroys before its instance.
pub(crate) struct OwnedDevice {
    /// Logical device; drops before the instance below.
    device: RawDevice,
    /// Entry plus instance owning the device above.
    backend: AshBackend,
}

impl OwnedDevice {
    /// Owned pair of `device` on `backend`.
    pub fn new(device: RawDevice, backend: AshBackend) -> Self {
        Self { device, backend }
    }

    /// Logical device behind the pair.
    #[must_use]
    pub fn device(&self) -> &RawDevice {
        &self.device
    }

    /// Entry plus instance behind the pair.
    #[must_use]
    pub fn backend(&self) -> &AshBackend {
        &self.backend
    }
}

/// Memory type index for `requirements` with `flags`, if any.
///
/// Exact: every bit of `flags` must hold. Buffers the host maps keep this.
#[must_use]
pub(crate) fn pick_memory_type(
    requirements: vk::MemoryRequirements,
    properties: &vk::PhysicalDeviceMemoryProperties,
    flags: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..properties.memory_type_count).find(|index| {
        let bit = 1_u32 << index;
        requirements.memory_type_bits & bit != 0
            && properties.memory_types[*index as usize]
                .property_flags
                .contains(flags)
    })
}

/// Memory type index preferring `flags`, else any allowed type.
///
/// Session and image memory only needs an allowed type; some drivers allow
/// no device-local type for session memory, so exact matching fails there.
#[must_use]
pub(crate) fn pick_memory_type_preferred(
    requirements: vk::MemoryRequirements,
    properties: &vk::PhysicalDeviceMemoryProperties,
    flags: vk::MemoryPropertyFlags,
) -> Option<u32> {
    pick_memory_type(requirements, properties, flags).or_else(|| {
        (0..properties.memory_type_count)
            .find(|index| requirements.memory_type_bits & (1_u32 << index) != 0)
    })
}

/// 8-bit 4:2:0 picture format from the queried list, if any.
#[must_use]
pub(crate) fn pick_video_format(
    properties: &[vk::VideoFormatPropertiesKHR<'_>],
) -> Option<vk::Format> {
    const PREFERRED: [vk::Format; 2] = [
        vk::Format::G8_B8R8_2PLANE_420_UNORM,
        vk::Format::G8_B8_R8_3PLANE_420_UNORM,
    ];
    PREFERRED
        .iter()
        .find(|want| properties.iter().any(|have| have.format == **want))
        .copied()
}

/// Std header version pinning `spec_version` under `name`.
///
/// Names come from the Vulkan video Std headers, e.g.
/// <https://github.com/KhronosGroup/Vulkan-Headers/blob/main/include/vulkan/vulkan_video_codec_h264std_decode.h>.
pub(crate) fn std_header_version(name: &[u8], spec_version: u32) -> vk::ExtensionProperties {
    let mut extension_name = [0 as c_char; 256];
    for (index, byte) in name.iter().take(255).enumerate() {
        extension_name[index] = c_char::try_from(*byte).unwrap_or(0);
    }
    vk::ExtensionProperties {
        spec_version,
        extension_name,
    }
}

impl Drop for RawDevice {
    fn drop(&mut self) {
        // SAFETY: device is live and owned; queues borrow it, never own.
        unsafe { self.device.destroy_device(None) };
    }
}

/// Owned fence behind one inflight frame.
pub struct VkFence {
    /// Device that destroys the fence.
    device: ash::Device,
    /// Fence handle.
    fence: vk::Fence,
}

impl VkFence {
    /// Fence on `device`, signaled when `signaled`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when fence creation fails.
    pub fn new(device: &ash::Device, signaled: bool) -> Result<Self, VulkanError> {
        let mut info = vk::FenceCreateInfo::default();
        if signaled {
            info = info.flags(vk::FenceCreateFlags::SIGNALED);
        }
        // SAFETY: info is a plain descriptor; the fence is owned on success.
        let fence = unsafe { device.create_fence(&info, None) }?;
        Ok(Self {
            device: device.clone(),
            fence,
        })
    }
}

impl Fence for VkFence {
    fn signaled(&self) -> Result<bool, VulkanError> {
        // SAFETY: fence is live and owned.
        let status = unsafe { self.device.get_fence_status(self.fence) }?;
        Ok(status)
    }

    fn wait(&self, timeout_ns: u64) -> Result<bool, VulkanError> {
        // SAFETY: fence is live and owned; the wait names it explicitly.
        match unsafe { self.device.wait_for_fences(&[self.fence], true, timeout_ns) } {
            Ok(()) => Ok(true),
            Err(vk::Result::TIMEOUT) => Ok(false),
            Err(error) => Err(VulkanError::Vulkan(error)),
        }
    }
}

impl VkFence {
    /// Fence handle for queue submission.
    #[must_use]
    pub fn fence(&self) -> vk::Fence {
        self.fence
    }

    /// Return the fence to the unsignaled state.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the reset fails.
    pub fn reset(&self) -> Result<(), VulkanError> {
        // SAFETY: fence is live and owned.
        unsafe { self.device.reset_fences(&[self.fence]) }?;
        Ok(())
    }
}

impl Drop for VkFence {
    fn drop(&mut self) {
        // SAFETY: fence is live and owned by this device.
        unsafe { self.device.destroy_fence(self.fence, None) };
    }
}

/// Owned device memory freed on drop.
pub(crate) struct GpuMemory {
    /// Device that frees the memory.
    device: ash::Device,
    /// Memory handle.
    memory: vk::DeviceMemory,
    /// Allocation size in bytes.
    size: vk::DeviceSize,
}

impl GpuMemory {
    /// Allocation of `size` bytes of `type_index`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when allocation fails.
    pub fn allocate(
        device: &ash::Device,
        size: vk::DeviceSize,
        type_index: u32,
    ) -> Result<Self, VulkanError> {
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(type_index);
        // SAFETY: plain descriptor; the memory is owned on success.
        let memory = unsafe { device.allocate_memory(&info, None) }?;
        Ok(Self {
            device: device.clone(),
            memory,
            size,
        })
    }

    /// Allocation of `size` bytes of `type_index`, exportable as DMA-BUF.
    ///
    /// # Errors
    ///
    /// Returns `Err` when allocation fails or the platform exports nothing.
    pub fn allocate_exportable(
        device: &ash::Device,
        size: vk::DeviceSize,
        type_index: u32,
    ) -> Result<Self, VulkanError> {
        #[cfg(unix)]
        {
            let mut export = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let info = vk::MemoryAllocateInfo::default()
                .allocation_size(size)
                .memory_type_index(type_index)
                .push_next(&mut export);
            // SAFETY: plain descriptor; the memory is owned on success.
            let memory = unsafe { device.allocate_memory(&info, None) }?;
            Ok(Self {
                device: device.clone(),
                memory,
                size,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (device, size, type_index);
            Err(VulkanError::Unsupported {
                what: "dma-buf export".to_owned(),
            })
        }
    }

    /// Allocation of `size` bytes of `type_index` importing `fd` as DMA-BUF.
    ///
    /// The import consumes `fd` on success and closes it on failure.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the import fails or the platform imports nothing.
    #[cfg(unix)]
    pub fn allocate_imported(
        device: &ash::Device,
        size: vk::DeviceSize,
        type_index: u32,
        fd: OwnedFd,
    ) -> Result<Self, VulkanError> {
        let raw = fd.into_raw_fd();
        let mut import = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(raw);
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(type_index)
            .push_next(&mut import);
        // SAFETY: plain descriptor; the driver owns the fd on success.
        match unsafe { device.allocate_memory(&info, None) } {
            Ok(memory) => Ok(Self {
                device: device.clone(),
                memory,
                size,
            }),
            Err(error) => {
                // SAFETY: the import failed, so the fd is still ours.
                drop(unsafe { OwnedFd::from_raw_fd(raw) });
                Err(VulkanError::Vulkan(error))
            }
        }
    }

    /// Memory handle.
    #[must_use]
    pub fn memory(&self) -> vk::DeviceMemory {
        self.memory
    }

    /// Copy `bytes` into the allocation at offset zero.
    ///
    /// Maps `bytes.len`, never the allocation size.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `bytes` overruns the allocation or mapping fails.
    pub fn fill(&self, bytes: &[u8]) -> Result<(), VulkanError> {
        let len = vk::DeviceSize::try_from(bytes.len())
            .map_err(|_| VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY))?;
        if len > self.size {
            return Err(VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
        }
        // SAFETY: the memory is host-visible and owned; the copy stays in range.
        unsafe {
            let ptr = self
                .device
                .map_memory(self.memory, 0, len, vk::MemoryMapFlags::empty())?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
            self.device.unmap_memory(self.memory);
        }
        Ok(())
    }

    /// Copy `bytes` into the allocation, zeroing through `total`.
    ///
    /// Maps `total`, never the allocation size.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `total` overruns the allocation, `bytes` overruns
    /// `total`, or mapping fails.
    pub fn fill_padded(&self, bytes: &[u8], total: u64) -> Result<(), VulkanError> {
        let overrun = || VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
        let len = u64::try_from(bytes.len()).map_err(|_| overrun())?;
        if total > self.size || len > total {
            return Err(overrun());
        }
        // SAFETY: the memory is host-visible and owned; writes stay in range.
        unsafe {
            let ptr = self
                .device
                .map_memory(self.memory, 0, total, vk::MemoryMapFlags::empty())?;
            let dst = ptr.cast::<u8>();
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            let pad = usize::try_from(total - len).map_err(|_| overrun())?;
            std::ptr::write_bytes(dst.add(bytes.len()), 0, pad);
            self.device.unmap_memory(self.memory);
        }
        Ok(())
    }

    /// Copy the allocation prefix of `len` bytes into `out`, reusing it.
    ///
    /// Maps `len`, never the allocation size; skips the zero-fill a
    /// `resize` would write before the overwrite.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `len` overruns the allocation or mapping fails.
    pub fn read_to_vec(&self, out: &mut Vec<u8>, len: usize) -> Result<(), VulkanError> {
        let overrun = || VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
        let want = vk::DeviceSize::try_from(len).map_err(|_| overrun())?;
        if want > self.size {
            return Err(overrun());
        }
        out.clear();
        if len == 0 {
            return Ok(());
        }
        out.reserve(len);
        // SAFETY: the memory is host-visible and owned; the copy lands in
        // reserved spare capacity, and the length sets only after it.
        unsafe {
            let ptr = self
                .device
                .map_memory(self.memory, 0, want, vk::MemoryMapFlags::empty())?;
            std::ptr::copy_nonoverlapping(
                ptr.cast::<u8>(),
                out.spare_capacity_mut().as_mut_ptr().cast::<u8>(),
                len,
            );
            self.device.unmap_memory(self.memory);
            out.set_len(len);
        }
        Ok(())
    }
}

impl Drop for GpuMemory {
    fn drop(&mut self) {
        // SAFETY: the memory is live and owned by this device.
        unsafe { self.device.free_memory(self.memory, None) };
    }
}

/// Owned buffer with its memory, freed on drop.
pub(crate) struct GpuBuffer {
    /// Device that destroys the buffer.
    device: ash::Device,
    /// Buffer handle.
    buffer: vk::Buffer,
    /// Backing memory, dropped after the buffer.
    memory: GpuMemory,
}

impl GpuBuffer {
    /// Buffer of `size` bytes with `usage`, backed by `flags` memory.
    ///
    /// `profiles` rides a video profile list for video usages; empty otherwise.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation, type picking, or binding fails.
    pub fn create(
        device: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        flags: vk::MemoryPropertyFlags,
        profiles: &[vk::VideoProfileInfoKHR<'_>],
    ) -> Result<Self, VulkanError> {
        let mut list = vk::VideoProfileListInfoKHR {
            profile_count: u32::try_from(profiles.len()).unwrap_or(u32::MAX),
            p_profiles: profiles.as_ptr(),
            ..Default::default()
        };
        let mut info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        if !profiles.is_empty() {
            info = info.push_next(&mut list);
        }
        // SAFETY: plain descriptor; the buffer is owned on success.
        let buffer = unsafe { device.create_buffer(&info, None) }?;
        // SAFETY: the buffer is live and owned.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let Some(type_index) = pick_memory_type(requirements, properties, flags) else {
            // SAFETY: the buffer is live and owned.
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
        };
        let memory = GpuMemory::allocate(device, requirements.size, type_index)?;
        // SAFETY: the memory covers the requirements at offset zero.
        unsafe { device.bind_buffer_memory(buffer, memory.memory(), 0) }?;
        Ok(Self {
            device: device.clone(),
            buffer,
            memory,
        })
    }

    /// Buffer handle.
    #[must_use]
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }

    /// Backing memory.
    #[must_use]
    pub fn memory(&self) -> &GpuMemory {
        &self.memory
    }
}

impl Drop for GpuBuffer {
    fn drop(&mut self) {
        // SAFETY: the buffer is live and owned by this device.
        unsafe { self.device.destroy_buffer(self.buffer, None) };
    }
}

/// Image description for [`GpuImage::create`].
pub(crate) struct GpuImageDesc {
    /// Image format.
    pub(crate) format: vk::Format,
    /// Image extent.
    pub(crate) extent: vk::Extent2D,
    /// Array layers.
    pub(crate) layers: u32,
    /// Image tiling.
    pub(crate) tiling: vk::ImageTiling,
    /// Image usage.
    pub(crate) usage: vk::ImageUsageFlags,
    /// Backing memory flags.
    pub(crate) flags: vk::MemoryPropertyFlags,
}

/// Owned image with its view and memory, freed on drop.
pub(crate) struct GpuImage {
    /// Device that destroys the image.
    device: ash::Device,
    /// Image handle.
    image: vk::Image,
    /// Backing memory, freed after the image in `Drop`.
    memory: Option<GpuMemory>,
    /// Image format.
    format: vk::Format,
}

impl GpuImage {
    /// Image over `desc`, memory bound.
    ///
    /// `profiles` rides a video profile list for video usages; empty otherwise.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation, type picking, or binding fails.
    pub fn create(
        device: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        desc: &GpuImageDesc,
        profiles: &[vk::VideoProfileInfoKHR<'_>],
    ) -> Result<Self, VulkanError> {
        let mut list = vk::VideoProfileListInfoKHR {
            profile_count: u32::try_from(profiles.len()).unwrap_or(u32::MAX),
            p_profiles: profiles.as_ptr(),
            ..Default::default()
        };
        let mut info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(desc.format)
            .extent(vk::Extent3D {
                width: desc.extent.width,
                height: desc.extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(desc.layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(desc.tiling)
            .usage(desc.usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        if !profiles.is_empty() {
            info = info.push_next(&mut list);
        }
        // SAFETY: plain descriptor; the image is owned on success.
        let image = unsafe { device.create_image(&info, None) }?;
        // SAFETY: the image is live and owned.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let Some(type_index) = pick_memory_type_preferred(requirements, properties, desc.flags)
        else {
            // SAFETY: the image is live and owned.
            unsafe { device.destroy_image(image, None) };
            return Err(VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
        };
        let memory = GpuMemory::allocate(device, requirements.size, type_index)?;
        // SAFETY: the memory covers the requirements at offset zero.
        unsafe { device.bind_image_memory(image, memory.memory(), 0) }?;
        Ok(Self {
            device: device.clone(),
            image,
            memory: Some(memory),
            format: desc.format,
        })
    }

    /// Image handle.
    #[must_use]
    pub fn image(&self) -> vk::Image {
        self.image
    }

    /// View of one array `layer`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when view creation fails.
    pub fn layer_view(&self, layer: u32) -> Result<GpuView, VulkanError> {
        let view_info = vk::ImageViewCreateInfo::default()
            .image(self.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(self.format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: layer,
                layer_count: 1,
            });
        GpuView::create(&self.device, &view_info)
    }
}

/// Owned image view freed on drop.
pub(crate) struct GpuView {
    /// Device that destroys the view.
    device: ash::Device,
    /// View handle.
    view: vk::ImageView,
}

impl GpuView {
    /// View from `info`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation fails.
    pub fn create(
        device: &ash::Device,
        info: &vk::ImageViewCreateInfo<'_>,
    ) -> Result<Self, VulkanError> {
        // SAFETY: the image is live; the view is owned on success.
        let view = unsafe { device.create_image_view(info, None) }?;
        Ok(Self {
            device: device.clone(),
            view,
        })
    }

    /// View handle.
    #[must_use]
    pub fn view(&self) -> vk::ImageView {
        self.view
    }
}

impl Drop for GpuView {
    fn drop(&mut self) {
        // SAFETY: the view is live and owned by this device.
        unsafe { self.device.destroy_image_view(self.view, None) };
    }
}

impl Drop for GpuImage {
    fn drop(&mut self) {
        // SAFETY: the image is live and owned by this device.
        unsafe {
            self.device.destroy_image(self.image, None);
        }
        // Bound memory frees after the image regardless of field order.
        let _memory = self.memory.take();
    }
}

/// Linear DRM format modifier: tightly packed rows, no tiling.
#[cfg(unix)]
pub(crate) const DRM_MODIFIER_LINEAR: u64 = 0;

/// True when `format` exports as NV12 DMA-BUF planes.
#[must_use]
pub(crate) fn two_plane_nv12(format: vk::Format) -> bool {
    format == vk::Format::G8_B8R8_2PLANE_420_UNORM
}

/// One exportable linear plane: image plus exportable memory.
pub(crate) struct ExportPlane {
    /// Device that destroys the image.
    #[cfg(not(unix))]
    device: ash::Device,
    /// Image handle.
    image: vk::Image,
    /// Backing memory, freed after the image in `Drop`.
    #[cfg(unix)]
    memory: GpuMemory,
    /// Bytes per row.
    #[cfg(unix)]
    stride: u32,
    /// Byte offset of the first row.
    #[cfg(unix)]
    offset: u32,
    /// Instance plus device behind the image; drops last.
    #[cfg(unix)]
    keep: Arc<OwnedDevice>,
}

impl ExportPlane {
    /// Linear `format` plane at `extent` with `usage`, exportable as DMA-BUF.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation, type picking, binding, or the layout
    /// query fails, or the platform exports nothing.
    fn create(
        device: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        usage: vk::ImageUsageFlags,
        keep: Arc<OwnedDevice>,
    ) -> Result<Self, VulkanError> {
        #[cfg(unix)]
        {
            let mut external = vk::ExternalMemoryImageCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::LINEAR)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external);
            // SAFETY: plain descriptor; the image is owned on success.
            let image = unsafe { device.create_image(&info, None) }?;
            // SAFETY: the image is live and owned.
            let requirements = unsafe { device.get_image_memory_requirements(image) };
            let picked = pick_memory_type_preferred(
                requirements,
                properties,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            );
            let Some(type_index) = picked else {
                // SAFETY: the image is live and owned.
                unsafe { device.destroy_image(image, None) };
                return Err(VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
            };
            let memory = GpuMemory::allocate_exportable(device, requirements.size, type_index)
                .inspect_err(|_| {
                    // SAFETY: the image is live and owned.
                    unsafe { device.destroy_image(image, None) };
                })?;
            // SAFETY: the memory covers the requirements at offset zero.
            unsafe { device.bind_image_memory(image, memory.memory(), 0) }?;
            let subresource = vk::ImageSubresource {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                array_layer: 0,
            };
            // SAFETY: the image is live and owned.
            let layout = unsafe { device.get_image_subresource_layout(image, subresource) };
            let stride = u32::try_from(layout.row_pitch).map_err(|_| {
                crate::error::ParseError::OutOfRange {
                    syntax: "export stride",
                }
            })?;
            let offset =
                u32::try_from(layout.offset).map_err(|_| crate::error::ParseError::OutOfRange {
                    syntax: "export offset",
                })?;
            Ok(Self {
                image,
                memory,
                stride,
                offset,
                keep,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (device, properties, format, extent, usage, keep);
            Err(VulkanError::Unsupported {
                what: "dma-buf export".to_owned(),
            })
        }
    }

    /// Image handle.
    #[must_use]
    fn image(&self) -> vk::Image {
        self.image
    }
}

/// DMA-BUF file descriptor of `memory`, freshly owned by the caller.
#[cfg(unix)]
fn export_fd(ext: &khr::external_memory_fd::Device, memory: vk::DeviceMemory) -> Option<OwnedFd> {
    let info = vk::MemoryGetFdInfoKHR::default()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    // SAFETY: the memory is live and owned; the fd is owned on success.
    let fd = unsafe { ext.get_memory_fd(&info) }.ok()?;
    // SAFETY: the fd is freshly returned and owned.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl ExportPlane {
    /// DMA-BUF plane over a fresh descriptor of the backing memory.
    #[cfg(unix)]
    fn dmabuf(&self, ext: &khr::external_memory_fd::Device) -> Option<DmaBufPlane> {
        let fd = export_fd(ext, self.memory.memory())?;
        Some(DmaBufPlane::new(
            fd,
            self.stride,
            self.offset,
            DRM_MODIFIER_LINEAR,
        ))
    }
}

impl Drop for ExportPlane {
    fn drop(&mut self) {
        #[cfg(unix)]
        let device = self.keep.device().device();
        #[cfg(not(unix))]
        let device = &self.device;
        // SAFETY: the image is live and owned by this device.
        unsafe { device.destroy_image(self.image, None) };
    }
}

/// NV12 frame as two exportable linear planes: luma plus chroma.
pub(crate) struct ExportPair {
    /// DMA-BUF export functions.
    #[cfg(unix)]
    ext: khr::external_memory_fd::Device,
    /// Luma plane.
    y: ExportPlane,
    /// Chroma plane.
    uv: ExportPlane,
    /// Frame extent in luma samples.
    extent: vk::Extent2D,
    /// Decoded picture format the planes were copied from.
    source: vk::Format,
}

impl ExportPair {
    /// NV12 pair at `extent` copied from `source`, exportable as DMA-BUF.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate extent or when creation fails.
    pub fn create(
        device: &ash::Device,
        ext: &khr::external_memory_fd::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        extent: (u32, u32),
        source: vk::Format,
        keep: Arc<OwnedDevice>,
    ) -> Result<Self, VulkanError> {
        #[cfg(not(unix))]
        let _ = ext;
        if extent.0 == 0
            || extent.1 == 0
            || !extent.0.is_multiple_of(2)
            || !extent.1.is_multiple_of(2)
        {
            return Err(VulkanError::BadExtent {
                width: extent.0,
                height: extent.1,
            });
        }
        let luma = vk::Extent2D {
            width: extent.0,
            height: extent.1,
        };
        let chroma = vk::Extent2D {
            width: extent.0 / 2,
            height: extent.1 / 2,
        };
        let usage = vk::ImageUsageFlags::TRANSFER_DST;
        let y = ExportPlane::create(
            device,
            properties,
            vk::Format::R8_UNORM,
            luma,
            usage,
            keep.clone(),
        )?;
        let uv = ExportPlane::create(
            device,
            properties,
            vk::Format::R8G8_UNORM,
            chroma,
            usage,
            keep,
        )?;
        Ok(Self {
            #[cfg(unix)]
            ext: ext.clone(),
            y,
            uv,
            extent: luma,
            source,
        })
    }

    /// Frame extent in luma samples.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        (self.extent.width, self.extent.height)
    }

    /// Decoded picture format the planes were copied from.
    #[must_use]
    pub fn source(&self) -> vk::Format {
        self.source
    }

    /// Luma image handle.
    #[must_use]
    pub fn y_image(&self) -> vk::Image {
        self.y.image()
    }

    /// Chroma image handle.
    #[must_use]
    pub fn uv_image(&self) -> vk::Image {
        self.uv.image()
    }

    /// Platform export handle for the planes.
    ///
    /// DMA-BUF planes while both descriptors export; an opaque backend
    /// descriptor otherwise. Infallible by shape.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        #[cfg(unix)]
        {
            match (self.y.dmabuf(&self.ext), self.uv.dmabuf(&self.ext)) {
                (Some(y), Some(uv)) => GpuExport::DmaBuf {
                    format: PixelFormat::Nv12,
                    planes: [y, uv],
                },
                _ => GpuExport::Opaque { name: "vulkan" },
            }
        }
        #[cfg(not(unix))]
        {
            GpuExport::Opaque { name: "vulkan" }
        }
    }

    /// Fresh descriptors of both planes, in plane order.
    #[cfg(unix)]
    fn fresh_fds(&self) -> Option<[OwnedFd; 2]> {
        Some([
            export_fd(&self.ext, self.y.memory.memory())?,
            export_fd(&self.ext, self.uv.memory.memory())?,
        ])
    }
}

/// Device export behind a decoded frame: shared exportable planes.
pub(crate) struct DeviceExport {
    /// Exportable planes; the pool slot stays out of reuse while live.
    pair: std::sync::Arc<ExportPair>,
}

impl DeviceExport {
    /// Export over shared `pair`.
    #[must_use]
    pub fn new(pair: std::sync::Arc<ExportPair>) -> Self {
        Self { pair }
    }

    /// Shared exportable planes.
    #[must_use]
    pub(crate) fn pair(&self) -> &Arc<ExportPair> {
        &self.pair
    }

    /// Platform export handle for the frame.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        self.pair.export()
    }

    /// Frame extent in luma samples.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        self.pair.extent()
    }

    /// Decoded picture format the planes were copied from.
    #[must_use]
    pub fn source(&self) -> vk::Format {
        self.pair.source()
    }
}

/// One imported linear plane: image plus imported memory.
#[cfg(unix)]
struct ImportPlane {
    /// Device that destroys the image.
    device: ash::Device,
    /// Image handle.
    image: vk::Image,
    /// Imported backing memory, freed after the image in `Drop`.
    memory: Option<GpuMemory>,
}

#[cfg(unix)]
impl ImportPlane {
    /// Linear `format` plane at `extent` importing `fd` as DMA-BUF.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation, type picking, or binding fails.
    fn import(
        device: &ash::Device,
        ext: &khr::external_memory_fd::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        fd: OwnedFd,
    ) -> Result<Self, VulkanError> {
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::LINEAR)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external);
        // SAFETY: plain descriptor; the image is owned on success.
        let image = unsafe { device.create_image(&info, None) }?;
        // SAFETY: the image is live and owned.
        let mut requirements = unsafe { device.get_image_memory_requirements(image) };
        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        // SAFETY: the fd is live and owned.
        let queried = unsafe {
            ext.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                fd.as_raw_fd(),
                &mut fd_props,
            )
        };
        if let Err(error) = queried {
            // SAFETY: the image is live and owned.
            unsafe { device.destroy_image(image, None) };
            return Err(VulkanError::Vulkan(error));
        }
        requirements.memory_type_bits &= fd_props.memory_type_bits;
        let picked = pick_memory_type_preferred(
            requirements,
            properties,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        );
        let Some(type_index) = picked else {
            // SAFETY: the image is live and owned.
            unsafe { device.destroy_image(image, None) };
            return Err(VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
        };
        let memory = GpuMemory::allocate_imported(device, requirements.size, type_index, fd)
            .inspect_err(|_| {
                // SAFETY: the image is live and owned.
                unsafe { device.destroy_image(image, None) };
            })?;
        // SAFETY: the memory covers the requirements at offset zero.
        unsafe { device.bind_image_memory(image, memory.memory(), 0) }?;
        Ok(Self {
            device: device.clone(),
            image,
            memory: Some(memory),
        })
    }
}

#[cfg(unix)]
impl Drop for ImportPlane {
    fn drop(&mut self) {
        // SAFETY: the image is live and owned by this device.
        unsafe { self.device.destroy_image(self.image, None) };
        // Bound memory frees after the image regardless of field order.
        let _memory = self.memory.take();
    }
}

/// NV12 frame imported from a decode-side export.
pub(crate) struct ImportPair {
    /// Luma plane.
    #[cfg(unix)]
    y: ImportPlane,
    /// Chroma plane.
    #[cfg(unix)]
    uv: ImportPlane,
}

impl ImportPair {
    /// Pair importing `export` as DMA-BUF planes on `device`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a descriptor fails to export or import.
    pub fn import(
        device: &ash::Device,
        ext: &khr::external_memory_fd::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        export: &DeviceExport,
    ) -> Result<Self, VulkanError> {
        #[cfg(unix)]
        {
            let (width, height) = export.extent();
            let luma = vk::Extent2D { width, height };
            let chroma = vk::Extent2D {
                width: width / 2,
                height: height / 2,
            };
            let Some([y_fd, uv_fd]) = export.pair.fresh_fds() else {
                return Err(VulkanError::Unsupported {
                    what: "dma-buf export".to_owned(),
                });
            };
            let y = ImportPlane::import(device, ext, properties, vk::Format::R8_UNORM, luma, y_fd)?;
            let uv = ImportPlane::import(
                device,
                ext,
                properties,
                vk::Format::R8G8_UNORM,
                chroma,
                uv_fd,
            )?;
            Ok(Self { y, uv })
        }
        #[cfg(not(unix))]
        {
            let _ = (device, ext, properties, export);
            Err(VulkanError::Unsupported {
                what: "dma-buf import".to_owned(),
            })
        }
    }

    /// Luma image handle.
    #[must_use]
    pub fn y_image(&self) -> vk::Image {
        #[cfg(unix)]
        {
            self.y.image
        }
        #[cfg(not(unix))]
        {
            vk::Image::null()
        }
    }

    /// Chroma image handle.
    #[must_use]
    pub fn uv_image(&self) -> vk::Image {
        #[cfg(unix)]
        {
            self.uv.image
        }
        #[cfg(not(unix))]
        {
            vk::Image::null()
        }
    }
}

/// True when `owned` exports linear planes as DMA-BUF descriptors.
#[must_use]
pub(crate) fn probe_export(owned: &Arc<OwnedDevice>, adapter: vk::PhysicalDevice) -> bool {
    #[cfg(unix)]
    {
        let instance = owned.backend().instance();
        let device = owned.device().device();
        // SAFETY: query writes into ash-owned storage.
        let properties = unsafe { instance.get_physical_device_memory_properties(adapter) };
        let ext = khr::external_memory_fd::Device::new(instance, device);
        let pair = ExportPair::create(
            device,
            &ext,
            &properties,
            (64, 64),
            two_plane_format(),
            owned.clone(),
        );
        match pair {
            Ok(pair) => matches!(pair.export(), GpuExport::DmaBuf { .. }),
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (owned, adapter);
        false
    }
}

/// True when `owned` imports DMA-BUF planes it just exported.
#[must_use]
pub(crate) fn probe_import(owned: &Arc<OwnedDevice>, adapter: vk::PhysicalDevice) -> bool {
    #[cfg(unix)]
    {
        let instance = owned.backend().instance();
        let device = owned.device().device();
        // SAFETY: query writes into ash-owned storage.
        let properties = unsafe { instance.get_physical_device_memory_properties(adapter) };
        let ext = khr::external_memory_fd::Device::new(instance, device);
        let pair = ExportPair::create(
            device,
            &ext,
            &properties,
            (64, 64),
            two_plane_format(),
            owned.clone(),
        );
        let Ok(pair) = pair else {
            return false;
        };
        let export = DeviceExport::new(std::sync::Arc::new(pair));
        ImportPair::import(device, &ext, &properties, &export).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (owned, adapter);
        false
    }
}

/// Two-plane NV12 picture format behind export probes.
#[cfg(unix)]
fn two_plane_format() -> vk::Format {
    vk::Format::G8_B8R8_2PLANE_420_UNORM
}

/// Session description for [`VideoSession::create`].
pub(crate) struct VideoSessionDesc<'a> {
    /// Video profile; outlives creation.
    pub(crate) profile: &'a vk::VideoProfileInfoKHR<'a>,
    /// Queue family index.
    pub(crate) queue_family: u32,
    /// Picture format.
    pub(crate) format: vk::Format,
    /// Maximum coded extent.
    pub(crate) extent: vk::Extent2D,
    /// Maximum DPB slots.
    pub(crate) dpb_slots: u32,
    /// Maximum active references.
    pub(crate) active_refs: u32,
    /// Std header version; outlives creation.
    pub(crate) std_version: &'a vk::ExtensionProperties,
}

/// Owned video session with bound memory, freed on drop.
pub(crate) struct VideoSession {
    /// Device that destroys the session.
    device: ash::Device,
    /// Base video functions.
    video: khr::video_queue::Device,
    /// Session handle.
    session: vk::VideoSessionKHR,
    /// Bound session memory, dropped after the session.
    memories: Vec<GpuMemory>,
}

/// Decode-queue video commands.
pub(crate) struct DecodeQueue {
    /// Decode functions.
    decode: khr::video_decode_queue::Device,
}

impl DecodeQueue {
    /// Decode functions over `device`.
    pub fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        Self {
            decode: khr::video_decode_queue::Device::new(instance, device),
        }
    }

    /// Record a decode command.
    pub fn decode_video(&self, cmd: vk::CommandBuffer, info: &vk::VideoDecodeInfoKHR<'_>) {
        // SAFETY: inside video coding; resources are live and laid out.
        unsafe {
            (self.decode.fp().cmd_decode_video_khr)(cmd, &raw const *info);
        }
    }
}

/// Encoded parameter-set staging in bytes; sets are tens of bytes.
const ENCODED_SETS_BYTES: usize = 4096;

/// Encode-queue video commands.
pub(crate) struct EncodeQueue {
    /// Encode functions.
    encode: khr::video_encode_queue::Device,
}

impl EncodeQueue {
    /// Encode functions over `device`.
    pub fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        Self {
            encode: khr::video_encode_queue::Device::new(instance, device),
        }
    }

    /// Record an encode command.
    pub fn encode_video(&self, cmd: vk::CommandBuffer, info: &vk::VideoEncodeInfoKHR<'_>) {
        // SAFETY: inside video coding; resources are live and laid out.
        unsafe {
            (self.encode.fp().cmd_encode_video_khr)(cmd, &raw const *info);
        }
    }

    /// Driver-encoded H.264 parameter sets behind `params`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the query fails.
    pub fn encoded_parameters_h264(
        &self,
        params: vk::VideoSessionParametersKHR,
    ) -> Result<Vec<u8>, VulkanError> {
        let mut codec = vk::VideoEncodeH264SessionParametersGetInfoKHR {
            write_std_sps: vk::TRUE,
            write_std_pps: vk::TRUE,
            std_sps_id: 0,
            std_pps_id: 0,
            ..Default::default()
        };
        let get = vk::VideoEncodeSessionParametersGetInfoKHR {
            video_session_parameters: params,
            ..Default::default()
        };
        let get = get.push_next(&mut codec);
        self.encoded_parameters(&get)
    }

    /// Driver-encoded H.265 parameter sets behind `params`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the query fails.
    pub fn encoded_parameters_h265(
        &self,
        params: vk::VideoSessionParametersKHR,
    ) -> Result<Vec<u8>, VulkanError> {
        let mut codec = vk::VideoEncodeH265SessionParametersGetInfoKHR {
            write_std_vps: vk::TRUE,
            write_std_sps: vk::TRUE,
            write_std_pps: vk::TRUE,
            std_vps_id: 0,
            std_sps_id: 0,
            std_pps_id: 0,
            ..Default::default()
        };
        let get = vk::VideoEncodeSessionParametersGetInfoKHR {
            video_session_parameters: params,
            ..Default::default()
        };
        let get = get.push_next(&mut codec);
        self.encoded_parameters(&get)
    }

    /// Driver-encoded parameter sets behind `get`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the query fails.
    fn encoded_parameters(
        &self,
        get: &vk::VideoEncodeSessionParametersGetInfoKHR<'_>,
    ) -> Result<Vec<u8>, VulkanError> {
        let mut feedback = vk::VideoEncodeSessionParametersFeedbackInfoKHR::default();
        let mut data = vec![0_u8; ENCODED_SETS_BYTES];
        let mut size = data.len();
        // SAFETY: descriptors outlive the call; the buffer holds `size` bytes.
        let status = unsafe {
            (self.encode.fp().get_encoded_video_session_parameters_khr)(
                self.encode.device(),
                &raw const *get,
                &raw mut feedback,
                &raw mut size,
                data.as_mut_ptr().cast(),
            )
        };
        if status == vk::Result::INCOMPLETE {
            return Err(VulkanError::OverCaps {
                what: "encoded sets",
            });
        }
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        data.truncate(size);
        Ok(data)
    }
}

/// Fence wait per submission in nanoseconds.
pub(crate) const SUBMIT_TIMEOUT_NS: u64 = 5_000_000_000;

/// Submit `cmd` on `queue`, wait `timeout_ns` for `fence`, then reset it.
///
/// The queue `lock` stays held through the wait.
///
/// # Errors
///
/// Returns `Err` when the lock poisons, submission fails, the wait
/// fails or times out, or the reset fails.
pub(crate) fn submit_and_wait(
    device: &ash::Device,
    lock: &std::sync::Mutex<()>,
    queue: vk::Queue,
    cmd: vk::CommandBuffer,
    fence: &VkFence,
    timeout_ns: u64,
) -> Result<(), VulkanError> {
    let buffers = [cmd];
    let submit = vk::SubmitInfo::default().command_buffers(&buffers);
    let held = lock.lock().map_err(|_| VulkanError::QueuePoisoned)?;
    submit_to_queue(device, queue, &[submit], fence.fence())?;
    if !fence.wait(timeout_ns)? {
        return Err(VulkanError::Timeout);
    }
    fence.reset()?;
    drop(held);
    Ok(())
}

/// Queue submission on a raw device.
///
/// # Errors
///
/// Returns `Err` when submission fails.
pub(crate) fn submit_to_queue(
    device: &ash::Device,
    queue: vk::Queue,
    submits: &[vk::SubmitInfo<'_>],
    fence: vk::Fence,
) -> Result<(), VulkanError> {
    // SAFETY: buffers are recorded and not pending; the fence is owned.
    unsafe { device.queue_submit(queue, submits, fence) }?;
    Ok(())
}

impl VideoSession {
    /// Session over `desc`, memory bound.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation, type picking, or binding fails.
    pub fn create(
        device: &ash::Device,
        instance: &ash::Instance,
        properties: &vk::PhysicalDeviceMemoryProperties,
        desc: &VideoSessionDesc<'_>,
    ) -> Result<Self, VulkanError> {
        let video = khr::video_queue::Device::new(instance, device);
        let info = vk::VideoSessionCreateInfoKHR {
            queue_family_index: desc.queue_family,
            picture_format: desc.format,
            max_coded_extent: desc.extent,
            reference_picture_format: desc.format,
            max_dpb_slots: desc.dpb_slots,
            max_active_reference_pictures: desc.active_refs,
            p_video_profile: std::ptr::from_ref(desc.profile),
            p_std_header_version: std::ptr::from_ref(desc.std_version),
            ..Default::default()
        };
        // SAFETY: descriptors outlive the call; the session is owned on success.
        let mut session = vk::VideoSessionKHR::null();
        let status = unsafe {
            (video.fp().create_video_session_khr)(
                device.handle(),
                &raw const info,
                std::ptr::null(),
                &raw mut session,
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        let memories = Self::bind_all(device, &video, session, properties).inspect_err(|_| {
            // SAFETY: the session is live and owned; binding failed.
            unsafe {
                (video.fp().destroy_video_session_khr)(device.handle(), session, std::ptr::null());
            }
        })?;
        Ok(Self {
            device: device.clone(),
            video,
            session,
            memories,
        })
    }

    /// Session memory requirements bound to fresh allocations.
    fn bind_all(
        device: &ash::Device,
        video: &khr::video_queue::Device,
        session: vk::VideoSessionKHR,
        properties: &vk::PhysicalDeviceMemoryProperties,
    ) -> Result<Vec<GpuMemory>, VulkanError> {
        // SAFETY: the session is live and owned.
        let mut count = 0;
        let status = unsafe {
            (video.fp().get_video_session_memory_requirements_khr)(
                device.handle(),
                session,
                &raw mut count,
                std::ptr::null_mut(),
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        let mut requirements =
            vec![vk::VideoSessionMemoryRequirementsKHR::default(); count as usize];
        // SAFETY: count came from the driver.
        let status = unsafe {
            (video.fp().get_video_session_memory_requirements_khr)(
                device.handle(),
                session,
                &raw mut count,
                requirements.as_mut_ptr(),
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        let mut binds = Vec::with_capacity(requirements.len());
        let mut memories = Vec::with_capacity(requirements.len());
        for requirement in &requirements {
            let memory_requirements = requirement.memory_requirements;
            let flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
            let Some(type_index) =
                pick_memory_type_preferred(memory_requirements, properties, flags)
            else {
                return Err(VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
            };
            let memory = GpuMemory::allocate(device, memory_requirements.size, type_index)?;
            binds.push(
                vk::BindVideoSessionMemoryInfoKHR::default()
                    .memory_bind_index(requirement.memory_bind_index)
                    .memory(memory.memory())
                    .memory_offset(0)
                    .memory_size(memory_requirements.size),
            );
            memories.push(memory);
        }
        let count = u32::try_from(binds.len())
            .map_err(|_| VulkanError::Vulkan(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY))?;
        // SAFETY: memories cover their requirements; the session is live.
        let status = unsafe {
            (video.fp().bind_video_session_memory_khr)(
                device.handle(),
                session,
                count,
                binds.as_ptr(),
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        Ok(memories)
    }

    /// Session handle.
    #[must_use]
    pub fn session(&self) -> vk::VideoSessionKHR {
        self.session
    }

    /// Base video functions.
    #[must_use]
    pub fn video(&self) -> &khr::video_queue::Device {
        &self.video
    }

    /// Device behind the session.
    #[must_use]
    pub fn device(&self) -> &ash::Device {
        &self.device
    }

    /// Record a begin-coding command.
    pub fn begin_coding(&self, cmd: vk::CommandBuffer, info: &vk::VideoBeginCodingInfoKHR<'_>) {
        // SAFETY: session and params are live; slots borrow caller chains.
        unsafe {
            (self.video.fp().cmd_begin_video_coding_khr)(cmd, &raw const *info);
        }
    }

    /// Record a coding-control command.
    pub fn control_coding(&self, cmd: vk::CommandBuffer, info: &vk::VideoCodingControlInfoKHR<'_>) {
        // SAFETY: inside video coding; the control is a plain descriptor.
        unsafe {
            (self.video.fp().cmd_control_video_coding_khr)(cmd, &raw const *info);
        }
    }

    /// Record an end-coding command.
    pub fn end_coding(&self, cmd: vk::CommandBuffer, info: &vk::VideoEndCodingInfoKHR) {
        // SAFETY: inside video coding; the info is a plain descriptor.
        unsafe {
            (self.video.fp().cmd_end_video_coding_khr)(cmd, &raw const *info);
        }
    }
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        // SAFETY: the session is live and owned by this device.
        unsafe {
            (self.video.fp().destroy_video_session_khr)(
                self.device.handle(),
                self.session,
                std::ptr::null(),
            );
        }
        // Bound memory frees after the session regardless of field order.
        let _memories = std::mem::take(&mut self.memories);
    }
}

/// Owned session parameters, freed on drop.
pub(crate) struct SessionParameters {
    /// Device that destroys the parameters.
    device: ash::Device,
    /// Base video functions.
    video: khr::video_queue::Device,
    /// Parameters handle.
    params: vk::VideoSessionParametersKHR,
    /// Update sequence count.
    sequence: u32,
}

impl SessionParameters {
    /// Parameters over `session` with a codec `create` chain.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation fails.
    pub fn create(
        session: &VideoSession,
        create: &vk::VideoSessionParametersCreateInfoKHR<'_>,
    ) -> Result<Self, VulkanError> {
        // SAFETY: descriptors outlive the call; the params are owned on success.
        let mut params = vk::VideoSessionParametersKHR::null();
        let status = unsafe {
            (session.video().fp().create_video_session_parameters_khr)(
                session.device().handle(),
                &raw const *create,
                std::ptr::null(),
                &raw mut params,
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        Ok(Self {
            device: session.device().clone(),
            video: session.video().clone(),
            params,
            sequence: 0,
        })
    }

    /// Parameters handle.
    #[must_use]
    pub fn params(&self) -> vk::VideoSessionParametersKHR {
        self.params
    }

    /// Apply a codec `update` chain, bumping the sequence count.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the update fails.
    pub fn update(
        &mut self,
        update: &vk::VideoSessionParametersUpdateInfoKHR<'_>,
    ) -> Result<(), VulkanError> {
        let mut chained = *update;
        chained.update_sequence_count = self.sequence + 1;
        // SAFETY: descriptors outlive the call; the params are live.
        let status = unsafe {
            (self.video.fp().update_video_session_parameters_khr)(
                self.device.handle(),
                self.params,
                &raw const chained,
            )
        };
        if status != vk::Result::SUCCESS {
            return Err(VulkanError::Vulkan(status));
        }
        self.sequence += 1;
        Ok(())
    }
}

impl Drop for SessionParameters {
    fn drop(&mut self) {
        // SAFETY: the params are live and owned by this device.
        unsafe {
            (self.video.fp().destroy_video_session_parameters_khr)(
                self.device.handle(),
                self.params,
                std::ptr::null(),
            );
        }
    }
}

/// Owned command pool for one-shot video submissions.
pub(crate) struct CommandPool {
    /// Device that destroys the pool.
    device: ash::Device,
    /// Pool handle.
    pool: vk::CommandPool,
}

impl CommandPool {
    /// Pool over `family` with resettable buffers.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation fails.
    pub fn create(device: &ash::Device, family: u32) -> Result<Self, VulkanError> {
        let info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: plain descriptor; the pool is owned on success.
        let pool = unsafe { device.create_command_pool(&info, None) }?;
        Ok(Self {
            device: device.clone(),
            pool,
        })
    }

    /// One primary buffer from the pool.
    ///
    /// # Errors
    ///
    /// Returns `Err` when allocation fails.
    pub fn allocate(&self) -> Result<vk::CommandBuffer, VulkanError> {
        let info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool is live and owned.
        let buffers = unsafe { self.device.allocate_command_buffers(&info) }?;
        Ok(buffers[0])
    }

    /// Reset `buffer` for reuse without freeing.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the reset fails.
    pub fn reset_buffer(&self, buffer: vk::CommandBuffer) -> Result<(), VulkanError> {
        // SAFETY: the buffer came from this pool and finished its submit.
        unsafe {
            self.device
                .reset_command_buffer(buffer, vk::CommandBufferResetFlags::empty())
        }?;
        Ok(())
    }

    /// Begin a one-time recording on `buffer`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the begin fails.
    pub fn begin_buffer(&self, buffer: vk::CommandBuffer) -> Result<(), VulkanError> {
        let info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the buffer comes from the owned pool and is not pending.
        unsafe { self.device.begin_command_buffer(buffer, &info) }?;
        Ok(())
    }

    /// End the recording on `buffer`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the end fails.
    pub fn end_buffer(&self, buffer: vk::CommandBuffer) -> Result<(), VulkanError> {
        // SAFETY: all recorded commands name live owned objects.
        unsafe { self.device.end_command_buffer(buffer) }?;
        Ok(())
    }

    /// Recorder over `buffer`.
    pub fn recorder(&self, buffer: vk::CommandBuffer) -> Recorder<'_> {
        Recorder {
            device: &self.device,
            buffer,
        }
    }
}

/// Command recording over one buffer.
pub(crate) struct Recorder<'a> {
    /// Device recording the commands.
    device: &'a ash::Device,
    /// Buffer under recording.
    buffer: vk::CommandBuffer,
}

impl Recorder<'_> {
    /// Record a synchronization-2 barrier set.
    pub fn barrier(&self, dependency: &vk::DependencyInfo<'_>) {
        // SAFETY: barriers name owned objects; no host access races.
        unsafe { self.device.cmd_pipeline_barrier2(self.buffer, dependency) };
    }

    /// Record a tightly packed image-to-buffer copy.
    pub fn copy_image_to_buffer(
        &self,
        image: vk::Image,
        layout: vk::ImageLayout,
        buffer: vk::Buffer,
        regions: &[vk::BufferImageCopy],
    ) {
        // SAFETY: regions pack the owned destination buffer exactly.
        unsafe {
            self.device
                .cmd_copy_image_to_buffer(self.buffer, image, layout, buffer, regions);
        }
    }

    /// Record a tightly packed buffer-to-image copy.
    pub fn copy_buffer_to_image(
        &self,
        buffer: vk::Buffer,
        image: vk::Image,
        layout: vk::ImageLayout,
        regions: &[vk::BufferImageCopy],
    ) {
        // SAFETY: regions unpack the owned source buffer exactly.
        unsafe {
            self.device
                .cmd_copy_buffer_to_image(self.buffer, buffer, image, layout, regions);
        }
    }

    /// Record a plane-to-plane image copy.
    pub fn copy_image(
        &self,
        src: vk::Image,
        src_layout: vk::ImageLayout,
        dst: vk::Image,
        dst_layout: vk::ImageLayout,
        regions: &[vk::ImageCopy],
    ) {
        // SAFETY: regions name live owned images in the named layouts.
        unsafe {
            self.device
                .cmd_copy_image(self.buffer, src, src_layout, dst, dst_layout, regions);
        }
    }

    /// Reset `count` queries of `pool` from `first`.
    pub fn reset_queries(&self, pool: vk::QueryPool, first: u32, count: u32) {
        // SAFETY: the pool is live and owned; the range is in bounds.
        unsafe {
            self.device
                .cmd_reset_query_pool(self.buffer, pool, first, count);
        }
    }

    /// Begin query `index` of `pool`.
    pub fn begin_query(&self, pool: vk::QueryPool, index: u32) {
        // SAFETY: the pool is live and owned; the index is in bounds.
        unsafe {
            self.device
                .cmd_begin_query(self.buffer, pool, index, vk::QueryControlFlags::empty());
        }
    }

    /// End query `index` of `pool`.
    pub fn end_query(&self, pool: vk::QueryPool, index: u32) {
        // SAFETY: the pool is live and owned; the index is in bounds.
        unsafe { self.device.cmd_end_query(self.buffer, pool, index) };
    }
}

/// Owned encode-feedback query pool, freed on drop.
pub(crate) struct EncodeFeedback {
    /// Device that destroys the pool.
    device: ash::Device,
    /// Pool handle.
    pool: vk::QueryPool,
}

impl EncodeFeedback {
    /// Pool of `count` encode-feedback queries reporting `flags` for `profile`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when creation fails.
    pub fn create(
        device: &ash::Device,
        count: u32,
        flags: vk::VideoEncodeFeedbackFlagsKHR,
        profile: &vk::VideoProfileInfoKHR<'_>,
    ) -> Result<Self, VulkanError> {
        let mut feedback =
            vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default().encode_feedback_flags(flags);
        let mut chained = *profile;
        // Push order is load-bearing: ash prepends, so the profile goes
        // first and keeps its shared codec tail; the feedback node lands
        // on top with a null tail of its own.
        let info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
            .query_count(count)
            .push_next(&mut chained)
            .push_next(&mut feedback);
        // SAFETY: plain descriptor; the pool is owned on success.
        let pool = unsafe { device.create_query_pool(&info, None) }?;
        Ok(Self {
            device: device.clone(),
            pool,
        })
    }

    /// Pool handle.
    #[must_use]
    pub fn pool(&self) -> vk::QueryPool {
        self.pool
    }

    /// 64-bit results from `first` into `out`, waiting for each.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the query read fails.
    pub fn results(&self, first: u32, out: &mut [u64]) -> Result<(), VulkanError> {
        // SAFETY: the pool is live and owned; the range is in bounds.
        unsafe {
            self.device.get_query_pool_results(
                self.pool,
                first,
                out,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )
        }?;
        Ok(())
    }
}

impl Drop for EncodeFeedback {
    fn drop(&mut self) {
        // SAFETY: the pool is live and owned by this device.
        unsafe { self.device.destroy_query_pool(self.pool, None) };
    }
}

impl Drop for CommandPool {
    fn drop(&mut self) {
        // SAFETY: the pool is live and owned by this device.
        unsafe { self.device.destroy_command_pool(self.pool, None) };
    }
}

#[cfg(test)]
mod tests {
    use super::{
        pick_memory_type, pick_memory_type_preferred, pick_video_format, std_header_version,
    };
    use ash::vk;

    /// Memory properties with one host-visible and one device-local type.
    fn properties() -> vk::PhysicalDeviceMemoryProperties {
        let mut memory_types = [vk::MemoryType::default(); 32];
        memory_types[0].property_flags =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        memory_types[1].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        vk::PhysicalDeviceMemoryProperties {
            memory_type_count: 2,
            memory_types,
            ..Default::default()
        }
    }

    #[test]
    fn memory_type_matches_flags_and_bits() {
        let properties = properties();
        let requirements = vk::MemoryRequirements::default().memory_type_bits(0b11);
        assert_eq!(
            pick_memory_type(
                requirements,
                &properties,
                vk::MemoryPropertyFlags::DEVICE_LOCAL
            ),
            Some(1)
        );
        assert_eq!(
            pick_memory_type(
                requirements,
                &properties,
                vk::MemoryPropertyFlags::HOST_VISIBLE
            ),
            Some(0)
        );
        let masked = vk::MemoryRequirements::default().memory_type_bits(0b01);
        assert_eq!(
            pick_memory_type(masked, &properties, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            None
        );
        assert_eq!(
            pick_memory_type_preferred(masked, &properties, vk::MemoryPropertyFlags::DEVICE_LOCAL,),
            Some(0)
        );
        let empty = vk::MemoryRequirements::default().memory_type_bits(0);
        assert_eq!(
            pick_memory_type_preferred(empty, &properties, vk::MemoryPropertyFlags::DEVICE_LOCAL,),
            None
        );
    }

    #[test]
    fn video_format_prefers_two_plane() {
        let three =
            vk::VideoFormatPropertiesKHR::default().format(vk::Format::G8_B8_R8_3PLANE_420_UNORM);
        let two =
            vk::VideoFormatPropertiesKHR::default().format(vk::Format::G8_B8R8_2PLANE_420_UNORM);
        assert!(pick_video_format(&[three, two]) == Some(vk::Format::G8_B8R8_2PLANE_420_UNORM));
        assert!(pick_video_format(&[three]) == Some(vk::Format::G8_B8_R8_3PLANE_420_UNORM));
        assert!(pick_video_format(&[]).is_none());
    }

    #[test]
    fn std_header_version_pins_name_and_spec() {
        let name = b"VK_STD_vulkan_video_codec_h264_decode";
        let pinned = std_header_version(name, 41);
        assert_eq!(pinned.spec_version, 41);
        let bytes: Vec<u8> = pinned
            .extension_name
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| u8::try_from(*byte).unwrap_or(b'?'))
            .collect();
        assert_eq!(bytes, name);
    }
}
