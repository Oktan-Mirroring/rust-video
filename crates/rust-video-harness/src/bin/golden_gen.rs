//! Golden generator over the vector corpus.
//!
//! ffmpeg oracle: probe, decode, hash, cover. Coverage rules live in
//! `docs/testing/goldens.md`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use rust_video_harness::{AnnexB, BitReader, Bits, FrameHash};

/// Canonical frames per clip; every matrix vector keeps all of them.
const CLIP_FRAMES: u32 = 120;

/// Motion clip lavfi source: 120f 1080p30 moving pattern.
const MOTION_SRC: &str = "testsrc=s=1920x1080:r=30:d=4";
/// Noise clip lavfi source: 120f 1080p30 adversarial noise.
const NOISE_SRC: &str = "nullsrc=s=1920x1080:r=30:d=4,geq=random(1)*255";

/// One canonical clip.
struct Clip {
    /// `motion` or `noise`.
    name: &'static str,
    /// lavfi source graph (before scale/format).
    src: &'static str,
}

const CLIPS: [Clip; 2] = [
    Clip {
        name: "motion",
        src: MOTION_SRC,
    },
    Clip {
        name: "noise",
        src: NOISE_SRC,
    },
];

/// Golden expectation flavor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Tier must decode byte-exact.
    Match,
    /// Tier decodes byte-exact or cleanly declines (baseline
    /// accept-or-decline lines); reason recorded.
    Either(&'static str),
    /// Tier must return `Err`; golden carries zero hashes.
    Decline(&'static str),
}

/// Byte-level derivation of one synth output into another vector.
#[derive(Clone, Copy)]
enum Derive {
    /// Remux through a bitstream filter (`-bsf:v <filter>`).
    Bsf(&'static str),
    /// Extract raw Annex B with `h264_mp4toannexb` / `hevc_mp4toannexb`.
    AnnexB(&'static str),
    /// Deterministic byte craft over named source vectors.
    Craft(Craft),
}

/// Deterministic crafts; every one names its source vector(s).
#[derive(Clone, Copy)]
enum Craft {
    /// Annex-B concatenation of two different-SPS streams (mid-stream
    /// SPS change; oracle must decode across the reset).
    SpsChange,
    /// Annex-B with the first slice moved ahead of the parameter sets.
    SlicesBeforeSets,
    /// Truncate the source to half its bytes.
    TruncateHalf,
    /// Complete sets plus a poison slice NAL (`slice_type` out of
    /// range): the stream ends in a broken NAL.
    TruncateAnnexB,
    /// Complete VPS plus a poison SPS tail (luma width out of range).
    TruncateSets,
    /// Fixed pseudorandom bytes (LCG over `seed`) after `prefix`;
    /// Annex B garbage carries a slice start code engaging the NAL path.
    Garbage { seed: u64, prefix: &'static [u8] },
    /// Valid SPS/PPS plus a hand-built SI slice (`slice_type` 8).
    SiSlice,
    /// PPS rewritten to two slice groups (FMO).
    Fmo,
    /// SPS/PPS ids renumbered with gaps (valid).
    SetIds,
    /// Second access unit `frame_num` jumped ahead (valid).
    FrameNumGap,
    /// SPS re-emitted without its VUI (valid).
    VuiAbsent,
    /// PPS citing a nonexistent SPS id.
    PpsUnknownSps,
    /// First slice citing a nonexistent PPS id.
    SliceUnknownPps,
    /// Conformance window larger than the picture.
    ConformOverrun,
    /// OBU stream cut mid-payload.
    TruncateObu,
    /// First OBU size field replaced with a non-terminating LEB128.
    BadLeb128,
    /// Leading sequence header OBU dropped.
    FrameBeforeSeqHdr,
    /// IVF frame width/height header patched away from coded size.
    RenderSize,
    /// Keyframe plus a show-existing entry for slot 0.
    ShowExisting,
    /// First VP9 frame marker flipped off `0b10`.
    BadMarker,
    /// Middle payload bytes of the first frame flipped.
    TokenCorrupt,
}

struct Probe {
    profile: String,
    pix_fmt: String,
    width: u32,
    height: u32,
    field_order: String,
    has_b_frames: String,
}

struct Vector {
    codec_dir: String,
    path: PathBuf,
    probe: Probe,
}

struct Req {
    codec: &'static str,
    /// Vector stem; the gate wants exactly this file.
    name: &'static str,
    /// Baseline clause this row pins.
    clause: &'static str,
    synth: Option<&'static str>,
    blocked: Option<&'static str>,
    /// Fetched file (via `just vectors`) when there is no synth recipe.
    fetch: Option<&'static str>,
}

struct Synth {
    name: &'static str,
    codec_dir: &'static str,
    /// Output container extension: `mp4`, `ivf`, `obu`, `h264`, `h265`.
    container: &'static str,
    clip: &'static str,
    width: u32,
    height: u32,
    pix_fmt: &'static str,
    encoder: &'static str,
    extra: &'static [&'static str],
    /// Derivation from other vectors.
    derive: Option<Derive>,
    /// Sources for [`Derive::Craft`] (stems in the same codec dir).
    craft_src: &'static [&'static str],
    expect: Expect,
}

#[allow(clippy::too_many_arguments)]
const fn synth(
    name: &'static str,
    codec_dir: &'static str,
    container: &'static str,
    clip: &'static str,
    width: u32,
    height: u32,
    pix_fmt: &'static str,
    encoder: &'static str,
    extra: &'static [&'static str],
) -> Synth {
    Synth {
        name,
        codec_dir,
        container,
        clip,
        width,
        height,
        pix_fmt,
        encoder,
        extra,
        derive: None,
        craft_src: &[],
        expect: Expect::Match,
    }
}

const fn either(mut synth: Synth, reason: &'static str) -> Synth {
    synth.expect = Expect::Either(reason);
    synth
}

const fn decline_vec(
    name: &'static str,
    codec_dir: &'static str,
    container: &'static str,
    craft: Craft,
    craft_src: &'static [&'static str],
    reason: &'static str,
) -> Synth {
    Synth {
        name,
        codec_dir,
        container,
        clip: "motion",
        width: 640,
        height: 360,
        pix_fmt: "yuv420p",
        encoder: "",
        extra: &[],
        derive: Some(Derive::Craft(craft)),
        craft_src,
        expect: Expect::Decline(reason),
    }
}

const fn either_derived(
    name: &'static str,
    codec_dir: &'static str,
    container: &'static str,
    derive: Derive,
    craft_src: &'static [&'static str],
    reason: &'static str,
) -> Synth {
    Synth {
        name,
        codec_dir,
        container,
        clip: "motion",
        width: 640,
        height: 360,
        pix_fmt: "yuv420p",
        encoder: "",
        extra: &[],
        derive: Some(derive),
        craft_src,
        expect: Expect::Either(reason),
    }
}

const fn derived(
    name: &'static str,
    codec_dir: &'static str,
    container: &'static str,
    derive: Derive,
    craft_src: &'static [&'static str],
) -> Synth {
    Synth {
        name,
        codec_dir,
        container,
        clip: "motion",
        width: 640,
        height: 360,
        pix_fmt: "yuv420p",
        encoder: "",
        extra: &[],
        derive: Some(derive),
        craft_src,
        expect: Expect::Match,
    }
}

