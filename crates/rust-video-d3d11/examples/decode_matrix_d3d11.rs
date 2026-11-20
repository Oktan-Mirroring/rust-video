//! D3D11 decode matrix over the six shipped codecs.

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "windows")]
use rust_video_d3d11::{
    Av1Decoder, D3d11Device, D3d11Error, D3d11Frame, H264Decoder, HevcDecoder, Mpeg2Decoder,
    Vc1Decoder, Vp9Decoder,
};

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
    run_decode("av1", Av1Decoder::open(&device));
    run_decode("h264", H264Decoder::open(&device));
    run_decode("h265", HevcDecoder::open(&device));
    run_decode("mpeg2", Mpeg2Decoder::open(&device));
    run_decode("vc1", Vc1Decoder::open(&device));
    run_decode("vp9", Vp9Decoder::open(&device));
}

/// Empty-packet probe through an opened `decoder`.
#[cfg(target_os = "windows")]
fn run_decode<D>(name: &str, mut decoder: D)
where
    D: Decoder<Frame = D3d11Frame, Error = D3d11Error>,
{
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("{name}: empty packet -> {} frames", frames.len()),
        Err(error) => println!("{name}: empty packet rejected ({error})"),
    }
    match decoder.flush() {
        Ok(frames) => println!("{name}: flush -> {} frames", frames.len()),
        Err(error) => println!("{name}: flush rejected ({error})"),
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("SKIP: d3d11 needs windows");
}
