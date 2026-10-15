//! H.265 Annex-B parameter sets and slice headers.
//!
//! Syntax per ITU-T H.265 section 7.3; the hardware parses slice bodies,
//! so the slice header stops after reference lists.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use crate::error::ParseError;
use crate::parse::bits::{BitReader, Rbsp};
use crate::parse::nal::Nalu as RawNalu;

/// H.265 NAL unit type id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NalType(u8);

impl NalType {
    /// True for coded slice segments.
    #[must_use]
    pub fn is_slice(self) -> bool {
        self.0 <= 21
    }

    /// True for IRAP pictures.
    #[must_use]
    pub fn is_irap(self) -> bool {
        (16..=21).contains(&self.0)
    }

    /// True for IDR pictures.
    #[must_use]
    pub fn is_idr(self) -> bool {
        self.0 == 19 || self.0 == 20
    }

    /// Raw type id.
    #[must_use]
    pub fn id(self) -> u8 {
        self.0
    }
}

/// H.265 NAL header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NalHeader {
    /// Payload type.
    unit_type: NalType,
    /// Temporal id.
    temporal_id: u8,
}

impl NalHeader {
    /// Payload type.
    #[must_use]
    pub fn unit_type(self) -> NalType {
        self.unit_type
    }

    /// Temporal id.
    #[must_use]
    pub fn temporal_id(self) -> u8 {
        self.temporal_id
    }
}

/// Source flags of one profile record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceFlags {
    /// Progressive source.
    pub(crate) progressive: bool,
    /// Interlaced source.
    pub(crate) interlaced: bool,
}

/// Packing constraint flags of one profile record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConstraintFlags {
    /// Non-packed constraint.
    pub(crate) non_packed: bool,
    /// Frame-only constraint.
    pub(crate) frame_only: bool,
}

/// Profile, tier and level.
#[derive(Clone, Debug)]
pub(crate) struct ProfileTierLevel {
    /// Tier flag.
    tier: bool,
    /// Profile indicator.
    profile_idc: u8,
    /// Source flags.
    source: SourceFlags,
    /// Packing constraint flags.
    constraints: ConstraintFlags,
    /// Level indicator.
    level_idc: u8,
}

impl ProfileTierLevel {
    /// Profile indicator.
    #[must_use]
    pub fn profile_idc(&self) -> u8 {
        self.profile_idc
    }

    /// Level indicator.
    #[must_use]
    pub fn level_idc(&self) -> u8 {
        self.level_idc
    }

    /// Tier flag.
    #[must_use]
    pub fn tier(&self) -> bool {
        self.tier
    }

    /// Progressive source.
    #[must_use]
    pub fn progressive(&self) -> bool {
        self.source.progressive
    }

    /// Non-packed constraint.
    #[must_use]
    pub fn non_packed(&self) -> bool {
        self.constraints.non_packed
    }

    /// Frame-only constraint.
    #[must_use]
    pub fn frame_only(&self) -> bool {
        self.constraints.frame_only
    }

    /// Profile, tier and level for `max_sub_layers` sub-layers.
    fn parse(reader: &mut BitReader<'_>, max_sub_layers: u8) -> Result<Self, ParseError> {
        let general = Self::parse_profile(reader, "ptl")?;
        if general.source.interlaced {
            return Err(ParseError::Unsupported {
                what: "interlaced source",
            });
        }
        if max_sub_layers > 1 {
            for _ in max_sub_layers - 1..8 {
                reader.skip("ptl reserved", 2)?;
            }
            let mut layers = Vec::new();
            for _ in 1..max_sub_layers {
                let profile_present = reader.bit("ptl sub profile")?;
                let level_present = reader.bit("ptl sub level")?;
                layers.push((profile_present, level_present));
            }
            for (profile_present, level_present) in layers {
                if profile_present {
                    Self::parse_profile(reader, "ptl sub")?;
                }
                if level_present {
                    narrow_u8(reader.bits("ptl sub level", 8)?, "ptl sub level")?;
                }
            }
        }
        let level_idc = narrow_u8(reader.bits("ptl level", 8)?, "ptl level")?;
        Ok(Self {
            level_idc,
            ..general
        })
    }

    /// One profile record without its level byte.
    fn parse_profile(reader: &mut BitReader<'_>, syntax: &'static str) -> Result<Self, ParseError> {
        reader.skip(syntax, 2)?;
        let tier = reader.bit(syntax)?;
        let profile_idc = narrow_u8(reader.bits(syntax, 5)?, syntax)?;
        let compatibility = reader.bits(syntax, 32)?;
        let progressive = reader.bit(syntax)?;
        let interlaced = reader.bit(syntax)?;
        let non_packed = reader.bit(syntax)?;
        let frame_only = reader.bit(syntax)?;
        if !matches!(profile_idc, 1..=3) && compatibility & 0x0E00_0000 == 0 {
            return Err(ParseError::Unsupported {
                what: "h265 profile",
            });
        }
        reader.skip(syntax, 43)?;
        reader.skip(syntax, 1)?;
        Ok(Self {
            tier,
            profile_idc,
            source: SourceFlags {
                progressive,
                interlaced,
            },
            constraints: ConstraintFlags {
                non_packed,
                frame_only,
            },
            level_idc: 0,
        })
    }
}

/// Checked narrowing of a parsed value to `u8`.
fn narrow_u8(value: u32, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a parsed value to `u16`.
fn narrow_u16(value: u32, syntax: &'static str) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// VPS timing information.
#[derive(Clone, Debug, Default)]
pub(crate) struct VpsTiming {
    /// Ticks per second.
    pub(crate) num_units_in_tick: u32,
    /// Time scale.
    pub(crate) time_scale: u32,
    /// POC proportional to timing.
    pub(crate) poc_proportional: bool,
    /// Ticks per POC step minus 1.
    pub(crate) num_ticks_poc_diff_one_minus1: u32,
    /// HRD parameter sets.
    pub(crate) hrd: Vec<HrdParams>,
}

/// Video parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Vps {
    /// Own id.
    id: u8,
    /// Highest sub-layer index.
    max_sub_layers_minus1: u8,
    /// Temporal id nesting.
    temporal_nesting: bool,
    /// Profile, tier and level.
    ptl: ProfileTierLevel,
    /// Sub-layer ordering info present.
    ordering_info_present: bool,
    /// DPB capacity per sub-layer minus 1.
    max_dec_pic_buffering: Vec<u32>,
    /// Reorder depth per sub-layer.
    max_num_reorder_pics: Vec<u32>,
    /// Maximum latency per sub-layer.
    max_latency: Vec<u32>,
    /// Timing information, when present.
    timing: Option<VpsTiming>,
}

impl Vps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Highest sub-layer index.
    #[must_use]
    pub fn max_sub_layers_minus1(&self) -> u8 {
        self.max_sub_layers_minus1
    }

    /// Temporal id nesting.
    #[must_use]
    pub fn temporal_nesting(&self) -> bool {
        self.temporal_nesting
    }

    /// Profile, tier and level.
    #[must_use]
    pub fn ptl(&self) -> &ProfileTierLevel {
        &self.ptl
    }

    /// Sub-layer ordering info present.
    #[must_use]
    pub fn ordering_info_present(&self) -> bool {
        self.ordering_info_present
    }

    /// DPB capacity per sub-layer minus 1.
    #[must_use]
    pub fn max_dec_pic_buffering(&self) -> &[u32] {
        &self.max_dec_pic_buffering
    }

    /// Reorder depth per sub-layer.
    #[must_use]
    pub fn max_num_reorder_pics(&self) -> &[u32] {
        &self.max_num_reorder_pics
    }

    /// Maximum latency per sub-layer.
    #[must_use]
    pub fn max_latency(&self) -> &[u32] {
        &self.max_latency
    }

    /// Timing information, when present.
    #[must_use]
    pub fn timing(&self) -> Option<&VpsTiming> {
        self.timing.as_ref()
    }

    /// Video set from RBSP bytes.
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ParseError> {
        let vps_id = narrow_u8(reader.bits("vps id", 4)?, "vps id")?;
        reader.skip("vps base layer", 2)?;
        let max_layers_minus1 = reader.bits("vps max layers", 6)?;
        if max_layers_minus1 > 0 {
            return Err(ParseError::Unsupported {
                what: "h265 layers",
            });
        }
        let max_sub_layers_minus1 = narrow_u8(reader.bits("vps max sub", 3)?, "vps max sub")?;
        if max_sub_layers_minus1 > 6 {
            return Err(ParseError::OutOfRange {
                syntax: "vps max sub",
            });
        }
        let temporal_nesting = reader.bit("vps nesting")?;
        reader.skip("vps reserved", 16)?;
        let ptl = ProfileTierLevel::parse(reader, max_sub_layers_minus1 + 1)?;
        let ordering_info_present = reader.bit("vps ordering info")?;
        let start = if ordering_info_present {
            0
        } else {
            u32::from(max_sub_layers_minus1)
        };
        let mut max_dec_pic_buffering = Vec::new();
        let mut max_num_reorder_pics = Vec::new();
        let mut max_latency = Vec::new();
        for _ in start..=u32::from(max_sub_layers_minus1) {
            max_dec_pic_buffering.push(reader.ue("vps buffering")?);
            max_num_reorder_pics.push(reader.ue("vps reorder")?);
            max_latency.push(reader.ue("vps latency")?);
        }
        reader.skip("vps max layer id", 6)?;
        let layer_sets = reader.ue("vps layer sets")?;
        if layer_sets > 0 {
            return Err(ParseError::Unsupported {
                what: "h265 layer sets",
            });
        }
        let layers = u32::from(max_sub_layers_minus1) + 1;
        let mut timing = None;
        if reader.bit("vps timing present")? {
            let num_units_in_tick = reader.bits("vps tick", 32)?;
            let time_scale = reader.bits("vps scale", 32)?;
            let poc_proportional = reader.bit("vps poc proportional")?;
            let num_ticks_poc_diff_one_minus1 = if poc_proportional {
                reader.ue("vps poc ticks")?
            } else {
                0
            };
            let hrd_count = reader.ue("vps hrd count")?;
            if hrd_count > 16 {
                return Err(ParseError::Unsupported { what: "vps hrd" });
            }
            let layers_u8 =
                u8::try_from(layers).map_err(|_| ParseError::OutOfRange { syntax: "vps hrd" })?;
            let mut hrd = Vec::new();
            for index in 0..hrd_count {
                reader.ue("vps hrd layer set")?;
                let mut inherit = None;
                if index > 0 && !reader.bit("vps hrd common")? {
                    let first: &HrdParams = &hrd[0];
                    inherit = Some((first.streams, first.sub_pic));
                }
                hrd.push(parse_hrd(reader, layers_u8, inherit)?);
            }
            timing = Some(VpsTiming {
                num_units_in_tick,
                time_scale,
                poc_proportional,
                num_ticks_poc_diff_one_minus1,
                hrd,
            });
        }
        if reader.more_rbsp() && reader.bit("vps extension")? {
            return Err(ParseError::Unsupported {
                what: "vps extension",
            });
        }
        Ok(Self {
            id: vps_id,
            max_sub_layers_minus1,
            temporal_nesting,
            ptl,
            ordering_info_present,
            max_dec_pic_buffering,
            max_num_reorder_pics,
            max_latency,
            timing,
        })
    }
}

/// Scaling lists for one parameter set.
#[derive(Clone, Debug)]
pub(crate) struct ScalingLists {
    /// 4x4 lists.
    pub(crate) list_4x4: [[u8; 16]; 6],
    /// 8x8 lists.
    pub(crate) list_8x8: [[u8; 64]; 6],
    /// 16x16 lists.
    pub(crate) list_16x16: [[u8; 64]; 6],
    /// 32x32 lists.
    pub(crate) list_32x32: [[u8; 64]; 2],
    /// 16x16 DC coefficients.
    pub(crate) dc_16x16: [u8; 6],
    /// 32x32 DC coefficients.
    pub(crate) dc_32x32: [u8; 2],
}

impl Default for ScalingLists {
    fn default() -> Self {
        Self {
            list_4x4: [[16; 16]; 6],
            list_8x8: [[16; 64]; 6],
            list_16x16: [[16; 64]; 6],
            list_32x32: [[16; 64]; 2],
            dc_16x16: [16; 6],
            dc_32x32: [16; 2],
        }
    }
}

