//! H.264 standard structures from parsed parameter sets.
//!
//! Pure host-side fills: parsed sets in, `StdVideo` structures out.
//! Pointer fields borrow box-stable backing owned by [`H264Tables`],
//! so a filled structure stays valid while its table entry lives.

use std::collections::HashMap;

use ash::vk::native::{
    __BindgenBitfieldUnit, StdVideoDecodeH264PictureInfo, StdVideoDecodeH264PictureInfoFlags,
    StdVideoDecodeH264ReferenceInfo, StdVideoDecodeH264ReferenceInfoFlags,
    StdVideoH264AspectRatioIdc_STD_VIDEO_H264_ASPECT_RATIO_IDC_INVALID,
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_422,
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_444,
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME,
    StdVideoH264HrdParameters, StdVideoH264PictureParameterSet, StdVideoH264PocType,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0, StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_1,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2, StdVideoH264PpsFlags, StdVideoH264ScalingLists,
    StdVideoH264SequenceParameterSet, StdVideoH264SequenceParameterSetVui, StdVideoH264SpsFlags,
    StdVideoH264SpsVuiFlags,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_EXPLICIT,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_IMPLICIT,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_INVALID,
};

use crate::error::ParseError;
use crate::parse::h264::{HrdParams, Pps, Sps, VuiParams};
use crate::parse::refs::Picture;
use crate::session::geometry::{h264_level_to_std, h264_profile_to_std};

/// H.264 parameter tables with box-stable standard-structure backing.
pub(crate) struct H264Tables {
    /// Sequence entries by set id.
    sps: HashMap<u8, SpsEntry>,
    /// Picture entries by set id.
    pps: HashMap<u8, PpsEntry>,
}

/// Box-stable backing of one sequence set.
#[derive(Debug)]
struct SpsEntry {
    /// Scaling lists, when the set carries them.
    scaling: Option<Box<StdVideoH264ScalingLists>>,
    /// Type-1 cycle offsets, when the cycle is non-empty.
    offsets: Option<Box<[i32]>>,
    /// VUI parameters, when present.
    vui: Option<Box<StdVideoH264SequenceParameterSetVui>>,
    /// NAL HRD parameters, when present.
    nal_hrd: Option<Box<StdVideoH264HrdParameters>>,
    /// VCL HRD parameters, when present.
    vcl_hrd: Option<Box<StdVideoH264HrdParameters>>,
    /// Standard level idc.
    level: u32,
    /// Standard profile idc.
    profile: u32,
}

/// Box-stable backing of one picture set.
struct PpsEntry {
    /// Scaling lists, when the set carries them.
    scaling: Option<Box<StdVideoH264ScalingLists>>,
    /// Default active L0 count minus 1.
    l0_default: u8,
    /// Default active L1 count minus 1.
    l1_default: u8,
    /// Standard weighted-bipred idc.
    bipred: u32,
    /// Picture QP minus 26.
    qp: i8,
    /// Picture QS minus 26.
    qs: i8,
    /// Chroma QP offset.
    chroma: i8,
    /// Second chroma QP offset.
    chroma2: i8,
}

impl H264Tables {
    /// Empty tables.
    pub(crate) fn new() -> Self {
        Self {
            sps: HashMap::new(),
            pps: HashMap::new(),
        }
    }

    /// Standard sequence set for `id`, if stored.
    ///
    /// Links the stored VUI at its HRD backing on every read, so the
    /// insert order of the two boxes never matters.
    pub(crate) fn sps(&mut self, id: u8, parsed: &Sps) -> Option<StdVideoH264SequenceParameterSet> {
        let entry = self.sps.get_mut(&id)?;
        if let Some(vui) = entry.vui.as_mut() {
            vui.pHrdParameters = entry
                .nal_hrd
                .as_deref()
                .or(entry.vcl_hrd.as_deref())
                .map_or(std::ptr::null(), std::ptr::from_ref);
        }
        Some(fill_sps(parsed, entry))
    }

