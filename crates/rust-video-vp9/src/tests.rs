//! Frame ingest tests behind synthetic headers and the committed vector.

use crate::error::Fault;
use crate::frame::FH_SEG_UPDATE_DATA;
use crate::picture::Ingest;

use rust_video_harness::{decode_dir, Bits};

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

/// Synthetic profile 0 keyframe, 16x16 with segmentation and one tile.
fn key_16x16() -> Vec<u8> {
    let mut writer = Bits::new();
    key_head(&mut writer, 16, 16);
    writer.bit(false);
    writer.bits(4, 16);
    let mut bytes = writer.finish_raw();
    bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    bytes.extend_from_slice(&[0x11, 0x22, 0x33]);
    bytes
}

/// Synthetic inter frame inheriting the key size.
fn inter_16x16() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(2, 2);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bits(0, 2);
    writer.bits(0x02, 8);
    for _ in 0..3 {
        writer.bits(0, 3);
        writer.bit(false);
    }
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(1, 2);
    writer.bit(true);
    writer.bit(false);
    writer.bits(1, 2);
    writer.bits(5, 6);
    writer.bits(0, 3);
    writer.bit(false);
    writer.bits(90, 8);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(2, 16);
    let mut bytes = writer.finish_raw();
    bytes.extend_from_slice(&[0xAA, 0xBB]);
    bytes.extend_from_slice(&[0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
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
fn keyframe_header_parses_every_field() {
    let bytes = key_16x16();
    let frames = Ingest::<Fault>::new()
        .ingest_packet(&bytes)
        .expect("keyframe");
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    let header = &frame.header;
    assert!(header.frame_type.is_intra());
    assert_eq!(header.profile, 0);
    assert_eq!(header.bit_depth, 8);
    assert_eq!(header.color_space, 0);
    assert_eq!(header.color_range, 0);
    assert!(!header.show_existing_frame());
    assert!(header.show_frame());
    assert!(!header.error_resilient_mode());
    assert!(!header.intra_only());
    assert_eq!((header.frame_width, header.frame_height), (16, 16));
    assert_eq!((header.render_width, header.render_height), (16, 16));
    assert!(!header.render_differs());
    assert_eq!(header.refresh_frame_flags, 0xFF);
    assert_eq!(header.ref_frame_idx, [0; 3]);
    assert_eq!(header.ref_frame_sign_bias, [0; 4]);
    assert_eq!(header.reset_frame_context, 0);
    assert!(header.refresh_frame_context());
    assert!(header.frame_parallel_decoding_mode());
    assert_eq!(header.frame_context_idx, 0);
    assert_eq!(header.filter_level, 10);
    assert_eq!(header.sharpness, 2);
    assert!(header.loop_filter_delta_enabled());
    assert!(header.loop_filter_delta_update());
    assert_eq!(header.ref_deltas, [5, 0, -1, -1]);
    assert_eq!(header.mode_deltas, [0; 2]);
    assert_eq!(header.base_q_idx, 100);
    assert_eq!(header.delta_q_y_dc, 3);
    assert_eq!(header.delta_q_uv_dc, 0);
    assert_eq!(header.delta_q_uv_ac, 0);
    assert!(header.segmentation_enabled());
    assert!(header.segmentation_update_map());
    assert!(header.segmentation_temporal_update());
    assert_ne!(header.flags & FH_SEG_UPDATE_DATA, 0);
    assert!(header.segmentation_abs_delta());
    assert_eq!(header.tree_probs, [128; 7]);
    assert_eq!(header.pred_probs, [200; 3]);
    assert_eq!(
        header.seg_enabled[0],
        [true, false, true, true],
        "segment 0 features"
    );
    assert_eq!(header.seg_value[0], [5, 0, 2, 0], "segment 0 values");
    for segment in 1..8 {
        assert_eq!(header.seg_enabled[segment], [false; 4]);
        assert_eq!(header.seg_value[segment], [0; 4]);
    }
    assert_eq!((header.tile_cols_log2, header.tile_rows_log2), (0, 0));
    assert_eq!(header.header_size_in_bytes, 4);
    assert_eq!(
        usize::from(header.uncompressed_size) + 4 + 3,
        bytes.len(),
        "header plus compressed plus tile fills the frame"
    );
    assert_eq!(frame.bytes, bytes);
    assert_eq!(frame.tiles.len(), 1);
    assert_eq!((frame.tiles[0].row, frame.tiles[0].column), (0, 0));
    assert_eq!(frame.tiles[0].bytes, vec![0x11, 0x22, 0x33]);
    assert!(header.subsampling_x() && header.subsampling_y());
}

#[test]
fn inter_frame_inherits_size_and_state() {
    let mut ingest = Ingest::<Fault>::new();
    ingest.ingest_packet(&key_16x16()).expect("keyframe");
    let frames = ingest.ingest_packet(&inter_16x16()).expect("inter");
    assert_eq!(frames.len(), 1);
    let header = &frames[0].header;
    assert!(!header.frame_type.is_intra());
    assert_eq!((header.frame_width, header.frame_height), (16, 16));
    assert!(header.show_frame());
    assert!(!header.intra_only());
    assert_eq!(header.reset_frame_context, 0);
    assert_eq!(header.refresh_frame_flags, 0x02);
    assert_eq!(header.ref_frame_idx, [0; 3]);
    assert_eq!(header.ref_frame_sign_bias, [0; 4]);
    assert_eq!(header.bit_depth, 8);
    assert_eq!(header.color_space, 0);
    assert_eq!(header.color_range, 0);
    assert!(header.subsampling_x() && header.subsampling_y());
    assert!(!header.allow_high_precision_mv());
    assert_eq!(header.interp_filter, 0);
    assert!(header.refresh_frame_context());
    assert!(!header.frame_parallel_decoding_mode());
    assert_eq!(header.frame_context_idx, 1);
    assert_eq!(header.filter_level, 5);
    assert!(!header.loop_filter_delta_enabled());
    assert!(!header.loop_filter_delta_update());
    assert_eq!(header.ref_deltas, [5, 0, -1, -1], "deltas persist");
    assert_eq!(header.base_q_idx, 90);
    assert!(!header.segmentation_enabled());
    assert_eq!(header.tree_probs, [128; 7], "probabilities persist");
    assert_eq!(header.seg_value[0], [5, 0, 2, 0], "features persist");
    assert_eq!(frames[0].tiles[0].bytes, vec![0x44, 0x55, 0x66, 0x77, 0x88]);
}

#[test]
fn inter_before_key_breaks_reference() {
    let error = Ingest::<Fault>::new()
        .ingest_packet(&inter_16x16())
        .unwrap_err();
    assert!(matches!(error, Fault::NamesEmptySlot { .. }));
    assert_eq!(error.to_string(), "ref_frame_idx names empty slot");
}

#[test]
fn reset_refs_recovers_after_failure() {
    let mut ingest = Ingest::<Fault>::new();
    ingest
        .ingest_packet(&inter_16x16())
        .expect_err("inter before key");
    ingest.reset_refs();
    let frames = ingest.ingest_packet(&key_16x16()).expect("keyframe");
    assert_eq!(frames.len(), 1);
}

#[test]
fn bad_marker_rejects() {
    let error = Ingest::<Fault>::new().ingest_packet(&[0x00]).unwrap_err();
    assert!(matches!(error, Fault::FrameMarkerMismatch));
    assert_eq!(error.to_string(), "frame marker mismatch");
}

#[test]
fn bad_sync_code_rejects() {
    let error = Ingest::<Fault>::new()
        .ingest_packet(&[0x82, 0x00, 0x00, 0x00])
        .unwrap_err();
    assert!(matches!(error, Fault::SyncCodeMismatch));
    assert_eq!(error.to_string(), "sync code mismatch");
}

#[test]
fn reserved_profile_rejects() {
    let error = Ingest::<Fault>::new().ingest_packet(&[0xBC]).unwrap_err();
    assert!(matches!(error, Fault::ReservedProfile));
    assert_eq!(error.to_string(), "reserved profile");
}

#[test]
fn truncated_header_rejects() {
    let error = Ingest::<Fault>::new().ingest_packet(&[0x82]).unwrap_err();
    assert!(matches!(error, Fault::Truncated { .. }));
}

#[test]
fn empty_packet_yields_no_frames() {
    let frames = Ingest::<Fault>::new()
        .ingest_packet(&[])
        .expect("empty packet");
    assert!(frames.is_empty(), "empty packet yields no frames");
    assert!(Ingest::<Fault>::new().finish().expect("finish").is_none());
}

#[test]
fn superframe_splits_members() {
    let key = key_16x16();
    let mut chunk = vec![0x88];
    chunk.extend_from_slice(&key);
    chunk.push(0xC1);
    chunk.push(1);
    chunk.push(u8::try_from(key.len()).expect("key fits"));
    chunk.push(0xC1);
    let frames = Ingest::<Fault>::new()
        .ingest_packet(&chunk)
        .expect("superframe");
    assert_eq!(frames.len(), 2);
    assert!(frames[0].header.show_existing_frame());
    assert_eq!(frames[0].header.frame_to_show_map_idx, 0);
    assert!(frames[0].tiles.is_empty(), "display carries no tiles");
    assert_eq!(frames[0].bytes, vec![0x88]);
    assert!(frames[1].header.frame_type.is_intra());
    assert_eq!(frames[1].bytes, key);
}

#[test]
fn corrupt_superframe_index_rejects() {
    let error = Ingest::<Fault>::new()
        .ingest_packet(&[0x88, 0xC1, 0x05, 0x00, 0xC1])
        .unwrap_err();
    assert!(matches!(error, Fault::Range { .. }));
}

#[test]
fn mismatched_superframe_bookends_parse_single() {
    let frames = Ingest::<Fault>::new()
        .ingest_packet(&[0x88, 0xAA, 0xBB, 0x01, 0xC1])
        .expect("plain frame");
    assert_eq!(frames.len(), 1);
    assert!(frames[0].header.show_existing_frame());
}

#[test]
fn tiles_split_big_endian_sizes() {
    let bytes = tiled_4096x16();
    let frames = Ingest::<Fault>::new().ingest_packet(&bytes).expect("tiled");
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(
        (frame.header.tile_cols_log2, frame.header.tile_rows_log2),
        (2, 1)
    );
    assert_eq!(frame.tiles.len(), 8);
    for (index, tile) in frame.tiles.iter().enumerate() {
        let position = u16::try_from(index).expect("tile fits");
        assert_eq!(tile.row, position / 4, "tile {index} row");
        assert_eq!(tile.column, position % 4, "tile {index} column");
        let fill = 0xA0 + u8::try_from(index).expect("tile fits");
        let len = if index < 7 { index + 1 } else { 8 };
        assert_eq!(tile.bytes, vec![fill; len], "tile {index} bytes");
    }
}

#[test]
fn truncated_tile_size_rejects() {
    let bytes = tiled_4096x16();
    let tile_section: usize = (1..=7).sum::<usize>() + 7 * 4 + 8;
    let cut = bytes.len() - tile_section + 2;
    let error = Ingest::<Fault>::new()
        .ingest_packet(&bytes[..cut])
        .unwrap_err();
    assert!(matches!(error, Fault::Truncated { .. }));
}

#[test]
fn oversized_tile_rejects() {
    let mut bytes = tiled_4096x16();
    let tile_section: usize = (1..=7).sum::<usize>() + 7 * 4 + 8;
    let start = bytes.len() - tile_section;
    bytes[start..start + 4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    let error = Ingest::<Fault>::new().ingest_packet(&bytes).unwrap_err();
    assert!(matches!(error, Fault::Truncated { .. }));
}

/// IVF payloads of the committed vector in file order.
fn vector_payloads() -> Vec<Vec<u8>> {
    let path = decode_dir().join("valid_320x240.ivf");
    let data = std::fs::read(&path).expect("vector readable");
    assert!(data.len() >= 32 && &data[..4] == b"DKIF", "vector is IVF");
    let head = usize::from(u16::from_le_bytes([data[6], data[7]]).max(32));
    let mut payloads = Vec::new();
    let mut offset = head;
    while let Some(entry) = data.get(offset..offset + 12) {
        let size = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let size = usize::try_from(size).expect("payload fits");
        let Some(payload) = data.get(offset + 12..offset + 12 + size) else {
            break;
        };
        payloads.push(payload.to_vec());
        offset += 12 + size;
    }
    payloads
}

#[test]
fn committed_vector_parses_fifteen_frames() {
    let payloads = vector_payloads();
    assert_eq!(payloads.len(), 15, "vector holds fifteen packets");
    let mut ingest = Ingest::<Fault>::new();
    let mut frames = Vec::new();
    for payload in &payloads {
        let parsed = ingest.ingest_packet(payload).expect("packet parses");
        assert_eq!(parsed.len(), 1, "packet holds one frame");
        frames.extend(parsed);
    }
    assert!(frames[0].header.frame_type.is_intra(), "first frame keys");
    for (index, frame) in frames.iter().enumerate() {
        let header = &frame.header;
        assert_eq!(
            (header.frame_width, header.frame_height),
            (320, 240),
            "frame {index} size"
        );
        assert!(header.show_frame(), "frame {index} shows");
        assert_eq!(header.profile, 0, "frame {index} profile");
        assert_eq!(header.bit_depth, 8, "frame {index} depth");
        assert!(
            header.subsampling_x() && header.subsampling_y(),
            "frame {index} 4:2:0"
        );
        assert_eq!(
            (header.tile_cols_log2, header.tile_rows_log2),
            (0, 0),
            "frame {index} single tile"
        );
        assert_eq!(frame.tiles.len(), 1, "frame {index} tile split");
        assert!(!frame.tiles[0].bytes.is_empty(), "frame {index} tile bytes");
        if index == 0 {
            assert_eq!(header.refresh_frame_flags, 0xFF, "key refreshes all");
        } else {
            assert!(!header.frame_type.is_intra(), "frame {index} inters");
        }
    }
}
