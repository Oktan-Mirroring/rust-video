//! Picture proof: GPU-sourced frames and bitstreams.
//!
//! Skips without hardware.

#[path = "../examples/common/bits.rs"]
mod bits;
#[path = "../examples/common/grey.rs"]
mod grey;
#[path = "../examples/common/hevc.rs"]
mod hevc;

use grey::grey_nv12;
use hevc::annex_b_packets;

#[cfg(not(target_os = "windows"))]
use rust_video_d3d12::SkipReason;
use rust_video_d3d12::{
    Av1Decoder, Codec, D3d12Device, D3d12Encoder, D3d12Error, D3d12Frame, H264Decoder, HevcDecoder,
    Mpeg2Decoder, Vc1Decoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};

/// GPU-sourced frames plus bitstreams, skipped without hardware.
#[test]
fn pictures_hold_frames_and_bitstreams() {
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d12 skipped: {error}");
            #[cfg(not(target_os = "windows"))]
            assert!(matches!(
                error,
                D3d12Error::Skipped(SkipReason::PlatformUnsupported)
            ));
            return;
        }
    };
    let garbage = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
    let mut h264 = H264Decoder::open(&device);
    assert!(
        h264.decode(&garbage).is_err(),
        "synthetic input fails loud, never pixels"
    );
    let forbidden = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00]));
    let mut hevc = HevcDecoder::open(&device);
    assert!(
        hevc.decode(&forbidden).is_err(),
        "synthetic input fails loud, never pixels"
    );
    drive_hevc_decode(&device);
    drive_shell(&mut Vp9Decoder::open(&device), Codec::Vp9, &garbage);
    drive_shell(&mut Av1Decoder::open(&device), Codec::Av1, &garbage);
    drive_shell(&mut Mpeg2Decoder::open(&device), Codec::Mpeg2, &garbage);
    drive_shell(&mut Vc1Decoder::open(&device), Codec::Vc1, &garbage);
    drive_encode_roundtrip(&device, 320, 240);
    for codec in [
        Codec::Hevc,
        Codec::Vp9,
        Codec::Av1,
        Codec::Mpeg2,
        Codec::Vc1,
    ] {
        drive_encode_loud(&device, codec, 64, 48);
    }
}

/// Loud fallback for a pending `codec` builder.
fn drive_shell<D>(decoder: &mut D, codec: Codec, packet: &Packet)
where
    D: Decoder<Frame = D3d12Frame, Error = D3d12Error>,
{
    match decoder.decode(packet) {
        Ok(_) => panic!("{codec} shell decoded without a builder"),
        Err(error) => {
            println!("{codec} decode pending: {error}");
            assert!(!error.to_string().is_empty());
        }
    }
}

/// HEVC decode of a synthetic IDR through the GPU.
fn drive_hevc_decode(device: &D3d12Device) {
    let mut decoder = HevcDecoder::open(device);
    let first = annex_b_packets().into_iter().next().expect("sets plus IDR");
    let packet = Packet::new(Bytes::from(first));
    let frames = match decoder.decode(&packet) {
        Ok(frames) => frames,
        Err(error) => {
            println!("hevc decode unavailable: {error}");
            assert!(matches!(error, D3d12Error::Skipped(_)));
            return;
        }
    };
    assert_eq!(frames.len(), 1);
    let proven = frames.first().expect("frame");
    assert_eq!(proven.identity().size().width(), 320);
    assert_eq!(proven.identity().size().height(), 240);
    let y_len = 320usize * 240usize;
    assert_eq!(proven.nv12().len(), y_len + y_len / 2);
    assert!(proven.to_cpu().is_some());
    let sum: u64 = proven.nv12()[..y_len]
        .iter()
        .map(|sample| u64::from(*sample))
        .sum();
    let mean = sum / u64::try_from(y_len).expect("nonzero extent");
    assert!(mean.abs_diff(16) < 2, "black IDR luma mean {mean}");
    let uv = &proven.nv12()[y_len..];
    let (mut u_sum, mut v_sum) = (0u64, 0u64);
    for pair in uv.chunks_exact(2) {
        u_sum += u64::from(pair[0]);
        v_sum += u64::from(pair[1]);
    }
    let chroma = u64::try_from(uv.len() / 2).expect("nonzero chroma");
    let (u_mean, v_mean) = (u_sum / chroma, v_sum / chroma);
    assert!(
        u_mean.abs_diff(128) < 2 && v_mean.abs_diff(128) < 2,
        "neutral chroma u={u_mean} v={v_mean}"
    );
    println!("hevc decode: 320x240 GPU frame proven, luma mean {mean}");
}

/// H.264 encode plus decode roundtrip through the GPU.
fn drive_encode_roundtrip(device: &D3d12Device, width: u32, height: u32) {
    let mut encoder = match D3d12Encoder::open(device, Codec::H264) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("h264 encode unavailable: {error}");
            assert!(matches!(error, D3d12Error::Unsupported(_)));
            return;
        }
    };
    let frame = grey_nv12(width, height);
    let packets = match encoder.encode(&frame) {
        Ok(packets) => packets,
        Err(error) => {
            println!("h264 encode pending: {error}");
            assert!(matches!(
                error,
                D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ));
            return;
        }
    };
    assert!(!packets.is_empty());
    let bytes: Vec<u8> = packets
        .iter()
        .flat_map(|packet| packet.data().to_vec())
        .collect();
    assert!(bytes.len() > 4);
    assert_eq!(&bytes[..5], &[0, 0, 0, 1, 0x67]);
    println!("h264 encode: {width}x{height} bitstream {}B", bytes.len());
    let mut decoder = H264Decoder::open(device);
    let packet = Packet::new(Bytes::from(bytes));
    let frames = decoder.decode(&packet).expect("roundtrip decode");
    assert_eq!(frames.len(), 1);
    let proven = frames.first().expect("frame");
    assert_eq!(proven.identity().size().width(), width);
    assert_eq!(proven.identity().size().height(), height);
    let y_len = width as usize * height as usize;
    assert_eq!(proven.nv12().len(), y_len + y_len / 2);
    assert!(proven.to_cpu().is_some());
    let sum: u64 = proven.nv12()[..y_len]
        .iter()
        .map(|sample| u64::from(*sample))
        .sum();
    let mean = sum / u64::try_from(y_len).expect("nonzero extent");
    assert!(mean.abs_diff(128) < 16, "grey roundtrip mean {mean}");
    println!("h264 roundtrip: {width}x{height} GPU frame proven, luma mean {mean}");
}

/// Bitstream or loud fallback for `codec`.
fn drive_encode_loud(device: &D3d12Device, codec: Codec, width: u32, height: u32) {
    let mut encoder = match D3d12Encoder::open(device, codec) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{codec} encode unavailable: {error}");
            assert!(matches!(error, D3d12Error::Unsupported(_)));
            return;
        }
    };
    let frame = grey_nv12(width, height);
    match encoder.encode(&frame) {
        Ok(packets) => {
            let bytes: usize = packets.iter().map(|packet| packet.data().len()).sum();
            assert!(bytes > 0);
            println!("{codec} encode: {width}x{height} bitstream {bytes}B");
        }
        Err(error) => {
            println!("{codec} encode pending: {error}");
            assert!(matches!(error, D3d12Error::Unsupported(_)));
        }
    }
}
