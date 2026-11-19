//! Sequence parameter sets, H.264 7.3.2.1.

use crate::bits::BitReader;
use crate::error::{narrow_u8, H264Error, H264Gate, ParseKind, UnsupportedKind};
use crate::nal::{Crop, MAX_SPS};
use crate::scaling::{fallback_scaling, sequence_scaling, ScalingMap, ScalingSets};
use crate::vui::{parse_vui, VuiParams};

/// Picture order count method, H.264 8.2.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PocType {
    /// LSB in slice headers.
    Lsb,
    /// Deltas in slice headers.
    Delta,
    /// Twice the frame number.
    FrameNum,
}

impl PocType {
    /// Method for `value`.
    fn new<E: H264Error>(value: u32) -> Result<Self, E> {
        match value {
            0 => Ok(Self::Lsb),
            1 => Ok(Self::Delta),
            2 => Ok(Self::FrameNum),
            _ => Err(E::parse(ParseKind::Range {
                syntax: "pic_order_cnt_type",
            })),
        }
    }

    /// Type number.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Lsb => 0,
            Self::Delta => 1,
            Self::FrameNum => 2,
        }
    }
}

/// Fidelity-range extensions of one sequence set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FidelityRange {
    /// Chroma sampling indicator.
    pub chroma_format_idc: u8,
    /// Separate colour plane coding.
    pub separate_plane: bool,
    /// Luma bit depth minus 8.
    pub bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    pub bit_depth_chroma_minus8: u8,
    /// Transform bypass on lossless slices.
    pub transform_bypass: bool,
}

/// Parsed sequence parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sps {
    /// Coded video sequence level.
    pub level_idc: u8,
    /// Set id.
    pub seq_parameter_set_id: u8,
    /// Coded video sequence profile.
    pub profile_idc: u8,
    /// Constraint-set flags with reserved bits.
    pub constraints: u8,
    /// Resolved sequence scaling lists.
    pub scaling: ScalingSets,
    /// Scaling presence, absent without a matrix flag.
    pub scaling_map: Option<ScalingMap>,
    /// Fidelity-range extensions.
    pub fidelity: FidelityRange,
    /// Frame number width minus 4.
    pub log2_max_frame_num_minus4: u8,
    /// Picture order count method.
    pub poc_type: PocType,
    /// POC LSB width minus 4.
    pub log2_max_poc_lsb_minus4: u8,
    /// POC deltas absent and zero.
    pub delta_zero: bool,
    /// Non-reference POC offset.
    pub offset_non_ref: i32,
    /// Bottom-field POC offset.
    pub offset_top_bottom: i32,
    /// Reference cycle length.
    pub cycle: u8,
    /// Reference cycle offsets.
    pub cycle_offsets: [i32; 255],
    /// Expected POC delta per cycle.
    pub expected_delta: i32,
    /// Maximum reference frame count.
    pub max_num_ref_frames: u8,
    /// Frame number gaps allowed.
    pub gaps_allowed: bool,
    /// Coded width in macroblocks minus one.
    pub width_in_mbs_minus1: u16,
    /// Coded height in macroblocks minus one.
    pub height_in_mbs_minus1: u16,
    /// Direct 8x8 inference.
    pub direct_8x8: bool,
    /// Crop offsets left, right, top, bottom, present with the window.
    pub crop_offsets: Option<[u32; 4]>,
    /// Visible rectangle, absent when uncropped or outside the coded size.
    pub crop: Option<Crop>,
    /// VUI parameters, when present.
    pub vui: Option<VuiParams>,
}

impl Sps {
    /// Coded width in luma samples.
    #[must_use]
    pub fn width(&self) -> u32 {
        (u32::from(self.width_in_mbs_minus1) + 1) * 16
    }

    /// Coded height in luma samples.
    #[must_use]
    pub fn height(&self) -> u32 {
        (u32::from(self.height_in_mbs_minus1) + 1) * 16
    }

    /// Maximum frame number.
    #[must_use]
    pub const fn max_frame_num(&self) -> u32 {
        1 << (self.log2_max_frame_num_minus4 + 4)
    }

    /// Maximum POC LSB.
    #[must_use]
    pub const fn max_poc_lsb(&self) -> u32 {
        1 << (self.log2_max_poc_lsb_minus4 + 4)
    }

    /// Visible width in luma samples.
    #[must_use]
    pub fn visible_width(&self) -> u32 {
        self.crop.map_or_else(
            || self.width(),
            |crop| self.width() - crop.left - crop.right,
        )
    }

