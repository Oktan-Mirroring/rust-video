//! Negotiated Vulkan device behind the facade `Device` trait.

use crate::error::VulkanError;
use crate::ladder::{CodecOp, Ladder, LadderError};
use crate::raw::{AshBackend, RawDevice};

use rust_video::{Device, SharedDevice};

/// Vulkan device with a VRAM budget.
pub struct VulkanDevice {
    /// Logical device; drops before the instance below.
    device: RawDevice,
    /// Entry plus instance owning the device above.
    backend: AshBackend,
    /// Picked physical device.
    adapter: ash::vk::PhysicalDevice,
    /// VRAM budget shared with decoders.
    shared: SharedDevice,
    /// Backend name.
    name: String,
}

impl VulkanDevice {
    /// Device negotiated for `op`, or `None` when the tier skips.
    ///
    /// # Errors
    ///
    /// Returns `Err` on unexpected Vulkan failure; a declined machine
    /// is `Ok(None)`, never an error.
    pub fn open(op: CodecOp) -> Result<Option<Self>, VulkanError> {
        let Some(backend) = complete(AshBackend::load())? else {
            return Ok(None);
        };
        let (device, adapter) = {
            let ladder = Ladder::new(&backend);
            let Some(won) = complete(ladder.negotiate(op))? else {
                return Ok(None);
            };
            (won.device, won.facts.handle)
        };
        let budget = backend.device_local_bytes(adapter);
        Ok(Some(Self {
            device,
            backend,
            adapter,
            shared: SharedDevice::new("vulkan", budget),
            name: "vulkan".to_owned(),
        }))
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

/// Ladder outcome as the `open` boundary reports it.
fn complete<T>(result: Result<T, LadderError>) -> Result<Option<T>, VulkanError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(LadderError::Skip(_)) => Ok(None),
        Err(LadderError::Hard(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::complete;
    use crate::error::{SkipReason, VulkanError};
    use crate::ladder::LadderError;

    #[test]
    fn complete_passes_success_through() {
        assert_eq!(complete::<u32>(Ok(7)).expect("ok"), Some(7));
    }

    #[test]
    fn complete_maps_skip_to_none() {
        let result: Result<u32, LadderError> = Err(LadderError::Skip(SkipReason::LoaderAbsent));
        assert_eq!(complete(result).expect("skip is ok"), None);
    }

    #[test]
    fn complete_maps_hard_to_error() {
        let error = VulkanError::BadExtent {
            width: 0,
            height: 0,
        };
        let result: Result<u32, LadderError> = Err(LadderError::Hard(error));
        assert!(matches!(
            complete(result).unwrap_err(),
            VulkanError::BadExtent { .. }
        ));
    }
}
