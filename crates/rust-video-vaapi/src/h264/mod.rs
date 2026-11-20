//! H.264 Annex B ingest over the shared grammar.

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

pub(crate) use rust_video_h264::{
    abs_frame_num, frame_num_wrap, initial_ref_lists, is_idr, mmco_short_target, nal_ref_idc,
    nal_unit_type, parse_pps, parse_pps_ids, parse_slice_prefix, parse_slice_rest, parse_sps,
    poc_msb, poc_type2, rbsp, split_nals, BitReader, Marking, Mmco, PocType, Pps, RefCandidate,
    RefMod, SliceHeader, SlicePoc, SliceType, Sps,
};

use rust_video_h264::{H264Error, H264Gate, ParseKind, UnsupportedKind};

use rust_video_caps::reason::{ParseReason as CapsParse, UnsupportedReason as CapsUnsupported};

use crate::error::VaApiError;

/// Tier capability gates over shared grammar output.
#[derive(Default)]
pub(crate) struct Gate;

impl H264Error for VaApiError {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Parse(CapsParse::Truncated { syntax }),
            ParseKind::SpsWithoutBody => Self::Parse(CapsParse::SpsWithoutBody),
            ParseKind::PpsWithoutBody => Self::Parse(CapsParse::PpsWithoutBody),
            ParseKind::SliceWithoutHeader => Self::Parse(CapsParse::SliceWithoutHeader),
            ParseKind::OverflowU32 { syntax } => Self::Parse(CapsParse::OverflowU32 { syntax }),
            ParseKind::OverflowI32 { syntax } => Self::Parse(CapsParse::OverflowI32 { syntax }),
            ParseKind::TooWide { syntax } => Self::Parse(CapsParse::TooWide { syntax }),
            ParseKind::Range { syntax } => Self::Parse(CapsParse::Range { syntax }),
            ParseKind::NalForbiddenBit => Self::Parse(CapsParse::NalForbiddenBit),
            ParseKind::SliceHeaderTooLong => Self::Parse(CapsParse::SliceHeaderTooLong),
            ParseKind::PpsCitesUnknownSps => Self::Parse(CapsParse::PpsCitesUnknownSps),
            ParseKind::SliceCitesUnknownPps => Self::Parse(CapsParse::SliceCitesUnknownPps),
            ParseKind::SliceCitesUnknownSps => Self::Parse(CapsParse::SliceCitesUnknownSps),
            ParseKind::PictureSlicesDisagree => Self::Parse(CapsParse::PictureSlicesDisagree),
            ParseKind::NonIntraSliceInIdr => Self::Parse(CapsParse::NonIntraSliceInIdr),
            ParseKind::FrameCropOutsideCodedSize => {
                Self::Parse(CapsParse::FrameCropOutsideCodedSize)
            }
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::Interlaced => Self::Unsupported(CapsUnsupported::Interlaced),
            UnsupportedKind::ChromaOutside420 => {
                Self::Unsupported(CapsUnsupported::ChromaOutside420)
            }
            UnsupportedKind::LumaDepthAbove8Bit => {
                Self::Unsupported(CapsUnsupported::LumaDepthAbove8Bit)
            }
            UnsupportedKind::ChromaDepthAbove8Bit => {
                Self::Unsupported(CapsUnsupported::ChromaDepthAbove8Bit)
            }
            UnsupportedKind::TransformBypass => Self::Unsupported(CapsUnsupported::TransformBypass),
            UnsupportedKind::FlexibleMacroblockOrder => {
                Self::Unsupported(CapsUnsupported::FlexibleMacroblockOrder)
            }
            UnsupportedKind::DataPartitions => Self::Unsupported(CapsUnsupported::DataPartitions),
            UnsupportedKind::Multiview => Self::Unsupported(CapsUnsupported::Multiview),
            UnsupportedKind::SpSiSlices => Self::Unsupported(CapsUnsupported::SpSiSlices),
            UnsupportedKind::RedundantPictures => {
                Self::Unsupported(CapsUnsupported::RedundantPictures)
            }
            UnsupportedKind::ArbitrarySliceOrder => {
                Self::Unsupported(CapsUnsupported::ArbitrarySliceOrder)
            }
        }
    }
}