impl ScalingLists {
    /// Scaling lists from `scaling_list_data`.
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ParseError> {
        let mut lists = Self::default();
        for size_id in 0..4_usize {
            let step = if size_id == 3 { 3 } else { 1 };
            let mut matrix_id = 0_usize;
            while matrix_id < 6 {
                if reader.bit("scaling pred mode")? {
                    let coef_num: usize = 64.min(1 << (4 + (size_id << 1)));
                    if size_id > 1 {
                        let dc = reader.se("scaling dc")? + 8;
                        let dc = u8::try_from(dc).map_err(|_| ParseError::OutOfRange {
                            syntax: "scaling dc",
                        })?;
                        if size_id == 2 {
                            lists.dc_16x16[matrix_id] = dc;
                        } else {
                            lists.dc_32x32[matrix_id / 3] = dc;
                        }
                    }
                    let mut next_coef = 8_i32;
                    for i in 0..coef_num {
                        if i == 0 && size_id > 1 {
                            let dc = if size_id == 2 {
                                i32::from(lists.dc_16x16[matrix_id])
                            } else {
                                i32::from(lists.dc_32x32[matrix_id / 3])
                            };
                            next_coef = dc;
                        }
                        let delta = reader.se("scaling delta")?;
                        next_coef = (next_coef + delta + 256) % 256;
                        let value =
                            u8::try_from(next_coef).map_err(|_| ParseError::OutOfRange {
                                syntax: "scaling delta",
                            })?;
                        match size_id {
                            0 => lists.list_4x4[matrix_id][i] = value,
                            1 => lists.list_8x8[matrix_id][i] = value,
                            2 => lists.list_16x16[matrix_id][i] = value,
                            _ => lists.list_32x32[matrix_id / 3][i] = value,
                        }
                    }
                } else {
                    reader.ue("scaling pred delta")?;
                }
                matrix_id += step;
            }
        }
        Ok(lists)
    }
}

/// One short-term reference picture set with derived POC deltas.
#[derive(Clone, Debug, Default)]
pub(crate) struct RefPicSet {
    /// Negative deltas with used-by-current flags, coding order.
    s0: Vec<(i32, bool)>,
    /// Positive deltas with used-by-current flags, coding order.
    s1: Vec<(i32, bool)>,
    /// Delta pictures of the predicted-from set.
    ref_pics: u8,
    /// Inter-predicted from another set.
    inter: bool,
    /// Reference set index delta minus 1, slice sets only.
    delta_idx_minus1: u32,
    /// Delta RPS sign.
    delta_rps_sign: bool,
    /// Absolute delta RPS minus 1.
    abs_delta_rps_minus1: u16,
    /// Used-by-current mask over the reference set's pictures.
    used_mask: u16,
    /// Use-delta mask over the reference set's pictures.
    use_mask: u16,
}

impl RefPicSet {
    /// Delta pictures of the predicted-from set.
    #[must_use]
    pub fn ref_pics(&self) -> u8 {
        self.ref_pics
    }

    /// Negative deltas with used-by-current flags, coding order.
    #[must_use]
    pub fn s0(&self) -> &[(i32, bool)] {
        &self.s0
    }

    /// Positive deltas with used-by-current flags, coding order.
    #[must_use]
    pub fn s1(&self) -> &[(i32, bool)] {
        &self.s1
    }

    /// Inter-predicted from another set.
    #[must_use]
    pub fn inter(&self) -> bool {
        self.inter
    }

    /// Reference set index delta minus 1, slice sets only.
    #[must_use]
    pub fn delta_idx_minus1(&self) -> u32 {
        self.delta_idx_minus1
    }

    /// Delta RPS sign.
    #[must_use]
    pub fn delta_rps_sign(&self) -> bool {
        self.delta_rps_sign
    }

    /// Absolute delta RPS minus 1.
    #[must_use]
    pub fn abs_delta_rps_minus1(&self) -> u16 {
        self.abs_delta_rps_minus1
    }

    /// Used-by-current mask over the reference set's pictures.
    #[must_use]
    pub fn used_mask(&self) -> u16 {
        self.used_mask
    }

    /// Use-delta mask over the reference set's pictures.
    #[must_use]
    pub fn use_mask(&self) -> u16 {
        self.use_mask
    }
}

impl RefPicSet {
    /// Set from `st_ref_pic_set` syntax at index `idx`.
    ///
    /// `inline` marks a slice-header set, the only place `delta_idx_minus1` is coded.
    fn parse(
        reader: &mut BitReader<'_>,
        idx: usize,
        sets: &[Self],
        inline: bool,
    ) -> Result<Self, ParseError> {
        if idx > 0 && reader.bit("rps inter predict")? {
            return Self::parse_inter(reader, idx, sets, inline);
        }
        let mut set = Self::default();
        let negative = reader.ue("rps negative")?;
        let positive = reader.ue("rps positive")?;
        if negative + positive > 16 {
            return Err(ParseError::Unsupported {
                what: "oversize rps",
            });
        }
        let mut prev = 0_i32;
        for _ in 0..negative {
            let delta = reader.ue("rps delta s0")?;
            let used = reader.bit("rps used s0")?;
            prev -= i32::try_from(delta)
                .map_err(|_| ParseError::OutOfRange {
                    syntax: "rps delta s0",
                })?
                .saturating_add(1);
            set.s0.push((prev, used));
        }
        prev = 0;
        for _ in 0..positive {
            let delta = reader.ue("rps delta s1")?;
            let used = reader.bit("rps used s1")?;
            prev += i32::try_from(delta)
                .map_err(|_| ParseError::OutOfRange {
                    syntax: "rps delta s1",
                })?
                .saturating_add(1);
            set.s1.push((prev, used));
        }
        Ok(set)
    }

    /// Inter-predicted set from a reference set.
    fn parse_inter(
        reader: &mut BitReader<'_>,
        idx: usize,
        sets: &[Self],
        inline: bool,
    ) -> Result<Self, ParseError> {
        let mut ref_idx = idx - 1;
        let mut delta_idx_minus1 = 0;
        if inline {
            delta_idx_minus1 = reader.ue("rps delta idx")?;
            ref_idx = idx
                .checked_sub(
                    1 + usize::try_from(delta_idx_minus1).map_err(|_| ParseError::OutOfRange {
                        syntax: "rps delta idx",
                    })?,
                )
                .ok_or(ParseError::OutOfRange {
                    syntax: "rps delta idx",
                })?;
        }
        let reference = sets.get(ref_idx).ok_or(ParseError::OutOfRange {
            syntax: "rps delta idx",
        })?;
        let ref_pics = u8::try_from(reference.s0.len() + reference.s1.len()).unwrap_or(u8::MAX);
        let delta_rps_sign = reader.bit("rps delta sign")?;
        let abs = reader.ue("rps abs delta")?;
        let abs_delta_rps_minus1 = u16::try_from(abs).map_err(|_| ParseError::OutOfRange {
            syntax: "rps abs delta",
        })?;
        let delta = i32::try_from(abs)
            .map_err(|_| ParseError::OutOfRange {
                syntax: "rps abs delta",
            })?
            .saturating_add(1);
        let delta = if delta_rps_sign { -delta } else { delta };
        let mut combined: Vec<(i32, bool, bool)> = Vec::new();
        let mut used_mask = 0_u16;
        let mut keep_mask = 0_u16;
        for (bit, (poc, _)) in reference.s0.iter().chain(reference.s1.iter()).enumerate() {
            let used = reader.bit("rps used")?;
            let keep = if used {
                true
            } else {
                reader.bit("rps use delta")?
            };
            if bit < 16 {
                let flag = 1_u16 << bit;
                if used {
                    used_mask |= flag;
                }
                if keep {
                    keep_mask |= flag;
                }
            }
            combined.push((*poc, used, keep));
        }
        let mut set = Self::default();
        for (poc, used, keep) in combined {
            if !keep {
                continue;
            }
            let derived = poc.saturating_add(delta);
            match derived.cmp(&0) {
                Ordering::Less => set.s0.push((derived, used)),
                Ordering::Greater => set.s1.push((derived, used)),
                Ordering::Equal => {}
            }
        }
        set.ref_pics = ref_pics;
        set.inter = true;
        set.delta_idx_minus1 = delta_idx_minus1;
        set.delta_rps_sign = delta_rps_sign;
        set.abs_delta_rps_minus1 = abs_delta_rps_minus1;
        set.used_mask = used_mask;
        set.use_mask = keep_mask;
        Ok(set)
    }
}

/// Long-term reference picture from the SPS.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LtSps {
    /// POC least significant bits.
    pub(crate) poc_lsb: u32,
    /// Used by the current picture.
    pub(crate) used: bool,
}

/// PCM parameters of one sequence set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PcmParams {
    /// Luma PCM bit depth minus 1.
    pub(crate) luma_depth_minus1: u8,
    /// Chroma PCM bit depth minus 1.
    pub(crate) chroma_depth_minus1: u8,
    /// Minimum PCM block size minus 3.
    pub(crate) min_cb_minus3: u32,
    /// PCM block size range.
    pub(crate) diff_cb: u32,
    /// Loop filter disabled on PCM.
    pub(crate) loop_filter_disabled: bool,
}

/// HRD coded-picture-buffer data of one layer.
#[derive(Clone, Debug, Default)]
pub(crate) struct HrdLayer {
    /// Bitrate minus 1 per CPB.
    pub(crate) bitrate_minus1: Vec<u32>,
    /// CPB size minus 1 per CPB.
    pub(crate) cpb_size_minus1: Vec<u32>,
    /// DU CPB size minus 1 per CPB.
    pub(crate) du_cpb_size_minus1: Vec<u32>,
    /// DU bitrate minus 1 per CPB.
    pub(crate) du_bitrate_minus1: Vec<u32>,
    /// CBR flag per CPB.
    pub(crate) cbr: Vec<bool>,
}

/// HRD stream presence of one parameter set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HrdStreams {
    /// NAL HRD present.
    pub(crate) nal: bool,
    /// VCL HRD present.
    pub(crate) vcl: bool,
}

/// HRD parameters of one layer set.
#[derive(Clone, Debug, Default)]
pub(crate) struct HrdParams {
    /// Shared header coded in this set rather than inherited.
    pub(crate) common: bool,
    /// Stream presence flags.
    pub(crate) streams: HrdStreams,
    /// Sub-picture HRD.
    pub(crate) sub_pic: bool,
    /// Tick divisor minus 2.
    pub(crate) tick_divisor_minus2: u8,
    /// Bitrate scale.
    pub(crate) bitrate_scale: u8,
    /// CPB size scale.
    pub(crate) cpb_scale: u8,
    /// DU CPB size scale.
    pub(crate) du_cpb_scale: u8,
    /// Initial CPB removal delay length minus 1.
    pub(crate) initial_delay_minus1: u8,
    /// AU CPB removal delay length minus 1.
    pub(crate) au_delay_minus1: u8,
    /// DPB output delay length minus 1.
    pub(crate) dpb_delay_minus1: u8,
    /// Fixed picture rate per layer.
    pub(crate) fixed_rate: Vec<bool>,
    /// Picture duration per layer.
    pub(crate) pic_duration: Vec<bool>,
    /// Elemental duration minus 1 per layer.
    pub(crate) elemental: Vec<u16>,
    /// CPB count minus 1 per layer.
    pub(crate) cpb_count_minus1: Vec<u32>,
    /// NAL CPB data per layer.
    pub(crate) nal_layers: Vec<HrdLayer>,
    /// VCL CPB data per layer.
    pub(crate) vcl_layers: Vec<HrdLayer>,
}

/// VUI bitstream restriction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BitstreamRestriction {
    /// Tiles fixed structure.
    pub(crate) tiles_fixed: bool,
    /// Motion vectors over picture boundaries.
    pub(crate) mv_over_pic: bool,
    /// Restricted reference pictures.
    pub(crate) restricted_ref: bool,
    /// Minimum spatial segmentation idc.
    pub(crate) min_spatial_segmentation: u16,
    /// Maximum bytes per picture denominator.
    pub(crate) max_bytes_per_pic_denom: u8,
    /// Maximum bits per MCU denominator.
    pub(crate) max_bits_per_mcu_denom: u8,
    /// Horizontal motion vector length bits.
    pub(crate) max_mv_horizontal: u8,
    /// Vertical motion vector length bits.
    pub(crate) max_mv_vertical: u8,
}

/// Aspect-ratio block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AspectRatio {
    /// Aspect ratio present.
    pub(crate) present: bool,
    /// Aspect ratio idc.
    pub(crate) idc: u8,
    /// Sample aspect width.
    pub(crate) sar_width: u16,
    /// Sample aspect height.
    pub(crate) sar_height: u16,
}

