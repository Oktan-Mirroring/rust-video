//! HEVC decode probe on the D3D11 tier: profile check plus pending-builder handling.

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_d3d11::{Codec, D3d11Device, D3d11Error, HevcDecoder};

fn main() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d11 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    println!("supports hevc: {}", device.supports(Codec::Hevc));
    let mut decoder = HevcDecoder::open(&device);
    let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x40, 0x01]));
    match decoder.decode(&packet) {
        Ok(frames) => println!("decoded {} frame(s)", frames.len()),
        Err(D3d11Error::Skipped(reason)) => println!("SKIP: {reason}"),
        Err(error) => println!("pending builder: {error}"),
    }
    match decoder.flush() {
        Ok(frames) => println!("flush: {} frame(s)", frames.len()),
        Err(error) => println!("flush: {error}"),
    }
}
