//! VA-API AV1 decode: staged frontend, typed decline.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vaapi::Av1Decoder;

fn main() {
    let mut decoder = Av1Decoder;
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("va-api av1: empty packet -> {} frames", frames.len()),
        Err(error) => println!("va-api av1: empty packet rejected ({error})"),
    }
    match decoder.flush() {
        Ok(frames) => println!("va-api av1: flush -> {} frames", frames.len()),
        Err(error) => println!("va-api av1: flush rejected ({error})"),
    }
}