/// Overscan block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Overscan {
    /// Overscan info present.
    pub(crate) present: bool,
    /// Overscan appropriate.
    pub(crate) appropriate: bool,
}

/// Video-signal block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VideoSignal {
    /// Video signal present.
    pub(crate) present: bool,
    /// Video format.
    pub(crate) format: u8,
    /// Full range.
    pub(crate) full_range: bool,
    /// Colour description present.
    pub(crate) colour_present: bool,
    /// Colour primaries.
    pub(crate) primaries: u8,
    /// Transfer characteristics.
    pub(crate) transfer: u8,
    /// Matrix coefficients.
    pub(crate) matrix: u8,
}

/// Chroma-location block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChromaLoc {
    /// Chroma location present.
    pub(crate) present: bool,
    /// Chroma top-field location.
    pub(crate) top: u32,
    /// Chroma bottom-field location.
    pub(crate) bottom: u32,
}

/// Timing block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VuiTiming {
    /// Timing present.
    pub(crate) present: bool,
    /// Ticks per second.
    pub(crate) num_units_in_tick: u32,
    /// Time scale.
    pub(crate) time_scale: u32,
    /// POC proportional to timing.
    pub(crate) proportional: bool,
    /// Ticks per POC step minus 1.
    pub(crate) num_ticks_poc_diff_one_minus1: u32,
}

/// VUI parameters of one sequence set.
#[derive(Clone, Debug, Default)]
pub(crate) struct VuiParams {
    /// Aspect-ratio block.
    pub(crate) aspect: AspectRatio,
    /// Overscan block.
    pub(crate) overscan: Overscan,
    /// Video-signal block.
    pub(crate) signal: VideoSignal,
    /// Chroma-location block.
    pub(crate) chroma_loc: ChromaLoc,
    /// Neutral chroma indication.
    pub(crate) neutral_chroma: bool,
    /// Field sequence flag.
    pub(crate) field_seq: bool,
    /// Frame field info present.
    pub(crate) frame_field_info: bool,
    /// First default display window left, right, top, bottom.
    pub(crate) display_window: Option<[u32; 4]>,
    /// Timing block.
    pub(crate) timing: VuiTiming,
    /// HRD parameters, when present.
    pub(crate) hrd: Option<HrdParams>,
    /// Bitstream restriction, when present.
    pub(crate) restriction: Option<BitstreamRestriction>,
}

/// Sub-layer ordering limits of one sequence set.
#[derive(Clone, Debug, Default)]
pub(crate) struct SubLayerOrdering {
    /// Sub-layer ordering info present.
    pub(crate) info_present: bool,
    /// DPB capacity minus 1.
    pub(crate) buffering: Vec<u32>,
    /// Reorder depth per coded layer.
    pub(crate) reorder: Vec<u32>,
    /// Maximum latency per coded layer.
    pub(crate) latency: Vec<u32>,
}

/// Block coding tools of one sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BlockTools {
    /// Asymmetric motion partitions enabled.
    pub(crate) amp: bool,
    /// Sample-adaptive offset enabled.
    pub(crate) sao: bool,
    /// Scaling lists enabled.
    pub(crate) scaling: bool,
}

/// Prediction tools of one sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PredictionTools {
    /// Long-term pictures may appear in slices.
    pub(crate) long_term_present: bool,
    /// Temporal motion-vector prediction enabled.
    pub(crate) temporal_mvp: bool,
    /// Strong intra smoothing enabled.
    pub(crate) strong_intra_smoothing: bool,
}

/// Sequence parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Sps {
    /// Video set id.
    vps_id: u8,
    /// Highest sub-layer index.
    max_sub_layers_minus1: u8,
    /// Temporal id nesting.
    temporal_nesting: bool,
    /// Profile, tier and level.
    ptl: ProfileTierLevel,
    /// Own id.
    id: u8,
    /// Chroma sampling indicator.
    chroma_format_idc: u8,
    /// Separate colour plane coding.
    separate_colour_plane: bool,
    /// Picture width in luma samples.
    pic_width: u32,
    /// Picture height in luma samples.
    pic_height: u32,
    /// Conformance window offsets left, right, top, bottom.
    conformance: Option<[u32; 4]>,
    /// Luma bit depth minus 8.
    bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    bit_depth_chroma_minus8: u8,
    /// POC lsb bit width minus 4.
    log2_max_pic_order_cnt_lsb_minus4: u8,
    /// Sub-layer ordering limits.
    ordering: SubLayerOrdering,
    /// Minimum luma block size minus 3.
    log2_min_cb_minus3: u32,
    /// Luma block size range.
    log2_diff_cb: u32,
    /// Minimum transform size minus 2.
    log2_min_tb_minus2: u32,
    /// Transform size range.
    log2_diff_tb: u32,
    /// Maximum inter transform depth.
    max_inter_depth: u32,
    /// Maximum intra transform depth.
    max_intra_depth: u32,
    /// Block coding tools.
    block: BlockTools,
    /// Scaling lists, when present.
    scaling: Option<ScalingLists>,
    /// PCM parameters, when enabled.
    pcm: Option<PcmParams>,
    /// Short-term reference picture sets.
    rps_sets: Vec<RefPicSet>,
    /// Long-term pictures from the SPS.
    long_term: Vec<LtSps>,
    /// Prediction tools.
    prediction: PredictionTools,
    /// VUI parameters, when present.
    vui: Option<VuiParams>,
}

impl Sps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Video set id.
    #[must_use]
    pub fn vps_id(&self) -> u8 {
        self.vps_id
    }

    /// Profile, tier and level.
    #[must_use]
    pub fn ptl(&self) -> &ProfileTierLevel {
        &self.ptl
    }

    /// Chroma sampling indicator.
    #[must_use]
    pub fn chroma_format_idc(&self) -> u8 {
        self.chroma_format_idc
    }

    /// Luma bit depth minus 8.
    #[must_use]
    pub fn bit_depth_luma_minus8(&self) -> u8 {
        self.bit_depth_luma_minus8
    }

    /// Chroma bit depth minus 8.
    #[must_use]
    pub fn bit_depth_chroma_minus8(&self) -> u8 {
        self.bit_depth_chroma_minus8
    }

    /// POC lsb bit width.
    #[must_use]
    pub fn pic_order_cnt_lsb_bits(&self) -> u32 {
        u32::from(self.log2_max_pic_order_cnt_lsb_minus4) + 4
    }

    /// DPB slot budget: capacity plus the picture under decode.
    #[must_use]
    pub fn dpb_slots(&self) -> u32 {
        self.max_dec_pic_buffering_minus1()
            .saturating_add(2)
            .min(16)
    }

    /// Reorder depth of the highest coded layer.
    #[must_use]
    pub fn max_num_reorder_pics(&self) -> u32 {
        self.ordering.reorder.last().copied().unwrap_or(0).min(16)
    }

    /// Short-term reference picture sets.
    #[must_use]
    pub fn rps_sets(&self) -> &[RefPicSet] {
        &self.rps_sets
    }

    /// Long-term pictures from the SPS.
    #[must_use]
    pub fn lt_sps(&self) -> &[LtSps] {
        &self.long_term
    }

    /// Long-term pictures may appear in slices.
    #[must_use]
    pub fn lt_present(&self) -> bool {
        self.prediction.long_term_present
    }

    /// Picture width in luma samples.
    #[must_use]
    pub fn pic_width(&self) -> u32 {
        self.pic_width
    }

    /// Picture height in luma samples.
    #[must_use]
    pub fn pic_height(&self) -> u32 {
        self.pic_height
    }

    /// POC lsb bit width minus 4.
    #[must_use]
    pub fn log2_max_pic_order_cnt_lsb_minus4(&self) -> u8 {
        self.log2_max_pic_order_cnt_lsb_minus4
    }

    /// Temporal motion-vector prediction enabled.
    #[must_use]
    pub fn temporal_mvp_enabled(&self) -> bool {
        self.prediction.temporal_mvp
    }

    /// Sample-adaptive offset enabled.
    #[must_use]
    pub fn sao_enabled(&self) -> bool {
        self.block.sao
    }

    /// Highest sub-layer index.
    #[must_use]
    pub fn max_sub_layers_minus1(&self) -> u8 {
        self.max_sub_layers_minus1
    }

    /// Temporal id nesting.
    #[must_use]
    pub fn temporal_nesting(&self) -> bool {
        self.temporal_nesting
    }

    /// Separate colour plane coding.
    #[must_use]
    pub fn separate_colour_plane(&self) -> bool {
        self.separate_colour_plane
    }

    /// Conformance window offsets left, right, top, bottom.
    #[must_use]
    pub fn conformance(&self) -> Option<[u32; 4]> {
        self.conformance
    }

    /// Sub-layer ordering info present.
    #[must_use]
    pub fn ordering_info_present(&self) -> bool {
        self.ordering.info_present
    }

    /// DPB capacity minus 1 of the highest coded layer.
    #[must_use]
    pub fn max_dec_pic_buffering_minus1(&self) -> u32 {
        self.ordering.buffering.last().copied().unwrap_or(0)
    }

    /// DPB capacity minus 1 per coded layer.
    #[must_use]
    pub fn dec_pic_buffering(&self) -> &[u32] {
        &self.ordering.buffering
    }

    /// Reorder depth per coded layer.
    #[must_use]
    pub fn reorder_pics(&self) -> &[u32] {
        &self.ordering.reorder
    }

    /// Maximum latency per coded layer.
    #[must_use]
    pub fn max_latency(&self) -> &[u32] {
        &self.ordering.latency
    }

    /// Scaling lists enabled.
    #[must_use]
    pub fn scaling_enabled(&self) -> bool {
        self.block.scaling
    }

    /// Minimum luma block size minus 3.
    #[must_use]
    pub fn log2_min_cb_minus3(&self) -> u32 {
        self.log2_min_cb_minus3
    }

    /// Luma block size range.
    #[must_use]
    pub fn log2_diff_cb(&self) -> u32 {
        self.log2_diff_cb
    }

    /// Minimum transform size minus 2.
    #[must_use]
    pub fn log2_min_tb_minus2(&self) -> u32 {
        self.log2_min_tb_minus2
    }

    /// Transform size range.
    #[must_use]
    pub fn log2_diff_tb(&self) -> u32 {
        self.log2_diff_tb
    }

    /// Maximum inter transform depth.
    #[must_use]
    pub fn max_inter_depth(&self) -> u32 {
        self.max_inter_depth
    }

    /// Maximum intra transform depth.
    #[must_use]
    pub fn max_intra_depth(&self) -> u32 {
        self.max_intra_depth
    }

    /// Asymmetric motion partitions enabled.
    #[must_use]
    pub fn amp_enabled(&self) -> bool {
        self.block.amp
    }

    /// Scaling lists, when present.
    #[must_use]
    pub fn scaling(&self) -> Option<&ScalingLists> {
        self.scaling.as_ref()
    }

    /// PCM parameters, when enabled.
    #[must_use]
    pub fn pcm(&self) -> Option<&PcmParams> {
        self.pcm.as_ref()
    }

    /// Strong intra smoothing enabled.
    #[must_use]
    pub fn strong_intra_smoothing_enabled(&self) -> bool {
        self.prediction.strong_intra_smoothing
    }

    /// VUI parameters, when present.
    #[must_use]
    pub fn vui(&self) -> Option<&VuiParams> {
        self.vui.as_ref()
    }

    /// Coded picture size in luma samples, window ignored.
    #[must_use]
    pub fn coded_size(&self) -> (u32, u32) {
        (self.pic_width, self.pic_height)
    }

    /// Display picture size in luma samples, window applied.
    #[must_use]
    pub fn display_size(&self) -> (u32, u32) {
        let Some(window) = self.conformance else {
            return (self.pic_width, self.pic_height);
        };
        let sub_width = if self.chroma_format_idc == 3 { 1 } else { 2 };
        let sub_height = if self.chroma_format_idc <= 1 { 2 } else { 1 };
        let width = self
            .pic_width
            .saturating_sub((window[0] + window[1]) * sub_width);
        let height = self
            .pic_height
            .saturating_sub((window[2] + window[3]) * sub_height);
        (width, height)
    }

    /// Sequence set from RBSP bytes.
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ParseError> {
        let vps_id = narrow_u8(reader.bits("sps vps id", 4)?, "sps vps id")?;
        let max_sub_layers_minus1 = narrow_u8(reader.bits("sps max sub", 3)?, "sps max sub")?;
        if max_sub_layers_minus1 > 6 {
            return Err(ParseError::OutOfRange {
                syntax: "sps max sub",
            });
        }
        let temporal_nesting = reader.bit("sps nesting")?;
        let ptl = ProfileTierLevel::parse(reader, max_sub_layers_minus1 + 1)?;
        let sps_id = narrow_u8(reader.ue("sps id")?, "sps id")?;
        let chroma_format_idc = narrow_u8(reader.ue("sps chroma")?, "sps chroma")?;
        if chroma_format_idc > 3 {
            return Err(ParseError::OutOfRange {
                syntax: "sps chroma",
            });
        }
        let mut separate_colour_plane = false;
        if chroma_format_idc == 3 {
            separate_colour_plane = reader.bit("sps separate plane")?;
        }
        let pic_width = reader.ue("sps width")?;
        let pic_height = reader.ue("sps height")?;
        if pic_width == 0 || pic_height == 0 {
            return Err(ParseError::OutOfRange {
                syntax: "sps width",
            });
        }
        let conformance = parse_conformance(reader)?;
        let bit_depth_luma_minus8 = narrow_u8(reader.ue("sps luma depth")?, "sps luma depth")?;
        let bit_depth_chroma_minus8 =
            narrow_u8(reader.ue("sps chroma depth")?, "sps chroma depth")?;
        let log2_max_pic_order_cnt_lsb_minus4 =
            narrow_u8(reader.ue("sps poc bits")?, "sps poc bits")?;
        if log2_max_pic_order_cnt_lsb_minus4 > 12 {
            return Err(ParseError::OutOfRange {
                syntax: "sps poc bits",
            });
        }
        let ordering_info_present = reader.bit("sps ordering info")?;
        let ordering =
            parse_sub_layer_ordering(reader, ordering_info_present, max_sub_layers_minus1)?;
        let log2_min_cb_minus3 = reader.ue("sps min cb size")?;
        let log2_diff_cb = reader.ue("sps diff cb size")?;
        let log2_min_transform_minus2 = reader.ue("sps min tb size")?;
        let log2_diff_transform = reader.ue("sps diff tb size")?;
        let max_inter_depth = reader.ue("sps max inter depth")?;
        let max_intra_depth = reader.ue("sps max intra depth")?;
        let mut scaling = None;
        let scaling_enabled = reader.bit("sps scaling enabled")?;
        if scaling_enabled && reader.bit("sps scaling present")? {
            scaling = Some(ScalingLists::parse(reader)?);
        }
        let amp_enabled = reader.bit("sps amp")?;
        let sao_enabled = reader.bit("sps sao")?;
        let pcm = parse_pcm_params(reader)?;
        let reference_sets = parse_sps_ref_sets(reader)?;
        let temporal_mvp_enabled = reader.bit("sps temporal mvp")?;
        let strong_intra_smoothing_enabled = reader.bit("sps smoothing")?;
        let vui = parse_sps_vui(reader, max_sub_layers_minus1)?;
        Ok(Self {
            vps_id,
            max_sub_layers_minus1,
            temporal_nesting,
            ptl,
            id: sps_id,
            chroma_format_idc,
            separate_colour_plane,
            pic_width,
            pic_height,
            conformance,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            log2_max_pic_order_cnt_lsb_minus4,
            ordering,
            log2_min_cb_minus3,
            log2_diff_cb,
            log2_min_tb_minus2: log2_min_transform_minus2,
            log2_diff_tb: log2_diff_transform,
            max_inter_depth,
            max_intra_depth,
            block: BlockTools {
                amp: amp_enabled,
                sao: sao_enabled,
                scaling: scaling_enabled,
            },
            scaling,
            pcm,
            rps_sets: reference_sets.short_term,
            long_term: reference_sets.long_term,
            prediction: PredictionTools {
                long_term_present: reference_sets.long_term_present,
                temporal_mvp: temporal_mvp_enabled,
                strong_intra_smoothing: strong_intra_smoothing_enabled,
            },
            vui,
        })
    }
}

