//! H.264 Annex-B ingest over the shared grammar.
//!
//! NAL dispatch and set tables over shared parameter sets; picture
//! assembly lives in the reference context.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::ParseError;

use rust_video_h264::{
    is_idr, nal_ref_idc, nal_unit_type, parse_pps, parse_pps_ids, parse_slice_prefix,
    parse_slice_rest, parse_sps, rbsp, BitReader, H264Error, ParseKind, PermissiveGate,
    UnsupportedKind,
};

pub(crate) use rust_video_h264::{
    abs_frame_num, apply_ref_mods, frame_num_wrap, initial_ref_lists, mmco_short_target, poc_type2,
    HrdParams, Marking, Mmco, Nalu, NaluSplitter, PocType, Pps, RefCandidate, ScalingMap,
    ScalingSets, SliceHeader, SlicePoc, Sps, VuiParams,
};

#[cfg(test)]
pub(crate) use rust_video_h264::SliceType;

impl H264Error for ParseError {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Truncated { syntax },
            ParseKind::OverflowU32 { syntax } | ParseKind::OverflowI32 { syntax } => {
                Self::Overflow { syntax }
            }
            ParseKind::TooWide { syntax } | ParseKind::Range { syntax } => {
                Self::OutOfRange { syntax }
            }
            ParseKind::NalForbiddenBit => Self::Unsupported {
                what: "nal forbidden bit",
            },
            ParseKind::SpsWithoutBody => Self::Truncated { syntax: "sps" },
            ParseKind::PpsWithoutBody => Self::Truncated { syntax: "pps" },
            ParseKind::SliceWithoutHeader => Self::Truncated {
                syntax: "slice header",
            },
            ParseKind::SliceHeaderTooLong => Self::OutOfRange {
                syntax: "slice header",
            },
            ParseKind::PpsCitesUnknownSps => Self::Unsupported {
                what: "pps cites unknown sps",
            },
            ParseKind::SliceCitesUnknownPps => Self::Unsupported {
                what: "slice cites unknown pps",
            },
            ParseKind::SliceCitesUnknownSps => Self::Unsupported {
                what: "slice cites unknown sps",
            },
            ParseKind::PictureSlicesDisagree => Self::Unsupported {
                what: "picture slices disagree",
            },
            ParseKind::NonIntraSliceInIdr => Self::OutOfRange {
                syntax: "slice type",
            },
            ParseKind::FrameCropOutsideCodedSize => Self::OutOfRange {
                syntax: "frame crop",
            },
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::Interlaced => Self::Unsupported {
                what: "field pictures",
            },
            UnsupportedKind::ChromaOutside420
            | UnsupportedKind::LumaDepthAbove8Bit
            | UnsupportedKind::ChromaDepthAbove8Bit => Self::Unsupported {
                what: "h264 sampling",
            },
            UnsupportedKind::TransformBypass => Self::Unsupported {
                what: "transform bypass",
            },
            UnsupportedKind::FlexibleMacroblockOrder => Self::Unsupported {
                what: "slice groups",
            },
            UnsupportedKind::DataPartitions => Self::Unsupported {
                what: "data partitioning",
            },
            UnsupportedKind::Multiview => Self::Unsupported { what: "multiview" },
            UnsupportedKind::SpSiSlices => Self::Unsupported {
                what: "sp/si slices",
            },
            UnsupportedKind::RedundantPictures => Self::Unsupported {
                what: "redundant pictures",
            },
            UnsupportedKind::ArbitrarySliceOrder => Self::Unsupported {
                what: "arbitrary slice order",
            },
        }
    }
}

/// Coded slice with its sets and payload for the hardware.
#[derive(Clone, Debug)]
pub(crate) struct Slice {
    /// NAL reference idc.
    pub(crate) ref_idc: u8,
    /// True for IDR slices.
    pub(crate) idr: bool,
    /// Parsed slice header.
    pub(crate) header: SliceHeader,
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
    /// Sequence parameter set.
    Sps(Rc<Sps>),
    /// Picture parameter set.
    Pps(Rc<Pps>),
    /// Coded slice.
    Slice(Box<Slice>),
    /// Anything the tier ignores.
    Other,
}

/// H.264 NAL parser with parameter-set tables.
pub(crate) struct H264Parser {
    /// Sequence sets by id.
    sps: HashMap<u8, Rc<Sps>>,
    /// Picture sets by id.
    pps: HashMap<u8, Rc<Pps>>,
}

