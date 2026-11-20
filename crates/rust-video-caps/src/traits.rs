//! Backend contracts: decoders and encoders.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::frame::{CpuFrame, Surface};
use crate::packet::Packet;
use crate::transcode::GpuCaps;

/// Coded packets to frames.
pub trait Decoder {
    /// Decoded frame type.
    type Frame;
    /// Decode failure.
    type Error: std::error::Error;

    /// Upfront share capabilities behind the decoder.
    ///
    /// Promise; per-frame export stays the outcome.
    fn caps(&self) -> GpuCaps;

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

    /// Upfront share capabilities behind the encoder.
    ///
    /// Promise; texture import stays the outcome.
    fn caps(&self) -> GpuCaps;

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

/// Keyframe request forcing the next coded picture to IDR.
pub trait KeyframeRequest {
    /// Encode failure.
    type Error: std::error::Error;

    /// Next coded picture forced to IDR.
    ///
    /// # Errors
    ///
    /// Returns `Err` wrapping a typed decline when the backend cannot force
    /// the next picture to IDR.
    fn request_keyframe(&mut self) -> Result<(), Self::Error>;
}

/// GPU surfaces to coded packets.
pub trait EncodeSurface {
    /// GPU surface type.
    type Surface;
    /// Encode failure.
    type Error: std::error::Error;

    /// Packets coded from `surface`, possibly none.
    ///
    /// # Arguments
    ///
    /// * `surface` - GPU input surface.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot encode `surface`.
    fn encode_surface(&mut self, surface: &Self::Surface) -> Result<Vec<Packet>, Self::Error>;
}

/// In-place surface filter.
pub trait SurfaceFilter<S: Surface> {
    /// Filtered `surface` in place.
    ///
    /// # Arguments
    ///
    /// * `surface` - surface to filter.
    fn filter(&mut self, surface: &mut S);
}

impl<S: Surface, F: FnMut(&mut S)> SurfaceFilter<S> for F {
    fn filter(&mut self, surface: &mut S) {
        self(surface);
    }
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
    use super::{Decoder, Device, Lease, OverBudget, SharedDevice};
    use crate::packet::Packet;
    use crate::transcode::GpuCaps;

    use bytes::Bytes;

    struct EchoDecoder;

    impl Decoder for EchoDecoder {
        type Frame = u64;
        type Error = OverBudget;

        fn caps(&self) -> GpuCaps {
            GpuCaps::new(None, false, false)
        }

        fn decode(&mut self, packet: &Packet) -> Result<Vec<u64>, OverBudget> {
            Ok(vec![packet.data().len() as u64])
        }

        fn flush(&mut self) -> Result<Vec<u64>, OverBudget> {
            Ok(Vec::new())
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

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

    #[test]
    fn shared_device_is_send_sync() {
        assert_send_sync::<Lease>();
        assert_send_sync::<SharedDevice>();
    }

    #[test]
    fn independent_decoders_run_concurrently() {
        const STREAMS: u8 = 4;
        assert_send_sync::<EchoDecoder>();
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(usize::from(STREAMS));
            for stream in 0..STREAMS {
                handles.push(scope.spawn(move || {
                    let mut decoder = EchoDecoder;
                    let packet = Packet::new(Bytes::from(vec![stream; usize::from(stream) + 1]));
                    let frames = decoder.decode(&packet).expect("decode");
                    assert_eq!(decoder.flush().expect("flush"), []);
                    frames
                }));
            }
            for (stream, handle) in handles.into_iter().enumerate() {
                assert_eq!(handle.join().expect("thread"), vec![stream as u64 + 1]);
            }
        });
    }

    #[test]
    fn encode_surface_shape_compiles() {
        use super::EncodeSurface;
        use crate::frame::CpuFrame;
        use crate::packet::Packet;
        struct Dummy;
        impl EncodeSurface for Dummy {
            type Surface = CpuFrame;
            type Error = std::io::Error;
            fn encode_surface(&mut self, _surface: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
                Ok(Vec::new())
            }
        }
        fn assert_encode_surface<T: EncodeSurface>() {}
        assert_encode_surface::<Dummy>();
    }

    #[test]
    fn closure_filters_cpu_frame() {
        use super::SurfaceFilter;
        use crate::common::PictureSize;
        use crate::frame::{CpuFrame, PixelFormat, Plane, Surface};
        fn apply<S: Surface>(filter: &mut impl SurfaceFilter<S>, surface: &mut S) {
            filter.filter(surface);
        }
        let size = PictureSize::new(16, 16).expect("valid size");
        let plane = || Plane::new(Bytes::from_static(&[0; 8]), 4).expect("valid plane");
        let mut frame =
            CpuFrame::new(size, PixelFormat::Nv12, vec![plane(), plane()]).expect("two planes");
        let mut observed = None;
        let mut filter = |surface: &mut CpuFrame| {
            observed = Some(surface.size());
        };
        apply(&mut filter, &mut frame);
        assert_eq!(observed, Some(size));
    }

    #[test]
    fn encoder_keyframe_shape_compiles() {
        use super::{Encoder, KeyframeRequest};
        use crate::frame::CpuFrame;
        struct Dummy;
        impl Encoder for Dummy {
            type Error = std::io::Error;
            fn caps(&self) -> GpuCaps {
                GpuCaps::new(None, false, false)
            }
            fn encode(&mut self, _frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
                Ok(Vec::new())
            }
            fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
                Ok(Vec::new())
            }
        }
        impl KeyframeRequest for Dummy {
            type Error = std::io::Error;
            fn request_keyframe(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }
        fn assert_encoder<T: Encoder>() {}
        fn assert_keyframe<T: KeyframeRequest>() {}
        assert_encoder::<Dummy>();
        assert_keyframe::<Dummy>();
        let mut dummy = Dummy;
        dummy.request_keyframe().expect("keyframe");
    }
}
