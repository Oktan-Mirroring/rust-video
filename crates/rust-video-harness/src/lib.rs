//! Vector suites and golden oracles.
//!
//! Corpus-gated end-to-end proof plus shared synthetic inputs.

mod alloc_tracker;
mod annexb;
mod bits;
mod drive;
mod error;
mod golden;
mod h264_synth;
mod hash;
mod ivf;
mod ivf_encode;
mod mp4;
mod obu;
mod oracle;
mod report;
mod suite;
mod synth;
mod vectors;

pub use alloc_tracker::{assert_no_allocs_after_warmup, CountingAlloc};
pub use annexb::AnnexB;
pub use bits::{BitOverrun, BitReader, Bits};
pub use drive::{Drive, Packets, Parity};
pub use error::Error;
pub use golden::{Golden, Mismatch};
pub use h264_synth::H264Synth;
pub use hash::FrameHash;
pub use ivf::Ivf;
pub use ivf_encode::IvfEncode;
pub use mp4::Stream;
pub use obu::Obu;
pub use oracle::Oracle;
pub use report::{Outcome, Report};
pub use suite::{Case, Suite};
pub use synth::Synth;
pub use vectors::{bench_dir, decode_dir};
