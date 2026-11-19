//! VP9 encode on the software tier, from a synthetic frame to packets.

#[cfg(feature = "ffmpeg")]
use rust_video_caps::packet::Packet;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::traits::Encoder;
#[cfg(feature = "ffmpeg")]
use rust_video_harness::Synth;
#[cfg(feature = "ffmpeg")]
use rust_video_sw::Vp9Encoder;

#[cfg(feature = "ffmpeg")]
fn main() {
    let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder init");
    let frame = Synth::black_yuv420p(64, 64);
    let mut packets_out = 0;
    let mut bytes_out = 0;
    for _ in 0..2 {
        match encoder.encode(&frame) {
            Ok(packets) => tally(&packets, &mut packets_out, &mut bytes_out),
            Err(error) => {
                eprintln!("sw vp9 encode: frame failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match encoder.flush() {
        Ok(packets) => tally(&packets, &mut packets_out, &mut bytes_out),
        Err(error) => {
            eprintln!("sw vp9 encode: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("sw vp9 encode: {packets_out} packets, {bytes_out} bytes");
}

/// Packets and payload bytes tallied into the counters.
#[cfg(feature = "ffmpeg")]
fn tally(packets: &[Packet], packets_out: &mut usize, bytes_out: &mut usize) {
    for packet in packets {
        *bytes_out += packet.data().len();
        *packets_out += 1;
    }
}

#[cfg(not(feature = "ffmpeg"))]
fn main() {
    println!("SKIP: encode_vp9_sw needs --features ffmpeg");
}
