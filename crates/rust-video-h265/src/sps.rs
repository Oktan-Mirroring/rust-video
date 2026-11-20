//! Sequence parameter sets.

use crate::bits::BitReader;
use crate::error::{H265Error, H265Gate, ParseReason};
use crate::framing::{Crop, MAX_LT_SPS, MAX_RPS, MAX_SPS};
use crate::ptl::{parse_profile_tier_level, ProfileTierLevel};
use crate::rps::{parse_short_term_rps, ShortTermRps};
use crate::scaling::{parse_scaling_list_data, ScalingLists};
use crate::vui::{parse_vui, VuiParams};

/// Temporal nesting flag bit.
pub const SPS_NESTING: u16 = 1;
/// Sublayer ordering info flag bit.
pub const SPS_ORDERING_INFO: u16 = 1 << 1;
/// Scaling lists enabled flag bit.
pub const SPS_SCALING: u16 = 1 << 2;
/// Asymmetric motion partitions flag bit.
pub const SPS_AMP: u16 = 1 << 3;
/// Sample adaptive offset flag bit.
pub const SPS_SAO: u16 = 1 << 4;
/// Pulse code modulation flag bit.
pub const SPS_PCM: u16 = 1 << 5;
/// PCM loop filter disabled flag bit.
pub const SPS_PCM_LOOP_DISABLED: u16 = 1 << 6;
/// Long-term pictures present flag bit.
pub const SPS_LONG_TERM: u16 = 1 << 7;
/// Temporal motion vectors flag bit.
pub const SPS_TEMPORAL_MVP: u16 = 1 << 8;
/// Strong intra smoothing flag bit.
pub const SPS_SMOOTHING: u16 = 1 << 9;

/// Parsed sequence parameter set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sps {
    /// Set id.
    pub id: u8,
    /// Packed tool flags.
    pub flags: u16,
    /// Sublayer count.
    pub max_sub_layers: u8,
    /// Coded video sequence level.
    pub level_idc: u8,
    /// Coded width in luma samples.
    pub width: u32,
    /// Coded height in luma samples.
    pub height: u32,
    /// Visible rectangle, absent when uncropped.
    pub crop: Option<Crop>,
    /// POC LSB width minus 4.
    pub log2_max_poc_lsb_minus4: u8,
    /// Maximum buffered pictures minus one, top sublayer.
    pub max_dec_pic_buffering_minus1: u8,
    /// Maximum reorder pictures over sublayers.
    pub max_num_reorder_pics: u8,
    /// Minimum luma block size minus 3.
    pub log2_min_cb_size_minus3: u8,
    /// Luma block size range.
    pub log2_diff_max_min_cb_size: u8,
    /// Minimum transform size minus 2.
    pub log2_min_tb_size_minus2: u8,
    /// Transform size range.
    pub log2_diff_max_min_tb_size: u8,
    /// Maximum inter transform depth.
    pub max_transform_depth_inter: u8,
    /// Maximum intra transform depth.
    pub max_transform_depth_intra: u8,
    /// Sequence scaling lists.
    pub scaling: ScalingLists,
    /// PCM luma depth minus one.
    pub pcm_depth_luma: u8,
    /// PCM chroma depth minus one.
    pub pcm_depth_chroma: u8,
    /// PCM minimum block size minus 3.
    pub pcm_log2_min_minus3: u8,
    /// PCM block size range.
    pub pcm_log2_diff: u8,
    /// Short-term reference set table.
    pub rps: Vec<ShortTermRps>,
    /// Sequence long-term POC LSBs.
    pub lt_poc_lsb: Vec<u32>,
    /// Sequence long-term used flags.
    pub lt_used_by_curr: Vec<bool>,
    /// Video set id.
    pub vps_id: u8,
    /// Profile, tier and level.
    pub ptl: ProfileTierLevel,
    /// Chroma sampling indicator.
    pub chroma_format: u8,
    /// Separate colour plane coding.
    pub separate_colour_plane: bool,
    /// Luma bit depth minus eight.
    pub bit_depth_luma: u8,
    /// Chroma bit depth minus eight.
    pub bit_depth_chroma: u8,
    /// Buffered pictures minus one per coded sublayer.
    pub ordering_buffering: Vec<u8>,
    /// Reorder pictures per coded sublayer.
    pub ordering_reorder: Vec<u8>,
    /// Latency increase per coded sublayer.
    pub ordering_latency: Vec<u32>,
    /// Video usability information, absent without the flag.
    pub vui: Option<VuiParams>,
    /// Sequence extension present.
    pub sps_extension_present: bool,
    /// Range, multilayer, 3D, and SCC extension flags.
    pub sps_extension_flags: [bool; 4],
}

