//! H.265 ingest over the shared grammar, mapped onto tier errors.
//!
//! Grammar in `rust-video-h265`; submit builders in `hevc_dpb`.

use rust_video_h265::{
    HevcError, HevcGate, NalAction, ParseKind, UnsupportedKind, MAX_PPS, MAX_SPS,
};

#[cfg(test)]
use rust_video_h265::{ConstraintFlags, ProfileTierLevel, ScalingLists, SourceFlags};

use crate::error::{D3d12Error, ParseReason, UnsupportedReason};
use crate::h264::Crop;

pub(crate) use rust_video_h265::{RpsBits, ShortTermRps, SliceType};

/// NAL units in one Annex B packet, start codes stripped.
#[cfg(test)]
pub(crate) fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = 0;
    let mut zeros = 0;
    let mut index = 0;
    while index < data.len() {
        if data[index] == 1 && zeros >= 2 {
            let end = index - zeros;
            if end > start {
                nals.push(&data[start..end]);
            }
            start = index + 1;
            zeros = 0;
        } else if data[index] == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        index += 1;
    }
    if start < data.len() {
        nals.push(&data[start..]);
    }
    nals
}

/// Parsed sequence parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sps {
    /// Set id.
    pub(crate) seq_parameter_set_id: u8,
    /// Coded width in luma samples.
    pub(crate) width: u32,
    /// Coded height in luma samples.
    pub(crate) height: u32,
    /// Visible rectangle, absent when unconformant.
    pub(crate) crop: Option<Crop>,
    /// POC LSB width minus 4.
    pub(crate) log2_max_poc_lsb_minus4: u8,
    /// Maximum buffering minus one.
    pub(crate) max_dec_pic_buffering_minus1: u8,
    /// Maximum reorder pictures.
    pub(crate) max_num_reorder_pics: u8,
    /// Short-term reference set table.
    pub(crate) rps: Vec<ShortTermRps>,
    /// Minimum luma block width minus 3.
    pub(crate) log2_min_cb_minus3: u8,
    /// Luma block size range.
    pub(crate) log2_diff_max_min_cb: u8,
    /// Minimum transform width minus 2.
    pub(crate) log2_min_tb_minus2: u8,
    /// Transform size range.
    pub(crate) log2_diff_max_min_tb: u8,
    /// Inter transform depth.
    pub(crate) max_depth_inter: u8,
    /// Intra transform depth.
    pub(crate) max_depth_intra: u8,
    /// Packed tool flags.
    pub(crate) flags: u8,
}

/// Asymmetric motion partitions in the sequence flags.
pub(crate) const FLAG_AMP: u8 = 1;
/// Sample adaptive offset in the sequence flags.
pub(crate) const FLAG_SAO: u8 = 1 << 1;
/// Temporal motion vectors in the sequence flags.
pub(crate) const FLAG_TEMPORAL_MVP: u8 = 1 << 2;
/// Strong intra smoothing in the sequence flags.
pub(crate) const FLAG_STRONG_INTRA: u8 = 1 << 3;

impl Sps {
    /// Coded width in luma samples.
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    /// Coded height in luma samples.
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    /// Visible width in luma samples.
    pub(crate) fn visible_width(&self) -> u32 {
        self.crop
            .map_or_else(|| self.width, |crop| self.width - crop.left - crop.right)
    }

    /// Visible height in luma samples.
    pub(crate) fn visible_height(&self) -> u32 {
        self.crop
            .map_or_else(|| self.height, |crop| self.height - crop.top - crop.bottom)
    }

    /// Minimum luma block edge in samples.
    pub(crate) fn min_cb(&self) -> u32 {
        1 << (self.log2_min_cb_minus3 + 3)
    }

    /// Maximum POC LSB.
    pub(crate) fn max_poc_lsb(&self) -> u32 {
        1 << (self.log2_max_poc_lsb_minus4 + 4)
    }

    /// True for asymmetric motion partitions.
    pub(crate) const fn amp(&self) -> bool {
        self.flags & FLAG_AMP != 0
    }

    /// True for sample adaptive offset.
    pub(crate) const fn sao(&self) -> bool {
        self.flags & FLAG_SAO != 0
    }

    /// True for temporal motion vector prediction.
    pub(crate) const fn temporal_mvp(&self) -> bool {
        self.flags & FLAG_TEMPORAL_MVP != 0
    }

    /// True for strong intra smoothing.
    pub(crate) const fn strong_intra_smoothing(&self) -> bool {
        self.flags & FLAG_STRONG_INTRA != 0
    }
}

/// Parsed picture parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pps {
    /// Set id.
    pub(crate) pic_parameter_set_id: u8,
    /// Sequence set id.
    pub(crate) seq_parameter_set_id: u8,
    /// Packed flags.
    pub(crate) flags: u32,
    /// Reserved slice header bits.
    pub(crate) num_extra_slice_header_bits: u8,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_l0: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_l1: u8,
    /// Luma QP init delta.
    pub(crate) init_qp_minus26: i8,
    /// CU QP delta depth.
    pub(crate) diff_cu_qp_delta_depth: u8,
    /// Cb QP offset.
    pub(crate) cb_qp_offset: i8,
    /// Cr QP offset.
    pub(crate) cr_qp_offset: i8,
    /// Tile columns minus one.
    pub(crate) tile_columns_minus1: u8,
    /// Tile rows minus one.
    pub(crate) tile_rows_minus1: u8,
    /// Tile column widths minus one.
    pub(crate) column_width_minus1: [u16; 19],
    /// Tile row heights minus one.
    pub(crate) row_height_minus1: [u16; 21],
    /// Beta deblocking offset.
    pub(crate) beta_offset_div2: i8,
    /// Tc deblocking offset.
    pub(crate) tc_offset_div2: i8,
    /// Parallel merge level minus 2.
    pub(crate) log2_parallel_merge_level_minus2: u8,
}

