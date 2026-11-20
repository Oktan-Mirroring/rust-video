//! H.265 Annex B ingest over the shared grammar.

use rust_video_caps::reason::{ParseReason as SharedParse, UnsupportedReason as SharedUnsupported};
use rust_video_h265::{HevcError, HevcGate, NalAction, ParseKind, UnsupportedKind};

use crate::error::{D3d12Error, ParseReason, UnsupportedReason};

pub(crate) use rust_video_h265::{Picture, Pps, ShortTermRps, Sps};

#[cfg(test)]
pub(crate) use rust_video_h265::{
    split_nals, ConstraintFlags, PendingSlice, ProfileTierLevel, RpsBits, ScalingLists,
    SliceHeader, SliceType, SourceFlags, SPS_SAO, SPS_TEMPORAL_MVP,
};

/// Stream levels the tier runs, H.265 A.4.1.
const LEVELS: [u8; 14] = [
    30, 60, 63, 90, 93, 120, 123, 150, 153, 156, 180, 183, 186, 210,
];

/// Annex B ingest over the tier gate.
pub(crate) type Ingest = rust_video_h265::Ingest<D3d12Error, Gate>;

/// Tier capability gates over shared grammar output.
pub(crate) struct Gate;

impl HevcError for D3d12Error {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        Self::Parse(ParseReason::Parse(match kind {
            ParseKind::EmptyCodedSize => SharedParse::EmptyCodedSize,
            ParseKind::ConformanceOutside => SharedParse::ConfWinOutsideCodedSize,
            ParseKind::RpsTableOversize => SharedParse::Range {
                syntax: "num_short_term_ref_pic_sets",
            },
            ParseKind::LtSpsOversize => SharedParse::Range {
                syntax: "num_long_term_ref_pics_sps",
            },
            ParseKind::LtTotalOversize => SharedParse::Range {
                syntax: "num_long_term_pics",
            },
            ParseKind::EmptySpsRps => SharedParse::Range {
                syntax: "short_term_ref_pic_set_idx",
            },
            ParseKind::Range { syntax } => SharedParse::Range {
                syntax: remap_syntax(syntax),
            },
            ParseKind::Truncated { syntax } => SharedParse::Truncated { syntax },
            ParseKind::OverflowU32 { syntax } => SharedParse::OverflowU32 { syntax },
            ParseKind::OverflowI32 { syntax } => SharedParse::OverflowI32 { syntax },
            ParseKind::TooWide { syntax } => SharedParse::TooWide { syntax },
            ParseKind::NalForbiddenBit => SharedParse::NalForbiddenBit,
            ParseKind::NalTemporalId => SharedParse::NalTemporalId,
            ParseKind::VpsWithoutBody | ParseKind::SpsWithoutBody => SharedParse::SpsWithoutBody,
            ParseKind::PpsWithoutBody => SharedParse::PpsWithoutBody,
            ParseKind::SliceWithoutHeader => SharedParse::SliceWithoutHeader,
            ParseKind::PpsCitesUnknownSps => SharedParse::PpsCitesUnknownSps,
            ParseKind::SliceCitesUnknownPps => SharedParse::SliceCitesUnknownPps,
            ParseKind::SliceCitesUnknownSps => SharedParse::SliceCitesUnknownSps,
            ParseKind::PictureSlicesDisagree => SharedParse::PictureSlicesDisagree,
            ParseKind::NonIntraSliceInIdr => SharedParse::NonIntraSliceInIdr,
            ParseKind::ShortTermRefPicSetSize => SharedParse::ShortTermRefPicSetSize,
        }))
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        Self::Unsupported(UnsupportedReason::Stream(match kind {
            UnsupportedKind::DependentSlices => SharedUnsupported::DependentSlices,
            UnsupportedKind::ArbitrarySliceOrder => SharedUnsupported::ArbitrarySliceOrder,
            UnsupportedKind::Multilayer => SharedUnsupported::Multilayer,
        }))
    }
}

