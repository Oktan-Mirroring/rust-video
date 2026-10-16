//! Negotiated Vulkan device behind the caps `Device` trait.

use std::sync::{Arc, Mutex};

use crate::error::VulkanError;
use crate::ladder::{AdapterFingerprint, CodecOp, Ladder, LadderError, OpCaps};
use crate::raw::{AshBackend, OwnedDevice, RawDevice};

use rust_video_caps::traits::{Device, SharedDevice};
use rust_video_caps::transcode::ShareMechanism;

/// Vulkan device with a VRAM budget.
///
/// `Send` plus `Sync`: the instance plus device live behind one `Arc`,
/// every other handle is an owned ash clone or an immutable fact, and
/// queue submission serializes on the queue mutex.
pub struct VulkanDevice {
    /// Owned instance plus logical device, shared with escaped frames.
    owned: Arc<OwnedDevice>,
    /// Picked physical device.
    adapter: ash::vk::PhysicalDevice,
    /// Identity stable across instances.
    fingerprint: AdapterFingerprint,
    /// Operation the device negotiated for.
    op: CodecOp,
    /// Queue family the device opened.
    queue_family: u32,
    /// Video queue at index zero.
    queue: ash::vk::Queue,
    /// Queue submission mutex; every submit holds it.
    queue_lock: Arc<Mutex<()>>,
    /// Capabilities the winning rung reported.
    caps: OpCaps,
    /// Share mechanism the adapter exports, if any.
    share: Option<ShareMechanism>,
    /// DMA-BUF export extensions enabled.
    external: bool,
    /// VRAM budget shared with decoders.
    shared: SharedDevice,
    /// Backend name.
    name: String,
}

