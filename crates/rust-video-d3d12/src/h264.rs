//! H.264 Annex B ingest: parameter sets plus slice headers.
//!
//! Frame-only progressive pictures for single-picture submissions.

use crate::bits::BitReader;
use crate::error::D3d12Error;

/// Sequence set count, H.264 7.4.2.1.
pub(crate) const MAX_SPS: usize = 32;

/// Picture set count, H.264 7.4.2.2.
pub(crate) const MAX_PPS: usize = 256;

/// Default 4x4 intra scaling list, H.264 Table 7-2, zig-zag scan order.
const DEFAULT_4X4_INTRA: [u8; 16] = [
    6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42,
];

/// Default 4x4 inter scaling list, H.264 Table 7-2, zig-zag scan order.
const DEFAULT_4X4_INTER: [u8; 16] = [
    10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34,
];

/// Default 8x8 intra scaling list, H.264 Table 7-2, zig-zag scan order.
const DEFAULT_8X8_INTRA: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
    31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
];

/// Default 8x8 inter scaling list, H.264 Table 7-2, zig-zag scan order.
const DEFAULT_8X8_INTER: [u8; 64] = [
    9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22,
    22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27,
    27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
];

/// NAL units in one Annex B packet, start codes stripped.
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

/// NAL with emulation prevention bytes removed.
pub(crate) fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Scaling lists in zig-zag scan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScalingSets {
    /// Six 4x4 lists.
    pub(crate) four: [[u8; 16]; 6],
    /// Two 8x8 lists.
    pub(crate) eight: [[u8; 64]; 2],
}

/// Scaling list from the reader, defaulting on a leading zero delta.
fn scaling_list<const N: usize>(
    reader: &mut BitReader,
    default: &[u8; N],
) -> Result<[u8; N], D3d12Error> {
    let mut zigzagged = [0u8; N];
    let mut last = 8i32;
    let mut next = 8i32;
    for slot in zigzagged.iter_mut().enumerate() {
        let (position, value) = slot;
        if next != 0 {
            let delta = reader.read_se("scaling delta")?;
            next = (last + delta).rem_euclid(256);
            if position == 0 && next == 0 {
                return Ok(*default);
            }
        }
        let scale = u8::try_from(if next == 0 { last } else { next })
            .map_err(|_| D3d12Error::Parse("scaling value range".to_owned()))?;
        *value = scale;
        last = i32::from(scale);
    }
    Ok(zigzagged)
}

/// Picture order count method, H.264 8.2.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PocType {
    /// LSB in slice headers.
    Lsb,
    /// Deltas in slice headers.
    Delta,
    /// Twice the frame number.
    FrameNum,
}

impl PocType {
    /// Method for `value`.
    fn new(value: u32) -> Result<Self, D3d12Error> {
        match value {
            0 => Ok(Self::Lsb),
            1 => Ok(Self::Delta),
            2 => Ok(Self::FrameNum),
            _ => Err(D3d12Error::Parse("pic_order_cnt_type range".to_owned())),
        }
    }

    /// DXVA type number.
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::Lsb => 0,
            Self::Delta => 1,
            Self::FrameNum => 2,
        }
    }
}

/// Visible rectangle in luma samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Crop {
    /// Cropped luma columns on the left.
    pub(crate) left: u32,
    /// Cropped luma columns on the right.
    pub(crate) right: u32,
    /// Cropped luma rows on top.
    pub(crate) top: u32,
    /// Cropped luma rows at the bottom.
    pub(crate) bottom: u32,
}

/// Parsed sequence parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sps {
    /// Coded video sequence level.
    pub(crate) level_idc: u8,
    /// Set id.
    pub(crate) seq_parameter_set_id: u8,
    /// Resolved sequence scaling lists.
    pub(crate) scaling: ScalingSets,
    /// Frame number width minus 4.
    pub(crate) log2_max_frame_num_minus4: u8,
    /// Picture order count method.
    pub(crate) poc_type: PocType,
    /// POC LSB width minus 4.
    pub(crate) log2_max_poc_lsb_minus4: u8,
    /// POC deltas absent and zero.
    pub(crate) delta_zero: bool,
    /// Non-reference POC offset.
    pub(crate) offset_non_ref: i32,
    /// Bottom-field POC offset.
    pub(crate) offset_top_bottom: i32,
    /// Reference cycle length.
    pub(crate) cycle: u8,
    /// Reference cycle offsets.
    pub(crate) cycle_offsets: [i32; 255],
    /// Expected POC delta per cycle.
    pub(crate) expected_delta: i32,
    /// Maximum reference frame count.
    pub(crate) max_num_ref_frames: u8,
    /// Frame number gaps allowed.
    pub(crate) gaps_allowed: bool,
    /// Coded width in macroblocks minus one.
    pub(crate) width_in_mbs_minus1: u16,
    /// Coded height in macroblocks minus one.
    pub(crate) height_in_mbs_minus1: u16,
    /// Direct 8x8 inference.
    pub(crate) direct_8x8: bool,
    /// Visible rectangle, absent when uncropped.
    pub(crate) crop: Option<Crop>,
}

impl Sps {
    /// Coded width in luma samples.
    pub(crate) fn width(&self) -> u32 {
        (u32::from(self.width_in_mbs_minus1) + 1) * 16
    }

    /// Coded height in luma samples.
    pub(crate) fn height(&self) -> u32 {
        (u32::from(self.height_in_mbs_minus1) + 1) * 16
    }

    /// Maximum frame number.
    pub(crate) const fn max_frame_num(&self) -> u32 {
        1 << (self.log2_max_frame_num_minus4 + 4)
    }

    /// Maximum POC LSB.
    pub(crate) const fn max_poc_lsb(&self) -> u32 {
        1 << (self.log2_max_poc_lsb_minus4 + 4)
    }

    /// Visible width in luma samples.
    pub(crate) fn visible_width(&self) -> u32 {
        self.crop.map_or_else(
            || self.width(),
            |crop| self.width() - crop.left - crop.right,
        )
    }

    /// Visible height in luma samples.
    pub(crate) fn visible_height(&self) -> u32 {
        self.crop.map_or_else(
            || self.height(),
            |crop| self.height() - crop.top - crop.bottom,
        )
    }
}

/// Sequence parameter set from RBSP `rbsp`.
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, D3d12Error> {
    let mut reader = BitReader::new(rbsp);
    let profile_idc = u8::try_from(reader.read_bits(8, "profile_idc")?)
        .map_err(|_| D3d12Error::Parse("profile_idc range".to_owned()))?;
    reader.read_bits(8, "constraints")?;
    let level_idc = u8::try_from(reader.read_bits(8, "level_idc")?)
        .map_err(|_| D3d12Error::Parse("level_idc range".to_owned()))?;
    let sps_id = parse_sps_id(&mut reader)?;
    let scaling = parse_sps_chroma(&mut reader, profile_idc)?;
    let log2_max_frame_num_minus4 = parse_log2(&mut reader, "log2_max_frame_num")?;
    let poc = parse_sps_poc(&mut reader)?;
    let max_num_ref_frames = u8::try_from(reader.read_ue("max_num_ref_frames")?)
        .map_err(|_| D3d12Error::Parse("max_num_ref_frames range".to_owned()))?;
    if max_num_ref_frames > 16 {
        return Err(D3d12Error::Parse("max_num_ref_frames range".to_owned()));
    }
    let gaps_allowed = reader.read_bit("gaps_in_frame_num")?;
    let width_in_mbs_minus1 = parse_mbs(&mut reader, "pic_width_in_mbs")?;
    let height_in_mbs_minus1 = parse_mbs(&mut reader, "pic_height_in_map_units")?;
    if !reader.read_bit("frame_mbs_only_flag")? {
        return Err(D3d12Error::Unsupported("interlaced".to_owned()));
    }
    let direct_8x8 = reader.read_bit("direct_8x8_inference_flag")?;
    let crop = parse_crop(&mut reader, width_in_mbs_minus1, height_in_mbs_minus1)?;
    Ok(Sps {
        level_idc,
        seq_parameter_set_id: sps_id,
        scaling,
        log2_max_frame_num_minus4,
        poc_type: poc.poc_type,
        log2_max_poc_lsb_minus4: poc.lsb,
        delta_zero: poc.delta_zero,
        offset_non_ref: poc.offset_non_ref,
        offset_top_bottom: poc.offset_top_bottom,
        cycle: poc.cycle,
        cycle_offsets: poc.offsets,
        expected_delta: poc.expected,
        max_num_ref_frames,
        gaps_allowed,
        width_in_mbs_minus1,
        height_in_mbs_minus1,
        direct_8x8,
        crop,
    })
}