#[allow(clippy::too_many_lines)]
fn h264_synths() -> Vec<Synth> {
    vec![
        synth(
            "anchor-high",
            "h264",
            "mp4",
            "motion",
            1920,
            1080,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-g", "60", "-bf", "3", "-aud", "1"],
        ),
        synth(
            "profile-baseline",
            "h264",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-profile", "baseline", "-g", "60"],
        ),
        synth(
            "profile-main",
            "h264",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-profile", "main", "-g", "60"],
        ),
        either(
            synth(
                "pix-422",
                "h264",
                "mp4",
                "motion",
                640,
                360,
                "yuv422p",
                "libx264",
                &["-preset", "veryfast", "-g", "60"],
            ),
            "High422 support varies by tier",
        ),
        either(
            synth(
                "pix-444",
                "h264",
                "mp4",
                "motion",
                640,
                360,
                "yuv444p",
                "libx264",
                &["-preset", "veryfast", "-g", "60"],
            ),
            "High444 support varies by tier",
        ),
        either(
            synth(
                "depth-10",
                "h264",
                "mp4",
                "motion",
                640,
                360,
                "yuv420p10le",
                "libx264",
                &["-preset", "veryfast", "-g", "60"],
            ),
            "High10 support varies by tier",
        ),
        synth(
            "frames-allintra",
            "h264",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-g", "1"],
        ),
        synth(
            "slices-single",
            "h264",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-g", "60", "-slices", "1"],
        ),
        synth(
            "slices-multi",
            "h264",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-g", "60", "-slices", "4"],
        ),
        either(
            synth(
                "mbaff",
                "h264",
                "mp4",
                "motion",
                640,
                360,
                "yuv420p",
                "libx264",
                &[
                    "-preset",
                    "veryfast",
                    "-g",
                    "60",
                    "-x264-params",
                    "interlaced=1:tff=1",
                ],
            ),
            "interlaced support varies by tier",
        ),
        synth(
            "cqm-custom",
            "h264",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx264",
            &[
                "-preset",
                "veryfast",
                "-g",
                "60",
                "-x264-params",
                "cqm=flat:8x8dct=1",
            ],
        ),
        synth(
            "noise",
            "h264",
            "mp4",
            "noise",
            1920,
            1080,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast", "-g", "60", "-bf", "3"],
        ),
        synth(
            "size-4k",
            "h264",
            "mp4",
            "motion",
            3840,
            2160,
            "yuv420p",
            "libx264",
            &["-preset", "ultrafast", "-g", "60", "-bf", "2"],
        ),
        derived(
            "crop",
            "h264",
            "mp4",
            Derive::Bsf("h264_metadata=crop_left=0:crop_right=8:crop_top=0:crop_bottom=8"),
            &["anchor-high"],
        ),
        derived(
            "annexb",
            "h264",
            "h264",
            Derive::AnnexB("h264_mp4toannexb"),
            &["anchor-high"],
        ),
        derived(
            "sps-change",
            "h264",
            "h264",
            Derive::Craft(Craft::SpsChange),
            &["anchor-high", "slices-single"],
        ),
        either_derived(
            "slices-before-sets",
            "h264",
            "h264",
            Derive::Craft(Craft::SlicesBeforeSets),
            &["anchor-high"],
            "slice/set ordering tolerance varies by tier",
        ),
        derived(
            "vui-absent",
            "h264",
            "h264",
            Derive::Craft(Craft::VuiAbsent),
            &["profile-main"],
        ),
        derived(
            "set-ids",
            "h264",
            "h264",
            Derive::Craft(Craft::SetIds),
            &["slices-multi"],
        ),
        derived(
            "frame-num-gaps",
            "h264",
            "h264",
            Derive::Craft(Craft::FrameNumGap),
            &["anchor-high"],
        ),
        decline_vec(
            "decline-truncated-nal",
            "h264",
            "h264",
            Craft::TruncateAnnexB,
            &["anchor-high"],
            "truncated NAL",
        ),
        decline_vec(
            "decline-garbage",
            "h264",
            "h264",
            Craft::Garbage {
                seed: 0x48_32_36_34,
                prefix: b"\x00\x00\x01\x65",
            },
            &[],
            "unparseable bytes",
        ),
        decline_vec(
            "decline-si-sp",
            "h264",
            "h264",
            Craft::SiSlice,
            &["profile-baseline"],
            "SI/SP slices unsupported",
        ),
        decline_vec(
            "decline-fmo",
            "h264",
            "h264",
            Craft::Fmo,
            &["anchor-high"],
            "flexible macroblock ordering unsupported",
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn h265_synths() -> Vec<Synth> {
    vec![
        synth(
            "anchor-main",
            "h265",
            "mp4",
            "motion",
            1920,
            1080,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "aud=1"],
        ),
        synth(
            "profile-main10",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p10le",
            "libx265",
            &["-preset", "veryfast"],
        ),
        either(
            synth(
                "pix-422",
                "h265",
                "mp4",
                "motion",
                640,
                360,
                "yuv422p",
                "libx265",
                &["-preset", "veryfast"],
            ),
            "4:2:2 support varies by tier",
        ),
        either(
            synth(
                "pix-444",
                "h265",
                "mp4",
                "motion",
                640,
                360,
                "yuv444p",
                "libx265",
                &["-preset", "veryfast"],
            ),
            "4:4:4 support varies by tier",
        ),
        synth(
            "sao-off",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "no-sao=1"],
        ),
        synth(
            "deblock-off",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "no-deblock=1"],
        ),
        synth(
            "tmvp-off",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "no-tmvp=1"],
        ),
        synth(
            "scaling-on",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &[
                "-preset",
                "veryfast",
                "-x265-params",
                "scaling-list=default",
            ],
        ),
        synth(
            "pcm-on",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "pcm=1"],
        ),
        synth(
            "open-gop",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &[
                "-preset",
                "veryfast",
                "-x265-params",
                "open-gop=1:keyint=30",
            ],
        ),
        synth(
            "tiles-uniform",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "tiles=2x2"],
        ),
        synth(
            "slices-multi",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "slices=2"],
        ),
        synth(
            "wpp",
            "h265",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast", "-x265-params", "wpp=1"],
        ),
        either(
            synth(
                "field-seq",
                "h265",
                "mp4",
                "motion",
                640,
                360,
                "yuv420p",
                "libx265",
                &["-preset", "veryfast", "-x265-params", "interlace=1"],
            ),
            "interlaced support varies by tier",
        ),
        synth(
            "noise",
            "h265",
            "mp4",
            "noise",
            1920,
            1080,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast"],
        ),
        synth(
            "size-4k",
            "h265",
            "mp4",
            "motion",
            3840,
            2160,
            "yuv420p10le",
            "libx265",
            &["-preset", "ultrafast"],
        ),
        derived(
            "annexb",
            "h265",
            "h265",
            Derive::AnnexB("hevc_mp4toannexb"),
            &["anchor-main"],
        ),
        decline_vec(
            "decline-truncated-sets",
            "h265",
            "h265",
            Craft::TruncateSets,
            &["anchor-main"],
            "truncated parameter sets",
        ),
        decline_vec(
            "decline-garbage",
            "h265",
            "h265",
            Craft::Garbage {
                seed: 0x48_32_36_35,
                prefix: b"\x00\x00\x01\x26\x01",
            },
            &[],
            "unparseable bytes",
        ),
        decline_vec(
            "decline-pps-unknown-sps",
            "h265",
            "h265",
            Craft::PpsUnknownSps,
            &["anchor-main"],
            "PPS cites unknown SPS",
        ),
        decline_vec(
            "decline-slice-unknown-pps",
            "h265",
            "h265",
            Craft::SliceUnknownPps,
            &["anchor-main"],
            "slice cites unknown PPS",
        ),
        decline_vec(
            "decline-conformance-overrun",
            "h265",
            "h265",
            Craft::ConformOverrun,
            &["anchor-main"],
            "conformance window exceeds picture",
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn av1_synths() -> Vec<Synth> {
    vec![
        synth(
            "anchor-main",
            "av1",
            "mp4",
            "motion",
            1920,
            1080,
            "yuv420p",
            "libsvtav1",
            &["-preset", "10"],
        ),
        synth(
            "depth-10",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p10le",
            "libsvtav1",
            &["-preset", "12"],
        ),
        either(
            synth(
                "pix-444",
                "av1",
                "mp4",
                "motion",
                640,
                360,
                "yuv444p",
                "libaom-av1",
                &["-cpu-used", "8", "-b:v", "1M"],
            ),
            "High profile support varies by tier",
        ),
        either(
            synth(
                "pix-422-12",
                "av1",
                "mp4",
                "motion",
                640,
                360,
                "yuv422p12le",
                "libaom-av1",
                &["-cpu-used", "8", "-b:v", "1M"],
            ),
            "Professional profile support varies by tier",
        ),
        synth(
            "noise",
            "av1",
            "mp4",
            "noise",
            1920,
            1080,
            "yuv420p",
            "libsvtav1",
            &["-preset", "12"],
        ),
        synth(
            "size-4k",
            "av1",
            "mp4",
            "motion",
            3840,
            2160,
            "yuv420p",
            "libsvtav1",
            &["-preset", "13"],
        ),
        synth(
            "grain-on",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &[
                "-cpu-used",
                "8",
                "-b:v",
                "1M",
                "-aom-params",
                "film-grain-test=8",
            ],
        ),
        synth(
            "tiles-uniform",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libsvtav1",
            &["-preset", "10", "-tile-columns", "1", "-tile-rows", "1"],
        ),
        synth(
            "tiles-aom",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &["-cpu-used", "8", "-b:v", "1M", "-tiles", "2x2"],
        ),
        synth(
            "obu-raw",
            "av1",
            "obu",
            "motion",
            640,
            360,
            "yuv420p",
            "libsvtav1",
            &["-preset", "12"],
        ),
        synth(
            "ivf",
            "av1",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libsvtav1",
            &["-preset", "12"],
        ),
        derived(
            "render-size",
            "av1",
            "ivf",
            Derive::Craft(Craft::RenderSize),
            &["ivf"],
        ),
        synth(
            "filters-off",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &[
                "-cpu-used",
                "8",
                "-b:v",
                "1M",
                "-aom-params",
                "enable-cdef=0:enable-restoration=0",
            ],
        ),
        synth(
            "segmentation",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &["-cpu-used", "8", "-b:v", "1M", "-aq-mode", "1"],
        ),
        synth(
            "global-motion-off",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &[
                "-cpu-used",
                "8",
                "-b:v",
                "1M",
                "-aom-params",
                "enable-global-motion=0",
            ],
        ),
        synth(
            "warped-off",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &[
                "-cpu-used",
                "8",
                "-b:v",
                "1M",
                "-aom-params",
                "enable-warped-motion=0",
            ],
        ),
        synth(
            "overlays",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libaom-av1",
            &[
                "-cpu-used",
                "8",
                "-b:v",
                "1M",
                "-aom-params",
                "enable-overlay=1",
            ],
        ),
        synth(
            "color-601",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libsvtav1",
            &[
                "-preset",
                "12",
                "-colorspace",
                "smpte170m",
                "-color_trc",
                "smpte170m",
                "-color_primaries",
                "smpte170m",
            ],
        ),
        synth(
            "color-709",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p",
            "libsvtav1",
            &[
                "-preset",
                "12",
                "-colorspace",
                "bt709",
                "-color_trc",
                "bt709",
                "-color_primaries",
                "bt709",
                "-chroma_sample_location",
                "left",
            ],
        ),
        synth(
            "color-2020",
            "av1",
            "mp4",
            "motion",
            640,
            360,
            "yuv420p10le",
            "libsvtav1",
            &[
                "-preset",
                "12",
                "-colorspace",
                "bt2020nc",
                "-color_trc",
                "smpte2084",
                "-color_primaries",
                "bt2020",
                "-chroma_sample_location",
                "topleft",
            ],
        ),
        decline_vec(
            "decline-truncated-obu",
            "av1",
            "obu",
            Craft::TruncateObu,
            &["obu-raw"],
            "truncated OBU",
        ),
        decline_vec(
            "decline-garbage",
            "av1",
            "obu",
            Craft::Garbage {
                seed: 0x00_A0_01,
                prefix: b"",
            },
            &[],
            "unparseable bytes",
        ),
        decline_vec(
            "decline-bad-leb128",
            "av1",
            "obu",
            Craft::BadLeb128,
            &["obu-raw"],
            "malformed OBU size",
        ),
        decline_vec(
            "decline-frame-before-seqhdr",
            "av1",
            "obu",
            Craft::FrameBeforeSeqHdr,
            &["obu-raw"],
            "frames before sequence header",
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn vp9_synths() -> Vec<Synth> {
    vec![
        synth(
            "anchor-p0",
            "vp9",
            "ivf",
            "motion",
            1920,
            1080,
            "yuv420p",
            "libvpx-vp9",
            &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "4M"],
        ),
        either(
            synth(
                "pix-422",
                "vp9",
                "ivf",
                "motion",
                640,
                360,
                "yuv422p",
                "libvpx-vp9",
                &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "1M"],
            ),
            "profile 1 support varies by tier",
        ),
        either(
            synth(
                "pix-444",
                "vp9",
                "ivf",
                "motion",
                640,
                360,
                "yuv444p",
                "libvpx-vp9",
                &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "1M"],
            ),
            "profile 1 support varies by tier",
        ),
        either(
            synth(
                "depth-10",
                "vp9",
                "ivf",
                "motion",
                640,
                360,
                "yuv420p10le",
                "libvpx-vp9",
                &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "1M"],
            ),
            "profile 2 support varies by tier",
        ),
        either(
            synth(
                "pix-444-12",
                "vp9",
                "ivf",
                "motion",
                640,
                360,
                "yuv444p12le",
                "libvpx-vp9",
                &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "1M"],
            ),
            "profile 3 support varies by tier",
        ),
        synth(
            "noise",
            "vp9",
            "ivf",
            "noise",
            1920,
            1080,
            "yuv420p",
            "libvpx-vp9",
            &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "8M"],
        ),
        synth(
            "size-4k",
            "vp9",
            "ivf",
            "motion",
            3840,
            2160,
            "yuv420p",
            "libvpx-vp9",
            &["-cpu-used", "8", "-deadline", "realtime", "-b:v", "12M"],
        ),
        synth(
            "tiles-2col",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-tile-columns",
                "1",
            ],
        ),
        synth(
            "tiles-4col-rows",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-tile-columns",
                "2",
                "-tile-rows",
                "1",
            ],
        ),
        synth(
            "lossless",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &["-cpu-used", "8", "-deadline", "realtime", "-lossless", "1"],
        ),
        synth(
            "qp-0",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "0",
                "-crf",
                "0",
                "-qmin",
                "0",
                "-qmax",
                "0",
            ],
        ),
        synth(
            "qp-63",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "0",
                "-crf",
                "63",
                "-qmin",
                "63",
                "-qmax",
                "63",
            ],
        ),
        synth(
            "error-resilient",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-error-resilient",
                "1",
            ],
        ),
        synth(
            "frame-parallel",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-frame-parallel",
                "1",
                "-tile-columns",
                "2",
            ],
        ),
        synth(
            "sharpness-7",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-sharpness",
                "7",
            ],
        ),
        synth(
            "segmentation",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-aq-mode",
                "1",
            ],
        ),
        synth(
            "altref",
            "vp9",
            "ivf",
            "motion",
            640,
            360,
            "yuv420p",
            "libvpx-vp9",
            &[
                "-cpu-used",
                "8",
                "-deadline",
                "realtime",
                "-b:v",
                "1M",
                "-lag-in-frames",
                "16",
                "-auto-alt-ref",
                "1",
            ],
        ),
        derived(
            "show-existing",
            "vp9",
            "ivf",
            Derive::Craft(Craft::ShowExisting),
            &["anchor-p0"],
        ),
        decline_vec(
            "decline-bad-marker",
            "vp9",
            "ivf",
            Craft::BadMarker,
            &["anchor-p0"],
            "bad VP9 frame marker",
        ),
        decline_vec(
            "decline-truncated-tiles",
            "vp9",
            "ivf",
            Craft::TruncateHalf,
            &["tiles-4col-rows"],
            "truncated tiles",
        ),
        decline_vec(
            "decline-token-partitions",
            "vp9",
            "ivf",
            Craft::TokenCorrupt,
            &["anchor-p0"],
            "corrupt token data",
        ),
        decline_vec(
            "decline-garbage",
            "vp9",
            "ivf",
            Craft::Garbage {
                seed: 0x00_56_50_39,
                prefix: b"",
            },
            &[],
            "unparseable bytes",
        ),
    ]
}

fn synths() -> BTreeMap<(&'static str, &'static str), Synth> {
    let mut list = Vec::new();
    list.extend(h264_synths());
    list.extend(h265_synths());
    list.extend(av1_synths());
    list.extend(vp9_synths());
    let mut map = BTreeMap::new();
    for synth in list {
        let key = (synth.codec_dir, synth.name);
        assert!(
            map.insert(key, synth).is_none(),
            "duplicate synth vector name",
        );
    }
    map
}

const fn req(codec: &'static str, name: &'static str, clause: &'static str) -> Req {
    Req {
        codec,
        name,
        clause,
        synth: Some(name),
        blocked: None,
        fetch: None,
    }
}

const fn req_fetch(
    codec: &'static str,
    name: &'static str,
    clause: &'static str,
    file: &'static str,
) -> Req {
    Req {
        codec,
        name,
        clause,
        synth: None,
        blocked: None,
        fetch: Some(file),
    }
}

const fn blocked(
    codec: &'static str,
    name: &'static str,
    clause: &'static str,
    why: &'static str,
) -> Req {
    Req {
        codec,
        name,
        clause,
        synth: None,
        blocked: Some(why),
        fetch: None,
    }
}

