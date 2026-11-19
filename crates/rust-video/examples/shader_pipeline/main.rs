//! Decode, GPU filter, re-encode across backends and codecs.
//!
//! One pipeline per backend, codec, and filter operation, timed at
//! median of 5 against an ffmpeg `cuda` baseline. Filters run on the
//! GPU without readback; declines name their missing hop.
//!
//! Positional filters narrow the matrix: `[backend] [codec] [op]`.
//! `$SHADER_PIPE_OUT`, when set to a directory, collects the
//! preflight bitstream of every combo for offline verification.

mod content;
#[cfg(all(target_os = "windows", feature = "d3d11"))]
mod d3d11;
#[cfg(all(target_os = "windows", feature = "d3d12"))]
mod d3d12;
mod driver;
mod ffmpeg;
mod measure;
mod sw;
#[cfg(feature = "vulkan")]
mod vulkan;

use content::{Clip, Codec, Op};
use driver::Pipeline;

/// Combo outcome: timed runs or a named decline.
pub enum Outcome {
    /// Timed runs plus the combo note.
    Ran {
        /// Median frames per second.
        fps: f64,
        /// Relative spread around the median.
        spread: f64,
        /// Frames per pass.
        frames: usize,
        /// Outlier rerun round ran.
        reran: bool,
        /// Combo note.
        note: String,
    },
    /// Declined with reason.
    Declined(String),
}

/// Filtered matrix run: `[backend] [codec] [op]`.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (backend, codec, op) = (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    );
    let codecs = filtered_codecs(codec);
    let ops = filtered_ops(op);
    run_d3d11(backend, &codecs, &ops);
    run_d3d12(backend, &codecs, &ops);
    run_vulkan(backend, &codecs, &ops);
    run_sw(backend, &codecs, &ops);
    run_native(backend, "vaapi", "no example filter for vaapi");
    run_native(backend, "mediacodec", "no example filter for mediacodec");
    run_ffmpeg(backend, &codecs);
}

/// Codecs under the CLI filter, or all.
fn filtered_codecs(filter: Option<&str>) -> Vec<Codec> {
    match filter {
        None => Codec::ALL.to_vec(),
        Some(name) => Codec::parse(name).into_iter().collect(),
    }
}

/// Operations under the CLI filter, or all.
fn filtered_ops(filter: Option<&str>) -> Vec<Op> {
    match filter {
        None => Op::ALL.to_vec(),
        Some(name) => Op::parse(name).into_iter().collect(),
    }
}

/// D3D11 combos, or the decline line.
#[cfg(all(target_os = "windows", feature = "d3d11"))]
fn run_d3d11(backend: Option<&str>, codecs: &[Codec], ops: &[Op]) {
    if !wanted(backend, "d3d11") {
        return;
    }
    let backend = match d3d11::Backend::open() {
        Ok(backend) => backend,
        Err(error) => {
            println!("d3d11: declined: {error}");
            return;
        }
    };
    for codec in codecs {
        let clip = match Clip::load(*codec) {
            Ok(clip) => clip,
            Err(error) => {
                println!("d3d11/{}: declined: {error}", codec.name());
                continue;
            }
        };
        for op in ops {
            report("d3d11", *codec, *op, backend.run(*codec, *op, &clip));
        }
    }
}

/// D3D11 decline outside Windows or without the feature.
#[cfg(not(all(target_os = "windows", feature = "d3d11")))]
fn run_d3d11(backend: Option<&str>, _codecs: &[Codec], _ops: &[Op]) {
    if wanted(backend, "d3d11") {
        println!("d3d11: declined: needs windows plus the d3d11 feature");
    }
}

/// D3D12 combos, or the decline line.
#[cfg(all(target_os = "windows", feature = "d3d12"))]
fn run_d3d12(backend: Option<&str>, codecs: &[Codec], ops: &[Op]) {
    if !wanted(backend, "d3d12") {
        return;
    }
    let backend = match d3d12::Backend::open() {
        Ok(backend) => backend,
        Err(error) => {
            println!("d3d12: declined: {error}");
            return;
        }
    };
    for codec in codecs {
        let clip = match Clip::load(*codec) {
            Ok(clip) => clip,
            Err(error) => {
                println!("d3d12/{}: declined: {error}", codec.name());
                continue;
            }
        };
        for op in ops {
            report("d3d12", *codec, *op, backend.run(*codec, *op, &clip));
        }
    }
}

