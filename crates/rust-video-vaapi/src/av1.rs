// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! AV1 syntax: staged parser, typed decline.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance. The OBU parser and helpers live
//! on disk under `src/av1/` and join the build when the frontend is
//! wired; until then every packet declines with typed `Unsupported`.

use crate::decoder::VaApiFrame;
use crate::error::VaApiError;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;

/// AV1 decoder handle: frontend staged, every packet declines typed.
pub struct Av1Decoder;

impl Decoder for Av1Decoder {
    type Frame = VaApiFrame;
    type Error = VaApiError;

    fn decode(&mut self, _packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        Err(VaApiError::Unsupported("av1 frontend not wired".to_owned()))
    }

    fn flush(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::Av1Decoder;
    use crate::error::VaApiError;

    use bytes::Bytes;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;

    #[test]
    fn decode_declines_typed() {
        let mut decoder = Av1Decoder;
        let packet = Packet::new(Bytes::from_static(&[0, 1, 2]));
        let Err(VaApiError::Unsupported(message)) = decoder.decode(&packet) else {
            panic!("staged frontend declines");
        };
        assert!(message.contains("av1"));
    }

    #[test]
    fn flush_holds_nothing() {
        let mut decoder = Av1Decoder;
        assert!(decoder.flush().expect("empty flush").is_empty());
    }
}