/// Dependent slices in the picture flags.
const P_DEPENDENT_SLICES: u32 = 1;
/// Output flag in the picture flags.
const P_OUTPUT_FLAG: u32 = 1 << 1;
/// Sign data hiding in the picture flags.
const P_SIGN_HIDING: u32 = 1 << 2;
/// CABAC init in the picture flags.
const P_CABAC_INIT: u32 = 1 << 3;
/// Constrained intra prediction in the picture flags.
const P_CONSTRAINED_INTRA: u32 = 1 << 4;
/// Transform skip in the picture flags.
const P_TRANSFORM_SKIP: u32 = 1 << 5;
/// CU QP deltas in the picture flags.
const P_CU_QP_DELTA: u32 = 1 << 6;
/// Slice chroma QP offsets in the picture flags.
const P_CHROMA_QP_OFFSETS: u32 = 1 << 7;
/// Weighted prediction in the picture flags.
const P_WEIGHTED_PRED: u32 = 1 << 8;
/// Weighted biprediction in the picture flags.
const P_WEIGHTED_BIPRED: u32 = 1 << 9;
/// Transform bypass in the picture flags.
const P_TRANSQUANT_BYPASS: u32 = 1 << 10;
/// Tiles in the picture flags.
const P_TILES: u32 = 1 << 11;
/// Uniform tile spacing in the picture flags.
const P_UNIFORM_SPACING: u32 = 1 << 12;
/// Entropy sync in the picture flags.
const P_ENTROPY_SYNC: u32 = 1 << 13;
/// Loop filter across tiles in the picture flags.
const P_LOOP_TILES: u32 = 1 << 14;
/// Loop filter across slices in the picture flags.
const P_LOOP_SLICES: u32 = 1 << 15;
/// Deblocking override in the picture flags.
const P_DEBLOCK_OVERRIDE: u32 = 1 << 16;
/// Disabled deblocking in the picture flags.
const P_DEBLOCK_DISABLED: u32 = 1 << 17;
/// Reference list modification in the picture flags.
const P_LISTS_MOD: u32 = 1 << 18;
/// Slice header extension in the picture flags.
const P_SLICE_EXT: u32 = 1 << 19;

impl Pps {
    /// True for dependent slice segments.
    pub(crate) const fn dependent_slices(&self) -> bool {
        self.flags & P_DEPENDENT_SLICES != 0
    }

    /// True for the output flag in slices.
    pub(crate) const fn output_flag_present(&self) -> bool {
        self.flags & P_OUTPUT_FLAG != 0
    }

    /// True for sign data hiding.
    pub(crate) const fn sign_data_hiding(&self) -> bool {
        self.flags & P_SIGN_HIDING != 0
    }

    /// True for CABAC init in slices.
    pub(crate) const fn cabac_init_present(&self) -> bool {
        self.flags & P_CABAC_INIT != 0
    }

    /// True for constrained intra prediction.
    pub(crate) const fn constrained_intra(&self) -> bool {
        self.flags & P_CONSTRAINED_INTRA != 0
    }

    /// True for transform skip.
    pub(crate) const fn transform_skip(&self) -> bool {
        self.flags & P_TRANSFORM_SKIP != 0
    }

    /// True for CU QP deltas.
    pub(crate) const fn cu_qp_delta(&self) -> bool {
        self.flags & P_CU_QP_DELTA != 0
    }

    /// True for slice chroma QP offsets.
    pub(crate) const fn slice_chroma_qp_offsets_present(&self) -> bool {
        self.flags & P_CHROMA_QP_OFFSETS != 0
    }

    /// True for weighted prediction.
    pub(crate) const fn weighted_pred(&self) -> bool {
        self.flags & P_WEIGHTED_PRED != 0
    }

    /// True for weighted biprediction.
    pub(crate) const fn weighted_bipred(&self) -> bool {
        self.flags & P_WEIGHTED_BIPRED != 0
    }

    /// True for transform bypass.
    pub(crate) const fn transquant_bypass(&self) -> bool {
        self.flags & P_TRANSQUANT_BYPASS != 0
    }

    /// True for tiles.
    pub(crate) const fn tiles(&self) -> bool {
        self.flags & P_TILES != 0
    }

    /// True for uniform tile spacing.
    pub(crate) const fn uniform_spacing(&self) -> bool {
        self.flags & P_UNIFORM_SPACING != 0
    }

    /// True for entropy sync.
    pub(crate) const fn entropy_sync(&self) -> bool {
        self.flags & P_ENTROPY_SYNC != 0
    }

    /// True for the loop filter across tiles.
    pub(crate) const fn loop_across_tiles(&self) -> bool {
        self.flags & P_LOOP_TILES != 0
    }

    /// True for the loop filter across slices.
    pub(crate) const fn loop_across_slices(&self) -> bool {
        self.flags & P_LOOP_SLICES != 0
    }

    /// True for deblocking override.
    pub(crate) const fn deblocking_override(&self) -> bool {
        self.flags & P_DEBLOCK_OVERRIDE != 0
    }

    /// True for disabled deblocking.
    pub(crate) const fn deblocking_disabled(&self) -> bool {
        self.flags & P_DEBLOCK_DISABLED != 0
    }

