//! CPU frame vocabulary: formats, planes, owned frames.

use std::fmt;
#[cfg(unix)]
use std::os::fd::OwnedFd;

use bytes::{Bytes, BytesMut};

use rust_video_caps::common::PictureSize;

/// CPU readback failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CpuReadback {
    /// No backend attached to the frame.
    BackendAbsent,
}

impl fmt::Display for CpuReadback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendAbsent => write!(f, "no backend attached"),
        }
    }
}

impl std::error::Error for CpuReadback {}

/// GPU frame export handle.
#[non_exhaustive]
#[derive(Debug)]
pub enum GpuExport {
    /// DMA-BUF descriptor.
    #[cfg(unix)]
    DmaBuf {
        /// Buffer file descriptor.
        fd: OwnedFd,
        /// Bytes per row.
        stride: u32,
        /// Byte offset of the first row.
        offset: u32,
        /// Format modifier.
        modifier: u64,
    },
    /// NT handle descriptor.
    #[cfg(windows)]
    NtHandle {
        /// Buffer handle.
        handle: isize,
    },
    /// Opaque backend descriptor.
    Opaque {
        /// Backend name.
        name: &'static str,
    },
}

/// Pixel layout of a CPU frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PixelFormat {
    /// Semi-planar 8-bit 4:2:0.
    Nv12,
    /// Planar 8-bit 4:2:0.
    Yuv420P,
    /// Packed 8-bit RGBA.
    Rgba8,
    /// Packed 8-bit BGRA.
    Bgra8,
}

impl PixelFormat {
    /// Plane count the format stores.
    #[must_use]
    pub fn plane_count(self) -> usize {
        match self {
            Self::Nv12 => 2,
            Self::Yuv420P => 3,
            Self::Rgba8 | Self::Bgra8 => 1,
        }
    }

    /// Bytes per luma sample, times two.
    #[must_use]
    fn doubled_bytes_per_sample(self) -> u64 {
        match self {
            Self::Nv12 | Self::Yuv420P => 3,
            Self::Rgba8 | Self::Bgra8 => 8,
        }
    }
}

/// Owned CPU frame.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CpuFrame {
    size: PictureSize,
    format: PixelFormat,
    planes: Vec<Plane>,
}

impl CpuFrame {
    /// Frame of `size` and `format` over `planes`; plane count must match the format.
    #[must_use]
    pub fn new(size: PictureSize, format: PixelFormat, planes: Vec<Plane>) -> Option<Self> {
        (planes.len() == format.plane_count()).then_some(Self {
            size,
            format,
            planes,
        })
    }

    /// Picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.size
    }

    /// Pixel layout.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Image planes.
    #[must_use]
    pub fn planes(&self) -> &[Plane] {
        &self.planes
    }
}

/// Recycled plane backing owned by one decoder.
#[derive(Debug)]
pub struct FramePool {
    size: PictureSize,
    format: PixelFormat,
    free: Vec<BytesMut>,
}

impl FramePool {
    /// Empty pool for `size` and `format` buffers.
    #[must_use]
    pub fn new(size: PictureSize, format: PixelFormat) -> Self {
        Self {
            size,
            format,
            free: Vec::new(),
        }
    }

    /// Picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.size
    }

    /// Pixel layout.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Recycled buffer count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.free.len()
    }

    /// True when no buffer is recycled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.free.is_empty()
    }

    /// Buffer capacity in bytes for the pooled size and format.
    #[must_use]
    fn buffer_len(&self) -> usize {
        let bytes = self.size.samples() * self.format.doubled_bytes_per_sample() / 2;
        usize::try_from(bytes).unwrap_or(usize::MAX)
    }

    /// Recycled buffer, or a fresh one sized for the pooled frame.
    #[must_use]
    pub fn acquire(&mut self) -> BytesMut {
        self.free.pop().map_or_else(
            || BytesMut::with_capacity(self.buffer_len()),
            |mut buffer| {
                buffer.clear();
                buffer
            },
        )
    }

    /// Return `bytes` to the pool when uniquely owned; shared buffers drop.
    pub fn recycle(&mut self, bytes: Bytes) {
        if let Ok(buffer) = bytes.try_into_mut() {
            self.free.push(buffer);
        }
    }
}

/// Device-resident frame without CPU access.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuFrame {
    size: PictureSize,
    format: PixelFormat,
    generation: u64,
}

impl GpuFrame {
    /// Device frame of `size` and `format` at `generation`.
    #[must_use]
    pub fn new(size: PictureSize, format: PixelFormat, generation: u64) -> Self {
        Self {
            size,
            format,
            generation,
        }
    }

