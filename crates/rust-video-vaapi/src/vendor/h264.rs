// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Vendored H.264 parser and DPB; see `super` for provenance.

pub(crate) mod dpb;
pub(crate) mod nalu;
pub(crate) mod parser;
pub(crate) mod picture;
