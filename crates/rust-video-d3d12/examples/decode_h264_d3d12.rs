//! H.264 decode on the D3D12 tier, from synthetic Annex B to frames.

#[path = "common/bits.rs"]
#[cfg(target_os = "windows")]
mod bits;
#[path = "common/grey.rs"]
#[cfg(target_os = "windows")]
mod grey;
#[path = "common/h264.rs"]
#[cfg(target_os = "windows")]
mod h264;

#[cfg(target_os = "windows")]
use grey::grey_nv12;
#[cfg(target_os = "windows")]
use h264::{
    annex_b_h264_packets, annex_b_h264_packets_cropped, annex_b_h264_packets_cropped_sized,
};

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Decoder, Device, Encoder};
#[cfg(target_os = "windows")]
use rust_video_d3d12::{Codec, D3d12Device, D3d12Encoder, D3d12Frame, H264Decoder};

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
    println!("supports h264: {}", device.supports(Codec::H264));
    let mut decoder = H264Decoder::open(&device);
    let mut total = 0;
    let packets = annex_b_h264_packets()
        .into_iter()
        .chain(annex_b_h264_packets_cropped())
        .chain(annex_b_h264_packets_cropped_sized(39, 22, 4, 22))
        .chain(annex_b_h264_packets_cropped_sized(119, 67, 4, 40));
    for (index, bytes) in packets.enumerate() {
        let packet = Packet::new(Bytes::from(bytes));
        match decoder.decode(&packet) {
            Ok(frames) => {
                println!("packet {index}: {} frame(s)", frames.len());
                total += frames.len();
                for frame in &frames {
                    report(frame);
                }
            }
            Err(error) => {
                println!("ERROR: decode packet {index}: {error}");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => {
            println!("flush: {} frame(s)", frames.len());
            total += frames.len();
            for frame in &frames {
                report(frame);
            }
        }
        Err(error) => {
            println!("ERROR: flush: {error}");
            std::process::exit(1);
        }
    }
    if total == 0 {
        println!("ERROR: h264 decode produced no frames");
        std::process::exit(1);
    }
    println!("total: {total} frame(s)");
    println!("parity: size-proven only; pixel content unverified vs oracle");
    probe_encode(&device);
}

/// One decoded frame on stdout.
#[cfg(target_os = "windows")]
fn report(frame: &D3d12Frame) {
    let identity = frame.identity();
    let size = identity.size();
    println!(
        "frame gen={} {}x{} lease={} nv12={} export={:?}",
        identity.generation(),
        size.width(),
        size.height(),
        frame.lease_bytes(),
        frame.nv12().len(),
        frame.export(),
    );
    match frame.to_cpu() {
        Some(cpu) => println!("cpu: {} plane(s)", cpu.planes().len()),
        None => println!("cpu: unavailable"),
    }
}

/// H.264 encode probe on flat input.
#[cfg(target_os = "windows")]
fn probe_encode(device: &D3d12Device) {
    let mut encoder = match D3d12Encoder::open(device, Codec::H264) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("MISS: h264 encode unavailable ({error})");
            return;
        }
    };
    let frame = grey_nv12(320, 240);
    match encoder.encode(&frame) {
        Ok(packets) => {
            let bytes: usize = packets.iter().map(|packet| packet.data().len()).sum();
            println!("h264 encode: 320x240 bitstream {bytes}B (flat input only)");
        }
        Err(error) => println!("MISS: h264 encode failed ({error})"),
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}
