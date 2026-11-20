//! Full-GPU transcode on the D3D12 tier: decode texture to encoder texture.
//!
//! Pixels stay device-resident; any hop miss declines loud with the
//! missing capability named. The committed vector transcodes under the
//! default config with a no-op surface filter and a no-op CPU callback,
//! and the bitstream lands in the temp dir for the ffmpeg oracle.

#[cfg(target_os = "windows")]
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::common::PictureSize;
#[cfg(target_os = "windows")]
use rust_video_caps::encode::EncodeConfig;
#[cfg(target_os = "windows")]
use rust_video_caps::frame::CpuFrame;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::Device;
#[cfg(target_os = "windows")]
use rust_video_d3d12::{Codec, D3d12Device, D3d12Error, GpuSurface, Transcoder};
#[cfg(target_os = "windows")]
use rust_video_harness::AnnexB;

#[cfg(target_os = "windows")]
fn main() {
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder.with_config(EncodeConfig::default()),
        Err(error) => {
            println!("SKIP: no h264 transcode ({error})");
            return;
        }
    };
    let caps = transcoder.caps();
    println!(
        "caps: share={:?} decode_export={} encode_import={} ready={}",
        caps.share(),
        caps.decode_export(),
        caps.encode_import(),
        caps.ready()
    );
    let mut packets = 0;
    let mut bytes = 0;
    let mut out = Vec::new();
    let mut surface_filter = |_surface: &mut GpuSurface| {};
    let mut cpu_callback = |_frame: &mut CpuFrame| {};
    for (index, raw) in AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl)
        .into_iter()
        .enumerate()
    {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode_surface_with(&packet, &mut surface_filter) {
            Ok(coded_packets) => {
                report(index, &coded_packets);
                packets += coded_packets.len();
                bytes += bytes_of(&coded_packets);
                for coded in &coded_packets {
                    out.extend_from_slice(coded.data());
                }
            }
            Err(error) => {
                println!("ERROR: transcode packet {index}: {error}");
                std::process::exit(1);
            }
        }
    }
    match transcoder.flush_with(&mut cpu_callback) {
        Ok(coded_packets) => {
            println!(
                "flush: {} packet(s) route={:?}",
                coded_packets.len(),
                transcoder.plan().route()
            );
            packets += coded_packets.len();
            bytes += bytes_of(&coded_packets);
            for coded in &coded_packets {
                out.extend_from_slice(coded.data());
            }
        }
        Err(error) => {
            println!("ERROR: flush: {error}");
            std::process::exit(1);
        }
    }
    if packets == 0 || bytes == 0 {
        println!("ERROR: transcode produced no bitstream");
        std::process::exit(1);
    }
    let path = std::env::temp_dir().join("rust-video-d3d12-transcode.h264");
    match std::fs::write(&path, &out) {
        Ok(()) => println!(
            "bitstream: {} (oracle: ffmpeg -v error -i <file> -f null -)",
            path.display()
        ),
        Err(error) => {
            println!("ERROR: bitstream unwritable: {error}");
            std::process::exit(1);
        }
    }
    println!("transcoded {packets} packet(s), {bytes} byte(s)");
    probe_scale(&device);
}

/// One transcode result on stdout.
#[cfg(target_os = "windows")]
fn report(index: usize, packets: &[Packet]) {
    println!("packet {index}: {} packet(s)", packets.len());
}

/// Bitstream bytes behind `packets`.
#[cfg(target_os = "windows")]
fn bytes_of(packets: &[Packet]) -> usize {
    packets.iter().map(|packet| packet.data().len()).sum()
}

/// Scale request through the GPU scale hop; a box without video
/// process support keeps the loud miss.
#[cfg(target_os = "windows")]
fn probe_scale(device: &D3d12Device) {
    let target = PictureSize::new(160, 112).expect("nonzero extent");
    let mut transcoder = match Transcoder::open(device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder.with_target(target),
        Err(error) => {
            println!("SKIP: scale probe without transcode ({error})");
            return;
        }
    };
    let mut packets = 0;
    let mut bytes = 0;
    for (index, raw) in AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl)
        .into_iter()
        .enumerate()
    {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode(&packet) {
            Ok(coded) => {
                packets += coded.len();
                bytes += bytes_of(&coded);
            }
            Err(D3d12Error::Unsupported(error)) if error.to_string().contains("scale") => {
                println!("MISS: {error}");
                return;
            }
            Err(error) => {
                println!("ERROR: scale packet {index}: {error}");
                std::process::exit(1);
            }
        }
    }
    match transcoder.flush() {
        Ok(coded) => {
            packets += coded.len();
            bytes += bytes_of(&coded);
        }
        Err(D3d12Error::Unsupported(error)) if error.to_string().contains("scale") => {
            println!("MISS: {error}");
            return;
        }
        Err(error) => {
            println!("ERROR: scale flush: {error}");
            std::process::exit(1);
        }
    }
    if packets == 0 || bytes == 0 {
        println!("ERROR: scale probe produced no bitstream");
        std::process::exit(1);
    }
    println!("scale: to 160x112 bitstream {bytes}B route=gpu");
}

/// Committed vector path.
#[cfg(target_os = "windows")]
fn vector() -> PathBuf {
    rust_video_harness::decode_dir().join("valid_320x240.h264")
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("SKIP: d3d12 needs windows");
}
