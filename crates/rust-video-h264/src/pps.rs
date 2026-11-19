//! Picture parameter sets, H.264 7.3.2.2.

use crate::bits::BitReader;
use crate::error::{narrow_u8, H264Error, H264Gate, ParseKind, UnsupportedKind};
use crate::scaling::{fallback_scaling, picture_scaling, ScalingLists, ScalingMap};
use crate::sps::Sps;

/// CABAC entropy coding flag bit.
pub const PPS_ENTROPY: u16 = 1;
/// POC delta present flag bit.
pub const PPS_PIC_ORDER: u16 = 1 << 1;
/// Weighted prediction flag bit.
pub const PPS_WEIGHTED_PRED: u16 = 1 << 2;
/// Constrained intra prediction flag bit.
pub const PPS_CONSTRAINED_INTRA: u16 = 1 << 3;
/// Deblocking control present flag bit.
pub const PPS_DEBLOCKING: u16 = 1 << 4;
/// Redundant pictures present flag bit.
pub const PPS_REDUNDANT: u16 = 1 << 5;
/// 8x8 transform mode flag bit.
pub const PPS_TRANSFORM_8X8: u16 = 1 << 6;

/// Parsed picture parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pps {
    /// Set id.
    pub pic_parameter_set_id: u8,
    /// Sequence set id.
    pub seq_parameter_set_id: u8,
    /// Packed flags.
    pub flags: u16,
    /// Default L0 reference count minus one.
    pub num_ref_l0: u8,
    /// Default L1 reference count minus one.
    pub num_ref_l1: u8,
    /// Weighted prediction mode on B slices.
    pub weighted_bipred_idc: u8,
    /// Luma QP init delta.
    pub pic_init_qp: i8,
    /// SP/SI QP init delta.
    pub pic_init_qs: i8,
    /// Chroma QP offset.
    pub chroma_qp: i8,
    /// Second chroma QP offset.
    pub second_chroma_qp: i8,
    /// Resolved picture scaling lists.
    pub scaling: ScalingLists,
    /// Scaling presence, absent without a matrix flag.
    pub scaling_map: Option<ScalingMap>,
}

impl Pps {
    /// CABAC entropy coding.
    #[must_use]
    pub const fn entropy(&self) -> bool {
        self.flags & PPS_ENTROPY != 0
    }

    /// POC delta present in slices.
    #[must_use]
    pub const fn pic_order_present(&self) -> bool {
        self.flags & PPS_PIC_ORDER != 0
    }

    /// Weighted prediction.
    #[must_use]
    pub const fn weighted_pred(&self) -> bool {
        self.flags & PPS_WEIGHTED_PRED != 0
    }

    /// Constrained intra prediction.
    #[must_use]
    pub const fn constrained_intra(&self) -> bool {
        self.flags & PPS_CONSTRAINED_INTRA != 0
    }

    /// Deblocking control present in slices.
    #[must_use]
    pub const fn deblocking_present(&self) -> bool {
        self.flags & PPS_DEBLOCKING != 0
    }

    /// Redundant pictures present in slices.
    #[must_use]
    pub const fn redundant_present(&self) -> bool {
        self.flags & PPS_REDUNDANT != 0
    }

    /// 8x8 transform mode.
    #[must_use]
    pub const fn transform_8x8(&self) -> bool {
        self.flags & PPS_TRANSFORM_8X8 != 0
    }

    /// Weighted biprediction mode.
    #[must_use]
    pub const fn weighted_bipred(&self) -> u8 {
        self.weighted_bipred_idc
    }
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
///
/// A missing set skips the citation check and resolves scaling flat.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range syntax, slice groups, or a
/// set the gate declines.
pub fn parse_pps<E: H264Error, G: H264Gate<E>>(rbsp: &[u8], sps: Option<&Sps>) -> Result<Pps, E> {
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
        return Err(E::unsupported(UnsupportedKind::FlexibleMacroblockOrder));
    }
    let num_ref_l0 = parse_ref_count(&mut reader, "num_ref_idx_l0_default_active_minus1")?;
    let num_ref_l1 = parse_ref_count(&mut reader, "num_ref_idx_l1_default_active_minus1")?;
    if reader.read_bit("weighted_pred_flag")? {
        flags |= PPS_WEIGHTED_PRED;
    }
    let weighted_bipred_idc = narrow_u8(
        reader.read_bits(2, "weighted_bipred_idc")?,
        "weighted_bipred_idc",
    )?;
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
    // H.264 7.4.2.2 absent-tail inference.
    let mut second_chroma_qp = chroma_qp;
    let flat = fallback_scaling();
    let base = sps.map_or(&flat, |sps| &sps.scaling);
    let mut scaling = base.clone();
    let mut map = None;
    if reader.more_data() {
        if reader.read_bit("transform_8x8_mode_flag")? {
            flags |= PPS_TRANSFORM_8X8;
        }
        if reader.read_bit("pic_scaling_matrix_present_flag")? {
            let chroma = sps.map_or(1, |sps| sps.fidelity.chroma_format_idc);
            let resolved = picture_scaling(&mut reader, base, chroma)?;
            scaling = resolved.0;
            map = Some(resolved.1);
        }
        second_chroma_qp = parse_qp(&mut reader, "second_chroma_qp_index_offset")?;
    }
    let pps = Pps {
        pic_parameter_set_id: pps_id,
        seq_parameter_set_id: sps_id,
        flags,
        num_ref_l0,
        num_ref_l1,
        weighted_bipred_idc,
        pic_init_qp: luma_qp,
        pic_init_qs: switch_qs,
        chroma_qp,
        second_chroma_qp,
        scaling,
        scaling_map: map,
    };
    if let Some(sps) = sps {
        G::check_pps(&pps, sps)?;
    }
    Ok(pps)
}

