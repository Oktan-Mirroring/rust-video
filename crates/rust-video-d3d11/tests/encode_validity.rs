//! Encode validity: golden bytes, grammar parses, roundtrip bounds,
//! config reflection, and typed declines.
//!
//! Skips without hardware; fails when the encoder diverges.

#![cfg(target_os = "windows")]

use rust_video_d3d11::{Codec, D3d11Device, D3d11Encoder, D3d11Error, H264Decoder};

use rust_video_caps::encode::{Decline, EncodeConfig, RateControl};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder, KeyframeRequest};
use rust_video_harness::Synth;

/// Pre-change bytes: 8 grey 320x240 frames plus flush drain.
const H264_GOLDEN: &[u8] = include_bytes!("vectors/h264-grey-320x240-8f.bin");
const H265_GOLDEN: &[u8] = include_bytes!("vectors/h265-grey-320x240-8f.bin");

/// Open device, loud skip without hardware.
fn device() -> Option<D3d11Device> {
    match D3d11Device::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("d3d11 skipped: {error}");
            None
        }
    }
}

/// Open auto encoder for `codec`, loud skip without an MFT.
fn auto_encoder(device: &D3d11Device, codec: Codec) -> Option<D3d11Encoder> {
    let config = D3d11Encoder::auto_config(codec).expect("codec auto");
    match D3d11Encoder::open(device, config) {
        Ok(encoder) => {
            println!("{} mft: {}", codec, encoder.mft_name());
            Some(encoder)
        }
        Err(error) => {
            println!("{codec} skipped: {error}");
            None
        }
    }
}

/// Packet bytes of 8 `frame` encodes plus the flush drain.
fn encode_eight(encoder: &mut D3d11Encoder, frame: &CpuFrame) -> Vec<Packet> {
    let mut packets = Vec::new();
    for _ in 0..8 {
        packets.extend(encoder.encode(frame).expect("encode runs"));
    }
    packets.extend(encoder.flush().expect("flush runs"));
    packets
}

/// Concatenated packet bytes.
fn concat(packets: &[Packet]) -> Vec<u8> {
    let mut out = Vec::new();
    for packet in packets {
        out.extend_from_slice(packet.data());
    }
    out
}

/// Mean absolute byte difference in thousandths between `a` and `b`.
fn mad_milli(a: &[u8], b: &[u8]) -> u64 {
    assert_eq!(a.len(), b.len(), "compared extents match");
    let sum: u64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    let len = u64::try_from(a.len()).expect("frame fits");
    sum.saturating_mul(1000) / len
}

/// Auto output matches the pre-change golden bytes on fixed input.
#[test]
fn auto_output_matches_prechange_bytes() {
    let Some(device) = device() else {
        return;
    };
    for (codec, golden) in [(Codec::H264, H264_GOLDEN), (Codec::H265, H265_GOLDEN)] {
        let Some(mut encoder) = auto_encoder(&device, codec) else {
            continue;
        };
        let frame = Synth::grey_nv12(320, 240);
        let packets = encode_eight(&mut encoder, &frame);
        let bytes = concat(&packets);
        assert_eq!(
            bytes.len(),
            golden.len(),
            "{codec} golden byte count matches"
        );
        assert_eq!(bytes, golden, "{codec} golden bytes match");
        println!("{codec} golden: {} bytes, 0 mismatches", bytes.len());
    }
}

/// Same fixed input encodes twice to identical bytes.
#[test]
fn encoder_output_is_deterministic() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut first) = auto_encoder(&device, Codec::H264) else {
        return;
    };
    let frame = Synth::grey_nv12(320, 240);
    let one = concat(&encode_eight(&mut first, &frame));
    drop(first);
    let Some(mut second) = auto_encoder(&device, Codec::H264) else {
        return;
    };
    let two = concat(&encode_eight(&mut second, &frame));
    assert_eq!(one, two, "repeated encodes match byte-exact");
    println!("determinism: {} bytes twice, 0 mismatches", one.len());
}

