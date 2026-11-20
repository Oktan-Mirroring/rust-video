// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! H.265 Annex B ingest over the shared grammar.

#[cfg(target_os = "linux")]
pub(crate) mod decoder;
#[allow(dead_code)]
pub(crate) mod dpb;
#[allow(dead_code)]
pub(crate) mod frontend;
#[allow(dead_code)]
pub(crate) mod params;
#[allow(dead_code)]
pub(crate) mod picture;

#[cfg(target_os = "linux")]
pub use crate::h265::decoder::H265Decoder;

pub(crate) use rust_video_h265::{
    anchors_tid0, is_bla, is_cra, is_idr, is_irap, is_rasl, nal_unit_type, parse_pps,
    parse_pps_ids, parse_slice_prefix, parse_slice_rest, parse_slice_tail, parse_sps, parse_vps,
    rbsp, split_nals, temporal_id, BitReader, LtSpec, Pps, ShortTermRps, SliceHeader, SliceTail,
    SliceType, Sps, WeightEntry,
};

/// Shared weight table behind submit tests.
#[cfg(test)]
pub(crate) use rust_video_h265::SliceWeights;

use rust_video_h265::{HevcError, HevcGate, ParseKind, UnsupportedKind};

use rust_video_caps::reason::{ParseReason as CapsParse, UnsupportedReason as CapsUnsupported};

use crate::error::VaApiError;

/// Tier capability gates over shared grammar output.
#[derive(Default)]
pub(crate) struct Gate;

impl HevcError for VaApiError {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Parse(CapsParse::Truncated { syntax }),
            ParseKind::VpsWithoutBody => Self::Parse(CapsParse::VpsWithoutBody),
            ParseKind::SpsWithoutBody => Self::Parse(CapsParse::SpsWithoutBody),
            ParseKind::PpsWithoutBody => Self::Parse(CapsParse::PpsWithoutBody),
            ParseKind::SliceWithoutHeader => Self::Parse(CapsParse::SliceWithoutHeader),
            ParseKind::OverflowU32 { syntax } => Self::Parse(CapsParse::OverflowU32 { syntax }),
            ParseKind::OverflowI32 { syntax } => Self::Parse(CapsParse::OverflowI32 { syntax }),
            ParseKind::TooWide { syntax } => Self::Parse(CapsParse::TooWide { syntax }),
            ParseKind::Range { syntax } => Self::Parse(CapsParse::Range { syntax }),
            ParseKind::NalForbiddenBit => Self::Parse(CapsParse::NalForbiddenBit),
            ParseKind::NalTemporalId => Self::Parse(CapsParse::NalTemporalId),
            ParseKind::PpsCitesUnknownSps => Self::Parse(CapsParse::PpsCitesUnknownSps),
            ParseKind::SliceCitesUnknownPps => Self::Parse(CapsParse::SliceCitesUnknownPps),
            ParseKind::SliceCitesUnknownSps => Self::Parse(CapsParse::SliceCitesUnknownSps),
            ParseKind::PictureSlicesDisagree => Self::Parse(CapsParse::PictureSlicesDisagree),
            ParseKind::NonIntraSliceInIdr => Self::Parse(CapsParse::NonIntraSliceInIdr),
            ParseKind::ShortTermRefPicSetSize => Self::Parse(CapsParse::ShortTermRefPicSetSize),
            ParseKind::EmptyCodedSize => Self::Parse(CapsParse::EmptyCodedSize),
            ParseKind::ConformanceOutside => Self::Parse(CapsParse::ConfWinOutsideCodedSize),
            ParseKind::RpsTableOversize => Self::Parse(CapsParse::RpsTableOversize),
            ParseKind::LtSpsOversize => Self::Parse(CapsParse::LtSpsOversize),
            ParseKind::LtTotalOversize => Self::Parse(CapsParse::LtTotalOversize),
            ParseKind::EmptySpsRps => Self::Parse(CapsParse::EmptySpsRps),
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            // Well-formed but declined: the tier holds no address or
            // tail for dependent slices.
            UnsupportedKind::DependentSlices => Self::Unsupported(CapsUnsupported::DependentSlices),
            UnsupportedKind::ArbitrarySliceOrder => {
                Self::Unsupported(CapsUnsupported::ArbitrarySliceOrder)
            }
            UnsupportedKind::Multilayer => Self::Unsupported(CapsUnsupported::Multilayer),
        }
    }
}

