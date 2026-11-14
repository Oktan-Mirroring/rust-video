//! AV1 decode probe on the `MediaCodec` tier.

#[cfg(target_os = "android")]
use bytes::Bytes;

#[cfg(target_os = "android")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "android")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "android")]
use rust_video_mediacodec::{
    FormatSpec, MediaCodecDecoder, MediaCodecDevice, OutputLayout, QuirkTable,
};

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
    let Some(spec) = FormatSpec::av1(64, 64) else {
        println!("ERROR: av1 spec rejected");
        std::process::exit(1);
    };
    let tentative = OutputLayout::tentative(&spec).is_some_and(|layout| !layout.negotiated());
    println!(
        "av1: mime={} min_sdk={} tentative={tentative}",
        spec.mime(),
        spec.min_sdk()
    );
    let mut decoder = match MediaCodecDecoder::open(&spec, "generic", &quirks) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("SKIP: no av1 decoder ({error})");
            return;
        }
    };
    println!(
        "av1: opened state={:?} quirks={:?}",
        decoder.state(),
        decoder.applied_quirks()
    );
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("av1: empty packet -> {} frames", frames.len()),
        Err(error) => {
            println!("ERROR: decode empty packet ({error})");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "android"))]
fn main() {}
