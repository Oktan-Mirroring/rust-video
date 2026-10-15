//! `MediaCodec` decode matrix over the five shipped codecs.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_mediacodec::{
    FormatSpec, MediaCodecDecoder, MediaCodecDevice, OutputLayout, QuirkTable,
};

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
        match MediaCodecDecoder::open(&spec, "generic", &quirks) {
            Ok(mut decoder) => {
                println!(
                    "{name}: opened state={:?} quirks={:?}",
                    decoder.state(),
                    decoder.applied_quirks()
                );
                let empty = Packet::new(Bytes::new());
                match decoder.decode(&empty) {
                    Ok(frames) => println!("{name}: empty packet -> {} frames", frames.len()),
                    Err(error) => println!("{name}: empty packet rejected ({error})"),
                }
            }
            Err(error) => println!("{name}: skipped ({error})"),
        }
    }
}
