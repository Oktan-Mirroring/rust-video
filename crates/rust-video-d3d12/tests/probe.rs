#![cfg(target_os = "windows")]

//! Device-tier probe: adapter, profiles, pictures, encoder.
//!
//! Prints the adapter matrix on hardware; passes without hardware.

use rust_video_d3d12::{
    Av1Decoder, Codec, D3d12Device, D3d12Encoder, D3d12Error, D3d12Frame, H264Decoder, HevcDecoder,
    Mpeg2Decoder, Vc1Decoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder};

/// Adapter matrix plus picture and encoder contracts, skipped without hardware.
#[test]
fn probe_reports_or_skips() {
    let start = std::time::Instant::now();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d12 skipped: {error}");
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
        println!(
            "supports {codec}: decode={} encode={}",
            device.supports(*codec),
            device.supports_encode(*codec)
        );
    }
    for profile in device.profiles() {
        println!("profile: {} ({})", profile.name(), profile.codec());
    }
    assert!(device.supports(Codec::H264));
    assert!(!device.profiles().is_empty());

    let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
    let mut h264 = H264Decoder::open(&device);
    assert!(
        h264.decode(&packet).is_err(),
        "synthetic input fails loud, never pixels"
    );
    assert!(h264.flush().expect("flush").is_empty());

    drive_shell(&mut HevcDecoder::open(&device), Codec::Hevc);
    drive_shell(&mut Vp9Decoder::open(&device), Codec::Vp9);
    drive_shell(&mut Av1Decoder::open(&device), Codec::Av1);
    drive_shell(&mut Mpeg2Decoder::open(&device), Codec::Mpeg2);
    drive_shell(&mut Vc1Decoder::open(&device), Codec::Vc1);

    for codec in Codec::ALL {
        drive_encoder(&device, *codec, 64, 48);
    }
}

/// Loud fallback for a pending `codec` builder.
fn drive_shell<D>(decoder: &mut D, codec: Codec)
where
    D: Decoder<Frame = D3d12Frame, Error = D3d12Error>,
{
    let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
    match decoder.decode(&packet) {
        Ok(_) => panic!("{codec} shell decoded without a builder"),
        Err(error) => println!("{codec} decode pending: {error}"),
    }
    assert!(decoder.flush().expect("flush").is_empty());
}

/// Bitstream or loud fallback for `codec` at `width` x `height`.
fn drive_encoder(device: &D3d12Device, codec: Codec, width: u32, height: u32) {
    let mut encoder = match D3d12Encoder::open(device, codec) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{codec} encode unavailable: {error}");
            assert!(
                matches!(error, D3d12Error::Unsupported(_)),
                "loud fallback: {error}"
            );
            assert!(
                error.to_string().contains("d3d12 video encode"),
                "loud fallback: {error}"
            );
            return;
        }
    };
    assert_eq!(encoder.codec(), codec);
    let frame = grey_nv12(width, height);
    match encoder.encode(&frame) {
        Ok(packets) => {
            assert!(!packets.is_empty());
            assert!(packets.iter().any(|packet| !packet.data().is_empty()));
        }
        Err(error) => {
            println!("{codec} encode pending: {error}");
            assert!(
                matches!(
                    error,
                    D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
                ),
                "loud fallback: {error}"
            );
        }
    }
    assert!(encoder.flush().expect("flush").is_empty());
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
