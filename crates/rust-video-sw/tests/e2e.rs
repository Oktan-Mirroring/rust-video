#![cfg(feature = "ffmpeg")]

#[path = "../../rust-video-vaapi/tests/support/mp4.rs"]
mod mp4;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use rust_video::VideoCodec;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{Case, FrameHash, Golden, Suite};
use rust_video_sw::{Av1Decoder, H264Decoder, Vp8Decoder, Vp9Decoder};

fn ivf_frames(path: &Path) -> Vec<Vec<u8>> {
    let bytes = fs::read(path).unwrap_or_else(|_| panic!("unreadable ivf: {}", path.display()));
    assert!(bytes.len() >= 32, "short header: {}", path.display());
    assert!(&bytes[0..4] == b"DKIF", "bad magic: {}", path.display());
    let mut frames = Vec::new();
    let mut pos = 32;
    while pos < bytes.len() {
        assert!(
            pos + 12 <= bytes.len(),
            "short frame header: {}",
            path.display()
        );
        let size = usize::try_from(u32::from_le_bytes(
            bytes[pos..pos + 4].try_into().expect("frame size"),
        ))
        .expect("frame size fits");
        pos += 12;
        assert!(
            pos + size <= bytes.len(),
            "short frame payload: {}",
            path.display()
        );
        frames.push(bytes[pos..pos + size].to_vec());
        pos += size;
    }
    frames
}

fn hash_frame(frame: &CpuFrame) -> FrameHash {
    let mut bytes = Vec::new();
    for plane in frame.planes() {
        bytes.extend_from_slice(plane.data());
    }
    FrameHash::of(&bytes)
}

fn corpus() -> PathBuf {
    env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
        PathBuf::from,
    )
}

fn goldens() -> PathBuf {
    env::var_os("RUST_VIDEO_GOLDENS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rust-video-harness/goldens"),
        PathBuf::from,
    )
}

/// Payloads of one IVF case, `None` for containers outside the IVF path.
fn ivf_payloads(case: &Case) -> Option<Vec<Vec<u8>>> {
    let ivf = Path::new(case.name())
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ivf"));
    ivf.then(|| ivf_frames(case.path()))
}

/// Payloads of one MP4 case, `None` for containers outside the MP4 path.
fn mp4_payloads(case: &Case) -> Option<Vec<Vec<u8>>> {
    let mp4 = Path::new(case.name())
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"));
    if !mp4 {
        return None;
    }
    Some(
        mp4::demux(case.path())
            .unwrap_or_else(|err| panic!("demux {}: {err}", case.name()))
            .samples,
    )
}

/// Golden hashes decoded from `payloads` through `decoder`.
fn decode_hashes<D>(decoder: &mut D, name: &str, payloads: &[Vec<u8>]) -> Vec<FrameHash>
where
    D: Decoder<Frame = CpuFrame>,
{
    let mut actual = Vec::new();
    for payload in payloads {
        let packet = Packet::new(Bytes::from(payload.clone()));
        let frames = decoder
            .decode(&packet)
            .unwrap_or_else(|err| panic!("decode {name}: {err}"));
        for frame in &frames {
            actual.push(hash_frame(frame));
        }
    }
    let tail = decoder
        .flush()
        .unwrap_or_else(|err| panic!("flush {name}: {err}"));
    for frame in &tail {
        actual.push(hash_frame(frame));
    }
    actual
}

/// Cases of `codec` checked against `golden_dir`, skipping vectors the
/// packetizer declines; skips the codec when `required` is false and the
/// corpus holds none.
fn check_codec<D>(
    codec: VideoCodec,
    golden_dir: &str,
    decoder: &mut D,
    payloads_of: fn(&Case) -> Option<Vec<Vec<u8>>>,
    required: bool,
) where
    D: Decoder<Frame = CpuFrame>,
{
    let root = corpus();
    let suite = Suite::open_at(&root).expect("corpus opens");
    let cases: Vec<_> = suite.cases_for(codec).collect();
    if cases.is_empty() {
        assert!(!required, "no {golden_dir} cases in {}", root.display());
        println!("{golden_dir}: no cases, vectors not fetched");
        return;
    }
    let goldens = goldens();
    let mut checked = 0;
    for case in cases {
        let Some(payloads) = payloads_of(case) else {
            println!("{}: outside packetizer, skipped", case.name());
            continue;
        };
        if case.name().starts_with("invalid-") {
            println!("{}: error vector, skipped", case.name());
            continue;
        }
        let stem = Path::new(case.name())
            .file_stem()
            .expect("case has stem")
            .to_string_lossy()
            .into_owned();
        let golden_path = goldens.join(golden_dir).join(format!("{stem}.sha256"));
        assert!(
            golden_path.is_file(),
            "missing golden for {}: {}",
            case.name(),
            golden_path.display()
        );
        let golden = Golden::load(&golden_path).expect("golden loads");
        let actual = decode_hashes(decoder, case.name(), &payloads);
        let gaps = golden.check(&actual);
        assert!(
            gaps.is_empty(),
            "mismatch {}: {}",
            case.name(),
            gaps.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
        checked += 1;
    }
    println!("{golden_dir}: {checked} matched");
    assert!(checked > 0, "no {golden_dir} case checked");
}

#[test]
fn vp8_decode_matches_golden() {
    let mut decoder = Vp8Decoder::open(4).expect("vp8 decoder opens");
    check_codec(VideoCodec::Vp8, "vp8", &mut decoder, ivf_payloads, true);
}

#[test]
fn vp9_decode_matches_golden() {
    let mut decoder = Vp9Decoder::open(4).expect("vp9 decoder opens");
    check_codec(VideoCodec::Vp9, "vp9", &mut decoder, ivf_payloads, true);
}

#[test]
fn h264_decode_matches_golden() {
    let mut decoder = H264Decoder::open(4).expect("h264 decoder opens");
    check_codec(VideoCodec::H264, "h264", &mut decoder, mp4_payloads, false);
}

#[test]
fn av1_decode_matches_golden() {
    let mut decoder = Av1Decoder::open(4).expect("av1 decoder opens");
    check_codec(VideoCodec::Av1, "av1", &mut decoder, ivf_payloads, false);
}
