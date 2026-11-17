//! H.264 decode probe on the `MediaCodec` tier.

#[cfg(target_os = "android")]
use bytes::Bytes;

#[cfg(target_os = "android")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "android")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "android")]
use rust_video_mediacodec::{FormatSpec, MediaCodecDecoder, MediaCodecDevice, OutputLayout};

#[cfg(target_os = "android")]
fn main() {
    let device = MediaCodecDevice::probe();
    println!(
        "mediacodec: device {} available={}",
        device.name(),
        device.available()
    );
    let Some(spec) = FormatSpec::h264(64, 64) else {
        println!("ERROR: h264 spec rejected");
        std::process::exit(1);
    };
    let tentative = OutputLayout::tentative(&spec).is_some_and(|layout| !layout.negotiated());
    println!(
        "h264: mime={} min_sdk={} tentative={tentative}",
        spec.mime(),
        spec.min_sdk()
    );
    let mut decoder = match MediaCodecDecoder::open(&spec) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("SKIP no h264 decoder ({error})");
            return;
        }
    };
    println!("h264: opened state={:?}", decoder.state());
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("h264: empty packet -> {} frames", frames.len()),
        Err(error) => {
            println!("ERROR: decode empty packet ({error})");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "android"))]
fn main() {
    println!("SKIP: mediacodec needs android");
}
