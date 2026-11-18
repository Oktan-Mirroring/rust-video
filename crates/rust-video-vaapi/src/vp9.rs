// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! VP9 syntax: superframes, frame headers, and tiles.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance.

#[cfg(target_os = "linux")]
pub(crate) mod decoder;
pub(crate) mod frontend;
pub(crate) mod lookups;
pub(crate) mod params;
pub(crate) mod parser;

#[cfg(target_os = "linux")]
pub use decoder::Vp9Decoder;
