//! Vulkan throughput: 1080p decode plus encode against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

use std::sync::Mutex;

use rust_video_vulkan::{
    Av1Decoder, CodecOp, H264Decoder, H264Encoder, H265Decoder, H265Encoder, ParseError,
    VulkanDevice, VulkanError, VulkanFrame,
};

use bytes::Bytes;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_harness::{bench_dir, AnnexB, H264Synth, Synth};

// Parallel VkDevice teardown deadlocks in driver join; guard serializes it.
static SERIAL: Mutex<()> = Mutex::new(());

/// Iters timed after warmup.
const ITERS: u32 = 30;

/// Tier realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Device for `op`, skipping loud when the tier declines.
fn device_or_skip(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open(op) {
        Ok(Some(device)) => Some(device),
        Ok(None) => {
            println!("vulkan skipped: {op:?} declined on this box");
            None
        }
        Err(error) => {
            println!("vulkan skipped: {op:?} failed: {error}");
            None
        }
    }
}

/// Packets over `raws`, built once outside the timed loop.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// True when `error` is the tier declining the vector's PTL profile.
fn profile_gap(error: &VulkanError) -> bool {
    matches!(
        error,
        VulkanError::Parse(ParseError::OutOfRange {
            syntax: "ptl profile",
        })
    )
}

/// Measured fps of `ITERS` 1080p passes of `raws` through `decoder`.
fn decode_fps(
    decoder: &mut impl Decoder<Frame = VulkanFrame, Error = VulkanError>,
    warmup: &[Vec<u8>],
    raws: &[Vec<u8>],
) -> Result<f64, VulkanError> {
    let warmup = packets(warmup);
    let raws = packets(raws);
    for packet in &warmup {
        decoder.decode(packet)?;
    }
    decoder.flush()?;
    let mut frames = 0;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for packet in &raws {
            frames += decoder.decode(packet)?.len();
        }
    }
    frames += decoder.flush()?.len();
    let elapsed = start.elapsed().as_secs_f64();
    assert!(frames > 0);
    Ok(f64::from(ITERS) / elapsed)
}

/// Surface decode behind `decode_surface` plus `flush_surface`.
trait SurfaceDecoder {
    /// Frames decoded from `packet` with device exports attached.
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError>;

    /// Surface frames held back for reordering or buffering.
    fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError>;
}

impl SurfaceDecoder for H264Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        H264Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        H264Decoder::flush_surface(self)
    }
}

impl SurfaceDecoder for H265Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        H265Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        H265Decoder::flush_surface(self)
    }
}

impl SurfaceDecoder for Av1Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        Av1Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        Av1Decoder::flush_surface(self)
    }
}

/// True when `error` is the tier declining exportable planes.
fn export_gap(error: &VulkanError) -> bool {
    matches!(error, VulkanError::Unsupported { .. })
}

/// Measured fps of `ITERS` 1080p surface passes of `raws` through `decoder`.
fn decode_surface_fps(
    decoder: &mut impl SurfaceDecoder,
    warmup: &[Vec<u8>],
    raws: &[Vec<u8>],
) -> Result<f64, VulkanError> {
    let warmup = packets(warmup);
    let raws = packets(raws);
    for packet in &warmup {
        decoder.decode_surface(packet)?;
    }
    decoder.flush_surface()?;
    let mut frames = 0;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for packet in &raws {
            frames += decoder.decode_surface(packet)?.len();
        }
    }
    frames += decoder.flush_surface()?.len();
    let elapsed = start.elapsed().as_secs_f64();
    assert!(frames > 0);
    Ok(f64::from(ITERS) / elapsed)
}

/// Measured fps of `ITERS` 1080p encodes cycling `frames` through `encoder`.
fn encode_fps(encoder: &mut impl Encoder<Error = VulkanError>, frames: &[CpuFrame]) -> f64 {
    for frame in frames {
        encoder.encode(frame).expect("warmup encode");
    }
    encoder.flush().expect("warmup drain");
    let mut bytes = 0;
    let start = std::time::Instant::now();
    let count = usize::try_from(ITERS).unwrap_or(0);
    for frame in frames.iter().cycle().take(count) {
        bytes += encoder
            .encode(frame)
            .expect("timed encode")
            .iter()
            .map(|packet| packet.data().len())
            .sum::<usize>();
    }
    bytes += encoder
        .flush()
        .expect("timed drain")
        .iter()
        .map(|packet| packet.data().len())
        .sum::<usize>();
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    f64::from(ITERS) / elapsed
}

