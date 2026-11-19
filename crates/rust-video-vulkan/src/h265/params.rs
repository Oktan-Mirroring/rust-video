//! H.265 standard structures from parsed parameter sets.
//!
//! Pure host-side fills: parsed sets in, `StdVideo` structures out.
//! Pointer fields borrow box-stable backing owned by [`H265Tables`],
//! so a filled structure stays valid while its table entry lives.

use std::collections::HashMap;

use ash::vk::native::{
    __BindgenBitfieldUnit, StdVideoDecodeH265PictureInfo, StdVideoDecodeH265PictureInfoFlags,
    StdVideoDecodeH265ReferenceInfo, StdVideoDecodeH265ReferenceInfoFlags,
    StdVideoH265AspectRatioIdc_STD_VIDEO_H265_ASPECT_RATIO_IDC_INVALID,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_422,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_444,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_MONOCHROME,
    StdVideoH265DecPicBufMgr, StdVideoH265HrdFlags, StdVideoH265HrdParameters,
    StdVideoH265LongTermRefPicsSps, StdVideoH265PictureParameterSet, StdVideoH265PpsFlags,
    StdVideoH265ProfileTierLevel, StdVideoH265ProfileTierLevelFlags, StdVideoH265ScalingLists,
    StdVideoH265SequenceParameterSet, StdVideoH265SequenceParameterSetVui,
    StdVideoH265ShortTermRefPicSet, StdVideoH265ShortTermRefPicSetFlags, StdVideoH265SpsFlags,
    StdVideoH265SpsVuiFlags, StdVideoH265SubLayerHrdParameters, StdVideoH265VideoParameterSet,
    StdVideoH265VpsFlags,
};

use super::dpb::H265Picture;
use crate::dpb::SlotBook;
use crate::error::ParseError;
use crate::h265::{
    HrdLayerData, HrdParams, Pps, ProfileTierLevel, ScalingLists, ShortTermRps, Sps, Vps,
    VuiParams, PPS_SCALING, SPS_NESTING, SPS_ORDERING_INFO,
};
use crate::session::geometry::{h265_level_to_std, h265_profile_to_std};

/// H.265 parameter tables with box-stable standard-structure backing.
pub(crate) struct H265Tables {
    /// Video entries by set id.
    vps: HashMap<u8, VpsEntry>,
    /// Sequence entries by set id.
    sps: HashMap<u8, SpsEntry>,
    /// Picture entries by set id.
    pps: HashMap<u8, PpsEntry>,
}

/// Box-stable backing of one HRD parameter set.
#[derive(Debug)]
struct HrdBacking {
    /// Standard HRD parameters.
    std: Box<StdVideoH265HrdParameters>,
    /// NAL sub-layer parameters.
    nal: Box<[StdVideoH265SubLayerHrdParameters; 7]>,
    /// VCL sub-layer parameters.
    vcl: Box<[StdVideoH265SubLayerHrdParameters; 7]>,
}

/// Box-stable backing of one video set.
struct VpsEntry {
    /// Standard profile, tier and level.
    ptl: Box<StdVideoH265ProfileTierLevel>,
    /// Standard DPB manager.
    dec_buf: Box<StdVideoH265DecPicBufMgr>,
    /// Standard HRD sets in coded order.
    hrd: Vec<HrdBacking>,
}

/// Box-stable backing of one VUI block.
#[derive(Debug)]
struct VuiBacking {
    /// Standard VUI parameters.
    std: Box<StdVideoH265SequenceParameterSetVui>,
    /// Standard HRD parameters, when present.
    hrd: Option<HrdBacking>,
}

/// Box-stable backing of one sequence set.
struct SpsEntry {
    /// Standard profile, tier and level.
    ptl: Box<StdVideoH265ProfileTierLevel>,
    /// Standard DPB manager.
    dec_buf: Box<StdVideoH265DecPicBufMgr>,
    /// Standard scaling lists, when present.
    scaling: Option<Box<StdVideoH265ScalingLists>>,
    /// Standard short-term sets in coded order.
    rps: Box<[StdVideoH265ShortTermRefPicSet]>,
    /// Standard long-term pictures.
    lt: Box<StdVideoH265LongTermRefPicsSps>,
    /// Standard VUI block, when present.
    vui: Option<VuiBacking>,
}

/// Box-stable backing of one picture set.
struct PpsEntry {
    /// Standard scaling lists, when present.
    scaling: Option<Box<StdVideoH265ScalingLists>>,
    /// Tile column widths minus 1.
    widths: [u16; 19],
    /// Tile row heights minus 1.
    heights: [u16; 21],
    /// Video set id of the parent sequence set.
    vps_id: u8,
}

impl H265Tables {
    /// Empty tables.
    pub(crate) fn new() -> Self {
        Self {
            vps: HashMap::new(),
            sps: HashMap::new(),
            pps: HashMap::new(),
        }
    }

    /// Standard video set for `id`, if stored.
    ///
    /// Links every stored HRD at its sub-layer backing on every read, so
    /// the insert order of the boxes never matters.
    pub(crate) fn vps(&mut self, id: u8, parsed: &Vps) -> Option<StdVideoH265VideoParameterSet> {
        let entry = self.vps.get_mut(&id)?;
        for backing in &mut entry.hrd {
            backing.std.pSubLayerHrdParametersNal = std::ptr::addr_of!(backing.nal[0]);
            backing.std.pSubLayerHrdParametersVcl = std::ptr::addr_of!(backing.vcl[0]);
        }
        Some(fill_vps(parsed, entry))
    }

