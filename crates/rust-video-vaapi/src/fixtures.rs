//! Synthetic codec payloads for unit tests.

use rust_video_harness::Bits;

/// Low-overhead OBU of `type_` with `payload`.
pub(crate) fn obu(type_: u8, payload: &[u8]) -> Vec<u8> {
    let mut obu = vec![type_ << 3 | 0x02];
    let mut size = Vec::new();
    let mut value = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        size.push(byte);
        if value == 0 {
            break;
        }
    }
    obu.extend(size);
    obu.extend_from_slice(payload);
    obu
}

/// Minimal 64x64 Profile 0 sequence header payload.
pub(crate) fn seq_payload() -> Vec<u8> {
    seq_payload_sized(64, 64)
}

/// Minimal Profile 0 sequence header payload at `width` by `height`.
pub(crate) fn seq_payload_sized(width: u32, height: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 5);
    writer.bits(0, 12);
    writer.bits(0, 5);
    let width_bits = (u32::BITS - (width - 1).leading_zeros()).max(1);
    let height_bits = (u32::BITS - (height - 1).leading_zeros()).max(1);
    writer.bits(width_bits - 1, 4);
    writer.bits(height_bits - 1, 4);
    writer.bits(width - 1, width_bits);
    writer.bits(height - 1, height_bits);
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
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 2);
    writer.bit(false);
    writer.bit(false);
    writer.finish_rbsp()
}

/// Minimal keyframe header payload: shown, single tile, lossless.
pub(crate) fn keyframe_payload() -> Vec<u8> {
    keyframe_bits(true, false)
}

/// Hidden keyframe header payload: showable, single tile, lossless.
pub(crate) fn keyframe_hidden_payload() -> Vec<u8> {
    keyframe_bits(false, false)
}

/// Keyframe header payload over 128x64: two tiles, lossless.
pub(crate) fn keyframe_wide_payload() -> Vec<u8> {
    keyframe_bits(true, true)
}

/// Keyframe header bits: `show` controls output, `wide` adds a tile column.
fn keyframe_bits(show: bool, wide: bool) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(false);
    writer.bits(0, 2);
    writer.bit(show);
    if !show {
        writer.bit(true);
        writer.bit(false);
    }
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    if !show {
        writer.bits(0xff, 8);
    }
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    if wide {
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 2);
    }
    writer.bits(0, 8);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.finish_rbsp()
}

/// Minimal inter-frame header payload: hidden, single tile, lossless.
pub(crate) fn pframe_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(false);
    writer.bits(1, 2);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(7, 3);
    writer.bits(1, 8);
    for _ in 0..7 {
        writer.bits(0, 3);
    }
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bits(0, 8);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    for _ in 0..7 {
        writer.bit(false);
    }
    writer.finish_rbsp()
}

/// Show-existing-frame header payload for reference slot `slot`.
pub(crate) fn show_existing_payload(slot: u8) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bits(u32::from(slot), 3);
    writer.finish_rbsp()
}

/// Tile group payload holding `tiles` bytes of tile data.
pub(crate) fn tile_group_payload(tiles: &[u8]) -> Vec<u8> {
    tiles.to_vec()
}

/// Two-tile group payload over 128x64: flag, first size, tile bytes.
pub(crate) fn tile_group_2_payload(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(false);
    let mut payload = writer.finish_raw();
    let size = u8::try_from(first.len().saturating_sub(1)).unwrap_or(u8::MAX);
    payload.push(size);
    payload.extend_from_slice(first);
    payload.extend_from_slice(second);
    payload
}

/// Baseline SPS RBSP with `poc_type`, 320x240.
pub(crate) fn sps_body(poc_type: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(66, 8);
    writer.bits(0, 8);
    writer.bits(30, 8);
    writer.ue(0);
    writer.ue(0);
    writer.ue(poc_type);
    if poc_type == 0 {
        writer.ue(0);
    }
    writer.ue(1);
    writer.bit(false);
    writer.ue(19);
    writer.ue(14);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.finish_rbsp()
}

/// CAVLC picture set RBSP over sequence set 0.
pub(crate) fn pps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bits(0, 2);
    writer.se(0);
    writer.se(0);
    writer.se(0);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.finish_rbsp()
}

