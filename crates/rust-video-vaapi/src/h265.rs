// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! H.265 syntax: NAL units, parameter sets, slices, and the DPB.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance.

#[cfg(target_os = "linux")]
pub(crate) mod decoder;
pub(crate) mod dpb;
#[cfg(test)]
pub(crate) mod fixture;
pub(crate) mod frontend;
pub(crate) mod params;
pub(crate) mod parser;
pub(crate) mod picture;

#[cfg(target_os = "linux")]
pub use decoder::H265Decoder;