    /// Standard picture set for `id`, if stored.
    pub(crate) fn pps(&self, id: u8, parsed: &Pps) -> Option<StdVideoH264PictureParameterSet> {
        self.pps.get(&id).map(|entry| fill_pps(parsed, entry))
    }

    /// Standard structures for `sps`, replacing any entry under its id.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn insert_sps(&mut self, sps: &Sps) -> Result<(), ParseError> {
        let level = h264_level_to_std(sps.level_idc(), sps.constraint_set3()).ok_or(
            ParseError::OutOfRange {
                syntax: "sps level",
            },
        )?;
        let profile = h264_profile_to_std(sps.profile_idc()).ok_or(ParseError::OutOfRange {
            syntax: "sps profile",
        })?;
        let scaling = sps.scaling().map(|lists| Box::new(fill_scaling(lists)));
        let offsets = (sps.num_ref_frames_in_poc_cycle() > 0).then(|| {
            sps.offset_for_ref_frame()[..usize::from(sps.num_ref_frames_in_poc_cycle())]
                .to_vec()
                .into_boxed_slice()
        });
        let vui_parsed = sps.vui();
        let mut nal_hrd = None;
        let mut vcl_hrd = None;
        if let Some(vui) = vui_parsed {
            if let Some(hrd) = &vui.nal_hrd {
                nal_hrd = Some(Box::new(fill_hrd(hrd)));
            }
            if let Some(hrd) = &vui.vcl_hrd {
                vcl_hrd = Some(Box::new(fill_hrd(hrd)));
            }
        }
        let vui = vui_parsed.map(fill_vui).transpose()?.map(Box::new);
        self.sps.insert(
            sps.id(),
            SpsEntry {
                scaling,
                offsets,
                vui,
                nal_hrd,
                vcl_hrd,
                level,
                profile,
            },
        );
        Ok(())
    }

    /// Standard structures for `pps`, replacing any entry under its id.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn insert_pps(&mut self, pps: &Pps) -> Result<(), ParseError> {
        let scaling = pps.scaling().map(|lists| Box::new(fill_scaling(lists)));
        let entry = PpsEntry {
            scaling,
            l0_default: narrow_u8(pps.num_ref_idx_l0_default_minus1(), "pps l0 default")?,
            l1_default: narrow_u8(pps.num_ref_idx_l1_default_minus1(), "pps l1 default")?,
            bipred: bipred_to_std(pps.weighted_bipred_idc()),
            qp: narrow_i8(pps.pic_init_qp_minus26(), "pps init qp")?,
            qs: narrow_i8(pps.pic_init_qs_minus26(), "pps init qs")?,
            chroma: narrow_i8(pps.chroma_qp_index_offset(), "pps chroma qp")?,
            chroma2: narrow_i8(pps.second_chroma_qp_index_offset(), "pps chroma qp 2")?,
        };
        self.pps.insert(pps.id(), entry);
        Ok(())
    }
}

impl Default for H264Tables {
    fn default() -> Self {
        Self::new()
    }
}