impl Sps {
    /// Scaling lists enabled.
    #[must_use]
    pub const fn scaling_enabled(&self) -> bool {
        self.flags & SPS_SCALING != 0
    }

    /// Asymmetric motion partitions.
    #[must_use]
    pub const fn amp(&self) -> bool {
        self.flags & SPS_AMP != 0
    }

    /// Sample adaptive offset.
    #[must_use]
    pub const fn sao(&self) -> bool {
        self.flags & SPS_SAO != 0
    }

    /// Pulse code modulation.
    #[must_use]
    pub const fn pcm(&self) -> bool {
        self.flags & SPS_PCM != 0
    }

    /// PCM loop filter disabled.
    #[must_use]
    pub const fn pcm_loop_disabled(&self) -> bool {
        self.flags & SPS_PCM_LOOP_DISABLED != 0
    }

    /// Long-term pictures present.
    #[must_use]
    pub const fn long_term_present(&self) -> bool {
        self.flags & SPS_LONG_TERM != 0
    }

    /// Temporal motion vectors.
    #[must_use]
    pub const fn temporal_mvp(&self) -> bool {
        self.flags & SPS_TEMPORAL_MVP != 0
    }

    /// Strong intra smoothing.
    #[must_use]
    pub const fn smoothing(&self) -> bool {
        self.flags & SPS_SMOOTHING != 0
    }

    /// Maximum POC LSB.
    #[must_use]
    pub const fn max_poc_lsb(&self) -> u32 {
        1 << (self.log2_max_poc_lsb_minus4 + 4)
    }

    /// Picture order count from `lsb` against `anchor`, H.265 8.3.1.
    ///
    /// `anchor` carries the previous temporal-0 POC; `None` resets the
    /// MSB on the first picture. `reset_msb` zeroes the MSB on BLA
    /// pictures and first-sequence CRA pictures. `None` answers a count
    /// outside the `i32` range.
    #[must_use]
    pub fn poc_from_lsb(&self, anchor: Option<i32>, lsb: u32, reset_msb: bool) -> Option<i32> {
        let max = i64::from(self.max_poc_lsb());
        let lsb = i64::from(lsb);
        let mut msb = anchor.map_or(0, |anchor| {
            let anchor = i64::from(anchor);
            let prev_lsb = anchor.rem_euclid(max);
            let base = anchor - prev_lsb;
            if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
                base + max
            } else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
                base - max
            } else {
                base
            }
        });
        if reset_msb {
            msb = 0;
        }
        i32::try_from(msb + lsb).ok()
    }

    /// Minimum luma block width in samples.
    #[must_use]
    pub const fn min_cb_size(&self) -> u32 {
        1 << (self.log2_min_cb_size_minus3 + 3)
    }

    /// Luma coding tree block width in samples.
    #[must_use]
    pub const fn ctb_size(&self) -> u32 {
        1 << (self.log2_min_cb_size_minus3 + 3 + self.log2_diff_max_min_cb_size)
    }

    /// Picture width in minimum coding blocks.
    #[must_use]
    pub fn width_in_min_cbs(&self) -> u32 {
        self.width.div_ceil(self.min_cb_size())
    }

    /// Picture height in minimum coding blocks.
    #[must_use]
    pub fn height_in_min_cbs(&self) -> u32 {
        self.height.div_ceil(self.min_cb_size())
    }

    /// Luma coding tree block columns.
    #[must_use]
    pub fn width_in_ctbs(&self) -> u32 {
        self.width.div_ceil(self.ctb_size())
    }

    /// Luma coding tree block rows.
    #[must_use]
    pub fn height_in_ctbs(&self) -> u32 {
        self.height.div_ceil(self.ctb_size())
    }

    /// Visible width in luma samples.
    #[must_use]
    pub fn visible_width(&self) -> u32 {
        self.crop
            .map_or(self.width, |crop| self.width - crop.left - crop.right)
    }

    /// Visible height in luma samples.
    #[must_use]
    pub fn visible_height(&self) -> u32 {
        self.crop
            .map_or(self.height, |crop| self.height - crop.top - crop.bottom)
    }
}

