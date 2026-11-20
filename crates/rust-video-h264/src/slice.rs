//! Slice headers, H.264 7.3.3.

use crate::bits::BitReader;
use crate::error::{narrow_u16, narrow_u8, H264Error, H264Gate, ParseReason, UnsupportedReason};
use crate::pps::Pps;
use crate::sps::{PocType, Sps};

/// Reference marking for one slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Marking {
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

/// Decoded reference picture marking operation, H.264 8.2.5.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mmco {
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
    /// Mark the current picture long-term by index.
    CurrentToLong(u32),
}

/// Reference list modification operation, H.264 8.2.4.3.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefMod {
    /// Insert the short-term picture below the prediction.
    ShortBelow(u32),
    /// Insert the short-term picture above the prediction.
    ShortAbove(u32),
    /// Insert the long-term picture by index.
    Long(u32),
}

/// Slice picture order count syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlicePoc {
    /// Type 0 LSB with its bottom-field delta.
    Lsb(u32, i32),
    /// Type 1 deltas.
    Deltas(i32, Option<i32>),
    /// Type 2 carries none.
    None,
}

impl Marking {
    /// True when the marking resets every reference picture.
    #[must_use]
    pub fn has_mmco5(&self) -> bool {
        match self {
            Self::Adaptive(operations) => operations.contains(&Mmco::MarkAllUnused),
            Self::Idr { .. } | Self::Silent => false,
        }
    }

    /// Long-term index the current picture takes, if assigned.
    #[must_use]
    pub fn current_to_long(&self) -> Option<u32> {
        match self {
            Self::Idr { long_term, .. } => long_term.then_some(0),
            Self::Adaptive(operations) => operations.iter().find_map(|op| match op {
                Mmco::CurrentToLong(index) => Some(*index),
                _ => None,
            }),
            Self::Silent => None,
        }
    }
}

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SliceType {
    /// Predictive.
    P,
    /// Bi-predictive.
    B,
    /// Intra.
    I,
}

impl SliceType {
    /// Type for the `slice_type` value.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a value above 9 or an SP/SI slice type.
    pub fn new<E: H264Error>(value: u32) -> Result<Self, E> {
        if value > 9 {
            return Err(E::parse(ParseReason::Range {
                syntax: "slice_type",
            }));
        }
        match value % 5 {
            0 => Ok(Self::P),
            1 => Ok(Self::B),
            2 => Ok(Self::I),
            _ => Err(E::unsupported(UnsupportedReason::SpSiSlices)),
        }
    }

    /// True for intra slices.
    #[must_use]
    pub const fn is_intra(self) -> bool {
        matches!(self, Self::I)
    }

    /// True for bi-predictive slices.
    #[must_use]
    pub const fn is_b(self) -> bool {
        matches!(self, Self::B)
    }

    /// Slice type number.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::P => 0,
            Self::B => 1,
            Self::I => 2,
        }
    }
}

/// Parsed slice header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceHeader {
    /// First macroblock address.
    pub first_mb: u32,
    /// Slice coding type.
    pub slice_type: SliceType,
    /// Fixed slice type marker.
    pub fixed: bool,
    /// Picture set id.
    pub pps_id: u8,
    /// Frame number.
    pub frame_num: u32,
    /// IDR id, present on IDR slices.
    pub idr_pic_id: Option<u16>,
    /// Picture order count syntax.
    pub poc: SlicePoc,
    /// Spatial direct prediction.
    pub direct_spatial: bool,
    /// Reference count override.
    pub ref_override: Option<(u8, u8)>,
    /// L0 reference list modifications.
    pub ref_mod_l0: Vec<RefMod>,
    /// L1 reference list modifications.
    pub ref_mod_l1: Vec<RefMod>,
    /// Prediction weight table.
    pub weights: SliceWeights,
    /// Reference marking.
    pub marking: Marking,
    /// Redundant picture count.
    pub redundant_pic_cnt: u32,
    /// CABAC init idc.
    pub cabac_init_idc: u8,
    /// Slice QP delta.
    pub qp_delta: i32,
    /// Deblocking filter idc.
    pub deblocking_idc: u8,
    /// Deblocking alpha offset.
    pub deblocking_alpha: i32,
    /// Deblocking beta offset.
    pub deblocking_beta: i32,
    /// Bit offset to slice data.
    pub bit_offset: u16,
}

