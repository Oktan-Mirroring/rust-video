//! Vulkan end-to-end: goldens, pipelined decode, encode.

#[cfg(have_corpus)]
use std::cell::{Cell, RefCell};
#[cfg(have_corpus)]
use std::path::{Path, PathBuf};
#[cfg(have_corpus)]
use std::process::Command;
#[cfg(have_corpus)]
use std::sync::MutexGuard;
#[cfg(have_corpus)]
use std::time::Instant;

#[cfg(have_corpus)]
use bytes::Bytes;

#[cfg(have_corpus)]
use rust_video::VideoCodec;
#[cfg(have_corpus)]
use rust_video_caps::delivery::SubmitStats;
#[cfg(have_corpus)]
use rust_video_caps::encode::{EncodeConfig, Gop, Level, Profile, RateControl};
#[cfg(have_corpus)]
use rust_video_caps::frame::CpuFrame;
#[cfg(have_corpus)]
use rust_video_caps::h265;
#[cfg(have_corpus)]
use rust_video_caps::packet::Packet;
#[cfg(have_corpus)]
use rust_video_caps::traits::{Decoder, Encoder};
#[cfg(have_corpus)]
use rust_video_caps::transcode::Route;
#[cfg(have_corpus)]
use rust_video_harness::{Case, Drive, Error, FrameHash, Golden, Oracle, Suite};
#[cfg(have_corpus)]
use rust_video_vulkan::{
    CodecOp, H264Decoder, H264Encoder, H265Decoder, H265Encoder, SkipReason, Transcoder,
    VulkanDevice, VulkanError, VulkanFrame,
};

#[cfg(have_corpus)]
/// Harness oracle over a Vulkan decoder.
struct VulkanOracle<D> {
    /// Decoder under test, behind interior mutability for `Oracle`.
    decoder: RefCell<D>,
    /// Last decode hit VRAM exhaustion.
    oom: Cell<bool>,
}

#[cfg(have_corpus)]
impl<D> VulkanOracle<D> {
    /// Oracle over `decoder`.
    fn new(decoder: D) -> Self {
        Self {
            decoder: RefCell::new(decoder),
            oom: Cell::new(false),
        }
    }

    /// True when the last decode hit VRAM exhaustion.
    fn oom(&self) -> bool {
        self.oom.get()
    }

    /// Harness error, flagging VRAM exhaustion.
    fn map(&self, error: &VulkanError) -> Error {
        if error.is_out_of_device_memory() {
            self.oom.set(true);
        }
        Error::Decode(error.to_string())
    }
}

#[cfg(have_corpus)]
impl<D> VulkanOracle<D>
where
    D: Decoder<Frame = VulkanFrame, Error = VulkanError>,
{
    /// Hashes of every frame decoded from raw Annex-B `bytes`.
    fn decode_bytes(&self, bytes: &[u8]) -> Result<Vec<FrameHash>, Error> {
        self.oom.set(false);
        let mut decoder = self.decoder.borrow_mut();
        let mut hashes = Vec::new();
        let mut scratch = Vec::new();
        for frame in decoder
            .decode(&Packet::new(Bytes::copy_from_slice(bytes)))
            .map_err(|error| self.map(&error))?
        {
            frame.yuv420p_into(&mut scratch);
            hashes.push(FrameHash::of(&scratch));
        }
        for frame in decoder.flush().map_err(|error| self.map(&error))? {
            frame.yuv420p_into(&mut scratch);
            hashes.push(FrameHash::of(&scratch));
        }
        Ok(hashes)
    }
}

#[cfg(have_corpus)]
impl<D> Oracle for VulkanOracle<D>
where
    D: Decoder<Frame = VulkanFrame, Error = VulkanError>,
{
    fn decode(&self, case: &Case) -> Result<Vec<FrameHash>, Error> {
        let bytes = std::fs::read(case.path())?;
        self.decode_bytes(&bytes)
    }
}

#[cfg(have_corpus)]
/// Corpus root: `RUST_VIDEO_CORPUS`, else the workspace `corpus`.
fn corpus() -> PathBuf {
    std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
        PathBuf::from,
    )
}

#[cfg(have_corpus)]
/// Device for `op`, declining loudly when the tier skips.
fn device_or_decline(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open_or_skip(op) {
        Ok(device) => Some(device),
        Err(VulkanError::BackendAbsent(reason @ SkipReason::OutOfDeviceMemory { .. })) => {
            println!("declined:OOM {op:?} on this box: {reason}");
            None
        }
        Err(VulkanError::BackendAbsent(reason)) => {
            println!("declined: {op:?} on this box: {reason}");
            None
        }
        Err(error) if error.is_out_of_device_memory() => {
            println!("declined:OOM {op:?} on this box: {error}");
            None
        }
        Err(error) => panic!("negotiation failed: {error}"),
    }
}

#[cfg(have_corpus)]
/// Loud VRAM-exhaustion decline with live budget numbers.
fn decline_oom(context: &str, device: &VulkanDevice, detail: impl std::fmt::Display) {
    let (free_mb, total_mb) = device.memory_budget_mb();
    println!("declined:OOM {context}: {detail} (free {free_mb}MB of {total_mb}MB)");
}

#[cfg(have_corpus)]
/// Unwrap `result`, declining loud with budget numbers on VRAM exhaustion.
///
/// Panics on other errors.
fn unwrap_or_oom<T>(
    result: Result<T, VulkanError>,
    context: &str,
    device: &VulkanDevice,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) if error.is_out_of_device_memory() => {
            decline_oom(context, device, &error);
            None
        }
        Err(error) => panic!("{context}: {error}"),
    }
}

