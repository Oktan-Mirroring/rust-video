//! Negotiated Vulkan device behind the caps `Device` trait.

use crate::error::VulkanError;
use crate::ladder::{CodecOp, Ladder, LadderError, OpCaps};
use crate::raw::{AshBackend, RawDevice};

use rust_video_caps::traits::{Device, SharedDevice};

/// Vulkan device with a VRAM budget.
pub struct VulkanDevice {
    /// Logical device; drops before the instance below.
    device: RawDevice,
    /// Entry plus instance owning the device above.
    backend: AshBackend,
    /// Picked physical device.
    adapter: ash::vk::PhysicalDevice,
    /// Operation the device negotiated for.
    op: CodecOp,
    /// Queue family the device opened.
    queue_family: u32,
    /// Video queue at index zero.
    queue: ash::vk::Queue,
    /// Capabilities the winning rung reported.
    caps: OpCaps,
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
        let queue = won.device.queue(won.queue_family);
        let budget = backend.device_local_bytes(won.facts.handle);
        Ok(Self {
            queue_family: won.queue_family,
            caps: won.caps,
            adapter: won.facts.handle,
            device: won.device,
            backend,
            op,
            queue,
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

    /// Backend behind the device.
    #[must_use]
    pub(crate) fn backend(&self) -> &AshBackend {
        &self.backend
    }

    /// Raw device behind the tier.
    #[must_use]
    pub(crate) fn raw(&self) -> &RawDevice {
        &self.device
    }

    /// Device-local heap bytes behind the picked adapter.
    ///
    /// Live re-query of the heap size the budget was seeded from.
    #[must_use]
    pub fn device_local_bytes(&self) -> u64 {
        self.backend.device_local_bytes(self.adapter)
    }

    /// VRAM budget shared with decoders.
    #[must_use]
    pub(crate) fn shared(&self) -> &SharedDevice {
        &self.shared
    }

    /// Negotiated logical device behind the tier.
    #[must_use]
    pub fn vk_device(&self) -> &ash::Device {
        self.device.device()
    }
}

impl Device for VulkanDevice {
    fn name(&self) -> &str {
        &self.name
    }
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