/// Annex-B bytes with emulation prevention past `header`.
pub(crate) fn annexed(header: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, header];
    let mut zeros = 0;
    for byte in payload {
        if zeros == 2 && *byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(*byte);
        zeros = if *byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Sequence set for 1080p High with `poc_type`.
pub(crate) fn sps(poc_type: rust_video_h264::PocType) -> rust_video_h264::Sps {
    use rust_video_h264::{FidelityRange, ScalingSets};

    rust_video_h264::Sps {
        level_idc: 40,
        seq_parameter_set_id: 0,
        profile_idc: 100,
        constraints: 0,
        scaling: ScalingSets {
            four: [[16; 16]; 6],
            eight: [[16; 64]; 6],
        },
        scaling_map: None,
        fidelity: FidelityRange {
            chroma_format_idc: 1,
            separate_plane: false,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            transform_bypass: false,
        },
        log2_max_frame_num_minus4: 0,
        poc_type,
        log2_max_poc_lsb_minus4: 0,
        delta_zero: false,
        offset_non_ref: 0,
        offset_top_bottom: 0,
        cycle: 0,
        cycle_offsets: [0; 255],
        expected_delta: 0,
        max_num_ref_frames: 4,
        gaps_allowed: false,
        width_in_mbs_minus1: 119,
        height_in_mbs_minus1: 67,
        direct_8x8: true,
        crop_offsets: None,
        crop: None,
        vui: None,
    }
}

/// Picture set over sequence set zero.
pub(crate) fn pps() -> rust_video_h264::Pps {
    use rust_video_h264::ScalingSets;

    rust_video_h264::Pps {
        pic_parameter_set_id: 0,
        seq_parameter_set_id: 0,
        flags: 0,
        num_ref_l0: 0,
        num_ref_l1: 0,
        weighted_bipred_idc: 0,
        pic_init_qp: 0,
        pic_init_qs: 0,
        chroma_qp: 0,
        second_chroma_qp: 0,
        scaling: ScalingSets {
            four: [[16; 16]; 6],
            eight: [[16; 64]; 6],
        },
        scaling_map: None,
    }
}

/// P slice header, frame 0 over picture set 0.
pub(crate) fn slice_header() -> rust_video_h264::SliceHeader {
    use rust_video_h264::{Marking, SlicePoc, SliceType, Weights};

    rust_video_h264::SliceHeader {
        first_mb: 0,
        slice_type: SliceType::P,
        fixed: false,
        pps_id: 0,
        frame_num: 0,
        idr_pic_id: None,
        poc: SlicePoc::Lsb(0, 0),
        direct_spatial: false,
        ref_override: None,
        ref_mod_l0: Vec::new(),
        ref_mod_l1: Vec::new(),
        weights: Weights::default(),
        marking: Marking::Silent,
        redundant_pic_cnt: 0,
        cabac_init_idc: 0,
        qp_delta: 0,
        deblocking_idc: 0,
        deblocking_alpha: 0,
        deblocking_beta: 0,
        bit_offset: 0,
    }
}

/// Intra slice RBSP over picture set 0, frame 0.
pub(crate) fn idr_slice_body() -> Vec<u8> {
    slice_body(2, 0, 0, Some(0))
}

/// Slice RBSP of `type_` over picture set 0.
///
/// IDR slices carry `idr_pic_id`; inter slices carry silent marking.
pub(crate) fn slice_body(type_: u32, frame_num: u32, lsb: u32, idr_pic_id: Option<u32>) -> Vec<u8> {
    let mut slice = Bits::new();
    slice.ue(0);
    slice.ue(type_);
    slice.ue(0);
    slice.bits(frame_num, 4);
    if let Some(id) = idr_pic_id {
        slice.ue(id);
    }
    slice.bits(lsb, 4);
    slice.bit(false);
    if idr_pic_id.is_none() {
        // Default reference counts, unmodified lists.
        slice.bit(false);
        slice.bit(false);
    }
    // Long-term flag on IDR slices, silent marking otherwise.
    slice.bit(false);
    slice.se(0);
    slice.ue(0);
    slice.se(0);
    slice.se(0);
    slice.finish_rbsp()
}

/// P slice RBSP over picture set 0, frame 1.
pub(crate) fn p_slice_body() -> Vec<u8> {
    slice_body(0, 1, 2, None)
}

/// Annex B NAL of `type_` with `payload`, start code included.
pub(crate) fn nal(type_: u8, payload: &[u8]) -> Vec<u8> {
    let mut nal = vec![0, 0, 0, 1, type_ << 1, 1];
    nal.extend_from_slice(payload);
    nal
}

/// Minimal 64x64 Main sequence set payload.
pub(crate) fn sps_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.bits(0, 2);
    writer.bit(false);
    writer.bits(1, 5);
    for index in 0..32 {
        writer.bit(index == 1);
    }
    writer.bit(true);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bits(0, 31);
    writer.bits(0, 12);
    writer.bit(false);
    writer.bits(120, 8);
    writer.ue(0);
    writer.ue(1);
    writer.ue(64);
    writer.ue(64);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(4);
    writer.bit(true);
    writer.ue(1);
    writer.ue(1);
    writer.ue(0);
    writer.ue(0);
    writer.ue(3);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// Minimal picture set payload over sequence set 0.
pub(crate) fn pps_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.se(0);
    writer.se(0);
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
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// IDR slice payload.
pub(crate) fn idr_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bit(true);
    writer.ue(0);
    writer.ue(2);
    writer.se(0);
    writer.finish()
}

/// P slice payload with order lsb `lsb`.
///
/// The set holds one negative picture `delta_minus1` plus one below
/// the current order; the fixture sequence set carries no set to
/// predict from, so no prediction flag is emitted.
pub(crate) fn p_payload(lsb: u32, delta_minus1: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.ue(0);
    writer.ue(1);
    writer.bits(lsb, 8);
    writer.bit(false);
    writer.ue(1);
    writer.ue(0);
    writer.ue(delta_minus1);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.se(0);
    writer.finish()
}
