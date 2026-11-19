//! Software pipeline: ffmpeg decode, CPU filter, ffmpeg encode.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use rust_video_harness::bench_dir;

use crate::content::{Clip, Codec, Op};
use crate::measure::{median_of_5, Sample};
use crate::Outcome;

/// Frames per software pass; full-clip AV1 CPU encode would stall the suite.
const PASS_FRAMES: usize = 30;

/// Software backend over the ffmpeg CLI.
pub struct Backend;

impl Backend {
    /// Backend when ffmpeg answers on `PATH`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without an ffmpeg binary.
    pub fn open() -> Result<Self, String> {
        if Command::new("ffmpeg")
            .args(["-hide_banner", "-version"])
            .output()
            .is_err()
        {
            return Err("no ffmpeg on PATH".to_owned());
        }
        Ok(Self)
    }

    /// Decode, filter on the CPU, re-encode: median of 5.
    pub fn run(codec: Codec, op: Op, clip: &Clip) -> Outcome {
        let vector = vector(codec);
        let Some(vector) = vector else {
            return Outcome::Declined("no bench vector".to_owned());
        };
        if op != Op::Gray && (!clip.width().is_multiple_of(2) || !clip.height().is_multiple_of(2)) {
            return Outcome::Declined("odd extent".to_owned());
        }
        let frames = clip.raws().len().min(PASS_FRAMES);
        if let Err(error) = Self::pass(codec, op, clip, &vector, frames) {
            return Outcome::Declined(error);
        }
        let (fps, spread, _, reran) = median_of_5(|| {
            Self::pass(codec, op, clip, &vector, frames).unwrap_or(Sample {
                frames: 0,
                fps: 0.0,
            })
        });
        Outcome::Ran {
            fps,
            spread,
            frames,
            reran,
            note: format!("cpu {} {frames}f, {}", op.name(), encoder(codec)),
        }
    }

    /// One decode, filter, encode pass.
    fn pass(
        codec: Codec,
        op: Op,
        clip: &Clip,
        vector: &str,
        frames: usize,
    ) -> Result<Sample, String> {
        let start = Instant::now();
        let raw = Self::decode(codec, vector, frames)?;
        let frame_len = frame_len(clip.width(), clip.height());
        if raw.len() < frame_len * frames {
            return Err(format!("short decode: {} bytes", raw.len()));
        }
        let (filtered, width, height) =
            filter(op, &raw[..frame_len * frames], clip.width(), clip.height());
        let stream = Self::encode(codec, &filtered, width, height)?;
        if stream.is_empty() {
            return Err("encoder emitted no bytes".to_owned());
        }
        crate::dump("sw", codec, op, "mkv", &stream);
        let elapsed = start.elapsed().as_secs_f64();
        Ok(Sample {
            frames,
            fps: f64::from(u32::try_from(frames).unwrap_or(u32::MAX)) / elapsed.max(1e-9),
        })
    }

    /// Raw NV12 bytes for the first `frames` pictures.
    fn decode(codec: Codec, vector: &str, frames: usize) -> Result<Vec<u8>, String> {
        let output = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-f",
                demux(codec),
                "-i",
                vector,
                "-frames:v",
                &frames.to_string(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "nv12",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("decode spawn: {error}"))?;
        if !output.status.success() {
            return Err(format!("decode exit {}", output.status));
        }
        Ok(output.stdout)
    }

