//! True ffmpeg end-to-end baseline over `cuda`: cuvid, cuda filters, nvenc.

use std::process::{Command, Stdio};
use std::time::Instant;

use rust_video_harness::bench_dir;

use crate::content::Codec;
use crate::measure::{median_of_5, Sample};

/// Baseline outcome for one codec.
pub enum Baseline {
    /// Timed runs plus the chain.
    Ran {
        /// Median frames per second.
        fps: f64,
        /// Relative spread around the median.
        spread: f64,
        /// Frames per pass.
        frames: usize,
        /// Outlier rerun round ran.
        reran: bool,
        /// Filter chain.
        chain: String,
    },
    /// Declined with reason.
    Declined(String),
}

/// Cuvid decoder, cuda filters, nvenc for `codec`, median of 5.
///
/// The chain carries no `hwdownload`: frames stay on the GPU from
/// decode to encode. VP9 has no nvenc encoder and cuvid tags its
/// frames with a colorspace swscale rejects, so its chain runs the
/// native decoder plus CPU scale into `libvpx-vp9`.
pub fn baseline(codec: Codec) -> Baseline {
    let Some(vector) = vector(codec) else {
        return Baseline::Declined("no bench vector".to_owned());
    };
    if Command::new("ffmpeg")
        .args(["-hide_banner", "-version"])
        .output()
        .is_err()
    {
        return Baseline::Declined("no ffmpeg on PATH".to_owned());
    }
    let chain = chain(codec);
    let args = args(codec, &vector, &chain);
    if let Err(error) = timed(&args) {
        return Baseline::Declined(error);
    }
    let (fps, spread, frames, reran) = median_of_5(|| {
        timed(&args).unwrap_or(Sample {
            frames: 0,
            fps: 0.0,
        })
    });
    Baseline::Ran {
        fps,
        spread,
        frames,
        reran,
        chain,
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

/// Filter chain for `codec`: downscale plus studio-to-full range convert.
fn chain(codec: Codec) -> String {
    match codec {
        Codec::Vp9 => "scale=960:540,format=yuv420p".to_owned(),
        _ => "scale_cuda=960:540,colorspace_cuda=pc".to_owned(),
    }
}

/// Full ffmpeg arguments for `codec` over `vector` with `chain`.
fn args(codec: Codec, vector: &str, chain: &str) -> Vec<String> {
    let (demux, decoder, encoder) = match codec {
        Codec::H264 => ("h264", "h264_cuvid", "h264_nvenc"),
        Codec::H265 => ("hevc", "hevc_cuvid", "hevc_nvenc"),
        Codec::Av1 => ("ivf", "av1_cuvid", "av1_nvenc"),
        Codec::Vp9 => ("ivf", "vp9", "libvpx-vp9"),
    };
    let mut args = vec!["-hide_banner".to_owned()];
    if codec != Codec::Vp9 {
        args.extend([
            "-hwaccel".to_owned(),
            "cuda".to_owned(),
            "-hwaccel_output_format".to_owned(),
            "cuda".to_owned(),
        ]);
    }
    args.extend([
        "-c:v".to_owned(),
        decoder.to_owned(),
        "-f".to_owned(),
        demux.to_owned(),
        "-i".to_owned(),
        vector.to_owned(),
        "-vf".to_owned(),
        chain.to_owned(),
        "-c:v".to_owned(),
        encoder.to_owned(),
    ]);
    if codec == Codec::Vp9 {
        args.extend([
            "-speed".to_owned(),
            "8".to_owned(),
            "-b:v".to_owned(),
            "0".to_owned(),
            "-crf".to_owned(),
            "30".to_owned(),
        ]);
    }
    args.extend(["-f".to_owned(), "null".to_owned(), "-".to_owned()]);
    args
}

/// One timed ffmpeg pass.
fn timed(args: &[String]) -> Result<Sample, String> {
    let start = Instant::now();
    let output = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("run ffmpeg: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr
            .split(['\r', '\n'])
            .rfind(|line| !line.trim().is_empty());
        return Err(format!(
            "ffmpeg exit {}: {}",
            output.status,
            tail.unwrap_or("failed")
        ));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let frames = progress(&stderr, "frame=")
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "no progress frames".to_owned())?;
    let wall = start.elapsed().as_secs_f64();
    let fps = progress_fps(&stderr)
        .unwrap_or_else(|| f64::from(u32::try_from(frames).unwrap_or(u32::MAX)) / wall.max(1e-9));
    Ok(Sample { frames, fps })
}

/// Last `key` progress value in ffmpeg stderr.
///
/// `frame=` detaches its value (`frame= 120`); `fps=` attaches it (`fps=12.3`).
fn progress<'a>(stderr: &'a str, key: &str) -> Option<&'a str> {
    let tokens: Vec<&str> = stderr
        .split(['\r', '\n'])
        .flat_map(|line| line.split_whitespace())
        .collect();
    let mut value = None;
    let mut index = 0;
    while index < tokens.len() {
        if let Some(rest) = tokens[index].strip_prefix(key) {
            if rest.is_empty() {
                value = tokens.get(index + 1).copied();
                index += 1;
            } else {
                value = Some(rest);
            }
        }
        index += 1;
    }
    value
}

/// Last nonzero `fps` progress value: the final line reads zero on fast runs.
fn progress_fps(stderr: &str) -> Option<f64> {
    let tokens: Vec<&str> = stderr
        .split(['\r', '\n'])
        .flat_map(|line| line.split_whitespace())
        .collect();
    let mut value = None;
    for token in tokens {
        if let Some(rest) = token.strip_prefix("fps=") {
            if let Ok(fps) = rest.parse::<f64>() {
                if fps > 0.0 {
                    value = Some(fps);
                }
            }
        }
    }
    value
}
