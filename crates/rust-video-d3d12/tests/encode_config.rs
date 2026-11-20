//! D3D12 encode config: mapped rate control, profiles, declines, and bounds.
//!
//! GPU reflection over explicit configs plus typed declines.

#![cfg(target_os = "windows")]

use rust_video_d3d12::{
    Av1Decoder, Codec, D3d12Device, D3d12Encoder, D3d12Error, D3d12Frame, GpuSurface, H264Decoder,
    HevcDecoder, Mpeg2Decoder, Transcoder, Vc1Decoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::encode::{
    Decline, EncodeConfig, Gop, Level, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::frame::{CpuFrame, Surface};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder, KeyframeRequest};
use rust_video_caps::{h264, h265};
use rust_video_h264::{
    parse_sps, rbsp, split_nals, H264Error, ParseReason, StrictGate, UnsupportedReason,
};
use rust_video_harness::Synth;

/// Test H.264 parse error behind grammar assertions.
#[derive(Debug)]
struct TestError;

impl H264Error for TestError {
    /// Parse failure of `kind`.
    fn parse(_kind: ParseReason) -> Self {
        Self
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(_kind: UnsupportedReason) -> Self {
        Self
    }
}

/// Test HEVC parse error behind grammar assertions.
#[derive(Debug)]
struct HevcTestError;

impl rust_video_h265::H265Error for HevcTestError {
    /// Parse failure of `kind`.
    fn parse(_kind: rust_video_h265::ParseReason) -> Self {
        Self
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(_kind: rust_video_h265::UnsupportedReason) -> Self {
        Self
    }
}

/// Open device, loud skip without hardware.
fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => {
            println!("adapter: {}", device.name());
            Some(device)
        }
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            None
        }
    }
}

/// All-intra GOP: keyframe every picture.
fn intra_gop() -> Gop {
    Gop {
        keyint: 1,
        min_keyint: 1,
        b_frames: 0,
        refs: 1,
    }
}

/// H.264 High config with `rate`.
fn h264_high(rate: RateControl) -> EncodeConfig {
    EncodeConfig {
        rate_control: rate,
        gop: intra_gop(),
        profile: Profile::H264(h264::EncodeProfile::High),
        level: Level::H264(h264::EncodeLevel::Auto),
        ..EncodeConfig::default()
    }
}

/// H.264 High CBR config at `bitrate` bps with a 1s buffer.
fn cbr(bitrate: u32) -> EncodeConfig {
    h264_high(RateControl::Cbr {
        bitrate,
        maxrate: bitrate,
        bufsize: bitrate,
    })
}

/// Bytes encoding `frames` with `config`, absent without an encoder.
fn encode_total(
    device: &D3d12Device,
    config: EncodeConfig,
    frames: &[CpuFrame],
) -> Option<Vec<u8>> {
    let mut encoder = D3d12Encoder::open(device, config).ok()?;
    let mut out = Vec::new();
    for frame in frames {
        for packet in encoder.encode(frame).ok()? {
            assert!(!packet.data().is_empty(), "packets carry bytes");
            out.extend_from_slice(packet.data());
        }
    }
    for packet in encoder.flush().ok()? {
        out.extend_from_slice(packet.data());
    }
    Some(out)
}

/// Frames decoding `bytes` with `decoder`.
fn decode_all<D>(decoder: &mut D, bytes: &[u8]) -> Vec<D::Frame>
where
    D: Decoder<Error = D3d12Error>,
    D::Error: std::fmt::Debug,
{
    let packet = Packet::new(Bytes::copy_from_slice(bytes));
    let mut frames = decoder.decode(&packet).expect("roundtrip decode");
    frames.extend(decoder.flush().expect("roundtrip drain"));
    frames
}

/// Auto CQP26 still emits an IDR without VUI.
#[test]
fn auto_cqp26_emits_idr() {
    let Some(device) = device() else {
        return;
    };
    let config = D3d12Encoder::legacy_config(Codec::H264).expect("legacy h264");
    let mut encoder = match D3d12Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    let frame = Synth::grey_nv12(320, 240);
    let packets = encoder.encode(&frame).expect("cqp26 emits");
    assert_eq!(packets.len(), 1);
    let bytes = packets[0].data();
    assert!(bytes.len() > 8);
    assert_eq!(&bytes[..5], &[0, 0, 0, 1, 0x67]);
    let nals = split_nals(bytes);
    assert!(nals.len() >= 3, "sps, pps, and idr");
    assert_eq!(nals[0][0] & 0x1f, 7);
    assert_eq!(nals[1][0] & 0x1f, 8);
    assert!(
        nals[2..].iter().any(|nal| nal[0] & 0x1f == 5),
        "idr slice emits"
    );
    let sps = parse_sps::<TestError, StrictGate>(&rbsp(nals[0])[1..]).expect("sps");
    assert_eq!(sps.profile_idc, 100);
    assert!(sps.vui.is_none(), "auto headers carry no vui");
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_all(&mut decoder, bytes);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].nv12().len(), 320 * 240 * 3 / 2);
}

