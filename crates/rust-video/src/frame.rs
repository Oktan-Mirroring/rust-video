//! CPU frame vocabulary: formats, planes, owned frames.

use rust_video_caps::common::PictureSize;

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
}

/// One image plane: bytes plus row stride.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Plane {
    data: Vec<u8>,
    stride: u32,
}

impl Plane {
    /// Plane over `data` with `stride` bytes per row; zero stride is rejected.
    #[must_use]
    pub fn new(data: Vec<u8>, stride: u32) -> Option<Self> {
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

#[cfg(test)]
mod tests {
    use super::{CpuFrame, PixelFormat, Plane};

    use rust_video_caps::common::PictureSize;

    fn plane() -> Plane {
        Plane::new(vec![0; 8], 4).expect("valid plane")
    }

    #[test]
    fn plane_rejects_zero_stride() {
        assert!(Plane::new(vec![1, 2], 0).is_none());
    }

    #[test]
    fn plane_stores_bytes_and_stride() {
        let plane = Plane::new(vec![1, 2, 3], 8).expect("valid stride");
        assert_eq!(plane.data(), &[1, 2, 3]);
        assert_eq!(plane.stride(), 8);
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
        let size = PictureSize::new(16, 16).expect("valid size");
        let frame =
            CpuFrame::new(size, PixelFormat::Nv12, vec![plane(), plane()]).expect("two planes");
        assert_eq!(frame.size(), size);
        assert_eq!(frame.format(), PixelFormat::Nv12);
        assert_eq!(frame.planes().len(), 2);
    }

    #[test]
    fn cpu_frame_rejects_plane_count_mismatch() {
        let size = PictureSize::new(16, 16).expect("valid size");
        assert!(CpuFrame::new(size, PixelFormat::Nv12, vec![plane()]).is_none());
        assert!(CpuFrame::new(size, PixelFormat::Rgba8, vec![plane(), plane()]).is_none());
    }
}
