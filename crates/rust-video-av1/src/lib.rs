//! AV1 grammar shared by every decode tier.
//!
//! One OBU, sequence, and frame parser behind the tier error types.

mod bits;
mod error;
mod frame;
mod obu;
mod sequence;
mod syntax;

pub use bits::BitReader;
pub use error::{BrokenReferenceReason, Error, ParseReason, UnsupportedReason};
pub use frame::{
    parse_frame_header, FilmGrain, FrameHeader, FrameType, GlobalMotion, RefInfo, FG_APPLY,
    FG_CHROMA_FROM_LUMA, FG_OVERLAP, FG_RESTRICTED_RANGE, FH_ALLOW_INTRABC, FH_ALLOW_WARPED_MOTION,
    FH_DELTA_LF_MULTI, FH_DELTA_LF_PRESENT, FH_DELTA_Q_PRESENT, FH_DIFF_UV_DELTA,
    FH_DISABLE_CDF_UPDATE, FH_DISABLE_FRAME_END_UPDATE_CDF, FH_ERROR_RESILIENT,
    FH_FORCE_INTEGER_MV, FH_HIGH_PRECISION_MV, FH_LOOP_DELTA_ENABLED, FH_LOOP_DELTA_UPDATE,
    FH_MOTION_MODE_SWITCHABLE, FH_REDUCED_TX_SET, FH_REFERENCE_SELECT, FH_RENDER_DIFFERENT,
    FH_SEG_ENABLED, FH_SEG_TEMPORAL_UPDATE, FH_SEG_UPDATE_DATA, FH_SEG_UPDATE_MAP,
    FH_SHORT_SIGNALING, FH_SHOWABLE_FRAME, FH_SHOW_EXISTING, FH_SHOW_FRAME, FH_SIZE_OVERRIDE,
    FH_SKIP_MODE_PRESENT, FH_USE_REF_MVS, FH_USING_QMATRIX, INTERP_SWITCHABLE, SUPERRES_DENOM_MIN,
};
pub use obu::{
    split_obus, split_tiles, tile_group_range, Frame, Ingest, Obu, TileData, TileGroupHead,
    OBU_FRAME, OBU_FRAME_HEADER, OBU_METADATA, OBU_PADDING, OBU_REDUNDANT_FRAME_HEADER,
    OBU_SEQUENCE_HEADER, OBU_TEMPORAL_DELIMITER, OBU_TILE_GROUP, OBU_TILE_LIST,
};
pub use sequence::{
    parse_sequence_header, DecoderModelPoint, OperatingPoint, SequenceHeader, SEQ_CDEF,
    SEQ_CHOOSE_INTEGER_MV, SEQ_CHOOSE_SCREEN_CONTENT, SEQ_COLOR_DESCRIPTION, SEQ_COLOR_RANGE,
    SEQ_DECODER_MODEL_PRESENT, SEQ_DUAL_FILTER, SEQ_EQUAL_INTERVAL, SEQ_FILM_GRAIN,
    SEQ_FILTER_INTRA, SEQ_FRAME_ID_NUMBERS, SEQ_HIGH_BITDEPTH, SEQ_INITIAL_DISPLAY_PRESENT,
    SEQ_INTERINTRA, SEQ_INTRA_EDGE, SEQ_JNT_COMP, SEQ_MASKED_COMPOUND, SEQ_MONO_CHROME,
    SEQ_ORDER_HINT, SEQ_REDUCED_HEADER, SEQ_REF_MVS, SEQ_RESTORATION, SEQ_SEPARATE_UV_DELTA_Q,
    SEQ_STILL_PICTURE, SEQ_SUBSAMPLING_X, SEQ_SUBSAMPLING_Y, SEQ_SUPERBLOCK_128, SEQ_SUPERRES,
    SEQ_TIMING_PRESENT, SEQ_TWELVE_BIT, SEQ_WARPED_MOTION,
};
pub use syntax::{relative_dist, tile_log2};

/// Operating point count.
pub(crate) const MAX_OPERATING_POINTS: usize = 32;

/// Reference frame count.
pub const NUM_REF_FRAMES: usize = 8;

/// References per frame.
pub const REFS_PER_FRAME: usize = 7;

/// Segments per frame.
pub const MAX_SEGMENTS: usize = 8;

/// Segment levels per segment.
pub const SEG_LEVELS: usize = 8;

/// Reference deltas per frame.
pub const TOTAL_REFS: usize = 8;

/// Superres numerator.
pub const SUPERRES_NUM: u8 = 8;

/// Screen content tools selected per frame.
pub(crate) const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;

/// Integer motion vectors selected per frame.
pub(crate) const SELECT_INTEGER_MV: u8 = 2;

/// AV1 codec namespace.
pub struct Av1;