/// Set id below the set count.
fn parse_sps_id(reader: &mut BitReader) -> Result<u8, D3d12Error> {
    let id = reader.read_ue("seq_parameter_set_id")?;
    let id =
        u8::try_from(id).map_err(|_| D3d12Error::Parse("seq_parameter_set_id range".to_owned()))?;
    if usize::from(id) >= MAX_SPS {
        return Err(D3d12Error::Parse("seq_parameter_set_id range".to_owned()));
    }
    Ok(id)
}

/// Chroma, depth, and scaling section of a sequence set.
fn parse_sps_chroma(reader: &mut BitReader, profile_idc: u8) -> Result<ScalingSets, D3d12Error> {
    if !matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 134 | 135 | 138 | 139
    ) {
        return Ok(flat_scaling());
    }
    if reader.read_ue("chroma_format_idc")? != 1 {
        return Err(D3d12Error::Unsupported("chroma outside 4:2:0".to_owned()));
    }
    if reader.read_ue("bit_depth_luma_minus8")? != 0 {
        return Err(D3d12Error::Unsupported("luma depth above 8 bit".to_owned()));
    }
    if reader.read_ue("bit_depth_chroma_minus8")? != 0 {
        return Err(D3d12Error::Unsupported(
            "chroma depth above 8 bit".to_owned(),
        ));
    }
    if reader.read_bit("qpprime_y_zero_transform_bypass_flag")? {
        return Err(D3d12Error::Unsupported("transform bypass".to_owned()));
    }
    if !reader.read_bit("seq_scaling_matrix_present_flag")? {
        return Ok(flat_scaling());
    }
    parse_sequence_scaling(reader)
}

/// Scaling lists resolved against the Table 7-2 defaults.
fn parse_sequence_scaling(reader: &mut BitReader) -> Result<ScalingSets, D3d12Error> {
    let mut four = [[0u8; 16]; 6];
    for index in 0..6 {
        four[index] = sequence_list_4x4(reader, index, &four)?;
    }
    let mut eight = [[0u8; 64]; 2];
    for (index, list) in eight.iter_mut().enumerate() {
        *list = sequence_list_8x8(reader, index)?;
    }
    Ok(ScalingSets { four, eight })
}

/// One sequence 4x4 list: parsed, or the fallback chain value.
fn sequence_list_4x4(
    reader: &mut BitReader,
    index: usize,
    four: &[[u8; 16]; 6],
) -> Result<[u8; 16], D3d12Error> {
    if reader.read_bit("seq_scaling_list_present_flag")? {
        let default = if index < 3 {
            &DEFAULT_4X4_INTRA
        } else {
            &DEFAULT_4X4_INTER
        };
        return scaling_list(reader, default);
    }
    match index {
        0 => Ok(DEFAULT_4X4_INTRA),
        3 => Ok(DEFAULT_4X4_INTER),
        _ => Ok(four[index - 1]),
    }
}

/// One sequence 8x8 list: parsed, or the Table 7-2 default.
fn sequence_list_8x8(reader: &mut BitReader, index: usize) -> Result<[u8; 64], D3d12Error> {
    if reader.read_bit("seq_scaling_list_present_flag")? {
        let default = if index.is_multiple_of(2) {
            &DEFAULT_8X8_INTRA
        } else {
            &DEFAULT_8X8_INTER
        };
        return scaling_list(reader, default);
    }
    if index.is_multiple_of(2) {
        Ok(DEFAULT_8X8_INTRA)
    } else {
        Ok(DEFAULT_8X8_INTER)
    }
}

/// Unity scaling for profiles without scaling syntax.
fn flat_scaling() -> ScalingSets {
    ScalingSets {
        four: [[16u8; 16]; 6],
        eight: [[16u8; 64]; 2],
    }
}

