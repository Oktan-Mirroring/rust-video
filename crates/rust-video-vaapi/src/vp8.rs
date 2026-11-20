// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! VP8 syntax: staged parser, typed decline.
//!
//! Assimilated from `cros-codecs` 0.0.6; see `docs/vaapi.md` and
//! `LICENSE.cros-codecs` for provenance. The parser and boolean decoder
//! live on disk under `src/vp8/` and join the build when the frontend is
//! wired; until then every packet declines with typed `Unsupported`.

use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::h264::decoder::VaApiFrame;

use rust_video_caps::packet::Packet;
use rust_video_caps::reason::UnsupportedReason as CapsUnsupported;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

/// VP8 decoder handle: frontend staged, every packet declines typed.
pub struct Vp8Decoder;

impl Vp8Decoder {
    /// Decoder over `device`; the frontend stays staged.
    #[must_use]
    pub fn open(_device: &VaApiDevice) -> Self {
        Self
    }
}

impl Decoder for Vp8Decoder {
    type Frame = VaApiFrame;
    type Error = VaApiError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, _packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        Err(VaApiError::Unsupported(CapsUnsupported::ProfileOutsideMain))
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
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;

    #[test]
    fn decode_declines_typed() {
        let mut decoder = Vp8Decoder;
        let packet = Packet::new(Bytes::from_static(&[0, 1, 2]));
        let Err(VaApiError::Unsupported(reason)) = decoder.decode(&packet) else {
            panic!("staged frontend declines");
        };
        assert_eq!(
            reason,
            rust_video_caps::reason::UnsupportedReason::ProfileOutsideMain
        );
    }

    #[test]
    fn open_builds_handle() {
        let Ok(device) = crate::device::VaApiDevice::open() else {
            return;
        };
        let mut decoder = Vp8Decoder::open(&device);
        let packet = Packet::new(Bytes::from_static(&[0, 1, 2]));
        assert!(matches!(
            decoder.decode(&packet),
            Err(VaApiError::Unsupported(_))
        ));
    }

    #[test]
    fn flush_holds_nothing() {
        let mut decoder = Vp8Decoder;
        assert!(decoder.flush().expect("empty flush").is_empty());
    }

    #[test]
    fn caps_report_no_share() {
        use rust_video_caps::transcode::GpuCaps;

        let decoder = Vp8Decoder;
        assert_eq!(decoder.caps(), GpuCaps::new(None, false, false));
    }
}
