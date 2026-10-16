//! Synthetic NV12 frames for the VA-API examples.

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

/// Gradient frame with a moving bar at `index`.
#[must_use]
pub fn frame(size: PictureSize, index: u32) -> CpuFrame {
    let width = usize::try_from(size.width()).unwrap_or(0);
    let height = usize::try_from(size.height()).unwrap_or(0);
    let mut luma = vec![0u8; width * height];
    let bar = if height == 0 {
        0
    } else {
        (index as usize * 8) % height
    };
    for y in 0..height {
        for x in 0..width {
            let value = x + y * 3 + index as usize * 5 + if y == bar { 128 } else { 0 };
            luma[y * width + x] = u8::try_from(value % 256).unwrap_or(0);
        }
    }
    let chroma = vec![128u8; width * height / 2];
    let stride = size.width();
    CpuFrame::new(
        size,
        PixelFormat::Nv12,
        vec![
            Plane::new(Bytes::from(luma), stride).expect("luma plane"),
            Plane::new(Bytes::from(chroma), stride).expect("chroma plane"),
        ],
    )
    .expect("synth frame")
}
