//! Facade cross-check: tier resolution plus one live transcode hop.
//!
//! Resolution runs everywhere; each live hop runs on its own hardware
//! and skips loud without it.

#[cfg(any(d3d11_available, d3d12_available))]
use rust_video_harness::H264Synth;
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    have_corpus,
    any(vaapi_available, feature = "vulkan")
))]
use rust_video_harness::Stream;

use rust_video::{Capabilities, Platform, Tier, TierPreference, VideoCodec};

/// Resolution failure carries `codec` under `preference`.
fn failed(codec: VideoCodec, preference: TierPreference, error: rust_video::TierError) {
    assert_eq!(error.codec(), codec);
    assert_eq!(error.preference(), preference);
}

/// Resolution of every codec under software-only caps.
#[test]
fn software_caps_resolve_all_but_h265() {
    let caps = Capabilities::none().with_software();
    for codec in [
        VideoCodec::H264,
        VideoCodec::Vp8,
        VideoCodec::Vp9,
        VideoCodec::Av1,
        VideoCodec::Av2,
    ] {
        assert_eq!(
            TierPreference::Auto.resolve(codec, &caps, Platform::Linux),
            Ok(Tier::Software),
            "{codec:?}"
        );
    }
    let error = TierPreference::Auto
        .resolve(VideoCodec::H265, &caps, Platform::Linux)
        .expect_err("h265 without a realtime backend");
    failed(VideoCodec::H265, TierPreference::Auto, error);
    let caps = caps.with_h265_software();
    assert_eq!(
        TierPreference::Auto.resolve(VideoCodec::H265, &caps, Platform::Linux),
        Ok(Tier::Software)
    );
}

/// Forced hardware tiers bind to their native platform only.
#[test]
fn forced_hardware_binds_native_platform() {
    let caps = Capabilities::none().with_platform_hw();
    for (tier, platform) in [
        (Tier::D3d11, Platform::Windows),
        (Tier::D3d12, Platform::Windows),
        (Tier::VaApi, Platform::Linux),
        (Tier::MediaCodec, Platform::Android),
    ] {
        assert_eq!(
            TierPreference::Force(tier).resolve(VideoCodec::H264, &caps, platform),
            Ok(tier),
            "{tier:?}"
        );
    }
    let error = TierPreference::Force(Tier::VaApi)
        .resolve(VideoCodec::H264, &caps, Platform::Windows)
        .expect_err("foreign hardware tier");
    failed(VideoCodec::H264, TierPreference::Force(Tier::VaApi), error);
}

/// Resolution failure names its codec and preference.
#[test]
fn failure_names_codec_and_preference() {
    let caps = Capabilities::none();
    let error = TierPreference::Auto
        .resolve(VideoCodec::Av1, &caps, Platform::Other)
        .expect_err("no tier resolves");
    failed(VideoCodec::Av1, TierPreference::Auto, error);
    assert_eq!(error.to_string(), "no tier available for codec Av1");
}

/// Synth H.264 vector under the workspace corpus.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    have_corpus,
    any(vaapi_available, feature = "vulkan")
))]
fn synth_h264() -> std::path::PathBuf {
    std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus/synth/h264/h264-high.mp4")
        },
        |root| std::path::PathBuf::from(root).join("synth/h264/h264-high.mp4"),
    )
}

/// Resolved tier drives one live VA-API transcode hop on Linux hardware.
#[cfg(all(target_os = "linux", vaapi_available, have_corpus))]
#[test]
fn resolved_tier_transcodes_vaapi_hop() {
    use bytes::Bytes;
    use rust_video::{Packet, Route};
    use rust_video_vaapi::{EncodeCodec, Transcoder, VaApiDevice, VaApiError};

    let caps = Capabilities::none().with_platform_hw().with_software();
    assert_eq!(
        TierPreference::Force(Tier::VaApi).resolve(VideoCodec::H264, &caps, Platform::Linux),
        Ok(Tier::VaApi)
    );
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("facade xcode skipped: no va-api device ({reason})");
            return;
        }
        Err(error) => panic!("facade xcode: va-api device failed ({error})"),
    };
    let mut transcoder = Transcoder::open(&device, EncodeCodec::H264);
    if !transcoder.caps().ready() {
        println!("facade xcode skipped: va-api without export plus encode caps");
        return;
    }
    let stream = Stream::demux(&synth_h264()).expect("synth h264 vector");
    let mut count = 0;
    for raw in stream.samples.iter().take(3) {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::copy_from_slice(raw)))
            .expect("facade hop");
        assert_eq!(outcome.plan().route(), Route::Gpu);
        count += outcome.packets().len();
    }
    let drained = transcoder.flush().expect("facade drain");
    assert_eq!(drained.plan().route(), Route::Gpu);
    count += drained.packets().len();
    assert!(count > 0, "no packets from the va-api hop");
    println!("facade xcode live: va-api gpu hop, {count} packets");
}

