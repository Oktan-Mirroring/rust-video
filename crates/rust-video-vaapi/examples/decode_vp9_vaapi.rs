//! VA-API VP9 decode: staged frontend, typed decline.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vaapi::Vp9Decoder;

fn main() {
    let mut decoder = Vp9Decoder;
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("va-api vp9: empty packet -> {} frames", frames.len()),
        Err(error) => println!("va-api vp9: empty packet rejected ({error})"),
    }
    match decoder.flush() {
        Ok(frames) => println!("va-api vp9: flush -> {} frames", frames.len()),
        Err(error) => println!("va-api vp9: flush rejected ({error})"),
    }
}