    /// Standard sequence set for `id`, if stored.
    ///
    /// Links the stored VUI and every HRD at their backing on every read.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn sps(
        &mut self,
        id: u8,
        parsed: &Sps,
    ) -> Option<Result<StdVideoH265SequenceParameterSet, ParseError>> {
        let entry = self.sps.get_mut(&id)?;
        if let Some(vui) = entry.vui.as_mut() {
            if let Some(hrd) = vui.hrd.as_mut() {
                hrd.std.pSubLayerHrdParametersNal = std::ptr::addr_of!(hrd.nal[0]);
                hrd.std.pSubLayerHrdParametersVcl = std::ptr::addr_of!(hrd.vcl[0]);
            }
            vui.std.pHrdParameters = vui.hrd.as_ref().map_or(std::ptr::null(), |backing| {
                std::ptr::from_ref(backing.std.as_ref())
            });
        }
        Some(fill_sps(parsed, entry))
    }

    /// Standard picture set for `id`, if stored.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn pps(&self, id: u8, parsed: &Pps) -> Option<StdVideoH265PictureParameterSet> {
        self.pps.get(&id).map(|entry| fill_pps(parsed, entry))
    }

    /// Standard structures for `vps`, replacing any entry under its id.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn insert_vps(&mut self, vps: &Vps) -> Result<(), ParseError> {
        let ptl = Box::new(fill_ptl(&vps.ptl)?);
        let dec_buf = Box::new(fill_dec_buf(
            vps.ordering_info_present,
            usize::from(vps.max_sub_layers_minus1) + 1,
            &vps.max_dec_pic_buffering,
            &vps.max_num_reorder_pics,
            &vps.max_latency,
        )?);
        let mut hrd = Vec::new();
        if let Some(timing) = vps.timing.as_ref() {
            let mut first: Option<&HrdParams> = None;
            for set in &timing.hrd {
                hrd.push(fill_hrd(set, first)?);
                if first.is_none() {
                    first = Some(set);
                }
            }
        }
        self.vps.insert(vps.id, VpsEntry { ptl, dec_buf, hrd });
        Ok(())
    }

    /// Standard structures for `sps`, replacing any entry under its id.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn insert_sps(&mut self, sps: &Sps) -> Result<(), ParseError> {
        let ptl = Box::new(fill_ptl(&sps.ptl)?);
        let buffering: Vec<u32> = sps
            .ordering_buffering
            .iter()
            .map(|v| u32::from(*v))
            .collect();
        let reorder: Vec<u32> = sps.ordering_reorder.iter().map(|v| u32::from(*v)).collect();
        let dec_buf = Box::new(fill_dec_buf(
            sps.flags & SPS_ORDERING_INFO != 0,
            usize::from(sps.max_sub_layers).max(1),
            &buffering,
            &reorder,
            &sps.ordering_latency,
        )?);
        let scaling = sps
            .scaling_enabled()
            .then(|| Box::new(fill_scaling(&sps.scaling)));
        let mut rps = Vec::with_capacity(sps.rps.len());
        for set in &sps.rps {
            rps.push(fill_rps(set)?);
        }
        let lt = Box::new(fill_lt_sps(&sps.lt_poc_lsb, &sps.lt_used_by_curr));
        let vui = sps
            .vui
            .as_ref()
            .map(|vui| -> Result<VuiBacking, ParseError> {
                let hrd = vui
                    .hrd
                    .as_ref()
                    .map(|hrd| fill_hrd(hrd, None))
                    .transpose()?;
                Ok(VuiBacking {
                    std: Box::new(fill_vui(vui)?),
                    hrd,
                })
            })
            .transpose()?;
        self.sps.insert(
            sps.id,
            SpsEntry {
                ptl,
                dec_buf,
                scaling,
                rps: rps.into_boxed_slice(),
                lt,
                vui,
            },
        );
        Ok(())
    }

    /// Standard structures for `pps` under `sps`, replacing any entry under its id.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a value exceeds its standard field width.
    pub(crate) fn insert_pps(&mut self, pps: &Pps, sps: &Sps) -> Result<(), ParseError> {
        let scaling = (pps.flags & PPS_SCALING != 0).then(|| Box::new(fill_scaling(&pps.scaling)));
        let mut widths = [0_u16; 19];
        for (index, width) in pps.column_width.iter().enumerate() {
            widths[index] = narrow_u16(u32::from(*width) - 1, "pps column width")?;
        }
        let mut heights = [0_u16; 21];
        for (index, height) in pps.row_height.iter().enumerate() {
            heights[index] = narrow_u16(u32::from(*height) - 1, "pps row height")?;
        }
        self.pps.insert(
            pps.id,
            PpsEntry {
                scaling,
                widths,
                heights,
                vps_id: sps.vps_id,
            },
        );
        Ok(())
    }
}

impl Default for H265Tables {
    fn default() -> Self {
        Self::new()
    }
}

/// Standard video set with pointers into `entry`.
fn fill_vps(vps: &Vps, entry: &VpsEntry) -> StdVideoH265VideoParameterSet {
    let mut flags = StdVideoH265VpsFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    flags.set_vps_temporal_id_nesting_flag(u32::from(vps.temporal_nesting));
    flags.set_vps_sub_layer_ordering_info_present_flag(u32::from(vps.ordering_info_present));
    let timing = vps.timing.as_ref();
    flags.set_vps_timing_info_present_flag(u32::from(timing.is_some()));
    flags.set_vps_poc_proportional_to_timing_flag(u32::from(
        timing.is_some_and(|timing| timing.poc_proportional),
    ));
    StdVideoH265VideoParameterSet {
        flags,
        vps_video_parameter_set_id: vps.id,
        vps_max_sub_layers_minus1: vps.max_sub_layers_minus1,
        reserved1: 0,
        reserved2: 0,
        vps_num_units_in_tick: timing.map_or(0, |timing| timing.num_units_in_tick),
        vps_time_scale: timing.map_or(0, |timing| timing.time_scale),
        vps_num_ticks_poc_diff_one_minus1: timing
            .map_or(0, |timing| timing.num_ticks_poc_diff_one_minus1),
        reserved3: 0,
        pDecPicBufMgr: std::ptr::from_ref(entry.dec_buf.as_ref()),
        pHrdParameters: entry.hrd.first().map_or(std::ptr::null(), |backing| {
            std::ptr::from_ref(backing.std.as_ref())
        }),
        pProfileTierLevel: std::ptr::from_ref(entry.ptl.as_ref()),
    }
}

/// Standard sequence-set flags.
fn sps_flags(sps: &Sps) -> StdVideoH265SpsFlags {
    let mut flags = StdVideoH265SpsFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_sps_temporal_id_nesting_flag(u32::from(sps.flags & SPS_NESTING != 0));
    flags.set_separate_colour_plane_flag(u32::from(sps.separate_colour_plane));
    flags.set_conformance_window_flag(u32::from(sps.crop.is_some()));
    flags.set_sps_sub_layer_ordering_info_present_flag(u32::from(
        sps.flags & SPS_ORDERING_INFO != 0,
    ));
    flags.set_scaling_list_enabled_flag(u32::from(sps.scaling_enabled()));
    flags.set_sps_scaling_list_data_present_flag(u32::from(sps.scaling_enabled()));
    flags.set_amp_enabled_flag(u32::from(sps.amp()));
    flags.set_sample_adaptive_offset_enabled_flag(u32::from(sps.sao()));
    flags.set_pcm_enabled_flag(u32::from(sps.pcm()));
    flags.set_pcm_loop_filter_disabled_flag(u32::from(sps.pcm() && sps.pcm_loop_disabled()));
    flags.set_long_term_ref_pics_present_flag(u32::from(sps.long_term_present()));
    flags.set_sps_temporal_mvp_enabled_flag(u32::from(sps.temporal_mvp()));
    flags.set_strong_intra_smoothing_enabled_flag(u32::from(sps.smoothing()));
    flags.set_vui_parameters_present_flag(u32::from(sps.vui.is_some()));
    flags
}