/// Conformance window of one sequence set, when present.
fn parse_conformance(reader: &mut BitReader<'_>) -> Result<Option<[u32; 4]>, ParseError> {
    if !reader.bit("sps conformance")? {
        return Ok(None);
    }
    let left = reader.ue("sps crop left")?;
    let right = reader.ue("sps crop right")?;
    let top = reader.ue("sps crop top")?;
    let bottom = reader.ue("sps crop bottom")?;
    Ok(Some([left, right, top, bottom]))
}

/// Reference sets of one sequence set.
struct SpsReferenceSets {
    /// Short-term reference picture sets.
    short_term: Vec<RefPicSet>,
    /// Long-term pictures from the SPS.
    long_term: Vec<LtSps>,
    /// Long-term pictures may appear in slices.
    long_term_present: bool,
}

/// Sub-layer ordering limits from RBSP bits.
fn parse_sub_layer_ordering(
    reader: &mut BitReader<'_>,
    ordering_info_present: bool,
    max_sub_layers_minus1: u8,
) -> Result<SubLayerOrdering, ParseError> {
    let start = if ordering_info_present {
        0
    } else {
        u32::from(max_sub_layers_minus1)
    };
    let mut ordering = SubLayerOrdering {
        info_present: ordering_info_present,
        ..Default::default()
    };
    for _ in start..=u32::from(max_sub_layers_minus1) {
        ordering.buffering.push(reader.ue("sps buffering")?);
        ordering.reorder.push(reader.ue("sps reorder")?);
        ordering.latency.push(reader.ue("sps latency")?);
    }
    Ok(ordering)
}

/// PCM parameters of one sequence set, when enabled.
fn parse_pcm_params(reader: &mut BitReader<'_>) -> Result<Option<PcmParams>, ParseError> {
    if !reader.bit("sps pcm enabled")? {
        return Ok(None);
    }
    let luma_depth_minus1 = narrow_u8(reader.bits("sps pcm luma", 4)?, "sps pcm luma")?;
    let chroma_depth_minus1 = narrow_u8(reader.bits("sps pcm chroma", 4)?, "sps pcm chroma")?;
    let min_cb_minus3 = reader.ue("sps pcm min cb")?;
    let diff_cb = reader.ue("sps pcm diff cb")?;
    let loop_filter_disabled = reader.bit("sps pcm loop filter")?;
    Ok(Some(PcmParams {
        luma_depth_minus1,
        chroma_depth_minus1,
        min_cb_minus3,
        diff_cb,
        loop_filter_disabled,
    }))
}

/// Short-term and long-term reference sets of one sequence set.
fn parse_sps_ref_sets(reader: &mut BitReader<'_>) -> Result<SpsReferenceSets, ParseError> {
    let num_rps = reader.ue("sps num rps")?;
    if num_rps > 64 {
        return Err(ParseError::Unsupported {
            what: "oversize rps list",
        });
    }
    let mut short_term = Vec::with_capacity(num_rps as usize);
    for idx in 0..num_rps as usize {
        let set = RefPicSet::parse(reader, idx, &short_term, false)?;
        short_term.push(set);
    }
    let mut long_term = Vec::new();
    let long_term_present = reader.bit("sps lt present")?;
    if long_term_present {
        let count = reader.ue("sps num lt")?;
        if count > 32 {
            return Err(ParseError::Unsupported {
                what: "oversize lt list",
            });
        }
        for _ in 0..count {
            let poc_lsb = reader.ue("sps lt poc")?;
            let used = reader.bit("sps lt used")?;
            long_term.push(LtSps { poc_lsb, used });
        }
    }
    Ok(SpsReferenceSets {
        short_term,
        long_term,
        long_term_present,
    })
}

/// VUI block of one sequence set, then the extension guard.
fn parse_sps_vui(
    reader: &mut BitReader<'_>,
    max_sub_layers_minus1: u8,
) -> Result<Option<VuiParams>, ParseError> {
    let mut vui = None;
    if reader.bit("sps vui present")? {
        vui = Some(parse_vui(reader, max_sub_layers_minus1 + 1)?);
    }
    if reader.bit("sps extension present")? {
        return Err(ParseError::Unsupported {
            what: "sps extension",
        });
    }
    Ok(vui)
}

/// VUI parameters.
fn parse_vui(reader: &mut BitReader<'_>, max_sub_layers: u8) -> Result<VuiParams, ParseError> {
    let aspect_present = reader.bit("vui aspect present")?;
    let mut aspect = AspectRatio {
        present: aspect_present,
        ..Default::default()
    };
    if aspect.present {
        aspect.idc = narrow_u8(reader.bits("vui aspect idc", 8)?, "vui aspect idc")?;
        if aspect.idc == 255 {
            aspect.sar_width = narrow_u16(reader.bits("vui sar width", 16)?, "vui sar width")?;
            aspect.sar_height = narrow_u16(reader.bits("vui sar height", 16)?, "vui sar height")?;
        }
    }
    let mut overscan = Overscan {
        present: reader.bit("vui overscan present")?,
        ..Default::default()
    };
    if overscan.present {
        overscan.appropriate = reader.bit("vui overscan")?;
    }
    let mut signal = VideoSignal {
        present: reader.bit("vui signal present")?,
        ..Default::default()
    };
    if signal.present {
        signal.format = narrow_u8(reader.bits("vui format", 3)?, "vui format")?;
        signal.full_range = reader.bit("vui full range")?;
        signal.colour_present = reader.bit("vui colour present")?;
        if signal.colour_present {
            signal.primaries = narrow_u8(reader.bits("vui primaries", 8)?, "vui primaries")?;
            signal.transfer = narrow_u8(reader.bits("vui transfer", 8)?, "vui transfer")?;
            signal.matrix = narrow_u8(reader.bits("vui matrix", 8)?, "vui matrix")?;
        }
    }
    let mut chroma_loc = ChromaLoc {
        present: reader.bit("vui chroma loc present")?,
        ..Default::default()
    };
    if chroma_loc.present {
        chroma_loc.top = reader.ue("vui chroma top")?;
        chroma_loc.bottom = reader.ue("vui chroma bottom")?;
    }
    let neutral_chroma = reader.bit("vui neutral chroma")?;
    let field_seq = reader.bit("vui field seq")?;
    let frame_field_info = reader.bit("vui frame field info")?;
    let mut display_window = None;
    if reader.bit("vui default display present")? {
        let left = reader.ue("vui display left")?;
        let right = reader.ue("vui display right")?;
        let top = reader.ue("vui display top")?;
        let bottom = reader.ue("vui display bottom")?;
        display_window = Some([left, right, top, bottom]);
    }
    let mut timing = VuiTiming {
        present: reader.bit("vui timing present")?,
        ..Default::default()
    };
    let mut hrd = None;
    if timing.present {
        timing.num_units_in_tick = reader.bits("vui tick", 32)?;
        timing.time_scale = reader.bits("vui scale", 32)?;
        timing.proportional = reader.bit("vui poc proportional")?;
        if timing.proportional {
            timing.num_ticks_poc_diff_one_minus1 = reader.ue("vui poc ticks")?;
        }
        if reader.bit("vui hrd present")? {
            hrd = Some(parse_hrd(reader, max_sub_layers, None)?);
        }
    }
    let mut restriction = None;
    if reader.bit("vui bitstream restriction")? {
        restriction = Some(parse_restriction(reader)?);
    }
    Ok(VuiParams {
        aspect,
        overscan,
        signal,
        chroma_loc,
        neutral_chroma,
        field_seq,
        frame_field_info,
        display_window,
        timing,
        hrd,
        restriction,
    })
}

/// Bitstream restriction of one VUI record.
fn parse_restriction(reader: &mut BitReader<'_>) -> Result<BitstreamRestriction, ParseError> {
    Ok(BitstreamRestriction {
        tiles_fixed: reader.bit("vui tiles fixed")?,
        mv_over_pic: reader.bit("vui mv over pic")?,
        restricted_ref: reader.bit("vui restricted ref")?,
        min_spatial_segmentation: narrow_u16(
            reader.ue("vui min segmentation")?,
            "vui min segmentation",
        )?,
        max_bytes_per_pic_denom: narrow_u8(reader.ue("vui max bytes")?, "vui max bytes")?,
        max_bits_per_mcu_denom: narrow_u8(reader.ue("vui max bits")?, "vui max bits")?,
        max_mv_horizontal: narrow_u8(reader.ue("vui max mv h")?, "vui max mv h")?,
        max_mv_vertical: narrow_u8(reader.ue("vui max mv v")?, "vui max mv v")?,
    })
}

