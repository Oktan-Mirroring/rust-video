//! D3D12 end to end: GPU transcode, cropped streams, picture
//! proof, and the adapter probe.
//!
//! Shared vector `valid_320x240.h264` holds 15 ffmpeg
//! testsrc2 pictures (320x240 constrained baseline, IDR at 0 and
//! 8); harness `AnnexB` splits it per picture.

#![cfg(target_os = "windows")]

#[path = "../examples/common/hevc.rs"]
mod hevc;

use std::path::PathBuf;

use hevc::annex_b_packets;
use rust_video_d3d12::{
    Av1Decoder, Codec, D3d12Device, D3d12Encoder, D3d12Error, D3d12Frame, H264Decoder, HevcDecoder,
    Mpeg2Decoder, Transcoder, Vc1Decoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::{EncodeConfig, Gop, Level, Profile, RateControl};
use rust_video_caps::frame::GpuExport;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder};
use rust_video_caps::transcode::Route;
use rust_video_caps::{av1, vp9};
use rust_video_harness::{decode_dir, AnnexB, H264Synth, Synth};

/// Pictures in the shared vector.
const PICTURES: usize = 15;

/// Open device, loud skip without hardware.
fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => {
            println!("adapter: {}", device.name());
            Some(device)
        }
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            None
        }
    }
}

/// Shared vector path.
fn vector() -> PathBuf {
    decode_dir().join("valid_320x240.h264")
}

/// HEVC vector bytes grouped into one packet per picture.
fn picture_packets_hevc() -> Vec<Vec<u8>> {
    let path = decode_dir().join("intra_320x240.hevc");
    let data = std::fs::read(path).expect("vector readable");
    AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl)
}

/// Bitstream transcoded from `raws`, every hop on the GPU.
fn transcode_all(transcoder: &mut Transcoder<'_>, raws: &[Vec<u8>]) -> (usize, usize, Vec<u8>) {
    let mut coded_packets = 0;
    let mut coded_bytes = 0;
    let mut out = Vec::new();
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let coded = match transcoder.transcode(&packet) {
            Ok(coded) => coded,
            Err(error) => panic!("transcode packet {index}: {error}"),
        };
        assert_eq!(
            transcoder.plan().route(),
            Route::Gpu,
            "packet {index} stays on gpu"
        );
        coded_packets += coded.len();
        for packet in &coded {
            coded_bytes += packet.data().len();
            out.extend_from_slice(packet.data());
        }
    }
    let coded = transcoder.flush().expect("flush drains");
    assert_eq!(transcoder.plan().route(), Route::Gpu, "flush stays on gpu");
    coded_packets += coded.len();
    for packet in &coded {
        coded_bytes += packet.data().len();
        out.extend_from_slice(packet.data());
    }
    assert!(coded_bytes > 0, "transcode emits a bitstream");
    (coded_packets, coded_bytes, out)
}

/// Bitstream opens on the `header` start code for `name`.
fn assert_out_opens(out: &[u8], header: &[u8], name: &str) {
    assert!(out.len() >= header.len());
    assert_eq!(&out[..header.len()], header, "bitstream opens on {name}");
}

/// ffmpeg decode over `path`, failing loud without the oracle.
fn ffmpeg_clean(path: &std::path::Path) {
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap_or_else(|error| panic!("ffmpeg oracle missing: {error}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ffmpeg rejects the bitstream: {stderr}"
    );
    assert!(output.stderr.is_empty(), "ffmpeg warnings: {stderr}");
}

/// Live H.264 transcoder, absent without hardware.
fn transcoder(device: &D3d12Device) -> Option<Transcoder<'_>> {
    Transcoder::open(device, Codec::H264, Codec::H264).ok()
}

/// Bitstream bytes behind `packets`.
fn bytes_of(packets: &[Packet]) -> usize {
    packets.iter().map(|packet| packet.data().len()).sum()
}

/// Bitstream bytes through `transcoder`, every hop on the GPU route.
fn drive(transcoder: &mut Transcoder) -> usize {
    let packets = H264Synth::annex_b_h264_packets()
        .into_iter()
        .chain(H264Synth::annex_b_h264_packets_cropped())
        .collect();
    drive_raw(transcoder, packets)
}

/// Bitstream bytes for `raws` through `transcoder`, every hop on the GPU route.
fn drive_raw(transcoder: &mut Transcoder, raws: Vec<Vec<u8>>) -> usize {
    let mut bytes = 0;
    for raw in raws {
        let packet = Packet::new(Bytes::from(raw));
        let coded = transcoder.transcode(&packet).expect("cropped transcode");
        assert!(matches!(transcoder.plan().route(), Route::Gpu));
        bytes += bytes_of(&coded);
    }
    let coded = transcoder.flush().expect("cropped flush");
    assert!(matches!(transcoder.plan().route(), Route::Gpu));
    bytes + bytes_of(&coded)
}