fn h264_reqs() -> Vec<Req> {
    vec![
        req(
            "h264",
            "anchor-high",
            "High profile; IDR/I/P/B; VUI present",
        ),
        req("h264", "profile-baseline", "Baseline profile"),
        req("h264", "profile-main", "Main profile"),
        req("h264", "pix-422", "High422 accept-or-decline"),
        req("h264", "pix-444", "High444 accept-or-decline"),
        req("h264", "depth-10", "High10 accept-or-decline"),
        req("h264", "frames-allintra", "IDR/I-only frames"),
        req("h264", "slices-single", "single slice"),
        req("h264", "slices-multi", "multi slice"),
        req("h264", "mbaff", "MBAFF interlaced accept-or-decline"),
        req("h264", "cqm-custom", "custom 4x4/8x8 scaling matrices"),
        req("h264", "noise", "adversarial noise content"),
        req("h264", "size-4k", "4K scale derivative"),
        req("h264", "crop", "frame cropping"),
        req(
            "h264",
            "annexb",
            "Annex B framing (SPS/PPS/SEI/AUD/IDR/P/B); RBSP escapes",
        ),
        req("h264", "sps-change", "mid-stream SPS change (reset)"),
        req(
            "h264",
            "slices-before-sets",
            "slices before sets accept-or-decline",
        ),
        req("h264", "vui-absent", "VUI absent"),
        req(
            "h264",
            "set-ids",
            "set-id gaps (active SPS 2, inert PPS 3)",
        ),
        req("h264", "frame-num-gaps", "frame_num gaps"),
        req("h264", "decline-truncated-nal", "robustness: truncated NAL"),
        req("h264", "decline-garbage", "robustness: garbage"),
        req("h264", "decline-si-sp", "SI/SP: decline"),
        blocked(
            "h264",
            "aso",
            "ASO: decline",
            "swapped slices decode with logged errors (neither clean match nor clean decline); needs crafted vectors",
        ),
        req("h264", "decline-fmo", "FMO: decline"),
        blocked(
            "h264",
            "mmco",
            "MMCO incl MMCO6",
            "no MMCO-emitting encoder (x264 never emits MMCO); needs JM vectors",
        ),
        blocked(
            "h264",
            "paff-mixed",
            "frame/field mixing; field-coded pictures",
            "x264 is MBAFF-only; needs PAFF encoder vectors",
        ),
    ]
}

fn h265_reqs() -> Vec<Req> {
    vec![
        req("h265", "anchor-main", "Main profile; VPS/PTL; IDR/P/B; RPS"),
        req("h265", "profile-main10", "Main10; 420 10-bit"),
        req("h265", "pix-422", "422 accept-or-decline"),
        req("h265", "pix-444", "444 accept-or-decline"),
        req("h265", "sao-off", "SAO off (anchor: on)"),
        req("h265", "deblock-off", "deblocking off (anchor: on)"),
        req("h265", "tmvp-off", "TMVP off (anchor: on)"),
        req("h265", "scaling-on", "scaling on (anchor: off)"),
        req("h265", "pcm-on", "PCM on (anchor: off)"),
        req("h265", "open-gop", "CRA/RASL/RADL; explicit + inter RPS"),
        req("h265", "tiles-uniform", "tiles + entry points, uniform"),
        req("h265", "slices-multi", "independent slices"),
        req("h265", "wpp", "dependent slices (WPP)"),
        req("h265", "field-seq", "field_seq_flag set accept-or-decline"),
        req("h265", "noise", "adversarial noise content"),
        req("h265", "size-4k", "4K scale derivative"),
        req("h265", "annexb", "Annex B framing (VPS/SPS/PPS/SEI/AUD)"),
        req(
            "h265",
            "decline-truncated-sets",
            "robustness: truncated sets",
        ),
        req("h265", "decline-garbage", "robustness: garbage"),
        req(
            "h265",
            "decline-pps-unknown-sps",
            "robustness: PPS citing unknown SPS",
        ),
        req(
            "h265",
            "decline-slice-unknown-pps",
            "robustness: slice citing unknown PPS",
        ),
        req(
            "h265",
            "decline-conformance-overrun",
            "robustness: conformance overruns",
        ),
        blocked(
            "h265",
            "rext-tagged-420",
            "Rext-tagged 8-bit 4:2:0 must decode",
            "no Range Extension encoder; needs fetched vectors",
        ),
        blocked(
            "h265",
            "true-rext",
            "true Range Extension declines",
            "no Range Extension encoder; needs fetched vectors",
        ),
        blocked(
            "h265",
            "bla",
            "BLA frames",
            "no splicing encoder; needs fetched vectors",
        ),
        blocked(
            "h265",
            "long-term",
            "long-term refs",
            "x265 has no long-term refs; needs fetched vectors",
        ),
        blocked(
            "h265",
            "tiles-explicit",
            "tiles with explicit spacing",
            "x265 emits uniform tiles only; needs crafted vectors",
        ),
        blocked(
            "h265",
            "deblock-override",
            "deblocking override",
            "encoder cannot force slice override flags; needs crafted vectors",
        ),
    ]
}

#[allow(clippy::too_many_lines)]
fn av1_reqs() -> Vec<Req> {
    vec![
        req(
            "av1",
            "anchor-main",
            "Main/420/8-bit; KEY/INTER; refs; one TG",
        ),
        req("av1", "depth-10", "10-bit"),
        req("av1", "pix-444", "High profile accept-or-decline"),
        req(
            "av1",
            "pix-422-12",
            "Professional accept-or-decline; 12-bit",
        ),
        blocked(
            "av1",
            "monochrome",
            "monochrome accept-or-decline",
            "this libaom exposes no monochrome option; needs crafted vectors",
        ),
        req("av1", "noise", "adversarial noise content"),
        req("av1", "size-4k", "4K scale derivative"),
        req("av1", "grain-on", "film grain on (anchor: off)"),
        req("av1", "tiles-uniform", "tiles uniform"),
        req("av1", "tiles-aom", "multiple tiles per frame (second encoder)"),
        req("av1", "obu-raw", "raw OBU input; size-bearing framing"),
        req("av1", "ivf", "IVF input"),
        blocked(
            "av1",
            "superres",
            "superres accept-or-decline",
            "this libaom exposes no superres option; needs crafted vectors",
        ),
        req(
            "av1",
            "render-size",
            "render-size mismatch accept-or-decline",
        ),
        req("av1", "filters-off", "CDEF/restoration off (anchor: on)"),
        blocked(
            "av1",
            "loopfilter-off",
            "loop filter off (anchor: on)",
            "this libaom exposes no loop-filter toggle; needs crafted vectors",
        ),
        req("av1", "segmentation", "segmentation on (anchor: off)"),
        req(
            "av1",
            "global-motion-off",
            "global motion identity (anchor: full search)",
        ),
        req("av1", "warped-off", "warped off (anchor: on)"),
        blocked(
            "av1",
            "compound-off",
            "compound off (anchor: on)",
            "this libaom exposes no compound toggle; default encodes use compound",
        ),
        req("av1", "overlays", "overlays"),
        req("av1", "color-601", "BT.601 color"),
        req("av1", "color-709", "BT.709 color + chroma position"),
        req("av1", "color-2020", "BT.2020 color + transfer"),
        req("av1", "decline-truncated-obu", "robustness: truncated OBU"),
        req("av1", "decline-garbage", "robustness: garbage"),
        req("av1", "decline-bad-leb128", "robustness: bad LEB128"),
        req(
            "av1",
            "decline-frame-before-seqhdr",
            "robustness: frames before sequence header",
        ),
        blocked(
            "av1",
            "decline-tg-count-mismatch",
            "robustness: tile-group count mismatch",
            "encoders pack tiles into Frame OBUs (tiles-aom: 0 tile-group OBUs); needs crafted vectors",
        ),
        blocked(
            "av1",
            "intra-only",
            "INTRA_ONLY frames",
            "encoders emit KEY instead; needs crafted vectors",
        ),
        blocked(
            "av1",
            "switch",
            "SWITCH frames",
            "encoders never emit SWITCH; needs crafted vectors",
        ),
        blocked(
            "av1",
            "show-existing",
            "show_existing incl to-empty-slot decline",
            "encoders never emit show_existing; needs crafted vectors",
        ),
        blocked(
            "av1",
            "tiles-nonuniform",
            "tiles non-uniform",
            "encoders emit uniform tiles only; needs crafted vectors",
        ),
        blocked(
            "av1",
            "ext-headers-op",
            "extension headers; operating points",
            "needs scalable streams; no SVC encoder",
        ),
        blocked(
            "av1",
            "scalability",
            "scalability layers: decline",
            "no scalable-stream encoder; tier-relative without vectors",
        ),
    ]
}

fn vp9_reqs() -> Vec<Req> {
    vec![
        req("vp9", "anchor-p0", "profile 0; keyframes; inter"),
        req("vp9", "pix-422", "profile 1 422 accept-or-decline"),
        req("vp9", "pix-444", "profile 1 444 accept-or-decline"),
        req("vp9", "depth-10", "profile 2 accept-or-decline"),
        req("vp9", "pix-444-12", "profile 3 accept-or-decline"),
        req("vp9", "noise", "adversarial noise content"),
        req("vp9", "size-4k", "4K scale derivative"),
        req("vp9", "tiles-2col", "2 tile columns"),
        req("vp9", "tiles-4col-rows", "4+ tile columns plus rows"),
        req("vp9", "lossless", "lossless on (anchor: off)"),
        req("vp9", "qp-0", "QP 0"),
        req("vp9", "qp-63", "QP 63"),
        req("vp9", "error-resilient", "error-resilient mode"),
        req("vp9", "frame-parallel", "frame-parallel mode"),
        req("vp9", "sharpness-7", "loop filter sweep (anchor: 0)"),
        req("vp9", "segmentation", "segmentation on (anchor: off)"),
        req("vp9", "altref", "shown/unshown alt-refs; golden/last"),
        req("vp9", "show-existing", "show_existing frame"),
        req_fetch(
            "vp9",
            "vp90-2-20-big_superframe-01",
            "superframes (multi-frame packets)",
            "vp90-2-20-big_superframe-01.webm",
        ),
        req("vp9", "decline-bad-marker", "robustness: bad marker"),
        req(
            "vp9",
            "decline-truncated-tiles",
            "robustness: truncated tiles",
        ),
        req(
            "vp9",
            "decline-token-partitions",
            "robustness: invalid token partitions",
        ),
        req("vp9", "decline-garbage", "robustness: garbage"),
    ]
}

fn reqs() -> Vec<Req> {
    let mut out = Vec::new();
    out.extend(h264_reqs());
    out.extend(h265_reqs());
    out.extend(av1_reqs());
    out.extend(vp9_reqs());
    out
}

fn spawn(cmd: &mut Command) -> Result<(bool, String, Vec<u8>), String> {
    let out = cmd.output().map_err(|e| format!("spawn failed: {e}"))?;
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), text, out.stdout))
}

fn children(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{} unreadable: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    out.sort();
    Ok(out)
}

fn keep<T>(path: &Path, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(why) => {
            println!("skip {}: {why}", path.display());
            None
        }
    }
}

fn ffmpeg_version() -> Result<String, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-version"]);
    let (ok, _, out) = spawn(&mut cmd)?;
    if !ok {
        return Err("ffmpeg -version failed".to_owned());
    }
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .next()
        .unwrap_or("unknown")
        .to_owned())
}

fn probe(path: &Path) -> Result<Probe, String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=profile,pix_fmt,width,height,field_order,has_b_frames",
        "-of",
        "default=noprint_wrappers=1:nokey=0",
    ]);
    cmd.arg(path);
    let (ok, err, out) = spawn(&mut cmd)?;
    if !ok {
        return Err(format!("ffprobe {} failed: {err}", path.display()));
    }
    let line = String::from_utf8_lossy(&out);
    let mut fields = BTreeMap::new();
    for entry in line.lines() {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(format!("ffprobe {} bad entry: {entry}", path.display()));
        };
        fields.insert(key, value);
    }
    let get = |key: &str| fields.get(key).copied().unwrap_or("unknown");
    let width: u32 = get("width")
        .parse()
        .map_err(|_| format!("bad width in {line}"))?;
    let height: u32 = get("height")
        .parse()
        .map_err(|_| format!("bad height in {line}"))?;
    Ok(Probe {
        profile: get("profile").to_owned(),
        pix_fmt: get("pix_fmt").to_owned(),
        width,
        height,
        field_order: get("field_order").to_owned(),
        has_b_frames: get("has_b_frames").to_owned(),
    })
}

