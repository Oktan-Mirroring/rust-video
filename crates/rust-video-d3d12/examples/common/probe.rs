//! Single-packet decode probe for pending-builder tiers.

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_d3d12::{Codec, D3d12Device, D3d12Error};

/// Open device, absent with a loud skip.
pub(crate) fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => {
            println!("device: {}", device.name());
            Some(device)
        }
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            None
        }
    }
}

/// Profile check plus one pending-builder packet for `codec`.
///
/// Decodes `bytes` through `decoder`, then flushes.
pub(crate) fn run<D>(device: &D3d12Device, codec: Codec, label: &str, bytes: &[u8], mut decoder: D)
where
    D: Decoder<Error = D3d12Error>,
{
    println!("supports {label}: {}", device.supports(codec));
    let packet = Packet::new(Bytes::copy_from_slice(bytes));
    match decoder.decode(&packet) {
        Ok(frames) => {
            if frames.is_empty() {
                println!("decoded 0 frame(s)");
            } else {
                println!("ERROR: {label} pending builder emitted pixels");
                std::process::exit(1);
            }
        }
        Err(D3d12Error::Skipped(reason)) => println!("SKIP: {reason}"),
        Err(D3d12Error::Unsupported(error)) => println!("pending builder: {error}"),
        Err(error) => {
            println!("ERROR: {label} probe: {error}");
            std::process::exit(1);
        }
    }
    match decoder.flush() {
        Ok(frames) => {
            if frames.is_empty() {
                println!("flush: 0 frame(s)");
            } else {
                println!("ERROR: {label} pending flush emitted pixels");
                std::process::exit(1);
            }
        }
        Err(error) => {
            println!("ERROR: {label} flush: {error}");
            std::process::exit(1);
        }
    }
}