impl SliceHeader {
    /// Effective L0 reference count minus one.
    #[must_use]
    pub fn num_ref_l0(&self, pps: &Pps) -> u8 {
        self.ref_override.map_or(pps.num_ref_l0, |(l0, _)| l0)
    }

    /// Effective L1 reference count minus one.
    #[must_use]
    pub fn num_ref_l1(&self, pps: &Pps) -> u8 {
        self.ref_override.map_or(pps.num_ref_l1, |(_, l1)| l1)
    }
}

/// Explicit weights for one reference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WeightEntry {
    /// Luma weight and offset, absent for the default.
    pub luma: Option<(i32, i32)>,
    /// Chroma weights and offsets, absent for the default.
    pub chroma: Option<[(i32, i32); 2]>,
}

/// Prediction weight table for one slice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SliceWeights {
    /// Luma weight denominator.
    pub luma_denom: u8,
    /// Chroma weight denominator.
    pub chroma_denom: u8,
    /// Explicit L0 weights by reference index.
    pub l0: Vec<WeightEntry>,
    /// Explicit L1 weights by reference index.
    pub l1: Vec<WeightEntry>,
}

pub use self::SliceWeights as Weights;

/// Slice header prefix: address, type, fixed marker, set id.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range syntax, or SP/SI slices.
pub fn parse_slice_prefix<E: H264Error>(
    reader: &mut BitReader<E>,
) -> Result<(u32, SliceType, bool, u8), E> {
    let first_mb = reader.read_ue("first_mb_in_slice")?;
    let raw_type = reader.read_ue("slice_type")?;
    let slice_type = SliceType::new(raw_type)?;
    let pps_id = narrow_u8(
        reader.read_ue("pic_parameter_set_id")?,
        "pic_parameter_set_id",
    )?;
    Ok((first_mb, slice_type, raw_type >= 5, pps_id))
}