/// One live Vulkan transcode hop on `platform` hardware.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vulkan",
    have_corpus
))]
fn vulkan_hop(platform: Platform) {
    use bytes::Bytes;
    use rust_video::{FallbackCause, Packet};
    use rust_video_vulkan::{CodecOp, Transcoder, VulkanDevice};

    let caps = Capabilities::none().with_vulkan().with_software();
    assert_eq!(
        TierPreference::Force(Tier::Vulkan).resolve(VideoCodec::H264, &caps, platform),
        Ok(Tier::Vulkan)
    );
    let decode_device = match VulkanDevice::open(CodecOp::H264Decode) {
        Ok(Some(device)) => device,
        Ok(None) => {
            println!("facade xcode skipped: no vulkan decode device");
            return;
        }
        Err(error) => panic!("facade xcode: vulkan decode negotiation failed ({error})"),
    };
    let encode_device = match VulkanDevice::open(CodecOp::H264Encode) {
        Ok(Some(device)) => device,
        Ok(None) => {
            println!("facade xcode skipped: no vulkan encode device");
            return;
        }
        Err(error) => panic!("facade xcode: vulkan encode negotiation failed ({error})"),
    };
    let mut transcoder = Transcoder::open(
        &decode_device,
        CodecOp::H264Decode,
        &encode_device,
        CodecOp::H264Encode,
    )
    .expect("facade transcode open");
    let plan = transcoder.plan();
    if transcoder.caps().ready() && plan.cause() == Some(FallbackCause::DeviceMismatch) {
        println!("facade xcode skipped: vulkan decode and encode adapters differ");
        return;
    }
    let stream = Stream::demux(&synth_h264()).expect("synth h264 vector");
    let mut count = 0;
    for raw in stream.samples.iter().take(3) {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::copy_from_slice(raw)))
            .expect("facade hop");
        assert_eq!(outcome.plan(), plan);
        count += outcome.packets().len();
    }
    let drained = transcoder.flush().expect("facade drain");
    assert_eq!(drained.plan(), plan);
    count += drained.packets().len();
    assert!(count > 0, "no packets from the vulkan hop");
    println!(
        "facade xcode live: vulkan {:?} hop, {count} packets",
        plan.route()
    );
}

/// Resolved tier drives one live Vulkan transcode hop on Linux hardware.
#[cfg(all(target_os = "linux", feature = "vulkan", have_corpus))]
#[test]
fn resolved_tier_transcodes_vulkan_hop() {
    vulkan_hop(Platform::Linux);
}

/// Resolved tier drives one live Vulkan transcode hop on Windows hardware.
#[cfg(all(target_os = "windows", feature = "vulkan", have_corpus))]
#[test]
fn resolved_tier_transcodes_vulkan_hop_windows() {
    vulkan_hop(Platform::Windows);
}

/// Resolved tier drives one live transcode hop on Windows hardware.
#[cfg(d3d12_available)]
#[test]
fn resolved_tier_transcodes_one_hop() {
    use bytes::Bytes;
    use rust_video::{Packet, Route};
    use rust_video_d3d12::{Codec, D3d12Device, Transcoder};

    let caps = Capabilities::none().with_platform_hw().with_software();
    assert_eq!(
        TierPreference::Force(Tier::D3d12).resolve(VideoCodec::H264, &caps, Platform::Windows),
        Ok(Tier::D3d12)
    );
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("facade xcode skipped: {error}");
            return;
        }
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("facade xcode skipped: {error}");
            return;
        }
    };
    if !transcoder.caps().ready() {
        println!("facade xcode skipped: d3d12 without export plus encode caps");
        return;
    }
    let raws: Vec<Vec<u8>> = H264Synth::annex_b_h264_packets()
        .into_iter()
        .chain(H264Synth::annex_b_h264_packets_cropped())
        .chain(H264Synth::annex_b_h264_packets_cropped_sized(39, 22, 4, 22))
        .collect();
    let mut count = 0;
    for raw in raws {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::from(raw)))
            .expect("facade hop");
        assert_eq!(outcome.plan().route(), Route::Gpu);
        count += outcome.packets().len();
    }
    let drained = transcoder.flush().expect("facade drain");
    assert_eq!(drained.plan().route(), Route::Gpu);
    count += drained.packets().len();
    assert!(count > 0, "no packets from the d3d12 hop");
    println!("facade xcode live: d3d12 gpu hop, {count} packets");
}

/// Resolved tier drives one live d3d11 transcode hop on Windows hardware.
#[cfg(d3d11_available)]
#[test]
fn resolved_tier_transcodes_d3d11_hop() {
    use bytes::Bytes;
    use rust_video::{Packet, Route};
    use rust_video_d3d11::{Codec, D3d11Device, Transcoder};

    let caps = Capabilities::none().with_platform_hw().with_software();
    assert_eq!(
        TierPreference::Force(Tier::D3d11).resolve(VideoCodec::H264, &caps, Platform::Windows),
        Ok(Tier::D3d11)
    );
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("facade xcode skipped: {error}");
            return;
        }
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("facade xcode skipped: {error}");
            return;
        }
    };
    if !transcoder.caps().ready() {
        println!("facade xcode skipped: d3d11 without export plus encode caps");
        return;
    }
    let raws: Vec<Vec<u8>> = H264Synth::annex_b_h264_packets()
        .into_iter()
        .chain(H264Synth::annex_b_h264_packets_cropped())
        .chain(H264Synth::annex_b_h264_packets_cropped_sized(39, 22, 4, 22))
        .collect();
    let mut count = 0;
    for raw in raws {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::from(raw)))
            .expect("facade hop");
        assert_eq!(outcome.plan().route(), Route::Gpu);
        count += outcome.packets().len();
    }
    let drained = transcoder.flush().expect("facade drain");
    assert_eq!(drained.plan().route(), Route::Gpu);
    count += drained.packets().len();
    assert!(count > 0, "no packets from the d3d11 hop");
    println!("facade xcode live: d3d11 gpu hop, {count} packets");
}