/// Standard sequence set with pointers into `entry`.
fn fill_sps(sps: &Sps, entry: &SpsEntry) -> Result<StdVideoH265SequenceParameterSet, ParseError> {
    let flags = sps_flags(sps);
    let window = sps.crop.map_or([0; 4], |crop| {
        [crop.left / 2, crop.right / 2, crop.top / 2, crop.bottom / 2]
    });
    let pcm = sps.pcm();
    Ok(StdVideoH265SequenceParameterSet {
        flags,
        chroma_format_idc: chroma_to_std(sps.chroma_format),
        pic_width_in_luma_samples: sps.width,
        pic_height_in_luma_samples: sps.height,
        sps_video_parameter_set_id: sps.vps_id,
        sps_max_sub_layers_minus1: sps.max_sub_layers.saturating_sub(1),
        sps_seq_parameter_set_id: sps.id,
        bit_depth_luma_minus8: sps.bit_depth_luma,
        bit_depth_chroma_minus8: sps.bit_depth_chroma,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_poc_lsb_minus4,
        log2_min_luma_coding_block_size_minus3: sps.log2_min_cb_size_minus3,
        log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_cb_size,
        log2_min_luma_transform_block_size_minus2: sps.log2_min_tb_size_minus2,
        log2_diff_max_min_luma_transform_block_size: sps.log2_diff_max_min_tb_size,
        max_transform_hierarchy_depth_inter: sps.max_transform_depth_inter,
        max_transform_hierarchy_depth_intra: sps.max_transform_depth_intra,
        num_short_term_ref_pic_sets: narrow_len(sps.rps.len(), "sps num rps")?,
        num_long_term_ref_pics_sps: narrow_len(sps.lt_poc_lsb.len(), "sps num lt")?,
        pcm_sample_bit_depth_luma_minus1: if pcm { sps.pcm_depth_luma } else { 0 },
        pcm_sample_bit_depth_chroma_minus1: if pcm { sps.pcm_depth_chroma } else { 0 },
        log2_min_pcm_luma_coding_block_size_minus3: if pcm { sps.pcm_log2_min_minus3 } else { 0 },
        log2_diff_max_min_pcm_luma_coding_block_size: if pcm { sps.pcm_log2_diff } else { 0 },
        reserved1: 0,
        reserved2: 0,
        palette_max_size: 0,
        delta_palette_max_predictor_size: 0,
        motion_vector_resolution_control_idc: 0,
        sps_num_palette_predictor_initializers_minus1: 0,
        conf_win_left_offset: window[0],
        conf_win_right_offset: window[1],
        conf_win_top_offset: window[2],
        conf_win_bottom_offset: window[3],
        pProfileTierLevel: std::ptr::from_ref(entry.ptl.as_ref()),
        pDecPicBufMgr: std::ptr::from_ref(entry.dec_buf.as_ref()),
        pScalingLists: entry
            .scaling
            .as_deref()
            .map_or(std::ptr::null(), std::ptr::from_ref),
        pShortTermRefPicSet: entry.rps.as_ptr(),
        pLongTermRefPicsSps: std::ptr::from_ref(entry.lt.as_ref()),
        pSequenceParameterSetVui: entry
            .vui
            .as_ref()
            .map_or(std::ptr::null(), |vui| std::ptr::from_ref(vui.std.as_ref())),
        pPredictorPaletteEntries: std::ptr::null(),
    })
}

/// Standard picture-set flags.
fn pps_flags(pps: &Pps) -> StdVideoH265PpsFlags {
    let mut flags = StdVideoH265PpsFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_dependent_slice_segments_enabled_flag(u32::from(pps.dependent_slices()));
    flags.set_output_flag_present_flag(u32::from(pps.output_flag_present()));
    flags.set_sign_data_hiding_enabled_flag(u32::from(pps.sign_hiding()));
    flags.set_cabac_init_present_flag(u32::from(pps.cabac_init_present()));
    flags.set_constrained_intra_pred_flag(u32::from(pps.constrained_intra()));
    flags.set_transform_skip_enabled_flag(u32::from(pps.transform_skip()));
    flags.set_cu_qp_delta_enabled_flag(u32::from(pps.cu_qp_delta()));
    flags.set_pps_slice_chroma_qp_offsets_present_flag(u32::from(pps.chroma_offsets_present()));
    flags.set_weighted_pred_flag(u32::from(pps.weighted_pred()));
    flags.set_weighted_bipred_flag(u32::from(pps.weighted_bipred()));
    flags.set_transquant_bypass_enabled_flag(u32::from(pps.transquant_bypass()));
    flags.set_tiles_enabled_flag(u32::from(pps.tiles_enabled()));
    flags.set_entropy_coding_sync_enabled_flag(u32::from(pps.entropy_sync()));
    flags.set_uniform_spacing_flag(u32::from(pps.uniform_spacing()));
    flags.set_loop_filter_across_tiles_enabled_flag(u32::from(pps.loop_across_tiles()));
    flags.set_pps_loop_filter_across_slices_enabled_flag(u32::from(pps.loop_across_slices()));
    flags.set_deblocking_filter_control_present_flag(u32::from(pps.deblock_control_present));
    flags.set_deblocking_filter_override_enabled_flag(u32::from(pps.deblocking_override()));
    flags.set_pps_deblocking_filter_disabled_flag(u32::from(pps.deblocking_disabled()));
    flags.set_pps_scaling_list_data_present_flag(u32::from(pps.flags & PPS_SCALING != 0));
    flags.set_lists_modification_present_flag(u32::from(pps.lists_modification_present()));
    flags.set_slice_segment_header_extension_present_flag(u32::from(pps.slice_ext_present()));
    flags
}

