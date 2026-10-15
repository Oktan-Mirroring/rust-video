//! Full-GPU transcode on the D3D12 tier: decode texture to encoder texture.
//!
//! Pixels stay device-resident on the GPU route; any hop miss falls back
//! loud to the proven CPU path, never silently.

#[path = "common/bits.rs"]
mod bits;
#[path = "common/h264.rs"]
mod h264;

use h264::annex_b_h264_packets;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Device;
use rust_video_caps::transcode::{Outcome, Route};
use rust_video_d3d12::{Codec, D3d12Device, D3d12Error, Transcoder};

fn main() {
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("SKIP: no h264 transcode ({error})");
            return;
        }
    };
    let caps = transcoder.caps();
    println!(
        "caps: platform={} decode_export={} encode_import={} ready={}",
        caps.platform(),
        caps.decode_export(),
        caps.encode_import(),
        caps.ready()
    );
    let mut packets = 0;
    let mut bytes = 0;
    for (index, raw) in annex_b_h264_packets().into_iter().enumerate() {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode(&packet) {
            Ok(outcome) => {
                report(index, &outcome);
                packets += outcome.packets().len();
                bytes += outcome
                    .packets()
                    .iter()
                    .map(|packet| packet.data().len())
                    .sum::<usize>();
            }
            Err(error) => {
                println!("ERROR: transcode packet {index}: {error}");
                std::process::exit(1);
            }
        }
    }
    match transcoder.flush() {
        Ok(outcome) => {
            println!(
                "flush: {} packet(s) route={:?}",
                outcome.packets().len(),
                outcome.plan().route()
            );
            packets += outcome.packets().len();
            bytes += outcome
                .packets()
                .iter()
                .map(|packet| packet.data().len())
                .sum::<usize>();
        }
        Err(error) => {
            println!("ERROR: flush: {error}");
            std::process::exit(1);
        }
    }
    if packets == 0 || bytes == 0 {
        println!("ERROR: transcode produced no bitstream");
        std::process::exit(1);
    }
    println!("transcoded {packets} packet(s), {bytes} byte(s)");
    println!("parity: size-proven only; pixel content unverified vs oracle");
    probe_scale_miss(&device);
}

/// One transcode outcome on stdout.
fn report(index: usize, outcome: &Outcome) {
    let plan = outcome.plan();
    match plan.route() {
        Route::Gpu => println!(
            "packet {index}: {} packet(s) route=gpu",
            outcome.packets().len()
        ),
        Route::Cpu => println!(
            "packet {index}: {} packet(s) FALLBACK route=cpu cause={}",
            outcome.packets().len(),
            plan.cause()
                .map_or("unknown".to_owned(), |cause| cause.to_string()),
        ),
    }
}

/// Scale request against the GPU-only scale hop; the miss stays loud.
fn probe_scale_miss(device: &D3d12Device) {
    let target = PictureSize::new(160, 120).expect("nonzero extent");
    let mut transcoder = match Transcoder::open(device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder.with_target(target),
        Err(error) => {
            println!("SKIP: scale probe without transcode ({error})");
            return;
        }
    };
    let raw = annex_b_h264_packets()
        .into_iter()
        .next()
        .expect("one packet");
    let packet = Packet::new(Bytes::from(raw));
    match transcoder.transcode(&packet) {
        Ok(outcome) => {
            let bytes: usize = outcome
                .packets()
                .iter()
                .map(|packet| packet.data().len())
                .sum();
            if outcome.packets().is_empty() || bytes == 0 {
                println!("ERROR: scale probe produced no bitstream");
                std::process::exit(1);
            }
            println!(
                "scale: to 160x120 bitstream {bytes}B route={:?}",
                outcome.plan().route()
            );
        }
        Err(D3d12Error::Unsupported(error)) if error.contains("scale") => {
            println!("MISS: {error}");
        }
        Err(error) => {
            println!("ERROR: scale probe: {error}");
            std::process::exit(1);
        }
    }
}