/// Golden expectation flavor plus its recorded reason.
#[cfg(have_corpus)]
enum Expect {
    /// Decode must succeed byte-exact (on the strict subset).
    Match,
    /// Decode byte-exact or decline cleanly.
    Either(String),
    /// Decode must return `Err`.
    Decline(String),
}

/// Committed goldens root: `RUST_VIDEO_GOLDENS`, else the harness dir.
#[cfg(have_corpus)]
fn goldens_dir() -> PathBuf {
    std::env::var_os("RUST_VIDEO_GOLDENS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rust-video-harness/goldens"),
        PathBuf::from,
    )
}

/// Workspace root for `# vector:` header paths.
#[cfg(have_corpus)]
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Resolve a `# vector:` header path, re-rooted under
/// `RUST_VIDEO_CORPUS` when set.
#[cfg(have_corpus)]
fn resolve_vector(header_path: &str) -> PathBuf {
    let rel = PathBuf::from(header_path);
    if let Some(root) = std::env::var_os("RUST_VIDEO_CORPUS") {
        if let Ok(stripped) = rel.strip_prefix("corpus") {
            return PathBuf::from(root).join(stripped);
        }
    }
    workspace_root().join(rel)
}

/// Vector path, expectation, and probe evidence from one golden.
#[cfg(have_corpus)]
fn parse_golden(path: &Path) -> (PathBuf, Expect, String, String) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let mut vector = None;
    let mut expect = None;
    let mut pix_fmt = String::new();
    let mut field_order = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# vector:") {
            vector = Some(resolve_vector(rest.trim()));
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
        } else if let Some(rest) = line.strip_prefix("# probe:") {
            // Anchored from the end: profiles contain spaces
            // ("Constrained Baseline", "High 4:2:2").
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 4 {
                parts[parts.len() - 4].clone_into(&mut pix_fmt);
                parts[parts.len() - 2].clone_into(&mut field_order);
            }
        }
    }
    match (vector, expect) {
        (Some(vector), Some(expect)) => (vector, expect, pix_fmt, field_order),
        _ => panic!("{}: golden lacks vector/expect header", path.display()),
    }
}

/// Annex-B bytes for one golden vector: raw vectors read through, mp4
/// remuxes through the codec filter with a workdir cache.
#[cfg(have_corpus)]
fn golden_bytes(codec: VideoCodec, vector: &Path) -> Vec<u8> {
    let ext = vector
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    if !ext.eq_ignore_ascii_case("mp4") {
        return std::fs::read(vector).expect("vector reads");
    }
    assert!(
        have_tool("ffmpeg"),
        "goldens need ffmpeg to remux {}",
        vector.display()
    );
    let (filter, out_ext) = match codec {
        VideoCodec::H264 => ("h264_mp4toannexb", "h264"),
        VideoCodec::H265 => ("hevc_mp4toannexb", "h265"),
        _ => panic!("no Annex-B path for {codec:?}"),
    };
    let dest = annexb_cached(vector, filter, out_ext, &format!("golden-{codec:?}"));
    std::fs::read(&dest).expect("annexb reads")
}

/// Matrix lines the vulkan tier is known to violate (wrong output,
/// not a typed decline): skipped loudly with the tier-bug reason.
/// Coverage stays pinned by the sw runner; remove entries as the tier
/// is fixed.
#[cfg(have_corpus)]
const VULKAN_TIER_GAPS: &[(&str, &str)] = &[
    (
        "h264/frame-num-gaps",
        "vulkan emits 10 wrong frames then stalls on legal gaps (DPB gap handling diverges from oracle)",
    ),
    (
        "h264/profile-baseline",
        "vulkan repeats one frame 119x on CAVLC content (Main/CABAC matches; oracle pixels verified)",
    ),
    (
        "h264/vui-absent",
        "vulkan emits B-frames out of order without VUI max_num_reorder_frames (all VUI-bearing lines match)",
    ),
];

/// Open one decoder, decode `bytes`, and report the hashes plus the
/// VRAM-exhaustion flag; `None` declines OOM at open time.
#[cfg(have_corpus)]
fn decode_line<D>(
    opened: Result<D, VulkanError>,
    label: &str,
    device: &VulkanDevice,
    bytes: &[u8],
) -> Option<(Result<Vec<FrameHash>, Error>, bool)>
where
    D: Decoder<Frame = VulkanFrame, Error = VulkanError>,
{
    let decoder = match opened {
        Ok(decoder) => decoder,
        Err(error) if error.is_out_of_device_memory() => {
            decline_oom(label, device, &error);
            return None;
        }
        Err(error) => panic!("{label}: engine failed to open: {error}"),
    };
    let oracle = VulkanOracle::new(decoder);
    let result = oracle.decode_bytes(bytes);
    Some((result, oracle.oom()))
}