/// H.264 output parses with SPS/PPS and a keyframe first.
#[test]
fn h264_output_parses_keyframe_first() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut encoder) = auto_encoder(&device, Codec::H264) else {
        return;
    };
    let frame = Synth::grey_nv12(320, 240);
    let mut packets = Vec::new();
    for _ in 0..4 {
        packets.extend(encoder.encode(&frame).expect("encode runs"));
    }
    packets.extend(encoder.flush().expect("flush runs"));
    assert!(!packets.is_empty(), "encoder emits packets");
    assert!(
        packets[0].is_keyframe(),
        "first packet carries the keyframe flag"
    );
    let mut ingest = rust_video_h264::Ingest::<D3d11Error, rust_video_h264::PermissiveGate>::new();
    let mut pictures = Vec::new();
    for packet in &packets {
        pictures.extend(ingest.ingest_packet(packet.data()).expect("grammar parses"));
    }
    assert_eq!(pictures.len(), 4, "four frames parse to four pictures");
    assert!(pictures[0].intra(), "first picture is intra");
    let sps = ingest.sps(0).expect("sps parses");
    assert!(ingest.pps(pictures[0].pps_id()).is_some(), "pps parses");
    println!(
        "h264 grammar: profile {} level {} first-intra",
        sps.profile_idc, sps.level_idc
    );
}

/// HEVC output parses with SPS/PPS and a keyframe first.
#[test]
fn h265_output_parses_keyframe_first() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut encoder) = auto_encoder(&device, Codec::H265) else {
        return;
    };
    let frame = Synth::grey_nv12(320, 240);
    let mut packets = Vec::new();
    for _ in 0..4 {
        packets.extend(encoder.encode(&frame).expect("encode runs"));
    }
    packets.extend(encoder.flush().expect("flush runs"));
    assert!(!packets.is_empty(), "encoder emits packets");
    assert!(
        packets[0].is_keyframe(),
        "first packet carries the keyframe flag"
    );
    let mut ingest = rust_video_h265::Ingest::<D3d11Error, rust_video_h265::PermissiveGate>::new();
    let mut pictures = Vec::new();
    for packet in &packets {
        pictures.extend(ingest.ingest_packet(packet.data()).expect("grammar parses"));
    }
    assert_eq!(pictures.len(), 4, "four frames parse to four pictures");
    assert!(ingest.sps(0).is_some(), "sps parses");
    assert!(ingest.pps(pictures[0].pps_id()).is_some(), "pps parses");
    println!("h265 grammar: four pictures, sets resolve, keyframe first");
}

/// Requested H.264 profile lands in the parsed SPS.
#[test]
fn h264_profile_signals_into_sps() {
    use rust_video_caps::encode::Profile;
    use rust_video_caps::h264::EncodeProfile;

    let Some(device) = device() else {
        return;
    };
    for (profile, idc) in [
        (EncodeProfile::Baseline, 66),
        (EncodeProfile::Main, 77),
        (EncodeProfile::High, 100),
    ] {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 2_000_000,
                maxrate: 2_000_000,
                bufsize: 2_000_000,
            },
            profile: Profile::H264(profile),
            ..EncodeConfig::default()
        };
        let mut encoder = match D3d11Encoder::open(&device, config) {
            Ok(encoder) => encoder,
            Err(error) => {
                println!("h264 {profile:?} skipped: {error}");
                continue;
            }
        };
        let frame = Synth::grey_nv12(320, 240);
        let mut ingest =
            rust_video_h264::Ingest::<D3d11Error, rust_video_h264::PermissiveGate>::new();
        for _ in 0..4 {
            for packet in encoder.encode(&frame).expect("encode runs") {
                ingest.ingest_packet(packet.data()).expect("grammar parses");
            }
        }
        let sps = ingest.sps(0).expect("sps parses");
        assert_eq!(
            sps.profile_idc, idc,
            "{profile:?} request signals profile {idc}"
        );
        println!("h264 {profile:?}: sps profile {}", sps.profile_idc);
    }
}

