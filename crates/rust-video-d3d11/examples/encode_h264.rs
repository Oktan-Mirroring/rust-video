//! H264 encode on the D3D11 tier, from synthetic NV12 to packets.

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::traits::{Device, Encoder};
use rust_video_d3d11::{Codec, D3d11Device, D3d11Encoder};

fn main() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d11 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    let mut encoder = match D3d11Encoder::open(&device, Codec::H264) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    println!(
        "mft: {} (d3d11: {})",
        encoder.mft_name(),
        encoder.uses_d3d11()
    );
    let frame = grey(64, 48);
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
    println!("encoded {packets} packet(s), {bytes} byte(s)");
}

/// Mid-grey NV12 frame of `width` x `height`.
fn grey(width: u32, height: u32) -> CpuFrame {
    let size = PictureSize::new(width, height).expect("nonzero extent");
    let y_len = width as usize * height as usize;
    CpuFrame::new(
        size,
        PixelFormat::Nv12,
        vec![
            Plane::new(Bytes::from(vec![0x80; y_len]), width).expect("luma"),
            Plane::new(Bytes::from(vec![0x80; y_len / 2]), width).expect("chroma"),
        ],
    )
    .expect("frame")
}
