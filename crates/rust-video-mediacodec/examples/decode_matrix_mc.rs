//! `MediaCodec` decode matrix over the five shipped codecs.

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
    for (name, spec) in [
        ("h264", FormatSpec::h264(64, 64)),
        ("h265", FormatSpec::h265(64, 64)),
        ("av1", FormatSpec::av1(64, 64)),
        ("vp9", FormatSpec::vp9(64, 64)),
        ("vp8", FormatSpec::vp8(64, 64)),
    ] {
        let Some(spec) = spec else {
            println!("{name}: spec rejected");
            continue;
        };
        let tentative = OutputLayout::tentative(&spec).is_some_and(|layout| !layout.negotiated());
        println!(
            "{name}: mime={} min_sdk={} tentative={tentative}",
            spec.mime(),
            spec.min_sdk()
        );
        match MediaCodecDecoder::open(&spec) {
            Ok(mut decoder) => {
                println!("{name}: opened state={:?}", decoder.state());
                let empty = Packet::new(Bytes::new());
                match decoder.decode(&empty) {
                    Ok(frames) => println!("{name}: empty packet -> {} frames", frames.len()),
                    Err(error) => println!("{name}: empty packet rejected ({error})"),
                }
            }
            Err(error) => println!("SKIP {name} ({error})"),
        }
    }
}

#[cfg(not(target_os = "android"))]
fn main() {
    println!("SKIP: mediacodec needs android");
}