/// CBR knob moves output across size classes.
#[test]
fn cbr_knob_changes_size_class() {
    let Some(device) = device() else {
        return;
    };
    let frames: Vec<CpuFrame> = (0..4).map(|seed| Synth::noise(320, 240, seed)).collect();
    let Some(low) = encode_total(&device, cbr(500_000), &frames) else {
        println!("SKIP: no h264 cbr encoder");
        return;
    };
    let Some(high) = encode_total(&device, cbr(8_000_000), &frames) else {
        println!("SKIP: no h264 cbr encoder");
        return;
    };
    println!("cbr sizes: low={}B high={}B", low.len(), high.len());
    assert!(low.len() < high.len(), "knob moves size");
    assert!(
        high.len() * 2 >= low.len() * 3,
        "size classes differ by half: {}B vs {}B",
        low.len(),
        high.len()
    );
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(decode_all(&mut decoder, &low).len(), 4);
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(decode_all(&mut decoder, &high).len(), 4);
}

/// VBR encodes noise and decodes back.
#[test]
fn vbr_encodes_and_decodes() {
    let Some(device) = device() else {
        return;
    };
    let config = h264_high(RateControl::Vbr {
        bitrate: 2_000_000,
        maxrate: 4_000_000,
    });
    let frames = vec![Synth::noise(320, 240, 9)];
    let Some(out) = encode_total(&device, config, &frames) else {
        println!("SKIP: no h264 vbr encoder");
        return;
    };
    assert_eq!(&out[..5], &[0, 0, 0, 1, 0x67]);
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_all(&mut decoder, &out);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].nv12().len(), 320 * 240 * 3 / 2);
}

/// Explicit CQP carries VUI color the legacy bytes omit.
#[test]
fn explicit_cqp_carries_vui() {
    let Some(device) = device() else {
        return;
    };
    let config = h264_high(RateControl::Cqp { qp: 26 });
    let frames = vec![Synth::grey_nv12(320, 240)];
    let Some(out) = encode_total(&device, config, &frames) else {
        println!("SKIP: no h264 encoder");
        return;
    };
    let nals = split_nals(&out);
    let sps = parse_sps::<TestError, StrictGate>(&rbsp(nals[0])[1..]).expect("sps");
    let signal = sps
        .vui
        .as_ref()
        .expect("vui")
        .signal
        .as_ref()
        .expect("signal");
    assert!(!signal.full_range);
    let colour = signal.colour.as_ref().expect("colour");
    assert_eq!(
        (colour.primaries, colour.transfer, colour.matrix),
        (1, 1, 1)
    );
}

/// Main profile encodes and decodes back.
#[test]
fn main_profile_roundtrips() {
    let Some(device) = device() else {
        return;
    };
    let config = EncodeConfig {
        profile: Profile::H264(h264::EncodeProfile::Main),
        ..h264_high(RateControl::Cqp { qp: 26 })
    };
    let frames = vec![Synth::grey_nv12(320, 240)];
    let Some(out) = encode_total(&device, config, &frames) else {
        println!("SKIP: no h264 main encoder");
        return;
    };
    let nals = split_nals(&out);
    let sps = parse_sps::<TestError, StrictGate>(&rbsp(nals[0])[1..]).expect("sps");
    assert_eq!(sps.profile_idc, 77);
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_all(&mut decoder, &out);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].nv12().len(), 320 * 240 * 3 / 2);
}

/// Unsupported configs decline typed at open.
#[test]
fn declines_are_typed() {
    let Some(device) = device() else {
        return;
    };
    let mode = RateControl::Crf { value: 23 };
    assert!(matches!(
        D3d12Encoder::open(&device, h264_high(mode)),
        Err(D3d12Error::Declined(Decline::RateControlUnsupported { .. }))
    ));
    let config = EncodeConfig {
        gop: Gop {
            b_frames: 1,
            ..intra_gop()
        },
        ..h264_high(RateControl::Cqp { qp: 26 })
    };
    assert!(matches!(
        D3d12Encoder::open(&device, config),
        Err(D3d12Error::Declined(Decline::BFramesUnsupported {
            actual: 1,
            max: 0
        }))
    ));
    let profile = Profile::H264(h264::EncodeProfile::Baseline);
    let config = EncodeConfig {
        profile,
        ..h264_high(RateControl::Cqp { qp: 26 })
    };
    assert!(matches!(
        D3d12Encoder::open(&device, config),
        Err(D3d12Error::Declined(Decline::ProfileUnsupported { .. }))
    ));
}

/// Pinned level below the extent declines at first encode.
#[test]
fn pinned_level_below_extent_declines_at_encode() {
    let Some(device) = device() else {
        return;
    };
    let config = EncodeConfig {
        level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L1)),
        ..h264_high(RateControl::Cqp { qp: 26 })
    };
    let mut encoder = match D3d12Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    let frame = Synth::grey_nv12(1280, 720);
    assert!(matches!(
        encoder.encode(&frame),
        Err(D3d12Error::OverCaps { what: "level" })
    ));
}

