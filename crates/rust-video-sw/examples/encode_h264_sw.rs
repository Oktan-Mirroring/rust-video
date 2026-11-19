//! H.264 encode on the software tier, from a synthetic frame to packets.

#[cfg(feature = "h264-openh264")]
use rust_video_caps::packet::Packet;
#[cfg(feature = "h264-openh264")]
use rust_video_caps::traits::Encoder;
#[cfg(feature = "h264-openh264")]
use rust_video_harness::Synth;
#[cfg(feature = "h264-openh264")]
use rust_video_sw::H264Encoder;

#[cfg(feature = "h264-openh264")]
fn main() {
    let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder init");
    let frame = Synth::black_yuv420p(64, 64);
    let mut packets_out = 0;
    let mut bytes_out = 0;
    for _ in 0..2 {
        match encoder.encode(&frame) {
            Ok(packets) => tally(&packets, &mut packets_out, &mut bytes_out),
            Err(error) => {
                eprintln!("sw h264 encode: frame failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match encoder.flush() {
        Ok(packets) => tally(&packets, &mut packets_out, &mut bytes_out),
        Err(error) => {
            eprintln!("sw h264 encode: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("sw h264 encode: {packets_out} packets, {bytes_out} bytes");
}

/// Packets and payload bytes tallied into the counters.
#[cfg(feature = "h264-openh264")]
fn tally(packets: &[Packet], packets_out: &mut usize, bytes_out: &mut usize) {
    for packet in packets {
        *bytes_out += packet.data().len();
        *packets_out += 1;
    }
}

#[cfg(not(feature = "h264-openh264"))]
fn main() {
    println!("SKIP: encode_h264_sw needs --features h264-openh264");
}
