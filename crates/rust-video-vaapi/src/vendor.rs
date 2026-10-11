//! Vendored H.264 frontend from `cros-codecs` 0.0.6, BSD-3-Clause.
//!
//! Covers the NAL/SPS/PPS/slice-header parser, the DPB, and the
//! bitstream reader they share. See `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance and the trimmed pieces.

mod bitstream_utils;
pub(crate) mod h264;
mod types;