    /// True for reference list modification.
    pub(crate) const fn lists_modification_present(&self) -> bool {
        self.flags & P_LISTS_MOD != 0
    }

    /// True for slice header extension.
    pub(crate) const fn slice_ext_present(&self) -> bool {
        self.flags & P_SLICE_EXT != 0
    }
}

/// Parsed slice segment header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SliceHeader {
    /// Picture set id.
    pub(crate) pps_id: u8,
    /// Slice coding type.
    pub(crate) slice_type: SliceType,
    /// Address of the first coding tree block.
    pub(crate) address: u32,
    /// First slice of the picture.
    pub(crate) first: bool,
    /// Picture output flag.
    pub(crate) pic_output: bool,
    /// Prior pictures dropped without output.
    pub(crate) no_output_prior: bool,
    /// Picture order count LSB, absent on IDR.
    pub(crate) poc_lsb: Option<u32>,
    /// Resolved short-term set, absent on IDR.
    pub(crate) rps: Option<ShortTermRps>,
    /// Short-term set provenance, absent on IDR.
    pub(crate) rps_bits: Option<RpsBits>,
}

/// One coded slice: header plus its NAL bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSlice {
    /// Parsed slice header.
    pub(crate) header: SliceHeader,
    /// NAL bytes without the start code.
    pub(crate) nal: Vec<u8>,
    /// NAL unit type.
    pub(crate) nal_type: u8,
    /// Temporal id.
    pub(crate) temporal_id: u8,
}

/// One coded picture: slices sharing the set and NAL type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    /// Picture slices in bitstream order.
    pub(crate) slices: Vec<PendingSlice>,
    /// True for IDR pictures.
    pub(crate) idr: bool,
    /// True for random access pictures.
    pub(crate) irap: bool,
    /// True for RASL pictures.
    pub(crate) rasl: bool,
}

impl Picture {
    /// Picture set id.
    pub(crate) fn pps_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.header.pps_id)
    }

    /// Temporal id.
    pub(crate) fn temporal_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.temporal_id)
    }

    /// True when prior pictures drop without output.
    pub(crate) fn no_output_prior(&self) -> bool {
        self.slices
            .first()
            .is_some_and(|slice| slice.header.no_output_prior)
    }

    /// Picture order count LSB, absent on IDR.
    pub(crate) fn poc_lsb(&self) -> Option<u32> {
        self.slices.first()?.header.poc_lsb
    }
}

/// Tier capability gates over shared grammar output.
pub(crate) struct Gate;

/// Stream levels the tier runs, H.265 A.4.1.
const LEVELS: [u8; 14] = [
    30, 60, 63, 90, 93, 120, 123, 150, 153, 156, 180, 183, 186, 210,
];

impl HevcError for D3d12Error {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::EmptyCodedSize => Self::Parse(ParseReason::EmptyCodedSize),
            ParseKind::ConformanceOutside => Self::Parse(ParseReason::ConfWinOutsideCodedSize),
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
            ParseKind::VpsWithoutBody | ParseKind::SpsWithoutBody => {
                Self::Parse(ParseReason::SpsWithoutBody)
            }
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
                Self::Unsupported(UnsupportedReason::DependentSlicesPending)
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
    fn check_sps(sps: &rust_video_h265::Sps) -> Result<(), D3d12Error> {
        if sps.max_sub_layers != 1 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Sublayers));
        }
        if sps.chroma_format != 1 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::ChromaOutside420));
        }
        if sps.bit_depth_luma != 0 {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::LumaDepthAbove8Bit,
            ));
        }
        if sps.bit_depth_chroma != 0 {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ChromaDepthAbove8Bit,
            ));
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
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ProfileOutsideMain,
            ));
        }
        if sps.ptl.source.interlaced && !sps.ptl.source.progressive {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Interlaced));
        }
        if !LEVELS.contains(&sps.ptl.level_idc) {
            return Err(D3d12Error::Parse(ParseReason::Range {
                syntax: "general_level_idc",
            }));
        }
        if sps.sps_extension_present {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::SpsExtensionPending,
            ));
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(
        pps: &rust_video_h265::Pps,
        sps: Option<&rust_video_h265::Sps>,
    ) -> Result<(), D3d12Error> {
        let Some(sps) = sps else {
            return Err(D3d12Error::Parse(ParseReason::PpsCitesUnknownSps));
        };
        if pps.sps_id != sps.id {
            return Err(D3d12Error::Parse(ParseReason::PpsCitesUnknownSps));
        }
        if pps.flags & rust_video_h265::PPS_SCALING != 0 {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ScalingListsPending,
            ));
        }
        if pps.diff_cu_qp_delta_depth > 31 {
            return Err(D3d12Error::Parse(ParseReason::Range {
                syntax: "diff_cu_qp_delta_depth",
            }));
        }
        check_tile_grid(pps.num_tile_columns, pps.num_tile_rows)?;
        let ctb_log2 = sps
            .log2_min_cb_size_minus3
            .saturating_add(3)
            .saturating_add(sps.log2_diff_max_min_cb_size);
        if pps.log2_parallel_merge_minus2 > ctb_log2.saturating_sub(2) {
            return Err(D3d12Error::Parse(ParseReason::Range {
                syntax: "log2_parallel_merge_level",
            }));
        }
        if pps.pps_extension_present {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::PpsExtensionPending,
            ));
        }
        Ok(())
    }

    /// Slice order of one picture: treeblock addresses with first flags.
    fn check_slice_order(addrs: &[u32], firsts: &[bool]) -> Result<(), D3d12Error> {
        let Some(first_first) = firsts.first() else {
            return Ok(());
        };
        if !first_first {
            return Err(D3d12Error::Parse(ParseReason::PictureWithoutFirstSlice));
        }
        let mut previous = 0u32;
        for (addr, first) in addrs.iter().zip(firsts.iter()).skip(1) {
            if *first || *addr <= previous {
                return Err(D3d12Error::Unsupported(
                    UnsupportedReason::ArbitrarySliceOrder,
                ));
            }
            previous = *addr;
        }
        Ok(())
    }

    /// NAL unit action for `nal_type` on layer `layer_id`.
    fn classify_nal(nal_type: u8, layer_id: u16) -> Result<NalAction, D3d12Error> {
        if layer_id != 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Multilayer));
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
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "num_tile_columns_minus1",
        }));
    }
    if rows_minus1 > 31 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "num_tile_rows_minus1",
        }));
    }
    if columns_minus1 >= 19 || rows_minus1 >= 21 {
        return Err(D3d12Error::Unsupported(UnsupportedReason::TileGridPending));
    }
    Ok(())
}

