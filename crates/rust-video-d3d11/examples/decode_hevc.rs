//! HEVC decode on the D3D11 tier, from the committed vector to frames.

#[path = "common/hevc.rs"]
#[cfg(target_os = "windows")]
mod hevc;

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::Decoder;
#[cfg(target_os = "windows")]
use rust_video_d3d11::{Codec, D3d11Device, D3d11Error, HevcDecoder};

#[cfg(target_os = "windows")]
fn main() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d11 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::Hevc) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    let path = std::env::args()
        .nth(1)
        .map_or_else(hevc::vector, std::path::PathBuf::from);
    let data = std::fs::read(&path).expect("vector readable");
    let raws = hevc::picture_packets(&data);
    let mut decoder = HevcDecoder::open(&device);
    let mut total = 0;
    for raw in &raws {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        match decoder.decode(&packet) {
            Ok(frames) => total += frames.len(),
            Err(D3d11Error::Skipped(reason)) => {
                println!("SKIP: {reason}");
                return;
            }
            Err(error) => panic!("decode failed: {error}"),
        }
    }
    total += decoder.flush().expect("flush drains").len();
    println!("decoded {total} frame(s), resets={}", decoder.resets());
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("SKIP: d3d11 needs windows");
}
