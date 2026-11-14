//! H.264 encode probe on the `MediaCodec` tier.

#[cfg(target_os = "android")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "android")]
use rust_video_harness::Synth;
#[cfg(target_os = "android")]
use rust_video_mediacodec::{EncodeSpec, MediaCodecDevice, MediaCodecEncoder, QuirkTable};

#[cfg(target_os = "android")]
fn main() {
    let device = MediaCodecDevice::probe();
    println!(
        "mediacodec: device {} available={}",
        device.name(),
        device.available()
    );
    let quirks = match QuirkTable::load() {
        Ok(quirks) => quirks,
        Err(error) => {
            eprintln!("mediacodec: quirk table failed ({error})");
            std::process::exit(1);
        }
    };
    println!("mediacodec: {} quirk entries", quirks.len());
    let Some(spec) = EncodeSpec::h264(64, 64, 1_000_000, 30, 1) else {
        println!("ERROR: h264 spec rejected");
        std::process::exit(1);
    };
    let mut encoder = match MediaCodecEncoder::open(&spec, "generic", &quirks) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    let frame = Synth::black_yuv420p(spec.width(), spec.height());
    match encoder.encode(&frame) {
        Ok(packets) => println!("h264: encode -> {} packet(s)", packets.len()),
        Err(error) => {
            println!("ERROR: encode ({error})");
            std::process::exit(1);
        }
    }
    match encoder.flush() {
        Ok(packets) => println!("h264: flush -> {} packet(s)", packets.len()),
        Err(error) => {
            println!("ERROR: flush ({error})");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "android"))]
fn main() {}
