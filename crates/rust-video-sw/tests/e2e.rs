#![cfg(feature = "ffmpeg")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use rust_video::traits::Decoder as _;
use rust_video::{CpuFrame, Packet, VideoCodec};
use rust_video_harness::{FrameHash, Golden, Suite};
use rust_video_sw::Vp8Decoder;

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

#[test]
fn vp8_decode_matches_golden() {
    let corpus = env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
        PathBuf::from,
    );
    let goldens = env::var_os("RUST_VIDEO_GOLDENS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rust-video-harness/goldens"),
        PathBuf::from,
    );
    let suite = Suite::open_at(&corpus).expect("corpus opens");
    let cases: Vec<_> = suite.cases_for(VideoCodec::Vp8).collect();
    assert!(!cases.is_empty(), "no vp8 cases in {}", corpus.display());
    let mut decoder = Vp8Decoder::open(4).expect("vp8 decoder opens");
    let mut checked = 0;
    let mut skipped = 0;
    for case in cases {
        let stem = Path::new(case.name())
            .file_stem()
            .expect("case has stem")
            .to_string_lossy()
            .into_owned();
        let golden_path = goldens.join("vp8").join(format!("{stem}.sha256"));
        if !golden_path.is_file() {
            println!("skip {}: no golden {}", case.name(), golden_path.display());
            skipped += 1;
            continue;
        }
        let golden = Golden::load(&golden_path).expect("golden loads");
        let mut actual = Vec::new();
        for payload in ivf_frames(case.path()) {
            let packet = Packet::new(Bytes::from(payload));
            let frames = decoder
                .decode(&packet)
                .unwrap_or_else(|err| panic!("decode {}: {err}", case.name()));
            for frame in &frames {
                actual.push(hash_frame(frame));
            }
        }
        let tail = decoder
            .flush()
            .unwrap_or_else(|err| panic!("flush {}: {err}", case.name()));
        for frame in &tail {
            actual.push(hash_frame(frame));
        }
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
    println!("vp8: {checked} matched, {skipped} skipped");
    assert!(checked > 0, "no vp8 case checked");
}
