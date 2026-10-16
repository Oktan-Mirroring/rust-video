//! VA-API AV1 decode: staged frontend, typed decline.

#[cfg(target_os = "linux")]
use bytes::Bytes;

#[cfg(target_os = "linux")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "linux")]
use rust_video_caps::traits::Decoder;
#[cfg(target_os = "linux")]
use rust_video_vaapi::Av1Decoder;

#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
fn main() {}
