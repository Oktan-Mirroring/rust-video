//! H264 encode on the D3D12 tier, from synthetic NV12 to packets.

#[path = "common/grey.rs"]
#[cfg(target_os = "windows")]
mod grey;

#[cfg(target_os = "windows")]
use grey::grey_nv12;

#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "windows")]
use rust_video_d3d12::{Codec, D3d12Device, D3d12Encoder};

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
    let mut encoder = match D3d12Encoder::open(&device, Codec::H264) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    println!("codec: {}", encoder.codec());
    let frame = grey_nv12(320, 240);
    let mut packets = 0;
    let mut bytes = 0;
    for _ in 0..8 {
        match encoder.encode(&frame) {
            Ok(out) => {
                packets += out.len();
                bytes += out.iter().map(|packet| packet.data().len()).sum::<usize>();
            }
            Err(error) => {
                println!("ERROR: encode: {error}");
                std::process::exit(1);
            }
        }
    }
    match encoder.flush() {
        Ok(out) => {
            packets += out.len();
            bytes += out.iter().map(|packet| packet.data().len()).sum::<usize>();
        }
        Err(error) => {
            println!("ERROR: flush: {error}");
            std::process::exit(1);
        }
    }
    if packets == 0 || bytes == 0 {
        println!("ERROR: h264 encode produced no bitstream");
        std::process::exit(1);
    }
    println!("encoded {packets} packet(s), {bytes} byte(s) (flat input only)");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
