//! H.264 Annex B fixtures: sets and slices behind tier tests.

use rust_video_harness::Bits;

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
