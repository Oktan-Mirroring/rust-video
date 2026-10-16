#![cfg(target_os = "windows")]

//! GPU transcode throughput: 1080p hops against the 60 fps bar.
//!
//! Skips without hardware. Prints measured fps; fails below the bar only
//! with `RUST_VIDEO_BENCH_STRICT=1`.

#[path = "../examples/common/bits.rs"]
mod bits;
#[path = "../examples/common/h264.rs"]
mod h264;

use h264::{
    annex_b_h264_packets, annex_b_h264_packets_cropped, annex_b_h264_packets_cropped_sized,
};

use rust_video_d3d12::{Codec, D3d12Device, Transcoder};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::transcode::Route;

/// Hops timed after warmup.
const HOPS: u32 = 30;

/// Tier1 realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Packets over `raws`, built once outside the timed loop.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// Measured fps of `HOPS` 1080p hops through `transcoder`.
fn fps(transcoder: &mut Transcoder, warmup: &[Vec<u8>], raws: &[Vec<u8>]) -> f64 {
    let warmup = packets(warmup);
    let raws = packets(raws);
    for packet in &warmup {
        let outcome = transcoder.transcode(packet).expect("warmup transcode");
        assert!(matches!(outcome.plan().route(), Route::Gpu));
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            let outcome = transcoder.transcode(packet).expect("timed transcode");
            assert!(matches!(outcome.plan().route(), Route::Gpu));
            bytes += outcome
                .packets()
                .iter()
                .map(|packet| packet.data().len())
                .sum::<usize>();
        }
    }
    let outcome = transcoder.flush().expect("timed flush");
    assert!(matches!(outcome.plan().route(), Route::Gpu));
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    f64::from(HOPS) / elapsed
}

/// 1080p GPU hops measured against the realtime bar.
#[test]
fn xcode_1080p_measures_realtime() {
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d12 skipped: {error}");
            return;
        }
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("d3d12 skipped: {error}");
            return;
        }
    };
    let warmup: Vec<Vec<u8>> = annex_b_h264_packets()
        .into_iter()
        .chain(annex_b_h264_packets_cropped())
        .collect();
    let raws = annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = fps(&mut transcoder, &warmup, &raws);
    println!("xcode 1080p: {measured:.1}fps over {HOPS} hops, bar {BAR:.0}fps");
    if std::env::var("RUST_VIDEO_BENCH_STRICT").is_ok_and(|value| value == "1") {
        assert!(
            measured >= BAR,
            "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
        );
    }
}