/// Standard sequence set with pointers into `entry`.
fn fill_sps(sps: &Sps, entry: &SpsEntry) -> StdVideoH264SequenceParameterSet {
    let mut flags = StdVideoH264SpsFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 2]),
        __bindgen_padding_0: 0,
    };
    for index in 0..6 {
        let set = u32::from(sps.constraint_set(index));
        match index {
            0 => flags.set_constraint_set0_flag(set),
            1 => flags.set_constraint_set1_flag(set),
            2 => flags.set_constraint_set2_flag(set),
            3 => flags.set_constraint_set3_flag(set),
            4 => flags.set_constraint_set4_flag(set),
            _ => flags.set_constraint_set5_flag(set),
        }
    }
    flags.set_direct_8x8_inference_flag(u32::from(sps.direct_8x8_inference()));
    flags.set_mb_adaptive_frame_field_flag(u32::from(sps.mb_adaptive_frame_field()));
    flags.set_frame_mbs_only_flag(u32::from(sps.frame_mbs_only()));
    flags.set_delta_pic_order_always_zero_flag(u32::from(sps.delta_pic_order_always_zero()));
    flags.set_separate_colour_plane_flag(u32::from(sps.separate_colour_plane()));
    flags.set_gaps_in_frame_num_value_allowed_flag(u32::from(sps.gaps_in_frame_num_allowed()));
    flags
        .set_qpprime_y_zero_transform_bypass_flag(u32::from(sps.qpprime_y_zero_transform_bypass()));
    flags.set_frame_cropping_flag(u32::from(sps.frame_cropping()));
    flags.set_seq_scaling_matrix_present_flag(u32::from(sps.scaling().is_some()));
    flags.set_vui_parameters_present_flag(u32::from(sps.vui().is_some()));
    StdVideoH264SequenceParameterSet {
        flags,
        profile_idc: entry.profile,
        level_idc: entry.level,
        chroma_format_idc: chroma_to_std(sps.chroma_format_idc()),
        seq_parameter_set_id: sps.id(),
        bit_depth_luma_minus8: sps.bit_depth_luma_minus8(),
        bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8(),
        log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4(),
        pic_order_cnt_type: poc_to_std(sps.pic_order_cnt_type()),
        offset_for_non_ref_pic: sps.offset_for_non_ref_pic(),
        offset_for_top_to_bottom_field: sps.offset_for_top_to_bottom_field(),
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4(),
        num_ref_frames_in_pic_order_cnt_cycle: sps.num_ref_frames_in_poc_cycle(),
        max_num_ref_frames: sps.max_num_ref_frames(),
        reserved1: 0,
        pic_width_in_mbs_minus1: u32::from(sps.pic_width_in_mbs_minus1()),
        pic_height_in_map_units_minus1: u32::from(sps.pic_height_in_map_units_minus1()),
        frame_crop_left_offset: sps.crop()[0],
        frame_crop_right_offset: sps.crop()[1],
        frame_crop_top_offset: sps.crop()[2],
        frame_crop_bottom_offset: sps.crop()[3],
        reserved2: 0,
        pOffsetForRefFrame: entry
            .offsets
            .as_deref()
            .map_or(std::ptr::null(), <[i32]>::as_ptr),
        pScalingLists: entry
            .scaling
            .as_deref()
            .map_or(std::ptr::null(), std::ptr::from_ref),
        pSequenceParameterSetVui: entry
            .vui
            .as_deref()
            .map_or(std::ptr::null(), std::ptr::from_ref),
    }
}

/// Standard picture set with pointers into `entry`.
fn fill_pps(pps: &Pps, entry: &PpsEntry) -> StdVideoH264PictureParameterSet {
    let mut flags = StdVideoH264PpsFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    flags.set_transform_8x8_mode_flag(u32::from(pps.transform_8x8_mode()));
    flags.set_redundant_pic_cnt_present_flag(u32::from(pps.redundant_pic_cnt_present()));
    flags.set_constrained_intra_pred_flag(u32::from(pps.constrained_intra_pred()));
    flags.set_deblocking_filter_control_present_flag(u32::from(pps.deblocking_control_present()));
    flags.set_weighted_pred_flag(u32::from(pps.weighted_pred()));
    flags.set_bottom_field_pic_order_in_frame_present_flag(u32::from(
        pps.bottom_field_pic_order_present(),
    ));
    flags.set_entropy_coding_mode_flag(u32::from(pps.entropy_coding_mode()));
    flags.set_pic_scaling_matrix_present_flag(u32::from(pps.scaling().is_some()));
    StdVideoH264PictureParameterSet {
        flags,
        seq_parameter_set_id: pps.sps_id(),
        pic_parameter_set_id: pps.id(),
        num_ref_idx_l0_default_active_minus1: entry.l0_default,
        num_ref_idx_l1_default_active_minus1: entry.l1_default,
        weighted_bipred_idc: entry.bipred,
        pic_init_qp_minus26: entry.qp,
        pic_init_qs_minus26: entry.qs,
        chroma_qp_index_offset: entry.chroma,
        second_chroma_qp_index_offset: entry.chroma2,
        pScalingLists: entry
            .scaling
            .as_deref()
            .map_or(std::ptr::null(), std::ptr::from_ref),
    }
}

