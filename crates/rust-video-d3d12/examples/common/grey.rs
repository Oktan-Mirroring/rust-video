//! Mid-grey NV12 frame for d3d12 targets.

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

/// Mid-grey NV12 frame of `width` x `height`.
pub(crate) fn grey_nv12(width: u32, height: u32) -> CpuFrame {
    let size = PictureSize::new(width, height).expect("nonzero extent");
    let y_len = width as usize * height as usize;
    CpuFrame::new(
        size,
        PixelFormat::Nv12,
        vec![
            Plane::new(Bytes::from(vec![0x80; y_len]), width).expect("luma"),
            Plane::new(Bytes::from(vec![0x80; y_len / 2]), width).expect("chroma"),
        ],
    )
    .expect("frame")
}