/// HRD parameters; `inherit` carries the first set's shared header.
fn parse_hrd(
    reader: &mut BitReader<'_>,
    max_sub_layers: u8,
    inherit: Option<(HrdStreams, bool)>,
) -> Result<HrdParams, ParseError> {
    let common = inherit.is_none();
    let (
        streams,
        sub_pic,
        tick_divisor_minus2,
        bitrate_scale,
        cpb_scale,
        du_cpb_scale,
        initial_delay_minus1,
        au_delay_minus1,
        dpb_delay_minus1,
    );
    if let Some((first_streams, first_sub_pic)) = inherit {
        streams = first_streams;
        sub_pic = first_sub_pic;
        tick_divisor_minus2 = 0;
        bitrate_scale = 0;
        cpb_scale = 0;
        du_cpb_scale = 0;
        initial_delay_minus1 = 0;
        au_delay_minus1 = 0;
        dpb_delay_minus1 = 0;
    } else {
        let mut present = HrdStreams::default();
        if max_sub_layers > 1 {
            present.nal = reader.bit("hrd nal present")?;
            present.vcl = reader.bit("hrd vcl present")?;
        } else {
            present.nal = true;
        }
        streams = present;
        sub_pic = reader.bit("hrd sub pic")?;
        tick_divisor_minus2 = if sub_pic {
            narrow_u8(reader.bits("hrd tick divisor", 8)?, "hrd tick divisor")?
        } else {
            0
        };
        bitrate_scale = narrow_u8(reader.bits("hrd bitrate scale", 4)?, "hrd bitrate scale")?;
        cpb_scale = narrow_u8(reader.bits("hrd cpb scale", 4)?, "hrd cpb scale")?;
        du_cpb_scale = if sub_pic {
            narrow_u8(reader.bits("hrd cpb du scale", 4)?, "hrd cpb du scale")?
        } else {
            0
        };
        initial_delay_minus1 =
            narrow_u8(reader.bits("hrd initial delay", 5)?, "hrd initial delay")?;
        au_delay_minus1 = narrow_u8(reader.bits("hrd au delay", 5)?, "hrd au delay")?;
        dpb_delay_minus1 = narrow_u8(reader.bits("hrd dpb delay", 5)?, "hrd dpb delay")?;
    }
    let mut hrd = HrdParams {
        common,
        streams,
        sub_pic,
        tick_divisor_minus2,
        bitrate_scale,
        cpb_scale,
        du_cpb_scale,
        initial_delay_minus1,
        au_delay_minus1,
        dpb_delay_minus1,
        ..Default::default()
    };
    let layers = max_sub_layers as usize;
    hrd.fixed_rate = vec![false; layers];
    hrd.pic_duration = vec![false; layers];
    hrd.elemental = vec![0; layers];
    hrd.cpb_count_minus1 = vec![0; layers];
    parse_hrd_layer_flags(reader, &mut hrd, layers)?;
    parse_hrd_layer_data(reader, &mut hrd, layers)?;
    Ok(hrd)
}

/// Per-layer rate flags and CPB counts of one HRD set.
fn parse_hrd_layer_flags(
    reader: &mut BitReader<'_>,
    hrd: &mut HrdParams,
    layers: usize,
) -> Result<(), ParseError> {
    for i in 0..layers {
        hrd.fixed_rate[i] = reader.bit("hrd fixed rate")?;
        if hrd.fixed_rate[i] {
            hrd.elemental[i] = narrow_u16(reader.ue("hrd elemental")?, "hrd elemental")?;
        } else {
            hrd.pic_duration[i] = reader.bit("hrd pic duration")?;
        }
        if hrd.pic_duration[i] {
            hrd.elemental[i] = narrow_u16(reader.ue("hrd elemental")?, "hrd elemental")?;
            hrd.cpb_count_minus1[i] = reader.ue("hrd cpb count")?;
            if hrd.cpb_count_minus1[i] > 31 {
                return Err(ParseError::Unsupported {
                    what: "oversize hrd",
                });
            }
        }
    }
    Ok(())
}

/// Per-layer CPB data of one HRD set.
fn parse_hrd_layer_data(
    reader: &mut BitReader<'_>,
    hrd: &mut HrdParams,
    layers: usize,
) -> Result<(), ParseError> {
    for i in 0..layers {
        hrd.nal_layers.push(HrdLayer::default());
        hrd.vcl_layers.push(HrdLayer::default());
        if hrd.fixed_rate[i] {
            continue;
        }
        if hrd.streams.nal {
            parse_hrd_layer(
                reader,
                hrd.sub_pic,
                hrd.cpb_count_minus1[i],
                &mut hrd.nal_layers[i],
            )?;
        }
        if hrd.streams.vcl {
            parse_hrd_layer(
                reader,
                hrd.sub_pic,
                hrd.cpb_count_minus1[i],
                &mut hrd.vcl_layers[i],
            )?;
        }
    }
    Ok(())
}

/// One HRD layer's CPB data.
fn parse_hrd_layer(
    reader: &mut BitReader<'_>,
    sub_pic: bool,
    cpb_count_minus1: u32,
    layer: &mut HrdLayer,
) -> Result<(), ParseError> {
    for _ in 0..=cpb_count_minus1 {
        layer.bitrate_minus1.push(reader.ue("hrd bitrate")?);
        layer.cpb_size_minus1.push(reader.ue("hrd cpb size")?);
        if sub_pic {
            layer.du_cpb_size_minus1.push(reader.ue("hrd du cpb size")?);
            layer.du_bitrate_minus1.push(reader.ue("hrd du bitrate")?);
        }
        layer.cbr.push(reader.bit("hrd cbr")?);
    }
    Ok(())
}

/// Slice-segment header flags of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SliceHeaderFlags {
    /// Dependent slice segments allowed.
    pub(crate) dependent: bool,
    /// Picture output flag in slices.
    pub(crate) output_present: bool,
    /// Reference-list modification in slices.
    pub(crate) lists_modification: bool,
}

/// Intra coding tools of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IntraTools {
    /// Constrained intra prediction.
    pub(crate) constrained_pred: bool,
    /// Transform skip enabled.
    pub(crate) transform_skip: bool,
    /// Sign data hiding enabled.
    pub(crate) sign_hiding: bool,
}

/// QP delta tools of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QpTools {
    /// CU QP delta enabled.
    pub(crate) cu_delta: bool,
    /// CU QP delta depth.
    pub(crate) cu_depth: u32,
    /// Cb QP offset.
    pub(crate) cb_offset: i32,
    /// Cr QP offset.
    pub(crate) cr_offset: i32,
    /// Slice chroma QP offsets present.
    pub(crate) chroma_present: bool,
}

/// Weighted prediction flags of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WeightFlags {
    /// Weighted prediction on P slices.
    pub(crate) pred_p: bool,
    /// Weighted prediction on B slices.
    pub(crate) pred_b: bool,
}

/// Tile layout of one picture set.
#[derive(Clone, Debug, Default)]
pub(crate) struct TileLayout {
    /// Tiles enabled.
    pub(crate) enabled: bool,
    /// Entropy coding sync enabled.
    pub(crate) entropy_sync: bool,
    /// Tile column count minus 1.
    pub(crate) columns_minus1: u32,
    /// Tile row count minus 1.
    pub(crate) rows_minus1: u32,
    /// Uniform tile spacing.
    pub(crate) uniform: bool,
    /// Tile column widths minus 1.
    pub(crate) column_widths: Vec<u32>,
    /// Tile row heights minus 1.
    pub(crate) row_heights: Vec<u32>,
}

/// Loop-filter defaults of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LoopFilter {
    /// Loop filter across tiles enabled.
    pub(crate) across_tiles: bool,
    /// Loop filter across slices enabled.
    pub(crate) across_slices: bool,
}

/// Deblocking defaults of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DeblockDefaults {
    /// Deblocking filter control present.
    pub(crate) control_present: bool,
    /// Deblocking override enabled in slices.
    pub(crate) override_enabled: bool,
    /// Deblocking disabled by default.
    pub(crate) disabled: bool,
    /// Beta offset divided by 2.
    pub(crate) beta: i32,
    /// Tc offset divided by 2.
    pub(crate) tc: i32,
}

/// Picture parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Pps {
    /// Own id.
    id: u8,
    /// Sequence set id.
    sps_id: u8,
    /// Slice-segment header flags.
    header: SliceHeaderFlags,
    /// Extra slice header bits.
    num_extra_slice_header_bits: u8,
    /// CABAC init flag in slices.
    cabac_init_present: bool,
    /// Default active L0 reference count minus 1.
    num_ref_idx_l0_default_minus1: u32,
    /// Default active L1 reference count minus 1.
    num_ref_idx_l1_default_minus1: u32,
    /// Initial QP minus 26.
    init_qp_minus26: i32,
    /// Intra coding tools.
    intra: IntraTools,
    /// QP delta tools.
    qp: QpTools,
    /// Weighted prediction flags.
    weights: WeightFlags,
    /// Transquant bypass enabled.
    transquant_bypass_enabled: bool,
    /// Tile layout.
    tiles: TileLayout,
    /// Loop-filter defaults.
    loop_filter: LoopFilter,
    /// Deblocking defaults.
    deblock: DeblockDefaults,
    /// Scaling lists, when present.
    scaling: Option<ScalingLists>,
    /// Parallel merge level minus 2.
    log2_parallel_merge_level_minus2: u8,
    /// Slice header extension present.
    slice_header_extension_present: bool,
}

