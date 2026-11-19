//! Full-GPU transcode on the D3D11 tier: decode texture to encoder texture.
//!
//! Pixels stay device-resident; any hop miss declines loud with the
//! missing capability named. The committed vector transcodes without
//! readback and the bitstream lands in the temp dir for the ffmpeg oracle.

#[cfg(target_os = "windows")]
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::Device;
#[cfg(target_os = "windows")]
use rust_video_caps::transcode::Outcome;
#[cfg(target_os = "windows")]
use rust_video_d3d11::{Codec, D3d11Device, Transcoder};
#[cfg(target_os = "windows")]
use rust_video_harness::AnnexB;

#[cfg(target_os = "windows")]
fn main() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d11 device ({error})");
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
    for (index, raw) in AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl)
        .into_iter()
        .enumerate()
    {
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
    let path = std::env::temp_dir().join("rust-video-d3d11-transcode.h264");
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
}

/// One transcode outcome on stdout.
#[cfg(target_os = "windows")]
fn report(index: usize, outcome: &Outcome) {
    println!(
        "packet {index}: {} packet(s) route={:?}",
        outcome.packets().len(),
        outcome.plan().route()
    );
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

/// Committed vector path.
#[cfg(target_os = "windows")]
fn vector() -> PathBuf {
    rust_video_harness::decode_dir().join("valid_320x240.h264")
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("SKIP: d3d11 needs windows");
}