/// Sequence parameter set from RBSP `rbsp`.
///
/// # Errors
///
/// Returns `Err` on malformed syntax or tier-declined constructs.
///
/// # Arguments
///
/// * `rbsp` - RBSP bytes with the NAL header stripped.
pub fn parse_sps<E: H265Error, G: H265Gate<E>>(rbsp: &[u8]) -> Result<Sps, E> {
    let mut reader = BitReader::new(rbsp);
    let vps_id = parse_vps_id(&mut reader)?;
    let max_sub_layers = parse_sub_layers(&mut reader)?;
    let mut flags = 0u16;
    if reader.read_bit("sps_temporal_id_nesting_flag")? {
        flags |= SPS_NESTING;
    }
    let ptl = parse_profile_tier_level(&mut reader, max_sub_layers)?;
    let id = parse_sps_id(&mut reader)?;
    let (chroma_format, separate_colour_plane) = parse_chroma(&mut reader)?;
    let (width, height) = parse_pic_size(&mut reader)?;
    let crop = parse_conformance(&mut reader, width, height)?;
    let (bit_depth_luma, bit_depth_chroma) = parse_depths(&mut reader)?;
    let log2_max_poc_lsb_minus4 = parse_log2(&mut reader, "log2_max_pic_order_cnt_lsb_minus4")?;
    let ordering = parse_ordering(&mut reader, max_sub_layers, &mut flags)?;
    let blocks = parse_block_sizes(&mut reader)?;
    let scaling = parse_sps_scaling(&mut reader, &mut flags)?;
    if reader.read_bit("amp_enabled_flag")? {
        flags |= SPS_AMP;
    }
    if reader.read_bit("sample_adaptive_offset_enabled_flag")? {
        flags |= SPS_SAO;
    }
    let pcm = parse_pcm(&mut reader, &mut flags)?;
    let rps = parse_rps_table(&mut reader)?;
    let (lt_poc_lsb, lt_used_by_curr) =
        parse_long_term_sps(&mut reader, log2_max_poc_lsb_minus4, &mut flags)?;
    if reader.read_bit("sps_temporal_mvp_enabled_flag")? {
        flags |= SPS_TEMPORAL_MVP;
    }
    if reader.read_bit("strong_intra_smoothing_enabled_flag")? {
        flags |= SPS_SMOOTHING;
    }
    let vui = if reader.read_bit("vui_parameters_present_flag")? {
        Some(parse_vui(&mut reader, max_sub_layers)?)
    } else {
        None
    };
    let (sps_extension_present, sps_extension_flags) = parse_sps_extension(&mut reader)?;
    let sps = Sps {
        id,
        flags,
        max_sub_layers,
        level_idc: ptl.level_idc,
        width,
        height,
        crop,
        log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1: ordering.buffering.last().copied().unwrap_or(0),
        max_num_reorder_pics: ordering.reorder.iter().copied().max().unwrap_or(0),
        log2_min_cb_size_minus3: blocks.0,
        log2_diff_max_min_cb_size: blocks.1,
        log2_min_tb_size_minus2: blocks.2,
        log2_diff_max_min_tb_size: blocks.3,
        max_transform_depth_inter: blocks.4,
        max_transform_depth_intra: blocks.5,
        scaling,
        pcm_depth_luma: pcm.0,
        pcm_depth_chroma: pcm.1,
        pcm_log2_min_minus3: pcm.2,
        pcm_log2_diff: pcm.3,
        rps,
        lt_poc_lsb,
        lt_used_by_curr,
        vps_id,
        ptl,
        chroma_format,
        separate_colour_plane,
        bit_depth_luma,
        bit_depth_chroma,
        ordering_buffering: ordering.buffering,
        ordering_reorder: ordering.reorder,
        ordering_latency: ordering.latency,
        vui,
        sps_extension_present,
        sps_extension_flags,
    };
    G::check_sps(&sps)?;
    Ok(sps)
}