/// Sequence parameter set from RBSP `rbsp`.
#[cfg(test)]
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, D3d12Error> {
    Ok(sps_from_shared(&rust_video_h265::parse_sps::<
        D3d12Error,
        Gate,
    >(rbsp)?))
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
#[cfg(test)]
pub(crate) fn parse_pps(rbsp: &[u8], sps: &Sps) -> Result<Pps, D3d12Error> {
    let shared_sps = approx_shared_sps(sps);
    let shared = rust_video_h265::parse_pps::<D3d12Error, Gate>(rbsp, Some(&shared_sps))?;
    pps_from_shared(&shared, &shared_sps)
}

/// Lean sequence set from the shared set.
fn sps_from_shared(shared: &rust_video_h265::Sps) -> Sps {
    let mut flags = 0;
    if shared.amp() {
        flags |= FLAG_AMP;
    }
    if shared.sao() {
        flags |= FLAG_SAO;
    }
    if shared.temporal_mvp() {
        flags |= FLAG_TEMPORAL_MVP;
    }
    if shared.smoothing() {
        flags |= FLAG_STRONG_INTRA;
    }
    Sps {
        seq_parameter_set_id: shared.id,
        width: shared.width,
        height: shared.height,
        crop: shared.crop.map(|crop| Crop {
            left: crop.left,
            right: crop.right,
            top: crop.top,
            bottom: crop.bottom,
        }),
        log2_max_poc_lsb_minus4: shared.log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1: shared.max_dec_pic_buffering_minus1,
        max_num_reorder_pics: shared.max_num_reorder_pics,
        rps: shared.rps.clone(),
        log2_min_cb_minus3: shared.log2_min_cb_size_minus3,
        log2_diff_max_min_cb: shared.log2_diff_max_min_cb_size,
        log2_min_tb_minus2: shared.log2_min_tb_size_minus2,
        log2_diff_max_min_tb: shared.log2_diff_max_min_tb_size,
        max_depth_inter: shared.max_transform_depth_inter,
        max_depth_intra: shared.max_transform_depth_intra,
        flags,
    }
}

/// Lean picture set from the shared set against its sequence set.
fn pps_from_shared(
    shared: &rust_video_h265::Pps,
    sps: &rust_video_h265::Sps,
) -> Result<Pps, D3d12Error> {
    let mut flags = 0;
    set_wide_flag(&mut flags, P_DEPENDENT_SLICES, shared.dependent_slices());
    set_wide_flag(&mut flags, P_OUTPUT_FLAG, shared.output_flag_present());
    set_wide_flag(&mut flags, P_SIGN_HIDING, shared.sign_hiding());
    set_wide_flag(&mut flags, P_CABAC_INIT, shared.cabac_init_present());
    set_wide_flag(&mut flags, P_CONSTRAINED_INTRA, shared.constrained_intra());
    set_wide_flag(&mut flags, P_TRANSFORM_SKIP, shared.transform_skip());
    set_wide_flag(&mut flags, P_CU_QP_DELTA, shared.cu_qp_delta());
    set_wide_flag(
        &mut flags,
        P_CHROMA_QP_OFFSETS,
        shared.chroma_offsets_present(),
    );
    set_wide_flag(&mut flags, P_WEIGHTED_PRED, shared.weighted_pred());
    set_wide_flag(&mut flags, P_WEIGHTED_BIPRED, shared.weighted_bipred());
    set_wide_flag(&mut flags, P_TRANSQUANT_BYPASS, shared.transquant_bypass());
    set_wide_flag(&mut flags, P_TILES, shared.tiles_enabled());
    set_wide_flag(&mut flags, P_UNIFORM_SPACING, shared.uniform_spacing());
    set_wide_flag(&mut flags, P_LOOP_TILES, shared.loop_across_tiles());
    set_wide_flag(&mut flags, P_ENTROPY_SYNC, shared.entropy_sync());
    set_wide_flag(&mut flags, P_LOOP_SLICES, shared.loop_across_slices());
    set_wide_flag(&mut flags, P_DEBLOCK_OVERRIDE, shared.deblocking_override());
    set_wide_flag(&mut flags, P_DEBLOCK_DISABLED, shared.deblocking_disabled());
    set_wide_flag(&mut flags, P_LISTS_MOD, shared.lists_modification_present());
    set_wide_flag(&mut flags, P_SLICE_EXT, shared.slice_ext_present());
    let (column_width_minus1, row_height_minus1) = tile_arrays(shared, sps)?;
    Ok(Pps {
        pic_parameter_set_id: shared.id,
        seq_parameter_set_id: shared.sps_id,
        flags,
        num_extra_slice_header_bits: shared.num_extra_slice_header_bits,
        num_ref_l0: shared.num_ref_l0,
        num_ref_l1: shared.num_ref_l1,
        init_qp_minus26: shared.pic_init_qp_minus26,
        diff_cu_qp_delta_depth: shared.diff_cu_qp_delta_depth,
        cb_qp_offset: shared.cb_qp_offset,
        cr_qp_offset: shared.cr_qp_offset,
        tile_columns_minus1: shared.num_tile_columns.saturating_sub(1),
        tile_rows_minus1: shared.num_tile_rows.saturating_sub(1),
        column_width_minus1,
        row_height_minus1,
        beta_offset_div2: shared.beta_offset_div2,
        tc_offset_div2: shared.tc_offset_div2,
        log2_parallel_merge_level_minus2: shared.log2_parallel_merge_minus2,
    })
}

/// Wide flag bit set in `flags` when `set`.
fn set_wide_flag(flags: &mut u32, bit: u32, set: bool) {
    if set {
        *flags |= bit;
    }
}

/// Lean tile arrays from the shared grid against its sequence set.
fn tile_arrays(
    shared: &rust_video_h265::Pps,
    sps: &rust_video_h265::Sps,
) -> Result<([u16; 19], [u16; 21]), D3d12Error> {
    if !shared.tiles_enabled() {
        return Ok(([0; 19], [0; 21]));
    }
    if shared.uniform_spacing() {
        return uniform_grid(
            sps.width,
            sps.height,
            sps.log2_min_cb_size_minus3,
            sps.log2_diff_max_min_cb_size,
            shared.num_tile_columns,
            shared.num_tile_rows,
        );
    }
    let mut columns = [0u16; 19];
    for (index, width) in shared.column_width.iter().enumerate() {
        if index + 1 >= shared.column_width.len() {
            break;
        }
        columns[index] = width
            .checked_sub(1)
            .ok_or(D3d12Error::Parse(ParseReason::Range {
                syntax: "column_width_minus1",
            }))?;
    }
    let mut rows = [0u16; 21];
    for (index, height) in shared.row_height.iter().enumerate() {
        if index + 1 >= shared.row_height.len() {
            break;
        }
        rows[index] = height
            .checked_sub(1)
            .ok_or(D3d12Error::Parse(ParseReason::Range {
                syntax: "row_height_minus1",
            }))?;
    }
    Ok((columns, rows))
}

/// Uniform tile widths and heights from the coding tree grid.
fn uniform_grid(
    width: u32,
    height: u32,
    log2_min_cb_minus3: u8,
    log2_diff: u8,
    columns: u8,
    rows: u8,
) -> Result<([u16; 19], [u16; 21]), D3d12Error> {
    let shift = log2_min_cb_minus3
        .saturating_add(3)
        .saturating_add(log2_diff);
    let ctb = 1u32
        .checked_shl(u32::from(shift))
        .unwrap_or(u32::MAX)
        .max(1);
    let ctbs_w = width.div_ceil(ctb).max(1);
    let ctbs_h = height.div_ceil(ctb).max(1);
    let columns = u32::from(columns);
    let rows = u32::from(rows);
    let mut column_width_minus1 = [0u16; 19];
    for (index, slot) in column_width_minus1.iter_mut().enumerate() {
        let i = u32::try_from(index).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "tile column",
            })
        })?;
        if i >= columns {
            break;
        }
        let width = (i + 1) * ctbs_w / columns - i * ctbs_w / columns;
        *slot = u16::try_from(width.saturating_sub(1)).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "tile column",
            })
        })?;
    }
    let mut row_height_minus1 = [0u16; 21];
    for (index, slot) in row_height_minus1.iter_mut().enumerate() {
        let i = u32::try_from(index)
            .map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax: "tile row" }))?;
        if i >= rows {
            break;
        }
        let height = (i + 1) * ctbs_h / rows - i * ctbs_h / rows;
        *slot = u16::try_from(height.saturating_sub(1))
            .map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax: "tile row" }))?;
    }
    Ok((column_width_minus1, row_height_minus1))
}