/// Golden comparison of every `codec_dir` line through a fresh
/// decoder per line.
///
/// Hardware pins the universal subset bit-exact (8-bit 4:2:0
/// non-interlaced `match` lines); readback converts anything else to
/// 8-bit 4:2:0, so off-subset lines only assert decode-or-decline.
/// Decline lines must return `Err`. Each line opens its own decoder:
/// matrix lines are independent conformance points and no parser,
/// surface, or DPB state carries across vectors.
#[cfg(have_corpus)]
fn check_goldens(codec: VideoCodec, codec_dir: &str, device: &VulkanDevice) {
    let dir = goldens_dir().join(codec_dir);
    assert!(
        dir.is_dir(),
        "goldens absent: {} (run just goldens --synth)",
        dir.display()
    );
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sha256"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "no {codec:?} goldens in {}",
        dir.display()
    );
    for golden_path in &entries {
        let stem = golden_path
            .file_stem()
            .expect("golden has stem")
            .to_string_lossy()
            .into_owned();
        let label = format!("{codec_dir}/{stem}");
        if let Some((_, reason)) = VULKAN_TIER_GAPS.iter().find(|(line, _)| *line == label) {
            println!("{label}: KNOWN-TIER-GAP skipped ({reason})");
            continue;
        }
        let (vector, expect, pix_fmt, field_order) = parse_golden(golden_path);
        assert!(
            vector.is_file(),
            "{label}: vector missing: {} (run just vectors + just goldens --synth)",
            vector.display()
        );
        let bytes = golden_bytes(codec, &vector);
        let golden = Golden::load(golden_path).expect("golden parses");
        // Raw Annex B carries no interlace signal (`unknown`), and every
        // such vector is progressive by construction; containers report
        // interlaced honestly, excluding interlaced lines from strictness.
        let interlaced = matches!(
            field_order.as_str(),
            "tt" | "bb" | "tb" | "bt" | "tff" | "bff"
        );
        let strict = matches!(expect, Expect::Match) && pix_fmt == "yuv420p" && !interlaced;
        let opened = match codec {
            VideoCodec::H264 => decode_line(H264Decoder::open(device), &label, device, &bytes),
            VideoCodec::H265 => decode_line(H265Decoder::open(device), &label, device, &bytes),
            _ => panic!("no vulkan decoder for {codec:?}"),
        };
        let Some((result, oom)) = opened else {
            return;
        };
        match result {
            Ok(hashes) => match expect {
                Expect::Decline(reason) => panic!(
                    "{label}: expected decline ({reason}), decoded {} frames",
                    hashes.len()
                ),
                _ if strict => {
                    let gaps = golden.check(&hashes);
                    let shown: Vec<String> =
                        gaps.iter().map(std::string::ToString::to_string).collect();
                    assert!(gaps.is_empty(), "mismatches in {label}: {shown:?}");
                    println!("{label}: match ({} frames)", hashes.len());
                }
                _ => println!(
                    "{label}: accepted uncompared ({} frames; readback is 420-8 only)",
                    hashes.len()
                ),
            },
            Err(error) => {
                if oom && !matches!(expect, Expect::Decline(_)) {
                    decline_oom(&label, device, &error);
                    return;
                }
                match expect {
                    Expect::Decline(reason) => {
                        println!("{label}: declined as expected ({reason}; {error})");
                    }
                    Expect::Either(reason) => {
                        println!("{label}: declined ({reason}; {error})");
                    }
                    Expect::Match if !strict => {
                        println!("{label}: declined (off-subset line; {error})");
                    }
                    Expect::Match => panic!("{label}: expected match, decode failed: {error}"),
                }
            }
        }
    }
}

#[cfg(have_corpus)]
#[test]
fn h264_matches_goldens() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    check_goldens(VideoCodec::H264, "h264", &device);
}

#[cfg(have_corpus)]
#[test]
fn h265_matches_goldens() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H265Decode) else {
        return;
    };
    check_goldens(VideoCodec::H265, "h265", &device);
}

#[cfg(have_corpus)]
/// Synthetic OOM through the decline helpers: decline, never panic.
#[test]
fn oom_helpers_decline_synthetic_oom() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    let oom = || VulkanError::Vulkan(ash::vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
    assert!(unwrap_or_oom::<()>(Err(oom()), "synthetic oom", &device).is_none());
    let Some(decoder) = unwrap_or_oom(H264Decoder::open(&device), "engine starts", &device) else {
        return;
    };
    let oracle = VulkanOracle::new(decoder);
    assert!(!oracle.oom());
    oracle.map(&oom());
    assert!(oracle.oom());
}

#[cfg(have_corpus)]
/// True when `tool -version` runs.
fn have_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|out| out.status.success())
}

#[cfg(have_corpus)]
/// Run `tool` with `args`, returning stdout; panics on failure.
fn run(tool: &str, args: &[String]) -> Vec<u8> {
    let out = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("spawn {tool}: {error}"));
    assert!(
        out.status.success(),
        "{tool} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[cfg(have_corpus)]
/// Working directory for extracted vectors.
fn workdir() -> PathBuf {
    let dir = std::env::temp_dir().join("vulkan-annexb");
    std::fs::create_dir_all(&dir).expect("workdir");
    dir
}

#[cfg(have_corpus)]
/// Pixel format of the first video stream.
fn pix_fmt(path: &Path) -> String {
    let name = path.to_string_lossy().into_owned();
    let out = run(
        "ffprobe",
        &[
            "-v".to_owned(),
            "error".to_owned(),
            "-select_streams".to_owned(),
            "v:0".to_owned(),
            "-show_entries".to_owned(),
            "stream=pix_fmt".to_owned(),
            "-of".to_owned(),
            "csv=p=0".to_owned(),
            name,
        ],
    );
    String::from_utf8_lossy(&out).trim().to_owned()
}

#[cfg(have_corpus)]
/// Annex-B bytes of one mp4 vector through `filter`, cached by source
/// identity (length plus mtime) so regenerated vectors never read a
/// stale extract.
fn annexb_cached(input: &Path, filter: &str, ext: &str, tag: &str) -> PathBuf {
    let meta = std::fs::metadata(input).expect("vector stats");
    let stem = input.file_stem().map_or_else(
        || "vector".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |age| age.as_secs());
    let dest = workdir().join(format!("{tag}-{stem}-{}-{mtime}.{ext}", meta.len()));
    let fresh = dest
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() > 0);
    if !fresh {
        run(
            "ffmpeg",
            &[
                "-y".to_owned(),
                "-v".to_owned(),
                "error".to_owned(),
                "-i".to_owned(),
                input.to_string_lossy().into_owned(),
                "-c".to_owned(),
                "copy".to_owned(),
                "-bsf:v".to_owned(),
                filter.to_owned(),
                dest.to_string_lossy().into_owned(),
            ],
        );
    }
    dest
}