    /// Picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.size
    }

    /// Pixel layout.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Decoder-issued sequence number.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// CPU copy of the device frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` while no backend is attached.
    pub fn try_to_cpu(&self) -> Result<CpuFrame, CpuReadback> {
        Err(CpuReadback::BackendAbsent)
    }

    /// Platform export handle for the device frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` while no backend is attached.
    pub fn export(&self) -> Result<GpuExport, CpuReadback> {
        Err(CpuReadback::BackendAbsent)
    }
}

/// One image plane: bytes plus row stride.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Plane {
    data: Bytes,
    stride: u32,
}

impl Plane {
    /// Plane over `data` with `stride` bytes per row; zero stride is rejected.
    #[must_use]
    pub fn new(data: Bytes, stride: u32) -> Option<Self> {
        (stride > 0).then_some(Self { data, stride })
    }

    /// Plane bytes in row-major order.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Bytes per row.
    #[must_use]
    pub fn stride(&self) -> u32 {
        self.stride
    }
}

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};

    use super::{CpuFrame, CpuReadback, FramePool, GpuFrame, PixelFormat, Plane};

    use rust_video_caps::common::PictureSize;

    fn plane() -> Plane {
        Plane::new(Bytes::from_static(&[0; 8]), 4).expect("valid plane")
    }

    fn size() -> PictureSize {
        PictureSize::new(16, 16).expect("valid size")
    }

    #[test]
    fn plane_rejects_zero_stride() {
        assert!(Plane::new(Bytes::from_static(&[1, 2]), 0).is_none());
    }

    #[test]
    fn plane_stores_bytes_and_stride() {
        let plane = Plane::new(Bytes::from_static(&[1, 2, 3]), 8).expect("valid stride");
        assert_eq!(plane.data(), &[1, 2, 3]);
        assert_eq!(plane.stride(), 8);
    }

    #[test]
    fn plane_shares_backing_without_copy() {
        let backing = Bytes::from_static(&[7; 8]);
        let plane = Plane::new(backing.clone(), 4).expect("valid stride");
        assert_eq!(backing.as_ptr(), plane.data().as_ptr());
    }

    #[test]
    fn pixel_format_plane_counts() {
        assert_eq!(PixelFormat::Nv12.plane_count(), 2);
        assert_eq!(PixelFormat::Yuv420P.plane_count(), 3);
        assert_eq!(PixelFormat::Rgba8.plane_count(), 1);
        assert_eq!(PixelFormat::Bgra8.plane_count(), 1);
    }

    #[test]
    fn cpu_frame_accepts_matching_plane_count() {
        let frame =
            CpuFrame::new(size(), PixelFormat::Nv12, vec![plane(), plane()]).expect("two planes");
        assert_eq!(frame.size(), size());
        assert_eq!(frame.format(), PixelFormat::Nv12);
        assert_eq!(frame.planes().len(), 2);
    }

    #[test]
    fn cpu_frame_rejects_plane_count_mismatch() {
        assert!(CpuFrame::new(size(), PixelFormat::Nv12, vec![plane()]).is_none());
        assert!(CpuFrame::new(size(), PixelFormat::Rgba8, vec![plane(), plane()]).is_none());
    }

    #[test]
    fn pool_acquire_sizes_fresh_buffer() {
        let mut pool = FramePool::new(size(), PixelFormat::Nv12);
        assert!(pool.is_empty());
        let buffer = pool.acquire();
        assert!(buffer.capacity() >= 16 * 16 * 3 / 2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn pool_recycles_unique_buffer() {
        let mut pool = FramePool::new(size(), PixelFormat::Rgba8);
        let mut buffer = BytesMut::with_capacity(8);
        buffer.extend_from_slice(&[1, 2, 3]);
        pool.recycle(buffer.freeze());
        assert_eq!(pool.len(), 1);
        let reused = pool.acquire();
        assert!(reused.is_empty());
        assert!(pool.is_empty());
    }

    #[test]
    fn pool_drops_shared_buffer() {
        let mut pool = FramePool::new(size(), PixelFormat::Rgba8);
        let bytes = Bytes::from_static(&[1, 2, 3]);
        let _shared = bytes.clone();
        pool.recycle(bytes);
        assert!(pool.is_empty());
    }

    #[test]
    fn gpu_frame_reports_absent_backend() {
        let frame = GpuFrame::new(size(), PixelFormat::Nv12, 7);
        assert_eq!(frame.size(), size());
        assert_eq!(frame.format(), PixelFormat::Nv12);
        assert_eq!(frame.generation(), 7);
        assert_eq!(frame.try_to_cpu().unwrap_err(), CpuReadback::BackendAbsent);
        assert!(frame.export().is_err());
    }

    #[test]
    fn readback_display_names_cause() {
        assert_eq!(
            CpuReadback::BackendAbsent.to_string(),
            "no backend attached"
        );
    }
}