/// Log2 width field below the 12 ceiling.
fn parse_log2(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    if value > 12 {
        return Err(D3d12Error::Parse(format!("{syntax} range")));
    }
    u8::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// POC section: method plus its parameters.
struct PocSection {
    /// Picture order count method.
    poc_type: PocType,
    /// POC LSB width minus 4.
    lsb: u8,
    /// POC deltas absent and zero.
    delta_zero: bool,
    /// Non-reference POC offset.
    offset_non_ref: i32,
    /// Bottom-field POC offset.
    offset_top_bottom: i32,
    /// Reference cycle length.
    cycle: u8,
    /// Reference cycle offsets.
    offsets: [i32; 255],
    /// Expected POC delta per cycle.
    expected: i32,
}

/// POC section of a sequence set.
fn parse_sps_poc(reader: &mut BitReader) -> Result<PocSection, D3d12Error> {
    let poc_type = PocType::new(reader.read_ue("pic_order_cnt_type")?)?;
    let mut section = PocSection {
        poc_type,
        lsb: 0,
        delta_zero: false,
        offset_non_ref: 0,
        offset_top_bottom: 0,
        cycle: 0,
        offsets: [0; 255],
        expected: 0,
    };
    if poc_type == PocType::Lsb {
        section.lsb = parse_log2(reader, "log2_max_pic_order_cnt_lsb")?;
    } else if poc_type == PocType::Delta {
        parse_poc_cycle(reader, &mut section)?;
    }
    Ok(section)
}

/// Type 1 POC cycle parameters.
fn parse_poc_cycle(reader: &mut BitReader, section: &mut PocSection) -> Result<(), D3d12Error> {
    section.delta_zero = reader.read_bit("delta_pic_order_always_zero_flag")?;
    section.offset_non_ref = reader.read_se("offset_for_non_ref_pic")?;
    section.offset_top_bottom = reader.read_se("offset_for_top_to_bottom_field")?;
    let count = reader.read_ue("num_ref_frames_in_pic_order_cnt_cycle")?;
    section.cycle = u8::try_from(count)
        .map_err(|_| D3d12Error::Parse("pic_order_cnt_cycle range".to_owned()))?;
    let mut total: i64 = 0;
    for slot in section.offsets.iter_mut().take(usize::from(section.cycle)) {
        *slot = reader.read_se("offset_for_ref_frame")?;
        total += i64::from(*slot);
    }
    section.expected = i32::try_from(total)
        .map_err(|_| D3d12Error::Parse("pic_order_cnt_cycle range".to_owned()))?;
    Ok(())
}

/// Macroblock count field.
fn parse_mbs(reader: &mut BitReader, syntax: &'static str) -> Result<u16, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    u16::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// Frame crop rectangle in luma samples, validated against the coded size.
fn parse_crop(
    reader: &mut BitReader,
    width_in_mbs: u16,
    height_in_mbs: u16,
) -> Result<Option<Crop>, D3d12Error> {
    if !reader.read_bit("frame_cropping_flag")? {
        return Ok(None);
    }
    let left = reader.read_ue("frame_crop_left_offset")?;
    let right = reader.read_ue("frame_crop_right_offset")?;
    let top = reader.read_ue("frame_crop_top_offset")?;
    let bottom = reader.read_ue("frame_crop_bottom_offset")?;
    let width = (u32::from(width_in_mbs) + 1) * 16;
    let height = (u32::from(height_in_mbs) + 1) * 16;
    let horizontal = left.checked_add(right).and_then(|sum| sum.checked_mul(2));
    let vertical = top.checked_add(bottom).and_then(|sum| sum.checked_mul(2));
    match (horizontal, vertical) {
        (Some(consumed), Some(rows)) if consumed < width && rows < height => Ok(Some(Crop {
            left: left * 2,
            right: right * 2,
            top: top * 2,
            bottom: bottom * 2,
        })),
        _ => Err(D3d12Error::Parse(
            "frame_crop outside coded size".to_owned(),
        )),
    }
}

/// CABAC entropy coding flag bit.
pub(crate) const PPS_ENTROPY: u16 = 1;
/// POC delta present flag bit.
pub(crate) const PPS_PIC_ORDER: u16 = 1 << 1;
/// Weighted prediction flag bit.
pub(crate) const PPS_WEIGHTED_PRED: u16 = 1 << 2;
/// Constrained intra prediction flag bit.
pub(crate) const PPS_CONSTRAINED_INTRA: u16 = 1 << 3;
/// Deblocking control present flag bit.
pub(crate) const PPS_DEBLOCKING: u16 = 1 << 4;
/// Redundant pictures present flag bit.
pub(crate) const PPS_REDUNDANT: u16 = 1 << 5;
/// 8x8 transform mode flag bit.
pub(crate) const PPS_TRANSFORM_8X8: u16 = 1 << 6;
/// Weighted biprediction mode shift.
pub(crate) const PPS_BIPRED_SHIFT: u16 = 7;

/// Parsed picture parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pps {
    /// Set id.
    pub(crate) pic_parameter_set_id: u8,
    /// Sequence set id.
    pub(crate) seq_parameter_set_id: u8,
    /// Packed flags plus the biprediction mode.
    pub(crate) flags: u16,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_l0: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_l1: u8,
    /// Luma QP init delta.
    pub(crate) pic_init_qp: i8,
    /// SP/SI QP init delta.
    pub(crate) pic_init_qs: i8,
    /// Chroma QP offset.
    pub(crate) chroma_qp: i8,
    /// Second chroma QP offset.
    pub(crate) second_chroma_qp: i8,
    /// Resolved picture scaling lists.
    pub(crate) scaling: ScalingSets,
}

impl Pps {
    /// CABAC entropy coding.
    pub(crate) const fn entropy(&self) -> bool {
        self.flags & PPS_ENTROPY != 0
    }

    /// POC delta present in slices.
    pub(crate) const fn pic_order_present(&self) -> bool {
        self.flags & PPS_PIC_ORDER != 0
    }

    /// Weighted prediction.
    pub(crate) const fn weighted_pred(&self) -> bool {
        self.flags & PPS_WEIGHTED_PRED != 0
    }

    /// Constrained intra prediction.
    pub(crate) const fn constrained_intra(&self) -> bool {
        self.flags & PPS_CONSTRAINED_INTRA != 0
    }

    /// Deblocking control present in slices.
    pub(crate) const fn deblocking_present(&self) -> bool {
        self.flags & PPS_DEBLOCKING != 0
    }

    /// Redundant pictures present in slices.
    pub(crate) const fn redundant_present(&self) -> bool {
        self.flags & PPS_REDUNDANT != 0
    }

    /// 8x8 transform mode.
    pub(crate) const fn transform_8x8(&self) -> bool {
        self.flags & PPS_TRANSFORM_8X8 != 0
    }

    /// Weighted biprediction mode.
    pub(crate) const fn weighted_bipred(&self) -> u8 {
        match (self.flags >> PPS_BIPRED_SHIFT) & 3 {
            0 => 0,
            1 => 1,
            _ => 2,
        }
    }
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
pub(crate) fn parse_pps(rbsp: &[u8], sps: &Sps) -> Result<Pps, D3d12Error> {
    let mut reader = BitReader::new(rbsp);
    let pps_id = parse_pps_id(&mut reader)?;
    let sps_id = parse_pps_sps_id(&mut reader, sps)?;
    let mut flags = 0u16;
    if reader.read_bit("entropy_coding_mode_flag")? {
        flags |= PPS_ENTROPY;
    }
    if reader.read_bit("pic_order_present_flag")? {
        flags |= PPS_PIC_ORDER;
    }
    if reader.read_ue("num_slice_groups_minus1")? != 0 {
        return Err(D3d12Error::Unsupported(
            "flexible macroblock order".to_owned(),
        ));
    }
    let num_ref_l0 = parse_ref_count(&mut reader, "num_ref_idx_l0_default_active_minus1")?;
    let num_ref_l1 = parse_ref_count(&mut reader, "num_ref_idx_l1_default_active_minus1")?;
    if reader.read_bit("weighted_pred_flag")? {
        flags |= PPS_WEIGHTED_PRED;
    }
    let bipred = reader.read_bits(2, "weighted_bipred_idc")?;
    if bipred > 2 {
        return Err(D3d12Error::Parse("weighted_bipred_idc range".to_owned()));
    }
    let shifted = bipred << u32::from(PPS_BIPRED_SHIFT);
    flags |= u16::try_from(shifted)
        .map_err(|_| D3d12Error::Parse("weighted_bipred_idc range".to_owned()))?;
    let luma_qp = parse_qp(&mut reader, "pic_init_qp_minus26")?;
    let switch_qs = parse_qp(&mut reader, "pic_init_qs_minus26")?;
    let chroma_qp = parse_qp(&mut reader, "chroma_qp_index_offset")?;
    if reader.read_bit("deblocking_filter_control_present_flag")? {
        flags |= PPS_DEBLOCKING;
    }
    if reader.read_bit("constrained_intra_pred_flag")? {
        flags |= PPS_CONSTRAINED_INTRA;
    }
    if reader.read_bit("redundant_pic_cnt_present_flag")? {
        flags |= PPS_REDUNDANT;
    }
    let mut second_chroma_qp = 0;
    let mut scaling = sps.scaling.clone();
    if reader.more_data() {
        if reader.read_bit("transform_8x8_mode_flag")? {
            flags |= PPS_TRANSFORM_8X8;
        }
        if reader.read_bit("pic_scaling_matrix_present_flag")? {
            scaling = parse_picture_scaling(&mut reader, sps)?;
        }
        second_chroma_qp = parse_qp(&mut reader, "second_chroma_qp_index_offset")?;
    }
    Ok(Pps {
        pic_parameter_set_id: pps_id,
        seq_parameter_set_id: sps_id,
        flags,
        num_ref_l0,
        num_ref_l1,
        pic_init_qp: luma_qp,
        pic_init_qs: switch_qs,
        chroma_qp,
        second_chroma_qp,
        scaling,
    })
}

/// Set id below the set count.
fn parse_pps_id(reader: &mut BitReader) -> Result<u8, D3d12Error> {
    let id = reader.read_ue("pic_parameter_set_id")?;
    u8::try_from(id).map_err(|_| D3d12Error::Parse("pic_parameter_set_id range".to_owned()))
}

/// Sequence id matching the supplied sequence set.
fn parse_pps_sps_id(reader: &mut BitReader, sps: &Sps) -> Result<u8, D3d12Error> {
    let id = reader.read_ue("seq_parameter_set_id")?;
    let id =
        u8::try_from(id).map_err(|_| D3d12Error::Parse("seq_parameter_set_id range".to_owned()))?;
    if id != sps.seq_parameter_set_id {
        return Err(D3d12Error::Parse("pps cites unknown sps".to_owned()));
    }
    Ok(id)
}

/// Reference count field below the 32 ceiling.
fn parse_ref_count(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    if value > 31 {
        return Err(D3d12Error::Parse(format!("{syntax} range")));
    }
    u8::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// QP delta field.
fn parse_qp(reader: &mut BitReader, syntax: &'static str) -> Result<i8, D3d12Error> {
    let value = reader.read_se(syntax)?;
    i8::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// Picture scaling lists resolved against the sequence lists.
fn parse_picture_scaling(reader: &mut BitReader, sps: &Sps) -> Result<ScalingSets, D3d12Error> {
    let mut four = [[0u8; 16]; 6];
    for index in 0..6 {
        four[index] = picture_list_4x4(reader, index, sps, &four)?;
    }
    let mut eight = [[0u8; 64]; 2];
    for (index, slot) in eight.iter_mut().enumerate() {
        *slot = picture_list_8x8(reader, index, sps)?;
    }
    Ok(ScalingSets { four, eight })
}

/// One picture 4x4 list: parsed, or the sequence-seeded fallback.
fn picture_list_4x4(
    reader: &mut BitReader,
    index: usize,
    sps: &Sps,
    four: &[[u8; 16]; 6],
) -> Result<[u8; 16], D3d12Error> {
    if reader.read_bit("pic_scaling_list_present_flag")? {
        let default = if index < 3 {
            &DEFAULT_4X4_INTRA
        } else {
            &DEFAULT_4X4_INTER
        };
        return scaling_list(reader, default);
    }
    match index {
        0 => Ok(sps.scaling.four[0]),
        3 => Ok(sps.scaling.four[3]),
        _ => Ok(four[index - 1]),
    }
}

/// One picture 8x8 list: parsed, or the sequence list.
fn picture_list_8x8(
    reader: &mut BitReader,
    index: usize,
    sps: &Sps,
) -> Result<[u8; 64], D3d12Error> {
    if reader.read_bit("pic_scaling_list_present_flag")? {
        let default = if index.is_multiple_of(2) {
            &DEFAULT_8X8_INTRA
        } else {
            &DEFAULT_8X8_INTER
        };
        return scaling_list(reader, default);
    }
    Ok(sps.scaling.eight[index])
}

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SliceType {
    /// Predictive.
    P,
    /// Bi-predictive.
    B,
    /// Intra.
    I,
}

impl SliceType {
    /// Type for the `slice_type` value.
    fn new(value: u32) -> Result<Self, D3d12Error> {
        if value > 9 {
            return Err(D3d12Error::Parse("slice_type range".to_owned()));
        }
        match value % 5 {
            0 => Ok(Self::P),
            1 => Ok(Self::B),
            2 => Ok(Self::I),
            _ => Err(D3d12Error::Unsupported("sp/si slices".to_owned())),
        }
    }

    /// True for intra slices.
    pub(crate) const fn is_intra(self) -> bool {
        matches!(self, Self::I)
    }

    /// True for bi-predictive slices.
    pub(crate) const fn is_b(self) -> bool {
        matches!(self, Self::B)
    }
}

/// Slice picture order count syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SlicePoc {
    /// Type 0 LSB.
    Lsb(u32),
    /// Type 1 deltas.
    Deltas(i32, Option<i32>),
    /// Type 2 carries none.
    None,
}

/// Decoded reference picture marking operation, H.264 8.2.5.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mmco {
    /// Forget the short-term picture by difference.
    ForgetShort(u32),
    /// Forget the long-term picture by index.
    ForgetLong(u32),
    /// Assign a short-term picture to a long-term index.
    ShortToLong(u32, u32),
    /// Set the maximum long-term index.
    SetMaxLong(u32),
    /// Mark every picture unused for reference.
    MarkAllUnused,
    /// Mark the current picture long-term.
    CurrentToLong,
}

/// Reference marking for one slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Marking {
    /// IDR marking flags.
    Idr {
        /// Discard prior pictures without output.
        no_output: bool,
        /// Current picture is long-term reference.
        long_term: bool,
    },
    /// Adaptive marking operations.
    Adaptive(Vec<Mmco>),
    /// No marking syntax.
    Silent,
}

/// Reference list modification operation, H.264 8.2.4.3.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefMod {
    /// Insert the short-term picture below the prediction.
    ShortBelow(u32),
    /// Insert the short-term picture above the prediction.
    ShortAbove(u32),
    /// Insert the long-term picture by index.
    Long(u32),
}

