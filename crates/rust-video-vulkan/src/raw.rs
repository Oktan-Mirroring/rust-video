//! Owned Vulkan handles behind safe methods.
//!
//! Sole `unsafe` module: every Vulkan call lands here, each block
//! carries its own justification, and no raw handle escapes.
#![allow(unsafe_code)]
//! Justification: Vulkan is a C API; ash marks every call `unsafe`.
//! This module owns each handle from creation to `Drop`, validates
//! extension gates before touching extension function pointers, and
//! exposes only safe methods. No other module uses `unsafe`.

use std::ffi::CString;

use crate::error::{SkipReason, VulkanError};
use crate::inflight::Fence;
use crate::ladder::{AdapterFacts, Backend, CodecOp, H264Facts, LadderError};

use ash::{khr, vk};

/// Loaded Vulkan entry plus a 1.0 instance for negotiation.
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

impl AshBackend {
    /// Entry plus 1.0 instance over the system loader.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when the loader is absent, or when the
    /// instance fails to create.
    pub fn load() -> Result<Self, LadderError> {
        // SAFETY: loads the system loader; the entry owns the handle.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|_| LadderError::Skip(SkipReason::LoaderAbsent))?;
        let app_info = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_0);
        let create_info = vk::InstanceCreateInfo::default().application_info(&app_info);
        // SAFETY: descriptors point at live info structs, no extensions.
        let instance = unsafe { entry.create_instance(&create_info, None) }?;
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
        for (index, family) in families.iter().enumerate() {
            if !family
                .queue_flags
                .contains(vk::QueueFlags::VIDEO_DECODE_KHR)
            {
                continue;
            }
            if let Ok(index) = u32::try_from(index) {
                decode.push(index);
            }
        }
        let h264 = self.h264_caps(adapter, &names);
        Ok(AdapterFacts {
            handle: adapter,
            api_version: properties.api_version,
            extensions: names,
            decode_queue_families: decode,
            h264,
        })
    }

    /// H.264 caps from the first supported profile, if any.
    ///
    /// Skips the query unless every video extension string is present:
    /// ash loads a panicking stub for missing extension entry points.
    fn h264_caps(&self, adapter: vk::PhysicalDevice, extensions: &[CString]) -> Option<H264Facts> {
        let op = CodecOp::H264Decode;
        let gated = op
            .extensions()
            .iter()
            .all(|need| extensions.iter().any(|have| have.as_c_str() == *need));
        if !gated {
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
            if status == vk::Result::SUCCESS {
                Some(H264Facts {
                    coded_extent: (caps.max_coded_extent.width, caps.max_coded_extent.height),
                    dpb_slots: caps.max_dpb_slots,
                    active_refs: caps.max_active_reference_pictures,
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
        let names: Vec<*const i8> = CodecOp::H264Decode
            .extensions()
            .iter()
            .map(|name| name.as_ptr())
            .collect();
        let create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&infos)
            .enabled_extension_names(&names);
        // SAFETY: families passed the ladder; names point at static strings.
        let device = unsafe { self.instance.create_device(*adapter, &create_info, None) }?;
        Ok(RawDevice { device })
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
        queues: &[u32],
    ) -> Result<RawDevice, LadderError> {
        (*self).open_device(adapter, queues)
    }
}

/// Owned logical device with its negotiated queues.
pub(crate) struct RawDevice {
    /// Logical device handle.
    device: ash::Device,
}

impl RawDevice {
    /// Borrowed logical device for fence creation.
    #[must_use]
    pub fn device(&self) -> &ash::Device {
        &self.device
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

impl Drop for VkFence {
    fn drop(&mut self) {
        // SAFETY: fence is live and owned by this device.
        unsafe { self.device.destroy_fence(self.fence, None) };
    }
}