/// Standard picture set with pointers into `entry`.
fn fill_pps(pps: &Pps, entry: &PpsEntry) -> StdVideoH265PictureParameterSet {
    let flags = pps_flags(pps);
    StdVideoH265PictureParameterSet {
        flags,
        pps_pic_parameter_set_id: pps.id,
        pps_seq_parameter_set_id: pps.sps_id,
        sps_video_parameter_set_id: entry.vps_id,
        num_extra_slice_header_bits: pps.num_extra_slice_header_bits,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_l0,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_l1,
        init_qp_minus26: pps.pic_init_qp_minus26,
        diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth,
        pps_cb_qp_offset: pps.cb_qp_offset,
        pps_cr_qp_offset: pps.cr_qp_offset,
        pps_beta_offset_div2: pps.beta_offset_div2,
        pps_tc_offset_div2: pps.tc_offset_div2,
        log2_parallel_merge_level_minus2: pps.log2_parallel_merge_minus2,
        log2_max_transform_skip_block_size_minus2: 0,
        diff_cu_chroma_qp_offset_depth: 0,
        chroma_qp_offset_list_len_minus1: 0,
        cb_qp_offset_list: [0; 6],
        cr_qp_offset_list: [0; 6],
        log2_sao_offset_scale_luma: 0,
        log2_sao_offset_scale_chroma: 0,
        pps_act_y_qp_offset_plus5: 0,
        pps_act_cb_qp_offset_plus5: 0,
        pps_act_cr_qp_offset_plus3: 0,
        pps_num_palette_predictor_initializers: 0,
        luma_bit_depth_entry_minus8: 0,
        chroma_bit_depth_entry_minus8: 0,
        num_tile_columns_minus1: pps.num_tile_columns.saturating_sub(1),
        num_tile_rows_minus1: pps.num_tile_rows.saturating_sub(1),
        reserved1: 0,
        reserved2: 0,
        column_width_minus1: entry.widths,
        row_height_minus1: entry.heights,
        reserved3: 0,
        pScalingLists: entry
            .scaling
            .as_deref()
            .map_or(std::ptr::null(), std::ptr::from_ref),
        pPredictorPaletteEntries: std::ptr::null(),
    }
}

/// Standard profile, tier and level.
fn fill_ptl(ptl: &ProfileTierLevel) -> Result<StdVideoH265ProfileTierLevel, ParseError> {
    let mut flags = StdVideoH265ProfileTierLevelFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    flags.set_general_tier_flag(u32::from(ptl.tier));
    flags.set_general_progressive_source_flag(u32::from(ptl.source.progressive));
    flags.set_general_interlaced_source_flag(0);
    flags.set_general_non_packed_constraint_flag(u32::from(ptl.constraints.non_packed));
    flags.set_general_frame_only_constraint_flag(u32::from(ptl.constraints.frame_only));
    let profile = h265_profile_to_std(ptl.profile_idc).ok_or(ParseError::OutOfRange {
        syntax: "ptl profile",
    })?;
    let level = h265_level_to_std(ptl.level_idc).ok_or(ParseError::OutOfRange {
        syntax: "ptl level",
    })?;
    Ok(StdVideoH265ProfileTierLevel {
        flags,
        general_profile_idc: profile,
        general_level_idc: level,
    })
}

/// Standard DPB manager for `layers` sub-layers.
fn fill_dec_buf(
    ordering_info: bool,
    layers: usize,
    buffering: &[u32],
    reorder: &[u32],
    latency: &[u32],
) -> Result<StdVideoH265DecPicBufMgr, ParseError> {
    let mut mgr = StdVideoH265DecPicBufMgr {
        max_latency_increase_plus1: [0; 7],
        max_dec_pic_buffering_minus1: [0; 7],
        max_num_reorder_pics: [0; 7],
    };
    for layer in 0..layers.min(7) {
        let coded = if ordering_info { layer } else { 0 };
        let buffering = buffering
            .get(coded)
            .copied()
            .ok_or(ParseError::OutOfRange {
                syntax: "dec buffering",
            })?;
        let reorder = reorder.get(coded).copied().ok_or(ParseError::OutOfRange {
            syntax: "dec reorder",
        })?;
        let latency = latency.get(coded).copied().ok_or(ParseError::OutOfRange {
            syntax: "dec latency",
        })?;
        mgr.max_dec_pic_buffering_minus1[layer] = narrow_u8(buffering, "dec buffering")?;
        mgr.max_num_reorder_pics[layer] = narrow_u8(reorder, "dec reorder")?;
        mgr.max_latency_increase_plus1[layer] = latency.saturating_add(1);
    }
    Ok(mgr)
}

/// Standard scaling lists from the parsed lists.
fn fill_scaling(lists: &ScalingLists) -> StdVideoH265ScalingLists {
    StdVideoH265ScalingLists {
        ScalingList4x4: lists.size0,
        ScalingList8x8: lists.size1,
        ScalingList16x16: lists.size2,
        ScalingList32x32: lists.size3,
        ScalingListDCCoef16x16: lists.dc2,
        ScalingListDCCoef32x32: lists.dc3,
    }
}

/// Standard short-term reference picture set.
pub(crate) fn fill_rps(set: &ShortTermRps) -> Result<StdVideoH265ShortTermRefPicSet, ParseError> {
    let mut flags = StdVideoH265ShortTermRefPicSetFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    flags.set_inter_ref_pic_set_prediction_flag(u32::from(set.predicted));
    flags.set_delta_rps_sign(u32::from(set.delta_sign));
    let s0 = pairs(&set.negative, &set.negative_used, set.negative_count);
    let s1 = pairs(&set.positive, &set.positive_used, set.positive_count);
    if s0.len() + s1.len() > 16 {
        return Err(ParseError::OutOfRange { syntax: "rps size" });
    }
    let (mut used, mut use_delta) = (set.used_mask, set.use_mask);
    if !set.predicted {
        (used, use_delta) = pack_explicit_masks(&s0, &s1);
    }
    let mut s0_used = 0_u16;
    let mut s0_delta = [0_u16; 16];
    fill_delta_array(&s0, &mut s0_used, &mut s0_delta)?;
    let mut s1_used = 0_u16;
    let mut s1_delta = [0_u16; 16];
    fill_delta_array(&s1, &mut s1_used, &mut s1_delta)?;
    Ok(StdVideoH265ShortTermRefPicSet {
        flags,
        delta_idx_minus1: set.delta_idx_minus1,
        use_delta_flag: use_delta,
        abs_delta_rps_minus1: narrow_abs_delta(set.abs_delta_minus1)?,
        used_by_curr_pic_flag: used,
        used_by_curr_pic_s0_flag: s0_used,
        used_by_curr_pic_s1_flag: s1_used,
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        num_negative_pics: narrow_len(s0.len(), "rps negative")?,
        num_positive_pics: narrow_len(s1.len(), "rps positive")?,
        delta_poc_s0_minus1: s0_delta,
        delta_poc_s1_minus1: s1_delta,
    })
}

/// Delta and used-flag pairs of one stored list.
fn pairs(deltas: &[i32], used: &[bool], count: u8) -> Vec<(i32, bool)> {
    deltas
        .iter()
        .zip(used.iter())
        .take(usize::from(count))
        .map(|(delta, keep)| (*delta, *keep))
        .collect()
}

/// Checked narrowing of an absolute delta RPS to `u16`.
fn narrow_abs_delta(value: u32) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange {
        syntax: "rps abs delta",
    })
}