/// Explicit weights for one reference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WeightEntry {
    /// Luma weight and offset, absent for the default.
    pub(crate) luma: Option<(i32, i32)>,
    /// Chroma weights and offsets, absent for the default.
    pub(crate) chroma: Option<[(i32, i32); 2]>,
}

/// Prediction weight table for one slice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Weights {
    /// Luma weight denominator.
    pub(crate) luma_denom: u8,
    /// Chroma weight denominator.
    pub(crate) chroma_denom: u8,
    /// Explicit L0 weights by reference index.
    pub(crate) l0: Vec<WeightEntry>,
    /// Explicit L1 weights by reference index.
    pub(crate) l1: Vec<WeightEntry>,
}

/// Parsed slice header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SliceHeader {
    /// First macroblock address.
    pub(crate) first_mb: u32,
    /// Slice coding type.
    pub(crate) slice_type: SliceType,
    /// Fixed slice type marker.
    pub(crate) fixed: bool,
    /// Picture set id.
    pub(crate) pps_id: u8,
    /// Frame number.
    pub(crate) frame_num: u32,
    /// IDR id, present on IDR slices.
    pub(crate) idr_pic_id: Option<u16>,
    /// Picture order count syntax.
    pub(crate) poc: SlicePoc,
    /// Spatial direct prediction.
    pub(crate) direct_spatial: bool,
    /// Reference count override.
    pub(crate) ref_override: Option<(u8, u8)>,
    /// L0 reference list modifications.
    pub(crate) ref_mod_l0: Vec<RefMod>,
    /// L1 reference list modifications.
    pub(crate) ref_mod_l1: Vec<RefMod>,
    /// Prediction weight table.
    pub(crate) weights: Weights,
    /// Reference marking.
    pub(crate) marking: Marking,
    /// CABAC init idc.
    pub(crate) cabac_init_idc: u8,
    /// Slice QP delta.
    pub(crate) qp_delta: i32,
    /// Deblocking filter idc.
    pub(crate) deblocking_idc: u8,
    /// Deblocking alpha offset.
    pub(crate) deblocking_alpha: i32,
    /// Deblocking beta offset.
    pub(crate) deblocking_beta: i32,
    /// Bit offset to slice data.
    pub(crate) bit_offset: u16,
}

