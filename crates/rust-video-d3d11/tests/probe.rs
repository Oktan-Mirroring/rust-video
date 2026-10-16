#![cfg(target_os = "windows")]

//! Device-tier probe: adapter, profiles, shells, encoder.
//!
//! Prints the adapter matrix on hardware; passes without hardware.

use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Encoder, D3d11Error, H264Decoder, HevcDecoder,
    Mpeg2Decoder, Vc1Decoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder};

/// Adapter matrix plus shell and encoder contracts, skipped without hardware.
#[test]
fn probe_reports_or_skips() {
    let start = std::time::Instant::now();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    println!(
        "adapter: {} (feature level {:#x}, open in {:?})",
        device.name(),
        device.feature_level(),
        start.elapsed()
    );
    for codec in Codec::ALL {
        println!("supports {codec}: {}", device.supports(*codec));
    }
    for profile in device.profiles() {
        println!("profile: {} ({})", profile.name(), profile.codec());
    }
    assert!(device.supports(Codec::H264));
    assert!(!device.profiles().is_empty());

    let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
    let mut h264 = H264Decoder::open(&device);
    assert!(h264.decode(&packet).is_err());
    assert!(h264.flush().expect("flush").is_empty());

    let mut hevc = HevcDecoder::open(&device);
    assert!(hevc.decode(&packet).is_err());
    assert!(hevc.flush().expect("flush").is_empty());
    let mut vp9 = Vp9Decoder::open(&device);
    assert!(vp9.decode(&packet).is_err());
    assert!(vp9.flush().expect("flush").is_empty());
    let mut av1 = Av1Decoder::open(&device);
    assert!(av1.decode(&packet).is_err());
    assert!(av1.flush().expect("flush").is_empty());
    let mut mpeg2 = Mpeg2Decoder::open(&device);
    assert!(mpeg2.decode(&packet).is_err());
    assert!(mpeg2.flush().expect("flush").is_empty());
    let mut vc1 = Vc1Decoder::open(&device);
    assert!(vc1.decode(&packet).is_err());
    assert!(vc1.flush().expect("flush").is_empty());

    drive_encoder(&device, Codec::H264, 1280, 720, true);
    drive_encoder(&device, Codec::H264, 64, 48, true);
    for codec in [
        Codec::Hevc,
        Codec::Av1,
        Codec::Vp9,
        Codec::Mpeg2,
        Codec::Vc1,
    ] {
        drive_encoder(&device, codec, 64, 48, false);
    }
}

/// Eight frames plus a drain through `codec`, or its loud fallback.
fn drive_encoder(device: &D3d11Device, codec: Codec, width: u32, height: u32, required: bool) {
    let mut encoder = match D3d11Encoder::open(device, codec) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{codec} encode unavailable: {error}");
            assert!(!required, "{codec} encoder required: {error}");
            assert!(
                matches!(error, D3d11Error::Unsupported(_)),
                "loud fallback: {error}"
            );
            assert!(
                error.to_string().contains("encoder MFT"),
                "loud fallback: {error}"
            );
            return;
        }
    };
    assert_eq!(encoder.codec(), codec);
    let frame = grey_nv12(width, height);
    let start = std::time::Instant::now();
    let mut count = 0usize;
    let mut bytes = 0usize;
    for _ in 0..8 {
        let packets = encoder.encode(&frame).expect("encode frame");
        count += packets.len();
        bytes += packets
            .iter()
            .map(|packet| packet.data().len())
            .sum::<usize>();
    }
    let drained = encoder.flush().expect("flush");
    count += drained.len();
    bytes += drained
        .iter()
        .map(|packet| packet.data().len())
        .sum::<usize>();
    println!(
        "{codec} encode: {count} packets, {bytes} bytes via {}, d3d11: {} in {:?}",
        encoder.mft_name(),
        encoder.uses_d3d11(),
        start.elapsed()
    );
    assert!(bytes > 0, "{codec} encoder emitted no bytes");
}

/// Mid-grey NV12 frame of `width` x `height`.
fn grey_nv12(width: u32, height: u32) -> CpuFrame {
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
