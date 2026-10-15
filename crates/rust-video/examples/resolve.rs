//! Facade tier resolution across platforms and codecs.

use rust_video::{
    auto_order, host_platform, resolve, Capabilities, Platform, TierPreference, VideoCodec,
};

fn main() {
    let caps = Capabilities::none()
        .with_vulkan()
        .with_platform_hw()
        .with_software()
        .with_h265_software();
    for platform in [
        Platform::Windows,
        Platform::Linux,
        Platform::Android,
        Platform::Other,
    ] {
        for codec in [
            VideoCodec::H264,
            VideoCodec::H265,
            VideoCodec::Av1,
            VideoCodec::Vp9,
            VideoCodec::Vp8,
            VideoCodec::Av2,
        ] {
            match resolve(codec, TierPreference::Auto, &caps, platform) {
                Ok(tier) => println!("{platform:?}/{codec:?} -> {tier:?}"),
                Err(error) => println!("{platform:?}/{codec:?} -> {error}"),
            }
        }
    }
    println!(
        "host: {:?} order: {:?}",
        host_platform(),
        auto_order(host_platform())
    );
}