/// Transcoded bitstream bytes for `raws` through `transcoder`.
fn transcode_bytes(transcoder: &mut Transcoder, raws: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for raw in raws {
        let packet = Packet::new(Bytes::from(raw));
        let coded = transcoder.transcode(&packet).expect("cropped transcode");
        assert!(matches!(transcoder.plan().route(), Route::Gpu));
        for packet in &coded {
            bytes.extend_from_slice(packet.data());
        }
    }
    let coded = transcoder.flush().expect("cropped flush");
    assert!(matches!(transcoder.plan().route(), Route::Gpu));
    for packet in &coded {
        bytes.extend_from_slice(packet.data());
    }
    bytes
}

/// Luma mean of `nv12` at `width` by `height`.
fn luma_mean(nv12: &[u8], width: usize, height: usize) -> u64 {
    let y_len = width * height;
    let sum: u64 = nv12[..y_len].iter().map(|sample| u64::from(*sample)).sum();
    sum / u64::try_from(y_len).expect("nonzero extent")
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
            assert_ne!(error.to_string(), "");
        }
    }
}

/// Loud fallback for a pending probe `codec` builder.
fn probe_drive_shell<D>(decoder: &mut D, codec: Codec)
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

/// Encode config for `codec`: legacy Auto for H.264/H.265, explicit CQP
/// elsewhere; absent when `codec` has no encode profile.
fn encoder_config(codec: Codec) -> Option<EncodeConfig> {
    if matches!(codec, Codec::H264 | Codec::H265) {
        return D3d12Encoder::legacy_config(codec).ok();
    }
    let (profile, level) = match codec {
        Codec::Vp9 => (
            Profile::Vp9(vp9::EncodeProfile::P0),
            Level::Vp9(vp9::EncodeLevel::Auto),
        ),
        Codec::Av1 => (
            Profile::Av1(av1::EncodeProfile::Main),
            Level::Av1(av1::EncodeLevel::Auto),
        ),
        Codec::Vp8 => (Profile::Vp8, Level::Vp8),
        _ => return None,
    };
    Some(EncodeConfig {
        rate_control: RateControl::Cqp { qp: 26 },
        gop: Gop {
            keyint: 1,
            min_keyint: 1,
            b_frames: 0,
            refs: 1,
        },
        profile,
        level,
        ..EncodeConfig::default()
    })
}

/// Bitstream or loud fallback for `codec` at `width` x `height`.
fn drive_encoder(device: &D3d12Device, codec: Codec, width: u32, height: u32) {
    let Some(config) = encoder_config(codec) else {
        println!("{codec} encode inexpressible in EncodeConfig: skipped");
        return;
    };
    let mut encoder = match D3d12Encoder::open(device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{codec} encode unavailable: {error}");
            if matches!(codec, Codec::H264 | Codec::H265) {
                assert!(
                    matches!(error, D3d12Error::Unsupported(_)),
                    "loud fallback: {error}"
                );
                assert!(
                    error.to_string().contains("d3d12 video encode"),
                    "loud fallback: {error}"
                );
            } else {
                assert!(
                    matches!(error, D3d12Error::Declined(_)),
                    "loud fallback: {error}"
                );
            }
            return;
        }
    };
    assert_eq!(encoder.codec(), codec);
    let frame = Synth::grey_nv12(width, height);
    match encoder.encode(&frame) {
        Ok(packets) => {
            assert_ne!(packets, [] as [rust_video_caps::packet::Packet; 0]);
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
    assert_eq!(
        encoder.flush().expect("flush"),
        [] as [rust_video_caps::packet::Packet; 0]
    );
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
    assert!(frames.is_empty(), "first submit held back");
    let frames = decoder.flush().expect("flush drains");
    assert_eq!(frames.len(), 1);
    let proven = frames.first().expect("frame");
    assert_eq!(proven.identity().size().width(), 320);
    assert_eq!(proven.identity().size().height(), 240);
    let y_len = 320usize * 240usize;
    assert_eq!(proven.nv12().len(), y_len + y_len / 2);
    assert!(proven.to_cpu().is_some());
    assert!(
        proven.nv12().iter().any(|sample| *sample != 0),
        "decoded content is nonzero"
    );
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
    match proven.export() {
        GpuExport::NtHandle { handle } => {
            assert_ne!(handle, 0, "shared export carries a live handle");
            println!("hevc decode: nt-handle export proven");
        }
        GpuExport::Opaque { name } => println!("hevc decode: opaque export ({name})"),
        _ => panic!("unproven export shape"),
    }
}

/// `codec` encode plus decode roundtrip through the GPU.
fn drive_encode_roundtrip<D>(
    device: &D3d12Device,
    codec: Codec,
    width: u32,
    height: u32,
    header: &[u8],
    mut decoder: D,
) where
    D: Decoder<Frame = D3d12Frame, Error = D3d12Error>,
{
    let config = encoder_config(codec).expect("h264/h265 expressible");
    let mut encoder = match D3d12Encoder::open(device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{codec} encode unavailable: {error}");
            assert!(matches!(error, D3d12Error::Unsupported(_)));
            return;
        }
    };
    let frame = Synth::grey_nv12(width, height);
    let packets = match encoder.encode(&frame) {
        Ok(packets) => packets,
        Err(error) => {
            println!("{codec} encode pending: {error}");
            assert!(matches!(
                error,
                D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ));
            return;
        }
    };
    assert_ne!(packets, [] as [rust_video_caps::packet::Packet; 0]);
    let bytes: Vec<u8> = packets
        .iter()
        .flat_map(|packet| packet.data().to_vec())
        .collect();
    assert!(bytes.len() >= header.len());
    assert_eq!(&bytes[..header.len()], header);
    println!(
        "{codec} encode: {width}x{height} bitstream {}B",
        bytes.len()
    );
    let packet = Packet::new(Bytes::from(bytes));
    let mut frames = decoder.decode(&packet).expect("roundtrip decode");
    frames.extend(decoder.flush().expect("roundtrip drain"));
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
    println!("{codec} roundtrip: {width}x{height} GPU frame proven, luma mean {mean}");
}

