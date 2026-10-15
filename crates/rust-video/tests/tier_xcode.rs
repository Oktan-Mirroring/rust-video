//! Facade cross-check: tier resolution plus one live transcode hop.
//!
//! Resolution runs everywhere; the live hop needs `--features tier-d3d12`
//! on Windows hardware and skips without it.

#[cfg(all(feature = "tier-d3d12", target_os = "windows"))]
#[path = "../../rust-video-d3d12/examples/common/bits.rs"]
mod bits;
#[cfg(all(feature = "tier-d3d12", target_os = "windows"))]
#[path = "../../rust-video-d3d12/examples/common/h264.rs"]
mod h264;

use rust_video::{resolve, Capabilities, Platform, Tier, TierPreference, VideoCodec};

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
            resolve(codec, TierPreference::Auto, &caps, Platform::Linux),
            Ok(Tier::Software),
            "{codec:?}"
        );
    }
    let error = resolve(
        VideoCodec::H265,
        TierPreference::Auto,
        &caps,
        Platform::Linux,
    )
    .expect_err("h265 without a realtime backend");
    failed(VideoCodec::H265, TierPreference::Auto, error);
    let caps = caps.with_h265_software();
    assert_eq!(
        resolve(
            VideoCodec::H265,
            TierPreference::Auto,
            &caps,
            Platform::Linux
        ),
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
            resolve(
                VideoCodec::H264,
                TierPreference::Force(tier),
                &caps,
                platform
            ),
            Ok(tier),
            "{tier:?}"
        );
    }
    let error = resolve(
        VideoCodec::H264,
        TierPreference::Force(Tier::VaApi),
        &caps,
        Platform::Windows,
    )
    .expect_err("foreign hardware tier");
    failed(VideoCodec::H264, TierPreference::Force(Tier::VaApi), error);
}

/// Resolution failure names its codec and preference.
#[test]
fn failure_names_codec_and_preference() {
    let caps = Capabilities::none();
    let error = resolve(
        VideoCodec::Av1,
        TierPreference::Auto,
        &caps,
        Platform::Other,
    )
    .expect_err("no tier resolves");
    failed(VideoCodec::Av1, TierPreference::Auto, error);
    assert_eq!(error.to_string(), "no tier available for codec Av1");
}

/// Resolved tier drives one live transcode hop on Windows hardware.
#[cfg(all(feature = "tier-d3d12", target_os = "windows"))]
#[test]
fn resolved_tier_transcodes_one_hop() {
    use rust_video::d3d12::{Codec, D3d12Device, Transcoder};
    use rust_video::{Bytes, Packet, Route};

    let caps = Capabilities::none().with_platform_hw().with_software();
    assert_eq!(
        resolve(
            VideoCodec::H264,
            TierPreference::Force(Tier::D3d12),
            &caps,
            Platform::Windows
        ),
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
    let raws: Vec<Vec<u8>> = crate::h264::annex_b_h264_packets()
        .into_iter()
        .chain(crate::h264::annex_b_h264_packets_cropped())
        .chain(crate::h264::annex_b_h264_packets_cropped_sized(
            39, 22, 4, 22,
        ))
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
    assert!(count + drained.packets().len() > 0);
}