/// Shared syntax label behind the tier label.
fn remap_syntax(syntax: &'static str) -> &'static str {
    match syntax {
        "log2_max_pic_order_cnt_lsb_minus4" => "log2_max_pic_order_cnt_lsb",
        "log2_min_luma_coding_block_size_minus3" => "log2_min_luma_coding_block_size",
        "log2_diff_max_min_luma_coding_block_size" => "log2_diff_max_min_luma_coding_block",
        "log2_min_luma_transform_block_size_minus2" => "log2_min_luma_transform_block_size",
        "log2_diff_max_min_luma_transform_block_size" => "log2_diff_max_min_luma_transform",
        "pic_init_qp_minus26" => "init_qp_minus26",
        _ => syntax,
    }
}

impl HevcGate<D3d12Error> for Gate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), D3d12Error> {
        if sps.max_sub_layers != 1 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::Sublayers,
            )));
        }
        if sps.chroma_format != 1 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::ChromaOutside420,
            )));
        }
        if sps.bit_depth_luma != 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::LumaDepthAbove8Bit,
            )));
        }
        if sps.bit_depth_chroma != 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::ChromaDepthAbove8Bit,
            )));
        }
        if sps.scaling_enabled() {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ScalingListsPending,
            ));
        }
        if sps.pcm() {
            return Err(D3d12Error::Unsupported(UnsupportedReason::PcmPending));
        }
        if sps.long_term_present() {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::LongTermRefsPending,
            ));
        }
        if sps.ptl.profile_idc != 1 && sps.ptl.compat & 0x4000_0000 == 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::ProfileOutsideMain,
            )));
        }
        if sps.ptl.source.interlaced && !sps.ptl.source.progressive {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::Interlaced,
            )));
        }
        if !LEVELS.contains(&sps.ptl.level_idc) {
            return Err(D3d12Error::Parse(ParseReason::Parse(SharedParse::Range {
                syntax: "general_level_idc",
            })));
        }
        if sps.sps_extension_present {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::SpsExtension,
            )));
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), D3d12Error> {
        let Some(sps) = sps else {
            return Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::PpsCitesUnknownSps,
            )));
        };
        if pps.sps_id != sps.id {
            return Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::PpsCitesUnknownSps,
            )));
        }
        if pps.flags & rust_video_h265::PPS_SCALING != 0 {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ScalingListsPending,
            ));
        }
        if pps.diff_cu_qp_delta_depth > 31 {
            return Err(D3d12Error::Parse(ParseReason::Parse(SharedParse::Range {
                syntax: "diff_cu_qp_delta_depth",
            })));
        }
        check_tile_grid(pps.num_tile_columns, pps.num_tile_rows)?;
        let ctb_log2 = sps
            .log2_min_cb_size_minus3
            .saturating_add(3)
            .saturating_add(sps.log2_diff_max_min_cb_size);
        if pps.log2_parallel_merge_minus2 > ctb_log2.saturating_sub(2) {
            return Err(D3d12Error::Parse(ParseReason::Parse(SharedParse::Range {
                syntax: "log2_parallel_merge_level",
            })));
        }
        if pps.pps_extension_present {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::PpsExtension,
            )));
        }
        Ok(())
    }

    /// Slice order of one picture: treeblock addresses with first flags.
    fn check_slice_order(addrs: &[u32], firsts: &[bool]) -> Result<(), D3d12Error> {
        let Some(first_first) = firsts.first() else {
            return Ok(());
        };
        if !first_first {
            return Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::PictureWithoutFirstSlice,
            )));
        }
        let mut previous = 0u32;
        for (addr, first) in addrs.iter().zip(firsts.iter()).skip(1) {
            if *first || *addr <= previous {
                return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                    SharedUnsupported::ArbitrarySliceOrder,
                )));
            }
            previous = *addr;
        }
        Ok(())
    }

    /// NAL unit action for `nal_type` on layer `layer_id`.
    fn classify_nal(nal_type: u8, layer_id: u16) -> Result<NalAction, D3d12Error> {
        if layer_id != 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::Multilayer,
            )));
        }
        match nal_type {
            0..=9 | 16..=21 => Ok(NalAction::Vcl),
            10..=15 => Err(D3d12Error::Unsupported(
                UnsupportedReason::ReservedSliceType,
            )),
            33 => Ok(NalAction::Sps),
            34 => Ok(NalAction::Pps),
            35..=37 => Ok(NalAction::Complete),
            _ => Ok(NalAction::Ignore),
        }
    }
}