/// Bitstream or loud fallback for `codec`.
fn drive_encode_loud(device: &D3d12Device, codec: Codec, width: u32, height: u32) {
    let Some(config) = encoder_config(codec) else {
        println!("{codec} encode inexpressible in EncodeConfig: skipped");
        return;
    };
    let mut encoder = match D3d12Encoder::open(device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{codec} encode unavailable: {error}");
            if matches!(codec, Codec::H264 | Codec::H265) {
                assert!(matches!(error, D3d12Error::Unsupported(_)));
            } else {
                assert!(matches!(error, D3d12Error::Declined(_)));
            }
            return;
        }
    };
    let frame = Synth::grey_nv12(width, height);
    match encoder.encode(&frame) {
        Ok(packets) => {
            let bytes: usize = packets.iter().map(|packet| packet.data().len()).sum();
            assert!(bytes > 0);
            println!("{codec} encode: {width}x{height} bitstream {bytes}B");
        }
        Err(error) => {
            println!("{codec} encode pending: {error}");
            assert!(matches!(
                error,
                D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ));
        }
    }
}

/// GPU transcode output decodes clean under ffmpeg.
#[test]
fn gpu_transcode_stays_ffmpeg_clean() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("SKIP: no h264 transcode ({error})");
            return;
        }
    };
    let raws = AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let (coded_packets, coded_bytes, out) = transcode_all(&mut transcoder, &raws);
    let header = [0, 0, 0, 1, 0x67];
    assert_out_opens(&out, &header, "SPS");
    let path = std::env::temp_dir().join("rust-video-d3d12-e2e.h264");
    std::fs::write(&path, &out).expect("oracle input writable");
    ffmpeg_clean(&path);
    std::fs::remove_file(&path).ok();
    println!("transcode: {coded_packets} packets, {coded_bytes} bytes, route=gpu, ffmpeg-clean");
}

/// HEVC GPU transcode output decodes clean under ffmpeg.
#[test]
fn gpu_transcode_hevc_stays_ffmpeg_clean() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H265, Codec::H265) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("SKIP: no hevc transcode ({error})");
            return;
        }
    };
    let raws = picture_packets_hevc();
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let (coded_packets, coded_bytes, out) = transcode_all(&mut transcoder, &raws);
    let header = [0, 0, 0, 1, 0x40, 0x01];
    assert_out_opens(&out, &header, "VPS");
    let path = std::env::temp_dir().join("rust-video-d3d12-e2e.hevc");
    std::fs::write(&path, &out).expect("oracle input writable");
    ffmpeg_clean(&path);
    std::fs::remove_file(&path).ok();
    println!(
        "transcode hevc: {coded_packets} packets, {coded_bytes} bytes, route=gpu, ffmpeg-clean"
    );
}

/// 320x224-visible stream through GPU passthrough.
#[test]
fn cropped_stream_transcodes_on_gpu_passthrough() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    assert!(drive(&mut transcoder) > 0);
}