impl Pps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Sequence set id.
    #[must_use]
    pub fn sps_id(&self) -> u8 {
        self.sps_id
    }

    /// Picture output flag in slices.
    #[must_use]
    pub fn output_flag_present(&self) -> bool {
        self.header.output_present
    }

    /// Extra slice header bits.
    #[must_use]
    pub fn num_extra_slice_header_bits(&self) -> u8 {
        self.num_extra_slice_header_bits
    }

    /// CABAC init flag in slices.
    #[must_use]
    pub fn cabac_init_present(&self) -> bool {
        self.cabac_init_present
    }

    /// Default active L0 reference count.
    #[must_use]
    pub fn num_ref_idx_l0_default(&self) -> u32 {
        self.num_ref_idx_l0_default_minus1 + 1
    }

    /// Default active L0 count minus 1.
    #[must_use]
    pub fn num_ref_idx_l0_default_minus1(&self) -> u32 {
        self.num_ref_idx_l0_default_minus1
    }

    /// Default active L1 count minus 1.
    #[must_use]
    pub fn num_ref_idx_l1_default_minus1(&self) -> u32 {
        self.num_ref_idx_l1_default_minus1
    }

    /// Default active L1 reference count.
    #[must_use]
    pub fn num_ref_idx_l1_default(&self) -> u32 {
        self.num_ref_idx_l1_default_minus1 + 1
    }

    /// Weighted prediction on P slices.
    #[must_use]
    pub fn weighted_pred(&self) -> bool {
        self.weights.pred_p
    }

    /// Weighted prediction on B slices.
    #[must_use]
    pub fn weighted_bipred(&self) -> bool {
        self.weights.pred_b
    }

    /// Tiles enabled.
    #[must_use]
    pub fn tiles_enabled(&self) -> bool {
        self.tiles.enabled
    }

    /// Entropy coding sync enabled.
    #[must_use]
    pub fn entropy_coding_sync_enabled(&self) -> bool {
        self.tiles.entropy_sync
    }

    /// Deblocking filter control present.
    #[must_use]
    pub fn deblocking_filter_control_present(&self) -> bool {
        self.deblock.control_present
    }

    /// Deblocking override enabled in slices.
    #[must_use]
    pub fn deblocking_filter_override_enabled(&self) -> bool {
        self.deblock.override_enabled
    }

    /// Reference-list modification in slices.
    #[must_use]
    pub fn lists_modification_present(&self) -> bool {
        self.header.lists_modification
    }

    /// Slice header extension present.
    #[must_use]
    pub fn slice_header_extension_present(&self) -> bool {
        self.slice_header_extension_present
    }

    /// Initial QP minus 26.
    #[must_use]
    pub fn init_qp_minus26(&self) -> i32 {
        self.init_qp_minus26
    }

    /// Constrained intra prediction.
    #[must_use]
    pub fn constrained_intra_pred(&self) -> bool {
        self.intra.constrained_pred
    }

    /// Transform skip enabled.
    #[must_use]
    pub fn transform_skip_enabled(&self) -> bool {
        self.intra.transform_skip
    }

    /// CU QP delta enabled.
    #[must_use]
    pub fn cu_qp_delta_enabled(&self) -> bool {
        self.qp.cu_delta
    }

    /// CU QP delta depth.
    #[must_use]
    pub fn diff_cu_qp_delta_depth(&self) -> u32 {
        self.qp.cu_depth
    }

    /// Cb QP offset.
    #[must_use]
    pub fn cb_qp_offset(&self) -> i32 {
        self.qp.cb_offset
    }

    /// Cr QP offset.
    #[must_use]
    pub fn cr_qp_offset(&self) -> i32 {
        self.qp.cr_offset
    }

    /// Slice chroma QP offsets present.
    #[must_use]
    pub fn slice_chroma_qp_offsets_present(&self) -> bool {
        self.qp.chroma_present
    }

    /// Transquant bypass enabled.
    #[must_use]
    pub fn transquant_bypass_enabled(&self) -> bool {
        self.transquant_bypass_enabled
    }

    /// Tile column count minus 1.
    #[must_use]
    pub fn num_tile_columns_minus1(&self) -> u32 {
        self.tiles.columns_minus1
    }

    /// Tile row count minus 1.
    #[must_use]
    pub fn num_tile_rows_minus1(&self) -> u32 {
        self.tiles.rows_minus1
    }

    /// Uniform tile spacing.
    #[must_use]
    pub fn uniform_spacing(&self) -> bool {
        self.tiles.uniform
    }

    /// Tile column widths minus 1.
    #[must_use]
    pub fn column_widths(&self) -> &[u32] {
        &self.tiles.column_widths
    }

    /// Tile row heights minus 1.
    #[must_use]
    pub fn row_heights(&self) -> &[u32] {
        &self.tiles.row_heights
    }

    /// Loop filter across tiles enabled.
    #[must_use]
    pub fn loop_filter_across_tiles(&self) -> bool {
        self.loop_filter.across_tiles
    }

    /// Loop filter across slices enabled.
    #[must_use]
    pub fn loop_filter_across_slices(&self) -> bool {
        self.loop_filter.across_slices
    }

    /// Deblocking disabled by default.
    #[must_use]
    pub fn deblocking_filter_disabled(&self) -> bool {
        self.deblock.disabled
    }

    /// Beta offset divided by 2.
    #[must_use]
    pub fn beta_offset_div2(&self) -> i32 {
        self.deblock.beta
    }

    /// Tc offset divided by 2.
    #[must_use]
    pub fn tc_offset_div2(&self) -> i32 {
        self.deblock.tc
    }

    /// Scaling lists, when present.
    #[must_use]
    pub fn scaling(&self) -> Option<&ScalingLists> {
        self.scaling.as_ref()
    }

    /// Parallel merge level minus 2.
    #[must_use]
    pub fn log2_parallel_merge_level_minus2(&self) -> u8 {
        self.log2_parallel_merge_level_minus2
    }

    /// Dependent slice segments allowed.
    #[must_use]
    pub fn dependent_slices_enabled(&self) -> bool {
        self.header.dependent
    }

    /// Sign data hiding enabled.
    #[must_use]
    pub fn sign_data_hiding_enabled(&self) -> bool {
        self.intra.sign_hiding
    }

    /// Picture set from RBSP bytes.
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ParseError> {
        let pps_id = narrow_u8(reader.ue("pps id")?, "pps id")?;
        let sps_id = narrow_u8(reader.ue("pps sps id")?, "pps sps id")?;
        let dependent_slices_enabled = reader.bit("pps dependent slices")?;
        let output_flag_present = reader.bit("pps output flag")?;
        let num_extra_slice_header_bits =
            narrow_u8(reader.bits("pps extra bits", 3)?, "pps extra bits")?;
        let sign_data_hiding_enabled = reader.bit("pps sign hiding")?;
        let cabac_init_present = reader.bit("pps cabac init")?;
        let num_ref_idx_l0_default_minus1 = reader.ue("pps l0 default")?;
        let num_ref_idx_l1_default_minus1 = reader.ue("pps l1 default")?;
        let init_qp_minus26 = reader.se("pps init qp")?;
        let constrained_intra_pred = reader.bit("pps constrained intra")?;
        let transform_skip_enabled = reader.bit("pps transform skip")?;
        let qp = parse_qp_tools(reader)?;
        let weighted_pred = reader.bit("pps weighted pred")?;
        let weighted_bipred = reader.bit("pps weighted bipred")?;
        let transquant_bypass_enabled = reader.bit("pps transquant bypass")?;
        let tiles = parse_tiles(reader)?;
        let across_tiles = if tiles.enabled {
            reader.bit("pps loop across tiles")?
        } else {
            false
        };
        let loop_filter_across_slices = reader.bit("pps loop across slices")?;
        let deblock = parse_deblock(reader)?;
        let mut scaling = None;
        if reader.bit("pps scaling present")? {
            scaling = Some(ScalingLists::parse(reader)?);
        }
        let lists_modification_present = reader.bit("pps list mods")?;
        let log2_parallel_merge_level_minus2 =
            narrow_u8(reader.ue("pps merge level")?, "pps merge level")?;
        let slice_header_extension_present = reader.bit("pps slice extension")?;
        if reader.bit("pps extension present")? {
            return Err(ParseError::Unsupported {
                what: "pps extension",
            });
        }
        Ok(Self {
            id: pps_id,
            sps_id,
            header: SliceHeaderFlags {
                dependent: dependent_slices_enabled,
                output_present: output_flag_present,
                lists_modification: lists_modification_present,
            },
            num_extra_slice_header_bits,
            cabac_init_present,
            num_ref_idx_l0_default_minus1,
            num_ref_idx_l1_default_minus1,
            init_qp_minus26,
            intra: IntraTools {
                constrained_pred: constrained_intra_pred,
                transform_skip: transform_skip_enabled,
                sign_hiding: sign_data_hiding_enabled,
            },
            qp,
            weights: WeightFlags {
                pred_p: weighted_pred,
                pred_b: weighted_bipred,
            },
            transquant_bypass_enabled,
            tiles,
            loop_filter: LoopFilter {
                across_tiles,
                across_slices: loop_filter_across_slices,
            },
            deblock,
            scaling,
            log2_parallel_merge_level_minus2,
            slice_header_extension_present,
        })
    }
}

/// QP delta tools of one picture set from RBSP bits.
fn parse_qp_tools(reader: &mut BitReader<'_>) -> Result<QpTools, ParseError> {
    let cu_delta = reader.bit("pps cu qp delta")?;
    let cu_depth = if cu_delta {
        reader.ue("pps cu qp depth")?
    } else {
        0
    };
    let cb_offset = reader.se("pps cb qp")?;
    let cr_offset = reader.se("pps cr qp")?;
    let chroma_present = reader.bit("pps chroma qp offsets")?;
    Ok(QpTools {
        cu_delta,
        cu_depth,
        cb_offset,
        cr_offset,
        chroma_present,
    })
}

/// Tile layout of one picture set from RBSP bits.
fn parse_tiles(reader: &mut BitReader<'_>) -> Result<TileLayout, ParseError> {
    let enabled = reader.bit("pps tiles")?;
    let entropy_sync = reader.bit("pps entropy sync")?;
    let mut tiles = TileLayout {
        enabled,
        entropy_sync,
        ..Default::default()
    };
    if enabled {
        tiles.columns_minus1 = reader.ue("pps tile columns")?;
        tiles.rows_minus1 = reader.ue("pps tile rows")?;
        if tiles.columns_minus1 > 19 || tiles.rows_minus1 > 21 {
            return Err(ParseError::Unsupported {
                what: "oversize tiles",
            });
        }
        tiles.uniform = reader.bit("pps uniform spacing")?;
        if !tiles.uniform {
            for _ in 0..tiles.columns_minus1 {
                tiles.column_widths.push(reader.ue("pps column width")?);
            }
            for _ in 0..tiles.rows_minus1 {
                tiles.row_heights.push(reader.ue("pps row height")?);
            }
        }
    }
    Ok(tiles)
}

/// Deblocking defaults of one picture set from RBSP bits.
fn parse_deblock(reader: &mut BitReader<'_>) -> Result<DeblockDefaults, ParseError> {
    let control_present = reader.bit("pps deblocking")?;
    let mut deblock = DeblockDefaults {
        control_present,
        ..Default::default()
    };
    if control_present {
        deblock.override_enabled = reader.bit("pps deblocking override")?;
        deblock.disabled = reader.bit("pps deblocking disabled")?;
        if !deblock.disabled {
            deblock.beta = reader.se("pps beta")?;
            deblock.tc = reader.se("pps tc")?;
        }
    }
    Ok(deblock)
}

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SliceType {
    /// Bi-predictive slice.
    B,
    /// Predictive slice.
    P,
    /// Intra slice.
    I,
}

impl SliceType {
    /// Type from the raw `slice_type` idc.
    fn from_idc(idc: u32) -> Option<Self> {
        match idc {
            0 => Some(Self::B),
            1 => Some(Self::P),
            2 => Some(Self::I),
            _ => None,
        }
    }
}

/// Long-term reference picture of one slice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LtSlice {
    /// POC least significant bits.
    poc_lsb: u32,
    /// POC MSB cycle offset.
    msb_cycle: i32,
    /// MSB cycle signaled in the slice.
    msb_cycle_present: bool,
    /// Used by the current picture.
    used: bool,
}

impl LtSlice {
    /// POC least significant bits.
    #[must_use]
    pub fn poc_lsb(&self) -> u32 {
        self.poc_lsb
    }

    /// POC MSB cycle offset.
    #[must_use]
    pub fn msb_cycle(&self) -> i32 {
        self.msb_cycle
    }

    /// MSB cycle signaled in the slice.
    #[must_use]
    pub fn msb_cycle_present(&self) -> bool {
        self.msb_cycle_present
    }

    /// Used by the current picture.
    #[must_use]
    pub fn used(&self) -> bool {
        self.used
    }
}

/// Current picture's reference set resolved from the slice.
#[derive(Clone, Debug, Default)]
pub(crate) struct SliceRps {
    /// Short-term POC deltas with used flags.
    short: Vec<(i32, bool)>,
    /// Long-term pictures.
    long: Vec<LtSlice>,
}

impl SliceRps {
    /// Short-term POC deltas with used flags.
    #[must_use]
    pub fn short(&self) -> &[(i32, bool)] {
        &self.short
    }

    /// Long-term pictures.
    #[must_use]
    pub fn long(&self) -> &[LtSlice] {
        &self.long
    }
}

/// Slice header of one H.265 slice: identity, POC and reference sets.
///
/// The hardware parses slice bodies, so the header stops after reference
/// lists; later syntax only advances the bit reader.
#[derive(Clone, Debug)]
pub(crate) struct SliceHeader {
    /// First segment of the picture.
    first_slice_in_pic: bool,
    /// IRAP pictures drop prior pictures from output.
    no_output_of_prior_pics: bool,
    /// Slice coding type.
    slice_type: SliceType,
    /// POC least significant bits, absent on IDR.
    poc_lsb: Option<u32>,
    /// Short-term set comes from the SPS.
    rps_sps_flag: bool,
    /// Delta pictures of the predicted-from set.
    rps_ref_count: u8,
    /// Syntax bit length of an explicit short-term set.
    rps_bits: u16,
    /// Current picture's reference set.
    rps: SliceRps,
}

impl SliceHeader {
    /// First segment of the picture.
    #[must_use]
    pub fn first_slice_in_pic(&self) -> bool {
        self.first_slice_in_pic
    }

    /// IRAP pictures drop prior pictures from output.
    #[must_use]
    pub fn no_output_of_prior_pics(&self) -> bool {
        self.no_output_of_prior_pics
    }

    /// Slice coding type.
    #[must_use]
    pub fn slice_type(&self) -> SliceType {
        self.slice_type
    }

    /// POC least significant bits, absent on IDR.
    #[must_use]
    pub fn poc_lsb(&self) -> Option<u32> {
        self.poc_lsb
    }

    /// Short-term set comes from the SPS.
    #[must_use]
    pub fn rps_sps_flag(&self) -> bool {
        self.rps_sps_flag
    }

    /// Delta pictures of the predicted-from set.
    #[must_use]
    pub fn rps_ref_count(&self) -> u8 {
        self.rps_ref_count
    }

    /// Syntax bit length of an explicit short-term set.
    #[must_use]
    pub fn rps_bits(&self) -> u16 {
        self.rps_bits
    }

    /// Current picture's reference set.
    #[must_use]
    pub fn rps(&self) -> &SliceRps {
        &self.rps
    }
}

