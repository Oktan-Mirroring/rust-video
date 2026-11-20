//! AV1 encode probe on the `MediaCodec` tier.

#[cfg(target_os = "android")]
use rust_video_caps::encode::{EncodeConfig, Level, Profile};
#[cfg(target_os = "android")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "android")]
use rust_video_harness::Synth;
#[cfg(target_os = "android")]
use rust_video_mediacodec::{EncodeSpec, MediaCodecDevice, MediaCodecEncoder};

#[cfg(target_os = "android")]
fn main() {
    let device = MediaCodecDevice::probe();
    println!(
        "mediacodec: device {} available={}",
        device.name(),
        device.available()
    );
    let Some(spec) = EncodeSpec::av1(64, 64, 1_000_000, 30, 1) else {
        println!("ERROR: av1 spec rejected");
        std::process::exit(1);
    };
    let spec = spec.with_config(EncodeConfig {
        profile: Profile::Av1(rust_video_caps::av1::EncodeProfile::Main),
        level: Level::Av1(rust_video_caps::av1::EncodeLevel::Auto),
        ..Default::default()
    });
    let mut encoder = match MediaCodecEncoder::open(&spec) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP no av1 encoder ({error})");
            return;
        }
    };
    let frame = Synth::black_yuv420p(spec.width(), spec.height());
    match encoder.encode(&frame) {
        Ok(packets) => println!("av1: encode -> {} packet(s)", packets.len()),
        Err(error) => {
            println!("ERROR: encode ({error})");
            std::process::exit(1);
        }
    }
    match encoder.flush() {
        Ok(packets) => println!("av1: flush -> {} packet(s)", packets.len()),
        Err(error) => {
            println!("ERROR: flush ({error})");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "android"))]
fn main() {
    println!("SKIP: mediacodec needs android");
}
