//! Shared CPU-frame fixtures for encoder tests.

use bytes::Bytes;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

/// 64x64 mid-grey YUV420P frame.
pub(crate) fn frame_64() -> CpuFrame {
    let y = Bytes::from(vec![0x80; 64 * 64]);
    let c = Bytes::from(vec![0x80; 32 * 32]);
    let planes = [
        Plane::new(y, 64).expect("plane"),
        Plane::new(c.clone(), 32).expect("plane"),
        Plane::new(c, 32).expect("plane"),
    ];
    let size = PictureSize::new(64, 64).expect("size");
    CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
}

/// 64x64 frame with a `y_len`-byte luma plane.
pub(crate) fn frame_with_y(y_len: usize) -> CpuFrame {
    let y = Bytes::from(vec![0x80; y_len]);
    let c = Bytes::from(vec![0x80; 32 * 32]);
    let planes = [
        Plane::new(y, 64).expect("plane"),
        Plane::new(c.clone(), 32).expect("plane"),
        Plane::new(c, 32).expect("plane"),
    ];
    let size = PictureSize::new(64, 64).expect("size");
    CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
}

/// 16x16 mid-grey YUV420P frame.
pub(crate) fn frame_16() -> CpuFrame {
    let y = Bytes::from(vec![0x80; 16 * 16]);
    let c = Bytes::from(vec![0x80; 8 * 8]);
    let planes = [
        Plane::new(y, 16).expect("plane"),
        Plane::new(c.clone(), 8).expect("plane"),
        Plane::new(c, 8).expect("plane"),
    ];
    let size = PictureSize::new(16, 16).expect("size");
    CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
}
