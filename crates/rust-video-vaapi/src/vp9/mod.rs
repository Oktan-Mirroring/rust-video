// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! VP9 decode over the shared grammar: ingest, submit, decode.

#[cfg(target_os = "linux")]
pub(crate) mod decoder;
#[allow(dead_code)]
pub(crate) mod frontend;
#[allow(dead_code)]
pub(crate) mod lookups;
#[allow(dead_code)]
pub(crate) mod params;

#[cfg(target_os = "linux")]
pub use crate::vp9::decoder::Vp9Decoder;

pub(crate) use rust_video_vp9::{FrameHeader, MAX_SEGMENTS, NUM_REF_FRAMES};

/// Shared VP9 ingest over tier errors.
pub(crate) type Parser = rust_video_vp9::Ingest<crate::error::VaApiError>;

impl From<rust_video_vp9::Fault> for crate::error::VaApiError {
    fn from(fault: rust_video_vp9::Fault) -> Self {
        match fault {
            rust_video_vp9::Fault::NamesEmptySlot { .. } => {
                Self::BrokenReference(fault.to_string())
            }
            _ => Self::Parse(fault.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;
    use crate::error::VaApiError;
    use crate::vp9::frontend::Frontend;

    use bytes::Bytes;
    use rust_video_harness::Bits;

    /// Profile 0 keyframe at `width` x `height`, one tile.
    fn keyframe(width: u32, height: u32) -> Vec<u8> {
        let mut writer = Bits::new();
        writer.bits(0x02, 2);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0x49_83_42, 24);
        writer.bits(1, 3);
        writer.bit(false);
        writer.bits(width - 1, 16);
        writer.bits(height - 1, 16);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 2);
        writer.bits(10, 6);
        writer.bits(2, 3);
        writer.bit(false);
        writer.bits(100, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bits(0, 16);
        let mut bytes = writer.finish_raw();
        bytes.extend_from_slice(&[0xAA; 8]);
        bytes
    }

    #[test]
    fn bad_marker_surfaces_tier_error() {
        let error = Parser::new().ingest_packet(&[0x00]).unwrap_err();
        assert!(matches!(error, VaApiError::Parse(_)));
        assert_eq!(error.to_string(), "parse failed: frame marker mismatch");
    }

    #[test]
    fn keyframe_feeds_submit_params() {
        use crate::vp9::frontend::Begun;
        use crate::vp9::params::Params;

        let bytes = Bytes::from(keyframe(320, 240));
        let mut frontend = Frontend::new();
        let mut ingest = frontend.ingest(&bytes).expect("ingest");
        assert!(ingest.reconfigured());
        assert_eq!(frontend.coded_size(), Some((320, 240)));
        assert_eq!(frontend.profile(), Some(0));
        let pending = ingest.frames().pop().expect("frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("begin") else {
            panic!("regular picture");
        };
        assert_eq!(
            (picture.header().frame_width, picture.header().frame_height),
            (320, 240)
        );
        let params = Params::new(&[]);
        let picture_params = params
            .picture(picture.header(), picture.refs())
            .expect("picture params");
        assert_eq!(picture_params.frame_width, 320);
        assert_eq!(picture_params.frame_height, 240);
        assert_eq!(picture_params.filter_level, 10);
        assert_eq!(picture_params.sharpness_level, 2);
        assert_eq!(picture_params.log2_tile_columns, 0);
        assert_eq!(picture_params.profile, 0);
        assert_eq!(picture_params.bit_depth, 8);
        let slice_params = Params::slice(picture.header(), picture.data().len()).expect("slice");
        assert_eq!(
            slice_params.slice_data_size,
            u32::try_from(picture.data().len()).expect("small frame")
        );
    }
}
