//! Software encode matrix over the shipped CPU encoders.

#[cfg(not(any(feature = "ffmpeg", feature = "gpl", feature = "h264-openh264")))]
fn main() {
    println!("sw encode matrix needs an encoder feature; skipped");
}

#[cfg(any(feature = "ffmpeg", feature = "gpl", feature = "h264-openh264"))]
fn main() {
    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::traits::Encoder;

    fn gray_frame() -> CpuFrame {
        let y = Bytes::from(vec![0x80; 64 * 64]);
        let c = Bytes::from(vec![0x80; 32 * 32]);
        let planes = vec![
            Plane::new(y, 64).expect("luma plane"),
            Plane::new(c.clone(), 32).expect("chroma plane"),
            Plane::new(c, 32).expect("chroma plane"),
        ];
        let size = PictureSize::new(64, 64).expect("nonzero extent");
        CpuFrame::new(size, PixelFormat::Yuv420P, planes).expect("yuv420p frame")
    }

    fn run<E: Encoder>(name: &str, opened: Result<E, E::Error>)
    where
        E::Error: std::fmt::Display,
    {
        let mut encoder = match opened {
            Ok(encoder) => encoder,
            Err(error) => {
                println!("{name}: open failed ({error})");
                return;
            }
        };
        let frame = gray_frame();
        let mut packets = match encoder.encode(&frame) {
            Ok(packets) => packets,
            Err(error) => {
                println!("{name}: encode rejected ({error})");
                return;
            }
        };
        match encoder.encode(&frame) {
            Ok(more) => packets.extend(more),
            Err(error) => println!("{name}: second encode rejected ({error})"),
        }
        match encoder.flush() {
            Ok(tail) => packets.extend(tail),
            Err(error) => println!("{name}: flush rejected ({error})"),
        }
        let bytes: usize = packets.iter().map(|packet| packet.data().len()).sum();
        let keyed = packets.iter().filter(|packet| packet.is_keyframe()).count();
        println!(
            "{name}: {} packets, {bytes} bytes, {keyed} keyframes",
            packets.len()
        );
    }

    #[cfg(feature = "ffmpeg")]
    run("av1", rust_video_sw::Av1Encoder::open(64, 64, 500, 1));
    #[cfg(feature = "ffmpeg")]
    run("vp9", rust_video_sw::Vp9Encoder::open(64, 64, 500, 1));
    #[cfg(feature = "h264-openh264")]
    run("h264", rust_video_sw::H264Encoder::open(64, 64, 500_000, 1));
    #[cfg(feature = "gpl")]
    run("h265", rust_video_sw::H265Encoder::open(64, 64, 500, 1));
}