/// Video set id behind the sequence set.
fn parse_vps_id<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<u8, E> {
    let id = reader.read_bits(4, "sps_video_parameter_set_id")?;
    u8::try_from(id).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "sps_video_parameter_set_id",
        })
    })
}

/// Sublayer count from the minus-one field.
fn parse_sub_layers<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<u8, E> {
    let minus1 = reader.read_bits(3, "sps_max_sub_layers_minus1")?;
    u8::try_from(minus1 + 1).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "sub layers",
        })
    })
}

/// Set id below the set count.
fn parse_sps_id<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<u8, E> {
    let id = reader.read_ue("seq_parameter_set_id")?;
    let id = u8::try_from(id).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "seq_parameter_set_id",
        })
    })?;
    if usize::from(id) >= MAX_SPS {
        return Err(E::parse(ParseReason::Range {
            syntax: "seq_parameter_set_id",
        }));
    }
    Ok(id)
}

/// Chroma sampling indicator with its separate-plane flag.
fn parse_chroma<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<(u8, bool), E> {
    let chroma = reader.read_ue("chroma_format_idc")?;
    let chroma = u8::try_from(chroma).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "chroma_format_idc",
        })
    })?;
    let separate = chroma == 3 && reader.read_bit("separate_colour_plane_flag")?;
    Ok((chroma, separate))
}

/// Coded picture size in luma samples.
fn parse_pic_size<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<(u32, u32), E> {
    let width = reader.read_ue("pic_width_in_luma_samples")?;
    let height = reader.read_ue("pic_height_in_luma_samples")?;
    if width == 0 || height == 0 {
        return Err(E::parse(ParseReason::EmptyCodedSize));
    }
    Ok((width, height))
}

/// Conformance crop rectangle in luma samples.
fn parse_conformance<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    width: u32,
    height: u32,
) -> Result<Option<Crop>, E> {
    if !reader.read_bit("conformance_window_flag")? {
        return Ok(None);
    }
    let left = reader.read_ue("conf_win_left_offset")?;
    let right = reader.read_ue("conf_win_right_offset")?;
    let top = reader.read_ue("conf_win_top_offset")?;
    let bottom = reader.read_ue("conf_win_bottom_offset")?;
    let horizontal = left.checked_add(right).and_then(|sum| sum.checked_mul(2));
    let vertical = top.checked_add(bottom).and_then(|sum| sum.checked_mul(2));
    match (horizontal, vertical) {
        (Some(consumed), Some(rows)) if consumed < width && rows < height => Ok(Some(Crop {
            left: left * 2,
            right: right * 2,
            top: top * 2,
            bottom: bottom * 2,
        })),
        _ => Err(E::parse(ParseReason::ConformanceOutside)),
    }
}

/// Luma and chroma bit depths minus eight.
fn parse_depths<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<(u8, u8), E> {
    let luma = reader.read_ue("bit_depth_luma_minus8")?;
    let chroma = reader.read_ue("bit_depth_chroma_minus8")?;
    let luma = u8::try_from(luma).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "bit_depth_luma_minus8",
        })
    })?;
    let chroma = u8::try_from(chroma).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "bit_depth_chroma_minus8",
        })
    })?;
    Ok((luma, chroma))
}