    /// Visible height in luma samples.
    #[must_use]
    pub fn visible_height(&self) -> u32 {
        self.crop.map_or_else(
            || self.height(),
            |crop| self.height() - crop.top - crop.bottom,
        )
    }

    /// VUI reorder ceiling, absent without a bitstream restriction.
    #[must_use]
    pub fn num_reorder_frames(&self) -> Option<u32> {
        self.vui.as_ref().and_then(|vui| vui.max_num_reorder_frames)
    }

    /// Constraint-set flag `index`, H.264 7.4.2.1.
    #[must_use]
    pub const fn constraint_set(&self, index: u8) -> bool {
        index < 6 && (self.constraints >> (7 - index)) & 1 != 0
    }

    /// Crop unit in luma samples per chroma sampling, H.264 7.4.2.1.
    #[must_use]
    pub const fn crop_unit(&self) -> (u32, u32) {
        let unit_x = match self.fidelity.chroma_format_idc {
            1 | 2 => 2,
            _ => 1,
        };
        let unit_y = match self.fidelity.chroma_format_idc {
            1 => 2,
            _ => 1,
        };
        (unit_x, unit_y)
    }

    /// Display picture size in luma samples, cropping applied.
    #[must_use]
    pub fn display_size(&self) -> (u32, u32) {
        let (mut width, mut height) = (self.width(), self.height());
        if let Some(crop) = self.crop_offsets {
            let (unit_x, unit_y) = self.crop_unit();
            width = width.saturating_sub((crop[0] + crop[1]) * unit_x);
            height = height.saturating_sub((crop[2] + crop[3]) * unit_y);
        }
        (width, height)
    }

    /// Crop left and top in luma samples, zero without a window.
    #[must_use]
    pub fn crop_origin(&self) -> (u32, u32) {
        let Some(crop) = self.crop_offsets else {
            return (0, 0);
        };
        let (unit_x, unit_y) = self.crop_unit();
        (
            crop[0].saturating_mul(unit_x),
            crop[2].saturating_mul(unit_y),
        )
    }

    /// Frame-number offset of `frame_num`, H.264 8.2.1.
    ///
    /// `prev` carries the previous reference number and offset; `None`
    /// resets the offset on IDR pictures, MMCO 5, and the first picture.
    #[must_use]
    pub fn frame_num_offset(&self, prev: Option<(u32, i64)>, frame_num: u32) -> i64 {
        let Some((prev_num, prev_offset)) = prev else {
            return 0;
        };
        if prev_num > frame_num {
            prev_offset.saturating_add(i64::from(self.max_frame_num()))
        } else {
            prev_offset
        }
    }

    /// Type-0 picture order count, H.264 8.2.1.1.
    ///
    /// `prev` carries the previous reference MSB and LSB; `None` resets
    /// the MSB on IDR pictures and the first picture. `None` answers a
    /// count outside the `i32` range.
    #[must_use]
    pub fn poc_type0(&self, lsb: u32, delta_bottom: i32, prev: Option<(i64, i64)>) -> Option<i32> {
        let max = i64::from(self.max_poc_lsb());
        let lsb = i64::from(lsb);
        let msb = prev.map_or(0, |pair| crate::derive::poc_msb(lsb, max, pair));
        let top = msb + lsb;
        let bottom = top + i64::from(delta_bottom);
        i32::try_from(top.min(bottom)).ok()
    }

    /// Expected picture order count for `abs_frame_num`, H.264 8.2.1.2.
    #[must_use]
    pub fn poc_cycle_expected(&self, abs_frame_num: i64) -> i64 {
        if abs_frame_num <= 0 || self.cycle == 0 {
            return 0;
        }
        let cycle = i64::from(self.cycle);
        let full = (abs_frame_num - 1) / cycle;
        let rest = (abs_frame_num - 1) % cycle;
        let mut expected = full.saturating_mul(i64::from(self.expected_delta));
        for offset in self
            .cycle_offsets
            .iter()
            .take(usize::try_from(rest + 1).unwrap_or(0))
        {
            expected = expected.saturating_add(i64::from(*offset));
        }
        expected
    }

    /// Type-1 picture order count, H.264 8.2.1.2.
    ///
    /// `None` answers a count outside the `i32` range.
    #[must_use]
    pub fn poc_type1(
        &self,
        abs_frame_num: i64,
        delta0: i32,
        delta1: i32,
        is_reference: bool,
    ) -> Option<i32> {
        let expected = self.poc_cycle_expected(abs_frame_num);
        let mut top = expected.saturating_add(i64::from(delta0));
        let mut bottom = top
            .saturating_add(i64::from(self.offset_top_bottom))
            .saturating_add(i64::from(delta1));
        if !is_reference {
            top = top.saturating_add(i64::from(self.offset_non_ref));
            bottom = bottom.saturating_add(i64::from(self.offset_non_ref));
        }
        i32::try_from(top.min(bottom)).ok()
    }
}

