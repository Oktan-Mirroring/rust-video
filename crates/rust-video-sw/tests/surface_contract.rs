//! Software surface contract: geometry, ticket, and wait over CPU frames.

#![cfg(any(feature = "ffmpeg", feature = "h264-openh264"))]

use bytes::Bytes;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport, PixelFormat, Plane};
use rust_video_sw::SwSurface;

/// 4x2 YUV420P frame with distinct plane bytes.
fn frame_4x2() -> CpuFrame {
    let size = PictureSize::new(4, 2).expect("extent");
    let y = Plane::new(Bytes::from(vec![1; 8]), 4).expect("luma");
    let u = Plane::new(Bytes::from(vec![2; 2]), 2).expect("chroma u");
    let v = Plane::new(Bytes::from(vec![3; 2]), 2).expect("chroma v");
    CpuFrame::new(size, PixelFormat::Yuv420P, vec![y, u, v]).expect("frame")
}

#[test]
fn cpu_surface_names_single_layer_geometry() {
    let frame = frame_4x2();
    assert_eq!(frame.slice(), 0);
    assert_eq!(frame.coded(), frame.size());
    assert_eq!(frame.origin(), (0, 0));
}

#[test]
fn cpu_surface_exports_opaque_and_waits_clean() {
    let frame = frame_4x2();
    let GpuExport::Opaque { name } = frame.export() else {
        panic!("sw export stays opaque");
    };
    assert_eq!(name, "sw");
    frame.wait();
}