/// Coded slice with its Annex-B bytes for the hardware.
#[derive(Clone, Debug)]
pub(crate) struct Slice {
    /// NAL header of the slice.
    nal: NalHeader,
    /// Parsed slice header.
    header: SliceHeader,
    /// Active video set.
    vps: Rc<Vps>,
    /// Active sequence set.
    sps: Rc<Sps>,
    /// Active picture set.
    pps: Rc<Pps>,
    /// Start code plus NAL as coded.
    bytes: Vec<u8>,
}

impl Slice {
    /// NAL header of the slice.
    #[must_use]
    pub fn nal(&self) -> NalHeader {
        self.nal
    }

    /// Parsed slice header.
    #[must_use]
    pub fn header(&self) -> &SliceHeader {
        &self.header
    }

    /// Active video set.
    #[must_use]
    pub fn vps(&self) -> &Rc<Vps> {
        &self.vps
    }

    /// Active sequence set.
    #[must_use]
    pub fn sps(&self) -> &Rc<Sps> {
        &self.sps
    }

    /// Active picture set.
    #[must_use]
    pub fn pps(&self) -> &Rc<Pps> {
        &self.pps
    }

    /// NAL header plus payload with the start code cut.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        if self.bytes.len() >= 4 && self.bytes[2] == 0 {
            &self.bytes[4..]
        } else if self.bytes.len() >= 3 {
            &self.bytes[3..]
        } else {
            &[]
        }
    }
}

/// One parsed NAL unit.
#[derive(Debug)]
pub(crate) enum ParsedNalu {
    /// Video parameter set.
    Vps(Rc<Vps>),
    /// Sequence parameter set.
    Sps(Rc<Sps>),
    /// Picture parameter set.
    Pps(Rc<Pps>),
    /// Coded slice.
    Slice(Box<Slice>),
    /// Anything the tier ignores.
    Other,
}

/// Parsed NAL unit.
#[derive(Debug)]
pub(crate) struct Nalu {
    /// Parsed payload.
    parsed: ParsedNalu,
}

impl Nalu {
    /// Unit with `parsed` payload.
    #[must_use]
    pub fn new(parsed: ParsedNalu) -> Self {
        Self { parsed }
    }

    /// Parsed payload.
    #[must_use]
    pub fn parsed(&self) -> &ParsedNalu {
        &self.parsed
    }
}

/// H.265 NAL parser with parameter-set tables.
pub(crate) struct H265Parser {
    /// Video sets by id.
    vps: HashMap<u8, Rc<Vps>>,
    /// Sequence sets by id.
    sps: HashMap<u8, Rc<Sps>>,
    /// Picture sets by id.
    pps: HashMap<u8, Rc<Pps>>,
}

impl H265Parser {
    /// Parser with empty tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vps: HashMap::new(),
            sps: HashMap::new(),
            pps: HashMap::new(),
        }
    }

    /// Parse one Annex-B NAL unit.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncated or unsupported syntax.
    pub fn parse(&mut self, nalu: &RawNalu) -> Result<ParsedNalu, ParseError> {
        let payload = nalu.payload();
        if payload.len() < 2 {
            return Err(ParseError::Truncated {
                syntax: "nal header",
            });
        }
        let header = NalHeader {
            unit_type: NalType((payload[0] >> 1) & 0x3F),
            temporal_id: (payload[1] & 0x07).saturating_sub(1),
        };
        if header.unit_type.id() == 32 {
            let rbsp = Rbsp::strip(&payload[2..]);
            let mut reader = BitReader::new(rbsp.bytes());
            let vps = Rc::new(Vps::parse(&mut reader)?);
            self.vps.insert(vps.id(), Rc::clone(&vps));
            return Ok(ParsedNalu::Vps(vps));
        }
        if header.unit_type.id() == 33 {
            let rbsp = Rbsp::strip(&payload[2..]);
            let mut reader = BitReader::new(rbsp.bytes());
            let sps = Rc::new(Sps::parse(&mut reader)?);
            self.sps.insert(sps.id(), Rc::clone(&sps));
            return Ok(ParsedNalu::Sps(sps));
        }
        if header.unit_type.id() == 34 {
            let rbsp = Rbsp::strip(&payload[2..]);
            let mut reader = BitReader::new(rbsp.bytes());
            let pps = Rc::new(Pps::parse(&mut reader)?);
            self.pps.insert(pps.id(), Rc::clone(&pps));
            return Ok(ParsedNalu::Pps(pps));
        }
        if header.unit_type.is_slice() {
            let rbsp = Rbsp::strip(&payload[2..]);
            let mut reader = BitReader::new(rbsp.bytes());
            let slice = self.parse_slice(&mut reader, header, nalu.bytes())?;
            return Ok(ParsedNalu::Slice(Box::new(slice)));
        }
        Ok(ParsedNalu::Other)
    }

    /// Parse a slice header against the stored tables.
    fn parse_slice(
        &self,
        reader: &mut BitReader<'_>,
        nal: NalHeader,
        bytes: &[u8],
    ) -> Result<Slice, ParseError> {
        let first_slice_in_pic = reader.bit("slice first in pic")?;
        let mut no_output_of_prior_pics = false;
        if nal.unit_type().is_irap() {
            no_output_of_prior_pics = reader.bit("slice no output prior")?;
        }
        let pps_id = narrow_u8(reader.ue("slice pps id")?, "slice pps id")?;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps_id),
            })?;
        if pps.dependent_slices_enabled() && !first_slice_in_pic && reader.bit("slice dependent")? {
            return Err(ParseError::Unsupported {
                what: "dependent slices",
            });
        }
        if !first_slice_in_pic {
            reader.ue("slice address")?;
        }
        let raw_type = reader.ue("slice type")?;
        let slice_type = SliceType::from_idc(raw_type).ok_or(ParseError::OutOfRange {
            syntax: "slice type",
        })?;
        if pps.output_flag_present() {
            reader.bit("slice output flag")?;
        }
        if pps.sps_id() > 15 {
            return Err(ParseError::OutOfRange {
                syntax: "slice sps id",
            });
        }
        let sps = self
            .sps
            .get(&pps.sps_id())
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps.sps_id()),
            })?;
        let vps = self
            .vps
            .get(&sps.vps_id())
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(sps.vps_id()),
            })?;
        if sps.separate_colour_plane() {
            reader.bits("slice colour plane", 2)?;
        }
        let mut poc_lsb = None;
        if !nal.unit_type().is_idr() {
            poc_lsb = Some(reader.bits("slice poc lsb", sps.pic_order_cnt_lsb_bits())?);
        }
        let (rps, short) = parse_slice_rps(reader, &sps, nal.unit_type())?;
        let mut temporal_mvp = false;
        if sps.temporal_mvp_enabled() {
            temporal_mvp = reader.bit("slice temporal mvp")?;
        }
        reader.skip(
            "slice extra bits",
            u32::from(pps.num_extra_slice_header_bits()),
        )?;
        let filter = parse_slice_inter(reader, &pps, &sps, slice_type, &rps, temporal_mvp)?;
        parse_slice_tail(reader, &pps, slice_type, &filter)?;
        Ok(Slice {
            nal,
            header: SliceHeader {
                first_slice_in_pic,
                no_output_of_prior_pics,
                slice_type,
                poc_lsb,
                rps_sps_flag: short.sps_flag,
                rps_ref_count: short.ref_count,
                rps_bits: short.bits,
                rps,
            },
            vps,
            sps,
            pps,
            bytes: bytes.to_vec(),
        })
    }
}

/// Loop-filter inputs of one non-intra slice.
struct SliceFilter {
    /// Luma or chroma SAO active.
    sao: bool,
    /// Deblocking filter disabled.
    deblocking_disabled: bool,
}

/// Reference lists, SAO, collocated and weights of one non-intra slice.
fn parse_slice_inter(
    reader: &mut BitReader<'_>,
    pps: &Pps,
    sps: &Sps,
    slice_type: SliceType,
    rps: &SliceRps,
    temporal_mvp: bool,
) -> Result<SliceFilter, ParseError> {
    let mut filter = SliceFilter {
        sao: false,
        deblocking_disabled: pps.deblocking_filter_disabled(),
    };
    if slice_type == SliceType::I {
        return Ok(filter);
    }
    let mut num_ref_idx_l0_active = pps.num_ref_idx_l0_default();
    let mut num_ref_idx_l1_active = pps.num_ref_idx_l1_default();
    if reader.bit("slice ref override")? {
        num_ref_idx_l0_active = reader.ue("slice l0 active")? + 1;
        if slice_type == SliceType::B {
            num_ref_idx_l1_active = reader.ue("slice l1 active")? + 1;
        }
    }
    if pps.lists_modification_present() {
        let total = rps.short.len() + rps.long.len();
        if total > 0 {
            parse_ref_list(reader, num_ref_idx_l0_active)?;
            if slice_type == SliceType::B {
                parse_ref_list(reader, num_ref_idx_l1_active)?;
            }
        }
    }
    if sps.sao_enabled() {
        let sao_luma = reader.bit("slice sao luma")?;
        let sao_chroma = reader.bit("slice sao chroma")?;
        filter.sao = sao_luma || sao_chroma;
    }
    if slice_type == SliceType::B {
        reader.bit("slice mvd l1 zero")?;
    }
    if pps.cabac_init_present() {
        reader.bit("slice cabac init")?;
    }
    skip_collocated(
        reader,
        temporal_mvp,
        slice_type,
        num_ref_idx_l0_active,
        num_ref_idx_l1_active,
    )?;
    if (pps.weighted_pred() && slice_type == SliceType::P)
        || (pps.weighted_bipred() && slice_type == SliceType::B)
    {
        skip_pred_weights(
            reader,
            sps,
            slice_type,
            num_ref_idx_l0_active,
            num_ref_idx_l1_active,
        )?;
    }
    reader.ue("slice merge cands")?;
    Ok(filter)
}

/// QP deltas, deblocking, loop filter and entry points of one slice.
fn parse_slice_tail(
    reader: &mut BitReader<'_>,
    pps: &Pps,
    slice_type: SliceType,
    filter: &SliceFilter,
) -> Result<(), ParseError> {
    reader.se("slice qp delta")?;
    if pps.slice_chroma_qp_offsets_present() {
        reader.se("slice cb qp offset")?;
        reader.se("slice cr qp offset")?;
    }
    let mut deblocking_disabled = filter.deblocking_disabled;
    if slice_type != SliceType::I
        && pps.deblocking_filter_override_enabled()
        && reader.bit("slice deblocking override")?
    {
        deblocking_disabled = reader.bit("slice deblocking disabled")?;
        if !deblocking_disabled {
            reader.se("slice beta")?;
            reader.se("slice tc")?;
        }
    }
    if pps.loop_filter_across_slices() && (filter.sao || !deblocking_disabled) {
        reader.bit("slice loop across slices")?;
    }
    if pps.tiles_enabled() || pps.entropy_coding_sync_enabled() {
        skip_entry_offsets(reader)?;
    }
    if pps.slice_header_extension_present() {
        skip_slice_extension(reader)?;
    }
    Ok(())
}

/// Entry-point offsets of one slice, consumed without storing.
fn skip_entry_offsets(reader: &mut BitReader<'_>) -> Result<(), ParseError> {
    let offsets = reader.ue("slice entry offsets")?;
    if offsets == 0 {
        return Ok(());
    }
    if offsets > 256 {
        return Err(ParseError::Unsupported {
            what: "oversize entry points",
        });
    }
    let width = reader.ue("slice offset len")? + 1;
    if width > 32 {
        return Err(ParseError::OutOfRange {
            syntax: "slice offset len",
        });
    }
    for _ in 0..offsets {
        reader.skip("slice entry offset", width)?;
    }
    Ok(())
}

/// Extension bytes of one slice header, consumed without storing.
fn skip_slice_extension(reader: &mut BitReader<'_>) -> Result<(), ParseError> {
    let length = reader.ue("slice extension length")?;
    if length > 256 {
        return Err(ParseError::Unsupported {
            what: "oversize slice extension",
        });
    }
    for _ in 0..length {
        reader.skip("slice extension byte", 8)?;
    }
    Ok(())
}

impl Default for H265Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// Short-term set of one slice with its driver echo.
struct ShortRps {
    /// POC deltas with used flags.
    deltas: Vec<(i32, bool)>,
    /// Set comes from the SPS rather than the slice.
    sps_flag: bool,
    /// Delta pictures of the predicted-from set.
    ref_count: u8,
    /// Syntax bit length of an explicit set.
    bits: u16,
}