/// Used and use-delta masks of an explicit set: packed used flags, all kept.
fn pack_explicit_masks(s0: &[(i32, bool)], s1: &[(i32, bool)]) -> (u16, u16) {
    let mut used = 0_u16;
    let mut use_delta = 0_u16;
    for (bit, (_, keep)) in s0.iter().chain(s1.iter()).enumerate() {
        if bit >= 16 {
            break;
        }
        let flag = 1_u16 << bit;
        if *keep {
            used |= flag;
        }
        use_delta |= flag;
    }
    (used, use_delta)
}

/// Successive delta gaps and used mask of one stored list.
fn fill_delta_array(
    list: &[(i32, bool)],
    used: &mut u16,
    delta: &mut [u16; 16],
) -> Result<(), ParseError> {
    let mut prev = 0_u32;
    for (index, (poc, keep)) in list.iter().enumerate() {
        if *keep {
            *used |= 1 << index;
        }
        let magnitude = poc.unsigned_abs();
        let gap = magnitude
            .checked_sub(prev)
            .and_then(|gap| gap.checked_sub(1))
            .ok_or(ParseError::OutOfRange {
                syntax: "rps delta",
            })?;
        delta[index] = u16::try_from(gap).map_err(|_| ParseError::OutOfRange {
            syntax: "rps delta",
        })?;
        prev = magnitude;
    }
    Ok(())
}

/// Standard long-term pictures from the sequence set.
fn fill_lt_sps(poc_lsb: &[u32], used_by_curr: &[bool]) -> StdVideoH265LongTermRefPicsSps {
    let mut used = 0_u32;
    let mut poc = [0_u32; 32];
    for (index, (lsb, keep)) in poc_lsb.iter().zip(used_by_curr.iter()).enumerate() {
        if *keep {
            used |= 1 << index;
        }
        poc[index] = *lsb;
    }
    StdVideoH265LongTermRefPicsSps {
        used_by_curr_pic_lt_sps_flag: used,
        lt_ref_pic_poc_lsb_sps: poc,
    }
}

/// Standard VUI parameters; `hrd` points at the HRD backing.
fn fill_vui(vui: &VuiParams) -> Result<StdVideoH265SequenceParameterSetVui, ParseError> {
    let mut flags = StdVideoH265SpsVuiFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 3]),
        __bindgen_padding_0: 0,
    };
    flags.set_aspect_ratio_info_present_flag(u32::from(vui.aspect.present));
    flags.set_overscan_info_present_flag(u32::from(vui.overscan.present));
    flags.set_overscan_appropriate_flag(u32::from(vui.overscan.appropriate));
    flags.set_video_signal_type_present_flag(u32::from(vui.signal.present));
    flags.set_video_full_range_flag(u32::from(vui.signal.full_range));
    flags.set_colour_description_present_flag(u32::from(vui.signal.colour_present));
    flags.set_chroma_loc_info_present_flag(u32::from(vui.chroma_loc.present));
    flags.set_neutral_chroma_indication_flag(u32::from(vui.neutral_chroma));
    flags.set_field_seq_flag(u32::from(vui.field_seq));
    flags.set_frame_field_info_present_flag(u32::from(vui.frame_field_info));
    flags.set_default_display_window_flag(u32::from(vui.display_window.is_some()));
    flags.set_vui_timing_info_present_flag(u32::from(vui.timing.present));
    flags.set_vui_poc_proportional_to_timing_flag(u32::from(vui.timing.proportional));
    flags.set_vui_hrd_parameters_present_flag(u32::from(vui.hrd.is_some()));
    let restriction = vui.restriction;
    flags.set_bitstream_restriction_flag(u32::from(restriction.is_some()));
    flags.set_tiles_fixed_structure_flag(u32::from(
        restriction.is_some_and(|restriction| restriction.tiles_fixed),
    ));
    flags.set_motion_vectors_over_pic_boundaries_flag(u32::from(
        restriction.is_some_and(|restriction| restriction.mv_over_pic),
    ));
    flags.set_restricted_ref_pic_lists_flag(u32::from(
        restriction.is_some_and(|restriction| restriction.restricted_ref),
    ));
    let window = vui.display_window.unwrap_or([0; 4]);
    Ok(StdVideoH265SequenceParameterSetVui {
        flags,
        aspect_ratio_idc: aspect_to_std(vui.aspect.idc),
        sar_width: narrow_u16(vui.aspect.sar_width, "vui sar width")?,
        sar_height: narrow_u16(vui.aspect.sar_height, "vui sar height")?,
        video_format: vui.signal.format,
        colour_primaries: vui.signal.primaries,
        transfer_characteristics: vui.signal.transfer,
        matrix_coeffs: vui.signal.matrix,
        chroma_sample_loc_type_top_field: narrow_u8(vui.chroma_loc.top, "vui chroma top")?,
        chroma_sample_loc_type_bottom_field: narrow_u8(vui.chroma_loc.bottom, "vui chroma bottom")?,
        reserved1: 0,
        reserved2: 0,
        def_disp_win_left_offset: narrow_u16(window[0], "vui display left")?,
        def_disp_win_right_offset: narrow_u16(window[1], "vui display right")?,
        def_disp_win_top_offset: narrow_u16(window[2], "vui display top")?,
        def_disp_win_bottom_offset: narrow_u16(window[3], "vui display bottom")?,
        vui_num_units_in_tick: vui.timing.num_units_in_tick,
        vui_time_scale: vui.timing.time_scale,
        vui_num_ticks_poc_diff_one_minus1: vui.timing.num_ticks_poc_diff_one_minus1,
        min_spatial_segmentation_idc: restriction.map_or(Ok(0), |restriction| {
            narrow_u16(restriction.min_spatial_segmentation, "vui min segmentation")
        })?,
        reserved3: 0,
        max_bytes_per_pic_denom: restriction.map_or(Ok(0), |restriction| {
            narrow_u8(restriction.max_bytes_per_pic_denom, "vui max bytes")
        })?,
        max_bits_per_min_cu_denom: restriction.map_or(Ok(0), |restriction| {
            narrow_u8(restriction.max_bits_per_mcu_denom, "vui max bits")
        })?,
        log2_max_mv_length_horizontal: restriction.map_or(Ok(0), |restriction| {
            narrow_u8(restriction.max_mv_horizontal, "vui max mv h")
        })?,
        log2_max_mv_length_vertical: restriction.map_or(Ok(0), |restriction| {
            narrow_u8(restriction.max_mv_vertical, "vui max mv v")
        })?,
        pHrdParameters: std::ptr::null(),
    })
}