impl SliceHeader {
    /// Effective L0 reference count minus one.
    pub(crate) fn num_ref_l0(&self, pps: &Pps) -> u8 {
        self.ref_override.map_or(pps.num_ref_l0, |(l0, _)| l0)
    }

    /// Effective L1 reference count minus one.
    pub(crate) fn num_ref_l1(&self, pps: &Pps) -> u8 {
        self.ref_override.map_or(pps.num_ref_l1, |(_, l1)| l1)
    }
}

/// Slice header prefix: address, type, fixed marker, set id.
fn parse_slice_prefix(reader: &mut BitReader) -> Result<(u32, SliceType, bool, u8), D3d12Error> {
    let first_mb = reader.read_ue("first_mb_in_slice")?;
    let raw_type = reader.read_ue("slice_type")?;
    let slice_type = SliceType::new(raw_type)?;
    let pps_id = reader.read_ue("pic_parameter_set_id")?;
    let pps_id = u8::try_from(pps_id)
        .map_err(|_| D3d12Error::Parse("pic_parameter_set_id range".to_owned()))?;
    Ok((first_mb, slice_type, raw_type >= 5, pps_id))
}

/// Slice header against its parameter sets.
fn parse_slice_rest(
    reader: &mut BitReader,
    prefix: (u32, SliceType, bool, u8),
    idr: bool,
    ref_idc: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceHeader, D3d12Error> {
    let (first_mb, slice_type, fixed, pps_id) = prefix;
    if idr && !slice_type.is_intra() {
        return Err(D3d12Error::Parse("non-intra slice in IDR".to_owned()));
    }
    let width = u32::from(sps.log2_max_frame_num_minus4) + 4;
    let frame_num = reader.read_bits(width, "frame_num")?;
    // field_pic_flag absent: the SPS gate admits frame-only streams.
    let idr_pic_id = parse_idr_pic_id(reader, idr)?;
    let poc = parse_slice_poc(reader, sps, pps)?;
    if pps.redundant_present() && reader.read_ue("redundant_pic_cnt")? != 0 {
        return Err(D3d12Error::Unsupported("redundant pictures".to_owned()));
    }
    let mut header = SliceHeader {
        first_mb,
        slice_type,
        fixed,
        pps_id,
        frame_num,
        idr_pic_id,
        poc,
        direct_spatial: false,
        ref_override: None,
        ref_mod_l0: Vec::new(),
        ref_mod_l1: Vec::new(),
        weights: Weights::default(),
        marking: Marking::Silent,
        cabac_init_idc: 0,
        qp_delta: 0,
        deblocking_idc: 0,
        deblocking_alpha: 0,
        deblocking_beta: 0,
        bit_offset: 0,
    };
    parse_slice_inter(reader, &mut header, ref_idc, sps, pps)?;
    parse_slice_tail(reader, &mut header, pps)?;
    let mut offset = reader.position();
    // CABAC slice data starts byte-aligned, DXVA H.264 p32-33.
    if pps.entropy() {
        offset = offset.next_multiple_of(8);
    }
    header.bit_offset =
        u16::try_from(offset).map_err(|_| D3d12Error::Parse("slice header too long".to_owned()))?;
    Ok(header)
}

/// IDR id for IDR slices.
fn parse_idr_pic_id(reader: &mut BitReader, idr: bool) -> Result<Option<u16>, D3d12Error> {
    if !idr {
        return Ok(None);
    }
    let id = reader.read_ue("idr_pic_id")?;
    u16::try_from(id)
        .map(Some)
        .map_err(|_| D3d12Error::Parse("idr_pic_id range".to_owned()))
}

/// Slice POC syntax for the sequence POC method.
fn parse_slice_poc(reader: &mut BitReader, sps: &Sps, pps: &Pps) -> Result<SlicePoc, D3d12Error> {
    match sps.poc_type {
        PocType::Lsb => {
            let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
            let lsb = reader.read_bits(width, "pic_order_cnt_lsb")?;
            if pps.pic_order_present() {
                reader.read_se("delta_pic_order_cnt_bottom")?;
            }
            Ok(SlicePoc::Lsb(lsb))
        }
        PocType::Delta => {
            if sps.delta_zero {
                return Ok(SlicePoc::Deltas(0, None));
            }
            let first = reader.read_se("delta_pic_order_cnt_0")?;
            let second = if pps.pic_order_present() {
                Some(reader.read_se("delta_pic_order_cnt_1")?)
            } else {
                None
            };
            Ok(SlicePoc::Deltas(first, second))
        }
        PocType::FrameNum => Ok(SlicePoc::None),
    }
}

/// Inter section: direct flag, override, list modification, weights, marking.
fn parse_slice_inter(
    reader: &mut BitReader,
    header: &mut SliceHeader,
    ref_idc: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<(), D3d12Error> {
    if header.slice_type.is_intra() {
        return parse_marking(reader, header, ref_idc, header.idr_pic_id.is_some());
    }
    if header.slice_type.is_b() {
        header.direct_spatial = reader.read_bit("direct_spatial_mv_pred_flag")?;
    }
    parse_ref_override(reader, header)?;
    header.ref_mod_l0 = parse_ref_modification(reader, sps)?;
    if header.slice_type.is_b() {
        header.ref_mod_l1 = parse_ref_modification(reader, sps)?;
    }
    parse_weights(reader, header, pps)?;
    parse_marking(reader, header, ref_idc, false)
}

/// Reference count override for inter slices.
fn parse_ref_override(reader: &mut BitReader, header: &mut SliceHeader) -> Result<(), D3d12Error> {
    if !reader.read_bit("num_ref_idx_active_override_flag")? {
        return Ok(());
    }
    let l0 = parse_ref_count(reader, "num_ref_idx_l0_active_minus1")?;
    let mut l1 = 0;
    if header.slice_type.is_b() {
        l1 = parse_ref_count(reader, "num_ref_idx_l1_active_minus1")?;
    }
    header.ref_override = Some((l0, l1));
    Ok(())
}

/// Reference list modification table.
fn parse_ref_modification(reader: &mut BitReader, sps: &Sps) -> Result<Vec<RefMod>, D3d12Error> {
    let mut operations = Vec::new();
    if !reader.read_bit("ref_pic_list_modification_flag")? {
        return Ok(operations);
    }
    loop {
        match reader.read_ue("modification_of_pic_nums_idc")? {
            0 => {
                let diff = reader.read_ue("abs_diff_pic_num_minus1")?;
                if diff >= sps.max_frame_num() {
                    return Err(D3d12Error::Parse("abs_diff_pic_num range".to_owned()));
                }
                operations.push(RefMod::ShortBelow(diff));
            }
            1 => {
                let diff = reader.read_ue("abs_diff_pic_num_minus1")?;
                if diff >= sps.max_frame_num() {
                    return Err(D3d12Error::Parse("abs_diff_pic_num range".to_owned()));
                }
                operations.push(RefMod::ShortAbove(diff));
            }
            2 => {
                let index = reader.read_ue("long_term_pic_num")?;
                if index > u32::from(sps.max_num_ref_frames) {
                    return Err(D3d12Error::Parse("long_term_pic_num range".to_owned()));
                }
                operations.push(RefMod::Long(index));
            }
            3 => return Ok(operations),
            _ => {
                return Err(D3d12Error::Parse(
                    "modification_of_pic_nums_idc range".to_owned(),
                ));
            }
        }
    }
}

/// Prediction weight table.
fn parse_weights(
    reader: &mut BitReader,
    header: &mut SliceHeader,
    pps: &Pps,
) -> Result<(), D3d12Error> {
    let present = (pps.weighted_pred() && !header.slice_type.is_b())
        || (pps.weighted_bipred() == 1 && header.slice_type.is_b());
    if header.slice_type.is_intra() || !present {
        return Ok(());
    }
    let luma_denom = reader.read_ue("luma_log2_weight_denom")?;
    if luma_denom > 7 {
        return Err(D3d12Error::Parse("luma_log2_weight_denom range".to_owned()));
    }
    let chroma_denom = reader.read_ue("chroma_log2_weight_denom")?;
    if chroma_denom > 7 {
        return Err(D3d12Error::Parse(
            "chroma_log2_weight_denom range".to_owned(),
        ));
    }
    let l0 = parse_weight_list(reader, header.num_ref_l0(pps))?;
    let mut l1 = Vec::new();
    if header.slice_type.is_b() {
        l1 = parse_weight_list(reader, header.num_ref_l1(pps))?;
    }
    header.weights = Weights {
        luma_denom: u8::try_from(luma_denom)
            .map_err(|_| D3d12Error::Parse("luma_log2_weight_denom range".to_owned()))?,
        chroma_denom: u8::try_from(chroma_denom)
            .map_err(|_| D3d12Error::Parse("chroma_log2_weight_denom range".to_owned()))?,
        l0,
        l1,
    };
    Ok(())
}

/// One reference weight list.
fn parse_weight_list(
    reader: &mut BitReader,
    count_minus1: u8,
) -> Result<Vec<WeightEntry>, D3d12Error> {
    let mut entries = Vec::new();
    for _ in 0..=count_minus1 {
        let luma = if reader.read_bit("luma_weight_flag")? {
            Some((
                reader.read_se("luma_weight")?,
                reader.read_se("luma_offset")?,
            ))
        } else {
            None
        };
        let chroma = if reader.read_bit("chroma_weight_flag")? {
            Some([
                (
                    reader.read_se("chroma_weight")?,
                    reader.read_se("chroma_offset")?,
                ),
                (
                    reader.read_se("chroma_weight")?,
                    reader.read_se("chroma_offset")?,
                ),
            ])
        } else {
            None
        };
        entries.push(WeightEntry { luma, chroma });
    }
    Ok(entries)
}

/// Reference marking for reference pictures.
fn parse_marking(
    reader: &mut BitReader,
    header: &mut SliceHeader,
    ref_idc: u8,
    idr: bool,
) -> Result<(), D3d12Error> {
    if ref_idc == 0 {
        return Ok(());
    }
    if idr {
        header.marking = Marking::Idr {
            no_output: reader.read_bit("no_output_of_prior_pics_flag")?,
            long_term: reader.read_bit("long_term_reference_flag")?,
        };
        return Ok(());
    }
    if !reader.read_bit("adaptive_ref_pic_marking_mode_flag")? {
        return Ok(());
    }
    let mut operations = Vec::new();
    loop {
        let op = reader.read_ue("memory_management_control_operation")?;
        match op {
            0 => break,
            1 => operations.push(Mmco::ForgetShort(
                reader.read_ue("difference_of_pic_nums_minus1")?,
            )),
            2 => operations.push(Mmco::ForgetLong(reader.read_ue("long_term_pic_num")?)),
            3 => operations.push(Mmco::ShortToLong(
                reader.read_ue("difference_of_pic_nums_minus1")?,
                reader.read_ue("long_term_frame_idx")?,
            )),
            4 => operations.push(Mmco::SetMaxLong(
                reader.read_ue("max_long_term_frame_idx_plus1")?,
            )),
            5 => operations.push(Mmco::MarkAllUnused),
            6 => operations.push(Mmco::CurrentToLong),
            _ => {
                return Err(D3d12Error::Parse(
                    "memory_management_control_operation range".to_owned(),
                ))
            }
        }
    }
    header.marking = Marking::Adaptive(operations);
    Ok(())
}

/// Slice tail: CABAC init, QP delta, deblocking offsets.
fn parse_slice_tail(
    reader: &mut BitReader,
    header: &mut SliceHeader,
    pps: &Pps,
) -> Result<(), D3d12Error> {
    if pps.entropy() && !header.slice_type.is_intra() {
        let idc = reader.read_ue("cabac_init_idc")?;
        header.cabac_init_idc =
            u8::try_from(idc).map_err(|_| D3d12Error::Parse("cabac_init_idc range".to_owned()))?;
    }
    header.qp_delta = reader.read_se("slice_qp_delta")?;
    if !pps.deblocking_present() {
        return Ok(());
    }
    let idc = reader.read_ue("disable_deblocking_filter_idc")?;
    if idc > 2 {
        return Err(D3d12Error::Parse(
            "disable_deblocking_filter_idc range".to_owned(),
        ));
    }
    header.deblocking_idc = u8::try_from(idc)
        .map_err(|_| D3d12Error::Parse("disable_deblocking_filter_idc range".to_owned()))?;
    if idc != 1 {
        header.deblocking_alpha = reader.read_se("slice_alpha_c0_offset_div2")?;
        header.deblocking_beta = reader.read_se("slice_beta_offset_div2")?;
    }
    Ok(())
}

/// One coded slice: header plus its NAL bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSlice {
    /// Parsed slice header.
    pub(crate) header: SliceHeader,
    /// NAL bytes without the start code.
    pub(crate) nal: Vec<u8>,
    /// NAL reference idc.
    pub(crate) ref_idc: u8,
    /// True for IDR slices.
    pub(crate) idr: bool,
}

/// One coded picture: slices sharing frame number and set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    /// Picture slices in bitstream order.
    pub(crate) slices: Vec<PendingSlice>,
    /// True for IDR pictures.
    pub(crate) idr: bool,
}