/// Slice header against its parameter sets.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range syntax, a non-intra slice in
/// an IDR picture, or a header the gate declines.
pub fn parse_slice_rest<E: H264Error, G: H264Gate<E>>(
    reader: &mut BitReader<E>,
    prefix: (u32, SliceType, bool, u8),
    idr: bool,
    ref_idc: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceHeader, E> {
    let (first_mb, slice_type, fixed, pps_id) = prefix;
    if idr && !slice_type.is_intra() {
        return Err(E::parse(ParseReason::NonIntraSliceInIdr));
    }
    let width = u32::from(sps.log2_max_frame_num_minus4) + 4;
    let frame_num = reader.read_bits(width, "frame_num")?;
    // field_pic_flag absent: the SPS gate admits frame-only streams.
    let idr_pic_id = parse_idr_pic_id(reader, idr)?;
    let poc = parse_slice_poc(reader, sps, pps)?;
    let redundant_pic_cnt = if pps.redundant_present() {
        reader.read_ue("redundant_pic_cnt")?
    } else {
        0
    };
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
        weights: SliceWeights::default(),
        marking: Marking::Silent,
        redundant_pic_cnt,
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
        u16::try_from(offset).map_err(|_| E::parse(ParseReason::SliceHeaderTooLong))?;
    G::check_slice_extra(&header, sps)?;
    Ok(header)
}

/// IDR id for IDR slices.
fn parse_idr_pic_id<E: H264Error>(reader: &mut BitReader<E>, idr: bool) -> Result<Option<u16>, E> {
    if !idr {
        return Ok(None);
    }
    let id = narrow_u16(reader.read_ue("idr_pic_id")?, "idr_pic_id")?;
    Ok(Some(id))
}

/// Slice POC syntax for the sequence POC method.
fn parse_slice_poc<E: H264Error>(
    reader: &mut BitReader<E>,
    sps: &Sps,
    pps: &Pps,
) -> Result<SlicePoc, E> {
    match sps.poc_type {
        PocType::Lsb => {
            let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
            let lsb = reader.read_bits(width, "pic_order_cnt_lsb")?;
            let bottom = if pps.pic_order_present() {
                reader.read_se("delta_pic_order_cnt_bottom")?
            } else {
                0
            };
            Ok(SlicePoc::Lsb(lsb, bottom))
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
fn parse_slice_inter<E: H264Error>(
    reader: &mut BitReader<E>,
    header: &mut SliceHeader,
    ref_idc: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<(), E> {
    if header.slice_type.is_intra() {
        return parse_marking(reader, header, ref_idc, header.idr_pic_id.is_some());
    }
    if header.slice_type.is_b() {
        header.direct_spatial = reader.read_bit("direct_spatial_mv_pred_flag")?;
    }
    parse_ref_override(reader, header)?;
    header.ref_mod_l0 = parse_ref_modification(reader)?;
    if header.slice_type.is_b() {
        header.ref_mod_l1 = parse_ref_modification(reader)?;
    }
    parse_weights(reader, header, sps, pps)?;
    parse_marking(reader, header, ref_idc, false)
}

/// Reference count override for inter slices.
fn parse_ref_override<E: H264Error>(
    reader: &mut BitReader<E>,
    header: &mut SliceHeader,
) -> Result<(), E> {
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

/// Reference count field below the 32 ceiling.
fn parse_ref_count<E: H264Error>(reader: &mut BitReader<E>, syntax: &'static str) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if value > 31 {
        return Err(E::parse(ParseReason::Range { syntax }));
    }
    narrow_u8(value, syntax)
}

/// Reference list modification table.
fn parse_ref_modification<E: H264Error>(reader: &mut BitReader<E>) -> Result<Vec<RefMod>, E> {
    let mut operations = Vec::new();
    if !reader.read_bit("ref_pic_list_modification_flag")? {
        return Ok(operations);
    }
    loop {
        match reader.read_ue("modification_of_pic_nums_idc")? {
            0 => operations.push(RefMod::ShortBelow(
                reader.read_ue("abs_diff_pic_num_minus1")?,
            )),
            1 => operations.push(RefMod::ShortAbove(
                reader.read_ue("abs_diff_pic_num_minus1")?,
            )),
            2 => operations.push(RefMod::Long(reader.read_ue("long_term_pic_num")?)),
            3 => return Ok(operations),
            _ => {
                return Err(E::parse(ParseReason::Range {
                    syntax: "modification_of_pic_nums_idc",
                }));
            }
        }
    }
}

/// Prediction weight table.
fn parse_weights<E: H264Error>(
    reader: &mut BitReader<E>,
    header: &mut SliceHeader,
    sps: &Sps,
    pps: &Pps,
) -> Result<(), E> {
    let present = (pps.weighted_pred() && !header.slice_type.is_b())
        || (pps.weighted_bipred() == 1 && header.slice_type.is_b());
    if header.slice_type.is_intra() || !present {
        return Ok(());
    }
    // Monochrome slices carry no chroma weights, H.264 7.3.3.
    let chroma = sps.fidelity.chroma_format_idc != 0;
    let luma_denom = reader.read_ue("luma_log2_weight_denom")?;
    if luma_denom > 7 {
        return Err(E::parse(ParseReason::Range {
            syntax: "luma_log2_weight_denom",
        }));
    }
    let mut chroma_denom = 0;
    if chroma {
        chroma_denom = reader.read_ue("chroma_log2_weight_denom")?;
        if chroma_denom > 7 {
            return Err(E::parse(ParseReason::Range {
                syntax: "chroma_log2_weight_denom",
            }));
        }
    }
    let l0 = parse_weight_list(reader, header.num_ref_l0(pps), chroma)?;
    let mut l1 = Vec::new();
    if header.slice_type.is_b() {
        l1 = parse_weight_list(reader, header.num_ref_l1(pps), chroma)?;
    }
    header.weights = SliceWeights {
        luma_denom: narrow_u8(luma_denom, "luma_log2_weight_denom")?,
        chroma_denom: narrow_u8(chroma_denom, "chroma_log2_weight_denom")?,
        l0,
        l1,
    };
    Ok(())
}

/// One reference weight list.
fn parse_weight_list<E: H264Error>(
    reader: &mut BitReader<E>,
    count_minus1: u8,
    chroma_samples: bool,
) -> Result<Vec<WeightEntry>, E> {
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
        let chroma = if chroma_samples && reader.read_bit("chroma_weight_flag")? {
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
fn parse_marking<E: H264Error>(
    reader: &mut BitReader<E>,
    header: &mut SliceHeader,
    ref_idc: u8,
    idr: bool,
) -> Result<(), E> {
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
            6 => operations.push(Mmco::CurrentToLong(reader.read_ue("long_term_frame_idx")?)),
            _ => {
                return Err(E::parse(ParseReason::Range {
                    syntax: "memory_management_control_operation",
                }))
            }
        }
    }
    header.marking = Marking::Adaptive(operations);
    Ok(())
}

/// Slice tail: CABAC init, QP delta, deblocking offsets.
fn parse_slice_tail<E: H264Error>(
    reader: &mut BitReader<E>,
    header: &mut SliceHeader,
    pps: &Pps,
) -> Result<(), E> {
    if pps.entropy() && !header.slice_type.is_intra() {
        let idc = narrow_u8(reader.read_ue("cabac_init_idc")?, "cabac_init_idc")?;
        header.cabac_init_idc = idc;
    }
    header.qp_delta = reader.read_se("slice_qp_delta")?;
    if !pps.deblocking_present() {
        return Ok(());
    }
    let idc = reader.read_ue("disable_deblocking_filter_idc")?;
    if idc > 2 {
        return Err(E::parse(ParseReason::Range {
            syntax: "disable_deblocking_filter_idc",
        }));
    }
    header.deblocking_idc = narrow_u8(idc, "disable_deblocking_filter_idc")?;
    if idc != 1 {
        header.deblocking_alpha = reader.read_se("slice_alpha_c0_offset_div2")?;
        header.deblocking_beta = reader.read_se("slice_beta_offset_div2")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::bits::BitReader;
    use crate::error::test_support::TestError;
    use crate::error::{H264Gate, ParseReason, PermissiveGate, StrictGate, UnsupportedReason};
    use crate::pps::parse_pps;

    use crate::tests::{
        idr_slice_body, p_slice_body, p_slice_body_mmco, p_slice_body_redundant, pps,
        pps_body_redundant, sps,
    };

    use super::{parse_slice_prefix, parse_slice_rest, Marking, Mmco, RefMod, SlicePoc, SliceType};

    use rust_video_harness::Bits;

    /// Slice header over `body` with the IDR flag and reference idc.
    fn header(body: &[u8], idr: bool, ref_idc: u8) -> super::SliceHeader {
        let mut reader = BitReader::<TestError>::new(body);
        let prefix = parse_slice_prefix(&mut reader).expect("prefix");
        parse_slice_rest::<TestError, StrictGate>(&mut reader, prefix, idr, ref_idc, &sps(), &pps())
            .expect("header")
    }

    #[test]
    fn slice_header_reports_idr_fields() {
        let body = idr_slice_body();
        let mut reader = BitReader::<TestError>::new(&body);
        let prefix = parse_slice_prefix(&mut reader).expect("prefix");
        assert_eq!(prefix.0, 0);
        assert_eq!(prefix.1, SliceType::I);
        let header =
            parse_slice_rest::<TestError, StrictGate>(&mut reader, prefix, true, 2, &sps(), &pps())
                .expect("header");
        assert_eq!(header.frame_num, 0);
        assert_eq!(header.idr_pic_id, Some(0));
        assert_eq!(header.poc, SlicePoc::Lsb(0, 0));
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
        let header = header(&idr_slice_body(), true, 2);
        assert_eq!(header.bit_offset % 8, 0);
    }

    #[test]
    fn slice_header_refuses_sp_type() {
        let mut writer = Bits::new();
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        let body = writer.finish();
        let mut reader = BitReader::<TestError>::new(&body);
        assert!(parse_slice_prefix::<TestError>(&mut reader).is_err());
    }

    #[test]
    fn gates_split_on_redundant_pictures() {
        let sps = sps();
        let pps =
            parse_pps::<TestError, StrictGate>(&pps_body_redundant(), Some(&sps)).expect("pps");
        let body = p_slice_body_redundant(2);
        let mut reader = BitReader::<TestError>::new(&body);
        let prefix = parse_slice_prefix(&mut reader).expect("prefix");
        assert_eq!(
            parse_slice_rest::<TestError, StrictGate>(&mut reader, prefix, false, 2, &sps, &pps)
                .unwrap_err(),
            TestError::Unsupported(UnsupportedReason::RedundantPictures)
        );
        let mut reader = BitReader::<TestError>::new(&body);
        let prefix = parse_slice_prefix(&mut reader).expect("prefix");
        let header = parse_slice_rest::<TestError, PermissiveGate>(
            &mut reader,
            prefix,
            false,
            2,
            &sps,
            &pps,
        )
        .expect("lenient header");
        assert_eq!(header.redundant_pic_cnt, 2);
    }

    #[test]
    fn gates_split_on_wild_refmod() {
        let sps = sps();
        let mut header = header(&p_slice_body(1, 2), false, 2);
        header.ref_mod_l0.push(RefMod::ShortBelow(u32::MAX));
        let strict: Result<(), TestError> = StrictGate::check_slice_extra(&header, &sps);
        assert_eq!(
            strict.unwrap_err(),
            TestError::Parse(ParseReason::Range {
                syntax: "abs_diff_pic_num",
            })
        );
        let lenient: Result<(), TestError> = PermissiveGate::check_slice_extra(&header, &sps);
        assert!(lenient.is_ok());
        header.ref_mod_l0.clear();
        header.ref_mod_l1.push(RefMod::Long(99));
        let strict: Result<(), TestError> = StrictGate::check_slice_extra(&header, &sps);
        assert_eq!(
            strict.unwrap_err(),
            TestError::Parse(ParseReason::Range {
                syntax: "long_term_pic_num",
            })
        );
    }

    #[test]
    fn mmco6_reads_index() {
        let header = header(&p_slice_body_mmco(2), false, 2);
        assert_eq!(
            header.marking,
            Marking::Adaptive(vec![Mmco::CurrentToLong(2)])
        );
    }

    #[test]
    fn marking_queries_reset_and_assignment() {
        assert!(Marking::Adaptive(vec![Mmco::MarkAllUnused]).has_mmco5());
        assert!(!Marking::Adaptive(vec![Mmco::CurrentToLong(1)]).has_mmco5());
        assert!(!Marking::Silent.has_mmco5());
        assert_eq!(
            Marking::Idr {
                no_output: false,
                long_term: true,
            }
            .current_to_long(),
            Some(0)
        );
        assert_eq!(
            Marking::Adaptive(vec![Mmco::CurrentToLong(2)]).current_to_long(),
            Some(2)
        );
        assert_eq!(Marking::Silent.current_to_long(), None);
    }
}