/// SHA-256 hex of one full clip render through the lavfi graph.
fn render_clip(src: &str) -> Result<String, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-f", "lavfi", "-i", src]);
    cmd.args([
        "-pix_fmt", "yuv420p", "-c:v", "rawvideo", "-f", "rawvideo", "pipe:1",
    ]);
    cmd.stdin(Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| format!("clip render failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("clip render failed: {err}"));
    }
    if out.stdout.len() != 1920usize * 1080 * 3 / 2 * CLIP_FRAMES as usize {
        return Err(format!("clip render short: {} bytes", out.stdout.len()));
    }
    Ok(FrameHash::of(&out.stdout).hex())
}

/// Baseline determinism gate: each canonical clip renders twice with
/// identical hashes, or the recipe is rejected before any vector builds.
fn prove_clips() -> Result<(), String> {
    for clip in &CLIPS {
        let first = render_clip(clip.src)?;
        let second = render_clip(clip.src)?;
        if first != second {
            return Err(format!(
                "clip {} nondeterministic: {first} vs {second}",
                clip.name
            ));
        }
        println!("clip {}: deterministic ({first})", clip.name);
    }
    Ok(())
}

fn clip_src(name: &str) -> Result<&'static str, String> {
    CLIPS
        .iter()
        .find(|clip| clip.name == name)
        .map(|clip| clip.src)
        .ok_or_else(|| format!("unknown clip {name}"))
}

fn lavfi_graph(synth: &Synth) -> Result<String, String> {
    let src = clip_src(synth.clip)?;
    Ok(format!(
        "{src},scale={}x{}:flags=bilinear,format={}",
        synth.width, synth.height, synth.pix_fmt
    ))
}

fn synth_path(corpus: &Path, synth: &Synth) -> PathBuf {
    corpus
        .join("synth")
        .join(synth.codec_dir)
        .join(format!("{}.{}", synth.name, synth.container))
}

fn run_encode(corpus: &Path, synth: &Synth) -> Result<PathBuf, String> {
    let out = synth_path(corpus, synth);
    if out.is_file() {
        return Ok(out);
    }
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("synth dir failed: {e}"))?;
    }
    let graph = lavfi_graph(synth)?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"]);
    cmd.arg(&graph);
    cmd.args(["-frames:v", &CLIP_FRAMES.to_string()]);
    // Pinned thread count: encoder output varies with thread layout,
    // and committed goldens must verify on any core count.
    cmd.args(["-threads", "8"]);
    cmd.args(["-c:v", synth.encoder]);
    cmd.args(synth.extra);
    if synth.container == "obu" {
        cmd.args(["-f", "obu"]);
    }
    cmd.arg(&out);
    let (ok, err, _) = spawn(&mut cmd)?;
    if !ok {
        let _ = fs::remove_file(&out);
        return Err(format!("synth {} failed: {err}", synth.name));
    }
    Ok(out)
}

fn run_bsf(corpus: &Path, synth: &Synth, filter: &str) -> Result<PathBuf, String> {
    let out = synth_path(corpus, synth);
    if out.is_file() {
        return Ok(out);
    }
    let [src] = synth.craft_src else {
        return Err(format!("bsf {} needs one source", synth.name));
    };
    let recipes = synths();
    let src_synth = recipes
        .get(&(synth.codec_dir, src))
        .ok_or_else(|| format!("bsf {} unknown source {src}", synth.name))?;
    let input = run_synth(corpus, src_synth)?;
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("synth dir failed: {e}"))?;
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-i"]);
    cmd.arg(&input);
    cmd.args(["-c", "copy", "-bsf:v", filter]);
    cmd.arg(&out);
    let (ok, err, _) = spawn(&mut cmd)?;
    if !ok {
        let _ = fs::remove_file(&out);
        return Err(format!("bsf {} failed: {err}", synth.name));
    }
    Ok(out)
}

/// One IVF frame payload with its presentation timestamp.
struct IvfFrame {
    pts: u64,
    data: Vec<u8>,
}

/// Parse IVF into its 32-byte header plus frames.
fn parse_ivf(bytes: &[u8]) -> Result<(Vec<u8>, Vec<IvfFrame>), String> {
    if bytes.len() < 32 || &bytes[0..4] != b"DKIF" {
        return Err("not IVF".to_owned());
    }
    let header = bytes[..32].to_vec();
    let mut frames = Vec::new();
    let mut pos = 32;
    while pos + 12 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("u32")) as usize;
        let stamp = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().expect("u64"));
        pos += 12;
        if pos + size > bytes.len() {
            return Err("IVF frame overruns file".to_owned());
        }
        frames.push(IvfFrame {
            pts: stamp,
            data: bytes[pos..pos + size].to_vec(),
        });
        pos += size;
    }
    if frames.is_empty() {
        return Err("IVF holds no frames".to_owned());
    }
    Ok((header, frames))
}

fn build_ivf(header: &[u8], frames: &[IvfFrame]) -> Result<Vec<u8>, String> {
    let mut out = header.to_vec();
    for frame in frames {
        let size = u32::try_from(frame.data.len()).map_err(|_| "IVF frame too large")?;
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&frame.pts.to_le_bytes());
        out.extend_from_slice(&frame.data);
    }
    Ok(out)
}

/// One OBU location: byte offset, header length, type, payload length.
struct ObuLoc {
    offset: usize,
    header: usize,
    obu_type: u8,
    payload: usize,
}

fn read_leb128(bytes: &[u8], mut pos: usize) -> Result<(u64, usize), String> {
    let mut value: u64 = 0;
    for index in 0..8 {
        let byte = *bytes.get(pos).ok_or("LEB128 overrun")?;
        pos += 1;
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok((value, pos));
        }
    }
    Err("LEB128 never terminates".to_owned())
}

/// Walk size-bearing OBUs; stops at the first size-less OBU.
fn walk_obus(bytes: &[u8]) -> Result<Vec<ObuLoc>, String> {
    let mut locs = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let head = bytes[pos];
        if head & 0x80 != 0 {
            return Err("OBU forbidden bit set".to_owned());
        }
        let obu_type = (head >> 3) & 0x0f;
        let has_ext = (head >> 2) & 1 == 1;
        let has_size = (head >> 1) & 1 == 1;
        let mut at = pos + 1;
        if has_ext {
            at += 1;
            if at > bytes.len() {
                return Err("OBU extension overruns".to_owned());
            }
        }
        if !has_size {
            break;
        }
        let (size, next) = read_leb128(bytes, at)?;
        let payload: usize = size.try_into().map_err(|_| "OBU size too large")?;
        if next + payload > bytes.len() {
            return Err("OBU payload overruns".to_owned());
        }
        locs.push(ObuLoc {
            offset: pos,
            header: next - pos,
            obu_type,
            payload,
        });
        pos = next + payload;
    }
    if locs.is_empty() {
        return Err("no OBUs found".to_owned());
    }
    Ok(locs)
}