#[cfg(test)]
mod tests {
    use crate::bits::BitReader;
    use crate::error::Error;
    use crate::frame::{
        inverse_recenter, reconstruct_motion_param, shear_valid, REF_ALTREF, REF_BWDREF,
    };
    use crate::syntax::{read_increment, read_leb128, read_ns, read_su, read_subexp, read_uvlc};
    use crate::{
        parse_sequence_header, relative_dist, split_obus, tile_log2, Ingest, SequenceHeader,
    };

    /// Test bit writer.
    struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        fn bit(&mut self, set: bool) {
            if set {
                self.current |= 1 << (7 - self.filled);
            }
            self.filled += 1;
            if self.filled == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.filled = 0;
            }
        }

        /// Low `count` bits of `value`, most significant first.
        fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Byte alignment with zero bits.
        fn finish(mut self) -> Vec<u8> {
            while self.filled != 0 {
                self.bit(false);
            }
            self.bytes
        }
    }

    /// OBU with type, size field, and payload.
    fn obu(unit_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut obu = vec![unit_type << 3 | 1 << 1];
        let mut size = payload.len();
        loop {
            let mut byte = u8::try_from(size & 0x7F).expect("seven bits fit");
            size >>= 7;
            if size != 0 {
                byte |= 0x80;
            }
            obu.push(byte);
            if size == 0 {
                break;
            }
        }
        obu.extend_from_slice(payload);
        obu
    }

    /// 320x240 sequence header payload.
    fn seq_body() -> Vec<u8> {
        seq_body_opts(false)
    }

    /// 320x240 sequence header payload with superres selection.
    fn seq_body_opts(superres: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 5);
        writer.bits(0, 12);
        writer.bits(0, 5);
        writer.bits(8, 4);
        writer.bits(7, 4);
        writer.bits(319, 9);
        writer.bits(239, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bits(0, 3);
        writer.bit(superres);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 2);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// Sequence header used across tests.
    fn seq() -> SequenceHeader {
        parse_sequence_header::<Error>(&seq_body()).expect("valid seq")
    }

    /// Key frame header body over the test sequence.
    fn key_body() -> Vec<u8> {
        key_body_opts(false, false, false)
    }

    /// Key frame header body with tile, render, and superres selection.
    fn key_body_opts(two_col: bool, render_diff: bool, use_superres: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(false);
        writer.bits(0, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        if use_superres {
            writer.bit(true);
            writer.bits(0, 3);
        }
        if render_diff {
            writer.bit(true);
            writer.bits(159, 16);
            writer.bits(119, 16);
        } else {
            writer.bit(false);
        }
        writer.bit(false);
        writer.bit(true);
        if two_col {
            writer.bit(true);
            writer.bit(false);
            writer.bit(false);
            writer.bits(0, 1);
            writer.bits(0, 2);
        } else {
            writer.bit(false);
            writer.bit(false);
        }
        writer.bits(100, 8);
        writer.bit(true);
        writer.bits(0x7D, 7);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(6, 6);
        writer.bits(6, 6);
        writer.bits(11, 6);
        writer.bits(17, 6);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.finish()
    }

    /// Inter frame header body with `order_hint` over slot zero.
    fn inter_body(order_hint: u32, short_signaling: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(false);
        writer.bits(1, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(order_hint, 1);
        writer.bits(7, 3);
        writer.bits(0x02, 8);
        if short_signaling {
            writer.bit(true);
            writer.bits(0, 3);
            writer.bits(0, 3);
        } else {
            writer.bit(false);
            for _ in 0..7 {
                writer.bits(0, 3);
            }
        }
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bits(100, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(6, 6);
        writer.bits(6, 6);
        writer.bits(11, 6);
        writer.bits(17, 6);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        for _ in 0..7 {
            writer.bit(false);
        }
        writer.finish()
    }

    /// Show-existing frame header body for `slot`.
    fn show_existing_body(slot: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bits(slot, 3);
        writer.finish()
    }

    /// Frame OBU with header plus one four-byte tile.
    fn frame_obu(header: &[u8]) -> Vec<u8> {
        let mut payload = header.to_vec();
        payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        obu(6, &payload)
    }

    /// Tile group OBU covering `start` through `end` with `tiles` payloads.
    fn tile_group_obu(start: usize, end: usize, tiles: &[&[u8]]) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bits(u32::try_from(start).expect("tile index fits"), 1);
        writer.bits(u32::try_from(end).expect("tile index fits"), 1);
        let mut payload = writer.finish();
        for (index, tile) in tiles.iter().enumerate() {
            if start + index != end {
                payload.push(u8::try_from(tile.len() - 1).expect("tile size fits"));
            }
            payload.extend_from_slice(tile);
        }
        obu(4, &payload)
    }

    #[test]
    fn seq_reports_tools_and_max() {
        let seq = seq();
        assert_eq!(seq.profile, 0);
        assert_eq!(seq.operating.len(), 1);
        assert_eq!((seq.max_frame_width(), seq.max_frame_height()), (320, 240));
        assert_eq!(seq.bit_depth(), 8);
        assert_eq!(seq.num_planes(), 3);
        assert!(seq.enable_order_hint());
        assert_eq!(seq.order_hint_bits(), 1);
        assert!(seq.subsampling_x());
        assert!(seq.subsampling_y());
        assert!(!seq.enable_cdef());
        assert!(!seq.film_grain_params_present());
    }

    #[test]
    fn seq_refuses_depth_and_chroma() {
        let mut writer = BitWriter::new();
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 5);
        writer.bits(0, 12);
        writer.bits(0, 5);
        writer.bits(8, 4);
        writer.bits(7, 4);
        writer.bits(319, 9);
        writer.bits(239, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        let mut ingest = Ingest::new();
        let data = obu(1, &writer.finish());
        assert!(ingest.ingest_packet::<Error>(&data).is_err());

        let mut writer = BitWriter::new();
        writer.bits(1, 3);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 5);
        writer.bits(0, 12);
        writer.bits(0, 5);
        writer.bits(8, 4);
        writer.bits(7, 4);
        writer.bits(319, 9);
        writer.bits(239, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        let mut ingest = Ingest::new();
        let data = obu(1, &writer.finish());
        assert!(ingest.ingest_packet::<Error>(&data).is_err());
    }

    #[test]
    fn key_frame_parses() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        let frames = ingest.ingest_packet::<Error>(&data).expect("key packet");
        assert_eq!(frames.len(), 1);
        let header = &frames[0].header;
        assert_eq!(header.frame_type, crate::FrameType::Key);
        assert!(header.show_frame());
        assert!(!header.showable_frame());
        assert_eq!(header.order_hint, 0);
        assert_eq!(header.refresh_frame_flags, 0xFF);
        assert_eq!((header.frame_width, header.frame_height), (320, 240));
        assert_eq!((header.tile_cols, header.tile_rows), (1, 1));
        assert_eq!(header.base_q_idx, 100);
        assert_eq!(header.delta_q_y_dc, -3);
        assert_eq!(header.loop_filter_level, [6, 6, 11, 17]);
        assert_eq!(header.tx_mode, 2);
        assert!(!header.segmentation_enabled());
        assert!(!header.film_grain.apply_grain());
        assert!(header.global_motion.iter().all(|motion| motion.invalid));
        assert_eq!(frames[0].tiles.len(), 1);
        assert_eq!(frames[0].tiles[0].bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn inter_frame_parses_with_refs() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        data.extend(frame_obu(&inter_body(1, false)));
        let frames = ingest.ingest_packet::<Error>(&data).expect("inter packet");
        assert_eq!(frames.len(), 2);
        let header = &frames[1].header;
        assert_eq!(header.frame_type, crate::FrameType::Inter);
        assert!(header.show_frame());
        assert_eq!(header.order_hint, 1);
        assert_eq!(header.refresh_frame_flags, 0x02);
        assert_eq!(header.ref_frame_idx, [0; 7]);
        assert!(header.allow_high_precision_mv());
        assert!(header.is_motion_mode_switchable());
        assert_eq!(header.interp_filter, 0);
        assert!(header.global_motion.iter().all(|motion| !motion.invalid));
        assert_eq!(
            header.global_motion[0].params,
            [0, 0, 1 << 16, 0, 0, 1 << 16]
        );
    }

    #[test]
    fn short_signaling_derives_refs() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        data.extend(frame_obu(&inter_body(1, true)));
        let frames = ingest.ingest_packet::<Error>(&data).expect("short packet");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].header.ref_frame_idx, [0, 7, 6, 0, 5, 4, 3]);
    }

    #[test]
    fn tile_groups_assemble_two_columns() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(obu(3, &key_body_opts(true, false, false)));
        data.extend(tile_group_obu(0, 0, &[&[0xAA; 8]]));
        data.extend(tile_group_obu(1, 1, &[&[0xBB; 6]]));
        let frames = ingest.ingest_packet::<Error>(&data).expect("tiled packet");
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!((frame.header.tile_cols, frame.header.tile_rows), (2, 1));
        assert_eq!(frame.header.tile_size_bytes, 1);
        assert_eq!(frame.tiles.len(), 2);
        assert_eq!((frame.tiles[0].row, frame.tiles[0].column), (0, 0));
        assert_eq!(frame.tiles[0].bytes, vec![0xAA; 8]);
        assert_eq!((frame.tiles[1].row, frame.tiles[1].column), (0, 1));
        assert_eq!(frame.tiles[1].bytes, vec![0xBB; 6]);
    }

    #[test]
    fn superres_and_render_decline() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body_opts(true));
        data.extend(frame_obu(&key_body_opts(false, false, true)));
        let error = ingest.ingest_packet::<Error>(&data).unwrap_err();
        assert!(matches!(error, crate::Error::Unsupported(_)));

        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body_opts(false, true, false)));
        let error = ingest.ingest_packet::<Error>(&data).unwrap_err();
        assert!(matches!(error, crate::Error::Unsupported(_)));
    }

    #[test]
    fn spatial_layers_and_tile_lists_decline() {
        let mut ingest = Ingest::new();
        let error = ingest
            .ingest_packet::<Error>(&[0x36, 0x08, 0x00])
            .unwrap_err();
        assert!(matches!(error, crate::Error::Unsupported(_)));

        let mut ingest = Ingest::new();
        let error = ingest.ingest_packet::<Error>(&obu(8, &[])).unwrap_err();
        assert!(matches!(error, crate::Error::Unsupported(_)));
    }

    #[test]
    fn show_existing_frame_parses() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        data.extend(obu(3, &show_existing_body(0)));
        let frames = ingest.ingest_packet::<Error>(&data).expect("show packet");
        assert_eq!(frames.len(), 2);
        let header = &frames[1].header;
        assert!(header.show_existing_frame());
        assert_eq!(header.frame_to_show_map_idx, 0);
        assert_eq!(header.frame_type, crate::FrameType::Key);
        assert!(frames[1].tiles.is_empty(), "show existing carries no tiles");

        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(obu(3, &show_existing_body(2)));
        let error = ingest.ingest_packet::<Error>(&data).unwrap_err();
        assert!(matches!(error, crate::Error::BrokenReference(_)));
    }

    #[test]
    fn split_obus_handles_extension_and_size() {
        let mut data = obu(1, &seq_body());
        data.extend([0x36, 0x40, 0x03, 0xAA, 0xBB, 0xCC]);
        data.extend([0x10]);
        let obus = split_obus::<Error>(&data).expect("obus split");
        assert_eq!(obus.len(), 3);
        assert_eq!(obus[0].unit_type, 1);
        assert_eq!(obus[1].unit_type, 6);
        assert_eq!((obus[1].temporal_id, obus[1].spatial_id), (2, 0));
        assert_eq!(obus[1].payload, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(obus[2].unit_type, 2);
        assert!(obus[2].payload.is_empty(), "delimiter carries no payload");
    }

    #[test]
    fn readers_match_spec_vectors() {
        let mut reader = BitReader::new(&[0x20]);
        assert_eq!(read_uvlc::<Error>(&mut reader, "uvlc").expect("uvlc"), 3);
        let mut reader = BitReader::new(&[0xAC, 0x02]);
        assert_eq!(
            read_leb128::<Error>(&mut reader, "leb128").expect("leb128"),
            300
        );
        let mut reader = BitReader::new(&[0x00]);
        assert_eq!(read_ns::<Error>(&mut reader, 5, "ns").expect("ns"), 0);
        let mut reader = BitReader::new(&[0xE0]);
        assert_eq!(read_ns::<Error>(&mut reader, 5, "ns").expect("ns"), 4);
        let mut reader = BitReader::new(&[0x80]);
        assert_eq!(
            read_increment::<Error>(&mut reader, 1, 3, "inc").expect("inc"),
            2
        );
        let mut reader = BitReader::new(&[0x00]);
        assert_eq!(
            read_subexp::<Error>(&mut reader, 1025, "subexp").expect("subexp"),
            0
        );
        let mut reader = BitReader::new(&[0x06, 0xFA]);
        assert_eq!(read_su::<Error>(&mut reader, 7, "su").expect("su"), 3);
        let mut reader = BitReader::new(&[0xFA, 0x00]);
        assert_eq!(read_su::<Error>(&mut reader, 7, "su").expect("su"), -3);
        assert_eq!(tile_log2(64, 5), 0);
        assert_eq!(tile_log2(1, 5), 3);
        assert_eq!(tile_log2(32, 33), 1);
        assert_eq!(relative_dist(6, 0, 6), 6);
        assert_eq!(relative_dist(0, 6, 6), -6);
    }

    #[test]
    fn motion_reconstruction_roundtrips() {
        assert_eq!(reconstruct_motion_param(0, 0, 0, 9, 3), 0);
        assert_eq!(inverse_recenter(512, 0), 512);
        assert_eq!(inverse_recenter(512, 1), 511);
        assert!(shear_valid(&[0, 0, 1 << 16, 0, 0, 1 << 16]));
        assert!(!shear_valid(&[0, 0, 0, 0, 0, 0]));
        assert_eq!((REF_BWDREF, REF_ALTREF), (5, 7));
    }
}
