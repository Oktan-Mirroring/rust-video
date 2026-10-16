//! H264 decode on the D3D11 tier, from synthetic Annex B to frames.

#[path = "common/h264.rs"]
#[cfg(target_os = "windows")]
mod h264;

#[cfg(target_os = "windows")]
use h264::annex_b_packets;

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "windows")]
use rust_video_d3d11::{D3d11Device, D3d11Frame, H264Decoder};

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
    let mut decoder = H264Decoder::open(&device);
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
}

/// One decoded frame on stdout.
#[cfg(target_os = "windows")]
fn report(frame: &D3d11Frame) {
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

#[cfg(not(target_os = "windows"))]
fn main() {}
