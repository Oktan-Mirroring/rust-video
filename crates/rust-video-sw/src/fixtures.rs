//! Shared CPU-frame fixtures for encoder tests.

#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
use std::fmt;

use bytes::Bytes;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
use rust_video_caps::packet::Packet;
#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
use rust_video_h264::{
    nal_unit_type, parse_sps, rbsp, split_nals, H264Error, ParseKind, PermissiveGate, Sps,
    UnsupportedKind,
};

/// Grammar error sink for SPS proofs.
#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
#[derive(Debug)]
pub(crate) struct TestError(String);

#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
impl H264Error for TestError {
    fn parse(kind: ParseKind) -> Self {
        Self(format!("parse {kind:?}"))
    }

    fn unsupported(kind: UnsupportedKind) -> Self {
        Self(format!("unsupported {kind:?}"))
    }
}

#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
impl std::error::Error for TestError {}

/// SPS behind the first packet of `packets`.
#[cfg(any(feature = "h264-openh264", feature = "gpl"))]
pub(crate) fn first_sps_of(packets: &[Packet]) -> Sps {
    let nals = split_nals(packets[0].data());
    let sps = nals
        .iter()
        .find(|nal| nal_unit_type(nal[0]) == 7)
        .expect("sps present");
    let cleaned = rbsp(sps);
    parse_sps::<TestError, PermissiveGate>(&cleaned[1..]).expect("sps parses")
}

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

/// 64x64 deterministic-noise YUV420P frame from `seed`.
pub(crate) fn frame_noise_64(seed: u64) -> CpuFrame {
    let mut state = seed | 1;
    let mut fill = |len: usize| {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let chunk = state.to_le_bytes();
            let take = (len - out.len()).min(chunk.len());
            out.extend_from_slice(&chunk[..take]);
        }
        Bytes::from(out)
    };
    let planes = [
        Plane::new(fill(64 * 64), 64).expect("plane"),
        Plane::new(fill(32 * 32), 32).expect("plane"),
        Plane::new(fill(32 * 32), 32).expect("plane"),
    ];
    let size = PictureSize::new(64, 64).expect("size");
    CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
}