/// D3D12 decline outside Windows or without the feature.
#[cfg(not(all(target_os = "windows", feature = "d3d12")))]
fn run_d3d12(backend: Option<&str>, _codecs: &[Codec], _ops: &[Op]) {
    if wanted(backend, "d3d12") {
        println!("d3d12: declined: needs windows plus the d3d12 feature");
    }
}

/// Vulkan combos, or the decline line.
#[cfg(feature = "vulkan")]
fn run_vulkan(backend: Option<&str>, codecs: &[Codec], ops: &[Op]) {
    if !wanted(backend, "vulkan") {
        return;
    }
    let backend = match vulkan::Backend::open() {
        Ok(backend) => backend,
        Err(error) => {
            println!("vulkan: declined: {error}");
            return;
        }
    };
    for codec in codecs {
        let clip = match Clip::load(*codec) {
            Ok(clip) => clip,
            Err(error) => {
                println!("vulkan/{}: declined: {error}", codec.name());
                continue;
            }
        };
        for op in ops {
            report("vulkan", *codec, *op, backend.run(*codec, *op, &clip));
        }
    }
}

/// Vulkan decline without the feature.
#[cfg(not(feature = "vulkan"))]
fn run_vulkan(backend: Option<&str>, _codecs: &[Codec], _ops: &[Op]) {
    if wanted(backend, "vulkan") {
        println!("vulkan: declined: needs the vulkan feature");
    }
}

/// Software combos.
fn run_sw(backend: Option<&str>, codecs: &[Codec], ops: &[Op]) {
    if !wanted(backend, "sw") {
        return;
    }
    let backend = match sw::Backend::open() {
        Ok(backend) => backend,
        Err(error) => {
            println!("sw: declined: {error}");
            return;
        }
    };
    for codec in codecs {
        let clip = match Clip::load(*codec) {
            Ok(clip) => clip,
            Err(error) => {
                println!("sw/{}: declined: {error}", codec.name());
                continue;
            }
        };
        for op in ops {
            report("sw", *codec, *op, backend.run(*codec, *op, &clip));
        }
    }
}

/// One-line decline for an unimplemented native tier.
fn run_native(backend: Option<&str>, name: &str, reason: &str) {
    if wanted(backend, name) {
        println!("{name}: declined: {reason}");
    }
}

/// Ffmpeg baselines per codec.
fn run_ffmpeg(backend: Option<&str>, codecs: &[Codec]) {
    if !wanted(backend, "ffmpeg") {
        return;
    }
    for codec in codecs {
        match ffmpeg::baseline(*codec) {
            ffmpeg::Baseline::Ran {
                fps,
                spread,
                frames,
                reran,
                chain,
            } => {
                println!(
                    "ffmpeg/{}: {fps:.1}fps spread={:.1}% frames={frames}{} [{chain}]",
                    codec.name(),
                    spread * 100.0,
                    if reran { " reran" } else { "" },
                );
            }
            ffmpeg::Baseline::Declined(error) => {
                println!("ffmpeg/{}: declined: {error}", codec.name());
            }
        }
    }
}

/// One combo report line.
fn report(backend: &str, codec: Codec, op: Op, outcome: Outcome) {
    match outcome {
        Outcome::Ran {
            fps,
            spread,
            frames,
            reran,
            note,
        } => {
            println!(
                "{}/{}/{}: {fps:.1}fps spread={:.1}% frames={frames}{} [{note}]",
                backend,
                codec.name(),
                op.name(),
                spread * 100.0,
                if reran { " reran" } else { "" },
            );
        }
        Outcome::Declined(error) => {
            println!(
                "{}/{}/{}: declined: {error}",
                backend,
                codec.name(),
                op.name()
            );
        }
    }
}

/// True when the CLI filter selects `name`.
fn wanted(filter: Option<&str>, name: &str) -> bool {
    filter.is_none_or(|want| want.eq_ignore_ascii_case(name) || want.eq_ignore_ascii_case("all"))
}

/// Preflight bitstream to `$SHADER_PIPE_OUT`, when set, for offline verification.
pub fn dump(backend: &str, codec: Codec, op: Op, ext: &str, bytes: &[u8]) {
    let Ok(dir) = std::env::var("SHADER_PIPE_OUT") else {
        return;
    };
    let path =
        std::path::Path::new(&dir).join(format!("{backend}_{}_{}.{ext}", codec.name(), op.name()));
    let _ = std::fs::write(path, bytes);
}
