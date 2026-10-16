//! VA-API GPU transcode: file in, recoded packets out.
//!
//! Reads MP4 (demuxed) or raw Annex B H.264:
//! `transcode_gpu_vaapi <input> [h264|h265] [width height]`.

#[cfg(target_os = "linux")]
#[path = "common/input.rs"]
mod input;

#[cfg(target_os = "linux")]
fn main() {
    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Device;
    use rust_video_harness::FrameHash;
    use rust_video_vaapi::{EncodeCodec, Transcoder, VaApiDevice, VaApiError};

    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: transcode_gpu_vaapi <input> [h264|h265] [width height]");
        std::process::exit(2);
    };
    let codec = match args.next().as_deref() {
        None | Some("h264") => EncodeCodec::H264,
        Some("h265") => EncodeCodec::H265,
        Some(other) => {
            eprintln!("transcode_gpu_vaapi: unknown codec {other}");
            std::process::exit(2);
        }
    };
    let target = match (args.next(), args.next()) {
        (None, None) => None,
        (Some(width), Some(height)) => {
            let parsed = width.parse().and_then(|w| height.parse().map(|h| (w, h)));
            let Ok((w, h)) = parsed else {
                eprintln!("usage: transcode_gpu_vaapi <input> [h264|h265] [width height]");
                std::process::exit(2);
            };
            PictureSize::new(w, h)
        }
        _ => {
            eprintln!("usage: transcode_gpu_vaapi <input> [h264|h265] [width height]");
            std::process::exit(2);
        }
    };
    if target.is_some() && args.next().is_some() {
        eprintln!("usage: transcode_gpu_vaapi <input> [h264|h265] [width height]");
        std::process::exit(2);
    }
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("va-api transcode: skipped ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api transcode: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api transcode: device {}", device.name());
    let samples = input::samples("va-api transcode", &path);
    let mut transcoder = Transcoder::open(&device, codec);
    if let Some(size) = target {
        transcoder = transcoder.with_target(size);
    }
    println!("va-api transcode: {} packets from {path}", samples.len());
    let mut packets_out = 0;
    for sample in samples {
        let packet = Packet::new(Bytes::from(sample));
        match transcoder.transcode(&packet) {
            Ok(outcome) => {
                for packet in outcome.packets() {
                    println!(
                        "packet {packets_out} {} bytes key {} hash {} plan {:?}",
                        packet.data().len(),
                        packet.is_keyframe(),
                        &FrameHash::of(packet.data()).hex()[..16],
                        outcome.plan(),
                    );
                    packets_out += 1;
                }
            }
            Err(VaApiError::Unsupported(message)) => {
                println!("va-api transcode: unsupported ({message})");
                return;
            }
            Err(error) => {
                eprintln!("va-api transcode: failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match transcoder.flush() {
        Ok(outcome) => packets_out += outcome.packets().len(),
        Err(error) => {
            eprintln!("va-api transcode: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api transcode: {packets_out} packets");
}

#[cfg(not(target_os = "linux"))]
fn main() {}