/// Forced keyframe codes the next picture as IDR.
#[test]
fn keyframe_request_forces_idr() {
    let Some(device) = device() else {
        return;
    };
    let config = EncodeConfig {
        rate_control: RateControl::Cbr {
            bitrate: 2_000_000,
            maxrate: 2_000_000,
            bufsize: 2_000_000,
        },
        ..EncodeConfig::default()
    };
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("cbr skipped: {error}");
            return;
        }
    };
    let frame = Synth::grey_nv12(320, 240);
    let mut packets = Vec::new();
    for _ in 0..4 {
        packets.extend(encoder.encode(&frame).expect("encode runs"));
    }
    encoder.request_keyframe().expect("keyframe forces");
    for _ in 0..2 {
        packets.extend(encoder.encode(&frame).expect("encode runs"));
    }
    packets.extend(encoder.flush().expect("flush runs"));
    let mut ingest = rust_video_h264::Ingest::<D3d11Error, rust_video_h264::PermissiveGate>::new();
    let mut pictures = Vec::new();
    for packet in &packets {
        pictures.extend(ingest.ingest_packet(packet.data()).expect("grammar parses"));
    }
    assert_eq!(pictures.len(), 6, "six frames parse to six pictures");
    assert!(
        pictures[1..4].iter().any(|picture| !picture.idr),
        "p-frames flow before the request"
    );
    assert!(pictures[4].idr, "first picture after the request is IDR");
    println!("keyframe: p-flow then forced IDR at picture 5");
}

/// Grey and pattern roundtrips land within error bounds.
#[test]
fn grey_and_pattern_roundtrip_within_bounds() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut encoder) = auto_encoder(&device, Codec::H264) else {
        return;
    };
    let grey = Synth::grey_nv12(320, 240);
    let noise = Synth::noise(320, 240, 7);
    let grey_packets = encode_eight(&mut encoder, &grey);
    let noise_packets = encode_eight(&mut encoder, &noise);
    // One decoder over the continuous stream: sets ride the first segment.
    let mut decoder = H264Decoder::open(&device);
    let mut frames = Vec::new();
    for packets in [&grey_packets, &noise_packets] {
        for packet in packets {
            frames.extend(decoder.decode(packet).expect("decode runs"));
        }
    }
    frames.extend(decoder.flush().expect("decode drains"));
    assert_eq!(frames.len(), 16, "roundtrip yields every frame");
    for (tag, frame, bound, slice) in [
        ("grey", &grey, 1000u64, &frames[..8]),
        ("noise", &noise, 30_000u64, &frames[8..]),
    ] {
        let y = frame.planes()[0].data();
        let uv = frame.planes()[1].data();
        let mut src = Vec::with_capacity(y.len() + uv.len());
        src.extend_from_slice(y);
        src.extend_from_slice(uv);
        let mut total = 0u64;
        for decoded in slice {
            total += mad_milli(&src, decoded.nv12());
        }
        let count = u64::try_from(slice.len()).expect("count fits");
        let mean = total / count;
        println!("roundtrip {tag}: mean abs diff {mean} milli, bound {bound}");
        assert!(mean < bound, "{tag} mean {mean} within {bound}");
    }
    assert!(
        decoder.flush().expect("decode drains").is_empty(),
        "nothing held back"
    );
}