/// Tile grid counts against the tier grid.
fn check_tile_grid(columns: u8, rows: u8) -> Result<(), D3d12Error> {
    let columns_minus1 = columns.saturating_sub(1);
    let rows_minus1 = rows.saturating_sub(1);
    if columns_minus1 > 31 {
        return Err(D3d12Error::Parse(ParseReason::Parse(SharedParse::Range {
            syntax: "num_tile_columns_minus1",
        })));
    }
    if rows_minus1 > 31 {
        return Err(D3d12Error::Parse(ParseReason::Parse(SharedParse::Range {
            syntax: "num_tile_rows_minus1",
        })));
    }
    if columns_minus1 >= 19 || rows_minus1 >= 21 {
        return Err(D3d12Error::Unsupported(UnsupportedReason::TileGridPending));
    }
    Ok(())
}

/// Sequence parameter set from RBSP `rbsp`.
#[cfg(test)]
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, D3d12Error> {
    rust_video_h265::parse_sps::<D3d12Error, Gate>(rbsp)
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
#[cfg(test)]
pub(crate) fn parse_pps(rbsp: &[u8], sps: &Sps) -> Result<Pps, D3d12Error> {
    rust_video_h265::parse_pps::<D3d12Error, Gate>(rbsp, Some(sps))
}

#[cfg(test)]
mod gate_tests {
    use rust_video_h265::{HevcGate, Pps as SharedPps, Sps as SharedSps};

    use super::{ConstraintFlags, Gate, ProfileTierLevel, ScalingLists, SourceFlags};
    use crate::error::{D3d12Error, ParseReason, UnsupportedReason};
    use rust_video_caps::reason::{
        ParseReason as SharedParse, UnsupportedReason as SharedUnsupported,
    };

    /// Shared sequence set with Main profile and one sublayer.
    fn sps() -> SharedSps {
        SharedSps {
            id: 0,
            flags: 0,
            max_sub_layers: 1,
            level_idc: 120,
            width: 64,
            height: 48,
            crop: None,
            log2_max_poc_lsb_minus4: 0,
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: 3,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 0,
            max_transform_depth_inter: 1,
            max_transform_depth_intra: 1,
            scaling: ScalingLists::default(),
            pcm_depth_luma: 0,
            pcm_depth_chroma: 0,
            pcm_log2_min_minus3: 0,
            pcm_log2_diff: 0,
            rps: Vec::new(),
            lt_poc_lsb: Vec::new(),
            lt_used_by_curr: Vec::new(),
            vps_id: 0,
            ptl: ProfileTierLevel {
                tier: false,
                profile_idc: 1,
                compat: 0x4000_0000,
                source: SourceFlags {
                    progressive: true,
                    interlaced: false,
                },
                constraints: ConstraintFlags {
                    non_packed: true,
                    frame_only: true,
                },
                level_idc: 120,
                sub_layers: Vec::new(),
            },
            chroma_format: 1,
            separate_colour_plane: false,
            bit_depth_luma: 0,
            bit_depth_chroma: 0,
            ordering_buffering: Vec::new(),
            ordering_reorder: Vec::new(),
            ordering_latency: Vec::new(),
            vui: None,
            sps_extension_present: false,
            sps_extension_flags: [false; 4],
        }
    }

    #[test]
    fn gate_accepts_test_sets() {
        let sps = sps();
        assert!(Gate::check_sps(&sps).is_ok());
    }

    #[test]
    fn gate_refuses_pending_constructs() {
        let mut set = sps();
        set.max_sub_layers = 2;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::Sublayers
            )))
        ));
        let mut set = sps();
        set.flags |= rust_video_h265::SPS_SCALING;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(
                UnsupportedReason::ScalingListsPending
            ))
        ));
        let mut set = sps();
        set.flags |= rust_video_h265::SPS_PCM;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(UnsupportedReason::PcmPending))
        ));
        let mut set = sps();
        set.flags |= rust_video_h265::SPS_LONG_TERM;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(
                UnsupportedReason::LongTermRefsPending
            ))
        ));
        let mut set = sps();
        set.ptl.profile_idc = 2;
        set.ptl.compat = 0;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::ProfileOutsideMain
            )))
        ));
        let mut set = sps();
        set.ptl.level_idc = 121;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::Range { .. }
            )))
        ));
        let mut set = sps();
        set.sps_extension_present = true;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::SpsExtension
            )))
        ));
    }

    #[test]
    fn gate_refuses_picture_pending_constructs() {
        let sps = sps();
        let mut pps = SharedPps {
            id: 0,
            sps_id: 0,
            flags: 0,
            num_extra_slice_header_bits: 0,
            num_ref_l0: 0,
            num_ref_l1: 0,
            pic_init_qp_minus26: 0,
            diff_cu_qp_delta_depth: 0,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            num_tile_columns: 1,
            num_tile_rows: 1,
            column_width: Vec::new(),
            row_height: Vec::new(),
            beta_offset_div2: 0,
            tc_offset_div2: 0,
            scaling: rust_video_h265::ScalingLists::default(),
            log2_parallel_merge_minus2: 0,
            deblock_control_present: false,
            pps_extension_present: false,
            pps_extension_flags: [false; 4],
        };
        assert!(Gate::check_pps(&pps, Some(&sps)).is_ok());
        pps.sps_id = 1;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::PpsCitesUnknownSps
            )))
        ));
        pps.sps_id = 0;
        pps.diff_cu_qp_delta_depth = 32;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::Range { .. }
            )))
        ));
        pps.diff_cu_qp_delta_depth = 0;
        pps.num_tile_columns = 20;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Unsupported(UnsupportedReason::TileGridPending))
        ));
        pps.num_tile_columns = 1;
        pps.log2_parallel_merge_minus2 = 5;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::Range { .. }
            )))
        ));
        pps.log2_parallel_merge_minus2 = 0;
        pps.pps_extension_present = true;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::PpsExtension
            )))
        ));
    }

    #[test]
    fn gate_orders_slices_behind_first() {
        assert!(Gate::check_slice_order(&[0, 4], &[true, false]).is_ok());
        assert!(matches!(
            Gate::check_slice_order(&[4], &[false]),
            Err(D3d12Error::Parse(ParseReason::Parse(
                SharedParse::PictureWithoutFirstSlice
            )))
        ));
        assert!(matches!(
            Gate::check_slice_order(&[0, 0], &[true, false]),
            Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::ArbitrarySliceOrder
            )))
        ));
    }

    #[test]
    fn gate_classifies_reserved_slices() {
        assert!(matches!(
            Gate::classify_nal(1, 0),
            Ok(rust_video_h265::NalAction::Vcl)
        ));
        assert!(matches!(
            Gate::classify_nal(12, 0),
            Err(D3d12Error::Unsupported(
                UnsupportedReason::ReservedSliceType
            ))
        ));
        assert!(matches!(
            Gate::classify_nal(1, 1),
            Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::Multilayer
            )))
        ));
    }
}
