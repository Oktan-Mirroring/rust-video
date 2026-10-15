//! Software decode matrix over the ffmpeg native decoders.

#[cfg(feature = "ffmpeg")]
fn main() {
    use bytes::Bytes;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;
    use rust_video_sw::{
        Av1Decoder, Codec, FfmpegDecoder, H264Decoder, H265Decoder, Vp8Decoder, Vp9Decoder,
    };

    fn run<D: Decoder>(name: &str, mut decoder: D)
    where
        D::Error: std::fmt::Display,
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

    for (name, codec) in [
        ("h264", Codec::H264),
        ("h265", Codec::H265),
        ("vp8", Codec::Vp8),
        ("vp9", Codec::Vp9),
        ("av1", Codec::Av1),
    ] {
        match FfmpegDecoder::open(codec, 1) {
            Ok(decoder) => run(name, decoder),
            Err(error) => println!("{name}: open failed ({error})"),
        }
    }
    for (name, opened) in [
        ("h264-typed", H264Decoder::open(1).map(|_| ())),
        ("h265-typed", H265Decoder::open(1).map(|_| ())),
        ("vp8-typed", Vp8Decoder::open(1).map(|_| ())),
        ("vp9-typed", Vp9Decoder::open(1).map(|_| ())),
        ("av1-typed", Av1Decoder::open(1).map(|_| ())),
    ] {
        match opened {
            Ok(()) => println!("{name}: open ok"),
            Err(error) => println!("{name}: open failed ({error})"),
        }
    }
}

#[cfg(not(feature = "ffmpeg"))]
fn main() {
    println!("sw decode matrix needs --features ffmpeg; skipped");
}
