//! HEVC decode on the D3D12 tier, from synthetic Annex B to frames.
//!
//! Encode stays a named miss until the args builder lands.

#[path = "common/synthetic.rs"]
mod synthetic;

use synthetic::{annex_b_packets, grey_nv12};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder};
use rust_video_d3d12::{Codec, D3d12Device, D3d12Encoder, D3d12Frame, HevcDecoder};

fn main() {
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    println!("supports hevc: {}", device.supports(Codec::Hevc));
    let mut decoder = HevcDecoder::open(&device);
    let mut total = 0;
    for (index, bytes) in annex_b_packets().into_iter().enumerate() {
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
    println!("total: {total} frame(s)");
    println!("parity: size-proven only; pixel content unverified vs oracle");
    probe_encode_miss(&device);
}

/// One decoded frame on stdout.
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

/// HEVC encode attempt; the args builder is pending, so this names the miss.
fn probe_encode_miss(device: &D3d12Device) {
    let mut encoder = match D3d12Encoder::open(device, Codec::Hevc) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("MISS: hevc encode unavailable ({error})");
            return;
        }
    };
    let frame = grey_nv12(320, 240);
    match encoder.encode(&frame) {
        Ok(packets) => {
            let bytes: usize = packets.iter().map(|packet| packet.data().len()).sum();
            println!("hevc encode: 320x240 bitstream {bytes}B");
        }
        Err(error) => println!("MISS: hevc encode args pending ({error})"),
    }
}