/// Standard HRD parameters; `first` carries the shared header an inheriting set reuses.
fn fill_hrd(hrd: &HrdParams, first: Option<&HrdParams>) -> Result<HrdBacking, ParseError> {
    let shared = if hrd.common {
        hrd
    } else {
        first.unwrap_or(hrd)
    };
    let mut flags = StdVideoH265HrdFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_nal_hrd_parameters_present_flag(u32::from(shared.streams.nal));
    flags.set_vcl_hrd_parameters_present_flag(u32::from(shared.streams.vcl));
    flags.set_sub_pic_hrd_params_present_flag(u32::from(shared.sub_pic));
    flags.set_sub_pic_cpb_params_in_pic_timing_sei_flag(0);
    let top = hrd.fixed_rate.len().saturating_sub(1);
    let fixed_general = hrd.fixed_rate.get(top).copied().unwrap_or(false);
    let fixed_within = hrd.pic_duration.get(top).copied().unwrap_or(false);
    let elemental = hrd
        .elemental
        .get(top)
        .copied()
        .flatten()
        .map(|duration| narrow_u16(duration, "hrd elemental"))
        .transpose()?
        .unwrap_or(0);
    flags.set_fixed_pic_rate_general_flag(u32::from(fixed_general));
    flags.set_fixed_pic_rate_within_cvs_flag(u32::from(fixed_within));
    flags.set_low_delay_hrd_flag(u32::from(!fixed_general && fixed_within && elemental == 0));
    let mut cpb_cnt = [0_u8; 7];
    let mut elemental_out = [0_u16; 7];
    for (layer, count) in hrd.cpb_count_minus1.iter().enumerate().take(7) {
        cpb_cnt[layer] = narrow_u8(*count, "hrd cpb count")?;
    }
    for (layer, duration) in hrd.elemental.iter().enumerate().take(7) {
        elemental_out[layer] = duration
            .map(|duration| narrow_u16(duration, "hrd elemental"))
            .transpose()?
            .unwrap_or(0);
    }
    let mut nal = Box::new([sub_layer_zero(); 7]);
    let mut vcl = Box::new([sub_layer_zero(); 7]);
    for (layer, entries) in hrd.nal_layers.iter().enumerate().take(7) {
        nal[layer] = fill_sub_layer(entries)?;
    }
    for (layer, entries) in hrd.vcl_layers.iter().enumerate().take(7) {
        vcl[layer] = fill_sub_layer(entries)?;
    }
    let std = Box::new(StdVideoH265HrdParameters {
        flags,
        tick_divisor_minus2: shared.tick_divisor_minus2,
        du_cpb_removal_delay_increment_length_minus1: 0,
        dpb_output_delay_du_length_minus1: 0,
        bit_rate_scale: shared.bitrate_scale,
        cpb_size_scale: shared.cpb_scale,
        cpb_size_du_scale: shared.du_cpb_scale,
        initial_cpb_removal_delay_length_minus1: shared.initial_delay_minus1,
        au_cpb_removal_delay_length_minus1: shared.au_delay_minus1,
        dpb_output_delay_length_minus1: shared.dpb_delay_minus1,
        cpb_cnt_minus1: cpb_cnt,
        elemental_duration_in_tc_minus1: elemental_out,
        reserved: [0; 3],
        pSubLayerHrdParametersNal: std::ptr::null(),
        pSubLayerHrdParametersVcl: std::ptr::null(),
    });
    Ok(HrdBacking { std, nal, vcl })
}

/// Zeroed sub-layer HRD parameters.
fn sub_layer_zero() -> StdVideoH265SubLayerHrdParameters {
    StdVideoH265SubLayerHrdParameters {
        bit_rate_value_minus1: [0; 32],
        cpb_size_value_minus1: [0; 32],
        cpb_size_du_value_minus1: [0; 32],
        bit_rate_du_value_minus1: [0; 32],
        cbr_flag: 0,
    }
}

/// Standard sub-layer HRD parameters from one coded layer.
fn fill_sub_layer(layer: &HrdLayerData) -> Result<StdVideoH265SubLayerHrdParameters, ParseError> {
    if layer.bitrate_minus1.len() > 32 {
        return Err(ParseError::OutOfRange {
            syntax: "hrd layer size",
        });
    }
    let mut out = sub_layer_zero();
    for (index, value) in layer.bitrate_minus1.iter().enumerate() {
        out.bit_rate_value_minus1[index] = *value;
    }
    for (index, value) in layer.cpb_size_minus1.iter().enumerate() {
        out.cpb_size_value_minus1[index] = *value;
    }
    for (index, value) in layer.du_cpb_size_minus1.iter().enumerate() {
        out.cpb_size_du_value_minus1[index] = *value;
    }
    for (index, value) in layer.du_bitrate_minus1.iter().enumerate() {
        out.bit_rate_du_value_minus1[index] = *value;
    }
    for (index, cbr) in layer.cbr.iter().enumerate() {
        if *cbr {
            out.cbr_flag |= 1 << index;
        }
    }
    Ok(out)
}

/// Unused reference-set entry: a reference the DPB cannot supply.
const UNUSED_SLOT: u8 = 0xFF;

/// Standard picture info for one decoded picture.
///
/// Reference ids resolve through `slots`; an unresolvable entry stays
/// [`UNUSED_SLOT`] so it holds its list position, and only an over-long
/// reference list fails.
pub(crate) fn fill_picture(
    picture: &H265Picture,
    slots: &SlotBook,
) -> Result<StdVideoDecodeH265PictureInfo, ParseError> {
    let mut flags = StdVideoDecodeH265PictureInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    let echo = picture.echo();
    let body = picture.picture();
    flags.set_IrapPicFlag(u32::from(echo.is_irap()));
    flags.set_IdrPicFlag(u32::from(echo.is_idr()));
    flags.set_IsReference(u32::from(body.is_reference()));
    flags.set_short_term_ref_pic_set_sps_flag(u32::from(echo.sps_flag()));
    Ok(StdVideoDecodeH265PictureInfo {
        flags,
        sps_video_parameter_set_id: echo.vps_id(),
        pps_seq_parameter_set_id: body.sps_id(),
        pps_pic_parameter_set_id: body.pps_id(),
        NumDeltaPocsOfRefRpsIdx: echo.ref_count(),
        PicOrderCntVal: body.poc(),
        NumBitsForSTRefPicSetInSlice: echo.bits(),
        reserved: 0,
        RefPicSetStCurrBefore: resolve_list(echo.before(), slots, "rps before")?,
        RefPicSetStCurrAfter: resolve_list(echo.after(), slots, "rps after")?,
        RefPicSetLtCurr: resolve_list(echo.lt(), slots, "rps lt")?,
    })
}