/// Sequence parameter set from RBSP `rbsp`.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range syntax, interlaced fields,
/// or a set the gate declines.
pub fn parse_sps<E: H264Error, G: H264Gate<E>>(rbsp: &[u8]) -> Result<Sps, E> {
    let mut reader = BitReader::new(rbsp);
    let profile_idc = narrow_u8(reader.read_bits(8, "profile_idc")?, "profile_idc")?;
    let constraints = narrow_u8(reader.read_bits(8, "constraints")?, "constraints")?;
    let level_idc = narrow_u8(reader.read_bits(8, "level_idc")?, "level_idc")?;
    let sps_id = parse_sps_id(&mut reader)?;
    let (fidelity, scaling, scaling_map) = parse_sps_chroma(&mut reader, profile_idc)?;
    let log2_max_frame_num_minus4 = parse_log2(&mut reader, "log2_max_frame_num")?;
    let poc = parse_sps_poc(&mut reader)?;
    let max_num_ref_frames =
        narrow_u8(reader.read_ue("max_num_ref_frames")?, "max_num_ref_frames")?;
    let gaps_allowed = reader.read_bit("gaps_in_frame_num")?;
    let width_in_mbs_minus1 = parse_mbs(&mut reader, "pic_width_in_mbs")?;
    let height_in_mbs_minus1 = parse_mbs(&mut reader, "pic_height_in_map_units")?;
    if !reader.read_bit("frame_mbs_only_flag")? {
        return Err(E::unsupported(UnsupportedKind::Interlaced));
    }
    let direct_8x8 = reader.read_bit("direct_8x8_inference_flag")?;
    let (crop_offsets, crop) = parse_crop(&mut reader, width_in_mbs_minus1, height_in_mbs_minus1)?;
    let vui = parse_vui(&mut reader)?;
    let sps = Sps {
        level_idc,
        seq_parameter_set_id: sps_id,
        profile_idc,
        constraints,
        scaling,
        scaling_map,
        fidelity,
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
        crop_offsets,
        crop,
        vui,
    };
    G::check_sps(&sps)?;
    Ok(sps)
}

/// Set id below the set count.
fn parse_sps_id<E: H264Error>(reader: &mut BitReader<E>) -> Result<u8, E> {
    let id = narrow_u8(
        reader.read_ue("seq_parameter_set_id")?,
        "seq_parameter_set_id",
    )?;
    if usize::from(id) >= MAX_SPS {
        return Err(E::parse(ParseKind::Range {
            syntax: "seq_parameter_set_id",
        }));
    }
    Ok(id)
}

/// Base fidelity-range extensions for profiles without the syntax.
fn base_fidelity() -> FidelityRange {
    FidelityRange {
        chroma_format_idc: 1,
        separate_plane: false,
        bit_depth_luma_minus8: 0,
        bit_depth_chroma_minus8: 0,
        transform_bypass: false,
    }
}

/// Chroma, depth, and scaling section of a sequence set.
fn parse_sps_chroma<E: H264Error>(
    reader: &mut BitReader<E>,
    profile_idc: u8,
) -> Result<(FidelityRange, ScalingSets, Option<ScalingMap>), E> {
    if !matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 134 | 135 | 138 | 139
    ) {
        return Ok((base_fidelity(), fallback_scaling(), None));
    }
    let chroma = narrow_u8(reader.read_ue("chroma_format_idc")?, "chroma_format_idc")?;
    if chroma > 3 {
        return Err(E::parse(ParseKind::Range {
            syntax: "chroma_format_idc",
        }));
    }
    let fidelity = FidelityRange {
        chroma_format_idc: chroma,
        separate_plane: chroma == 3 && reader.read_bit("separate_colour_plane_flag")?,
        bit_depth_luma_minus8: narrow_u8(
            reader.read_ue("bit_depth_luma_minus8")?,
            "bit_depth_luma_minus8",
        )?,
        bit_depth_chroma_minus8: narrow_u8(
            reader.read_ue("bit_depth_chroma_minus8")?,
            "bit_depth_chroma_minus8",
        )?,
        transform_bypass: reader.read_bit("qpprime_y_zero_transform_bypass_flag")?,
    };
    if !reader.read_bit("seq_scaling_matrix_present_flag")? {
        return Ok((fidelity, fallback_scaling(), None));
    }
    let (scaling, map) = sequence_scaling(reader, chroma)?;
    Ok((fidelity, scaling, Some(map)))
}

