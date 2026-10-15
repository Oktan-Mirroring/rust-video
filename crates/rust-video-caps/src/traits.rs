//! Backend contracts: decoders and encoders.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::frame::CpuFrame;
use crate::packet::Packet;

/// Coded packets to frames.
pub trait Decoder {
    /// Decoded frame type.
    type Frame;
    /// Decode failure.
    type Error: std::error::Error;

    /// Frames decoded from `packet`, possibly none.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet`.
    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error>;
}

/// Named execution target.
pub trait Device {
    /// Backend name.
    fn name(&self) -> &str;
}

/// Frames to coded packets.
pub trait Encoder {
    /// Encode failure.
    type Error: std::error::Error;

    /// Packets coded from `frame`, possibly none.
    ///
    /// # Arguments
    ///
    /// * `frame` - raw input frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot encode `frame`.
    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error>;

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held packets.
    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error>;
}

/// Held VRAM bytes, released on drop.
#[derive(Debug)]
pub struct Lease {
    device: SharedDevice,
    bytes: u64,
}

impl Lease {
    /// Held byte count.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.device
            .inner
            .used
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// VRAM budget overrun.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OverBudget {
    requested: u64,
    available: u64,
}

impl OverBudget {
    /// Overrun of `requested` bytes against `available` bytes left.
    #[must_use]
    fn new(requested: u64, available: u64) -> Self {
        Self {
            requested,
            available,
        }
    }

    /// Bytes the failed acquisition asked for.
    #[must_use]
    pub fn requested(&self) -> u64 {
        self.requested
    }

    /// Bytes left under budget at failure time.
    #[must_use]
    pub fn available(&self) -> u64 {
        self.available
    }
}

impl fmt::Display for OverBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "requested {} bytes with {} available",
            self.requested, self.available
        )
    }
}

impl std::error::Error for OverBudget {}

/// Reference-counted device with a VRAM budget.
#[derive(Clone, Debug)]
pub struct SharedDevice {
    inner: Arc<SharedInner>,
}

impl SharedDevice {
    /// Device named `name` with `budget` bytes of VRAM.
    #[must_use]
    pub fn new(name: &str, budget: u64) -> Self {
        Self {
            inner: Arc::new(SharedInner {
                name: name.to_owned(),
                budget,
                used: AtomicU64::new(0),
            }),
        }
    }

    /// VRAM budget in bytes.
    #[must_use]
    pub fn budget(&self) -> u64 {
        self.inner.budget
    }

    /// VRAM bytes currently leased.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.inner.used.load(Ordering::Relaxed)
    }

    /// Lease of `bytes` against the budget.
    ///
    /// # Arguments
    ///
    /// * `bytes` - VRAM bytes to hold.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `bytes` exceeds the unleased budget.
    pub fn try_acquire(&self, bytes: u64) -> Result<Lease, OverBudget> {
        let mut used = self.inner.used.load(Ordering::Relaxed);
        loop {
            let total = used.saturating_add(bytes);
            if total > self.inner.budget {
                return Err(OverBudget::new(bytes, self.inner.budget - used));
            }
            match self.inner.used.compare_exchange_weak(
                used,
                total,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(Lease {
                        device: self.clone(),
                        bytes,
                    });
                }
                Err(actual) => used = actual,
            }
        }
    }
}

impl Device for SharedDevice {
    fn name(&self) -> &str {
        &self.inner.name
    }
}

#[derive(Debug)]
struct SharedInner {
    name: String,
    budget: u64,
    used: AtomicU64,
}

#[cfg(test)]
mod tests {
    use super::{Device, OverBudget, SharedDevice};

    #[test]
    fn acquire_within_budget_leases() {
        let device = SharedDevice::new("vaapi", 100);
        assert_eq!(device.name(), "vaapi");
        assert_eq!(device.budget(), 100);
        let lease = device.try_acquire(60).expect("within budget");
        assert_eq!(lease.bytes(), 60);
        assert_eq!(device.used(), 60);
    }

    #[test]
    fn acquire_past_budget_fails() {
        let device = SharedDevice::new("vaapi", 100);
        let _held = device.try_acquire(60).expect("within budget");
        let err = device.try_acquire(50).unwrap_err();
        assert_eq!(err, OverBudget::new(50, 40));
        assert_eq!(device.used(), 60);
    }

    #[test]
    fn dropped_lease_releases() {
        let device = SharedDevice::new("vaapi", 100);
        {
            let _lease = device.try_acquire(60).expect("within budget");
            assert_eq!(device.used(), 60);
        }
        assert_eq!(device.used(), 0);
        device.try_acquire(100).expect("budget freed");
    }

    #[test]
    fn shared_clone_counts_one_budget() {
        let device = SharedDevice::new("vaapi", 100);
        let mirror = device.clone();
        let _lease = mirror.try_acquire(100).expect("within budget");
        assert!(device.try_acquire(1).is_err());
    }

    #[test]
    fn over_budget_display_names_amounts() {
        let err = OverBudget::new(50, 40);
        assert_eq!(err.to_string(), "requested 50 bytes with 40 available");
    }
}
