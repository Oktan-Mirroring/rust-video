//! Matrix golden runner: one data-driven test walks every committed
//! golden; no per-case test functions. Multi-slice openh264 output
//! still decodes to full-size frames.

#[cfg(feature = "ffmpeg")]
use std::env;
#[cfg(feature = "ffmpeg")]
use std::path::{Path, PathBuf};

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use bytes::Bytes;
#[cfg(all(feature = "h264-openh264", nasm_available))]
use openh264::decoder::{DecodeOptions, Decoder as OpenH264Decoder, DecoderConfig, Flush};
#[cfg(all(feature = "h264-openh264", nasm_available))]
use openh264::formats::YUVSource;
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_caps::common::PictureSize;
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_caps::encode::{EncodeConfig, FrameRate, RateControl};
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use rust_video_caps::frame::CpuFrame;
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_caps::frame::{PixelFormat, Plane};
#[cfg(feature = "ffmpeg")]
use rust_video_caps::packet::Packet;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::traits::Decoder;
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_caps::traits::Encoder as _;
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_harness::Synth;
#[cfg(feature = "ffmpeg")]
use rust_video_harness::{FrameHash, Golden, Ivf, Stream};
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_sw::H264Encoder;
#[cfg(feature = "ffmpeg")]
use rust_video_sw::{Av1Decoder, H264Decoder, H265Decoder, Vp8Decoder, Vp9Decoder};

#[cfg(feature = "ffmpeg")]
fn hash_frame(frame: &CpuFrame) -> FrameHash {
    let mut bytes = Vec::new();
    for plane in frame.planes() {
        bytes.extend_from_slice(plane.data());
    }
    FrameHash::of(&bytes)
}

/// Golden expectation flavor plus its recorded reason.
#[cfg(feature = "ffmpeg")]
enum Expect {
    /// Decode must succeed byte-exact.
    Match,
    /// Decode byte-exact or decline cleanly.
    Either(String),
    /// Decode must return `Err`.
    Decline(String),
}

#[cfg(feature = "ffmpeg")]
fn goldens() -> PathBuf {
    env::var_os("RUST_VIDEO_GOLDENS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rust-video-harness/goldens"),
        PathBuf::from,
    )
}

#[cfg(feature = "ffmpeg")]
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Resolve a `# vector:` header path: workspace-relative by default,
/// re-rooted under `RUST_VIDEO_CORPUS` when set.
#[cfg(feature = "ffmpeg")]
fn resolve(header_path: &str) -> PathBuf {
    let rel = PathBuf::from(header_path);
    if let Some(root) = env::var_os("RUST_VIDEO_CORPUS") {
        if let Ok(stripped) = rel.strip_prefix("corpus") {
            return PathBuf::from(root).join(stripped);
        }
    }
    workspace_root().join(rel)
}

#[cfg(feature = "ffmpeg")]
fn parse_header(path: &Path) -> (PathBuf, Expect) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let mut vector = None;
    let mut expect = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# vector:") {
            vector = Some(resolve(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("# expect:") {
            let rest = rest.trim();
            if rest == "match" {
                expect = Some(Expect::Match);
            } else if let Some(reason) = rest.strip_prefix("either ") {
                expect = Some(Expect::Either(reason.to_owned()));
            } else if let Some(reason) = rest.strip_prefix("decline ") {
                expect = Some(Expect::Decline(reason.to_owned()));
            } else {
                panic!("{}: bad expect line: {line}", path.display());
            }
        }
    }
    match (vector, expect) {
        (Some(vector), Some(expect)) => (vector, expect),
        _ => panic!("{}: golden lacks vector/expect header", path.display()),
    }
}

/// Decode payloads for one matrix line: mp4 demuxes to samples, IVF to
/// frames, Annex B and OBU ride as one packet each (the sw decoders
/// split byte streams internally).
#[cfg(feature = "ffmpeg")]
fn payloads(vector: &Path) -> Vec<Vec<u8>> {
    let ext = vector
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    if ext.eq_ignore_ascii_case("mp4") {
        return Stream::demux(vector)
            .unwrap_or_else(|err| panic!("demux {}: {err}", vector.display()))
            .samples;
    }
    if ext.eq_ignore_ascii_case("ivf") {
        return Ivf::read(vector).frames;
    }
    vec![std::fs::read(vector).unwrap_or_else(|err| panic!("read {}: {err}", vector.display()))]
}

/// Hashes of every decoded frame, or the first decode error.
#[cfg(feature = "ffmpeg")]
fn decode_all<D>(decoder: &mut D, payloads: &[Vec<u8>]) -> Result<Vec<FrameHash>, String>
where
    D: Decoder<Frame = CpuFrame>,
{
    let mut actual = Vec::new();
    for payload in payloads {
        let packet = Packet::new(Bytes::from(payload.clone()));
        let frames = decoder.decode(&packet).map_err(|err| err.to_string())?;
        for frame in &frames {
            actual.push(hash_frame(frame));
        }
    }
    let tail = decoder.flush().map_err(|err| err.to_string())?;
    for frame in &tail {
        actual.push(hash_frame(frame));
    }
    Ok(actual)
}