/// Deterministic filler bytes (LCG over `seed`).
fn garbage(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.extend_from_slice(&(state >> 33).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// H.264 SPS fields the crafts need.
struct H264Sps {
    log2_max_frame_num: usize,
    poc_type: u32,
    log2_max_poc_lsb: usize,
}

/// Parsed H.264 SPS: the fields the crafts need plus the raw RBSP
/// and flag positions for verbatim rewrites.
struct H264SpsInfo {
    sps: H264Sps,
    rbsp: Vec<u8>,
    vui_flag_at: usize,
    vui_present: bool,
    gaps_flag_at: usize,
    prefix: Vec<u8>,
}

/// Parse an H.264 SPS NAL (header included) through the VUI flag.
fn parse_h264_sps(nal: &[u8]) -> Result<H264SpsInfo, String> {
    let rbsp = AnnexB::remove_emulation(nal);
    let mut br = BitReader::new(&rbsp);
    br.byte()?;
    let profile = br.byte()?;
    let constraints = br.byte()?;
    let level = br.byte()?;
    let _sps_id = br.ue()?;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma = br.ue()?;
        if chroma == 3 {
            br.bit()?;
        }
        br.ue()?;
        br.ue()?;
        br.bit()?;
        if br.bit()? == 1 {
            let count = if chroma == 3 { 12 } else { 8 };
            for _ in 0..count {
                if br.bit()? == 1 {
                    return Err("custom scaling lists unsupported here".to_owned());
                }
            }
        }
    }
    let log2_max_frame_num = br.ue()? as usize + 4;
    let poc_type = br.ue()?;
    let mut log2_max_poc_lsb = 0;
    if poc_type == 0 {
        log2_max_poc_lsb = br.ue()? as usize + 4;
    } else if poc_type == 1 {
        br.bit()?;
        br.ue()?;
        br.ue()?;
        let cycle = br.ue()?;
        for _ in 0..cycle {
            br.ue()?;
        }
    }
    br.ue()?;
    let gaps_flag_at = br.pos();
    br.bit()?;
    br.ue()?;
    br.ue()?;
    if br.bit()? == 0 {
        br.bit()?;
    }
    br.bit()?;
    if br.bit()? == 1 {
        br.ue()?;
        br.ue()?;
        br.ue()?;
        br.ue()?;
    }
    let vui_flag_at = br.pos();
    let vui_present = br.bit()? == 1;
    let prefix = vec![profile, constraints, level];
    Ok(H264SpsInfo {
        sps: H264Sps {
            log2_max_frame_num,
            poc_type,
            log2_max_poc_lsb,
        },
        rbsp,
        vui_flag_at,
        vui_present,
        gaps_flag_at,
        prefix,
    })
}

/// Rewrite one H.264 slice header prefix; `edit` sees
/// (`first_mb`, `slice_type`, `pps_id`, `frame_num`) and returns the
/// new tail (`pps_id`, `frame_num`) to write.
fn rewrite_h264_slice(
    nal: &[u8],
    log2_max_frame_num: usize,
    edit: impl Fn(u32, u32, u32, u32) -> (u32, u32),
) -> Result<Vec<u8>, String> {
    let header = nal[0];
    let rbsp = AnnexB::remove_emulation(&nal[1..]);
    let mut br = BitReader::new(&rbsp);
    let first_mb = br.ue()?;
    let slice_type = br.ue()?;
    let pps_id = br.ue()?;
    let frame_num = br.bits(log2_max_frame_num)?;
    let rest = br.rest();
    let (new_pps, new_frame) = edit(first_mb, slice_type, pps_id, frame_num);
    let mut bw = Bits::new();
    bw.byte(header);
    bw.ue(first_mb);
    bw.ue(slice_type);
    bw.ue(new_pps);
    bw.bits(
        new_frame,
        u32::try_from(log2_max_frame_num).expect("small count"),
    );
    bw.copy(&BitReader::strip_trailing(rest));
    Ok(AnnexB::insert_emulation(&bw.finish_rbsp()))
}

fn h264_slice_frame_num(nal: &[u8], log2_max_frame_num: usize) -> Result<u32, String> {
    let rbsp = AnnexB::remove_emulation(&nal[1..]);
    let mut br = BitReader::new(&rbsp);
    br.ue()?;
    br.ue()?;
    br.ue()?;
    Ok(br.bits(log2_max_frame_num)?)
}

/// Annex B of an mp4 synth vector via the mp4toannexb filter.
fn annexb_of(input: &Path, filter: &str) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(input);
    cmd.args(["-c", "copy", "-bsf:v", filter, "-f", "h264", "pipe:1"]);
    cmd.stdin(Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| format!("annexb extract failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("annexb extract failed: {err}"));
    }
    Ok(out.stdout)
}

fn craft_h264_sps_change(first: &[u8], second: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = first.to_vec();
    out.extend_from_slice(second);
    let nals = AnnexB::split_owned(&out);
    if nals.len() < 4 {
        return Err("sps-change concat too short".to_owned());
    }
    Ok(out)
}

fn craft_h264_slices_before_sets(annexb: &[u8]) -> Result<Vec<u8>, String> {
    let nals = AnnexB::split_owned(annexb);
    let first_sps = nals
        .iter()
        .position(|nal| AnnexB::h264_nal_type(nal) == Some(7))
        .ok_or("no SPS found")?;
    let first_slice = nals
        .iter()
        .position(|nal| matches!(AnnexB::h264_nal_type(nal), Some(1 | 5)))
        .ok_or("no slice found")?;
    if first_slice < first_sps {
        return Err("slices already precede sets".to_owned());
    }
    let mut out = nals;
    let slice = out.remove(first_slice);
    out.insert(first_sps, slice);
    Ok(AnnexB::join_units(&out))
}

fn craft_h264_vui_absent(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_of(mp4, "h264_mp4toannexb")?;
    let nals = AnnexB::split_owned(&annexb);
    let mut out = Vec::with_capacity(nals.len());
    let mut stripped = 0;
    for nal in &nals {
        if AnnexB::h264_nal_type(nal) != Some(7) {
            out.push(nal.clone());
            continue;
        }
        let info = parse_h264_sps(nal)?;
        if !info.vui_present {
            return Err("source SPS already lacks VUI".to_owned());
        }
        // Every field up to the VUI flag replays verbatim: copy raw bits
        // from the original RBSP between the fixed prefix and the flag.
        let mut raw = Bits::new();
        raw.byte(nal[0]);
        for byte in &info.prefix {
            raw.byte(*byte);
        }
        for at in (8 + 24)..info.vui_flag_at {
            raw.bit(info.rbsp[at / 8] >> (7 - at % 8) & 1 == 1);
        }
        raw.bit(false);
        out.push(AnnexB::insert_emulation(&raw.finish_rbsp()));
        stripped += 1;
    }
    if stripped == 0 {
        return Err("no SPS stripped".to_owned());
    }
    Ok(AnnexB::join_units(&out))
}

fn craft_h264_set_ids(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_of(mp4, "h264_mp4toannexb")?;
    let nals = AnnexB::split_owned(&annexb);
    let sps_at = nals
        .iter()
        .position(|nal| AnnexB::h264_nal_type(nal) == Some(7))
        .ok_or("no SPS found")?;
    let info = parse_h264_sps(&nals[sps_at])?;
    let prefix = info.prefix;
    let rbsp = info.rbsp;
    // SPS id starts right after the 4 fixed bytes.
    let mut probe = BitReader::new(&rbsp);
    probe.byte()?;
    probe.byte()?;
    probe.byte()?;
    probe.byte()?;
    probe.ue()?;
    let id_end = probe.pos();
    let mut bw = Bits::new();
    bw.byte(nals[sps_at][0]);
    for byte in &prefix {
        bw.byte(*byte);
    }
    bw.ue(2);
    let total = rbsp.len() * 8;
    let mut tail = Vec::new();
    for at in id_end..total {
        tail.push(rbsp[at / 8] >> (7 - at % 8) & 1 == 1);
    }
    bw.copy(&BitReader::strip_trailing(tail));
    let new_sps = AnnexB::insert_emulation(&bw.finish_rbsp());
    // Active sets use SPS id 2 (gap over 0-1); an inert PPS id 3
    // rides along parsed and unreferenced. Active non-zero PPS
    // ids stay out: libav corrupts such decodes (MB errors on
    // bit-exact rewrites), so the gap shape keeps PPS id 0 active.
    let mut out = Vec::with_capacity(nals.len());
    for nal in &nals {
        match AnnexB::h264_nal_type(nal) {
            Some(7) => out.push(new_sps.clone()),
            Some(8) => {
                let body = AnnexB::remove_emulation(&nal[1..]);
                let mut reader = BitReader::new(&body);
                let pic_id = reader.ue()?;
                if pic_id != 0 {
                    return Err("set-ids source PPS id drifted".to_owned());
                }
                reader.ue()?;
                let rest = reader.rest();
                let mut writer = Bits::new();
                writer.byte(nal[0]);
                writer.ue(0);
                writer.ue(2);
                writer.copy(&BitReader::strip_trailing(rest.clone()));
                out.push(AnnexB::insert_emulation(&writer.finish_rbsp()));
                let mut inert = Bits::new();
                inert.byte(nal[0]);
                inert.ue(3);
                inert.ue(2);
                inert.copy(&BitReader::strip_trailing(rest));
                out.push(AnnexB::insert_emulation(&inert.finish_rbsp()));
            }
            _ => out.push(nal.clone()),
        }
    }
    Ok(AnnexB::join_units(&out))
}

fn craft_h264_frame_num_gap(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_of(mp4, "h264_mp4toannexb")?;
    let nals = AnnexB::split_owned(&annexb);
    let sps_nal = nals
        .iter()
        .find(|nal| AnnexB::h264_nal_type(nal) == Some(7))
        .ok_or("no SPS found")?;
    let info = parse_h264_sps(sps_nal)?;
    let sps = info.sps;
    // Flip the SPS gaps flag on: x264 forbids gaps (single bit,
    // emulation recomputed).
    let mut sps_rbsp = info.rbsp;
    sps_rbsp[info.gaps_flag_at / 8] |= 1u8 << (7 - info.gaps_flag_at % 8);
    let gaps_sps = AnnexB::insert_emulation(&sps_rbsp);
    let modulus = 1 << sps.log2_max_frame_num.min(16);
    let mut seen = BTreeSet::new();
    for nal in &nals {
        if matches!(AnnexB::h264_nal_type(nal), Some(1 | 2 | 5)) {
            seen.insert(h264_slice_frame_num(nal, sps.log2_max_frame_num)?);
        }
    }
    let mut ordered: Vec<u32> = seen.into_iter().collect();
    ordered.sort_unstable();
    let target = *ordered.get(1).ok_or("fewer than two frame_nums")?;
    // A legal gap shifts every following number forward (IDRs keep 0
    // and re-arm the shift). The jump is 2: gap frames occupy DPB slots
    // as non-existing pictures, and the level-limited DPB here holds 4.
    let mut shifting = false;
    let mut patched = 0;
    let mut out = Vec::with_capacity(nals.len());
    for nal in &nals {
        if AnnexB::h264_nal_type(nal) == Some(7) {
            out.push(gaps_sps.clone());
            continue;
        }
        if !matches!(AnnexB::h264_nal_type(nal), Some(1 | 2 | 5)) {
            out.push(nal.clone());
            continue;
        }
        if AnnexB::h264_nal_type(nal) == Some(5) {
            shifting = false;
            out.push(nal.clone());
            continue;
        }
        let num = h264_slice_frame_num(nal, sps.log2_max_frame_num)?;
        if num == target {
            shifting = true;
        }
        if !shifting {
            out.push(nal.clone());
            continue;
        }
        out.push(rewrite_h264_slice(
            nal,
            sps.log2_max_frame_num,
            |_, _, p, f| (p, (f + 2) % modulus),
        )?);
        patched += 1;
    }
    if patched == 0 {
        return Err("no slice patched".to_owned());
    }
    Ok(AnnexB::join_units(&out))
}

fn craft_h264_si_slice(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_of(mp4, "h264_mp4toannexb")?;
    let nals = AnnexB::split_owned(&annexb);
    let sps_nal = nals
        .iter()
        .find(|nal| AnnexB::h264_nal_type(nal) == Some(7))
        .ok_or("no SPS found")?
        .clone();
    let pps_nal = nals
        .iter()
        .find(|nal| AnnexB::h264_nal_type(nal) == Some(8))
        .ok_or("no PPS found")?
        .clone();
    let sps = parse_h264_sps(&sps_nal)?.sps;
    let mut bw = Bits::new();
    bw.byte(0x41);
    bw.ue(0);
    bw.ue(8);
    bw.ue(0);
    bw.bits(
        1,
        u32::try_from(sps.log2_max_frame_num).expect("small count"),
    );
    match sps.poc_type {
        0 => bw.bits(2, u32::try_from(sps.log2_max_poc_lsb).expect("small count")),
        2 => {}
        other => return Err(format!("SI craft needs poc_type 0/2, got {other}")),
    }
    bw.bits(0x3f, 6);
    let slice = AnnexB::insert_emulation(&bw.finish_raw());
    Ok(AnnexB::join_units(&[sps_nal, pps_nal, slice]))
}

fn craft_h264_truncated_nal(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_of(mp4, "h264_mp4toannexb")?;
    let nals = AnnexB::split_owned(&annexb);
    let pps_at = nals
        .iter()
        .position(|nal| AnnexB::h264_nal_type(nal) == Some(8))
        .ok_or("no PPS found")?;
    let mut out = nals[..=pps_at].to_vec();
    let mut poison = Bits::new();
    poison.byte(0x65);
    poison.ue(0);
    poison.ue((1 << 24) - 1);
    out.push(AnnexB::insert_emulation(&poison.finish_raw()));
    Ok(AnnexB::join_units(&out))
}

fn craft_h264_fmo(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_of(mp4, "h264_mp4toannexb")?;
    let nals = AnnexB::split_owned(&annexb);
    let mut out = Vec::with_capacity(nals.len());
    let mut patched = 0;
    for nal in &nals {
        if AnnexB::h264_nal_type(nal) != Some(8) {
            out.push(nal.clone());
            continue;
        }
        let body = AnnexB::remove_emulation(&nal[1..]);
        let mut br = BitReader::new(&body);
        let pic_id = br.ue()?;
        let seq_id = br.ue()?;
        let cabac = br.bit()?;
        let bottom = br.bit()?;
        let groups = br.ue()?;
        if groups != 0 {
            return Err("source PPS already grouped".to_owned());
        }
        let rest = br.rest();
        let mut bw = Bits::new();
        bw.byte(nal[0]);
        bw.ue(pic_id);
        bw.ue(seq_id);
        bw.bit(cabac == 1);
        bw.bit(bottom == 1);
        bw.ue(1);
        bw.ue(0);
        bw.ue(100);
        bw.ue(100);
        bw.copy(&BitReader::strip_trailing(rest));
        out.push(AnnexB::insert_emulation(&bw.finish_rbsp()));
        patched += 1;
    }
    if patched == 0 {
        return Err("no PPS patched".to_owned());
    }
    Ok(AnnexB::join_units(&out))
}

/// Annex B of an HEVC mp4 synth vector.
fn annexb_hevc_of(input: &Path) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(input);
    cmd.args([
        "-c",
        "copy",
        "-bsf:v",
        "hevc_mp4toannexb",
        "-f",
        "hevc",
        "pipe:1",
    ]);
    cmd.stdin(Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| format!("annexb extract failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("annexb extract failed: {err}"));
    }
    Ok(out.stdout)
}

fn craft_h265_truncated_sets(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_hevc_of(mp4)?;
    let nals = AnnexB::split_owned(&annexb);
    let vps_at = nals
        .iter()
        .position(|nal| AnnexB::h265_nal_type(nal) == Some(32))
        .ok_or("no VPS found")?;
    let mut out = nals[..=vps_at].to_vec();
    let mut poison = Bits::new();
    poison.byte(0x42);
    poison.byte(0x01);
    poison.bits(0, 4);
    poison.bits(0, 3);
    poison.bit(true);
    for _ in 0..12 {
        poison.byte(0);
    }
    poison.ue(0);
    poison.ue(1);
    poison.ue((1 << 24) - 1);
    out.push(AnnexB::insert_emulation(&poison.finish_raw()));
    Ok(AnnexB::join_units(&out))
}

fn craft_h265_pps_unknown_sps(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_hevc_of(mp4)?;
    let nals = AnnexB::split_owned(&annexb);
    let mut out = Vec::with_capacity(nals.len());
    let mut patched = 0;
    for nal in &nals {
        if AnnexB::h265_nal_type(nal) != Some(34) {
            out.push(nal.clone());
            continue;
        }
        let body = AnnexB::remove_emulation(&nal[2..]);
        let mut br = BitReader::new(&body);
        let pic_id = br.ue()?;
        let _seq_id = br.ue()?;
        let rest = br.rest();
        let mut bw = Bits::new();
        bw.byte(nal[0]);
        bw.byte(nal[1]);
        bw.ue(pic_id);
        bw.ue(3);
        bw.copy(&BitReader::strip_trailing(rest));
        out.push(AnnexB::insert_emulation(&bw.finish_rbsp()));
        patched += 1;
    }
    if patched == 0 {
        return Err("no PPS patched".to_owned());
    }
    Ok(AnnexB::join_units(&out))
}

fn craft_h265_slice_unknown_pps(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_hevc_of(mp4)?;
    let nals = AnnexB::split_owned(&annexb);
    let mut out = Vec::with_capacity(nals.len());
    let mut patched = 0;
    for nal in &nals {
        let Some(nal_type) = AnnexB::h265_nal_type(nal) else {
            out.push(nal.clone());
            continue;
        };
        let slice = nal_type <= 21;
        if !slice || patched > 0 {
            out.push(nal.clone());
            continue;
        }
        let body = AnnexB::remove_emulation(&nal[2..]);
        let mut br = BitReader::new(&body);
        let first = br.bit()?;
        if first != 1 {
            return Err("first slice is dependent".to_owned());
        }
        let mut bw = Bits::new();
        bw.byte(nal[0]);
        bw.byte(nal[1]);
        bw.bit(true);
        if (16..=23).contains(&nal_type) {
            let noop = br.bit()?;
            bw.bit(noop == 1);
        }
        let _pps_id = br.ue()?;
        bw.ue(5);
        bw.copy(&BitReader::strip_trailing(br.rest()));
        out.push(AnnexB::insert_emulation(&bw.finish_rbsp()));
        patched += 1;
    }
    if patched == 0 {
        return Err("no slice patched".to_owned());
    }
    Ok(AnnexB::join_units(&out))
}

fn craft_h265_conform_overrun(mp4: &Path) -> Result<Vec<u8>, String> {
    let annexb = annexb_hevc_of(mp4)?;
    let nals = AnnexB::split_owned(&annexb);
    let sps_nal = nals
        .iter()
        .find(|nal| AnnexB::h265_nal_type(nal) == Some(33))
        .ok_or("no SPS found")?;
    let body = AnnexB::remove_emulation(&sps_nal[2..]);
    let mut br = BitReader::new(&body);
    br.bits(4)?;
    let max_sub_layers = br.bits(3)?;
    br.bit()?;
    if max_sub_layers != 0 {
        return Err("sub-layer PTL unsupported here".to_owned());
    }
    for _ in 0..12 {
        br.byte()?;
    }
    br.ue()?;
    let chroma = br.ue()?;
    if chroma == 3 {
        br.bit()?;
    }
    let width = br.ue()?;
    br.ue()?;
    let flag_at = br.pos();
    let had_window = br.bit()?;
    if had_window == 1 {
        br.ue()?;
        br.ue()?;
        br.ue()?;
        br.ue()?;
    }
    let rest = br.rest();
    let shift = u32::from(chroma == 1);
    let overrun = (width >> shift) + 8;
    // New RBSP: bits before the flag, flag set, overrunning window,
    // then the original tail verbatim.
    let mut bw = Bits::new();
    bw.byte(sps_nal[0]);
    bw.byte(sps_nal[1]);
    for at in 0..flag_at {
        bw.bit(body[at / 8] >> (7 - at % 8) & 1 == 1);
    }
    bw.bit(true);
    bw.ue(0);
    bw.ue(overrun);
    bw.ue(0);
    bw.ue(overrun);
    bw.copy(&BitReader::strip_trailing(rest));
    let new_sps = AnnexB::insert_emulation(&bw.finish_rbsp());
    let mut out = Vec::with_capacity(nals.len());
    for nal in &nals {
        if AnnexB::h265_nal_type(nal) == Some(33) {
            out.push(new_sps.clone());
        } else {
            out.push(nal.clone());
        }
    }
    Ok(AnnexB::join_units(&out))
}

fn craft_av1_bad_leb128(obu: &[u8]) -> Result<Vec<u8>, String> {
    let locs = walk_obus(obu)?;
    let first = &locs[0];
    let mut out = obu.to_vec();
    // Overwrite the first size field (and following payload bytes as
    // needed) with a non-terminating LEB128.
    let size_at = first.offset + first.header - leb_len(obu, first)?;
    for at in 0..9 {
        out[size_at + at] = 0xff;
    }
    Ok(out)
}

fn leb_len(obu: &[u8], loc: &ObuLoc) -> Result<usize, String> {
    let mut at = loc.offset + 1;
    let head = obu[loc.offset];
    if (head >> 2) & 1 == 1 {
        at += 1;
    }
    let (_, next) = read_leb128(obu, at)?;
    Ok(next - at)
}

fn craft_av1_frame_before_seqhdr(obu: &[u8]) -> Result<Vec<u8>, String> {
    let locs = walk_obus(obu)?;
    // Skip leading temporal delimiters, then drop the sequence header
    // so frame OBUs arrive with no sequence context.
    let seq = locs
        .iter()
        .find(|loc| loc.obu_type == 1)
        .ok_or("no sequence header found")?;
    if locs
        .iter()
        .all(|loc| loc.offset < seq.offset + seq.header + seq.payload)
    {
        return Err("no frames after the sequence header".to_owned());
    }
    let mut out = obu[..seq.offset].to_vec();
    out.extend_from_slice(&obu[seq.offset + seq.header + seq.payload..]);
    Ok(out)
}

fn craft_av1_render_size(ivf: &[u8]) -> Result<Vec<u8>, String> {
    if ivf.len() < 32 || &ivf[0..4] != b"DKIF" {
        return Err("not IVF".to_owned());
    }
    let mut out = ivf.to_vec();
    // IVF header display size away from the coded 640x360.
    out[12..14].copy_from_slice(&320u16.to_le_bytes());
    out[14..16].copy_from_slice(&180u16.to_le_bytes());
    Ok(out)
}

fn craft_vp9_show_existing(ivf: &[u8]) -> Result<Vec<u8>, String> {
    let (header, frames) = parse_ivf(ivf)?;
    let key = frames.first().ok_or("no keyframe")?;
    if key.data.first().is_some_and(|byte| byte & 0xc0 != 0x80) {
        return Err("first frame is not a VP9 keyframe".to_owned());
    }
    // Uncompressed header only: marker 0b10, profile 0, show_existing
    // set, slot 0. Exactly one byte.
    let shown = IvfFrame {
        pts: 1,
        data: vec![0x88],
    };
    build_ivf(
        &header,
        &[
            IvfFrame {
                pts: 0,
                data: key.data.clone(),
            },
            shown,
        ],
    )
}

fn craft_vp9_bad_marker(ivf: &[u8]) -> Result<Vec<u8>, String> {
    let (header, frames) = parse_ivf(ivf)?;
    let mut first = frames.into_iter().next().ok_or("no frames")?;
    let head = first.data.first_mut().ok_or("empty frame")?;
    *head &= 0x3f;
    build_ivf(&header, &[first])
}

fn craft_vp9_token_corrupt(ivf: &[u8]) -> Result<Vec<u8>, String> {
    let (header, mut frames) = parse_ivf(ivf)?;
    let first = frames.first_mut().ok_or("no frames")?;
    if first.data.len() < 64 {
        return Err("frame too small to corrupt".to_owned());
    }
    let start = first.data.len() / 3;
    let end = start + first.data.len() / 3;
    for at in (start..end).step_by(7) {
        first.data[at] ^= 0xff;
    }
    build_ivf(&header, &frames)
}

fn read_src(corpus: &Path, codec: &str, stem: &str) -> Result<(PathBuf, Vec<u8>), String> {
    let recipes = synths();
    let synth = recipes
        .get(&(codec, stem))
        .ok_or_else(|| format!("unknown craft source {codec}/{stem}"))?;
    let path = run_synth(corpus, synth)?;
    let bytes = fs::read(&path).map_err(|e| format!("craft source unreadable: {e}"))?;
    Ok((path, bytes))
}

#[allow(clippy::too_many_lines)]
fn run_craft(corpus: &Path, synth: &Synth, craft: Craft) -> Result<PathBuf, String> {
    let out = synth_path(corpus, synth);
    if out.is_file() {
        return Ok(out);
    }
    let bytes = match craft {
        Craft::Garbage { seed, prefix } => {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&garbage(seed, 4096 - prefix.len()));
            bytes
        }
        Craft::TruncateHalf | Craft::TruncateObu => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            if bytes.len() < 128 {
                return Err(format!("craft {} source too small", synth.name));
            }
            bytes[..bytes.len() / 2].to_vec()
        }
        Craft::TruncateAnnexB => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_truncated_nal(&path)?
        }
        Craft::TruncateSets => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h265_truncated_sets(&path)?
        }
        Craft::SpsChange => {
            let [first, second] = synth.craft_src else {
                return Err(format!("craft {} needs two sources", synth.name));
            };
            let (first_path, _) = read_src(corpus, synth.codec_dir, first)?;
            let (second_path, _) = read_src(corpus, synth.codec_dir, second)?;
            let a = annexb_of(&first_path, "h264_mp4toannexb")?;
            let b = annexb_of(&second_path, "h264_mp4toannexb")?;
            craft_h264_sps_change(&a, &b)?
        }
        Craft::SlicesBeforeSets => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_slices_before_sets(&annexb_of(&path, "h264_mp4toannexb")?)?
        }
        Craft::VuiAbsent => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_vui_absent(&path)?
        }
        Craft::SetIds => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_set_ids(&path)?
        }
        Craft::FrameNumGap => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_frame_num_gap(&path)?
        }
        Craft::SiSlice => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_si_slice(&path)?
        }
        Craft::Fmo => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h264_fmo(&path)?
        }
        Craft::PpsUnknownSps => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h265_pps_unknown_sps(&path)?
        }
        Craft::SliceUnknownPps => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h265_slice_unknown_pps(&path)?
        }
        Craft::ConformOverrun => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (path, _) = read_src(corpus, synth.codec_dir, src)?;
            craft_h265_conform_overrun(&path)?
        }
        Craft::BadLeb128 => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            craft_av1_bad_leb128(&bytes)?
        }
        Craft::FrameBeforeSeqHdr => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            craft_av1_frame_before_seqhdr(&bytes)?
        }
        Craft::RenderSize => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            craft_av1_render_size(&bytes)?
        }
        Craft::ShowExisting => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            craft_vp9_show_existing(&bytes)?
        }
        Craft::BadMarker => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            craft_vp9_bad_marker(&bytes)?
        }
        Craft::TokenCorrupt => {
            let [src] = synth.craft_src else {
                return Err(format!("craft {} needs one source", synth.name));
            };
            let (_, bytes) = read_src(corpus, synth.codec_dir, src)?;
            craft_vp9_token_corrupt(&bytes)?
        }
    };
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("synth dir failed: {e}"))?;
    }
    fs::write(&out, bytes).map_err(|e| format!("craft write failed: {e}"))?;
    Ok(out)
}