impl H264Gate<VaApiError> for Gate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), VaApiError> {
        if !matches!(
            sps.level_idc,
            9 | 10
                | 11
                | 12
                | 13
                | 20
                | 21
                | 22
                | 30
                | 31
                | 32
                | 40
                | 41
                | 42
                | 50
                | 51
                | 52
                | 60
                | 61
                | 62
        ) {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "level_idc",
            }));
        }
        if sps.scaling_map.is_some() {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "scaling_map",
            }));
        }
        if sps.fidelity.separate_plane {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "separate_plane",
            }));
        }
        if sps.fidelity.bit_depth_luma_minus8 > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "bit_depth_luma_minus8",
            }));
        }
        if sps.fidelity.bit_depth_chroma_minus8 > 6 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "bit_depth_chroma_minus8",
            }));
        }
        if sps.cycle > 254 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "cycle" }));
        }
        if sps.max_num_ref_frames > 16 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "max_num_ref_frames",
            }));
        }
        Self::check_crop(sps)?;
        Self::check_vui(sps)?;
        Ok(())
    }

    /// Picture set against tier caps with its sequence set.
    fn check_pps(pps: &Pps, sps: &Sps) -> Result<(), VaApiError> {
        if pps.scaling_map.is_some() {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "scaling_map",
            }));
        }
        if pps.seq_parameter_set_id > 31 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "seq_parameter_set_id",
            }));
        }
        let qp_min = -i32::from(26 + 6 * sps.fidelity.bit_depth_luma_minus8);
        if i32::from(pps.pic_init_qp) < qp_min || pps.pic_init_qp > 25 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "pic_init_qp",
            }));
        }
        if pps.pic_init_qs < -26 || pps.pic_init_qs > 25 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "pic_init_qs",
            }));
        }
        if pps.chroma_qp < -12 || pps.chroma_qp > 12 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "chroma_qp",
            }));
        }
        Ok(())
    }

    /// Slice header tail counts against tier caps.
    fn check_slice_extra(header: &SliceHeader, sps: &Sps) -> Result<(), VaApiError> {
        let _ = sps;
        if header.redundant_pic_cnt > 127 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "redundant_pic_cnt",
            }));
        }
        if header.cabac_init_idc > 2 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "cabac_init_idc",
            }));
        }
        if !(-87..=77).contains(&header.qp_delta) {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "qp_delta" }));
        }
        if !(-6..=6).contains(&header.deblocking_alpha) {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "deblocking_alpha",
            }));
        }
        if !(-6..=6).contains(&header.deblocking_beta) {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "deblocking_beta",
            }));
        }
        Self::check_weights(header)?;
        Ok(())
    }
}

