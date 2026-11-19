// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! AV1 OBU ingest over the shared grammar.

#[cfg(target_os = "linux")]
pub(crate) mod decoder;
#[allow(dead_code)]
pub(crate) mod frontend;
#[allow(dead_code)]
pub(crate) mod params;

#[cfg(target_os = "linux")]
pub use crate::av1::decoder::Av1Decoder;

pub(crate) use rust_video_av1::{
    frame_obu_tile_offset, parse_frame_header, parse_sequence_header, refresh_refs,
    show_existing_refs, split_obus, split_tiles, tile_group_range, FilmGrain, FrameHeader,
    GlobalMotion, RefInfo, SequenceHeader, MAX_SEGMENTS, NUM_REF_FRAMES, OBU_FRAME,
    OBU_FRAME_HEADER, OBU_SEQUENCE_HEADER, OBU_TILE_GROUP, OBU_TILE_LIST, REFS_PER_FRAME,
    SEG_LEVELS, SEQ_COLOR_RANGE, SUPERRES_NUM,
};

/// Shared frame type behind submit tests.
#[cfg(test)]
pub(crate) use rust_video_av1::FrameType;

use rust_video_av1::{
    BrokenReferenceReason as Av1Broken, Error as Av1Error, ParseReason as Av1Parse,
    UnsupportedReason as Av1Unsupported,
};

use crate::error::VaApiError;

impl From<Av1Error> for VaApiError {
    fn from(error: Av1Error) -> Self {
        match error {
            Av1Error::Parse(reason) => Self::Parse(match reason {
                Av1Parse::FrameBeforeSequenceHeader => "frame before sequence header".to_owned(),
                Av1Parse::FrameWithoutTileData => "frame without tile data".to_owned(),
                Av1Parse::IncompleteTileGroup => "incomplete tile group".to_owned(),
                Av1Parse::ObuExtensionReservedBits => "obu extension reserved bits".to_owned(),
                Av1Parse::ObuForbiddenBit => "obu forbidden bit".to_owned(),
                Av1Parse::ObuReservedBit => "obu reserved bit".to_owned(),
                Av1Parse::ObuSizeExceedsPayload => "obu size exceeds payload".to_owned(),
                Av1Parse::OverflowI32 { syntax } | Av1Parse::OverflowU32 { syntax } => {
                    format!("read number cannot fit in 32 bits at {syntax}")
                }
                Av1Parse::Range { syntax } => format!("{syntax} out of range"),
                Av1Parse::RedundantHeaderMismatch => "redundant header mismatch".to_owned(),
                Av1Parse::ReservedObuType => "reserved obu type".to_owned(),
                Av1Parse::TileGridCoverage => "tile grid coverage".to_owned(),
                Av1Parse::TileGroupOrder => "tile group order".to_owned(),
                Av1Parse::TileGroupWithoutData => "tile group without data".to_owned(),
                Av1Parse::TileGroupWithoutFrameHeader => {
                    "tile group without frame header".to_owned()
                }
                Av1Parse::TooWide { syntax } => {
                    format!("more than 31 bits were requested at {syntax}")
                }
                Av1Parse::Truncated { syntax } => format!("reader ran out of bits at {syntax}"),
            }),
            Av1Error::Unsupported(reason) => Self::Unsupported(match reason {
                Av1Unsupported::Av1BitDepthNeedsTarget { depth } => {
                    format!("av1 bit depth {depth} needs target")
                }
                Av1Unsupported::Av1ChromaOutside420 => "av1 chroma outside 4:2:0".to_owned(),
                Av1Unsupported::Av1Monochrome => "av1 monochrome".to_owned(),
                Av1Unsupported::RenderSizeDiffersFromFrameSize => {
                    "render size differs from frame size".to_owned()
                }
                Av1Unsupported::SpatialLayers => "spatial layers".to_owned(),
                Av1Unsupported::SuperresUpscale => "superres upscale".to_owned(),
                Av1Unsupported::TileCountAbove256 => "tile count above 256".to_owned(),
                Av1Unsupported::TileList => "large-scale tiles".to_owned(),
            }),
            Av1Error::BrokenReference(reason) => Self::BrokenReference(match reason {
                Av1Broken::NamesEmptySlot { reference } => {
                    format!("{reference} names empty slot")
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_video_av1::{BrokenReferenceReason, Error, ParseReason, UnsupportedReason};

    use crate::error::VaApiError;
    use crate::fixtures::{keyframe_payload, seq_payload};

    #[test]
    fn shared_grammar_agrees_on_fixture_stream() {
        let shared =
            rust_video_av1::parse_sequence_header::<VaApiError>(&seq_payload()).expect("seq");
        assert_eq!(shared.profile, 0);
        assert_eq!(
            (
                shared.max_frame_width_minus1,
                shared.max_frame_height_minus1
            ),
            (63, 63)
        );

        let mut refs = core::array::from_fn(|_| rust_video_av1::RefInfo::default());
        let (shared_frame, _) = rust_video_av1::parse_frame_header::<VaApiError>(
            &keyframe_payload(),
            &shared,
            &mut refs,
            0,
            0,
        )
        .expect("frame");
        assert!(shared_frame.frame_type.is_intra());
        assert_eq!(
            (shared_frame.frame_width, shared_frame.frame_height),
            (64, 64)
        );
    }

    #[test]
    fn errors_map_onto_tier_errors() {
        assert_eq!(
            VaApiError::from(Error::Parse(ParseReason::ObuForbiddenBit)).to_string(),
            "parse failed: obu forbidden bit"
        );
        assert_eq!(
            VaApiError::from(Error::Parse(ParseReason::Range {
                syntax: "tile data"
            }))
            .to_string(),
            "parse failed: tile data out of range"
        );
        assert_eq!(
            VaApiError::from(Error::Unsupported(UnsupportedReason::TileList)).to_string(),
            "unsupported stream: large-scale tiles"
        );
        assert_eq!(
            VaApiError::from(Error::BrokenReference(
                BrokenReferenceReason::NamesEmptySlot {
                    reference: "show_existing_frame",
                },
            ))
            .to_string(),
            "broken reference list: show_existing_frame names empty slot"
        );
    }
}