/// Framing elements the Annex B vectors must carry; fails the recipe
/// when the encoder output lacks one.
fn verify_annexb(codec: &str, annexb: &[u8]) -> Result<(), String> {
    let nals = AnnexB::split_owned(annexb);
    if nals.len() < 4 {
        return Err(format!("{codec} annexb holds {} NALs", nals.len()));
    }
    let types: BTreeSet<u8> = nals
        .iter()
        .filter_map(|nal| {
            if codec == "h264" {
                AnnexB::h264_nal_type(nal)
            } else {
                AnnexB::h265_nal_type(nal)
            }
        })
        .collect();
    let want: &[&[u8]] = if codec == "h264" {
        &[&[1], &[5], &[6], &[7], &[8], &[9]]
    } else {
        &[&[0, 1], &[19, 20, 21], &[32], &[33], &[34], &[35]]
    };
    for group in want {
        if !group.iter().any(|unit| types.contains(unit)) {
            return Err(format!("{codec} annexb lacks NAL type {group:?}"));
        }
    }
    let escapes = annexb.windows(3).any(|window| window == [0x00, 0x00, 0x03]);
    if !escapes {
        return Err(format!("{codec} annexb lacks RBSP escapes"));
    }
    Ok(())
}

/// True when ffmpeg decodes the vector cleanly: `-xerror` fails the
/// run on any decode error, and the progress counter must show at
/// least one frame (exit 0 alone also covers tolerated garbage).
fn oracle_decodes(path: &Path) -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-xerror", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-c:v",
        "rawvideo",
        "-progress",
        "pipe:1",
        "-f",
        "null",
        "-",
    ]);
    cmd.stdin(Stdio::null());
    let Ok(out) = cmd.output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("frame="))
        .filter_map(|count| count.trim().parse::<u64>().ok())
        .max()
        .is_some_and(|frames| frames >= 1)
}

/// Every synth vector is oracle-verified at build time: valid vectors
/// decode, decline vectors fail. Anything else is a recipe bug.
fn verify_vector(synth: &Synth, path: &Path) -> Result<(), String> {
    let decodes = oracle_decodes(path);
    match synth.expect {
        Expect::Decline(reason) => {
            if decodes {
                return Err(format!(
                    "decline vector {} decodes; reason `{reason}` is hollow",
                    synth.name
                ));
            }
        }
        Expect::Match | Expect::Either(_) => {
            if !decodes {
                return Err(format!("vector {} fails oracle decode", synth.name));
            }
        }
    }
    if synth.name == "annexb" {
        let bytes = fs::read(path).map_err(|e| format!("annexb reread failed: {e}"))?;
        verify_annexb(synth.codec_dir, &bytes)?;
    }
    Ok(())
}