/// Standard scaling lists from one parsed set.
fn fill_scaling(lists: &crate::parse::h264::ScalingLists) -> StdVideoH264ScalingLists {
    StdVideoH264ScalingLists {
        scaling_list_present_mask: lists.present_mask,
        use_default_scaling_matrix_mask: lists.default_mask,
        ScalingList4x4: lists.list_4x4,
        ScalingList8x8: lists.list_8x8,
    }
}

/// Standard VUI parameters with an unlinked HRD pointer.
///
/// The read path links `pHrdParameters` at the entry HRD backing.
fn fill_vui(vui: &VuiParams) -> Result<StdVideoH264SequenceParameterSetVui, ParseError> {
    let mut flags = StdVideoH264SpsVuiFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 2]),
        __bindgen_padding_0: 0,
    };
    flags.set_aspect_ratio_info_present_flag(u32::from(vui.aspect.is_some()));
    flags.set_overscan_info_present_flag(u32::from(vui.overscan.is_some()));
    flags.set_overscan_appropriate_flag(u32::from(
        vui.overscan.is_some_and(|overscan| overscan.appropriate),
    ));
    flags.set_video_signal_type_present_flag(u32::from(vui.signal.is_some()));
    flags.set_video_full_range_flag(u32::from(
        vui.signal.is_some_and(|signal| signal.full_range),
    ));
    flags.set_color_description_present_flag(u32::from(
        vui.signal.is_some_and(|signal| signal.colour.is_some()),
    ));
    flags.set_chroma_loc_info_present_flag(u32::from(vui.chroma_loc.is_some()));
    flags.set_timing_info_present_flag(u32::from(vui.timing.is_some()));
    flags.set_fixed_frame_rate_flag(u32::from(
        vui.timing.is_some_and(|timing| timing.fixed_rate),
    ));
    flags.set_bitstream_restriction_flag(u32::from(vui.max_num_reorder_frames.is_some()));
    flags.set_nal_hrd_parameters_present_flag(u32::from(vui.nal_hrd.is_some()));
    flags.set_vcl_hrd_parameters_present_flag(u32::from(vui.vcl_hrd.is_some()));
    let aspect = vui.aspect.unwrap_or_default();
    let signal = vui.signal.unwrap_or_default();
    let colour = signal.colour.unwrap_or_default();
    let chroma = vui.chroma_loc.unwrap_or_default();
    let timing = vui.timing.unwrap_or_default();
    Ok(StdVideoH264SequenceParameterSetVui {
        flags,
        aspect_ratio_idc: aspect_to_std(aspect.idc),
        sar_width: aspect.sar_width,
        sar_height: aspect.sar_height,
        video_format: signal.format,
        colour_primaries: colour.primaries,
        transfer_characteristics: colour.transfer,
        matrix_coefficients: colour.matrix,
        num_units_in_tick: timing.num_units_in_tick,
        time_scale: timing.time_scale,
        max_num_reorder_frames: narrow_u8(
            vui.max_num_reorder_frames.unwrap_or(0),
            "vui reorder frames",
        )?,
        max_dec_frame_buffering: narrow_u8(
            vui.max_dec_frame_buffering.unwrap_or(0),
            "vui max buffering",
        )?,
        chroma_sample_loc_type_top_field: narrow_u8(chroma.top, "vui chroma top")?,
        chroma_sample_loc_type_bottom_field: narrow_u8(chroma.bottom, "vui chroma bottom")?,
        reserved1: 0,
        pHrdParameters: std::ptr::null(),
    })
}