/// Log2 width field below the 12 ceiling.
fn parse_log2<E: H265Error>(reader: &mut BitReader<'_, E>, syntax: &'static str) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if value > 12 {
        return Err(E::parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| E::parse(ParseReason::Range { syntax }))
}

/// Sublayer ordering limits.
struct Ordering {
    /// Buffered pictures minus one per coded sublayer.
    buffering: Vec<u8>,
    /// Reorder pictures per coded sublayer.
    reorder: Vec<u8>,
    /// Latency increase per coded sublayer.
    latency: Vec<u32>,
}

/// Buffering and reorder limits over sublayers.
fn parse_ordering<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    max_sub_layers: u8,
    flags: &mut u16,
) -> Result<Ordering, E> {
    if reader.read_bit("sps_sub_layer_ordering_info_present_flag")? {
        *flags |= SPS_ORDERING_INFO;
    }
    let first = if *flags & SPS_ORDERING_INFO != 0 {
        0
    } else {
        max_sub_layers - 1
    };
    let mut ordering = Ordering {
        buffering: Vec::new(),
        reorder: Vec::new(),
        latency: Vec::new(),
    };
    for _ in first..max_sub_layers {
        let buffered = reader.read_ue("sps_max_dec_pic_buffering_minus1")?;
        if buffered > 15 {
            return Err(E::parse(ParseReason::Range {
                syntax: "sps_max_dec_pic_buffering_minus1",
            }));
        }
        ordering.buffering.push(u8::try_from(buffered).map_err(|_| {
            E::parse(ParseReason::Range {
                syntax: "sps_max_dec_pic_buffering_minus1",
            })
        })?);
        let reordered = reader.read_ue("sps_max_num_reorder_pics")?;
        if reordered > 16 {
            return Err(E::parse(ParseReason::Range {
                syntax: "sps_max_num_reorder_pics",
            }));
        }
        ordering.reorder.push(u8::try_from(reordered).map_err(|_| {
            E::parse(ParseReason::Range {
                syntax: "sps_max_num_reorder_pics",
            })
        })?);
        ordering
            .latency
            .push(reader.read_ue("sps_max_latency_increase_plus1")?);
    }
    Ok(ordering)
}

/// Luma block sizes plus transform depths.
fn parse_block_sizes<E: H265Error>(
    reader: &mut BitReader<'_, E>,
) -> Result<(u8, u8, u8, u8, u8, u8), E> {
    let cb_min = parse_bounded_ue(reader, "log2_min_luma_coding_block_size_minus3", 3)?;
    let cb_range = parse_bounded_ue(reader, "log2_diff_max_min_luma_coding_block_size", 3)?;
    let tb_min = parse_bounded_ue(reader, "log2_min_luma_transform_block_size_minus2", 2)?;
    let tb_range = parse_bounded_ue(reader, "log2_diff_max_min_luma_transform_block_size", 3)?;
    let depth_inter = parse_bounded_ue(reader, "max_transform_hierarchy_depth_inter", 7)?;
    let depth_intra = parse_bounded_ue(reader, "max_transform_hierarchy_depth_intra", 7)?;
    Ok((cb_min, cb_range, tb_min, tb_range, depth_inter, depth_intra))
}

/// Unsigned field below `ceiling`.
pub(crate) fn parse_bounded_ue<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    syntax: &'static str,
    ceiling: u32,
) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if value > ceiling {
        return Err(E::parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| E::parse(ParseReason::Range { syntax }))
}