impl HevcGate<VaApiError> for Gate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), VaApiError> {
        if sps.chroma_format != 1 {
            return Err(VaApiError::Unsupported(CapsUnsupported::ChromaOutside420));
        }
        if sps.separate_colour_plane {
            return Err(VaApiError::Unsupported(CapsUnsupported::ChromaOutside420));
        }
        if sps.bit_depth_luma > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "bit_depth_luma",
            }));
        }
        if sps.bit_depth_chroma > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "bit_depth_chroma",
            }));
        }
        if sps.bit_depth_luma != 0 {
            return Err(VaApiError::Unsupported(CapsUnsupported::LumaDepthAbove8Bit));
        }
        if sps.bit_depth_chroma != 0 {
            return Err(VaApiError::Unsupported(
                CapsUnsupported::ChromaDepthAbove8Bit,
            ));
        }
        Self::check_level(sps.level_idc)?;
        for layer in &sps.ptl.sub_layers {
            if let Some(level) = layer.level_idc {
                Self::check_level(level)?;
            }
        }
        if sps.max_sub_layers == 0 || sps.max_sub_layers > 7 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "max_sub_layers",
            }));
        }
        if sps.id > 15 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "sps_id" }));
        }
        Self::check_sps_dimensions(sps)?;
        Self::check_sps_ordering(sps)?;
        Self::check_sps_blocks(sps)?;
        if sps.rps.len() > 64 {
            return Err(VaApiError::Parse(CapsParse::RpsTableOversize));
        }
        if sps.lt_poc_lsb.len() > 32 {
            return Err(VaApiError::Parse(CapsParse::LtSpsOversize));
        }
        if sps.sps_extension_flags[1] {
            return Err(VaApiError::Unsupported(CapsUnsupported::Multilayer));
        }
        if sps.sps_extension_flags[2] {
            return Err(VaApiError::Unsupported(CapsUnsupported::Multiview));
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), VaApiError> {
        if pps.id > 63 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "pps_id" }));
        }
        if pps.sps_id > 15 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "sps_id" }));
        }
        Self::check_pps_qp(pps, sps)?;
        Self::check_pps_tiles(pps, sps)?;
        if pps.beta_offset_div2 < -6 || pps.beta_offset_div2 > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "beta_offset_div2",
            }));
        }
        if pps.tc_offset_div2 < -6 || pps.tc_offset_div2 > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "tc_offset_div2",
            }));
        }
        if pps.pps_extension_flags[1] {
            return Err(VaApiError::Unsupported(CapsUnsupported::Multilayer));
        }
        if pps.pps_extension_flags[2] {
            return Err(VaApiError::Unsupported(CapsUnsupported::Multiview));
        }
        Ok(())
    }
}

