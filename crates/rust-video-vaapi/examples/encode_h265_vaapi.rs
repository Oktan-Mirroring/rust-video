//! VA-API H.265 encode: synthetic frames in, packets out.
//!
//! `encode_h265_vaapi [width height count]`; defaults `320 240 30`.

#[cfg(target_os = "linux")]
fn main() {
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::encode::{EncodeConfig, Level, Profile, RateControl};
    use rust_video_caps::traits::{Device, Encoder};
    use rust_video_harness::{FrameHash, Synth};
    use rust_video_vaapi::{EncodeCodec, VaApiDevice, VaApiEncoder, VaApiError};

    let mut args = std::env::args().skip(1);
    let width: u32 = args
        .next()
        .map_or(Ok(320), |arg| arg.parse())
        .unwrap_or_else(|_| {
            eprintln!("usage: encode_h265_vaapi [width height count]");
            std::process::exit(2);
        });
    let height: u32 = args
        .next()
        .map_or(Ok(240), |arg| arg.parse())
        .unwrap_or_else(|_| {
            eprintln!("usage: encode_h265_vaapi [width height count]");
            std::process::exit(2);
        });
    let count: u32 = args
        .next()
        .map_or(Ok(30), |arg| arg.parse())
        .unwrap_or_else(|_| {
            eprintln!("usage: encode_h265_vaapi [width height count]");
            std::process::exit(2);
        });
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("SKIP va-api h265 encode ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api h265 encode: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api h265 encode: device {}", device.name());
    let Some(size) = PictureSize::new(width, height) else {
        eprintln!("va-api h265 encode: zero extent {width}x{height}");
        std::process::exit(2);
    };
    let config = EncodeConfig {
        rate_control: RateControl::Auto,
        profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..EncodeConfig::default()
    };
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H265, size, &config) {
        Ok(encoder) => encoder,
        Err(VaApiError::Unsupported(reason)) => {
            println!("SKIP va-api h265 encode ({reason})");
            return;
        }
        Err(VaApiError::Declined(reason)) => {
            println!("SKIP va-api h265 encode ({reason})");
            return;
        }
        Err(VaApiError::NoPackedHeaders { .. } | VaApiError::NoNv12ImageFormat) => {
            println!("SKIP va-api h265 encode (driver gap)");
            return;
        }
        Err(error) => {
            eprintln!("va-api h265 encode: session failed ({error})");
            std::process::exit(1);
        }
    };
    let mut packets_out = 0;
    let mut bytes_out = 0;
    for index in 0..count {
        let frame = Synth::gradient(size, index);
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
                eprintln!("va-api h265 encode: frame {index} failed ({error})");
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
            eprintln!("va-api h265 encode: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api h265 encode: {packets_out} packets, {bytes_out} bytes");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("SKIP: vaapi needs linux");
}
