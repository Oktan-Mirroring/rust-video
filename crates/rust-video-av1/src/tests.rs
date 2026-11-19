//! Grammar tests behind synthetic OBUs.

use crate::bits::BitReader;
use crate::error::Error;
use crate::frame::{
    inverse_recenter, reconstruct_motion_param, shear_valid, REF_ALTREF, REF_BWDREF,
};
use crate::syntax::{read_increment, read_leb128, read_ns, read_su, read_subexp, read_uvlc};
use crate::{parse_sequence_header, relative_dist, split_obus, tile_log2, Ingest, SequenceHeader};

use rust_video_harness::Bits;

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
    let mut writer = Bits::new();
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
    writer.finish_raw()
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
    let mut writer = Bits::new();
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
    writer.finish_raw()
}

/// Inter frame header body with `order_hint` over slot zero.
fn inter_body(order_hint: u32, short_signaling: bool) -> Vec<u8> {
    let mut writer = Bits::new();
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
    writer.finish_raw()
}

/// Show-existing frame header body for `slot`.
fn show_existing_body(slot: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bits(slot, 3);
    writer.finish_raw()
}

/// Frame OBU with header plus one four-byte tile.
fn frame_obu(header: &[u8]) -> Vec<u8> {
    let mut payload = header.to_vec();
    payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    obu(6, &payload)
}

/// Tile group OBU covering `start` through `end` with `tiles` payloads.
fn tile_group_obu(start: usize, end: usize, tiles: &[&[u8]]) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bits(u32::try_from(start).expect("tile index fits"), 1);
    writer.bits(u32::try_from(end).expect("tile index fits"), 1);
    let mut payload = writer.finish_raw();
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
    let mut writer = Bits::new();
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
    let data = obu(1, &writer.finish_raw());
    assert!(ingest.ingest_packet::<Error>(&data).is_err());

    let mut writer = Bits::new();
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
    let data = obu(1, &writer.finish_raw());
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
    assert_eq!(tile_log2(1, 1), 0);
    assert_eq!(tile_log2(1, 2), 1);
    assert_eq!(tile_log2(32, 32), 0);
    assert_eq!(tile_log2(32, 64), 1);
    assert_eq!(tile_log2(32, 65), 2);
    assert_eq!(relative_dist(6, 0, 6), 6);
    assert_eq!(relative_dist(0, 6, 6), -6);
    assert_eq!(relative_dist(1, 0, 2), 1);
    assert_eq!(relative_dist(0, 1, 2), -1);
    assert_eq!(relative_dist(7, 0, 2), -1);
    assert_eq!(relative_dist(0, 7, 2), 1);
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

#[test]
fn skip_mode_frames_pair_nearest_references() {
    use crate::{
        skip_mode_frames, FrameHeader, FrameType, RefInfo, FH_REFERENCE_SELECT, SEQ_ORDER_HINT,
    };

    let mut seq = seq();
    seq.flags |= SEQ_ORDER_HINT;
    seq.order_hint_bits_minus1 = 2;
    let mut header = FrameHeader {
        frame_type: FrameType::Inter,
        flags: FH_REFERENCE_SELECT,
        order_hint: 4,
        ref_frame_idx: [0, 1, 2, 3, 4, 5, 6],
        ..FrameHeader::default()
    };
    let mut refs: [RefInfo; 8] = core::array::from_fn(|_| RefInfo::default());
    refs[0].order_hint = 3;
    refs[1].order_hint = 5;
    assert_eq!(skip_mode_frames(&seq, &header, &refs), Some([1, 2]));
    header.frame_type = FrameType::Key;
    assert_eq!(skip_mode_frames(&seq, &header, &refs), None);
}

#[test]
fn skip_mode_frames_span_current_hint() {
    use crate::{skip_mode_frames, FrameHeader, FrameType, RefInfo};

    let mut seq = seq();
    seq.flags |= crate::SEQ_ORDER_HINT;
    seq.order_hint_bits_minus1 = 2;
    let header = FrameHeader {
        frame_type: FrameType::Inter,
        flags: crate::FH_REFERENCE_SELECT,
        order_hint: 4,
        ref_frame_idx: [0, 1, 2, 3, 4, 5, 6],
        ..FrameHeader::default()
    };
    let mut refs: [RefInfo; 8] = core::array::from_fn(|_| RefInfo::default());
    for (index, slot) in refs.iter_mut().enumerate() {
        slot.order_hint = u8::try_from(index).expect("slot");
    }
    assert_eq!(skip_mode_frames(&seq, &header, &refs), Some([4, 6]));
}

#[test]
fn split_tiles_strips_size_prefixes() {
    use crate::split_tiles;

    let single = split_tiles::<Error>(b"abcdef", 0, 0, 1).expect("single");
    assert_eq!(single, vec![(0, 6)]);
    let data = [0x02, 0x01, 0x02, 0x03, 0x04, 0x05];
    let pair = split_tiles::<Error>(&data, 0, 1, 1).expect("pair");
    assert_eq!(pair, vec![(1, 3), (4, 2)]);
    assert!(split_tiles::<Error>(&[0x09, 0x01], 0, 1, 1).is_err());
}

#[test]
fn tile_group_range_cuts_tile_span() {
    use crate::tile_group_range;

    let payload = [0xb0, 0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
    let head = tile_group_range::<Error>(&payload, 4, 1, 1).expect("range");
    assert_eq!((head.start, head.end), (1, 2));
    assert_eq!(&head.data[1..4], &[0xaa, 0xbb, 0xcc]);
}

#[test]
fn libaom_sequence_header_pins_geometry() {
    let payload = [
        0x00, 0x00, 0x00, 0x04, 0x3c, 0xff, 0xbc, 0xfa, 0xf9, 0x80, 0x40,
    ];
    let seq = parse_sequence_header::<Error>(&payload).expect("seq");
    assert_eq!(seq.profile, 0);
    assert_eq!(seq.operating.first().map_or(0, |point| point.level), 0);
    assert_eq!(
        (
            seq.max_frame_width_minus1 + 1,
            seq.max_frame_height_minus1 + 1
        ),
        (320, 240)
    );
    assert!(!seq.mono_chrome());
    assert!(seq.subsampling_x());
    assert!(seq.subsampling_y());
    assert_eq!(seq.bit_depth(), 8);
    assert!(seq.enable_order_hint());
    assert!(!seq.frame_id_numbers_present());
    assert!(!seq.use_128x128_superblock());
}