/// Lean slice header from the shared header with set id `pps_id`.
fn slice_from_shared(shared: &rust_video_h265::SliceHeader, pps_id: u8) -> SliceHeader {
    SliceHeader {
        pps_id,
        slice_type: shared.slice_type,
        address: shared.address,
        first: shared.first,
        pic_output: shared.pic_output,
        no_output_prior: shared.no_output_prior,
        poc_lsb: shared.poc_lsb,
        rps: shared.rps.clone(),
        rps_bits: shared.rps_bits,
    }
}

/// Lean picture from the shared picture.
fn picture_from_shared(shared: &rust_video_h265::Picture) -> Picture {
    Picture {
        slices: shared
            .slices
            .iter()
            .map(|slice| PendingSlice {
                header: slice_from_shared(&slice.header, slice.pps_id),
                nal: slice.nal.clone(),
                nal_type: slice.nal_type,
                temporal_id: slice.temporal_id,
            })
            .collect(),
        idr: shared.idr,
        irap: shared.irap,
        rasl: shared.rasl,
    }
}

/// Approximate shared sequence set behind picture set derivation.
///
/// Carries the exact grid behind tile derivation; Gates check the lean
/// values, so the remaining fields stay dummy.
#[cfg(test)]
fn approx_shared_sps(sps: &Sps) -> rust_video_h265::Sps {
    rust_video_h265::Sps {
        id: sps.seq_parameter_set_id,
        flags: 0,
        max_sub_layers: 1,
        level_idc: 120,
        width: sps.width,
        height: sps.height,
        crop: None,
        log2_max_poc_lsb_minus4: sps.log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1: sps.max_dec_pic_buffering_minus1,
        max_num_reorder_pics: sps.max_num_reorder_pics,
        log2_min_cb_size_minus3: sps.log2_min_cb_minus3,
        log2_diff_max_min_cb_size: sps.log2_diff_max_min_cb,
        log2_min_tb_size_minus2: sps.log2_min_tb_minus2,
        log2_diff_max_min_tb_size: sps.log2_diff_max_min_tb,
        max_transform_depth_inter: sps.max_depth_inter,
        max_transform_depth_intra: sps.max_depth_intra,
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

/// Annex B ingest over the shared grammar with lean mirrors.
pub(crate) struct Ingest {
    /// Shared ingest behind picture assembly.
    inner: rust_video_h265::Ingest<D3d12Error, Gate>,
    /// Stored sequence sets by id.
    sps: [Option<Sps>; MAX_SPS],
    /// Stored picture sets by id.
    pps: [Option<Pps>; MAX_PPS],
    /// Mirrored store versions.
    versions: (u64, u64),
}

impl Ingest {
    /// Empty ingest.
    pub(crate) fn new() -> Self {
        Self {
            inner: rust_video_h265::Ingest::new(),
            sps: core::array::from_fn(|_| None),
            pps: core::array::from_fn(|_| None),
            versions: (0, 0),
        }
    }

    /// Sequence set `id`, if stored.
    pub(crate) fn sps(&self, id: u8) -> Option<&Sps> {
        self.sps.get(usize::from(id))?.as_ref()
    }

    /// Picture set `id`, if stored.
    pub(crate) fn pps(&self, id: u8) -> Option<&Pps> {
        self.pps.get(usize::from(id))?.as_ref()
    }

    /// Pictures completed by `data`; trailing slices complete the picture.
    pub(crate) fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Picture>, D3d12Error> {
        let pictures = self.inner.ingest_packet(data)?;
        self.sync_mirrors()?;
        Ok(pictures.iter().map(picture_from_shared).collect())
    }

    /// Open picture, if any slices arrived without a boundary.
    pub(crate) fn finish(&mut self) -> Result<Option<Picture>, D3d12Error> {
        let picture = self.inner.finish()?;
        self.sync_mirrors()?;
        Ok(picture.as_ref().map(picture_from_shared))
    }

    /// Lean mirrors behind the shared stores.
    fn sync_mirrors(&mut self) -> Result<(), D3d12Error> {
        let versions = self.inner.versions();
        if versions == self.versions {
            return Ok(());
        }
        self.sync_sets()?;
        self.versions = versions;
        Ok(())
    }

    /// Lean sets behind the shared stores.
    fn sync_sets(&mut self) -> Result<(), D3d12Error> {
        for (index, slot) in self.sps.iter_mut().enumerate() {
            let id = u8::try_from(index).map_err(|_| {
                D3d12Error::Parse(ParseReason::Range {
                    syntax: "seq_parameter_set_id",
                })
            })?;
            *slot = self.inner.sps(id).map(sps_from_shared);
        }
        for (index, slot) in self.pps.iter_mut().enumerate() {
            let id = u8::try_from(index).map_err(|_| {
                D3d12Error::Parse(ParseReason::Range {
                    syntax: "pic_parameter_set_id",
                })
            })?;
            *slot = match self.inner.pps(id) {
                Some(shared) => {
                    let sps = self
                        .inner
                        .sps(shared.sps_id)
                        .ok_or(D3d12Error::Parse(ParseReason::PpsCitesUnknownSps))?;
                    Some(pps_from_shared(shared, sps)?)
                }
                None => None,
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod gate_tests {
    use rust_video_h265::{HevcGate, Pps as SharedPps, Sps as SharedSps};

    use super::{approx_shared_sps, Gate, Sps};
    use crate::error::{D3d12Error, ParseReason, UnsupportedReason};

    /// Lean sequence set with 64x48 and Main blocks.
    fn lean_sps() -> Sps {
        Sps {
            seq_parameter_set_id: 0,
            width: 64,
            height: 48,
            crop: None,
            log2_max_poc_lsb_minus4: 0,
            max_dec_pic_buffering_minus1: 0,
            max_num_reorder_pics: 0,
            rps: Vec::new(),
            log2_min_cb_minus3: 0,
            log2_diff_max_min_cb: 3,
            log2_min_tb_minus2: 0,
            log2_diff_max_min_tb: 0,
            max_depth_inter: 1,
            max_depth_intra: 1,
            flags: 0,
        }
    }

    /// Shared sequence set with Main profile and one sublayer.
    fn sps() -> SharedSps {
        approx_shared_sps(&lean_sps())
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
            Err(D3d12Error::Unsupported(UnsupportedReason::Sublayers))
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
            Err(D3d12Error::Unsupported(
                UnsupportedReason::ProfileOutsideMain
            ))
        ));
        let mut set = sps();
        set.ptl.level_idc = 121;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Parse(ParseReason::Range { .. }))
        ));
        let mut set = sps();
        set.sps_extension_present = true;
        assert!(matches!(
            Gate::check_sps(&set),
            Err(D3d12Error::Unsupported(
                UnsupportedReason::SpsExtensionPending
            ))
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
            Err(D3d12Error::Parse(ParseReason::PpsCitesUnknownSps))
        ));
        pps.sps_id = 0;
        pps.diff_cu_qp_delta_depth = 32;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Parse(ParseReason::Range { .. }))
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
            Err(D3d12Error::Parse(ParseReason::Range { .. }))
        ));
        pps.log2_parallel_merge_minus2 = 0;
        pps.pps_extension_present = true;
        assert!(matches!(
            Gate::check_pps(&pps, Some(&sps)),
            Err(D3d12Error::Unsupported(
                UnsupportedReason::PpsExtensionPending
            ))
        ));
    }

    #[test]
    fn gate_orders_slices_behind_first() {
        assert!(Gate::check_slice_order(&[0, 4], &[true, false]).is_ok());
        assert!(matches!(
            Gate::check_slice_order(&[4], &[false]),
            Err(D3d12Error::Parse(ParseReason::PictureWithoutFirstSlice))
        ));
        assert!(matches!(
            Gate::check_slice_order(&[0, 0], &[true, false]),
            Err(D3d12Error::Unsupported(
                UnsupportedReason::ArbitrarySliceOrder
            ))
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
            Err(D3d12Error::Unsupported(UnsupportedReason::Multilayer))
        ));
    }
}

