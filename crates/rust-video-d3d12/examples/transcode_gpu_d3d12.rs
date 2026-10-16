//! Full-GPU transcode on the D3D12 tier: decode texture to encoder texture.
//!
//! Pixels stay device-resident on the GPU route; any hop miss falls back
//! loud to the proven CPU path, never silently. The committed vector
//! transcodes without readback and the bitstream lands in the temp dir
//! for the ffmpeg oracle.

#[path = "common/annexb.rs"]
#[cfg(target_os = "windows")]
mod annexb;

#[cfg(target_os = "windows")]
use annexb::picture_packets;

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::common::PictureSize;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::Device;
#[cfg(target_os = "windows")]
use rust_video_caps::transcode::{Outcome, Route};
#[cfg(target_os = "windows")]
use rust_video_d3d12::{Codec, D3d12Device, D3d12Error, Transcoder};

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
        Ok(transcoder) => transcoder,
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
    for (index, raw) in picture_packets().into_iter().enumerate() {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode(&packet) {
            Ok(outcome) => {
                report(index, &outcome);
                packets += outcome.packets().len();
                bytes += bytes_of(&outcome);
                for coded in outcome.packets() {
                    out.extend_from_slice(coded.data());
                }
            }
            Err(error) => {
                println!("ERROR: transcode packet {index}: {error}");
                std::process::exit(1);
            }
        }
    }
    match transcoder.flush() {
        Ok(outcome) => {
            println!(
                "flush: {} packet(s) route={:?}",
                outcome.packets().len(),
                outcome.plan().route()
            );
            packets += outcome.packets().len();
            bytes += bytes_of(&outcome);
            for coded in outcome.packets() {
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

/// One transcode outcome on stdout.
#[cfg(target_os = "windows")]
fn report(index: usize, outcome: &Outcome) {
    let plan = outcome.plan();
    match plan.route() {
        Route::Gpu => println!(
            "packet {index}: {} packet(s) route=gpu",
            outcome.packets().len()
        ),
        Route::Cpu => println!(
            "packet {index}: {} packet(s) FALLBACK route=cpu cause={}",
            outcome.packets().len(),
            plan.cause()
                .map_or("unknown".to_owned(), |cause| cause.to_string()),
        ),
    }
}

/// Bitstream bytes behind `outcome`.
#[cfg(target_os = "windows")]
fn bytes_of(outcome: &Outcome) -> usize {
    outcome
        .packets()
        .iter()
        .map(|packet| packet.data().len())
        .sum()
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
    for (index, raw) in picture_packets().into_iter().enumerate() {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode(&packet) {
            Ok(outcome) => {
                packets += outcome.packets().len();
                bytes += bytes_of(&outcome);
            }
            Err(D3d12Error::Unsupported(error)) if error.contains("scale") => {
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
        Ok(outcome) => {
            packets += outcome.packets().len();
            bytes += bytes_of(&outcome);
        }
        Err(D3d12Error::Unsupported(error)) if error.contains("scale") => {
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

#[cfg(not(target_os = "windows"))]
fn main() {}