fn run_synth(corpus: &Path, synth: &Synth) -> Result<PathBuf, String> {
    let out = match synth.derive {
        None => run_encode(corpus, synth)?,
        Some(Derive::Bsf(filter) | Derive::AnnexB(filter)) => run_bsf(corpus, synth, filter)?,
        Some(Derive::Craft(craft)) => run_craft(corpus, synth, craft)?,
    };
    // Re-verify cached vectors too: a stale corpus fails here.
    verify_vector(synth, &out)?;
    Ok(out)
}

fn recipe_of(synth: &Synth) -> String {
    match synth.derive {
        None => format!(
            "clip={} {}x{} {} threads=8 {} {}",
            synth.clip,
            synth.width,
            synth.height,
            synth.pix_fmt,
            synth.encoder,
            synth.extra.join(" ")
        ),
        Some(Derive::Bsf(filter)) => {
            format!("bsf {filter} over {}", synth.craft_src.join(","))
        }
        Some(Derive::AnnexB(filter)) => {
            format!("{} over {}", filter, synth.craft_src.join(","))
        }
        Some(Derive::Craft(_)) => {
            format!("craft over {}", synth.craft_src.join(","))
        }
    }
}

fn expect_of(synth: &Synth) -> String {
    match synth.expect {
        Expect::Match => "match".to_owned(),
        Expect::Either(reason) => format!("either {reason}"),
        Expect::Decline(reason) => format!("decline {reason}"),
    }
}

fn frame_root_from(tmpdir: Option<&OsStr>) -> PathBuf {
    tmpdir
        .filter(|set| !set.is_empty())
        .map_or_else(|| PathBuf::from(".tmp"), PathBuf::from)
}

fn frame_root() -> Result<PathBuf, String> {
    let root = frame_root_from(env::var_os("TMPDIR").as_deref());
    fs::create_dir_all(&root).map_err(|e| format!("frame root failed: {e}"))?;
    Ok(root)
}

fn raw_frame_size(path: &Path, pix_fmt: &str, slot: usize, root: &Path) -> Result<usize, String> {
    let probe = root.join(format!(
        "rust-video-golden-{}-{slot}.probe",
        std::process::id()
    ));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-noautoscale",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        "rawvideo",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
    ]);
    cmd.arg(&probe);
    cmd.stdin(Stdio::null());
    let out = cmd.output().map_err(|e| format!("spawn failed: {e}"))?;
    let size = fs::metadata(&probe).map_or(0, |found| found.len());
    let _ = fs::remove_file(&probe);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("decode {} failed: {err}", path.display()));
    }
    if size == 0 {
        return Err(format!("decode {} yielded no frames", path.display()));
    }
    usize::try_from(size).map_err(|_| format!("decode {} frame too large", path.display()))
}

enum Shapes {
    Unknown,
    Uniform,
    Varied,
}

fn parse_shapes(text: &str) -> Shapes {
    let mut first: Option<String> = None;
    let mut count = 0;
    for line in text.lines() {
        // Newer ffprobe appends trailing columns (empty or side-data)
        // to some rows; only width,height,pix_fmt matter.
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 3 || fields[..3].iter().any(|field| field.is_empty()) {
            return Shapes::Unknown;
        }
        let shape = fields[..3].join(",");
        count += 1;
        match first {
            None => first = Some(shape),
            Some(ref want) if *want == shape => {}
            Some(_) => return Shapes::Varied,
        }
    }
    if count == 0 {
        Shapes::Unknown
    } else {
        Shapes::Uniform
    }
}

fn frame_shapes(path: &Path) -> Shapes {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "frame=width,height,pix_fmt",
        "-of",
        "csv=p=0",
    ]);
    cmd.arg(path);
    let Ok(out) = cmd.output() else {
        return Shapes::Unknown;
    };
    if !out.status.success() {
        return Shapes::Unknown;
    }
    parse_shapes(&String::from_utf8_lossy(&out.stdout))
}

/// Spill ceiling for the file fallback (mixed-shape vectors like
/// the resolution-changing concat land here); past it generation fails.
const SPILL_CAP: u64 = 1024 * 1024 * 1024;

fn dir_size(dir: &Path) -> Result<u64, String> {
    let mut total = 0;
    let entries = fs::read_dir(dir).map_err(|e| format!("{} unreadable: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("frame entry failed: {e}"))?;
        total += entry.metadata().map_or(0, |found| found.len());
    }
    Ok(total)
}

fn file_hashes(
    path: &Path,
    pix_fmt: &str,
    slot: usize,
    root: &Path,
) -> Result<Vec<FrameHash>, String> {
    let tag = format!("rust-video-golden-{}-{slot}", std::process::id());
    let dir = root.join(&tag);
    if dir.is_dir() {
        fs::remove_dir_all(&dir).map_err(|e| format!("stale frame dir failed: {e}"))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("frame dir failed: {e}"))?;
    let pattern = dir.join("f%05d.bin");
    let err_path = root.join(format!("{tag}.stderr"));
    let err_file = fs::File::create(&err_path).map_err(|e| format!("stderr file failed: {e}"))?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-noautoscale",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        "rawvideo",
        "-fps_mode",
        "passthrough",
        "-f",
        "image2",
    ]);
    cmd.arg(&pattern);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(err_file);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            let _ = fs::remove_file(&err_path);
            return Err(format!("spawn failed: {e}"));
        }
    };
    loop {
        match child.try_wait() {
            Ok(None) => {
                let size = match dir_size(&dir) {
                    Ok(size) => size,
                    Err(why) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = fs::remove_dir_all(&dir);
                        let _ = fs::remove_file(&err_path);
                        return Err(why);
                    }
                };
                if size > SPILL_CAP {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_dir_all(&dir);
                    let _ = fs::remove_file(&err_path);
                    return Err(format!("decode {} spill cap exceeded", path.display()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(Some(status)) => {
                if !status.success() {
                    let err = fs::read_to_string(&err_path).unwrap_or_default();
                    let _ = fs::remove_dir_all(&dir);
                    let _ = fs::remove_file(&err_path);
                    return Err(format!("decode {} failed: {err}", path.display()));
                }
                break;
            }
            Err(e) => {
                let _ = child.kill();
                let _ = fs::remove_dir_all(&dir);
                let _ = fs::remove_file(&err_path);
                return Err(format!("wait failed: {e}"));
            }
        }
    }
    let mut names: Vec<PathBuf> = children(&dir)?
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    let mut hashes = Vec::with_capacity(names.len());
    for name in &names {
        let bytes = fs::read(name).map_err(|e| format!("frame read failed: {e}"))?;
        hashes.push(FrameHash::of(&bytes));
        fs::remove_file(name).map_err(|e| format!("frame cleanup failed: {e}"))?;
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("frame cleanup failed: {e}"))?;
    let _ = fs::remove_file(&err_path);
    if hashes.is_empty() {
        return Err(format!("decode {} yielded no frames", path.display()));
    }
    Ok(hashes)
}

fn pipe_hashes(
    path: &Path,
    pix_fmt: &str,
    slot: usize,
    root: &Path,
    size: usize,
) -> Result<Vec<FrameHash>, String> {
    use std::io::Read as _;
    let tag = format!("rust-video-golden-{}-{slot}", std::process::id());
    let err_path = root.join(format!("{tag}.stderr"));
    let err_file = fs::File::create(&err_path).map_err(|e| format!("stderr file failed: {e}"))?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-noautoscale",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        "rawvideo",
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(err_file);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = fs::remove_file(&err_path);
            return Err(format!("spawn failed: {e}"));
        }
    };
    let mut hashes = Vec::new();
    let mut frame = vec![0u8; size];
    let mut first = [0u8; 1];
    let failed = match child.stdout.take() {
        Some(stdout) => {
            let mut pipe = std::io::BufReader::new(stdout);
            loop {
                match pipe.read(&mut first) {
                    Ok(0) => break None,
                    Ok(_) => {
                        frame[0] = first[0];
                        match pipe.read_exact(&mut frame[1..]) {
                            Ok(()) => hashes.push(FrameHash::of(&frame)),
                            Err(_) => {
                                break Some(format!(
                                    "decode {} truncated mid-frame",
                                    path.display()
                                ));
                            }
                        }
                    }
                    Err(_) => {
                        break Some(format!("decode {} pipe read failed", path.display()));
                    }
                }
            }
        }
        None => Some(format!("decode {} pipe missing", path.display())),
    };
    let status = child.wait().map_err(|e| format!("wait failed: {e}"))?;
    if let Some(why) = failed {
        let _ = fs::remove_file(&err_path);
        return Err(why);
    }
    if !status.success() {
        let err = fs::read_to_string(&err_path).unwrap_or_default();
        let _ = fs::remove_file(&err_path);
        return Err(format!("decode {} failed: {err}", path.display()));
    }
    let _ = fs::remove_file(&err_path);
    if hashes.is_empty() {
        return Err(format!("decode {} yielded no frames", path.display()));
    }
    Ok(hashes)
}

fn decode_hashes(path: &Path, pix_fmt: &str, slot: usize) -> Result<Vec<FrameHash>, String> {
    let root = frame_root()?;
    if matches!(frame_shapes(path), Shapes::Uniform) {
        match raw_frame_size(path, pix_fmt, slot, &root) {
            Ok(size) => match pipe_hashes(path, pix_fmt, slot, &root, size) {
                Ok(hashes) => return Ok(hashes),
                Err(why) => println!("pipe retry as files {}: {why}", path.display()),
            },
            Err(why) => println!("pipe retry as files {}: {why}", path.display()),
        }
    }
    file_hashes(path, pix_fmt, slot, &root)
}

fn scan_dir(dir: &Path, codec: &str) -> Result<Vec<Vector>, String> {
    let files: Vec<PathBuf> = children(dir)?.into_iter().filter(|p| p.is_file()).collect();
    let mut out = Vec::new();
    for path in files {
        let decline = stem(&path).starts_with("decline-");
        // Decline vectors are unprobable by design; everything else
        // must probe or it skips loudly below.
        let probe = if decline {
            Probe {
                profile: "decline".to_owned(),
                pix_fmt: "decline".to_owned(),
                width: 0,
                height: 0,
                field_order: "decline".to_owned(),
                has_b_frames: "decline".to_owned(),
            }
        } else {
            let Some(probe) = keep(&path, probe(&path)) else {
                continue;
            };
            probe
        };
        out.push(Vector {
            codec_dir: codec.to_owned(),
            path,
            probe,
        });
    }
    Ok(out)
}

fn scan_corpus(root: &Path) -> Result<Vec<Vector>, String> {
    let dirs: Vec<PathBuf> = children(root)?.into_iter().filter(|p| p.is_dir()).collect();
    let mut out = Vec::new();
    for dir in dirs {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "synth" {
            let subs: Vec<PathBuf> = children(&dir)?.into_iter().filter(|p| p.is_dir()).collect();
            for sub in subs {
                let codec = sub.file_name().and_then(|n| n.to_str()).unwrap_or("");
                out.extend(scan_dir(&sub, codec)?);
            }
            continue;
        }
        out.extend(scan_dir(&dir, name)?);
    }
    Ok(out)
}