/// Picture and sequence ids heading one picture set.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range ids.
pub fn parse_pps_ids<E: H264Error>(rbsp: &[u8]) -> Result<(u8, u8), E> {
    let mut reader = BitReader::new(rbsp);
    let pps_id = parse_pps_id(&mut reader)?;
    let sps_id = narrow_u8(
        reader.read_ue("seq_parameter_set_id")?,
        "seq_parameter_set_id",
    )?;
    Ok((pps_id, sps_id))
}

/// Set id below the set count.
fn parse_pps_id<E: H264Error>(reader: &mut BitReader<E>) -> Result<u8, E> {
    narrow_u8(
        reader.read_ue("pic_parameter_set_id")?,
        "pic_parameter_set_id",
    )
}

/// Sequence id matching the supplied sequence set, when present.
fn parse_pps_sps_id<E: H264Error>(reader: &mut BitReader<E>, sps: Option<&Sps>) -> Result<u8, E> {
    let id = narrow_u8(
        reader.read_ue("seq_parameter_set_id")?,
        "seq_parameter_set_id",
    )?;
    if let Some(sps) = sps {
        if id != sps.seq_parameter_set_id {
            return Err(E::parse(ParseKind::PpsCitesUnknownSps));
        }
    }
    Ok(id)
}

/// Reference count field below the 32 ceiling.
fn parse_ref_count<E: H264Error>(reader: &mut BitReader<E>, syntax: &'static str) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if value > 31 {
        return Err(E::parse(ParseKind::Range { syntax }));
    }
    narrow_u8(value, syntax)
}

/// QP delta field.
fn parse_qp<E: H264Error>(reader: &mut BitReader<E>, syntax: &'static str) -> Result<i8, E> {
    let value = reader.read_se(syntax)?;
    i8::try_from(value).map_err(|_| E::parse(ParseKind::Range { syntax }))
}

#[cfg(test)]
mod tests {
    use crate::error::test_support::TestError;
    use crate::error::{ParseKind, PermissiveGate, StrictGate};
    use crate::scaling::DEFAULT_4X4_INTRA;
    use crate::tests::{pps_body, pps_body_bipred, pps_body_chroma_no_tail, sps};

    use super::parse_pps;

    use rust_video_harness::Bits;

    #[test]
    fn pps_reports_flags_and_defaults() {
        let sps = sps();
        let pps = parse_pps::<TestError, StrictGate>(&pps_body(), Some(&sps)).expect("valid pps");
        assert!(pps.entropy());
        assert!(!pps.pic_order_present());
        assert!(pps.deblocking_present());
        assert!(pps.transform_8x8());
        assert_eq!(pps.weighted_bipred(), 0);
        assert_eq!(pps.scaling.four[3], sps.scaling.four[3]);
    }

    #[test]
    fn pps_without_tail_copies_sequence_scaling() {
        let mut writer = Bits::new();
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
        let body = writer.finish();
        let sps = sps();
        let pps = parse_pps::<TestError, StrictGate>(&body, Some(&sps)).expect("valid pps");
        assert!(!pps.transform_8x8());
        assert_eq!(pps.second_chroma_qp, 0);
        assert_eq!(pps.scaling.eight, sps.scaling.eight);
    }

    #[test]
    fn absent_tail_infers_chroma_qp() {
        let body = pps_body_chroma_no_tail(-2);
        let sps = sps();
        let strict = parse_pps::<TestError, StrictGate>(&body, Some(&sps)).expect("strict pps");
        assert_eq!(strict.second_chroma_qp, -2);
        let lenient =
            parse_pps::<TestError, PermissiveGate>(&body, Some(&sps)).expect("lenient pps");
        assert_eq!(lenient.second_chroma_qp, -2);
    }

    #[test]
    fn pps_refuses_fmo() {
        let mut writer = Bits::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.ue(1);
        let body = writer.finish();
        assert!(parse_pps::<TestError, StrictGate>(&body, Some(&sps())).is_err());
    }

    #[test]
    fn gates_split_on_reserved_bipred() {
        let body = pps_body_bipred(3);
        assert_eq!(
            parse_pps::<TestError, StrictGate>(&body, Some(&sps())).unwrap_err(),
            TestError::Parse(ParseKind::Range {
                syntax: "weighted_bipred_idc",
            })
        );
        let pps = parse_pps::<TestError, PermissiveGate>(&body, Some(&sps())).expect("lenient pps");
        assert_eq!(pps.weighted_bipred_idc, 3);
        assert_eq!(pps.weighted_bipred(), 3);
    }

    #[test]
    fn scaling_list_applies_default_on_zero_delta() {
        let mut writer = Bits::new();
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
        let body = writer.finish();
        let pps = parse_pps::<TestError, StrictGate>(&body, Some(&sps())).expect("valid pps");
        assert_eq!(pps.scaling.four[0], DEFAULT_4X4_INTRA);
        assert_eq!(pps.scaling.four[1], pps.scaling.four[0]);
        let map = pps.scaling_map.expect("present map");
        assert_eq!((map.present_mask, map.default_mask), (1, 1));
    }
}