impl VulkanDevice {
    /// Backend name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Device negotiated for `op`, or `None` when the tier skips.
    ///
    /// # Errors
    ///
    /// Returns `Err` on unexpected Vulkan failure; a declined machine
    /// is `Ok(None)`, never an error.
    pub fn open(op: CodecOp) -> Result<Option<Self>, VulkanError> {
        match Self::open_or_skip(op) {
            Ok(device) => Ok(Some(device)),
            Err(VulkanError::BackendAbsent(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Device negotiated for `op`, with the skip reason on decline.
    ///
    /// # Errors
    ///
    /// Returns `Err` with the decline reason when the tier skips, or
    /// on unexpected Vulkan failure.
    pub fn open_or_skip(op: CodecOp) -> Result<Self, VulkanError> {
        let backend = match AshBackend::load() {
            Ok(backend) => backend,
            Err(LadderError::Skip(reason)) => return Err(VulkanError::BackendAbsent(reason)),
            Err(LadderError::Hard(error)) => return Err(error),
        };
        let won = {
            let ladder = Ladder::new(&backend);
            match ladder.negotiate(op) {
                Ok(won) => won,
                Err(LadderError::Skip(reason)) => return Err(VulkanError::BackendAbsent(reason)),
                Err(LadderError::Hard(error)) => return Err(error),
            }
        };
        let queue_family = won.queue_family;
        let caps = won.caps;
        let facts = won.facts;
        let owned = Arc::new(OwnedDevice::new(won.device, backend));
        let queue = owned.device().queue(queue_family);
        let budget = owned.backend().device_local_bytes(facts.handle);
        let share = share_of(&facts.extensions);
        let external = crate::raw::external_memory_present(&facts.extensions);
        Ok(Self {
            queue_family,
            caps,
            adapter: facts.handle,
            fingerprint: facts.fingerprint,
            owned,
            op,
            queue,
            queue_lock: Arc::new(Mutex::new(())),
            share,
            external,
            shared: SharedDevice::new("vulkan", budget),
            name: "vulkan".to_owned(),
        })
    }

    /// Operation the device negotiated for.
    #[must_use]
    pub(crate) fn op(&self) -> CodecOp {
        self.op
    }

    /// Queue family the device opened.
    #[must_use]
    pub(crate) fn queue_family(&self) -> u32 {
        self.queue_family
    }

    /// Video queue at index zero.
    #[must_use]
    pub(crate) fn queue(&self) -> ash::vk::Queue {
        self.queue
    }

    /// Queue submission mutex; every submit holds it.
    #[must_use]
    pub(crate) fn queue_lock(&self) -> &Arc<Mutex<()>> {
        &self.queue_lock
    }

    /// Share mechanism the adapter exports, if any.
    #[must_use]
    pub fn share_mechanism(&self) -> Option<ShareMechanism> {
        self.share
    }

    /// DMA-BUF export extensions enabled.
    #[must_use]
    pub(crate) fn external_memory(&self) -> bool {
        self.external && self.owned.device().external_memory()
    }

    /// True when the device exports linear planes as DMA-BUF descriptors.
    #[must_use]
    pub(crate) fn dmabuf_export(&self) -> bool {
        if !self.external_memory() {
            return false;
        }
        crate::raw::probe_export(&self.owned, self.adapter)
    }

    /// True when the device imports DMA-BUF planes it just exported.
    #[must_use]
    pub(crate) fn dmabuf_import(&self) -> bool {
        if !self.external_memory() {
            return false;
        }
        crate::raw::probe_import(&self.owned, self.adapter)
    }

    /// DMA-BUF fd functions over the logical device, absent without the extensions.
    #[must_use]
    pub(crate) fn external_fd(&self) -> Option<ash::khr::external_memory_fd::Device> {
        self.external_memory().then(|| {
            ash::khr::external_memory_fd::Device::new(
                self.owned.backend().instance(),
                self.owned.device().device(),
            )
        })
    }

    /// Capabilities the winning rung reported.
    #[must_use]
    pub(crate) fn caps(&self) -> OpCaps {
        self.caps
    }

    /// Picked physical device.
    #[must_use]
    pub(crate) fn adapter(&self) -> ash::vk::PhysicalDevice {
        self.adapter
    }

    /// Picked physical device id for transcode pairing.
    #[must_use]
    pub(crate) fn adapter_id(&self) -> u64 {
        self.fingerprint.uid()
    }

    /// Backend behind the device.
    #[must_use]
    pub(crate) fn backend(&self) -> &AshBackend {
        self.owned.backend()
    }

    /// Raw device behind the tier.
    #[must_use]
    pub(crate) fn raw(&self) -> &RawDevice {
        self.owned.device()
    }

    /// Owned instance plus device, shared with escaped frames.
    #[must_use]
    pub(crate) fn owned(&self) -> Arc<OwnedDevice> {
        self.owned.clone()
    }

    /// Device-local heap bytes behind the picked adapter.
    ///
    /// Live re-query of the heap size the budget was seeded from.
    #[must_use]
    pub fn device_local_bytes(&self) -> u64 {
        self.owned.backend().device_local_bytes(self.adapter)
    }

    /// VRAM budget shared with decoders.
    #[must_use]
    pub(crate) fn shared(&self) -> &SharedDevice {
        &self.shared
    }

    /// Negotiated logical device behind the tier.
    #[must_use]
    pub fn vk_device(&self) -> &ash::Device {
        self.owned.device().device()
    }
}

impl Device for VulkanDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Share mechanism behind `extensions`, if external memory exports.
fn share_of(extensions: &[std::ffi::CString]) -> Option<ShareMechanism> {
    use std::ffi::CStr;
    let present = |want: &CStr| extensions.iter().any(|have| have.as_c_str() == want);
    if !present(ash::vk::KHR_EXTERNAL_MEMORY_NAME) {
        return None;
    }
    #[cfg(unix)]
    if present(ash::vk::KHR_EXTERNAL_MEMORY_FD_NAME) {
        return Some(ShareMechanism::DmaBuf);
    }
    #[cfg(windows)]
    if present(ash::vk::KHR_EXTERNAL_MEMORY_WIN32_NAME) {
        return Some(ShareMechanism::NtHandle);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::VulkanDevice;
    use crate::error::VulkanError;
    use crate::ladder::CodecOp;

    #[test]
    fn open_reports_skip_or_device() {
        match VulkanDevice::open(CodecOp::H264Decode) {
            Ok(Some(device)) => assert_eq!(device.name(), "vulkan"),
            Ok(None) => {}
            Err(error) => panic!("unexpected hard failure: {error}"),
        }
    }

    #[test]
    fn device_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::VulkanDevice>();
    }

    #[test]
    fn share_needs_external_memory_strings() {
        use rust_video_caps::transcode::ShareMechanism;
        use std::ffi::CString;
        assert!(super::share_of(&[]).is_none());
        let base = CString::from(ash::vk::KHR_EXTERNAL_MEMORY_NAME);
        assert!(super::share_of(std::slice::from_ref(&base)).is_none());
        #[cfg(unix)]
        {
            let fd = CString::from(ash::vk::KHR_EXTERNAL_MEMORY_FD_NAME);
            assert_eq!(
                super::share_of(&[base.clone(), fd]),
                Some(ShareMechanism::DmaBuf)
            );
        }
        #[cfg(windows)]
        {
            let win32 = CString::from(ash::vk::KHR_EXTERNAL_MEMORY_WIN32_NAME);
            assert_eq!(
                super::share_of(&[base.clone(), win32]),
                Some(ShareMechanism::NtHandle)
            );
        }
    }

    #[test]
    fn open_or_skip_names_reason() {
        match VulkanDevice::open_or_skip(CodecOp::H264Decode) {
            Ok(device) => assert_eq!(device.name(), "vulkan"),
            Err(VulkanError::BackendAbsent(reason)) => {
                assert!(!reason.to_string().is_empty());
            }
            Err(error) => panic!("unexpected hard failure: {error}"),
        }
    }
}
