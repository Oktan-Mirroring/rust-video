//! H.265 Annex-B ingest over the shared grammar.
//!
//! NAL dispatch and set tables over shared parameter sets; picture
//! assembly lives in the reference context.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::ParseError;

use rust_video_h265::{
    nal_unit_type, parse_pps, parse_slice_prefix, parse_slice_rest, parse_sps, parse_vps, rbsp,
    temporal_id, BitReader, HevcError, HevcGate, ParseKind, UnsupportedKind,
};

pub(crate) use rust_video_h265::{
    anchors_tid0, is_bla, is_cra, is_idr, is_irap, is_rasl, is_slnr, HrdLayerData, HrdParams, Nalu,
    NaluSplitter, Pps, ProfileTierLevel, ScalingLists, ShortTermRps, SliceHeader, SliceType, Sps,
    Vps, VuiParams, PPS_SCALING, SPS_NESTING, SPS_ORDERING_INFO,
};

/// Tier capability gates over shared grammar output.
struct Gate;

impl HevcError for ParseError {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Truncated { syntax },
            ParseKind::OverflowU32 { syntax } | ParseKind::OverflowI32 { syntax } => {
                Self::Overflow { syntax }
            }
            ParseKind::TooWide { syntax } => Self::Overflow { syntax },
            ParseKind::Range { syntax } => Self::OutOfRange {
                syntax: remap_syntax(syntax),
            },
            ParseKind::NalForbiddenBit | ParseKind::NalTemporalId => Self::OutOfRange {
                syntax: "nal header",
            },
            ParseKind::VpsWithoutBody
            | ParseKind::SpsWithoutBody
            | ParseKind::PpsWithoutBody
            | ParseKind::SliceWithoutHeader => Self::Truncated { syntax: "nal body" },
            ParseKind::PpsCitesUnknownSps
            | ParseKind::SliceCitesUnknownPps
            | ParseKind::SliceCitesUnknownSps => Self::MissingSet { id: 0 },
            ParseKind::PictureSlicesDisagree => Self::OutOfRange {
                syntax: "picture slices",
            },
            ParseKind::NonIntraSliceInIdr => Self::OutOfRange {
                syntax: "slice type",
            },
            ParseKind::ShortTermRefPicSetSize => Self::Unsupported {
                what: "oversize rps",
            },
            ParseKind::EmptyCodedSize => Self::OutOfRange {
                syntax: "sps width",
            },
            ParseKind::ConformanceOutside => Self::OutOfRange {
                syntax: "sps conformance",
            },
            ParseKind::RpsTableOversize => Self::Unsupported {
                what: "oversize rps list",
            },
            ParseKind::LtSpsOversize | ParseKind::LtTotalOversize => Self::Unsupported {
                what: "oversize lt list",
            },
            ParseKind::EmptySpsRps => Self::Unsupported {
                what: "empty sps rps",
            },
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::DependentSlices => Self::Unsupported {
                what: "dependent slices",
            },
            UnsupportedKind::ArbitrarySliceOrder => Self::Unsupported {
                what: "arbitrary slice order",
            },
            UnsupportedKind::Multilayer => Self::Unsupported { what: "multilayer" },
        }
    }
}