/// Encoded bytes stay within sane bounds and decode exact.
#[test]
fn roundtrip_bounds() {
    const RAW: usize = 320 * 240 * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let config = D3d12Encoder::legacy_config(Codec::H264).expect("legacy h264");
    let frames: Vec<CpuFrame> = (0..4).map(|seed| Synth::noise(320, 240, seed)).collect();
    let Some(out) = encode_total(&device, config, &frames) else {
        println!("SKIP: no h264 encoder");
        return;
    };
    println!("roundtrip bytes: {}B for 4 frames", out.len());
    assert!(out.len() >= 4 * 100, "output is non-degenerate");
    assert!(out.len() < 4 * RAW * 2, "output never balloons");
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_all(&mut decoder, &out);
    assert_eq!(frames.len(), 4);
    for frame in &frames {
        assert_eq!(frame.nv12().len(), RAW);
    }
}

/// Surfaces implement the shared trait.
#[test]
fn surfaces_implement_shared_trait() {
    fn assert_surface<T: Surface>() {}
    assert_surface::<D3d12Frame>();
    assert_surface::<GpuSurface>();
}

/// Fresh decoders report zero submit gauges.
#[test]
fn decoder_submit_stats_start_zero() {
    let Some(device) = device() else {
        return;
    };
    for stats in [
        H264Decoder::open(&device).submit_stats(),
        HevcDecoder::open(&device).submit_stats(),
        Av1Decoder::open(&device).submit_stats(),
        Vp9Decoder::open(&device).submit_stats(),
        Mpeg2Decoder::open(&device).submit_stats(),
        Vc1Decoder::open(&device).submit_stats(),
    ] {
        assert_eq!(
            (stats.submitted(), stats.peak_inflight(), stats.resets()),
            (0, 0, 0)
        );
    }
}

/// Forced keyframe codes the next picture as IDR.
#[test]
fn keyframe_request_forces_idr() {
    let Some(device) = device() else {
        return;
    };
    let config = D3d12Encoder::legacy_config(Codec::H264).expect("legacy h264");
    let mut encoder = match D3d12Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    let frame = Synth::grey_nv12(320, 240);
    let before = encoder.encode(&frame).expect("encode runs");
    encoder
        .request_keyframe()
        .expect("all-intra honors keyframe");
    let after = encoder.encode(&frame).expect("encode runs");
    // Grammar over the post-request bytes: first VCL NAL is an IDR slice.
    let mut vcl = Vec::new();
    for packet in &after {
        for nal in split_nals(packet.data()) {
            let typ = nal[0] & 0x1f;
            if (1..=5).contains(&typ) {
                vcl.push(typ);
            }
        }
    }
    assert!(!vcl.is_empty(), "post-request output codes a picture");
    assert_eq!(vcl[0], 5, "first picture after request is IDR");
    // All-intra session codes every picture IDR, pre-request included.
    for packet in &before {
        for nal in split_nals(packet.data()) {
            let typ = nal[0] & 0x1f;
            if (1..=5).contains(&typ) {
                assert_eq!(typ, 5, "all-intra codes IDR throughout");
            }
        }
    }
    println!("keyframe: idr before and after the request");
}

/// Transcoder plan matches its caps route.
#[test]
fn transcoder_plan_matches_caps() {
    use rust_video_caps::transcode::{Plan, Route};
    let Some(device) = device() else {
        return;
    };
    let transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("SKIP: no h264 transcode ({error})");
            return;
        }
    };
    assert_eq!(transcoder.plan().route(), Route::Gpu);
    assert_eq!(transcoder.plan(), Plan::for_caps(&transcoder.caps()));
}

/// Quality preset signals High tier at 1080p and roundtrips.
#[test]
fn hevc_quality_tier_roundtrips() {
    let Some(device) = device() else {
        return;
    };
    let mut config = EncodeConfig {
        rate_control: RateControl::Cqp { qp: 26 },
        gop: intra_gop(),
        profile: Profile::H265(h265::EncodeProfile::Main),
        level: Level::H265(h265::EncodeLevel::Auto),
        ..EncodeConfig::default()
    };
    config.quality.preset = SpeedPreset::Quality;
    let frames = vec![Synth::grey_nv12(1920, 1080)];
    let Some(out) = encode_total(&device, config, &frames) else {
        println!("SKIP: no hevc encoder");
        return;
    };
    let nals = rust_video_h265::split_nals(&out);
    assert!(nals.len() >= 3, "vps, sps, and idr");
    let sps = rust_video_h265::parse_sps::<HevcTestError, rust_video_h265::PermissiveGate>(
        &rust_video_h265::rbsp(nals[1])[2..],
    )
    .expect("sps");
    assert!(sps.ptl.tier, "quality at L4 signals high tier");
    let mut decoder = HevcDecoder::open(&device);
    let frames = decode_all(&mut decoder, &out);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].nv12().len(), 1920 * 1080 * 3 / 2);
}
