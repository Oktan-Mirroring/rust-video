//! VA-API H.265 decode: staged frontend, typed decline.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vaapi::H265Decoder;

fn main() {
    let mut decoder = H265Decoder;
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("va-api h265: empty packet -> {} frames", frames.len()),
        Err(error) => println!("va-api h265: empty packet rejected ({error})"),
    }
    match decoder.flush() {
        Ok(frames) => println!("va-api h265: flush -> {} frames", frames.len()),
        Err(error) => println!("va-api h265: flush rejected ({error})"),
    }
}