/// Short-term reference set from the slice header.
fn parse_short_rps(reader: &mut BitReader<'_>, sps: &Sps) -> Result<ShortRps, ParseError> {
    let sps_flag = if sps.rps_sets().is_empty() {
        false
    } else {
        reader.bit("slice rps sps flag")?
    };
    let before = reader.consumed();
    let (deltas, ref_count) = parse_short_set(reader, sps, sps_flag)?;
    let bits = u16::try_from(reader.consumed() - before).unwrap_or(u16::MAX);
    Ok(ShortRps {
        deltas,
        sps_flag,
        ref_count,
        bits: if sps_flag { 0 } else { bits },
    })
}

/// Short-term deltas from the SPS table or explicit syntax.
fn parse_short_set(
    reader: &mut BitReader<'_>,
    sps: &Sps,
    sps_flag: bool,
) -> Result<(Vec<(i32, bool)>, u8), ParseError> {
    if sps_flag {
        let count = sps.rps_sets().len();
        if count == 0 {
            return Err(ParseError::Unsupported {
                what: "empty sps rps",
            });
        }
        let index = if count == 1 {
            0
        } else {
            let count = u32::try_from(count).map_err(|_| ParseError::OutOfRange {
                syntax: "slice rps idx",
            })?;
            let width = 32 - (count - 1).leading_zeros();
            let raw = reader.bits("slice rps idx", width)?;
            usize::try_from(raw).map_err(|_| ParseError::OutOfRange {
                syntax: "slice rps idx",
            })?
        };
        let set = sps.rps_sets().get(index).ok_or(ParseError::OutOfRange {
            syntax: "slice rps idx",
        })?;
        let mut deltas = Vec::new();
        deltas.extend(set.s0.iter().copied());
        deltas.extend(set.s1.iter().copied());
        return Ok((deltas, 0));
    }
    let set = RefPicSet::parse(reader, sps.rps_sets().len(), sps.rps_sets(), true)?;
    let mut deltas = Vec::new();
    deltas.extend(set.s0.iter().copied());
    deltas.extend(set.s1.iter().copied());
    Ok((deltas, set.ref_pics()))
}

/// Current picture's reference set from the slice header.
fn parse_slice_rps(
    reader: &mut BitReader<'_>,
    sps: &Sps,
    nal_type: NalType,
) -> Result<(SliceRps, ShortRps), ParseError> {
    let mut rps = SliceRps::default();
    if nal_type.is_idr() {
        let empty = ShortRps {
            deltas: Vec::new(),
            sps_flag: false,
            ref_count: 0,
            bits: 0,
        };
        return Ok((rps, empty));
    }
    let short = parse_short_rps(reader, sps)?;
    rps.short.extend(short.deltas.iter().copied());
    if !sps.lt_present() {
        return Ok((rps, short));
    }
    let num_sps = reader.ue("slice num lt sps")?;
    let num_pics = reader.ue("slice num lt pics")?;
    if num_sps + num_pics > 16 {
        return Err(ParseError::Unsupported {
            what: "oversize lt list",
        });
    }
    let sps_lts = usize::try_from(num_sps).map_err(|_| ParseError::OutOfRange {
        syntax: "slice num lt sps",
    })?;
    if sps_lts > sps.lt_sps().len() {
        return Err(ParseError::OutOfRange {
            syntax: "slice num lt sps",
        });
    }
    for entry in sps.lt_sps().iter().take(sps_lts) {
        let used = reader.bit("slice lt used")?;
        let msb_present = reader.bit("slice lt msb present")?;
        let mut msb_cycle = 0;
        let mut msb_cycle_present = false;
        if msb_present {
            msb_cycle = reader.se("slice lt msb")?;
            msb_cycle_present = true;
        }
        rps.long.push(LtSlice {
            poc_lsb: entry.poc_lsb,
            msb_cycle,
            msb_cycle_present,
            used,
        });
    }
    let bits = sps.pic_order_cnt_lsb_bits();
    for _ in 0..num_pics {
        let poc_lsb = reader.bits("slice lt poc", bits)?;
        let used = reader.bit("slice lt used")?;
        let msb_present = reader.bit("slice lt msb present")?;
        let mut msb_cycle = 0;
        let mut msb_cycle_present = false;
        if msb_present {
            msb_cycle = reader.se("slice lt msb")?;
            msb_cycle_present = true;
        }
        rps.long.push(LtSlice {
            poc_lsb,
            msb_cycle,
            msb_cycle_present,
            used,
        });
    }
    Ok((rps, short))
}

/// One reference-list modification: flag plus entries.
fn parse_ref_list(reader: &mut BitReader<'_>, active: u32) -> Result<Vec<u32>, ParseError> {
    if active > 16 {
        return Err(ParseError::OutOfRange {
            syntax: "slice ref active",
        });
    }
    if !reader.bit("slice list mod flag")? {
        return Ok(Vec::new());
    }
    let mut entries = Vec::with_capacity(active as usize);
    for _ in 0..active {
        entries.push(reader.ue("slice list entry")?);
    }
    Ok(entries)
}

/// Collocated picture syntax, skipped.
/// Collocated picture of the slice, absent without temporal prediction.
/// Collocated-picture syntax of one slice, consumed without storing.
fn skip_collocated(
    reader: &mut BitReader<'_>,
    temporal_mvp: bool,
    slice_type: SliceType,
    l0_active: u32,
    l1_active: u32,
) -> Result<(), ParseError> {
    if !temporal_mvp {
        return Ok(());
    }
    let mut from_l0 = true;
    if slice_type == SliceType::B {
        from_l0 = reader.bit("slice collocated from l0")?;
    }
    if (from_l0 && l0_active > 1) || (!from_l0 && l1_active > 1) {
        reader.ue("slice collocated idx")?;
    }
    Ok(())
}

/// Prediction weight table of one slice, consumed without storing.
fn skip_pred_weights(
    reader: &mut BitReader<'_>,
    sps: &Sps,
    slice_type: SliceType,
    l0_active: u32,
    l1_active: u32,
) -> Result<(), ParseError> {
    reader.ue("weights luma denom")?;
    let chroma = sps.chroma_format_idc() != 0;
    if chroma {
        reader.se("weights chroma denom")?;
    }
    let lists = if slice_type == SliceType::B { 2 } else { 1 };
    for list in 0..lists {
        let active = if list == 0 { l0_active } else { l1_active };
        if active > 16 {
            return Err(ParseError::OutOfRange {
                syntax: "weights active",
            });
        }
        for _ in 0..active {
            if reader.bit("weights luma flag")? {
                reader.se("weights luma delta")?;
                reader.se("weights luma offset")?;
            }
            if chroma && reader.bit("weights chroma flag")? {
                for _ in 0..2 {
                    reader.se("weights chroma delta")?;
                    reader.se("weights chroma offset")?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{H265Parser, ParsedNalu, SliceType};
    use crate::parse::test_support::{hex, split};

    /// First NAL units of the fork `demo.h265` vector.
    const DEMO_VPS: &str = "40010c01ffff01600000030090000003000003003c92809000";
    /// First SPS of the fork `demo.h265` vector.
    const DEMO_SPS: &str =
        "42010101600000030090000003000003003ca00a080f16592a4932bc05a0200000030020000003032100";
    /// First PPS of the fork `demo.h265` vector.
    const DEMO_PPS: &str = "4401c172b4624000";
    /// Head of the first slice of the fork `demo.h265` vector.
    const DEMO_SLICE: &str =
        "2801af1d20a965478be0af3f4ae63151b2fffd273e7fa20b405b9ec15d0337a20427f6e32f9f583c57fee84d42429d33ffe3ff4aff62991873852f99fb8bf1a358a444ab906f2a97e66aa67a1c";

    #[test]
    fn demo_sps_matches_ffprobe() {
        let units = split(&[hex(DEMO_VPS), hex(DEMO_SPS)]);
        let mut parser = H265Parser::new();
        parser.parse(&units[0]).expect("vps");
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        assert_eq!(sps.ptl().profile_idc(), 1);
        assert_eq!(sps.coded_size(), (320, 240));
        assert_eq!(sps.display_size(), (320, 240));
        assert_eq!(sps.chroma_format_idc(), 1);
    }

    #[test]
    fn demo_pps_resolves_its_sps() {
        let units = split(&[hex(DEMO_VPS), hex(DEMO_SPS), hex(DEMO_PPS)]);
        let mut parser = H265Parser::default();
        parser.parse(&units[0]).expect("vps");
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        let ParsedNalu::Pps(pps) = parser.parse(&units[2]).expect("pps") else {
            panic!("want pps");
        };
        assert_eq!(pps.sps_id(), sps.id());
    }

    #[test]
    fn demo_slice_parses_against_tables() {
        let units = split(&[hex(DEMO_VPS), hex(DEMO_SPS), hex(DEMO_PPS), hex(DEMO_SLICE)]);
        let mut parser = H265Parser::new();
        parser.parse(&units[0]).expect("vps");
        parser.parse(&units[1]).expect("sps");
        parser.parse(&units[2]).expect("pps");
        let ParsedNalu::Slice(slice) = parser.parse(&units[3]).expect("slice") else {
            panic!("want slice");
        };
        assert!(slice.nal().unit_type().is_idr());
        assert_eq!(slice.header().slice_type(), SliceType::I);
        assert!(slice.header().first_slice_in_pic());
        assert!(slice.header().poc_lsb().is_none());
        assert_eq!(slice.nal().temporal_id(), 0);
    }

    #[test]
    fn truncated_vps_errors() {
        let units = split(&[vec![0x40, 0x01]]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::Truncated { .. }
        ));
    }

    #[test]
    fn slice_before_tables_errors() {
        let units = split(&[hex(DEMO_SLICE)]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::MissingSet { .. }
        ));
    }

    #[test]
    fn sei_parses_as_other() {
        let units = split(&[vec![0x4E, 0x01, 0x05, 0xFF]]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).expect("sei"),
            ParsedNalu::Other
        ));
    }

    #[test]
    fn explicit_rps_round_trips_through_std() {
        use super::RefPicSet;
        use crate::parse::bits::BitReader;
        // negative=2 (deltas 0,1; used 1,0), positive=1 (delta 2, used 1).
        let bytes = [0x6B, 0x47];
        let mut reader = BitReader::new(&bytes);
        let set = RefPicSet::parse(&mut reader, 0, &[], false).expect("rps");
        assert_eq!(set.s0(), &[(-1, true), (-3, false)]);
        assert_eq!(set.s1(), &[(3, true)]);
        let std = crate::session::std_h265::fill_rps(&set).expect("fill");
        assert_eq!(std.flags.inter_ref_pic_set_prediction_flag(), 0);
        assert_eq!(std.num_negative_pics, 2);
        assert_eq!(std.num_positive_pics, 1);
        assert_eq!(&std.delta_poc_s0_minus1[..2], &[0, 1]);
        assert_eq!(&std.delta_poc_s1_minus1[..1], &[2]);
        assert_eq!(std.used_by_curr_pic_s0_flag, 0b01);
        assert_eq!(std.used_by_curr_pic_s1_flag, 0b1);
        assert_eq!(std.used_by_curr_pic_flag, 0b101);
        assert_eq!(std.use_delta_flag, 0b111);
    }

    #[test]
    fn inter_rps_keeps_raw_prediction_syntax() {
        use super::RefPicSet;
        use crate::parse::bits::BitReader;
        let bytes = [0x6B, 0x47];
        let mut reader = BitReader::new(&bytes);
        let first = RefPicSet::parse(&mut reader, 0, &[], false).expect("rps");
        // Inter, sign 0, abs 0, used 1/0/1 with the middle kept.
        let bytes = [0xB7];
        let mut reader = BitReader::new(&bytes);
        let sets = [first];
        let set = RefPicSet::parse(&mut reader, 1, &sets, false).expect("inter");
        assert!(set.inter());
        assert_eq!(set.ref_pics(), 3);
        assert_eq!(set.used_mask(), 0b101);
        assert_eq!(set.use_mask(), 0b111);
        assert_eq!(set.s0(), &[(-2, false)]);
        assert_eq!(set.s1(), &[(4, true)]);
        let std = crate::session::std_h265::fill_rps(&set).expect("fill");
        assert_eq!(std.flags.inter_ref_pic_set_prediction_flag(), 1);
        assert_eq!(std.flags.delta_rps_sign(), 0);
        assert_eq!(std.abs_delta_rps_minus1, 0);
        assert_eq!(std.used_by_curr_pic_flag, 0b101);
        assert_eq!(std.use_delta_flag, 0b111);
        assert_eq!(&std.delta_poc_s0_minus1[..1], &[1]);
        assert_eq!(&std.delta_poc_s1_minus1[..1], &[3]);
    }
}