impl Picture {
    /// True when the picture is used for reference.
    pub(crate) fn ref_pic(&self) -> bool {
        self.slices.first().is_some_and(|slice| slice.ref_idc != 0)
    }

    /// Picture frame number.
    pub(crate) fn frame_num(&self) -> u32 {
        self.slices
            .first()
            .map_or(0, |slice| slice.header.frame_num)
    }

    /// Picture set id.
    pub(crate) fn pps_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.header.pps_id)
    }

    /// True when every slice is intra.
    pub(crate) fn intra(&self) -> bool {
        !self.slices.is_empty()
            && self
                .slices
                .iter()
                .all(|slice| slice.header.slice_type.is_intra())
    }
}

/// Annex B ingest: parameter sets plus picture assembly.
pub(crate) struct Ingest {
    /// Stored sequence sets by id.
    sps: [Option<Sps>; MAX_SPS],
    /// Stored picture sets by id.
    pps: [Option<Pps>; MAX_PPS],
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
}

impl Ingest {
    /// Empty ingest.
    pub(crate) fn new() -> Self {
        Self {
            sps: core::array::from_fn(|_| None),
            pps: core::array::from_fn(|_| None),
            pending: Vec::new(),
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
        let mut pictures = Vec::new();
        for nal in split_nals(data) {
            self.ingest_nal(nal, &mut pictures)?;
        }
        if let Some(picture) = self.complete_picture()? {
            pictures.push(picture);
        }
        Ok(pictures)
    }

    /// Open picture, if any slices arrived without a boundary.
    pub(crate) fn finish(&mut self) -> Result<Option<Picture>, D3d12Error> {
        self.complete_picture()
    }

    /// One NAL unit into the ingest state.
    fn ingest_nal(&mut self, nal: &[u8], pictures: &mut Vec<Picture>) -> Result<(), D3d12Error> {
        let Some(&header) = nal.first() else {
            return Ok(());
        };
        if header >> 7 != 0 {
            return Err(D3d12Error::Parse("nal forbidden bit".to_owned()));
        }
        let ref_idc = (header >> 5) & 3;
        match header & 0x1F {
            1 | 5 => self.ingest_vcl(nal, header, ref_idc, pictures),
            7 => self.ingest_sps(nal),
            8 => self.ingest_pps(nal),
            9..=11 => {
                if let Some(picture) = self.complete_picture()? {
                    pictures.push(picture);
                }
                Ok(())
            }
            2..=4 => Err(D3d12Error::Unsupported("data partitions".to_owned())),
            13..=15 | 20 => Err(D3d12Error::Unsupported("multiview".to_owned())),
            _ => Ok(()),
        }
    }

    /// Coded slice into the open picture.
    fn ingest_vcl(
        &mut self,
        nal: &[u8],
        header: u8,
        ref_idc: u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), D3d12Error> {
        let idr = header & 0x1F == 5;
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(1..) else {
            return Err(D3d12Error::Parse("slice without header".to_owned()));
        };
        let mut reader = BitReader::new(body);
        let prefix = parse_slice_prefix(&mut reader)?;
        let pps = self
            .pps(prefix.3)
            .ok_or_else(|| D3d12Error::Parse("slice cites unknown pps".to_owned()))?;
        let sps = self
            .sps(pps.seq_parameter_set_id)
            .ok_or_else(|| D3d12Error::Parse("slice cites unknown sps".to_owned()))?;
        let parsed = parse_slice_rest(&mut reader, prefix, idr, ref_idc, sps, pps)?;
        if parsed.first_mb == 0 && !self.pending.is_empty() {
            if let Some(picture) = self.complete_picture()? {
                pictures.push(picture);
            }
        }
        self.pending.push(PendingSlice {
            header: parsed,
            nal: nal.to_vec(),
            ref_idc,
            idr,
        });
        Ok(())
    }

    /// Sequence set into the store.
    fn ingest_sps(&mut self, nal: &[u8]) -> Result<(), D3d12Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(1..) else {
            return Err(D3d12Error::Parse("sps without body".to_owned()));
        };
        let sps = parse_sps(body)?;
        let id = sps.seq_parameter_set_id;
        self.sps[usize::from(id)] = Some(sps);
        Ok(())
    }