/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn decode_h264_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    let mut decoder = match H264Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("vulkan skipped: h264 decode open failed: {error}");
            return;
        }
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = decode_fps(&mut decoder, &raws, &raws).expect("bench runs");
    println!("vulkan h264 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.265 decode measured against the realtime bar.
#[test]
fn decode_h265_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::H265Decode) else {
        return;
    };
    let mut decoder = match H265Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("vulkan skipped: h265 decode open failed: {error}");
            return;
        }
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let measured = match decode_fps(&mut decoder, &raws, &raws) {
        Ok(measured) => measured,
        Err(error) if profile_gap(&error) => {
            println!("vulkan skipped: h265 bench vector declares ptl profile 4, tier parses 1-3");
            return;
        }
        Err(error) => panic!("bench runs: {error}"),
    };
    println!("vulkan h265 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn decode_av1_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::Av1Decode) else {
        return;
    };
    let mut decoder = match Av1Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("vulkan skipped: av1 decode open failed: {error}");
            return;
        }
    };
    let raws = vec![std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable")];
    let measured = decode_fps(&mut decoder, &raws, &raws).expect("bench runs");
    println!("vulkan av1 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 surface decode measured against the realtime bar.
#[test]
fn surface_h264_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    let mut decoder = match H264Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("vulkan skipped: h264 decode open failed: {error}");
            return;
        }
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = match decode_surface_fps(&mut decoder, &raws, &raws) {
        Ok(measured) => measured,
        Err(error) if export_gap(&error) => {
            println!("vulkan skipped: h264 surface bench without exportable planes");
            return;
        }
        Err(error) => panic!("bench runs: {error}"),
    };
    println!("vulkan h264 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.265 surface decode measured against the realtime bar.
#[test]
fn surface_h265_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::H265Decode) else {
        return;
    };
    let mut decoder = match H265Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("vulkan skipped: h265 decode open failed: {error}");
            return;
        }
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let measured = match decode_surface_fps(&mut decoder, &raws, &raws) {
        Ok(measured) => measured,
        Err(error) if export_gap(&error) => {
            println!("vulkan skipped: h265 surface bench without exportable planes");
            return;
        }
        Err(error) if profile_gap(&error) => {
            println!("vulkan skipped: h265 bench vector declares ptl profile 4, tier parses 1-3");
            return;
        }
        Err(error) => panic!("bench runs: {error}"),
    };
    println!("vulkan h265 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 surface decode measured against the realtime bar.
#[test]
fn surface_av1_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::Av1Decode) else {
        return;
    };
    let mut decoder = match Av1Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("vulkan skipped: av1 decode open failed: {error}");
            return;
        }
    };
    let raws = vec![std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable")];
    let measured = match decode_surface_fps(&mut decoder, &raws, &raws) {
        Ok(measured) => measured,
        Err(error) if export_gap(&error) => {
            println!("vulkan skipped: av1 surface bench without exportable planes");
            return;
        }
        Err(error) => panic!("bench runs: {error}"),
    };
    println!("vulkan av1 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 encode measured against the realtime bar.
#[test]
fn encode_h264_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::H264Encode) else {
        return;
    };
    let mut encoder = match H264Encoder::open(&device, 1920, 1080) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("vulkan skipped: h264 encode open failed: {error}");
            return;
        }
    };
    let frames: Vec<CpuFrame> = (0..4).map(|seed| Synth::noise(1920, 1080, seed)).collect();
    let measured = encode_fps(&mut encoder, &frames);
    println!("vulkan h264 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.265 encode measured against the realtime bar.
#[test]
fn encode_h265_1080p_measures_realtime() {
    let _guard = SERIAL.lock().expect("bench lock");
    let Some(device) = device_or_skip(CodecOp::H265Encode) else {
        return;
    };
    let mut encoder = match H265Encoder::open(&device, 1920, 1080) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("vulkan skipped: h265 encode open failed: {error}");
            return;
        }
    };
    let frames: Vec<CpuFrame> = (0..4).map(|seed| Synth::noise(1920, 1080, seed)).collect();
    let measured = encode_fps(&mut encoder, &frames);
    println!("vulkan h265 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}
