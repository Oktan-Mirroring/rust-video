//! VP9 ingest over the shared grammar, mapped onto tier errors.
//!
//! Grammar in `rust-video-vp9`; submit builders in `dpb`.

use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason};

pub(crate) use rust_video_vp9::{Frame, FrameHeader, FrameType, NUM_REF_FRAMES, REFS_PER_FRAME};

/// Shared tile and flag vocabulary behind submit tests.
#[cfg(test)]
pub(crate) use rust_video_vp9::{
    TileData, FH_INTRA_ONLY, FH_SEG_ABS_DELTA, FH_SEG_ENABLED, FH_SEG_TEMPORAL_UPDATE,
    FH_SEG_UPDATE_MAP, FH_SHOW_EXISTING, FH_SHOW_FRAME, FH_SUBSAMPLING_X, FH_SUBSAMPLING_Y,
};

/// Shared VP9 ingest over tier errors.
pub(crate) type Ingest = rust_video_vp9::Ingest<D3d11Error>;

impl From<rust_video_vp9::Fault> for D3d11Error {
    fn from(fault: rust_video_vp9::Fault) -> Self {
        match fault {
            rust_video_vp9::Fault::FrameMarkerMismatch => {
                Self::Parse(ParseReason::FrameMarkerMismatch)
            }
            rust_video_vp9::Fault::ReservedProfile => Self::Parse(ParseReason::ReservedProfile),
            rust_video_vp9::Fault::SyncCodeMismatch => Self::Parse(ParseReason::SyncCodeMismatch),
            rust_video_vp9::Fault::Range { syntax } => Self::Parse(ParseReason::Range { syntax }),
            rust_video_vp9::Fault::Truncated { syntax } => {
                Self::Parse(ParseReason::Truncated { syntax })
            }
            rust_video_vp9::Fault::TooWide { syntax } => {
                Self::Parse(ParseReason::TooWide { syntax })
            }
            rust_video_vp9::Fault::NamesEmptySlot { reference } => {
                Self::BrokenReference(BrokenReferenceReason::NamesEmptySlot { reference })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ingest;
    use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason};

    use rust_video_harness::Bits;

    /// Profile 0 keyframe head through segmentation at `width` x `height`.
    fn key_head(writer: &mut Bits, width: u32, height: u32) {
        writer.bits(2, 2);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0x49_83_42, 24);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bits(width - 1, 16);
        writer.bits(height - 1, 16);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bits(0, 2);
        writer.bits(10, 6);
        writer.bits(2, 3);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bits(5, 6);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(100, 8);
        writer.bit(true);
        writer.bits(3, 4);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        for _ in 0..7 {
            writer.bit(true);
            writer.bits(128, 8);
        }
        writer.bit(true);
        for _ in 0..3 {
            writer.bit(true);
            writer.bits(200, 8);
        }
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bits(5, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bits(2, 2);
        writer.bit(true);
        for _ in 1..8 {
            writer.bits(0, 4);
        }
    }

    /// Synthetic keyframe, 4096x16 with 4x2 tiles.
    fn tiled_4096x16() -> Vec<u8> {
        let mut writer = Bits::new();
        key_head(&mut writer, 4096, 16);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 16);
        let mut bytes = writer.finish_raw();
        for tile in 0..7u8 {
            let size = u32::from(tile) + 1;
            bytes.extend_from_slice(&size.to_be_bytes());
            let len = usize::try_from(size).expect("tile fits");
            bytes.extend(std::iter::repeat_n(0xA0 + tile, len));
        }
        bytes.extend_from_slice(&[0xA7, 0xA7, 0xA7, 0xA7, 0xA7, 0xA7, 0xA7, 0xA7]);
        bytes
    }

    #[test]
    fn dpb_slices_land_on_tile_payloads() {
        let bytes = tiled_4096x16();
        let frames = Ingest::new().ingest_packet(&bytes).expect("tiled");
        assert_eq!(frames.len(), 1);
        let mut dpb = crate::vp9::dpb::Vp9Dpb::new();
        let decoded = dpb.decode(&frames[0]).expect("dpb accepts key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slices.len(), frames[0].tiles.len());
        for (slice, tile) in submission.slices.iter().zip(frames[0].tiles.iter()) {
            let start = usize::try_from(slice.location).expect("offset fits");
            let len = usize::try_from(slice.bytes).expect("size fits");
            assert_eq!(
                &submission.bitstream[start..start + len],
                tile.bytes.as_slice()
            );
        }
    }

    #[test]
    fn faults_map_onto_tier_errors() {
        use rust_video_vp9::Fault;
        assert!(matches!(
            D3d11Error::from(Fault::FrameMarkerMismatch),
            D3d11Error::Parse(ParseReason::FrameMarkerMismatch)
        ));
        assert!(matches!(
            D3d11Error::from(Fault::ReservedProfile),
            D3d11Error::Parse(ParseReason::ReservedProfile)
        ));
        assert!(matches!(
            D3d11Error::from(Fault::SyncCodeMismatch),
            D3d11Error::Parse(ParseReason::SyncCodeMismatch)
        ));
        assert!(matches!(
            D3d11Error::from(Fault::Range {
                syntax: "tile_data"
            }),
            D3d11Error::Parse(ParseReason::Range { .. })
        ));
        assert!(matches!(
            D3d11Error::from(Fault::Truncated {
                syntax: "tile_data"
            }),
            D3d11Error::Parse(ParseReason::Truncated { .. })
        ));
        assert!(matches!(
            D3d11Error::from(Fault::TooWide {
                syntax: "tile_data"
            }),
            D3d11Error::Parse(ParseReason::TooWide { .. })
        ));
        assert!(matches!(
            D3d11Error::from(Fault::NamesEmptySlot {
                reference: "ref_frame_idx",
            }),
            D3d11Error::BrokenReference(BrokenReferenceReason::NamesEmptySlot { .. })
        ));
        assert_eq!(
            D3d11Error::from(Fault::FrameMarkerMismatch).to_string(),
            "parse failed: frame marker mismatch"
        );
        assert_eq!(
            D3d11Error::from(Fault::ReservedProfile).to_string(),
            "parse failed: reserved profile"
        );
        assert_eq!(
            D3d11Error::from(Fault::SyncCodeMismatch).to_string(),
            "parse failed: sync code mismatch"
        );
        assert_eq!(
            D3d11Error::from(Fault::NamesEmptySlot {
                reference: "ref_frame_idx",
            })
            .to_string(),
            "broken reference list: ref_frame_idx names empty slot"
        );
    }

    #[test]
    fn bad_marker_surfaces_tier_error() {
        let error = Ingest::new().ingest_packet(&[0x00]).unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::FrameMarkerMismatch)
        ));
        assert_eq!(error.to_string(), "parse failed: frame marker mismatch");
    }
}