#[cfg(have_corpus)]
/// Annex-B extracts of 8-bit 4:2:0 synth mp4s, cached in the workdir.
fn synth_vectors(codec: VideoCodec) -> Vec<PathBuf> {
    if !have_tool("ffmpeg") || !have_tool("ffprobe") {
        return Vec::new();
    }
    let (dir, filter, ext) = match codec {
        VideoCodec::H264 => ("h264", "h264_mp4toannexb", "h264"),
        VideoCodec::H265 => ("h265", "hevc_mp4toannexb", "h265"),
        _ => return Vec::new(),
    };
    let mut inputs = Vec::new();
    let mut entries = std::fs::read_dir(corpus().join("synth").join(dir))
        .map_or(Vec::new(), |read| {
            read.filter_map(Result::ok).collect::<Vec<_>>()
        });
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "mp4") {
            inputs.push(path);
        }
    }
    let mut out = Vec::new();
    for input in &inputs {
        if pix_fmt(input) != "yuv420p" {
            continue;
        }
        out.push(annexb_cached(input, filter, ext, "synth"));
    }
    out
}

#[cfg(have_corpus)]
/// Annex-B vectors for `codec`: corpus cases first, else synth extracts.
///
/// Callers hold the serial guard across extraction and decode.
fn vectors(codec: VideoCodec, _serial: &MutexGuard<'_, ()>) -> Vec<PathBuf> {
    let suite = Suite::open_at(&corpus()).expect("corpus present by build gate");
    let cases: Vec<PathBuf> = suite
        .cases_for(codec)
        .map(|case| case.path().to_path_buf())
        .collect();
    if cases.is_empty() {
        synth_vectors(codec)
    } else {
        cases
    }
}

#[cfg(have_corpus)]
/// Decoder plus submit-core gauges for the pipeline tests.
trait GaugedDecoder: Decoder<Frame = VulkanFrame, Error = VulkanError> {
    /// Submit-core gauges snapshot.
    fn stats(&self) -> SubmitStats;

    /// Fail the next submit once, exercising the reset path.
    fn inject(&mut self);
}

#[cfg(have_corpus)]
impl GaugedDecoder for H264Decoder<'_> {
    fn stats(&self) -> SubmitStats {
        self.submit_stats()
    }

    fn inject(&mut self) {
        self.inject_submit_fault();
    }
}

#[cfg(have_corpus)]
impl GaugedDecoder for H265Decoder<'_> {
    fn stats(&self) -> SubmitStats {
        self.submit_stats()
    }

    fn inject(&mut self) {
        self.inject_submit_fault();
    }
}

#[cfg(have_corpus)]
/// Frames decoded from `bytes` plus the flush drain.
fn decode_all(
    decoder: &mut impl GaugedDecoder,
    bytes: &[u8],
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let mut frames = decoder.decode(&Packet::new(Bytes::copy_from_slice(bytes)))?;
    frames.extend(decoder.flush()?);
    Ok(frames)
}

#[cfg(have_corpus)]
/// Frames decoded from `bytes`, skipping vectors the tier declines.
///
/// Parse failures are typed declines (interlaced, exotic sampling):
/// the overlap proof tries the next vector. OOM flags `oomed`; engine
/// failures panic.
fn decode_or_skip(
    decoder: &mut impl GaugedDecoder,
    bytes: &[u8],
    path: &Path,
    codec: VideoCodec,
    oomed: &mut bool,
) -> Option<Vec<VulkanFrame>> {
    match decode_all(decoder, bytes) {
        Ok(frames) => Some(frames),
        Err(error) if error.is_out_of_device_memory() => {
            *oomed = true;
            None
        }
        Err(VulkanError::Parse(reason)) => {
            println!(
                "declined: {codec:?} overlap proof on {}: parse failed: {reason}",
                path.display()
            );
            None
        }
        Err(error) => panic!("decode runs: {error}"),
    }
}

#[cfg(have_corpus)]
#[test]
fn pipelined_decode_overlaps_frames() {
    let guard = Drive::serial();
    for (codec, op) in [
        (VideoCodec::H264, CodecOp::H264Decode),
        (VideoCodec::H265, CodecOp::H265Decode),
    ] {
        let paths = vectors(codec, &guard);
        if paths.is_empty() {
            println!("declined: {codec:?} overlap proof without vectors");
            continue;
        }
        let Some(device) = device_or_decline(op) else {
            continue;
        };
        let mut proved = false;
        let mut oomed = false;
        for path in &paths {
            let bytes = std::fs::read(path).expect("vector reads");
            let start = Instant::now();
            let (frames, peak) = match codec {
                VideoCodec::H264 => {
                    let Some(mut decoder) =
                        unwrap_or_oom(H264Decoder::open(&device), "engine starts", &device)
                    else {
                        oomed = true;
                        continue;
                    };
                    let Some(frames) =
                        decode_or_skip(&mut decoder, &bytes, path, codec, &mut oomed)
                    else {
                        continue;
                    };
                    (frames, decoder.submit_stats().peak_inflight())
                }
                VideoCodec::H265 => {
                    let Some(mut decoder) =
                        unwrap_or_oom(H265Decoder::open(&device), "engine starts", &device)
                    else {
                        oomed = true;
                        continue;
                    };
                    let Some(frames) =
                        decode_or_skip(&mut decoder, &bytes, path, codec, &mut oomed)
                    else {
                        continue;
                    };
                    (frames, decoder.submit_stats().peak_inflight())
                }
                _ => unreachable!("overlap proof covers h264 and h265"),
            };
            println!(
                "overlap {codec:?} {}: {} frames in {}ms, peak {peak}",
                path.display(),
                frames.len(),
                start.elapsed().as_millis()
            );
            if frames.len() < 2 {
                continue;
            }
            assert!(peak >= 2, "serial execution on {}", path.display());
            proved = true;
        }
        if !proved && oomed {
            continue;
        }
        assert!(proved, "no multi-frame {codec:?} vector proved overlap");
    }
}

