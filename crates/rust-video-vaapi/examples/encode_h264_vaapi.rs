//! VA-API H.264 encode: synthetic frames in, packets out.
//!
//! `encode_h264_vaapi [width height count]`; defaults `320 240 30`.

#[cfg(target_os = "linux")]
#[path = "common/synth.rs"]
mod synth;

#[cfg(target_os = "linux")]
fn main() {
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::traits::{Device, Encoder};
    use rust_video_harness::FrameHash;
    use rust_video_vaapi::{EncodeCodec, VaApiDevice, VaApiEncoder, VaApiError};

    let mut args = std::env::args().skip(1);
    let width: u32 = args
        .next()
        .map_or(Ok(320), |arg| arg.parse())
        .unwrap_or_else(|_| {
            eprintln!("usage: encode_h264_vaapi [width height count]");
            std::process::exit(2);
        });
    let height: u32 = args
        .next()
        .map_or(Ok(240), |arg| arg.parse())
        .unwrap_or_else(|_| {
            eprintln!("usage: encode_h264_vaapi [width height count]");
            std::process::exit(2);
        });
    let count: u32 = args
        .next()
        .map_or(Ok(30), |arg| arg.parse())
        .unwrap_or_else(|_| {
            eprintln!("usage: encode_h264_vaapi [width height count]");
            std::process::exit(2);
        });
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("va-api h264 encode: skipped ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api h264 encode: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api h264 encode: device {}", device.name());
    let Some(size) = PictureSize::new(width, height) else {
        eprintln!("va-api h264 encode: zero extent {width}x{height}");
        std::process::exit(2);
    };
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H264, size) {
        Ok(encoder) => encoder,
        Err(VaApiError::Unsupported(message)) => {
            println!("va-api h264 encode: unsupported ({message})");
            return;
        }
        Err(error) => {
            eprintln!("va-api h264 encode: session failed ({error})");
            std::process::exit(1);
        }
    };
    let mut packets_out = 0;
    let mut bytes_out = 0;
    for index in 0..count {
        let frame = synth::frame(size, index);
        match encoder.encode(&frame) {
            Ok(packets) => {
                for packet in packets {
                    println!(
                        "packet {packets_out} {} bytes key {} hash {}",
                        packet.data().len(),
                        packet.is_keyframe(),
                        &FrameHash::of(packet.data()).hex()[..16],
                    );
                    bytes_out += packet.data().len();
                    packets_out += 1;
                }
            }
            Err(error) => {
                eprintln!("va-api h264 encode: frame {index} failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match encoder.flush() {
        Ok(packets) => {
            packets_out += packets.len();
            bytes_out += packets
                .iter()
                .map(|packet| packet.data().len())
                .sum::<usize>();
        }
        Err(error) => {
            eprintln!("va-api h264 encode: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api h264 encode: {packets_out} packets, {bytes_out} bytes");
}

#[cfg(not(target_os = "linux"))]
fn main() {}