/// Standard HRD parameters.
fn fill_hrd(hrd: &HrdParams) -> StdVideoH264HrdParameters {
    let mut bitrate = [0_u32; 32];
    let mut cpb_size = [0_u32; 32];
    let mut cbr = [0_u8; 32];
    for (index, value) in hrd.bitrate_minus1.iter().enumerate() {
        bitrate[index] = *value;
    }
    for (index, value) in hrd.cpb_size_minus1.iter().enumerate() {
        cpb_size[index] = *value;
    }
    for (index, value) in hrd.cbr.iter().enumerate() {
        cbr[index] = u8::from(*value);
    }
    StdVideoH264HrdParameters {
        cpb_cnt_minus1: hrd.cpb_count_minus1,
        bit_rate_scale: hrd.bitrate_scale,
        cpb_size_scale: hrd.cpb_scale,
        reserved1: 0,
        bit_rate_value_minus1: bitrate,
        cpb_size_value_minus1: cpb_size,
        cbr_flag: cbr,
        initial_cpb_removal_delay_length_minus1: u32::from(hrd.initial_delay_minus1),
        cpb_removal_delay_length_minus1: u32::from(hrd.removal_delay_minus1),
        dpb_output_delay_length_minus1: u32::from(hrd.output_delay_minus1),
        time_offset_length: u32::from(hrd.time_offset_length),
    }
}

/// Standard picture info for one decoded picture.
pub(crate) fn fill_picture(picture: &Picture) -> StdVideoDecodeH264PictureInfo {
    let mut flags = StdVideoDecodeH264PictureInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    flags.set_field_pic_flag(0);
    flags.set_is_intra(u32::from(picture.is_intra()));
    flags.set_IdrPicFlag(u32::from(picture.is_idr()));
    flags.set_bottom_field_flag(0);
    flags.set_is_reference(u32::from(picture.is_reference()));
    flags.set_complementary_field_pair(0);
    StdVideoDecodeH264PictureInfo {
        flags,
        seq_parameter_set_id: picture.sps_id(),
        pic_parameter_set_id: picture.pps_id(),
        reserved1: 0,
        reserved2: 0,
        frame_num: picture.frame_num(),
        idr_pic_id: picture.idr_pic_id(),
        PicOrderCnt: [picture.poc(), picture.poc()],
    }
}

/// Standard reference info for one DPB picture.
pub(crate) fn fill_reference(
    frame_num: u16,
    poc: i32,
    long_term: bool,
) -> StdVideoDecodeH264ReferenceInfo {
    let mut flags = StdVideoDecodeH264ReferenceInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    // Frame references set neither field flag; both set corrupts B
    // prediction. FFmpeg vulkan_h264.c vk_h264_fill_pict agrees.
    flags.set_top_field_flag(0);
    flags.set_bottom_field_flag(0);
    flags.set_used_for_long_term_reference(u32::from(long_term));
    flags.set_is_non_existing(0);
    StdVideoDecodeH264ReferenceInfo {
        flags,
        FrameNum: frame_num,
        reserved: 0,
        PicOrderCnt: [poc, poc],
    }
}

/// Wire chroma idc to its standard idc.
fn chroma_to_std(chroma_format_idc: u8) -> u32 {
    match chroma_format_idc {
        0 => StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME,
        1 => StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
        2 => StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_422,
        _ => StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_444,
    }
}

/// Wire POC type to its standard type.
fn poc_to_std(pic_order_cnt_type: u8) -> StdVideoH264PocType {
    match pic_order_cnt_type {
        0 => StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
        1 => StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_1,
        _ => StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2,
    }
}

/// Wire bipred idc to its standard idc.
fn bipred_to_std(weighted_bipred_idc: u8) -> u32 {
    match weighted_bipred_idc {
        0 => StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
        1 => StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_EXPLICIT,
        2 => StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_IMPLICIT,
        _ => StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_INVALID,
    }
}

/// Wire aspect idc to its standard idc.
fn aspect_to_std(aspect_idc: u8) -> u32 {
    if aspect_idc <= 17 {
        u32::from(aspect_idc)
    } else {
        StdVideoH264AspectRatioIdc_STD_VIDEO_H264_ASPECT_RATIO_IDC_INVALID
    }
}