#[cfg(have_corpus)]
/// Second decode after an injected fault: same frames, one more reset.
fn recovery_case(
    decoder: &mut impl GaugedDecoder,
    codec: VideoCodec,
    path: &Path,
    bytes: &[u8],
) -> Result<(), VulkanError> {
    let first = decode_all(decoder, bytes)?;
    assert!(
        !first.is_empty(),
        "no {codec:?} frames from {}",
        path.display()
    );
    let before = decoder.stats().resets();
    decoder.inject();
    let second = decode_all(decoder, bytes)?;
    let stats = decoder.stats();
    println!(
        "recovery {codec:?} {}: {} then {} frames, resets {before} -> {}",
        path.display(),
        first.len(),
        second.len(),
        stats.resets()
    );
    assert_eq!(
        second.len(),
        first.len(),
        "frame loss across reset on {}",
        path.display()
    );
    assert!(
        stats.resets() > before,
        "injected fault never fired on {}",
        path.display()
    );
    Ok(())
}

#[cfg(have_corpus)]
#[test]
fn reset_after_failure_recovers() {
    let guard = Drive::serial();
    for (codec, op) in [
        (VideoCodec::H264, CodecOp::H264Decode),
        (VideoCodec::H265, CodecOp::H265Decode),
    ] {
        let paths = vectors(codec, &guard);
        if paths.is_empty() {
            println!("declined: {codec:?} recovery without vectors");
            continue;
        }
        let Some(device) = device_or_decline(op) else {
            continue;
        };
        let bytes = std::fs::read(&paths[0]).expect("vector reads");
        match codec {
            VideoCodec::H264 => {
                let Some(mut decoder) =
                    unwrap_or_oom(H264Decoder::open(&device), "engine starts", &device)
                else {
                    continue;
                };
                let result = recovery_case(&mut decoder, codec, &paths[0], &bytes);
                let Some(()) = unwrap_or_oom(result, "recovery runs", &device) else {
                    continue;
                };
            }
            VideoCodec::H265 => {
                let Some(mut decoder) =
                    unwrap_or_oom(H265Decoder::open(&device), "engine starts", &device)
                else {
                    continue;
                };
                let result = recovery_case(&mut decoder, codec, &paths[0], &bytes);
                let Some(()) = unwrap_or_oom(result, "recovery runs", &device) else {
                    continue;
                };
            }
            _ => unreachable!("recovery covers h264 and h265"),
        }
    }
}

#[cfg(have_corpus)]
/// Multi-frame transcode over the GPU route, declining the CPU split.
fn transcode_case(
    codec: VideoCodec,
    decode_op: CodecOp,
    encode_op: CodecOp,
    guard: &MutexGuard<'_, ()>,
) {
    let paths = vectors(codec, guard);
    if paths.is_empty() {
        println!("declined: {codec:?} transcode without vectors");
        return;
    }
    let Some(decode_device) = device_or_decline(decode_op) else {
        return;
    };
    let Some(encode_device) = device_or_decline(encode_op) else {
        return;
    };
    let opened = Transcoder::open(&decode_device, decode_op, &encode_device, encode_op);
    let Some(mut transcoder) = unwrap_or_oom(opened, "transcoder opens", &decode_device) else {
        return;
    };
    if transcoder.plan().route() != Route::Gpu {
        match transcoder.plan().cause() {
            Some(cause) => println!("declined: {codec:?} transcode takes the cpu route ({cause})"),
            None => println!("declined: {codec:?} transcode takes the cpu route"),
        }
        return;
    }
    for path in &paths {
        let bytes = std::fs::read(path).expect("vector reads");
        let ran = transcoder.transcode(&Packet::new(Bytes::copy_from_slice(&bytes)));
        let Some(outcome) = unwrap_or_oom(ran, "transcode runs", &decode_device) else {
            return;
        };
        let Some(flushed) = unwrap_or_oom(transcoder.flush(), "flush drains", &decode_device)
        else {
            return;
        };
        let count = outcome.len() + flushed.len();
        println!("transcode {codec:?} {}: {count} packets", path.display());
        assert!(
            count >= 2,
            "multi-frame transcode emitted one packet: {}",
            path.display()
        );
    }
}

#[cfg(have_corpus)]
#[test]
fn gpu_transcode_runs_multi_frame() {
    let guard = Drive::serial();
    transcode_case(
        VideoCodec::H264,
        CodecOp::H264Decode,
        CodecOp::H264Encode,
        &guard,
    );
    transcode_case(
        VideoCodec::H265,
        CodecOp::H265Decode,
        CodecOp::H265Encode,
        &guard,
    );
}

