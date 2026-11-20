//! VA-API decode matrix over the five shipped codecs.

#[cfg(target_os = "linux")]
use bytes::Bytes;
#[cfg(target_os = "linux")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "linux")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "linux")]
use rust_video_vaapi::{
    Av1Decoder, H264Decoder, H265Decoder, VaApiDevice, VaApiError, VaApiFrame, Vp8Decoder,
    Vp9Decoder,
};

#[cfg(target_os = "linux")]
fn main() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("SKIP va-api decode matrix ({reason})");
            return;
        }
        Err(error) => {
            println!("SKIP va-api decode matrix: device failed ({error})");
            return;
        }
    };
    println!("va-api decode matrix: device {}", device.name());
    run_decode("av1", Av1Decoder::open(&device));
    run_decode("h264", H264Decoder::open(&device));
    run_decode("h265", H265Decoder::open(&device));
    run_decode("vp8", Vp8Decoder::open(&device));
    run_decode("vp9", Vp9Decoder::open(&device));
}

/// Empty-packet probe through an opened `decoder`.
#[cfg(target_os = "linux")]
fn run_decode<D>(name: &str, mut decoder: D)
where
    D: Decoder<Frame = VaApiFrame, Error = VaApiError>,
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

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("SKIP: vaapi needs linux");
}