/// Sequence scaling lists, flat without data.
fn parse_sps_scaling<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    flags: &mut u16,
) -> Result<ScalingLists, E> {
    if !reader.read_bit("scaling_list_enabled_flag")? {
        return Ok(ScalingLists::default());
    }
    *flags |= SPS_SCALING;
    if reader.read_bit("sps_scaling_list_data_present_flag")? {
        return parse_scaling_list_data(reader);
    }
    Ok(ScalingLists::default())
}

/// PCM parameters, zeroed without PCM.
fn parse_pcm<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    flags: &mut u16,
) -> Result<(u8, u8, u8, u8), E> {
    if !reader.read_bit("pcm_enabled_flag")? {
        return Ok((0, 0, 0, 0));
    }
    *flags |= SPS_PCM;
    let luma = reader.read_bits(4, "pcm_sample_bit_depth_luma_minus1")?;
    let chroma = reader.read_bits(4, "pcm_sample_bit_depth_chroma_minus1")?;
    let min = parse_bounded_ue(reader, "log2_min_pcm_luma_coding_block_size_minus3", 2)?;
    let diff = parse_bounded_ue(reader, "log2_diff_max_min_pcm_luma_coding_block_size", 2)?;
    if reader.read_bit("pcm_loop_filter_disabled_flag")? {
        *flags |= SPS_PCM_LOOP_DISABLED;
    }
    let luma = u8::try_from(luma).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "pcm depth",
        })
    })?;
    let chroma = u8::try_from(chroma).map_err(|_| {
        E::parse(ParseReason::Range {
            syntax: "pcm depth",
        })
    })?;
    Ok((luma, chroma, min, diff))
}

/// Short-term reference set table.
fn parse_rps_table<E: H265Error>(reader: &mut BitReader<'_, E>) -> Result<Vec<ShortTermRps>, E> {
    let count = reader.read_ue("num_short_term_ref_pic_sets")?;
    if usize::try_from(count).is_ok_and(|count| count > MAX_RPS) {
        return Err(E::parse(ParseReason::RpsTableOversize));
    }
    let mut table = Vec::new();
    for index in 0..count {
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        let total = usize::try_from(count).unwrap_or(usize::MAX);
        let resolved = parse_short_term_rps(reader, index, total, &table)?;
        table.push(resolved.set);
    }
    Ok(table)
}

/// Sequence long-term pictures.
fn parse_long_term_sps<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    log2_max_poc_lsb_minus4: u8,
    flags: &mut u16,
) -> Result<(Vec<u32>, Vec<bool>), E> {
    if !reader.read_bit("long_term_ref_pics_present_flag")? {
        return Ok((Vec::new(), Vec::new()));
    }
    *flags |= SPS_LONG_TERM;
    let count = reader.read_ue("num_long_term_ref_pics_sps")?;
    if usize::try_from(count).is_ok_and(|count| count > MAX_LT_SPS) {
        return Err(E::parse(ParseReason::LtSpsOversize));
    }
    let width = u32::from(log2_max_poc_lsb_minus4) + 4;
    let mut poc_lsb = Vec::new();
    let mut used = Vec::new();
    for _ in 0..count {
        poc_lsb.push(reader.read_bits(width, "lt_ref_pic_poc_lsb_sps")?);
        used.push(reader.read_bit("used_by_curr_pic_lt_sps_flag")?);
    }
    Ok((poc_lsb, used))
}

/// Sequence extension presence with its range, multilayer, 3D, and SCC flags.
fn parse_sps_extension<E: H265Error>(
    reader: &mut BitReader<'_, E>,
) -> Result<(bool, [bool; 4]), E> {
    if !reader.read_bit("sps_extension_present_flag")? {
        return Ok((false, [false; 4]));
    }
    let mut flags = [false; 4];
    for (slot, syntax) in flags.iter_mut().zip([
        "sps_range_extension_flag",
        "sps_multilayer_extension_flag",
        "sps_3d_extension_flag",
        "sps_scc_extension_flag",
    ]) {
        *slot = reader.read_bit(syntax)?;
    }
    reader.read_bits(4, "sps_extension_4bits")?;
    Ok((true, flags))
}