/// Shared syntax label behind the tier label.
fn remap_syntax(syntax: &'static str) -> &'static str {
    match syntax {
        "slice_type" => "slice type",
        "short_term_ref_pic_set_idx" => "slice rps idx",
        "slice_segment_address" => "slice address",
        "pic_parameter_set_id" => "pps id",
        "seq_parameter_set_id" => "sps id",
        "pps_seq_parameter_set_id" => "pps sps id",
        "slice_pic_parameter_set_id" => "slice pps id",
        "chroma_format_idc" => "sps chroma",
        "bit_depth_luma_minus8" => "sps luma depth",
        "bit_depth_chroma_minus8" => "sps chroma depth",
        "log2_max_pic_order_cnt_lsb_minus4" => "sps poc bits",
        "offset_len_minus1" => "slice offset len",
        _ => syntax,
    }
}

impl HevcGate<ParseError> for Gate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &rust_video_h265::Sps) -> Result<(), ParseError> {
        if sps.chroma_format > 3 {
            return Err(ParseError::OutOfRange {
                syntax: "sps chroma",
            });
        }
        if sps.max_sub_layers > 7 {
            return Err(ParseError::OutOfRange {
                syntax: "sps max sub",
            });
        }
        check_ptl(&sps.ptl)?;
        if sps.sps_extension_present {
            return Err(ParseError::Unsupported {
                what: "sps extension",
            });
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(
        pps: &rust_video_h265::Pps,
        sps: Option<&rust_video_h265::Sps>,
    ) -> Result<(), ParseError> {
        let _ = sps;
        if pps.num_tile_columns - 1 > 19 || pps.num_tile_rows - 1 > 21 {
            return Err(ParseError::Unsupported {
                what: "oversize tiles",
            });
        }
        if pps.pps_extension_present {
            return Err(ParseError::Unsupported {
                what: "pps extension",
            });
        }
        Ok(())
    }

    /// Video set against tier caps.
    fn check_vps(vps: &rust_video_h265::Vps) -> Result<(), ParseError> {
        if vps.max_layers_minus1 > 0 {
            return Err(ParseError::Unsupported {
                what: "h265 layers",
            });
        }
        if vps.max_sub_layers_minus1 > 6 {
            return Err(ParseError::OutOfRange {
                syntax: "vps max sub",
            });
        }
        check_ptl(&vps.ptl)?;
        if vps.num_layer_sets > 0 {
            return Err(ParseError::Unsupported {
                what: "h265 layer sets",
            });
        }
        if vps
            .timing
            .as_ref()
            .is_some_and(|timing| timing.hrd.len() > 16)
        {
            return Err(ParseError::Unsupported { what: "vps hrd" });
        }
        if vps.extension {
            return Err(ParseError::Unsupported {
                what: "vps extension",
            });
        }
        Ok(())
    }

    /// Slice header tail counts against tier caps.
    fn check_slice_extra(header: &rust_video_h265::SliceHeader) -> Result<(), ParseError> {
        if header.lt.len() > 16 {
            return Err(ParseError::Unsupported {
                what: "oversize lt list",
            });
        }
        if header.entry_count > 256 {
            return Err(ParseError::Unsupported {
                what: "oversize entry points",
            });
        }
        if header.extension_length > 256 {
            return Err(ParseError::Unsupported {
                what: "oversize slice extension",
            });
        }
        Ok(())
    }
}

/// Profile, tier, and level against tier caps.
fn check_ptl(ptl: &rust_video_h265::ProfileTierLevel) -> Result<(), ParseError> {
    check_profile(ptl.profile_idc, ptl.compat)?;
    if ptl.source.interlaced {
        return Err(ParseError::Unsupported {
            what: "interlaced source",
        });
    }
    for layer in &ptl.sub_layers {
        if !layer.present {
            continue;
        }
        check_profile(layer.profile_idc, layer.compat)?;
    }
    Ok(())
}

/// Profile indicator with compatibility against tier caps.
fn check_profile(profile_idc: u8, compat: u32) -> Result<(), ParseError> {
    if !matches!(profile_idc, 1..=3) && compat & 0x0E00_0000 == 0 {
        return Err(ParseError::Unsupported {
            what: "h265 profile",
        });
    }
    Ok(())
}

/// Coded slice with its sets and payload for the hardware.
#[derive(Clone, Debug)]
pub(crate) struct Slice {
    /// NAL unit type id.
    pub(crate) unit_type: u8,
    /// Temporal id.
    pub(crate) temporal_id: u8,
    /// Parsed slice header.
    pub(crate) header: SliceHeader,
    /// Active video set.
    pub(crate) vps: Rc<Vps>,
    /// Active sequence set.
    pub(crate) sps: Rc<Sps>,
    /// Active picture set.
    pub(crate) pps: Rc<Pps>,
    /// NAL header plus payload, start code cut.
    pub(crate) payload: Vec<u8>,
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
    pub fn parse(&mut self, nalu: &Nalu) -> Result<ParsedNalu, ParseError> {
        let payload = nalu.bytes();
        if payload.len() < 2 {
            return Err(ParseError::Truncated {
                syntax: "nal header",
            });
        }
        let unit_type = nal_unit_type(payload[0]);
        let temporal_id = temporal_id(payload[1]);
        if unit_type == 32 {
            let rbsp = rbsp(&payload[2..]);
            let shared = parse_vps::<ParseError>(&rbsp)?;
            Gate::check_vps(&shared)?;
            let vps = Rc::new(shared);
            self.vps.insert(vps.id, Rc::clone(&vps));
            return Ok(ParsedNalu::Vps(vps));
        }
        if unit_type == 33 {
            let rbsp = rbsp(&payload[2..]);
            let shared = Rc::new(parse_sps::<ParseError, Gate>(&rbsp)?);
            let id = shared.id;
            self.sps.insert(id, Rc::clone(&shared));
            return Ok(ParsedNalu::Sps(shared));
        }
        if unit_type == 34 {
            let rbsp = rbsp(&payload[2..]);
            let shared = Rc::new(parse_pps::<ParseError, Gate>(&rbsp, None)?);
            let id = shared.id;
            self.pps.insert(id, Rc::clone(&shared));
            return Ok(ParsedNalu::Pps(shared));
        }
        // Reserved 22..=31 never carry slices; the tier ignores them.
        if unit_type <= 21 {
            let rbsp = rbsp(&payload[2..]);
            let slice = self.parse_slice(&rbsp, unit_type, temporal_id, payload)?;
            return Ok(ParsedNalu::Slice(Box::new(slice)));
        }
        Ok(ParsedNalu::Other)
    }

