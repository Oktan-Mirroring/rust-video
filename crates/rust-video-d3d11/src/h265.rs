//! H.265 Annex B ingest over the shared grammar.

use rust_video_h265::{HevcError, HevcGate, ParseKind, UnsupportedKind};

use crate::error::{D3d11Error, ParseReason, UnsupportedReason};

pub(crate) use rust_video_h265::{LtSpec, Picture, Pps, ShortTermRps, Sps};

#[cfg(test)]
pub(crate) use rust_video_h265::{
    ConstraintFlags, PendingSlice, ProfileTierLevel, RpsBits, ScalingLists, SliceHeader, SliceType,
    SourceFlags, SPS_PCM, SPS_SAO, SPS_SCALING, SPS_TEMPORAL_MVP,
};

/// Tile column ceiling behind the DXVA grid.
pub(crate) const MAX_TILE_COLUMNS: usize = 20;

/// Tile row ceiling behind the DXVA grid.
pub(crate) const MAX_TILE_ROWS: usize = 22;

/// Coded extent ceiling in luma samples, H.265 A.4.1.
pub(crate) const MAX_PIC_SIZE: u32 = 16384;

/// Annex B ingest over the tier gate.
pub(crate) type Ingest = rust_video_h265::Ingest<D3d11Error, Gate>;

/// Tier capability gates over shared grammar output.
pub(crate) struct Gate;

impl HevcError for D3d11Error {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::EmptyCodedSize => Self::Parse(ParseReason::Range { syntax: "pic size" }),
            ParseKind::ConformanceOutside => {
                Self::Parse(ParseReason::ConformanceWindowOutsideCodedSize)
            }
            ParseKind::RpsTableOversize => Self::Parse(ParseReason::Range {
                syntax: "num_short_term_ref_pic_sets",
            }),
            ParseKind::LtSpsOversize => Self::Parse(ParseReason::Range {
                syntax: "num_long_term_ref_pics_sps",
            }),
            ParseKind::LtTotalOversize => Self::Parse(ParseReason::Range {
                syntax: "num_long_term_pics",
            }),
            ParseKind::EmptySpsRps => Self::Parse(ParseReason::Range {
                syntax: "short_term_ref_pic_set_idx",
            }),
            ParseKind::Range { syntax } => Self::Parse(ParseReason::Range {
                syntax: remap_syntax(syntax),
            }),
            ParseKind::Truncated { syntax } => Self::Parse(ParseReason::Truncated { syntax }),
            ParseKind::OverflowU32 { syntax } => Self::Parse(ParseReason::OverflowU32 { syntax }),
            ParseKind::OverflowI32 { syntax } => Self::Parse(ParseReason::OverflowI32 { syntax }),
            ParseKind::TooWide { syntax } => Self::Parse(ParseReason::TooWide { syntax }),
            ParseKind::NalForbiddenBit => Self::Parse(ParseReason::NalForbiddenBit),
            ParseKind::NalTemporalId => Self::Parse(ParseReason::NalTemporalId),
            ParseKind::VpsWithoutBody => Self::Parse(ParseReason::VpsWithoutBody),
            ParseKind::SpsWithoutBody => Self::Parse(ParseReason::SpsWithoutBody),
            ParseKind::PpsWithoutBody => Self::Parse(ParseReason::PpsWithoutBody),
            ParseKind::SliceWithoutHeader => Self::Parse(ParseReason::SliceWithoutHeader),
            ParseKind::PpsCitesUnknownSps => Self::Parse(ParseReason::PpsCitesUnknownSps),
            ParseKind::SliceCitesUnknownPps => Self::Parse(ParseReason::SliceCitesUnknownPps),
            ParseKind::SliceCitesUnknownSps => Self::Parse(ParseReason::SliceCitesUnknownSps),
            ParseKind::PictureSlicesDisagree => Self::Parse(ParseReason::PictureSlicesDisagree),
            ParseKind::NonIntraSliceInIdr => Self::Parse(ParseReason::NonIntraSliceInIdr),
            ParseKind::ShortTermRefPicSetSize => Self::Parse(ParseReason::ShortTermRefPicSetSize),
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::DependentSlices => {
                Self::Unsupported(UnsupportedReason::DependentSliceSegments)
            }
            UnsupportedKind::ArbitrarySliceOrder => {
                Self::Unsupported(UnsupportedReason::ArbitrarySliceOrder)
            }
            UnsupportedKind::Multilayer => Self::Unsupported(UnsupportedReason::Multilayer),
        }
    }
}

