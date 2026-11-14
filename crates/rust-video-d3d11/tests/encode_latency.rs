//! Encode latency: every call returns exactly its own picture.
//!
//! Skips without hardware; fails when any call returns another
//! frame's packet or holds one back for the flush.

#[cfg(target_os = "windows")]
use rust_video_caps::traits::Encoder;
#[cfg(target_os = "windows")]
use rust_video_d3d11::{Codec, D3d11Device, D3d11Encoder};
#[cfg(target_os = "windows")]
use rust_video_harness::Synth;

#[cfg(target_os = "windows")]
/// Calls of `codec` each return their own stamped packet, flush stays empty.
fn audit_own_frames(codec: Codec, tag: &str) {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("{tag} skipped: {error}");
            return;
        }
    };
    let mut encoder = match D3d11Encoder::open(&device, codec) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{tag} skipped: {error}");
            return;
        }
    };
    let mut step = None;
    let mut first = None;
    for call in 0..4u64 {
        let frame = Synth::noise(1920, 1080, call);
        let packets = encoder.encode(&frame).expect("encode runs");
        assert!(!packets.is_empty(), "{tag} call {call} held its frame back");
        let pts = packets[0].pts().expect("packet stamped");
        if call == 0 {
            first = Some(pts);
        }
        if call == 1 {
            step = Some(
                pts.checked_sub(first.expect("first stamped"))
                    .expect("pts increase"),
            );
        }
        let call = u32::try_from(call).expect("call fits");
        let want = first.expect("first stamped") + step.unwrap_or_default() * call;
        assert!(
            packets.iter().any(|packet| packet.pts() == Some(want)),
            "{tag} call {call} missed its own stamp"
        );
    }
    let drained = encoder.flush().expect("flush runs");
    assert!(
        drained.is_empty(),
        "{tag} flush held {} back",
        drained.len()
    );
}

#[cfg(target_os = "windows")]
#[test]
fn encode_returns_own_frames() {
    audit_own_frames(Codec::H264, "h264");
    audit_own_frames(Codec::Hevc, "hevc");
    audit_own_frames(Codec::Av1, "av1");
    audit_own_frames(Codec::Vp9, "vp9");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn encode_returns_own_frames() {}
