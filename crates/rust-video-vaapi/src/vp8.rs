// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! VP8 syntax: staged parser, typed decline.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance. The parser and boolean decoder
//! live on disk under `src/vp8/` and join the build when the frontend is
//! wired; until then every packet declines with typed `Unsupported`.

use crate::decoder::VaApiFrame;
use crate::error::VaApiError;

use rust_video::{Decoder, Packet};

/// VP8 decoder handle: frontend staged, every packet declines typed.
pub struct Vp8Decoder;

impl Decoder for Vp8Decoder {
    type Frame = VaApiFrame;
    type Error = VaApiError;

    fn decode(&mut self, _packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        Err(VaApiError::Unsupported("vp8 frontend not wired".to_owned()))
    }

    fn flush(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::Vp8Decoder;
    use crate::error::VaApiError;

    use bytes::Bytes;
    use rust_video::{Decoder, Packet};

    #[test]
    fn decode_declines_typed() {
        let mut decoder = Vp8Decoder;
        let packet = Packet::new(Bytes::from_static(&[0, 1, 2]));
        let Err(VaApiError::Unsupported(message)) = decoder.decode(&packet) else {
            panic!("staged frontend declines");
        };
        assert!(message.contains("vp8"));
    }

    #[test]
    fn flush_holds_nothing() {
        let mut decoder = Vp8Decoder;
        assert!(decoder.flush().expect("empty flush").is_empty());
    }
}