/// Synthetic inputs behind submission tests.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Test bit writer.
    pub(crate) struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        pub(crate) fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        pub(crate) fn bit(&mut self, set: bool) {
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
        pub(crate) fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Unsigned Exp-Golomb code.
        pub(crate) fn ue(&mut self, value: u32) {
            let code = value + 1;
            let width = 32 - code.leading_zeros();
            for _ in 1..width {
                self.bit(false);
            }
            self.bits(code, width);
        }

        /// Signed Exp-Golomb code.
        pub(crate) fn se(&mut self, value: i32) {
            let code = if value > 0 {
                u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
            } else {
                u32::try_from(-value).unwrap_or(u32::MAX) * 2
            };
            self.ue(code);
        }

        /// Finished RBSP with emulation prevention inserted.
        pub(crate) fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            while self.filled != 0 {
                self.bit(false);
            }
            let mut out = Vec::new();
            let mut zeros = 0;
            for byte in self.bytes {
                if zeros == 2 && byte <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                out.push(byte);
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            out
        }
    }

    /// Main-profile 64x48 sequence set body.
    pub(crate) fn sps_body() -> Vec<u8> {
        sps_body_inner(false)
    }

    /// Main-profile 64x48 sequence set body with one reference set.
    pub(crate) fn sps_body_rps() -> Vec<u8> {
        sps_body_inner(true)
    }

    /// Sequence set body, with a `[-2]` used set when `with_rps`.
    fn sps_body_inner(with_rps: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.bits(0, 2);
        writer.bit(false);
        writer.bits(1, 5);
        writer.bits(0x4000_0000, 32);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bits(0, 32);
        writer.bits(0, 11);
        writer.bit(false);
        writer.bits(93, 8);
        writer.ue(0);
        writer.ue(1);
        writer.ue(64);
        writer.ue(48);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(1);
        writer.ue(1);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        if with_rps {
            writer.ue(1);
            writer.ue(1);
            writer.ue(0);
            writer.ue(1);
            writer.bit(true);
        } else {
            writer.ue(0);
        }
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// Picture set body over sequence set zero.
    pub(crate) fn pps_body() -> Vec<u8> {
        pps_body_merge(0)
    }

    /// Picture set body with merge level `level`.
    pub(crate) fn pps_body_merge(level: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
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
        writer.ue(level);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// IDR slice body over the test sets.
    pub(crate) fn idr_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.ue(2);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// P slice body over the test sets: POC 2 with an explicit `[-2]` used set.
    pub(crate) fn p_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(1);
        writer.bits(2, 4);
        writer.bit(false);
        writer.ue(1);
        writer.ue(0);
        writer.ue(1);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.finish()
    }

    /// P slice body citing table set zero at POC 2.
    pub(crate) fn p_body_table() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(1);
        writer.bits(2, 4);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.finish()
    }

    /// B slice body over the test sets: POC 1 with explicit `[-1]` and `[1]` used sets.
    pub(crate) fn b_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.bits(1, 4);
        writer.bit(false);
        writer.ue(1);
        writer.ue(1);
        writer.ue(0);
        writer.bit(true);
        writer.ue(0);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.finish()
    }

    /// NAL unit with two-byte header over the body.
    pub(crate) fn nal(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![nal_type << 1, 1];
        nal.extend_from_slice(body);
        nal
    }

    /// Packet of NAL units with four-byte start codes.
    pub(crate) fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut packet = Vec::new();
        for nal in nals {
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(nal);
        }
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{
        b_body, idr_body, nal, p_body, p_body_table, packet, pps_body, pps_body_merge, sps_body,
        sps_body_rps,
    };
    use super::{parse_pps, parse_sps, split_nals, Ingest, SliceType};
    use rust_video_h265::rbsp;

    #[test]
    fn vui_parses_timing_and_hrd() {
        let mut writer = super::fixtures::BitWriter::new();
        for _ in 0..8 {
            writer.bit(false);
        }
        writer.bit(true);
        writer.bits(1, 32);
        writer.bits(1, 32);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 4);
        writer.bits(0, 4);
        writer.bits(0, 5);
        writer.bits(0, 5);
        writer.bits(0, 5);
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        let body = rbsp(&writer.finish()).into_owned();
        let mut reader = rust_video_h265::BitReader::<crate::error::D3d12Error>::new(&body);
        let vui = rust_video_h265::parse_vui(&mut reader, 1).expect("hrd parsed");
        assert!(vui.timing.present);
        assert_eq!(
            (
                vui.timing.num_units_in_tick,
                vui.timing.time_scale,
                vui.timing.proportional
            ),
            (1, 1, false)
        );
        let hrd = vui.hrd.expect("hrd parameters");
        assert!(hrd.streams.nal);
        assert!(!hrd.streams.vcl);
        assert!(!hrd.sub_pic);
        assert_eq!(hrd.fixed_rate, vec![true]);
        assert_eq!(hrd.cpb_count_minus1, vec![0]);
        assert_eq!(hrd.nal_layers[0].bitrate_minus1, vec![0]);
        assert_eq!(hrd.nal_layers[0].cbr, vec![false]);
        assert!(hrd.vcl_layers[0].bitrate_minus1.is_empty(), "no vcl data");
        assert!(vui.restriction.is_none());
        let mut bare = rust_video_h265::BitReader::<crate::error::D3d12Error>::new(&[0x00, 0x10]);
        let bare = rust_video_h265::parse_vui(&mut bare, 1).expect("vui without timing");
        assert!(!bare.timing.present);
        assert!(bare.hrd.is_none());
    }

    #[test]
    fn split_nals_strips_start_codes() {
        let nals = split_nals(b"\0\0\0\x01\x42\x01abc\0\0\x01\x44\x01de");
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0][0] >> 1, 33);
    }

    #[test]
    fn sps_reports_coded_size() {
        let sps = parse_sps(&rbsp(&sps_body())).expect("valid sps");
        assert_eq!((sps.width, sps.height), (64, 48));
        assert_eq!(sps.visible_width(), 64);
        assert_eq!(sps.visible_height(), 48);
        assert!(sps.sao());
        assert_eq!(sps.max_num_reorder_pics, 0);
        assert_eq!(sps.rps.len(), 0);
        assert_eq!(sps.max_poc_lsb(), 16);
    }

    #[test]
    fn sps_parses_rps_table() {
        let sps = parse_sps(&rbsp(&sps_body_rps())).expect("valid sps");
        assert_eq!(sps.rps.len(), 1);
        let set = &sps.rps[0];
        assert_eq!(set.negative_count, 1);
        assert_eq!(set.positive_count, 0);
        assert_eq!(set.negative[0], -2);
        assert!(set.negative_used[0]);
    }

    #[test]
    fn bench_vector_parses_rps_table_and_idr() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rust-video-harness/vectors/bench/h265-1080p-nvenc.h265");
        let data = std::fs::read(path).expect("bench vector readable");
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("bench ingest");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].idr);
        let sps = ingest.sps(0).expect("bench sps");
        assert_eq!(sps.rps.len(), 1);
        assert_eq!((sps.visible_width(), sps.visible_height()), (1920, 1080));
    }

    #[test]
    fn pps_reports_flags_and_defaults() {
        let sps = parse_sps(&rbsp(&sps_body())).expect("valid sps");
        let pps = parse_pps(&rbsp(&pps_body()), &sps).expect("valid pps");
        assert!(!pps.tiles());
        assert!(!pps.deblocking_override());
        assert_eq!(pps.log2_parallel_merge_level_minus2, 0);
    }

    #[test]
    fn pps_rejects_merge_level_above_ctb() {
        let sps = parse_sps(&rbsp(&sps_body())).expect("valid sps");
        assert!(parse_pps(&rbsp(&pps_body_merge(2)), &sps).is_err());
    }

    #[test]
    fn ingest_assembles_idr_picture() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].idr);
        assert!(pictures[0].irap);
        assert!(!pictures[0].rasl);
        assert_eq!(pictures[0].poc_lsb(), None);
    }

    #[test]
    fn ingest_assembles_p_picture() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
            nal(1, &p_body()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 2);
        let picture = &pictures[1];
        assert!(!picture.idr);
        assert!(!picture.irap);
        assert_eq!(picture.poc_lsb(), Some(2));
        let header = &picture.slices[0].header;
        assert_eq!(header.slice_type, SliceType::P);
        let set = header.rps.as_ref().expect("explicit set");
        assert_eq!((set.negative_count, set.positive_count), (1, 0));
        assert_eq!(set.negative[0], -2);
        assert!(set.negative_used[0]);
        let bits = header.rps_bits.expect("explicit provenance");
        assert!(bits.explicit);
        assert!(bits.bits > 0);
    }

    #[test]
    fn ingest_assembles_b_picture() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
            nal(1, &b_body()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 2);
        let picture = &pictures[1];
        assert_eq!(picture.poc_lsb(), Some(1));
        let header = &picture.slices[0].header;
        assert_eq!(header.slice_type, SliceType::B);
        let set = header.rps.as_ref().expect("explicit set");
        assert_eq!((set.negative[0], set.positive[0]), (-1, 1));
        assert!(set.negative_used[0] && set.positive_used[0]);
    }

    #[test]
    fn ingest_cites_rps_table() {
        let data = packet(&[
            nal(33, &sps_body_rps()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
            nal(1, &p_body_table()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 2);
        let header = &pictures[1].slices[0].header;
        let set = header.rps.as_ref().expect("table set");
        assert_eq!(set.negative[0], -2);
        assert!(set.negative_used[0]);
        let bits = header.rps_bits.expect("table provenance");
        assert!(!bits.explicit);
    }

    #[test]
    fn ingest_rejects_garbage() {
        let mut ingest = Ingest::new();
        assert!(ingest
            .ingest_packet(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00])
            .is_err());
    }
}