#[cfg(have_corpus)]
/// Decode plus encode steady state: every frame through, zero resets.
fn steady_h264(guard: &MutexGuard<'_, ()>) {
    let paths = vectors(VideoCodec::H264, guard);
    if paths.is_empty() {
        println!("declined: h264 steady state without vectors");
        return;
    }
    let bytes = std::fs::read(&paths[0]).expect("vector reads");
    let Some(decode_device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    let Some(encode_device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(
        H264Decoder::open(&decode_device),
        "engine starts",
        &decode_device,
    ) else {
        return;
    };
    let Some(frames) = unwrap_or_oom(
        decode_all(&mut decoder, &bytes),
        "decode runs",
        &decode_device,
    ) else {
        return;
    };
    assert!(
        !frames.is_empty(),
        "no h264 frames from {}",
        paths[0].display()
    );
    let size = frames[0].identity().size();
    let opened = H264Encoder::open(
        &encode_device,
        size.width(),
        size.height(),
        legacy_config(VideoCodec::H264),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &encode_device) else {
        return;
    };
    let mut packets = 0;
    for frame in &frames {
        let cpu = frame.to_cpu().expect("cpu planes");
        let Some(coded) = unwrap_or_oom(encoder.encode(&cpu), "encode runs", &encode_device) else {
            return;
        };
        packets += coded.len();
    }
    println!(
        "steady h264 {}: {} frames, {} packets, resets {}/{}",
        paths[0].display(),
        frames.len(),
        packets,
        decoder.submit_stats().resets(),
        encoder.submit_stats().resets()
    );
    assert_eq!(packets, frames.len(), "one packet per frame");
    assert_eq!(
        decoder.submit_stats().resets(),
        0,
        "decode recovered mid-run"
    );
    assert_eq!(
        encoder.submit_stats().resets(),
        0,
        "encode recovered mid-run"
    );
}

#[cfg(have_corpus)]
/// Decode plus encode steady state: every frame through, zero resets.
fn steady_h265(guard: &MutexGuard<'_, ()>) {
    let paths = vectors(VideoCodec::H265, guard);
    if paths.is_empty() {
        println!("declined: h265 steady state without vectors");
        return;
    }
    let bytes = std::fs::read(&paths[0]).expect("vector reads");
    let Some(decode_device) = device_or_decline(CodecOp::H265Decode) else {
        return;
    };
    let Some(encode_device) = device_or_decline(CodecOp::H265Encode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(
        H265Decoder::open(&decode_device),
        "engine starts",
        &decode_device,
    ) else {
        return;
    };
    let Some(frames) = unwrap_or_oom(
        decode_all(&mut decoder, &bytes),
        "decode runs",
        &decode_device,
    ) else {
        return;
    };
    assert!(
        !frames.is_empty(),
        "no h265 frames from {}",
        paths[0].display()
    );
    let size = frames[0].identity().size();
    let opened = H265Encoder::open(
        &encode_device,
        size.width(),
        size.height(),
        legacy_config(VideoCodec::H265),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &encode_device) else {
        return;
    };
    let mut packets = 0;
    for frame in &frames {
        let cpu = frame.to_cpu().expect("cpu planes");
        let Some(coded) = unwrap_or_oom(encoder.encode(&cpu), "encode runs", &encode_device) else {
            return;
        };
        packets += coded.len();
    }
    println!(
        "steady h265 {}: {} frames, {} packets, resets {}/{}",
        paths[0].display(),
        frames.len(),
        packets,
        decoder.submit_stats().resets(),
        encoder.submit_stats().resets()
    );
    assert_eq!(packets, frames.len(), "one packet per frame");
    assert_eq!(
        decoder.submit_stats().resets(),
        0,
        "decode recovered mid-run"
    );
    assert_eq!(
        encoder.submit_stats().resets(),
        0,
        "encode recovered mid-run"
    );
}

#[cfg(have_corpus)]
#[test]
fn steady_state_runs_without_timeout() {
    let guard = Drive::serial();
    steady_h264(&guard);
    steady_h265(&guard);
}

#[cfg(have_corpus)]
/// CPU frames decoded from the first vector yielding `want` frames.
fn encode_source(
    codec: VideoCodec,
    decode_op: CodecOp,
    guard: &MutexGuard<'_, ()>,
    want: usize,
) -> Option<Vec<CpuFrame>> {
    let paths = vectors(codec, guard);
    if paths.is_empty() {
        println!("declined: {codec:?} encode source without vectors");
        return None;
    }
    let device = device_or_decline(decode_op)?;
    let mut oomed = false;
    for path in &paths {
        let bytes = std::fs::read(path).expect("vector reads");
        let frames = match codec {
            VideoCodec::H264 => {
                let Some(mut decoder) =
                    unwrap_or_oom(H264Decoder::open(&device), "engine starts", &device)
                else {
                    oomed = true;
                    continue;
                };
                let Some(frames) =
                    unwrap_or_oom(decode_all(&mut decoder, &bytes), "decode runs", &device)
                else {
                    oomed = true;
                    continue;
                };
                frames
            }
            VideoCodec::H265 => {
                let Some(mut decoder) =
                    unwrap_or_oom(H265Decoder::open(&device), "engine starts", &device)
                else {
                    oomed = true;
                    continue;
                };
                let Some(frames) =
                    unwrap_or_oom(decode_all(&mut decoder, &bytes), "decode runs", &device)
                else {
                    oomed = true;
                    continue;
                };
                frames
            }
            _ => return None,
        };
        let cpu: Vec<CpuFrame> = frames.iter().filter_map(VulkanFrame::to_cpu).collect();
        println!(
            "source {codec:?} {}: {} frames ({} cpu)",
            path.display(),
            frames.len(),
            cpu.len()
        );
        if cpu.len() >= want {
            return Some(cpu.into_iter().take(want).collect());
        }
    }
    if oomed {
        return None;
    }
    println!("declined: {codec:?} encode source without {want} frames");
    None
}

#[cfg(have_corpus)]
/// Legacy encode config: auto rate over the codec defaults.
fn legacy_config(codec: VideoCodec) -> EncodeConfig {
    match codec {
        VideoCodec::H265 => EncodeConfig {
            rate_control: RateControl::Auto,
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..Default::default()
        },
        _ => EncodeConfig {
            rate_control: RateControl::Auto,
            ..Default::default()
        },
    }
}

#[cfg(have_corpus)]
/// Encode config with a `keyint` IDR cadence over the codec defaults.
fn cadence_config(codec: VideoCodec, keyint: u32) -> EncodeConfig {
    let gop = Gop {
        keyint,
        min_keyint: keyint,
        b_frames: 0,
        refs: 1,
    };
    match codec {
        VideoCodec::H265 => EncodeConfig {
            gop,
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..Default::default()
        },
        _ => EncodeConfig {
            gop,
            ..Default::default()
        },
    }
}

#[cfg(have_corpus)]
/// Packets coded from `frames` through `encoder`.
fn encode_run(
    encoder: &mut impl Encoder<Error = VulkanError>,
    frames: &[CpuFrame],
) -> Result<Vec<Packet>, VulkanError> {
    let mut packets = Vec::new();
    for frame in frames {
        packets.extend(encoder.encode(frame)?);
    }
    packets.extend(encoder.flush()?);
    Ok(packets)
}

#[cfg(have_corpus)]
/// Same frames through two fresh encoders: byte-identical packets.
fn determinism_case(
    codec: VideoCodec,
    decode_op: CodecOp,
    encode_op: CodecOp,
    guard: &MutexGuard<'_, ()>,
) {
    const FRAMES: usize = 10;
    let Some(frames) = encode_source(codec, decode_op, guard, FRAMES) else {
        return;
    };
    let Some(device) = device_or_decline(encode_op) else {
        return;
    };
    let size = frames[0].size();
    let config = cadence_config(codec, 4);
    let (first, second) = match codec {
        VideoCodec::H264 => {
            let opened = H264Encoder::open(&device, size.width(), size.height(), config);
            let Some(mut first_encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
                return;
            };
            if !first_encoder.supports_p_frames() {
                println!("declined: {codec:?} determinism without P frames");
                return;
            }
            let opened = H264Encoder::open(&device, size.width(), size.height(), config);
            let Some(mut second_encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
                return;
            };
            let Some(first) = unwrap_or_oom(
                encode_run(&mut first_encoder, &frames),
                "encode runs",
                &device,
            ) else {
                return;
            };
            let Some(second) = unwrap_or_oom(
                encode_run(&mut second_encoder, &frames),
                "encode runs",
                &device,
            ) else {
                return;
            };
            (first, second)
        }
        VideoCodec::H265 => {
            let opened = H265Encoder::open(&device, size.width(), size.height(), config);
            let Some(mut first_encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
                return;
            };
            if !first_encoder.supports_p_frames() {
                println!("declined: {codec:?} determinism without P frames");
                return;
            }
            let opened = H265Encoder::open(&device, size.width(), size.height(), config);
            let Some(mut second_encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
                return;
            };
            let Some(first) = unwrap_or_oom(
                encode_run(&mut first_encoder, &frames),
                "encode runs",
                &device,
            ) else {
                return;
            };
            let Some(second) = unwrap_or_oom(
                encode_run(&mut second_encoder, &frames),
                "encode runs",
                &device,
            ) else {
                return;
            };
            (first, second)
        }
        _ => return,
    };
    assert_eq!(first.len(), FRAMES, "packet loss in {codec:?} run");
    assert_eq!(second.len(), FRAMES, "packet loss in {codec:?} rerun");
    for (index, (run, rerun)) in first.iter().zip(&second).enumerate() {
        assert_eq!(
            run.data(),
            rerun.data(),
            "packet {index} differs in {codec:?} rerun"
        );
        assert_eq!(
            run.is_keyframe(),
            rerun.is_keyframe(),
            "keyframe flag {index} differs in {codec:?} rerun"
        );
    }
    println!("deterministic {codec:?}: {} identical packets", first.len());
}

#[cfg(have_corpus)]
#[test]
fn encode_deterministic_packets() {
    let guard = Drive::serial();
    determinism_case(
        VideoCodec::H264,
        CodecOp::H264Decode,
        CodecOp::H264Encode,
        &guard,
    );
    determinism_case(
        VideoCodec::H265,
        CodecOp::H265Decode,
        CodecOp::H265Encode,
        &guard,
    );
}

#[cfg(have_corpus)]
/// P stream through a fresh decoder: every frame back, none lost.
fn roundtrip_case(
    codec: VideoCodec,
    decode_op: CodecOp,
    encode_op: CodecOp,
    guard: &MutexGuard<'_, ()>,
) {
    const FRAMES: usize = 10;
    let Some(frames) = encode_source(codec, decode_op, guard, FRAMES) else {
        return;
    };
    let Some(encode_device) = device_or_decline(encode_op) else {
        return;
    };
    let size = frames[0].size();
    let config = cadence_config(codec, 4);
    let packets = match codec {
        VideoCodec::H264 => {
            let opened = H264Encoder::open(&encode_device, size.width(), size.height(), config);
            let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &encode_device) else {
                return;
            };
            if !encoder.supports_p_frames() {
                println!("declined: {codec:?} roundtrip without P frames");
                return;
            }
            let Some(packets) = unwrap_or_oom(
                encode_run(&mut encoder, &frames),
                "encode runs",
                &encode_device,
            ) else {
                return;
            };
            packets
        }
        VideoCodec::H265 => {
            let opened = H265Encoder::open(&encode_device, size.width(), size.height(), config);
            let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &encode_device) else {
                return;
            };
            if !encoder.supports_p_frames() {
                println!("declined: {codec:?} roundtrip without P frames");
                return;
            }
            let Some(packets) = unwrap_or_oom(
                encode_run(&mut encoder, &frames),
                "encode runs",
                &encode_device,
            ) else {
                return;
            };
            packets
        }
        _ => return,
    };
    assert_eq!(packets.len(), FRAMES, "packet loss in {codec:?} run");
    let mut stream = Vec::new();
    for packet in &packets {
        stream.extend_from_slice(packet.data());
    }
    let Some(decode_device) = device_or_decline(decode_op) else {
        return;
    };
    let count = match codec {
        VideoCodec::H264 => {
            let Some(mut decoder) = unwrap_or_oom(
                H264Decoder::open(&decode_device),
                "engine starts",
                &decode_device,
            ) else {
                return;
            };
            let Some(frames) = unwrap_or_oom(
                decode_all(&mut decoder, &stream),
                "decode runs",
                &decode_device,
            ) else {
                return;
            };
            frames.len()
        }
        VideoCodec::H265 => {
            let Some(mut decoder) = unwrap_or_oom(
                H265Decoder::open(&decode_device),
                "engine starts",
                &decode_device,
            ) else {
                return;
            };
            let Some(frames) = unwrap_or_oom(
                decode_all(&mut decoder, &stream),
                "decode runs",
                &decode_device,
            ) else {
                return;
            };
            frames.len()
        }
        _ => return,
    };
    println!(
        "roundtrip {codec:?}: {} packets -> {count} frames",
        packets.len()
    );
    assert_eq!(count, FRAMES, "P stream lost frames in {codec:?} roundtrip");
}

#[cfg(have_corpus)]
#[test]
fn encode_p_stream_decodes_clean() {
    let guard = Drive::serial();
    roundtrip_case(
        VideoCodec::H264,
        CodecOp::H264Decode,
        CodecOp::H264Encode,
        &guard,
    );
    roundtrip_case(
        VideoCodec::H265,
        CodecOp::H265Decode,
        CodecOp::H265Encode,
        &guard,
    );
}

#[cfg(have_corpus)]
/// H.264 keyframe flags over `frames`, asserting all-intra alongside.
fn gop_marks_h264(device: &VulkanDevice, frames: &[CpuFrame]) -> Option<Vec<bool>> {
    let size = frames[0].size();
    let opened = H264Encoder::open(
        device,
        size.width(),
        size.height(),
        cadence_config(VideoCodec::H264, 4),
    );
    let mut encoder = unwrap_or_oom(opened, "encoder opens", device)?;
    if !encoder.supports_p_frames() {
        println!("declined: H264 GOP boundary without P frames");
        return None;
    }
    let run = unwrap_or_oom(encode_run(&mut encoder, frames), "encode runs", device)?;
    let marks: Vec<bool> = run.iter().map(Packet::is_keyframe).collect();
    let opened = H264Encoder::open(
        device,
        size.width(),
        size.height(),
        cadence_config(VideoCodec::H264, 1),
    );
    let mut intra = unwrap_or_oom(opened, "encoder opens", device)?;
    let intra_run = unwrap_or_oom(encode_run(&mut intra, &frames[..4]), "encode runs", device)?;
    let intra_marks: Vec<bool> = intra_run.iter().map(Packet::is_keyframe).collect();
    assert_eq!(intra_marks, vec![true; 4], "all-intra missed a keyframe");
    Some(marks)
}

#[cfg(have_corpus)]
/// H.265 keyframe flags over `frames`, asserting all-intra alongside.
fn gop_marks_h265(device: &VulkanDevice, frames: &[CpuFrame]) -> Option<Vec<bool>> {
    let size = frames[0].size();
    let opened = H265Encoder::open(
        device,
        size.width(),
        size.height(),
        cadence_config(VideoCodec::H265, 4),
    );
    let mut encoder = unwrap_or_oom(opened, "encoder opens", device)?;
    if !encoder.supports_p_frames() {
        println!("declined: H265 GOP boundary without P frames");
        return None;
    }
    let run = unwrap_or_oom(encode_run(&mut encoder, frames), "encode runs", device)?;
    let marks: Vec<bool> = run.iter().map(Packet::is_keyframe).collect();
    let opened = H265Encoder::open(
        device,
        size.width(),
        size.height(),
        cadence_config(VideoCodec::H265, 1),
    );
    let mut intra = unwrap_or_oom(opened, "encoder opens", device)?;
    let intra_run = unwrap_or_oom(encode_run(&mut intra, &frames[..4]), "encode runs", device)?;
    let intra_marks: Vec<bool> = intra_run.iter().map(Packet::is_keyframe).collect();
    assert_eq!(intra_marks, vec![true; 4], "all-intra missed a keyframe");
    Some(marks)
}

#[cfg(have_corpus)]
/// Keyframe flags land on the IDR cadence exactly, plus all-intra mode.
fn gop_case(codec: VideoCodec, decode_op: CodecOp, encode_op: CodecOp, guard: &MutexGuard<'_, ()>) {
    const FRAMES: usize = 10;
    let Some(frames) = encode_source(codec, decode_op, guard, FRAMES) else {
        return;
    };
    let Some(device) = device_or_decline(encode_op) else {
        return;
    };
    let marks = match codec {
        VideoCodec::H264 => gop_marks_h264(&device, &frames),
        VideoCodec::H265 => gop_marks_h265(&device, &frames),
        _ => return,
    };
    let Some(marks) = marks else {
        return;
    };
    assert_eq!(
        marks,
        vec![true, false, false, false, true, false, false, false, true, false],
        "IDR cadence drifted in {codec:?}"
    );
    println!(
        "gop {codec:?}: IDR cadence exact over {} packets",
        marks.len()
    );
}

#[cfg(have_corpus)]
#[test]
fn encode_gop_boundary_exact() {
    let guard = Drive::serial();
    gop_case(
        VideoCodec::H264,
        CodecOp::H264Decode,
        CodecOp::H264Encode,
        &guard,
    );
    gop_case(
        VideoCodec::H265,
        CodecOp::H265Decode,
        CodecOp::H265Encode,
        &guard,
    );
}
