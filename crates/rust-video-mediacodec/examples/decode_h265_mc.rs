//! H.265 decode probe on the `MediaCodec` tier.

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
    let Some(spec) = FormatSpec::h265(64, 64) else {
        println!("ERROR: h265 spec rejected");
        std::process::exit(1);
    };
    let tentative = OutputLayout::tentative(&spec).is_some_and(|layout| !layout.negotiated());
    println!(
        "h265: mime={} min_sdk={} tentative={tentative}",
        spec.mime(),
        spec.min_sdk()
    );
    let mut decoder = match MediaCodecDecoder::open(&spec) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("SKIP no h265 decoder ({error})");
            return;
        }
    };
    println!("h265: opened state={:?}", decoder.state());
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("h265: empty packet -> {} frames", frames.len()),
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