#[cfg(feature = "ffmpeg")]
fn check_one<D>(codec: &str, golden_path: &Path, decoder: &mut D)
where
    D: Decoder<Frame = CpuFrame>,
{
    let stem = golden_path
        .file_stem()
        .expect("golden has stem")
        .to_string_lossy()
        .into_owned();
    let label = format!("{codec}/{stem}");
    let (vector, expect) = parse_header(golden_path);
    assert!(
        vector.is_file(),
        "{label}: vector missing: {} (run just vectors + just goldens --synth)",
        vector.display()
    );
    let payloads = payloads(&vector);
    let golden = Golden::load(golden_path).expect("golden loads");
    match (decode_all(decoder, &payloads), expect) {
        (Ok(actual), Expect::Match) => {
            let gaps = golden.check(&actual);
            assert!(
                gaps.is_empty(),
                "mismatch {label}: {}",
                gaps.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            println!("{label}: match ({} frames)", actual.len());
        }
        (Ok(actual), Expect::Either(reason)) => {
            let gaps = golden.check(&actual);
            assert!(
                gaps.is_empty(),
                "mismatch {label}: {}",
                gaps.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            println!(
                "{label}: accepted ({} frames; either: {reason})",
                actual.len()
            );
        }
        (Err(err), Expect::Either(reason)) => {
            println!("{label}: declined ({reason}; {err})");
        }
        (Err(err), Expect::Decline(reason)) => {
            println!("{label}: declined as expected ({reason}; {err})");
        }
        (Ok(actual), Expect::Decline(reason)) => {
            panic!(
                "{label}: expected decline ({reason}), decoded {} frames",
                actual.len()
            );
        }
        (Err(err), Expect::Match) => {
            panic!("{label}: expected match, decode failed: {err}");
        }
    }
}

#[cfg(feature = "ffmpeg")]
fn check_codec(codec: &str) -> usize {
    let dir = goldens().join(codec);
    if !dir.is_dir() {
        println!("{codec}: no goldens, skipped");
        return 0;
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sha256"))
        .collect();
    entries.sort();
    for golden_path in &entries {
        match codec {
            "h264" => {
                let mut decoder = H264Decoder::open(4).expect("h264 decoder opens");
                check_one(codec, golden_path, &mut decoder);
            }
            "h265" => {
                let mut decoder = H265Decoder::open(4).expect("h265 decoder opens");
                check_one(codec, golden_path, &mut decoder);
            }
            "vp8" => {
                let mut decoder = Vp8Decoder::open(4).expect("vp8 decoder opens");
                check_one(codec, golden_path, &mut decoder);
            }
            "vp9" => {
                let mut decoder = Vp9Decoder::open(4).expect("vp9 decoder opens");
                check_one(codec, golden_path, &mut decoder);
            }
            "av1" => {
                let mut decoder = Av1Decoder::open(4).expect("av1 decoder opens");
                check_one(codec, golden_path, &mut decoder);
            }
            _ => panic!("unknown codec dir: {codec}"),
        }
    }
    entries.len()
}

#[cfg(feature = "ffmpeg")]
#[test]
fn matrix_matches_goldens() {
    let root = goldens();
    assert!(
        root.is_dir(),
        "goldens absent: {} (run just goldens --synth)",
        root.display()
    );
    let mut checked = 0;
    for codec in ["h264", "h265", "vp8", "vp9", "av1"] {
        checked += check_codec(codec);
    }
    println!("matrix: {checked} lines checked");
    assert!(checked > 0, "no golden checked");
}

#[cfg(all(feature = "h264-openh264", nasm_available))]
fn yuv420p_from_noise(seed: u64) -> CpuFrame {
    let source = Synth::noise(1920, 1080, seed);
    let planes = source.planes();
    let mut u = Vec::with_capacity(planes[1].data().len() / 2);
    let mut v = Vec::with_capacity(planes[1].data().len() / 2);
    for pair in planes[1].data().chunks_exact(2) {
        u.push(pair[0]);
        v.push(pair[1]);
    }
    let size = PictureSize::new(1920, 1080).expect("size");
    let y = Plane::new(Bytes::copy_from_slice(planes[0].data()), 1920).expect("luma");
    let u = Plane::new(Bytes::from(u), 960).expect("chroma u");
    let v = Plane::new(Bytes::from(v), 960).expect("chroma v");
    CpuFrame::new(size, PixelFormat::Yuv420P, vec![y, u, v]).expect("frame")
}

#[cfg(all(feature = "h264-openh264", nasm_available))]
#[test]
fn multislice_output_decodes_to_full_size() {
    let config = EncodeConfig {
        rate_control: RateControl::Cbr {
            bitrate: 5_000_000,
            maxrate: 5_000_000,
            bufsize: 5_000_000,
        },
        frame_rate: FrameRate { num: 60, den: 1 },
        ..Default::default()
    };
    let mut encoder = H264Encoder::open(1920, 1080, config, 4).expect("open");
    let api = openh264::OpenH264API::from_source();
    let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
    let mut decoder = OpenH264Decoder::with_api_config(api, config).expect("decoder");
    let mut frames = 0;
    for seed in 0..3u64 {
        for packet in encoder.encode(&yuv420p_from_noise(seed)).expect("encode") {
            if packet.data().is_empty() {
                continue;
            }
            let options = DecodeOptions::new().flush_after_decode(Flush::NoFlush);
            if let Some(out) = decoder
                .decode_with_options(packet.data(), options)
                .expect("decode")
            {
                assert_eq!(out.dimensions(), (1920, 1080));
                frames += 1;
            }
        }
    }
    assert!(frames >= 1, "at least the IDR decodes");
}
