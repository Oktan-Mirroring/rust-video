// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! AV1 syntax: OBUs, sequence and frame headers, and tiles.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance.

#[cfg(target_os = "linux")]
pub(crate) mod decoder;
#[cfg(test)]
pub(crate) mod fixture;
pub(crate) mod frontend;
pub(crate) mod helpers;
pub(crate) mod params;
pub(crate) mod parser;
pub(crate) mod reader;

#[cfg(target_os = "linux")]
pub use decoder::Av1Decoder;