    /// Parse a slice header against the stored tables.
    fn parse_slice(
        &self,
        body: &[u8],
        unit_type: u8,
        temporal_id: u8,
        payload: &[u8],
    ) -> Result<Slice, ParseError> {
        let mut reader = BitReader::<ParseError>::new(body);
        let prefix = parse_slice_prefix(&mut reader, unit_type)?;
        let pps_id = prefix.2;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps_id),
            })?;
        if pps.sps_id > 15 {
            return Err(ParseError::OutOfRange {
                syntax: "slice sps id",
            });
        }
        let sps = self
            .sps
            .get(&pps.sps_id)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps.sps_id),
            })?;
        let vps = self
            .vps
            .get(&sps.vps_id)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(sps.vps_id),
            })?;
        let header = parse_slice_rest(&mut reader, prefix, unit_type, &sps, &pps)?;
        Gate::check_slice_extra(&header)?;
        Ok(Slice {
            unit_type,
            temporal_id,
            header,
            vps,
            sps,
            pps,
            payload: payload.to_vec(),
        })
    }
}

impl Default for H265Parser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use rust_video_h265::{parse_short_term_rps, BitReader as SharedBitReader, ShortTermRps};

    use super::{H265Parser, ParsedNalu, SliceType};
    use crate::fixtures::{hex, split265};
    use crate::h265::params::fill_rps;

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
        let units = split265(&[hex(DEMO_VPS), hex(DEMO_SPS)]);
        let mut parser = H265Parser::new();
        parser.parse(&units[0]).expect("vps");
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        assert_eq!(sps.ptl.profile_idc, 1);
        assert_eq!((sps.width, sps.height), (320, 240));
        assert_eq!((sps.visible_width(), sps.visible_height()), (320, 240));
        assert_eq!(sps.chroma_format, 1);
    }

    #[test]
    fn demo_pps_resolves_its_sps() {
        let units = split265(&[hex(DEMO_VPS), hex(DEMO_SPS), hex(DEMO_PPS)]);
        let mut parser = H265Parser::default();
        parser.parse(&units[0]).expect("vps");
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        let ParsedNalu::Pps(pps) = parser.parse(&units[2]).expect("pps") else {
            panic!("want pps");
        };
        assert_eq!(pps.sps_id, sps.id);
    }

    #[test]
    fn demo_slice_parses_against_tables() {
        let units = split265(&[hex(DEMO_VPS), hex(DEMO_SPS), hex(DEMO_PPS), hex(DEMO_SLICE)]);
        let mut parser = H265Parser::new();
        parser.parse(&units[0]).expect("vps");
        parser.parse(&units[1]).expect("sps");
        parser.parse(&units[2]).expect("pps");
        let ParsedNalu::Slice(slice) = parser.parse(&units[3]).expect("slice") else {
            panic!("want slice");
        };
        assert!(rust_video_h265::is_idr(slice.unit_type));
        assert_eq!(slice.header.slice_type, SliceType::I);
        assert!(slice.header.first);
        assert!(slice.header.poc_lsb.is_none());
        assert_eq!(slice.temporal_id, 0);
    }

    #[test]
    fn truncated_vps_errors() {
        let units = split265(&[vec![0x40, 0x01]]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::Truncated { .. }
        ));
    }

    #[test]
    fn slice_before_tables_errors() {
        let units = split265(&[hex(DEMO_SLICE)]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::MissingSet { .. }
        ));
    }

    #[test]
    fn sei_parses_as_other() {
        let units = split265(&[vec![0x4E, 0x01, 0x05, 0xFF]]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).expect("sei"),
            ParsedNalu::Other
        ));
    }

    /// Table reference set from `body` at `idx` against `table`.
    fn parse_table_set(body: &[u8], idx: usize, table: &[ShortTermRps]) -> ShortTermRps {
        let mut reader = SharedBitReader::<crate::error::ParseError>::new(body);
        parse_short_term_rps(&mut reader, idx, table.len() + 1, table)
            .expect("rps")
            .set
    }

    #[test]
    fn explicit_rps_round_trips_through_std() {
        // negative=2 (deltas 0,1; used 1,0), positive=1 (delta 2, used 1).
        let bytes = [0x6B, 0x47];
        let set = parse_table_set(&bytes, 0, &[]);
        assert_eq!(
            (&set.negative[..2], &set.negative_used[..2]),
            (&[-1, -3][..], &[true, false][..])
        );
        assert_eq!(
            (&set.positive[..1], &set.positive_used[..1]),
            (&[3][..], &[true][..])
        );
        let std = fill_rps(&set).expect("fill");
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
        let bytes = [0x6B, 0x47];
        let first = parse_table_set(&bytes, 0, &[]);
        // Inter, sign 0, abs 0, used 1/0/1 with the middle kept; the
        // trailing edge candidate carries deltaRps itself, H.265 7.4.8.
        let bytes = [0xB7];
        let sets = [first];
        let set = parse_table_set(&bytes, 1, &sets);
        assert!(set.predicted);
        assert_eq!(set.used_mask, 0b101);
        assert_eq!(set.use_mask, 0b111);
        assert_eq!(
            (&set.negative[..1], &set.negative_used[..1]),
            (&[-2][..], &[false][..])
        );
        assert_eq!(
            (&set.positive[..2], &set.positive_used[..2]),
            (&[1, 4][..], &[true, true][..])
        );
        let std = fill_rps(&set).expect("fill");
        assert_eq!(std.flags.inter_ref_pic_set_prediction_flag(), 1);
        assert_eq!(std.flags.delta_rps_sign(), 0);
        assert_eq!(std.abs_delta_rps_minus1, 0);
        assert_eq!(std.num_negative_pics, 1);
        assert_eq!(std.num_positive_pics, 2);
        assert_eq!(std.used_by_curr_pic_flag, 0b101);
        assert_eq!(std.use_delta_flag, 0b111);
        assert_eq!(&std.delta_poc_s0_minus1[..1], &[1]);
        assert_eq!(&std.delta_poc_s1_minus1[..2], &[0, 2]);
        assert_eq!(std.used_by_curr_pic_s1_flag, 0b11);
    }
}