impl Gate {
    /// Level indicator against Table A.8.
    fn check_level(level_idc: u8) -> Result<(), VaApiError> {
        if !matches!(
            level_idc,
            30 | 60 | 63 | 90 | 93 | 120 | 123 | 150 | 153 | 156 | 180 | 183 | 186
        ) {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "level_idc",
            }));
        }
        Ok(())
    }

    /// Coded size and depth bounds, 7.4.3.2.1.
    fn check_sps_dimensions(sps: &Sps) -> Result<(), VaApiError> {
        if sps.width == 0 || sps.width > 16888 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "width" }));
        }
        if sps.height == 0 || sps.height > 16888 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "height" }));
        }
        if sps.log2_max_poc_lsb_minus4 > 12 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "log2_max_poc_lsb",
            }));
        }
        Ok(())
    }

    /// Sublayer ordering bounds over the coded entries.
    fn check_sps_ordering(sps: &Sps) -> Result<(), VaApiError> {
        for (index, buffering) in sps.ordering_buffering.iter().enumerate() {
            if *buffering > 16 {
                return Err(VaApiError::Parse(CapsParse::Range {
                    syntax: "ordering_buffering",
                }));
            }
            let reorder = sps.ordering_reorder.get(index).copied().unwrap_or(0);
            if reorder > *buffering {
                return Err(VaApiError::Parse(CapsParse::Range {
                    syntax: "ordering_reorder",
                }));
            }
            let latency = sps.ordering_latency.get(index).copied().unwrap_or(0);
            if latency == u32::MAX || latency > 255 {
                return Err(VaApiError::Parse(CapsParse::Range {
                    syntax: "ordering_latency",
                }));
            }
        }
        Ok(())
    }

    /// Block geometry bounds, 7.4.3.2.1.
    fn check_sps_blocks(sps: &Sps) -> Result<(), VaApiError> {
        if sps.log2_min_cb_size_minus3 > 3 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "log2_min_cb_size",
            }));
        }
        if sps.log2_diff_max_min_cb_size > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "log2_diff_cb_size",
            }));
        }
        if sps.log2_min_tb_size_minus2 > 3 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "log2_min_tb_size",
            }));
        }
        if sps.log2_diff_max_min_tb_size > 3 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "log2_diff_tb_size",
            }));
        }
        if sps.max_transform_depth_inter > 4 || sps.max_transform_depth_intra > 4 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "max_transform_depth",
            }));
        }
        if sps.pcm_log2_min_minus3 > 2 || sps.pcm_log2_diff > 2 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "pcm_log2" }));
        }
        Ok(())
    }

    /// Picture QP bounds against the sequence set.
    fn check_pps_qp(pps: &Pps, sps: Option<&Sps>) -> Result<(), VaApiError> {
        if pps.num_ref_l0 > 14 || pps.num_ref_l1 > 14 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "num_ref" }));
        }
        let Some(sps) = sps else {
            return Ok(());
        };
        let qp_min = -i32::from(26 + 6 * sps.bit_depth_luma);
        if i32::from(pps.pic_init_qp_minus26) < qp_min || pps.pic_init_qp_minus26 > 25 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "pic_init_qp",
            }));
        }
        if pps.cb_qp_offset < -12 || pps.cb_qp_offset > 12 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "cb_qp_offset",
            }));
        }
        if pps.cr_qp_offset < -12 || pps.cr_qp_offset > 12 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "cr_qp_offset",
            }));
        }
        if pps.diff_cu_qp_delta_depth > sps.log2_diff_max_min_cb_size {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "diff_cu_qp_delta_depth",
            }));
        }
        let ctb_log2 = u32::from(sps.log2_min_cb_size_minus3 + 3 + sps.log2_diff_max_min_cb_size);
        if u32::from(pps.log2_parallel_merge_minus2) > ctb_log2.saturating_sub(2) {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "log2_parallel_merge",
            }));
        }
        Ok(())
    }

    /// Tile counts inside the treeblock grid.
    fn check_pps_tiles(pps: &Pps, sps: Option<&Sps>) -> Result<(), VaApiError> {
        if pps.num_tile_columns > 19 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "num_tile_columns",
            }));
        }
        if pps.num_tile_rows > 21 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "num_tile_rows",
            }));
        }
        let Some(sps) = sps else {
            return Ok(());
        };
        let width_ctbs = sps.width_in_ctbs();
        let height_ctbs = sps.height_in_ctbs();
        if u32::from(pps.num_tile_columns) > width_ctbs {
            return Err(VaApiError::Parse(CapsParse::TileGridCoverage));
        }
        if u32::from(pps.num_tile_rows) > height_ctbs {
            return Err(VaApiError::Parse(CapsParse::TileGridCoverage));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rust_video_h265::{HevcError, HevcGate};

    use super::{
        parse_pps, parse_slice_prefix, parse_slice_rest, parse_slice_tail, parse_sps, rbsp,
        BitReader, Gate, ParseKind, UnsupportedKind,
    };
    use crate::error::VaApiError;
    use crate::fixtures::{idr_payload, pps_payload, sps_payload};

    #[test]
    fn shared_grammar_agrees_on_fixture_sets() {
        let seq_raw = sps_payload();
        let pic_raw = pps_payload();
        let seq_rbsp = rbsp(&seq_raw);
        let shared = parse_sps::<VaApiError, Gate>(&seq_rbsp).expect("sps");
        assert_eq!((shared.width, shared.height), (64, 64));
        assert_eq!(shared.chroma_format, 1);

        let pic_rbsp = rbsp(&pic_raw);
        let pic = parse_pps::<VaApiError, Gate>(&pic_rbsp, Some(&shared)).expect("pps");
        assert_eq!(pic.sps_id, 0);

        let raw_idr = idr_payload();
        let idr_rbsp = rbsp(&raw_idr);
        let mut reader = BitReader::new(&idr_rbsp);
        let prefix = parse_slice_prefix::<VaApiError>(&mut reader, 19).expect("prefix");
        let slice =
            parse_slice_rest::<VaApiError>(&mut reader, prefix, 19, &shared, &pic).expect("slice");
        assert!(slice.slice_type.is_intra());
        let tail = parse_slice_tail::<VaApiError>(&idr_rbsp, 19, &shared, &pic).expect("tail");
        assert!(tail.bit_size > 16);
    }

    #[test]
    fn gate_declines_sampling_and_bounds() {
        let payload = sps_payload();
        let rbsp = rbsp(&payload);
        let mut set = parse_sps::<VaApiError, Gate>(&rbsp).expect("sps");
        set.chroma_format = 0;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(VaApiError::Unsupported(_))
        ));
        let mut set = parse_sps::<VaApiError, Gate>(&rbsp).expect("sps");
        set.bit_depth_luma = 2;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(VaApiError::Unsupported(_))
        ));
        let mut set = parse_sps::<VaApiError, Gate>(&rbsp).expect("sps");
        set.width = 0;
        assert!(matches!(Gate::check_sps(&set), Err(VaApiError::Parse(_))));
        let mut set = parse_sps::<VaApiError, Gate>(&rbsp).expect("sps");
        set.level_idc = 15;
        assert_eq!(
            Gate::check_sps(&set).unwrap_err().to_string(),
            "parse failed: level_idc range"
        );
    }

    #[test]
    fn errors_map_onto_tier_errors() {
        assert_eq!(
            VaApiError::parse(ParseKind::Range {
                syntax: "slice_type"
            })
            .to_string(),
            "parse failed: slice_type range"
        );
        assert_eq!(
            VaApiError::parse(ParseKind::NalTemporalId).to_string(),
            "parse failed: nal temporal id"
        );
        assert_eq!(
            VaApiError::unsupported(UnsupportedKind::DependentSlices).to_string(),
            "unsupported stream: dependent slices"
        );
        assert_eq!(
            VaApiError::unsupported(UnsupportedKind::Multilayer).to_string(),
            "unsupported stream: multilayer"
        );
    }
}