/// Log2 width field below the 12 ceiling.
fn parse_log2<E: H264Error>(reader: &mut BitReader<E>, syntax: &'static str) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if value > 12 {
        return Err(E::parse(ParseKind::Range { syntax }));
    }
    narrow_u8(value, syntax)
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
fn parse_sps_poc<E: H264Error>(reader: &mut BitReader<E>) -> Result<PocSection, E> {
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
fn parse_poc_cycle<E: H264Error>(
    reader: &mut BitReader<E>,
    section: &mut PocSection,
) -> Result<(), E> {
    section.delta_zero = reader.read_bit("delta_pic_order_always_zero_flag")?;
    section.offset_non_ref = reader.read_se("offset_for_non_ref_pic")?;
    section.offset_top_bottom = reader.read_se("offset_for_top_to_bottom_field")?;
    let count = reader.read_ue("num_ref_frames_in_pic_order_cnt_cycle")?;
    section.cycle = narrow_u8(count, "pic_order_cnt_cycle")?;
    let mut total: i64 = 0;
    for slot in section.offsets.iter_mut().take(usize::from(section.cycle)) {
        *slot = reader.read_se("offset_for_ref_frame")?;
        total += i64::from(*slot);
    }
    section.expected = i32::try_from(total).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "pic_order_cnt_cycle",
        })
    })?;
    Ok(())
}

/// Macroblock count field.
fn parse_mbs<E: H264Error>(reader: &mut BitReader<E>, syntax: &'static str) -> Result<u16, E> {
    let value = reader.read_ue(syntax)?;
    u16::try_from(value).map_err(|_| E::parse(ParseKind::Range { syntax }))
}

/// Frame crop raw offsets with the rectangle validated against the coded size.
fn parse_crop<E: H264Error>(
    reader: &mut BitReader<E>,
    width_in_mbs: u16,
    height_in_mbs: u16,
) -> Result<(Option<[u32; 4]>, Option<Crop>), E> {
    if !reader.read_bit("frame_cropping_flag")? {
        return Ok((None, None));
    }
    let left = reader.read_ue("frame_crop_left_offset")?;
    let right = reader.read_ue("frame_crop_right_offset")?;
    let top = reader.read_ue("frame_crop_top_offset")?;
    let bottom = reader.read_ue("frame_crop_bottom_offset")?;
    let width = (u32::from(width_in_mbs) + 1) * 16;
    let height = (u32::from(height_in_mbs) + 1) * 16;
    let horizontal = left.checked_add(right).and_then(|sum| sum.checked_mul(2));
    let vertical = top.checked_add(bottom).and_then(|sum| sum.checked_mul(2));
    let crop = match (horizontal, vertical) {
        (Some(consumed), Some(rows)) if consumed < width && rows < height => Some(Crop {
            left: left * 2,
            right: right * 2,
            top: top * 2,
            bottom: bottom * 2,
        }),
        _ => None,
    };
    Ok((Some([left, right, top, bottom]), crop))
}

#[cfg(test)]
mod tests {
    use crate::error::test_support::TestError;
    use crate::error::{ParseKind, PermissiveGate, StrictGate, UnsupportedKind};
    use crate::tests::{crop_sps_body, fidelity_sps_body, sps, sps_body_vui};

    use super::{parse_sps, PocType, Sps};

    use rust_video_harness::Bits;

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
    fn sps_without_vui_has_no_reorder_ceiling() {
        assert_eq!(sps().num_reorder_frames(), None);
    }

    #[test]
    fn sps_vui_without_restriction_has_no_ceiling() {
        let body = sps_body_vui(None);
        let bytes = crate::nal::rbsp(&body);
        let sps = parse_sps::<TestError, StrictGate>(&bytes).expect("vui sps");
        assert_eq!(sps.num_reorder_frames(), None);
    }

    #[test]
    fn sps_restriction_reports_reorder_ceiling() {
        for reorder in [0, 2] {
            let body = sps_body_vui(Some(reorder));
            let bytes = crate::nal::rbsp(&body);
            let sps = parse_sps::<TestError, StrictGate>(&bytes).expect("restricted sps");
            assert_eq!(sps.num_reorder_frames(), Some(reorder));
        }
    }

    #[test]
    fn sps_refuses_depth_and_interlace() {
        let mut writer = Bits::new();
        writer.bits(100, 8);
        writer.bits(0, 8);
        writer.bits(40, 8);
        writer.ue(0);
        writer.ue(1);
        writer.ue(1);
        let body = writer.finish();
        assert!(parse_sps::<TestError, StrictGate>(&body).is_err());

        let mut writer = Bits::new();
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
        let body = writer.finish();
        assert!(parse_sps::<TestError, StrictGate>(&body).is_err());
    }