/// 640x360-visible stream through GPU passthrough on a padded session.
#[test]
fn cropped_360p_transcodes_on_gpu_passthrough() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(39, 22, 4, 22);
    assert!(drive_raw(&mut transcoder, raws) > 0);
}

/// 1920x1080-visible stream through GPU passthrough on a padded session.
#[test]
fn cropped_1080p_transcodes_on_gpu_passthrough() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    assert!(drive_raw(&mut transcoder, raws) > 0);
}

/// Cropped-stream output decodes back to nonzero pixels.
#[test]
fn cropped_stream_output_holds_pixels() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets()
        .into_iter()
        .chain(H264Synth::annex_b_h264_packets_cropped())
        .collect();
    let bytes = transcode_bytes(&mut transcoder, raws);
    assert_ne!(bytes, [] as [u8; 0]);
    let mut decoder = H264Decoder::open(&device);
    let packet = Packet::new(Bytes::from(bytes));
    let mut frames = decoder.decode(&packet).expect("oracle decode");
    frames.extend(decoder.flush().expect("oracle drain"));
    assert!(!frames.is_empty());
    for frame in &frames {
        let size = frame.identity().size();
        let (width, height) = (size.width() as usize, size.height() as usize);
        assert_eq!((width, height), (320, 224));
        assert_eq!(frame.nv12().len(), width * height * 3 / 2);
        assert!(
            frame.nv12().iter().any(|sample| *sample != 0),
            "transcoded content is nonzero"
        );
        println!(
            "xcode oracle: {}x{} luma mean {}",
            width,
            height,
            luma_mean(frame.nv12(), width, height)
        );
    }
}

/// 320x224-visible stream through the GPU scale hop.
#[test]
fn cropped_stream_transcodes_on_gpu_scale() {
    let Some(device) = device() else {
        return;
    };
    let target = PictureSize::new(160, 112).expect("nonzero extent");
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder.with_target(target),
        Err(_) => return,
    };
    let mut bytes = 0;
    let packets = H264Synth::annex_b_h264_packets()
        .into_iter()
        .chain(H264Synth::annex_b_h264_packets_cropped());
    for raw in packets {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode(&packet) {
            Ok(coded) => {
                assert!(matches!(transcoder.plan().route(), Route::Gpu));
                bytes += bytes_of(&coded);
            }
            Err(D3d12Error::Unsupported(error)) if error.to_string().contains("scale") => {
                println!("SKIP: d3d12 scale ({error})");
                return;
            }
            Err(error) => panic!("cropped scale transcode: {error}"),
        }
    }
    let coded = transcoder.flush().expect("cropped scale flush");
    assert!(matches!(transcoder.plan().route(), Route::Gpu));
    bytes += bytes_of(&coded);
    assert!(bytes > 0);
}

/// GPU-sourced frames plus bitstreams, skipped without hardware.
#[test]
fn pictures_hold_frames_and_bitstreams() {
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
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
    drive_encode_roundtrip(
        &device,
        Codec::H264,
        320,
        240,
        &[0, 0, 0, 1, 0x67],
        H264Decoder::open(&device),
    );
    drive_encode_roundtrip(
        &device,
        Codec::H265,
        320,
        240,
        &[0, 0, 0, 1, 0x40, 0x01],
        HevcDecoder::open(&device),
    );
    for codec in [
        Codec::H265,
        Codec::Vp9,
        Codec::Av1,
        Codec::Mpeg2,
        Codec::Vc1,
    ] {
        drive_encode_loud(&device, codec, 64, 48);
    }
}

/// Adapter matrix plus picture and encoder contracts, skipped without hardware.
#[test]
fn probe_reports_or_skips() {
    let start = std::time::Instant::now();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
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
    assert_ne!(device.profiles(), []);

    let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
    let mut h264 = H264Decoder::open(&device);
    assert!(
        h264.decode(&packet).is_err(),
        "synthetic input fails loud, never pixels"
    );
    assert!(h264.flush().expect("flush").is_empty());

    probe_drive_shell(&mut HevcDecoder::open(&device), Codec::H265);
    probe_drive_shell(&mut Vp9Decoder::open(&device), Codec::Vp9);
    probe_drive_shell(&mut Av1Decoder::open(&device), Codec::Av1);
    probe_drive_shell(&mut Mpeg2Decoder::open(&device), Codec::Mpeg2);
    probe_drive_shell(&mut Vc1Decoder::open(&device), Codec::Vc1);

    for codec in Codec::ALL {
        drive_encoder(&device, *codec, 64, 48);
    }
}
