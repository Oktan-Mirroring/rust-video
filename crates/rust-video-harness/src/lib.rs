//! Vector suites and golden oracles.
//!
//! Corpus-gated end-to-end proof for decoders.

mod alloc_tracker;
mod error;
mod golden;
mod hash;
mod oracle;
mod report;
mod suite;

pub use alloc_tracker::{assert_no_allocs_after_warmup, CountingAlloc};
pub use error::Error;
pub use golden::{Golden, Mismatch};
pub use hash::FrameHash;
pub use oracle::Oracle;
pub use report::{Outcome, Report};
pub use suite::{Case, Suite};