impl H264Parser {
    /// Parser with empty tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
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
        let Some(&header) = payload.first() else {
            return Err(ParseError::Truncated {
                syntax: "nal header",
            });
        };
        let ref_idc = nal_ref_idc(header);
        let unit_type = nal_unit_type(header);
        let idr = is_idr(unit_type);
        let rbsp = rbsp(&payload[1..]);
        match unit_type {
            7 => {
                let sps = Rc::new(parse_sps::<ParseError, PermissiveGate>(&rbsp)?);
                self.sps.insert(sps.seq_parameter_set_id, Rc::clone(&sps));
                Ok(ParsedNalu::Sps(sps))
            }
            8 => {
                let (_, sps_id) = parse_pps_ids::<ParseError>(&rbsp)?;
                let sps = self.sps.get(&sps_id);
                let pps = Rc::new(parse_pps::<ParseError, PermissiveGate>(
                    &rbsp,
                    sps.map(Rc::as_ref),
                )?);
                self.pps.insert(pps.pic_parameter_set_id, Rc::clone(&pps));
                Ok(ParsedNalu::Pps(pps))
            }
            1 | 5 => {
                let slice = self.parse_slice(&rbsp, ref_idc, idr, payload)?;
                Ok(ParsedNalu::Slice(Box::new(slice)))
            }
            2..=4 => Err(ParseError::Unsupported {
                what: "data partitioning",
            }),
            _ => Ok(ParsedNalu::Other),
        }
    }

    /// Parse a slice header against the stored tables.
    fn parse_slice(
        &self,
        body: &[u8],
        ref_idc: u8,
        idr: bool,
        payload: &[u8],
    ) -> Result<Slice, ParseError> {
        let mut reader = BitReader::new(body);
        let prefix = parse_slice_prefix::<ParseError>(&mut reader)?;
        let pps = self
            .pps
            .get(&prefix.3)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(prefix.3),
            })?;
        let sps =
            self.sps
                .get(&pps.seq_parameter_set_id)
                .cloned()
                .ok_or(ParseError::MissingSet {
                    id: u32::from(pps.seq_parameter_set_id),
                })?;
        let header = parse_slice_rest::<ParseError, PermissiveGate>(
            &mut reader,
            prefix,
            idr,
            ref_idc,
            &sps,
            &pps,
        )?;
        Ok(Slice {
            ref_idc,
            idr,
            header,
            sps,
            pps,
            payload: payload.to_vec(),
        })
    }
}

impl Default for H264Parser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{H264Parser, ParsedNalu, SliceType};
    use crate::fixtures::{hex, split264};

    /// First two NAL units of the fork `demo.h264` vector.
    const DEMO_SPS: &str = "6764000dacb20283f60220000003002000000641e2854900";
    /// First PPS of the fork `demo.h264` vector.
    const DEMO_PPS: &str = "68ebc3cb22c0";
    /// Head of the first slice of the fork `demo.h264` vector.
    const DEMO_SLICE: &str =
        "6588840cfffef6ecbe053614c85094a04007bb443688f75792a97d0e881b266f395d1803e3eea5efbae04cafddfd1d8f5aa3b94b6acca893a915d63dcd";

    #[test]
    fn demo_sps_matches_ffprobe() {
        let units = split264(&[hex(DEMO_SPS)]);
        let mut parser = H264Parser::new();
        let ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        assert_eq!(sps.profile_idc, 100);
        assert_eq!(sps.level_idc, 13);
        assert_eq!((sps.width(), sps.height()), (320, 240));
        assert_eq!(sps.display_size(), (320, 240));
        assert_eq!(sps.fidelity.chroma_format_idc, 1);
    }

    #[test]
    fn demo_pps_resolves_its_sps() {
        let units = split264(&[hex(DEMO_SPS), hex(DEMO_PPS)]);
        let mut parser = H264Parser::default();
        parser.parse(&units[0]).expect("sps");
        let ParsedNalu::Pps(pps) = parser.parse(&units[1]).expect("pps") else {
            panic!("want pps");
        };
        assert_eq!(pps.seq_parameter_set_id, 0);
        assert!(matches!(
            parser.parse(&units[0]).expect("sps again"),
            ParsedNalu::Sps(_)
        ));
    }

    #[test]
    fn demo_slice_parses_against_tables() {
        let units = split264(&[hex(DEMO_SPS), hex(DEMO_PPS), hex(DEMO_SLICE)]);
        let mut parser = H264Parser::new();
        parser.parse(&units[0]).expect("sps");
        parser.parse(&units[1]).expect("pps");
        let ParsedNalu::Slice(slice) = parser.parse(&units[2]).expect("slice") else {
            panic!("want slice");
        };
        assert!(slice.idr);
        assert_eq!(slice.header.slice_type, SliceType::I);
        assert_eq!(slice.header.first_mb, 0);
        assert_eq!(slice.payload[0], 0x65);
    }

    #[test]
    fn truncated_sps_errors() {
        let units = split264(&[vec![0x67]]);
        let mut parser = H264Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::Truncated { .. }
        ));
    }

    #[test]
    fn slice_before_tables_errors() {
        let units = split264(&[hex(DEMO_SLICE)]);
        let mut parser = H264Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::MissingSet { .. }
        ));
    }

    #[test]
    fn sei_parses_as_other() {
        let units = split264(&[vec![0x06, 0x05, 0xFF]]);
        let mut parser = H264Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).expect("sei"),
            ParsedNalu::Other
        ));
    }
}