/// Shared syntax label behind the tier label.
fn remap_syntax(syntax: &'static str) -> &'static str {
    match syntax {
        "pps_seq_parameter_set_id" => "seq_parameter_set_id",
        _ => syntax,
    }
}

impl HevcGate<D3d11Error> for Gate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), D3d11Error> {
        if sps.chroma_format != 1 {
            return Err(D3d11Error::Unsupported(UnsupportedReason::ChromaOutside420));
        }
        if sps.bit_depth_luma != 0 {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::LumaDepthAbove8Bit,
            ));
        }
        if sps.bit_depth_chroma != 0 {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::ChromaDepthAbove8Bit,
            ));
        }
        if sps.width > MAX_PIC_SIZE || sps.height > MAX_PIC_SIZE {
            return Err(D3d11Error::Parse(ParseReason::Range { syntax: "pic size" }));
        }
        if sps.pcm() && (sps.pcm_depth_luma > 7 || sps.pcm_depth_chroma > 7) {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::PcmDepthAbove8Bit,
            ));
        }
        if sps.ptl.source.interlaced && !sps.ptl.source.progressive {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Interlaced));
        }
        if sps
            .ptl
            .sub_layers
            .iter()
            .any(|layer| layer.source.interlaced && !layer.source.progressive)
        {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Interlaced));
        }
        if sps.vui.as_ref().is_some_and(|vui| vui.field_seq) {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Interlaced));
        }
        if sps.sps_extension_flags.iter().any(|flag| *flag) {
            return Err(D3d11Error::Unsupported(UnsupportedReason::SpsExtension));
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), D3d11Error> {
        let Some(sps) = sps else {
            return Err(D3d11Error::Parse(ParseReason::PpsCitesUnknownSps));
        };
        if pps.sps_id != sps.id {
            return Err(D3d11Error::Parse(ParseReason::PpsCitesUnknownSps));
        }
        if pps.num_ref_l0 > 14 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "num_ref_idx_l0_default_active_minus1",
            }));
        }
        if pps.num_ref_l1 > 14 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "num_ref_idx_l1_default_active_minus1",
            }));
        }
        if usize::from(pps.num_tile_columns) > MAX_TILE_COLUMNS
            || usize::from(pps.num_tile_rows) > MAX_TILE_ROWS
        {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::TileGridAboveDriverGrid,
            ));
        }
        if pps.log2_parallel_merge_minus2 > 4 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "log2_parallel_merge_level_minus2",
            }));
        }
        if pps.pps_extension_flags.iter().any(|flag| *flag) {
            return Err(D3d11Error::Unsupported(UnsupportedReason::PpsExtension));
        }
        Ok(())
    }

    /// Sequence citation of video set `vps_id`, seen when `seen`.
    fn check_sps_citation(vps_id: u8, seen: bool) -> Result<(), D3d11Error> {
        let _ = vps_id;
        if seen {
            Ok(())
        } else {
            Err(D3d11Error::Parse(ParseReason::SpsCitesUnknownVps))
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_video_h265::HevcGate;

    use super::{
        ConstraintFlags, Gate, Pps, ProfileTierLevel, ScalingLists, SourceFlags, Sps, MAX_PIC_SIZE,
        SPS_PCM,
    };
    use crate::error::{D3d11Error, ParseReason, UnsupportedReason};

    /// Sequence set with 4:2:0 8-bit 320x240.
    fn sps() -> Sps {
        Sps {
            id: 0,
            flags: 0,
            max_sub_layers: 1,
            level_idc: 120,
            width: 320,
            height: 240,
            crop: None,
            log2_max_poc_lsb_minus4: 0,
            max_dec_pic_buffering_minus1: 4,
            max_num_reorder_pics: 2,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: 3,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 3,
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
            ordering_buffering: vec![4],
            ordering_reorder: vec![2],
            ordering_latency: vec![0],
            vui: None,
            sps_extension_present: false,
            sps_extension_flags: [false; 4],
        }
    }

    /// Picture set over sequence set zero.
    fn pps() -> Pps {
        Pps {
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
            scaling: ScalingLists::default(),
            log2_parallel_merge_minus2: 0,
            deblock_control_present: false,
            pps_extension_present: false,
            pps_extension_flags: [false; 4],
        }
    }

    #[test]
    fn gate_accepts_test_sets() {
        let sps = sps();
        assert!(Gate::check_sps(&sps).is_ok());
        assert!(Gate::check_pps(&pps(), Some(&sps)).is_ok());
        assert!(Gate::check_sps_citation(0, true).is_ok());
    }

    #[test]
    fn gate_refuses_chroma_depth_and_extent() {
        let mut set = sps();
        set.chroma_format = 2;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Unsupported(UnsupportedReason::ChromaOutside420))
        ));
        let mut set = sps();
        set.bit_depth_luma = 2;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Unsupported(
                UnsupportedReason::LumaDepthAbove8Bit
            ))
        ));
        let mut set = sps();
        set.bit_depth_chroma = 2;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Unsupported(
                UnsupportedReason::ChromaDepthAbove8Bit
            ))
        ));
        let mut set = sps();
        set.height = MAX_PIC_SIZE + 1;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Parse(ParseReason::Range { .. }))
        ));
    }

    #[test]
    fn gate_refuses_interlace_pcm_and_extension() {
        let mut set = sps();
        set.ptl.source.interlaced = true;
        set.ptl.source.progressive = false;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Unsupported(UnsupportedReason::Interlaced))
        ));
        let mut set = sps();
        set.flags |= SPS_PCM;
        set.pcm_depth_luma = 8;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Unsupported(
                UnsupportedReason::PcmDepthAbove8Bit
            ))
        ));
        let mut set = sps();
        set.sps_extension_present = true;
        set.sps_extension_flags[0] = true;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d11Error::Unsupported(UnsupportedReason::SpsExtension))
        ));
    }

    #[test]
    fn gate_refuses_grid_counts_and_extensions() {
        let set = sps();
        let mut pic = pps();
        pic.sps_id = 1;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&set)),
            Err(D3d11Error::Parse(ParseReason::PpsCitesUnknownSps))
        ));
        assert!(matches!(
            Gate::check_pps(&pps(), None),
            Err(D3d11Error::Parse(ParseReason::PpsCitesUnknownSps))
        ));
        let mut pic = pps();
        pic.num_ref_l0 = 15;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&set)),
            Err(D3d11Error::Parse(ParseReason::Range { .. }))
        ));
        let mut pic = pps();
        pic.num_tile_columns = 21;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&set)),
            Err(D3d11Error::Unsupported(
                UnsupportedReason::TileGridAboveDriverGrid
            ))
        ));
        let mut pic = pps();
        pic.log2_parallel_merge_minus2 = 5;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&set)),
            Err(D3d11Error::Parse(ParseReason::Range { .. }))
        ));
        let mut pic = pps();
        pic.pps_extension_flags[1] = true;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&set)),
            Err(D3d11Error::Unsupported(UnsupportedReason::PpsExtension))
        ));
        assert!(matches!(
            Gate::check_sps_citation(0, false),
            Err(D3d11Error::Parse(ParseReason::SpsCitesUnknownVps))
        ));
    }
}