impl Gate {
    /// Crop window inside the coded size, 7.4.2.1.1.
    fn check_crop(sps: &Sps) -> Result<(), VaApiError> {
        let Some(offsets) = sps.crop_offsets else {
            return Ok(());
        };
        let (unit_x, unit_y) = match sps.fidelity.chroma_format_idc {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let width = (u32::from(sps.width_in_mbs_minus1) + 1) * 16;
        let height = (u32::from(sps.height_in_mbs_minus1) + 1) * 16;
        offsets[0]
            .checked_add(offsets[1])
            .and_then(|sum| sum.checked_mul(unit_x))
            .and_then(|used| width.checked_sub(used))
            .ok_or(VaApiError::Parse(CapsParse::FrameCropOutsideCodedSize))?;
        offsets[2]
            .checked_add(offsets[3])
            .and_then(|sum| sum.checked_mul(unit_y))
            .and_then(|used| height.checked_sub(used))
            .ok_or(VaApiError::Parse(CapsParse::FrameCropOutsideCodedSize))?;
        Ok(())
    }

    /// VUI bounds the shared grammar leaves open, E.1.1.
    fn check_vui(sps: &Sps) -> Result<(), VaApiError> {
        let Some(vui) = &sps.vui else {
            return Ok(());
        };
        if let Some(loc) = &vui.chroma_loc {
            if loc.top > 5 || loc.bottom > 5 {
                return Err(VaApiError::Parse(CapsParse::Range {
                    syntax: "chroma_loc",
                }));
            }
        }
        if let Some(timing) = &vui.timing {
            if timing.num_units_in_tick == 0 {
                return Err(VaApiError::Parse(CapsParse::Range {
                    syntax: "num_units_in_tick",
                }));
            }
            if timing.time_scale == 0 {
                return Err(VaApiError::Parse(CapsParse::Range {
                    syntax: "time_scale",
                }));
            }
        }
        if vui.max_bits_per_mcu_denom > 16 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "max_bits_per_mcu_denom",
            }));
        }
        if vui.max_mv_horizontal > 16 || vui.max_mv_vertical > 16 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "max_mv" }));
        }
        Ok(())
    }

    /// Weight table entries inside the `i8` inference range.
    fn check_weights(header: &SliceHeader) -> Result<(), VaApiError> {
        for entry in header.weights.l0.iter().chain(&header.weights.l1) {
            if let Some((weight, offset)) = entry.luma {
                Self::check_weight(weight)?;
                Self::check_weight(offset)?;
            }
            if let Some(pairs) = &entry.chroma {
                for (weight, offset) in pairs {
                    Self::check_weight(*weight)?;
                    Self::check_weight(*offset)?;
                }
            }
        }
        Ok(())
    }

    /// Single weight or offset inside -128..=127.
    fn check_weight(value: i32) -> Result<(), VaApiError> {
        if !(-128..=127).contains(&value) {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "weight" }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rust_video_h264::{H264Error, H264Gate, PocType, ScalingMap};

    use super::{
        parse_pps, parse_slice_prefix, parse_slice_rest, parse_sps, rbsp, BitReader, Gate,
        ParseKind, SliceHeader, UnsupportedKind,
    };
    use crate::error::VaApiError;
    use crate::fixtures::{idr_slice_body, pps, pps_body, sps, sps_body};

    #[test]
    fn shared_grammar_agrees_on_baseline_sets() {
        let seq_raw = sps_body(0);
        let pic_raw = pps_body();
        let seq_shared = parse_sps::<VaApiError, Gate>(&rbsp(&seq_raw)).expect("sps");
        assert_eq!(seq_shared.profile_idc, 66);
        assert_eq!((seq_shared.width(), seq_shared.height()), (320, 240));
        assert!(matches!(seq_shared.poc_type, PocType::Lsb));

        let pic_shared =
            parse_pps::<VaApiError, Gate>(&rbsp(&pic_raw), Some(&seq_shared)).expect("pps");

        let raw_slice = idr_slice_body();
        let rbsp_slice = rbsp(&raw_slice);
        let mut reader = BitReader::new(&rbsp_slice);
        let prefix = parse_slice_prefix::<VaApiError>(&mut reader).expect("prefix");
        let shared = parse_slice_rest::<VaApiError, Gate>(
            &mut reader,
            prefix,
            true,
            3,
            &seq_shared,
            &pic_shared,
        )
        .expect("slice");
        assert_eq!(shared.first_mb, 0);
        assert_eq!(shared.frame_num, 0);
        assert_eq!(shared.idr_pic_id, Some(0));
        assert!(matches!(shared.poc, rust_video_h264::SlicePoc::Lsb(0, 0)));
    }

    #[test]
    fn gate_declines_scaling_planes_and_bounds() {
        let mut set = sps(PocType::Lsb);
        set.scaling_map = Some(ScalingMap {
            present_mask: 1,
            default_mask: 0,
        });
        assert!(matches!(Gate::check_sps(&set), Err(VaApiError::Parse(_))));
        let mut set = sps(PocType::Lsb);
        set.fidelity.separate_plane = true;
        assert_eq!(
            Gate::check_sps(&set).unwrap_err().to_string(),
            "parse failed: separate_plane range"
        );
        let mut set = sps(PocType::Lsb);
        set.max_num_ref_frames = 17;
        assert!(matches!(Gate::check_sps(&set), Err(VaApiError::Parse(_))));
        let mut set = sps(PocType::Lsb);
        set.crop_offsets = Some([500, 500, 0, 0]);
        assert!(matches!(Gate::check_sps(&set), Err(VaApiError::Parse(_))));
    }

    #[test]
    fn gate_bounds_picture_qp_and_slice_tail() {
        let set = sps(PocType::Lsb);
        let mut pic = pps();
        pic.pic_init_qp = 26;
        assert!(matches!(
            Gate::check_pps(&pic, &set),
            Err(VaApiError::Parse(_))
        ));
        let mut pic = pps();
        pic.chroma_qp = 13;
        assert!(matches!(
            Gate::check_pps(&pic, &set),
            Err(VaApiError::Parse(_))
        ));
        let raw = idr_slice_body();
        let rbsp = rbsp(&raw);
        let mut reader = BitReader::new(&rbsp);
        let prefix = parse_slice_prefix::<VaApiError>(&mut reader).expect("prefix");
        let header =
            parse_slice_rest::<VaApiError, Gate>(&mut reader, prefix, true, 3, &set, &pps())
                .expect("slice");
        assert!(Gate::check_slice_extra(&header, &set).is_ok());
        let wild = SliceHeader {
            qp_delta: 78,
            ..header.clone()
        };
        assert!(matches!(
            Gate::check_slice_extra(&wild, &set),
            Err(VaApiError::Parse(_))
        ));
        let wild = SliceHeader {
            qp_delta: 0,
            deblocking_alpha: 7,
            ..header
        };
        assert!(matches!(
            Gate::check_slice_extra(&wild, &set),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn gate_declines_unknown_level() {
        let payload = sps_body(0);
        let mut set = parse_sps::<VaApiError, Gate>(&rbsp(&payload)).expect("sps");
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
            VaApiError::parse(ParseKind::NalForbiddenBit).to_string(),
            "parse failed: nal forbidden bit"
        );
        assert_eq!(
            VaApiError::unsupported(UnsupportedKind::Interlaced).to_string(),
            "unsupported stream: interlaced"
        );
        assert_eq!(
            VaApiError::unsupported(UnsupportedKind::FlexibleMacroblockOrder).to_string(),
            "unsupported stream: flexible macroblock order"
        );
        assert_eq!(
            VaApiError::unsupported(UnsupportedKind::DataPartitions).to_string(),
            "unsupported stream: data partitions"
        );
    }

    #[test]
    fn empty_and_garbage_payloads_fail_typed() {
        assert!(matches!(
            parse_sps::<VaApiError, Gate>(&[]),
            Err(VaApiError::Parse(_))
        ));
        assert!(parse_sps::<VaApiError, Gate>(&[0xFF; 6]).is_err());
        assert!(parse_pps::<VaApiError, Gate>(&[], None).is_err());
    }
}