    #[test]
    fn gates_split_on_sampling() {
        let body = fidelity_sps_body(2);
        assert_eq!(
            parse_sps::<TestError, StrictGate>(&body).unwrap_err(),
            TestError::Unsupported(UnsupportedKind::ChromaOutside420)
        );
        let sps = parse_sps::<TestError, PermissiveGate>(&body).expect("permissive sps");
        assert_eq!(sps.fidelity.chroma_format_idc, 2);
    }

    #[test]
    fn gates_split_on_bad_crop() {
        let body = crop_sps_body(1000, 1000, 0, 0);
        assert_eq!(
            parse_sps::<TestError, StrictGate>(&body).unwrap_err(),
            TestError::Parse(ParseKind::FrameCropOutsideCodedSize)
        );
        let sps = parse_sps::<TestError, PermissiveGate>(&body).expect("permissive sps");
        assert_eq!(sps.crop_offsets, Some([1000, 1000, 0, 0]));
        assert_eq!(sps.crop, None);
    }

    #[test]
    fn valid_crop_reports_rectangle() {
        let body = crop_sps_body(1, 2, 3, 4);
        let sps = parse_sps::<TestError, StrictGate>(&body).expect("cropped sps");
        assert_eq!(sps.crop_offsets, Some([1, 2, 3, 4]));
        let crop = sps.crop.expect("validated crop");
        assert_eq!((crop.left, crop.right, crop.top, crop.bottom), (2, 4, 6, 8));
    }

    #[test]
    fn constraint_set_reads_flag_bits() {
        let sps = Sps {
            constraints: 0b1010_0000,
            ..sps()
        };
        assert!(sps.constraint_set(0));
        assert!(!sps.constraint_set(1));
        assert!(sps.constraint_set(2));
        assert!(!sps.constraint_set(6));
    }

    #[test]
    fn display_geometry_applies_crop_units() {
        assert_eq!(sps().display_size(), (1920, 1088));
        assert_eq!(sps().crop_origin(), (0, 0));
        let body = crop_sps_body(1, 2, 3, 4);
        let sps = parse_sps::<TestError, StrictGate>(&body).expect("cropped sps");
        assert_eq!(sps.crop_unit(), (2, 2));
        assert_eq!(sps.display_size(), (1914, 1074));
        assert_eq!(sps.crop_origin(), (2, 6));
    }

    #[test]
    fn frame_num_offset_wraps_past_previous() {
        let sps = sps();
        assert_eq!(sps.frame_num_offset(None, 7), 0);
        assert_eq!(sps.frame_num_offset(Some((5, 0)), 7), 0);
        assert_eq!(sps.frame_num_offset(Some((7, 0)), 5), 16);
        assert_eq!(sps.frame_num_offset(Some((7, 32)), 5), 48);
    }

    #[test]
    fn type0_poc_wraps_msb_and_mins_bottom() {
        let sps = sps();
        assert_eq!(sps.poc_type0(5, 0, None), Some(5));
        assert_eq!(sps.poc_type0(15, 0, Some((0, 0))), Some(-1));
        assert_eq!(sps.poc_type0(2, 0, Some((16, 2))), Some(18));
        assert_eq!(sps.poc_type0(5, -3, None), Some(2));
    }

    /// Sequence set with a two-entry type-1 POC cycle.
    fn cycle_sps() -> Sps {
        let mut offsets = [0; 255];
        offsets[0] = 2;
        offsets[1] = 4;
        Sps {
            poc_type: PocType::Delta,
            cycle: 2,
            cycle_offsets: offsets,
            expected_delta: 6,
            offset_non_ref: 3,
            offset_top_bottom: 1,
            ..sps()
        }
    }

    #[test]
    fn cycle_expected_counts_full_cycles_plus_prefix() {
        let sps = cycle_sps();
        assert_eq!(sps.poc_cycle_expected(0), 0);
        assert_eq!(sps.poc_cycle_expected(1), 2);
        assert_eq!(sps.poc_cycle_expected(2), 6);
        assert_eq!(sps.poc_cycle_expected(3), 8);
    }

    #[test]
    fn type1_poc_mins_top_and_bottom() {
        let sps = cycle_sps();
        assert_eq!(sps.poc_type1(2, 1, 0, true), Some(7));
        assert_eq!(sps.poc_type1(2, 1, 0, false), Some(10));
        assert_eq!(sps.poc_type1(2, 1, -9, true), Some(-1));
    }
}