/// Bitrate knob changes the output size class.
#[test]
fn bitrate_knob_changes_output_size_class() {
    let Some(device) = device() else {
        return;
    };
    let mut sizes = Vec::new();
    for bitrate in [500_000u32, 8_000_000] {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate,
                maxrate: bitrate,
                bufsize: bitrate,
            },
            ..EncodeConfig::default()
        };
        let mut encoder = match D3d11Encoder::open(&device, config) {
            Ok(encoder) => encoder,
            Err(error) => {
                println!("cbr {bitrate} skipped: {error}");
                return;
            }
        };
        let mut bytes = 0;
        for seed in 0..8u64 {
            for packet in encoder
                .encode(&Synth::noise(320, 240, seed))
                .expect("encode runs")
            {
                bytes += packet.data().len();
            }
        }
        for packet in encoder.flush().expect("flush runs") {
            bytes += packet.data().len();
        }
        assert!(bytes > 0, "cbr {bitrate} emits bytes");
        sizes.push(bytes);
    }
    println!(
        "size class: {} bytes at 500k vs {} at 8M, ratio {}%",
        sizes[0],
        sizes[1],
        sizes[1] * 100 / sizes[0]
    );
    assert!(
        sizes[1] * 2 > sizes[0] * 3,
        "bitrate knob moves size class past 1.5x"
    );
}

/// Unsupported configs decline with typed reasons.
#[test]
fn declined_configs_fail_typed() {
    use rust_video_caps::color::ContentLight;
    use rust_video_caps::encode::{Gop, Level, Profile};

    let Some(device) = device() else {
        return;
    };
    // VP8 names no encoder MFT.
    let vp8 = D3d11Encoder::auto_config(Codec::Vp8).expect("vp8 auto");
    assert!(matches!(
        D3d11Encoder::open(&device, vp8),
        Err(D3d11Error::Declined(Decline::ProfileUnsupported { .. }))
    ));
    // CQP names no rate path.
    let config = EncodeConfig {
        rate_control: RateControl::Cqp { qp: 23 },
        ..EncodeConfig::default()
    };
    assert!(matches!(
        D3d11Encoder::open(&device, config),
        Err(D3d11Error::Declined(Decline::RateControlUnsupported { .. }))
    ));
    // HDR metadata declines on the SDR tier.
    let mut config = EncodeConfig::default();
    config.color.light = Some(ContentLight {
        max_cll: 1000,
        max_fall: 400,
    });
    assert!(matches!(
        D3d11Encoder::open(&device, config),
        Err(D3d11Error::Declined(Decline::HdrUnsupported))
    ));
    // 10-bit depth declines under an allowlisted 10-bit profile.
    let config = EncodeConfig {
        profile: Profile::Av1(rust_video_caps::av1::EncodeProfile::Main),
        level: Level::Av1(rust_video_caps::av1::EncodeLevel::Auto),
        bit_depth: 10,
        ..EncodeConfig::default()
    };
    assert!(matches!(
        D3d11Encoder::open(&device, config),
        Err(D3d11Error::Declined(Decline::BitDepthUnsupported { .. }))
    ));
    // Triple B-frames exceed the prediction budget.
    let config = EncodeConfig {
        gop: Gop {
            b_frames: 3,
            ..Gop::default()
        },
        ..EncodeConfig::default()
    };
    assert!(matches!(
        D3d11Encoder::open(&device, config),
        Err(D3d11Error::Declined(Decline::BFramesUnsupported { .. }))
    ));
    println!("declines: vp8, cqp, hdr, 10-bit, b-frames all typed");
}

/// Encode probe agrees with encoder open.
#[test]
fn supports_encode_agrees_with_open() {
    let Some(device) = device() else {
        return;
    };
    for codec in [Codec::H264, Codec::H265, Codec::Vp9, Codec::Av1] {
        let config = D3d11Encoder::auto_config(codec).expect("codec auto");
        if D3d11Encoder::open(&device, config).is_ok() {
            assert!(
                device.supports_encode(codec),
                "{codec} open implies probe support"
            );
        }
    }
    for codec in [Codec::Vp8, Codec::Mpeg2, Codec::Vc1, Codec::Av2] {
        assert!(!device.supports_encode(codec), "{codec} probe stays silent");
    }
    println!("supports_encode agrees with open");
}