/// DPB slot indices of one echo list.
fn resolve_list(
    list: &[Option<crate::dpb::ReferenceId>],
    slots: &SlotBook,
    syntax: &'static str,
) -> Result<[u8; 8], ParseError> {
    if list.len() > 8 {
        return Err(ParseError::OutOfRange { syntax });
    }
    let mut out = [UNUSED_SLOT; 8];
    for (index, id) in list.iter().enumerate() {
        let Some(slot) = id.and_then(|id| slots.slot_of(id)) else {
            continue;
        };
        out[index] = u8::try_from(slot).map_err(|_| ParseError::OutOfRange { syntax })?;
    }
    Ok(out)
}

/// Standard reference info for one DPB picture.
pub(crate) fn fill_reference(poc: i32, long_term: bool) -> StdVideoDecodeH265ReferenceInfo {
    let mut flags = StdVideoDecodeH265ReferenceInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
        __bindgen_padding_0: [0; 3],
    };
    flags.set_used_for_long_term_reference(u32::from(long_term));
    flags.set_unused_for_reference(0);
    StdVideoDecodeH265ReferenceInfo {
        flags,
        PicOrderCntVal: poc,
    }
}

/// Wire chroma idc to its standard idc.
fn chroma_to_std(chroma_format_idc: u8) -> u32 {
    match chroma_format_idc {
        0 => StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_MONOCHROME,
        1 => StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
        2 => StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_422,
        _ => StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_444,
    }
}

/// Wire aspect idc to its standard idc.
fn aspect_to_std(aspect_idc: u8) -> u32 {
    if aspect_idc <= 17 {
        u32::from(aspect_idc)
    } else {
        StdVideoH265AspectRatioIdc_STD_VIDEO_H265_ASPECT_RATIO_IDC_INVALID
    }
}