    /// Coded bytes for filtered NV12 `raw` at `width` by `height`.
    fn encode(codec: Codec, raw: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        let mut command = Command::new("ffmpeg");
        command.args([
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "nv12",
            "-s",
            &format!("{width}x{height}"),
            "-i",
            "-",
            "-c:v",
            encoder(codec),
        ]);
        command.args(speed(codec));
        let mut child = command
            .args(["-f", "matroska", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("encode spawn: {error}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "no encode stdin".to_owned())?;
        stdin
            .write_all(raw)
            .map_err(|error| format!("encode stdin: {error}"))?;
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|error| format!("encode wait: {error}"))?;
        if !output.status.success() {
            return Err(format!("encode exit {}", output.status));
        }
        Ok(output.stdout)
    }
}

/// NV12 bytes for one `width` by `height` frame.
fn frame_len(width: u32, height: u32) -> usize {
    usize::try_from(u64::from(width) * u64::from(height) * 3 / 2).unwrap_or(0)
}

/// Filtered NV12 bytes plus output extent.
fn filter(op: Op, raw: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let stride = frame_len(width, height);
    if stride == 0 {
        return (Vec::new(), width, height);
    }
    let mut out = Vec::new();
    for frame in raw.chunks(stride) {
        if frame.len() < stride {
            break;
        }
        let (y, uv) = frame.split_at(y_len(width, height));
        match op {
            Op::Gray => {
                out.extend_from_slice(y);
                out.extend(std::iter::repeat_n(0x80, uv.len()));
            }
            Op::Down => out.extend(down_frame(y, uv, width, height)),
            Op::Color => {
                let converted = range_frame(y, uv);
                let (cy, cuv) = converted.split_at(y_len(width, height));
                out.extend(down_frame(cy, cuv, width, height));
            }
            Op::Crop => out.extend(crop_frame(y, uv, width, height)),
        }
    }
    let (width, height) = match op {
        Op::Gray => (width, height),
        Op::Down | Op::Color | Op::Crop => (width / 2, height / 2),
    };
    (out, width, height)
}

/// Luma bytes for one `width` by `height` frame.
fn y_len(width: u32, height: u32) -> usize {
    usize::try_from(u64::from(width) * u64::from(height)).unwrap_or(0)
}

/// Halved frame by 2x2 box.
fn down_frame(y: &[u8], uv: &[u8], width: u32, height: u32) -> Vec<u8> {
    let width = usize::try_from(width).unwrap_or(0);
    let height = usize::try_from(height).unwrap_or(0);
    let mut out = Vec::with_capacity(width * height * 3 / 8);
    for row in (0..height).step_by(2) {
        for col in (0..width).step_by(2) {
            let sum = u32::from(y[row * width + col])
                + u32::from(y[row * width + col + 1])
                + u32::from(y[(row + 1) * width + col])
                + u32::from(y[(row + 1) * width + col + 1]);
            out.push(u8::try_from(sum / 4).unwrap_or(u8::MAX));
        }
    }
    for row in (0..height / 2).step_by(2) {
        for col in (0..width).step_by(4) {
            for pair in 0..2 {
                let sum = u32::from(uv[row * width + col + pair])
                    + u32::from(uv[row * width + col + pair + 2])
                    + u32::from(uv[(row + 1) * width + col + pair])
                    + u32::from(uv[(row + 1) * width + col + pair + 2]);
                out.push(u8::try_from(sum / 4).unwrap_or(u8::MAX));
            }
        }
    }
    out
}

/// Studio-range frame expanded to full range at full extent.
fn range_frame(luma: &[u8], chroma: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(luma.len() + chroma.len());
    out.extend(luma.iter().map(|sample| expand(*sample, 16, 219)));
    out.extend(chroma.iter().map(|sample| expand(*sample, 16, 224)));
    out
}

/// Studio `sample` expanded from `foot` over `span` to full range.
fn expand(sample: u8, foot: i32, span: i32) -> u8 {
    clamp((i32::from(sample) - foot) * 255 / span.max(1))
}

/// Byte clamp of `value`.
fn clamp(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

/// Demuxer for `codec`.
fn demux(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::H265 => "hevc",
        Codec::Av1 | Codec::Vp9 => "ivf",
    }
}

/// CPU encoder for `codec`.
fn encoder(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "libx264",
        Codec::H265 => "libx265",
        Codec::Av1 => "libsvtav1",
        Codec::Vp9 => "libvpx-vp9",
    }
}

/// Speed flags for `codec`.
fn speed(codec: Codec) -> &'static [&'static str] {
    match codec {
        Codec::H264 | Codec::H265 => &["-preset", "ultrafast"],
        Codec::Av1 => &["-preset", "8"],
        Codec::Vp9 => &[
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-b:v",
            "0",
            "-crf",
            "30",
        ],
    }
}

/// Bench vector path for `codec`.
fn vector(codec: Codec) -> Option<String> {
    let file = match codec {
        Codec::H264 => "h264-1080p_ip.h264",
        Codec::H265 => "h265-1080p_ip.h265",
        Codec::Av1 => "av1-1080p_ip.ivf",
        Codec::Vp9 => "vp9-1080p_ip.ivf",
    };
    let path = bench_dir().join(file);
    path.is_file().then(|| path.to_string_lossy().into_owned())
}

/// Center quarter window at full resolution.
fn crop_frame(y: &[u8], uv: &[u8], width: u32, height: u32) -> Vec<u8> {
    let width = usize::try_from(width).unwrap_or(0);
    let height = usize::try_from(height).unwrap_or(0);
    let (left, top) = (width / 4, height / 4);
    let (cw, ch) = (width / 2, height / 2);
    let mut out = Vec::with_capacity(cw * ch * 3 / 2);
    for row in 0..ch {
        out.extend_from_slice(&y[(top + row) * width + left..(top + row) * width + left + cw]);
    }
    for row in 0..ch / 2 {
        let src = (top / 2 + row) * width + left;
        out.extend_from_slice(&uv[src..src + cw]);
    }
    out
}