/// Checked narrowing of a fill value to `u8`.
fn narrow_u8(value: u32, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a fill value to `i8`.
fn narrow_i8(value: i32, syntax: &'static str) -> Result<i8, ParseError> {
    i8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

#[cfg(test)]
mod tests {
    use crate::parse::au::{AccessUnit, AuSplitter};
    use crate::parse::h264::{H264Parser, Nalu, ParsedNalu};
    use crate::parse::refs::{H264Context, H264Instruction};
    use crate::parse::test_support::{hex, split};

    /// Fork `demo.h264` sequence set.
    const SPS: &str = "6764000dacb20283f60220000003002000000641e2854900";
    /// Fork `demo.h264` picture set.
    const PPS: &str = "68ebc3cb22c0";
    /// Fork `demo.h264` IDR slice head.
    const IDR: &str =
        "6588840cfffef6ecbe053614c85094a04007bb443688f75792a97d0e881b266f395d1803e3eea5efbae04cafddfd1d8f5aa3b94b6acca893a915d63dcd";
    /// Fork `demo.h264` first P slice head.
    const P: &str = "419a3b10cffe9e2a5997d0339bc66a0ec39c80006fafc5a3a0c7e796ec144229f6721f2948";

    /// Parsed sequence and picture sets of the fork demo.
    fn demo_sets() -> (
        std::rc::Rc<crate::parse::h264::Sps>,
        std::rc::Rc<crate::parse::h264::Pps>,
    ) {
        let units = split(&[hex(SPS), hex(PPS)]);
        let mut parser = H264Parser::new();
        let ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let ParsedNalu::Pps(pps) = parser.parse(&units[1]).expect("pps") else {
            panic!("want pps");
        };
        (sps, pps)
    }

    /// Access units out of parsed H.264 `payloads`.
    fn units264(payloads: &[Vec<u8>]) -> Vec<AccessUnit<Nalu>> {
        let mut parser = H264Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            let nalu = Nalu::new(parsed);
            if let Some(unit) = splitter.put(nalu) {
                units.push(unit);
            }
        }
        if let Some(unit) = splitter.flush() {
            units.push(unit);
        }
        units
    }

    #[test]
    fn demo_sps_fills_std() {
        use ash::vk::native::{
            StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_3,
            StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2,
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
        };
        let (sps, _) = demo_sets();
        let mut tables = super::H264Tables::new();
        tables.insert_sps(&sps).expect("insert");
        let std = tables.sps(0, &sps).expect("std sps");
        assert_eq!(
            std.profile_idc,
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH
        );
        assert_eq!(
            std.level_idc,
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_3
        );
        assert_eq!(
            std.chroma_format_idc,
            StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420
        );
        assert_eq!(std.pic_width_in_mbs_minus1, 19);
        assert_eq!(std.pic_height_in_map_units_minus1, 14);
        assert_eq!(std.log2_max_frame_num_minus4, 0);
        assert_eq!(
            std.pic_order_cnt_type,
            StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_2
        );
        assert_eq!(std.max_num_ref_frames, 3);
        assert_eq!(std.flags.frame_mbs_only_flag(), 1);
        assert_eq!(std.flags.frame_cropping_flag(), 0);
        assert_eq!(std.flags.vui_parameters_present_flag(), 1);
        assert_eq!(std.flags.seq_scaling_matrix_present_flag(), 0);
        assert!(!std.pSequenceParameterSetVui.is_null());
        assert!(std.pScalingLists.is_null());
        assert!(std.pOffsetForRefFrame.is_null());
    }

    #[test]
    fn demo_pps_fills_std() {
        use ash::vk::native::StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT;
        let (_, pps) = demo_sets();
        let mut tables = super::H264Tables::new();
        tables.insert_pps(&pps).expect("insert");
        let std = tables.pps(0, &pps).expect("std pps");
        assert_eq!(std.seq_parameter_set_id, 0);
        assert_eq!(std.num_ref_idx_l0_default_active_minus1, 2);
        assert_eq!(std.num_ref_idx_l1_default_active_minus1, 0);
        assert_eq!(
            std.weighted_bipred_idc,
            StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT
        );
        assert_eq!(std.pic_init_qp_minus26, -3);
        assert_eq!(std.pic_init_qs_minus26, 0);
        assert_eq!(std.chroma_qp_index_offset, -2);
        assert_eq!(std.second_chroma_qp_index_offset, -2);
        assert_eq!(std.flags.entropy_coding_mode_flag(), 1);
        assert_eq!(std.flags.weighted_pred_flag(), 1);
        assert_eq!(std.flags.transform_8x8_mode_flag(), 1);
        assert_eq!(std.flags.deblocking_filter_control_present_flag(), 1);
        assert!(std.pScalingLists.is_null());
    }

    #[test]
    fn demo_idr_picture_fills_std() {
        let mut units = units264(&[hex(SPS), hex(PPS), hex(IDR)]);
        let mut context = H264Context::new();
        let first = context.compile(units.remove(0));
        let Some(H264Instruction::Picture(idr)) = first.into_iter().next_back() else {
            panic!("want picture");
        };
        let std = super::fill_picture(&idr);
        assert_eq!(std.flags.is_intra(), 1);
        assert_eq!(std.flags.IdrPicFlag(), 1);
        assert_eq!(std.flags.is_reference(), 1);
        assert_eq!(std.flags.field_pic_flag(), 0);
        assert_eq!(std.seq_parameter_set_id, 0);
        assert_eq!(std.pic_parameter_set_id, 0);
        assert_eq!(std.frame_num, 0);
        assert_eq!(std.PicOrderCnt, [0, 0]);
    }

    #[test]
    fn demo_p_reference_fills_std() {
        let mut units = units264(&[hex(SPS), hex(PPS), hex(IDR), hex(P)]);
        let mut context = H264Context::new();
        let _ = context.compile(units.remove(0));
        let second = context.compile(units.remove(0));
        let Some(H264Instruction::Picture(p)) = second.into_iter().next() else {
            panic!("want picture");
        };
        let std = super::fill_picture(&p);
        assert_eq!(std.flags.is_intra(), 0);
        assert_eq!(std.flags.IdrPicFlag(), 0);
        assert_eq!(std.PicOrderCnt, [p.poc(), p.poc()]);
        let first = &p.l0().expect("l0")[0];
        let reference = super::fill_reference(first.frame_num(), first.poc(), false);
        // Frame reference carries neither field flag; FFmpeg vk_h264_fill_pict
        // gates both on is_field, and B output corrupts when both set.
        assert_eq!(reference.flags.top_field_flag(), 0);
        assert_eq!(reference.flags.bottom_field_flag(), 0);
        assert_eq!(reference.flags.used_for_long_term_reference(), 0);
        assert_eq!(reference.FrameNum, 0);
        assert_eq!(reference.PicOrderCnt, [0, 0]);
    }

    /// High 1920x1080 sequence set.
    const HIGH_SPS: &str = "67640028acd940780227e5c044000003000400000300f03c60c658";
    /// High vector picture set.
    const HIGH_PPS: &str = "68ef8fcb";

    /// Parsed High 1920x1080 level 4.0 sets.
    fn high_vector_sets() -> (
        std::rc::Rc<crate::parse::h264::Sps>,
        std::rc::Rc<crate::parse::h264::Pps>,
    ) {
        let units = split(&[hex(HIGH_SPS), hex(HIGH_PPS)]);
        let mut parser = H264Parser::new();
        let ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let ParsedNalu::Pps(pps) = parser.parse(&units[1]).expect("pps") else {
            panic!("want pps");
        };
        (sps, pps)
    }

    #[test]
    fn high_vector_std_sps() {
        use ash::vk::native::{
            StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0,
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
        };
        let (sps, _) = high_vector_sets();
        let mut tables = super::H264Tables::new();
        tables.insert_sps(&sps).expect("sps");
        let std_sps = tables.sps(sps.id(), &sps).expect("std sps");
        assert_eq!(
            std_sps.profile_idc,
            StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH
        );
        assert_eq!(
            std_sps.level_idc,
            StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0
        );
        assert_eq!(
            std_sps.chroma_format_idc,
            StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420
        );
        // 1920x1080 in 16-pixel units, padded to 1920x1088; the 8 leftover
        // luma rows are 4 4:2:0 crop units off the bottom (H.264 7.4.2.1).
        assert_eq!(std_sps.pic_width_in_mbs_minus1, 119);
        assert_eq!(std_sps.pic_height_in_map_units_minus1, 67);
        assert_eq!(
            [
                std_sps.frame_crop_left_offset,
                std_sps.frame_crop_right_offset,
                std_sps.frame_crop_top_offset,
                std_sps.frame_crop_bottom_offset,
            ],
            [0, 0, 0, 4]
        );
        assert_eq!(
            std_sps.pic_order_cnt_type,
            u32::from(sps.pic_order_cnt_type())
        );
        assert_eq!(std_sps.max_num_ref_frames, sps.max_num_ref_frames());
        assert_eq!(
            std_sps.log2_max_pic_order_cnt_lsb_minus4,
            sps.log2_max_pic_order_cnt_lsb_minus4()
        );
        assert_eq!(
            std_sps.log2_max_frame_num_minus4,
            sps.log2_max_frame_num_minus4()
        );
        assert_eq!(
            std_sps.flags.direct_8x8_inference_flag(),
            u32::from(sps.direct_8x8_inference())
        );
        assert_eq!(
            std_sps.flags.gaps_in_frame_num_value_allowed_flag(),
            u32::from(sps.gaps_in_frame_num_allowed())
        );
        assert_eq!(
            std_sps.flags.frame_mbs_only_flag(),
            u32::from(sps.frame_mbs_only())
        );
        assert!(std_sps.pOffsetForRefFrame.is_null());
    }

    #[test]
    fn high_vector_std_pps() {
        use ash::vk::native::StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_IMPLICIT;
        let (_, pps) = high_vector_sets();
        let mut tables = super::H264Tables::new();
        tables.insert_pps(&pps).expect("pps");
        let std_pps = tables.pps(pps.id(), &pps).expect("std pps");
        assert_eq!(
            std_pps.weighted_bipred_idc,
            StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_IMPLICIT
        );
        assert_eq!(
            std_pps.flags.weighted_pred_flag(),
            u32::from(pps.weighted_pred())
        );
        assert_eq!(
            u32::from(std_pps.num_ref_idx_l0_default_active_minus1),
            pps.num_ref_idx_l0_default_minus1()
        );
        assert_eq!(
            u32::from(std_pps.num_ref_idx_l1_default_active_minus1),
            pps.num_ref_idx_l1_default_minus1()
        );
        assert_eq!(
            std_pps.flags.entropy_coding_mode_flag(),
            u32::from(pps.entropy_coding_mode())
        );
        assert_eq!(
            std_pps.flags.transform_8x8_mode_flag(),
            u32::from(pps.transform_8x8_mode())
        );
        assert_eq!(
            std_pps.flags.deblocking_filter_control_present_flag(),
            u32::from(pps.deblocking_control_present())
        );
        assert_eq!(
            std_pps.pic_init_qp_minus26,
            pps.pic_init_qp_minus26().try_into().expect("qp width")
        );
        assert_eq!(
            std_pps.chroma_qp_index_offset,
            pps.chroma_qp_index_offset()
                .try_into()
                .expect("chroma width")
        );
        assert_eq!(
            std_pps.second_chroma_qp_index_offset,
            pps.second_chroma_qp_index_offset()
                .try_into()
                .expect("chroma2 width")
        );
    }

    #[test]
    fn unknown_level_rejected_at_insert() {
        let units = split(&[hex("67640063acb20283f60220000003002000000641e2854900")]);
        let mut parser = H264Parser::new();
        let ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let mut tables = super::H264Tables::new();
        assert!(tables.insert_sps(&sps).is_err());
    }
}