/// Checked narrowing of a fill value to `u8`.
fn narrow_u8(value: u32, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a fill value to `u16`.
fn narrow_u16(value: u32, syntax: &'static str) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a length to `u8`.
fn narrow_len(value: usize, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

#[cfg(test)]
mod tests {
    use crate::au::{AccessUnit, AuSplitter};
    use crate::dpb::{ReferenceId, SlotBook};
    use crate::fixtures::{hex, split265};
    use crate::h265::dpb::{H265Context, H265Instruction};
    use crate::h265::{H265Parser, ParsedNalu};

    /// Fork `demo.h265` video set.
    const VPS: &str = "40010c01ffff01600000030090000003000003003c92809000";
    /// Fork `demo.h265` sequence set.
    const SPS: &str =
        "42010101600000030090000003000003003ca00a080f16592a4932bc05a0200000030020000003032100";
    /// Fork `demo.h265` picture set.
    const PPS: &str = "4401c172b4624000";
    /// Fork `demo.h265` IDR slice head.
    const IDR: &str =
        "2801af1d20a965478be0af3f4ae63151b2fffd273e7fa20b405b9ec15d0337a20427f6e32f9f583c57fee84d42429d33ffe3ff4aff62991873852f99fb8bf1a358a444ab906f2a97e66aa67a1c";
    /// Fork `demo.h265` first inter slice head.
    const T: &str =
        "0201d0097e10c63a430a3d70fd7fb96259b8fc398744b4135036fbd45ed7fcdb5f3d45c649013c88aa2c8c2fdf";

    /// Parsed parameter sets of the fork demo.
    fn demo_sets() -> (
        std::rc::Rc<crate::h265::Vps>,
        std::rc::Rc<crate::h265::Sps>,
        std::rc::Rc<crate::h265::Pps>,
    ) {
        let units = split265(&[hex(VPS), hex(SPS), hex(PPS)]);
        let mut parser = H265Parser::new();
        let ParsedNalu::Vps(vps) = parser.parse(&units[0]).expect("vps") else {
            panic!("want vps");
        };
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        let ParsedNalu::Pps(pps) = parser.parse(&units[2]).expect("pps") else {
            panic!("want pps");
        };
        (vps, sps, pps)
    }

    /// Access units out of parsed H.265 `payloads`.
    fn units265(payloads: &[Vec<u8>]) -> Vec<AccessUnit<ParsedNalu>> {
        let mut parser = H265Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split265(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            if let Some(unit) = splitter.put(parsed) {
                units.push(unit);
            }
        }
        if let Some(unit) = splitter.flush() {
            units.push(unit);
        }
        units
    }

    #[test]
    fn demo_vps_fills_std() {
        let (vps, _, _) = demo_sets();
        let mut tables = super::H265Tables::new();
        tables.insert_vps(&vps).expect("insert");
        let std = tables.vps(0, &vps).expect("std");
        assert_eq!(std.vps_video_parameter_set_id, 0);
        assert_eq!(std.vps_max_sub_layers_minus1, 0);
        assert_eq!(std.flags.vps_temporal_id_nesting_flag(), 1);
        assert_eq!(std.flags.vps_sub_layer_ordering_info_present_flag(), 1);
        assert_eq!(std.flags.vps_timing_info_present_flag(), 0);
        assert!(!std.pDecPicBufMgr.is_null());
        assert!(!std.pProfileTierLevel.is_null());
        assert!(std.pHrdParameters.is_null());
    }

    #[test]
    fn demo_sps_fills_std() {
        use ash::vk::native::{
            StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
            StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_2_0,
            StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN,
        };
        let (_, sps, _) = demo_sets();
        let mut tables = super::H265Tables::new();
        tables.insert_sps(&sps).expect("insert");
        let std = tables.sps(0, &sps).expect("std").expect("std");
        assert_eq!(
            std.chroma_format_idc,
            StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420
        );
        assert_eq!(std.pic_width_in_luma_samples, 320);
        assert_eq!(std.pic_height_in_luma_samples, 240);
        assert_eq!(std.log2_max_pic_order_cnt_lsb_minus4, 4);
        assert_eq!(std.log2_min_luma_coding_block_size_minus3, 0);
        assert_eq!(std.log2_diff_max_min_luma_coding_block_size, 3);
        assert_eq!(std.log2_min_luma_transform_block_size_minus2, 0);
        assert_eq!(std.log2_diff_max_min_luma_transform_block_size, 3);
        assert_eq!(std.max_transform_hierarchy_depth_inter, 0);
        assert_eq!(std.max_transform_hierarchy_depth_intra, 0);
        assert_eq!(std.num_short_term_ref_pic_sets, 0);
        assert_eq!(std.num_long_term_ref_pics_sps, 0);
        assert_eq!(std.flags.sample_adaptive_offset_enabled_flag(), 1);
        assert_eq!(std.flags.amp_enabled_flag(), 0);
        assert_eq!(std.flags.sps_temporal_mvp_enabled_flag(), 1);
        assert_eq!(std.flags.pcm_enabled_flag(), 0);
        assert_eq!(std.flags.vui_parameters_present_flag(), 1);
        assert!(std.pScalingLists.is_null());
        assert!(!std.pSequenceParameterSetVui.is_null());
        let profile = super::fill_ptl(&sps.ptl).expect("ptl");
        assert_eq!(
            profile.general_profile_idc,
            StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN
        );
        assert_eq!(
            profile.general_level_idc,
            StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_2_0
        );
        assert_eq!(profile.flags.general_tier_flag(), 0);
        assert_eq!(profile.flags.general_progressive_source_flag(), 1);
    }

    #[test]
    fn demo_pps_fills_std() {
        let (_, sps, pps) = demo_sets();
        let mut tables = super::H265Tables::new();
        tables.insert_pps(&pps, &sps).expect("insert");
        let std = tables.pps(0, &pps).expect("std");
        assert_eq!(std.pps_pic_parameter_set_id, 0);
        assert_eq!(std.pps_seq_parameter_set_id, 0);
        assert_eq!(std.sps_video_parameter_set_id, 0);
        assert_eq!(std.num_extra_slice_header_bits, 0);
        assert_eq!(std.num_ref_idx_l0_default_active_minus1, 0);
        assert_eq!(std.num_ref_idx_l1_default_active_minus1, 0);
        assert_eq!(std.init_qp_minus26, 0);
        assert_eq!(std.diff_cu_qp_delta_depth, 1);
        assert_eq!(std.pps_cb_qp_offset, 0);
        assert_eq!(std.pps_cr_qp_offset, 0);
        assert_eq!(std.log2_parallel_merge_level_minus2, 0);
        assert_eq!(std.flags.tiles_enabled_flag(), 0);
        assert_eq!(std.flags.cabac_init_present_flag(), 0);
        assert_eq!(std.flags.lists_modification_present_flag(), 0);
        assert_eq!(std.flags.deblocking_filter_control_present_flag(), 0);
        assert!(std.pScalingLists.is_null());
    }

    #[test]
    fn backing_pointers_stay_stable() {
        let (_, sps, _) = demo_sets();
        let mut tables = super::H265Tables::new();
        tables.insert_sps(&sps).expect("sps");
        let first = tables.sps(0, &sps).expect("std").expect("std");
        let repeat = tables.sps(0, &sps).expect("std").expect("std");
        assert_eq!(first.pProfileTierLevel, repeat.pProfileTierLevel);
        assert_eq!(first.pDecPicBufMgr, repeat.pDecPicBufMgr);
        tables.insert_sps(&sps).expect("reinsert");
        let second = tables.sps(0, &sps).expect("std").expect("std");
        assert_ne!(first.pProfileTierLevel, second.pProfileTierLevel);
    }

    #[test]
    fn demo_idr_picture_fills_std() {
        let mut units = units265(&[hex(VPS), hex(SPS), hex(PPS), hex(IDR)]);
        let mut context = H265Context::new();
        let first = context.compile(units.remove(0));
        let Some(H265Instruction::Picture(idr)) = first.into_iter().next_back() else {
            panic!("want picture");
        };
        let book = SlotBook::with_slots(4);
        let std = super::fill_picture(&idr, &book).expect("fill");
        assert_eq!(std.flags.IrapPicFlag(), 1);
        assert_eq!(std.flags.IdrPicFlag(), 1);
        assert_eq!(std.flags.IsReference(), 1);
        assert_eq!(std.flags.short_term_ref_pic_set_sps_flag(), 0);
        assert_eq!(std.sps_video_parameter_set_id, 0);
        assert_eq!(std.pps_seq_parameter_set_id, 0);
        assert_eq!(std.pps_pic_parameter_set_id, 0);
        assert_eq!(std.PicOrderCntVal, 0);
        assert_eq!(std.NumBitsForSTRefPicSetInSlice, 0);
        assert_eq!(std.RefPicSetStCurrBefore, [0xFF; 8]);
        assert_eq!(std.RefPicSetStCurrAfter, [0xFF; 8]);
        assert_eq!(std.RefPicSetLtCurr, [0xFF; 8]);
    }

    #[test]
    fn demo_inter_picture_conceals_missing_refs() {
        let mut units = units265(&[hex(VPS), hex(SPS), hex(PPS), hex(IDR), hex(T)]);
        let mut context = H265Context::new();
        let _ = context.compile(units.remove(0));
        let second = context.compile(units.remove(0));
        let Some(H265Instruction::Picture(p)) = second.into_iter().next() else {
            panic!("want picture");
        };
        let mut book = SlotBook::with_slots(4);
        for id in p
            .echo()
            .before()
            .iter()
            .chain(p.echo().after())
            .chain(p.echo().lt())
            .flatten()
        {
            book.claim(*id).expect("slot");
        }
        let std = super::fill_picture(&p, &book).expect("fill");
        assert_eq!(std.flags.IrapPicFlag(), 0);
        assert_eq!(std.flags.IdrPicFlag(), 0);
        assert_eq!(std.PicOrderCntVal, 1);
        assert_eq!(std.NumBitsForSTRefPicSetInSlice, 6);
        assert_eq!(std.RefPicSetStCurrBefore[0], 0);
        assert_eq!(&std.RefPicSetStCurrBefore[1..3], &[0xFF, 0xFF]);
        assert_eq!(std.RefPicSetStCurrAfter, [0xFF; 8]);
        assert_eq!(std.RefPicSetLtCurr, [0xFF; 8]);
    }

    #[test]
    fn over_long_reference_list_rejected() {
        let book = SlotBook::with_slots(16);
        let ids: Vec<Option<ReferenceId>> = (0..9).map(|index| Some(ReferenceId(index))).collect();
        assert!(super::resolve_list(&ids, &book, "rps before").is_err());
    }

    #[test]
    fn dec_buf_maps_layers() {
        let mgr = super::fill_dec_buf(true, 2, &[3, 5], &[1, 2], &[0, 7]).expect("fill");
        assert_eq!(mgr.max_dec_pic_buffering_minus1[..2], [3, 5]);
        assert_eq!(mgr.max_num_reorder_pics[..2], [1, 2]);
        assert_eq!(mgr.max_latency_increase_plus1[..2], [1, 8]);
        assert_eq!(mgr.max_dec_pic_buffering_minus1[2], 0);
        let single = super::fill_dec_buf(false, 3, &[4], &[2], &[9]).expect("fill");
        assert_eq!(single.max_dec_pic_buffering_minus1[..3], [4, 4, 4]);
        assert_eq!(single.max_latency_increase_plus1[..3], [10, 10, 10]);
    }

    #[test]
    fn reference_fill_marks_long_term() {
        let short = super::fill_reference(4, false);
        assert_eq!(short.flags.used_for_long_term_reference(), 0);
        assert_eq!(short.PicOrderCntVal, 4);
        let long = super::fill_reference(2, true);
        assert_eq!(long.flags.used_for_long_term_reference(), 1);
    }
}