/// Vector path for golden headers: forward slashes on every
/// platform so runners resolve it without separator sniffing.
fn display_rel(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("vector")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Write one decline golden: v1 header with the decline expectation
/// and zero hashes. The oracle must refuse the vector or the recipe
/// is hollow and generation fails.
fn write_decline(
    target: &Path,
    vector: &Vector,
    synth: &Synth,
    reason: &str,
) -> Result<(), String> {
    if oracle_decodes(&vector.path) {
        return Err(format!(
            "decline vector {} decodes; reason `{reason}` is hollow",
            vector.path.display()
        ));
    }
    let text = format!(
        "# rust-video golden v1\n# vector: {}\n# expect: decline {reason}\n# frames: 0\n# recipe: {}\n",
        display_rel(&vector.path),
        recipe_of(synth),
    );
    let tmp = target.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| format!("golden write failed: {e}"))?;
    fs::rename(&tmp, target).map_err(|e| format!("golden swap failed: {e}"))?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn generate(
    goldens: &Path,
    vectors: &[Vector],
    ffmpeg: &str,
    recipes: &BTreeMap<(&'static str, &'static str), Synth>,
) -> Result<usize, String> {
    let fresh = goldens.with_extension("new");
    let old = goldens.with_extension("old");
    if fresh.is_dir() {
        fs::remove_dir_all(&fresh).map_err(|e| format!("stale .new cleanup failed: {e}"))?;
    }
    fs::create_dir_all(&fresh).map_err(|e| format!("fresh dir failed: {e}"))?;
    if goldens.is_dir() {
        for path in children(goldens)? {
            if path.is_file() && path.extension().is_some_and(|ext| ext != "sha256") {
                let name = path.file_name().ok_or("golden keep nameless")?;
                fs::copy(&path, fresh.join(name))
                    .map_err(|e| format!("golden keep failed: {e}"))?;
            }
        }
    }
    let mut made = 0;
    let mut skipped = 0;
    let mut seen: Vec<PathBuf> = Vec::new();
    for (slot, vector) in vectors.iter().enumerate() {
        let name = stem(&vector.path);
        let recipe = recipes.get(&(vector.codec_dir.as_str(), name.as_str()));
        let dir = fresh.join(&vector.codec_dir);
        fs::create_dir_all(&dir).map_err(|e| format!("golden dir failed: {e}"))?;
        let mut target = dir.join(format!("{name}.sha256"));
        let mut dup = 1;
        while seen.contains(&target) {
            dup += 1;
            target = dir.join(format!("{name}-{dup}.sha256"));
        }
        seen.push(target.clone());
        if let Some(synth) = recipe {
            if let Expect::Decline(reason) = synth.expect {
                write_decline(&target, vector, synth, reason)?;
                made += 1;
                continue;
            }
            let hashes = decode_hashes(&vector.path, &vector.probe.pix_fmt, slot)
                .map_err(|why| format!("matrix vector {} failed: {why}", vector.path.display()))?;
            let mut text = format!(
                "# rust-video golden v1\n# vector: {}\n# expect: {}\n# ffmpeg: {ffmpeg}\n# probe: {} {} {}x{} {} b={}\n# frames: {}\n# recipe: {}\n",
                display_rel(&vector.path),
                expect_of(synth),
                vector.probe.profile,
                vector.probe.pix_fmt,
                vector.probe.width,
                vector.probe.height,
                vector.probe.field_order,
                vector.probe.has_b_frames,
                hashes.len(),
                recipe_of(synth),
            );
            for hash in &hashes {
                text.push_str(&hash.hex());
                text.push('\n');
            }
            let tmp = target.with_extension("tmp");
            fs::write(&tmp, text).map_err(|e| format!("golden write failed: {e}"))?;
            fs::rename(&tmp, target).map_err(|e| format!("golden swap failed: {e}"))?;
            made += 1;
            continue;
        }
        // Fetched extras outside the matrix: hash when decodable,
        // skip loudly otherwise.
        let Ok(probe) = probe(&vector.path) else {
            println!("skip {}: unprobable fetched extra", vector.path.display());
            skipped += 1;
            continue;
        };
        let Some(hashes) = keep(
            &vector.path,
            decode_hashes(&vector.path, &probe.pix_fmt, slot),
        ) else {
            skipped += 1;
            continue;
        };
        let mut text = format!(
            "# rust-video golden v1\n# vector: {}\n# expect: match\n# ffmpeg: {ffmpeg}\n# probe: {} {} {}x{} {} b={}\n# frames: {}\n",
            display_rel(&vector.path),
            probe.profile,
            probe.pix_fmt,
            probe.width,
            probe.height,
            probe.field_order,
            probe.has_b_frames,
            hashes.len(),
        );
        for hash in &hashes {
            text.push_str(&hash.hex());
            text.push('\n');
        }
        let tmp = target.with_extension("tmp");
        fs::write(&tmp, text).map_err(|e| format!("golden write failed: {e}"))?;
        fs::rename(&tmp, target).map_err(|e| format!("golden swap failed: {e}"))?;
        made += 1;
    }
    println!("skipped: {skipped}");
    if made == 0 {
        return Err("floor failed: no goldens made".to_owned());
    }
    if goldens.is_dir() {
        if old.is_dir() {
            fs::remove_dir_all(&old).map_err(|e| format!("stale .old cleanup failed: {e}"))?;
        }
        fs::rename(goldens, &old).map_err(|e| format!("swap to .old failed: {e}"))?;
    }
    fs::rename(&fresh, goldens).map_err(|e| format!("swap failed: {e}"))?;
    if old.is_dir() {
        fs::remove_dir_all(&old).map_err(|e| format!("old cleanup failed: {e}"))?;
    }
    Ok(made)
}

fn coverage<'a>(
    vectors: &'a [Vector],
    reqs: &'a [Req],
) -> (Vec<(&'a Req, &'a Vector)>, Vec<&'a Req>) {
    let mut covered = Vec::new();
    let mut missing = Vec::new();
    for req in reqs {
        if req.blocked.is_some() {
            continue;
        }
        match vectors
            .iter()
            .find(|v| v.codec_dir == req.codec && stem(&v.path) == req.name)
        {
            Some(hit) => covered.push((req, hit)),
            None => missing.push(req),
        }
    }
    (covered, missing)
}

fn print_matrix() {
    println!("codec/line -> clause -> synth|fetch|blocked");
    for req in reqs() {
        let via = match (req.blocked, req.synth, req.fetch) {
            (Some(why), _, _) => format!("BLOCKED {why}"),
            (None, Some(name), _) => format!("synth {name}"),
            (None, None, Some(file)) => format!("fetch {file}"),
            (None, None, None) => "fetch".to_owned(),
        };
        println!("{}/{} -> {} -> {via}", req.codec, req.name, req.clause);
    }
}

fn usage() -> ! {
    eprintln!("usage: golden-gen [--corpus DIR] [--goldens DIR] [--synth] [--list-required]");
    std::process::exit(2);
}

fn main() {
    if let Err(why) = run() {
        eprintln!("golden-gen: {why}");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_lines)]
fn run() -> Result<(), String> {
    let mut corpus = PathBuf::from("corpus");
    let mut goldens = PathBuf::from("crates/rust-video-harness/goldens");
    let mut synth = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--corpus" => {
                corpus = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("missing --corpus DIR")?;
            }
            "--goldens" => {
                goldens = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("missing --goldens DIR")?;
            }
            "--synth" => synth = true,
            "--list-required" => {
                print_matrix();
                return Ok(());
            }
            _ => usage(),
        }
    }
    if synth {
        prove_clips()?;
        let recipes = synths();
        let wanted = reqs();
        let mut ran = Vec::new();
        let mut failed = Vec::new();
        for req in wanted.iter().filter(|req| req.blocked.is_none()) {
            let Some(name) = req.synth else { continue };
            if ran.contains(&(req.codec, name)) {
                continue;
            }
            let Some(recipe) = recipes.get(&(req.codec, name)) else {
                failed.push(format!("{}/{}: recipe missing", req.codec, req.name));
                continue;
            };
            match run_synth(&corpus, recipe) {
                Ok(_) => ran.push((req.codec, name)),
                Err(why) => failed.push(format!("{}/{}: {why}", req.codec, req.name)),
            }
        }
        println!("synthesized: {}", ran.len());
        if !failed.is_empty() {
            let mut msg = String::from("synth failures:\n");
            for failure in &failed {
                writeln!(msg, "- {failure}").expect("String write");
            }
            return Err(msg);
        }
    }
    if !corpus.is_dir() {
        return Err(format!(
            "corpus {} absent: run with --synth first",
            corpus.display()
        ));
    }
    let ffmpeg = ffmpeg_version()?;
    let vectors = scan_corpus(&corpus)?;
    let recipes = synths();
    let made = generate(&goldens, &vectors, &ffmpeg, &recipes)?;
    println!("goldens: {made}");
    let wanted = reqs();
    let (covered, missing) = coverage(&vectors, &wanted);
    for (req, hit) in &covered {
        println!("cover {}/{} <- {}", req.codec, req.name, hit.path.display());
    }
    for req in reqs().iter().filter(|r| r.blocked.is_some()) {
        println!(
            "BLOCKED {}/{}: {}",
            req.codec,
            req.name,
            req.blocked.unwrap_or("?")
        );
    }
    if missing.is_empty() {
        return Ok(());
    }
    let mut msg = String::from("uncovered options:\n");
    for req in missing {
        let fix = match (req.synth, req.fetch) {
            (Some(name), _) => format!("run with --synth ({name})"),
            (None, Some(file)) => format!("run just vectors to fetch {file}"),
            (None, None) => format!("fetch a {} vector exercising {}", req.codec, req.name),
        };
        writeln!(msg, "- {}/{}: {fix}", req.codec, req.name).expect("String write");
    }
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::frame_root_from;
    use super::{build_ivf, garbage, parse_ivf, parse_shapes, read_leb128, walk_obus, Shapes};
    use rust_video_harness::{AnnexB, BitReader, Bits};
    use std::path::PathBuf;

    #[test]
    fn root_prefers_tmpdir() {
        assert_eq!(
            frame_root_from(Some("/scratch/tmp".as_ref())),
            PathBuf::from("/scratch/tmp")
        );
    }

    #[test]
    fn root_falls_back_to_repo_tmp() {
        assert_eq!(frame_root_from(None), PathBuf::from(".tmp"));
        assert_eq!(frame_root_from(Some("".as_ref())), PathBuf::from(".tmp"));
    }

    #[test]
    fn shapes_classifies_uniformity() {
        assert!(matches!(
            parse_shapes("352,288,yuv420p\n352,288,yuv420p\n"),
            Shapes::Uniform
        ));
        assert!(matches!(
            parse_shapes("352,288,yuv420p\n176,144,yuv420p\n"),
            Shapes::Varied
        ));
        assert!(matches!(parse_shapes(""), Shapes::Unknown));
        assert!(matches!(parse_shapes("352,288\n"), Shapes::Unknown));
    }

    #[test]
    fn shapes_ignores_trailing_columns() {
        assert!(matches!(
            parse_shapes("1920,1080,yuv420p,\n1920,1080,yuv420p\n"),
            Shapes::Uniform
        ));
    }

    #[test]
    fn bits_roundtrip_ue() {
        for value in [0, 1, 2, 5, 100, 1000, 65535] {
            let mut bw = Bits::new();
            bw.ue(value);
            bw.ue(7);
            bw.bit(true);
            let bytes = bw.finish_raw();
            let mut br = BitReader::new(&bytes);
            assert_eq!(br.ue().expect("ue"), value);
            assert_eq!(br.ue().expect("ue"), 7);
            assert_eq!(br.bit().expect("bit"), 1);
        }
    }

    #[test]
    fn emulation_roundtrips() {
        let raw = vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x03, 0x27, 0x00, 0x00, 0x02];
        let coded = AnnexB::insert_emulation(&raw);
        assert_eq!(AnnexB::remove_emulation(&coded), raw);
    }

    #[test]
    fn annexb_roundtrips() {
        let nals = vec![vec![0x67, 0x42], vec![0x68, 0xce], vec![0x65, 0x01, 0x02]];
        let joined = AnnexB::join_units(&nals);
        assert_eq!(AnnexB::split_owned(&joined), nals);
    }

    #[test]
    fn leb128_reads_sizes() {
        let (value, next) = read_leb128(&[0x81, 0x01, 0x00], 0).expect("leb");
        assert_eq!((value, next), (129, 2));
        assert!(read_leb128(&[0xff; 9], 0).is_err());
    }

    #[test]
    fn obus_walk_synthetic() {
        // Two size-bearing OBUs: type 1 (2 payload bytes), type 3 (empty).
        let bytes = vec![0x0a, 0x02, 0xaa, 0xbb, 0x1a, 0x00];
        let locs = walk_obus(&bytes).expect("walk");
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].obu_type, 1);
        assert_eq!(locs[0].payload, 2);
        assert_eq!(locs[1].obu_type, 3);
    }

    #[test]
    fn ivf_roundtrips() {
        let mut header = vec![0u8; 32];
        header[0..4].copy_from_slice(b"DKIF");
        let frames = vec![
            super::IvfFrame {
                pts: 0,
                data: vec![1, 2, 3],
            },
            super::IvfFrame {
                pts: 1,
                data: vec![4, 5],
            },
        ];
        let bytes = build_ivf(&header, &frames).expect("build");
        let (back_header, back_frames) = parse_ivf(&bytes).expect("parse");
        assert_eq!(back_header, header);
        assert_eq!(back_frames.len(), 2);
        assert_eq!(back_frames[1].pts, 1);
    }

    #[test]
    fn garbage_is_deterministic() {
        assert_eq!(garbage(42, 100), garbage(42, 100));
        assert_ne!(garbage(42, 100), garbage(43, 100));
    }
}
