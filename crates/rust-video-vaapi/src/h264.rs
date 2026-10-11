// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! H.264 syntax: NAL units, parameter sets, slices, and the DPB.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance.

pub(crate) mod dpb;
pub(crate) mod nalu;
pub(crate) mod parser;
pub(crate) mod picture;