    /// Picture set into the store.
    fn ingest_pps(&mut self, nal: &[u8]) -> Result<(), D3d12Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(1..) else {
            return Err(D3d12Error::Parse("pps without body".to_owned()));
        };
        let mut probe = BitReader::new(body);
        probe.read_ue("pic_parameter_set_id")?;
        let sps_id = probe.read_ue("seq_parameter_set_id")?;
        let sps_id = u8::try_from(sps_id)
            .map_err(|_| D3d12Error::Parse("seq_parameter_set_id range".to_owned()))?;
        let sps = self
            .sps(sps_id)
            .ok_or_else(|| D3d12Error::Parse("pps cites unknown sps".to_owned()))?
            .clone();
        let pps = parse_pps(body, &sps)?;
        let id = pps.pic_parameter_set_id;
        self.pps[usize::from(id)] = Some(pps);
        Ok(())
    }

    /// Open slices as one validated picture.
    fn complete_picture(&mut self) -> Result<Option<Picture>, D3d12Error> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let slices = core::mem::take(&mut self.pending);
        let first = &slices[0];
        for slice in &slices {
            if slice.header.frame_num != first.header.frame_num
                || slice.header.pps_id != first.header.pps_id
                || slice.idr != first.idr
                || slice.ref_idc != first.ref_idc
            {
                return Err(D3d12Error::Parse("picture slices disagree".to_owned()));
            }
        }
        let mut previous: Option<u32> = None;
        for slice in &slices {
            match previous {
                None => previous = Some(slice.header.first_mb),
                Some(prev) if slice.header.first_mb > prev => {
                    previous = Some(slice.header.first_mb);
                }
                _ => return Err(D3d12Error::Unsupported("arbitrary slice order".to_owned())),
            }
        }
        Ok(Some(Picture {
            idr: first.idr,
            slices,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_pps, parse_slice_prefix, parse_slice_rest, parse_sps, rbsp, split_nals, Ingest,
        Marking, PocType, SlicePoc, SliceType, Sps, DEFAULT_4X4_INTRA,
    };

    /// Test bit writer with emulation prevention.
    struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        fn bit(&mut self, set: bool) {
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
        fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Unsigned Exp-Golomb code.
        fn ue(&mut self, value: u32) {
            let code = value + 1;
            let width = 32 - code.leading_zeros();
            for _ in 1..width {
                self.bit(false);
            }
            self.bits(code, width);
        }

        /// Signed Exp-Golomb code.
        fn se(&mut self, value: i32) {
            let code = if value > 0 {
                u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
            } else {
                u32::try_from(-value).unwrap_or(u32::MAX) * 2
            };
            self.ue(code);
        }

        /// Stop bit plus byte alignment.
        fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            while self.filled != 0 {
                self.bit(false);
            }
            self.bytes
        }

        /// Finished RBSP with emulation prevention inserted.
        fn finish_ebsp(self) -> Vec<u8> {
            let mut out = Vec::new();
            let mut zeros = 0;
            for byte in self.finish() {
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

    /// NAL unit with header over the EBSP body.
    fn nal(nal_type: u8, ref_idc: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![ref_idc << 5 | nal_type];
        nal.extend_from_slice(body);
        nal
    }

    /// Packet of NAL units with four-byte start codes.
    fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut packet = Vec::new();
        for nal in nals {
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(nal);
        }
        packet
    }

    /// Baseline 1920x1088 sequence set body.
    fn sps_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(66, 8);
        writer.bits(0, 8);
        writer.bits(30, 8);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(1);
        writer.bit(false);
        writer.ue(119);
        writer.ue(67);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.finish_ebsp()
    }

    /// Sequence set used across tests.
    fn sps() -> Sps {
        parse_sps(&sps_body()).expect("valid sps")
    }

    /// Picture set body over sequence set zero.
    fn pps_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
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
        writer.bit(true);
        writer.bit(false);
        writer.se(0);
        writer.finish_ebsp()
    }

    /// IDR slice header body over the test sets.
    fn idr_slice_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(2);
        writer.ue(0);
        writer.bits(0, 4);
        writer.ue(0);
        writer.bits(0, 4);
        writer.bit(false);
        writer.bit(false);
        writer.se(0);
        writer.ue(0);
        writer.se(0);
        writer.se(0);
        writer.finish_ebsp()
    }

    /// P slice header body with `frame_num` and `lsb`.
    fn p_slice_body(frame_num: u32, lsb: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bits(frame_num, 4);
        writer.bits(lsb, 4);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.ue(0);
        writer.se(0);
        writer.se(0);
        writer.finish_ebsp()
    }

    #[test]
    fn split_nals_strips_start_codes() {
        let nals = split_nals(b"\0\0\0\x01\x67abc\0\0\x01\x68de");
        assert_eq!(nals, vec![b"\x67abc".as_slice(), b"\x68de".as_slice()]);
    }

    #[test]
    fn rbsp_removes_prevention_bytes() {
        assert_eq!(
            rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 5]),
            vec![0, 0, 1, 0, 0, 0, 5]
        );
    }

    #[test]
    fn sps_reports_coded_size() {
        let sps = sps();
        assert_eq!(sps.level_idc, 30);
        assert_eq!(sps.width(), 1920);
        assert_eq!(sps.height(), 1088);
        assert_eq!(sps.max_frame_num(), 16);
        assert_eq!(sps.poc_type, PocType::Lsb);
        assert_eq!(sps.max_poc_lsb(), 16);
        assert_eq!(sps.visible_width(), 1920);
        assert_eq!(sps.visible_height(), 1088);
        assert_eq!(sps.scaling.four[0][0], 16);
        assert_eq!(sps.scaling.four[0][1], 16);
    }

    #[test]
    fn sps_refuses_depth_and_interlace() {
        let mut writer = BitWriter::new();
        writer.bits(100, 8);
        writer.bits(0, 8);
        writer.bits(40, 8);
        writer.ue(0);
        writer.ue(1);
        writer.ue(1);
        let body = writer.finish_ebsp();
        assert!(parse_sps(&body).is_err());

        let mut writer = BitWriter::new();
        writer.bits(66, 8);
        writer.bits(0, 8);
        writer.bits(30, 8);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(1);
        writer.bit(false);
        writer.ue(7);
        writer.ue(7);
        writer.bit(false);
        let body = writer.finish_ebsp();
        assert!(parse_sps(&body).is_err());
    }

    #[test]
    fn pps_reports_flags_and_defaults() {
        let sps = sps();
        let pps = parse_pps(&pps_body(), &sps).expect("valid pps");
        assert!(pps.entropy());
        assert!(!pps.pic_order_present());
        assert!(pps.deblocking_present());
        assert!(pps.transform_8x8());
        assert_eq!(pps.weighted_bipred(), 0);
        assert_eq!(pps.scaling.four[3], sps.scaling.four[3]);
    }

    #[test]
    fn pps_without_tail_copies_sequence_scaling() {
        let mut writer = BitWriter::new();
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
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        let body = writer.finish_ebsp();
        let sps = sps();
        let pps = parse_pps(&body, &sps).expect("valid pps");
        assert!(!pps.transform_8x8());
        assert_eq!(pps.second_chroma_qp, 0);
        assert_eq!(pps.scaling.eight, sps.scaling.eight);
    }

    #[test]
    fn pps_refuses_fmo() {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.ue(1);
        let body = writer.finish_ebsp();
        assert!(parse_pps(&body, &sps()).is_err());
    }

    #[test]
    fn scaling_list_applies_default_on_zero_delta() {
        let mut writer = BitWriter::new();
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
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.se(-8);
        for _ in 1..8 {
            writer.bit(false);
        }
        writer.se(0);
        let body = writer.finish_ebsp();
        let pps = parse_pps(&body, &sps()).expect("valid pps");
        assert_eq!(pps.scaling.four[0], DEFAULT_4X4_INTRA);
        assert_eq!(pps.scaling.four[1], pps.scaling.four[0]);
    }

    #[test]
    fn slice_header_reports_idr_fields() {
        let body = idr_slice_body();
        let mut reader = crate::bits::BitReader::new(&body);
        let prefix = parse_slice_prefix(&mut reader).expect("prefix");
        assert_eq!(prefix.0, 0);
        assert_eq!(prefix.1, SliceType::I);
        let header = parse_slice_rest(
            &mut reader,
            prefix,
            true,
            2,
            &sps(),
            &parse_pps(&pps_body(), &sps()).expect("pps"),
        )
        .expect("header");
        assert_eq!(header.frame_num, 0);
        assert_eq!(header.idr_pic_id, Some(0));
        assert_eq!(header.poc, SlicePoc::Lsb(0));
        assert_eq!(
            header.marking,
            Marking::Idr {
                no_output: false,
                long_term: false
            }
        );
    }

    #[test]
    fn cabac_slice_data_starts_byte_aligned() {
        let body = idr_slice_body();
        let mut reader = crate::bits::BitReader::new(&body);
        let prefix = parse_slice_prefix(&mut reader).expect("prefix");
        let header = parse_slice_rest(
            &mut reader,
            prefix,
            true,
            2,
            &sps(),
            &parse_pps(&pps_body(), &sps()).expect("pps"),
        )
        .expect("header");
        assert_eq!(header.bit_offset % 8, 0);
    }

    #[test]
    fn slice_header_refuses_sp_type() {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        let body = writer.finish_ebsp();
        let mut reader = crate::bits::BitReader::new(&body);
        assert!(parse_slice_prefix(&mut reader).is_err());
    }

    #[test]
    fn ingest_assembles_packet_picture() {
        let sps_nal = nal(7, 3, &sps_body());
        let pps_nal = nal(8, 3, &pps_body());
        let idr_nal = nal(5, 2, &idr_slice_body());
        let data = packet(&[sps_nal, pps_nal, idr_nal]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("packet");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].idr);
        assert!(pictures[0].ref_pic());
        assert!(pictures[0].intra());
        assert_eq!(pictures[0].frame_num(), 0);

        let p_nal = nal(1, 2, &p_slice_body(1, 2));
        let pictures = ingest.ingest_packet(&packet(&[p_nal])).expect("packet");
        assert_eq!(pictures.len(), 1);
        assert!(!pictures[0].idr);
        assert_eq!(pictures[0].pps_id(), 0);
    }

    #[test]
    fn ingest_splits_on_aud() {
        let first = nal(5, 2, &idr_slice_body());
        let aud = nal(9, 0, &[0x10, 0x80]);
        let second = nal(5, 2, &idr_slice_body());
        let data = packet(&[first, aud, second]);
        let mut ingest = Ingest::new();
        ingest
            .ingest_packet(&packet(&[nal(7, 3, &sps_body()), nal(8, 3, &pps_body())]))
            .expect("sets");
        let pictures = ingest.ingest_packet(&data).expect("packet");
        assert_eq!(pictures.len(), 2);
    }

    #[test]
    fn ingest_rejects_unknown_set_and_aso() {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(2);
        writer.ue(9);
        let body = writer.finish_ebsp();
        let mut ingest = Ingest::new();
        assert!(ingest.ingest_packet(&packet(&[nal(5, 2, &body)])).is_err());

        let mut first = BitWriter::new();
        first.ue(50);
        first.ue(2);
        first.ue(0);
        first.bits(0, 4);
        first.bit(false);
        first.ue(0);
        first.bits(0, 4);
        first.ue(0);
        first.bit(false);
        first.bit(false);
        first.se(0);
        first.ue(0);
        first.se(0);
        first.se(0);
        let mut second = BitWriter::new();
        second.ue(10);
        second.ue(2);
        second.ue(0);
        second.bits(0, 4);
        second.bit(false);
        second.ue(0);
        second.bits(0, 4);
        second.ue(0);
        second.bit(false);
        second.bit(false);
        second.se(0);
        second.ue(0);
        second.se(0);
        second.se(0);
        let mut ingest = Ingest::new();
        ingest
            .ingest_packet(&packet(&[nal(7, 3, &sps_body()), nal(8, 3, &pps_body())]))
            .expect("sets");
        let data = packet(&[
            nal(5, 2, &first.finish_ebsp()),
            nal(5, 2, &second.finish_ebsp()),
        ]);
        assert!(ingest.ingest_packet(&data).is_err());
    }

    #[test]
    fn ingest_rejects_garbage() {
        let mut ingest = Ingest::new();
        assert!(ingest
            .ingest_packet(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00])
            .is_err());
    }
}
