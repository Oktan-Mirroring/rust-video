//! AV1 OBU ingest over the shared grammar.
//!
//! OBU framing and temporal units over shared headers; frame
//! assembly lives in the reference context.

use crate::error::ParseError;

use rust_video_av1 as shared;

pub(crate) use rust_video_av1::{
    parse_frame_header, parse_sequence_header, relative_dist, skip_mode_frames, split_tiles,
    tile_group_range, tile_log2, FrameHeader, FrameType, RefInfo, SequenceHeader,
    INTERP_SWITCHABLE, REFS_PER_FRAME, SEQ_COLOR_RANGE, SUPERRES_DENOM_MIN, SUPERRES_NUM,
    TOTAL_REFS,
};

#[cfg(test)]
pub(crate) use rust_video_av1::{
    OperatingPoint, SEQ_FRAME_ID_NUMBERS, SEQ_SUBSAMPLING_X, SEQ_SUBSAMPLING_Y,
};

/// AV1 DPB slot budget: reference slots plus the picture under decode.
pub(crate) const DPB_SLOTS: u32 = 9;

const _: () = assert!(shared::NUM_REF_FRAMES + 1 == DPB_SLOTS as usize);

/// OBU type of one unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObuType {
    /// Sequence header.
    SequenceHeader,
    /// Temporal delimiter.
    TemporalDelimiter,
    /// Frame header.
    FrameHeader,
    /// Tile group.
    TileGroup,
    /// Metadata, skipped.
    Metadata,
    /// Frame header plus tile group.
    Frame,
    /// Repeated frame header, parsed as a frame header.
    RedundantFrameHeader,
    /// Tile list for large stills.
    TileList,
    /// Padding, skipped.
    Padding,
}

impl ObuType {
    /// OBU type of the 4-bit `code`, if defined.
    fn of(code: u8) -> Result<Self, ParseError> {
        match code {
            1 => Ok(Self::SequenceHeader),
            2 => Ok(Self::TemporalDelimiter),
            3 => Ok(Self::FrameHeader),
            4 => Ok(Self::TileGroup),
            5 => Ok(Self::Metadata),
            6 => Ok(Self::Frame),
            7 => Ok(Self::RedundantFrameHeader),
            8 => Ok(Self::TileList),
            15 => Ok(Self::Padding),
            _ => Err(ParseError::Unsupported {
                what: "av1 obu type",
            }),
        }
    }

    /// True for OBUs opening a frame.
    fn opens_frame(self) -> bool {
        matches!(
            self,
            Self::Frame | Self::FrameHeader | Self::RedundantFrameHeader
        )
    }

    /// True for OBUs opening a temporal unit.
    fn opens_unit(self) -> bool {
        matches!(self, Self::TemporalDelimiter | Self::SequenceHeader)
    }
}

/// One OBU: its type plus the payload past the header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Obu<'a> {
    /// OBU type.
    kind: ObuType,
    /// Payload past the OBU header and size field.
    payload: &'a [u8],
}

impl<'a> Obu<'a> {
    /// OBU type.
    pub(crate) fn kind(&self) -> ObuType {
        self.kind
    }

    /// Payload past the OBU header and size field.
    pub(crate) fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// OBUs of one buffer in order.
///
/// A unit without a size field runs to the buffer end, so low-overhead
/// streams split correctly only when fed one temporal unit per call.
pub(crate) fn split_obus(bytes: &[u8]) -> Result<Vec<Obu<'_>>, ParseError> {
    let mut obus = Vec::new();
    for unit in shared::split_obus::<ParseError>(bytes)? {
        let kind = ObuType::of(unit.unit_type)?;
        if unit.temporal_id != 0 || unit.spatial_id != 0 {
            return Err(ParseError::Unsupported {
                what: "av1 layered stream",
            });
        }
        obus.push(Obu {
            kind,
            payload: unit.payload,
        });
    }
    Ok(obus)
}

/// OBUs decoded as one display instant.
pub(crate) struct TemporalUnit<'a> {
    /// OBUs of the unit in order.
    obus: Vec<Obu<'a>>,
}

impl<'a> TemporalUnit<'a> {
    /// OBUs of the unit in order.
    pub(crate) fn obus(&self) -> &[Obu<'a>] {
        &self.obus
    }
}

/// Temporal units of parsed `obus` in order.
pub(crate) fn temporal_units(obus: Vec<Obu<'_>>) -> Vec<TemporalUnit<'_>> {
    let mut units: Vec<TemporalUnit<'_>> = Vec::new();
    for obu in obus {
        let cut = obu.kind().opens_unit()
            || (obu.kind().opens_frame()
                && units
                    .last()
                    .is_some_and(|unit| unit.obus.iter().any(|held| held.kind().opens_frame())));
        if cut || units.is_empty() {
            units.push(TemporalUnit { obus: Vec::new() });
        }
        if let Some(unit) = units.last_mut() {
            unit.obus.push(obu);
        }
    }
    units.retain(|unit| !unit.obus.is_empty());
    units
}

impl From<shared::Error> for ParseError {
    fn from(error: shared::Error) -> Self {
        use shared::{
            BrokenReferenceReason as Broken, ParseReason as Parse, UnsupportedReason as Unsupported,
        };
        match error {
            shared::Error::Parse(reason) => match reason {
                Parse::Truncated { syntax } => Self::Truncated { syntax },
                Parse::OverflowU32 { syntax }
                | Parse::OverflowI32 { syntax }
                | Parse::TooWide { syntax } => Self::Overflow { syntax },
                Parse::Range { syntax } => Self::OutOfRange { syntax },
                Parse::ObuForbiddenBit
                | Parse::ObuReservedBit
                | Parse::ObuExtensionReservedBits => Self::OutOfRange {
                    syntax: "obu header",
                },
                Parse::ObuSizeExceedsPayload => Self::Truncated {
                    syntax: "obu payload",
                },
                Parse::ReservedObuType => Self::Unsupported {
                    what: "av1 obu type",
                },
                Parse::FrameBeforeSequenceHeader => Self::OutOfRange {
                    syntax: "sequence header",
                },
                Parse::FrameWithoutTileData => Self::Truncated {
                    syntax: "tile data",
                },
                Parse::IncompleteTileGroup => Self::Truncated {
                    syntax: "tile group range",
                },
                Parse::RedundantHeaderMismatch => Self::OutOfRange {
                    syntax: "redundant header",
                },
                Parse::TileGridCoverage => Self::OutOfRange {
                    syntax: "tile grid",
                },
                Parse::TileGroupOrder | Parse::TileGroupWithoutFrameHeader => Self::OutOfRange {
                    syntax: "tile group range",
                },
                Parse::TileGroupWithoutData => Self::Truncated {
                    syntax: "tile group align",
                },
            },
            shared::Error::Unsupported(reason) => Self::Unsupported {
                what: match reason {
                    Unsupported::TileList => "av1 tile list",
                    Unsupported::SpatialLayers => "av1 layered stream",
                    Unsupported::SuperresUpscale => "av1 superres upscale",
                    Unsupported::RenderSizeDiffersFromFrameSize => "av1 render size",
                    Unsupported::Av1BitDepthNeedsTarget { .. } => "av1 bit depth",
                    Unsupported::Av1ChromaOutside420 => "av1 chroma",
                    Unsupported::Av1Monochrome => "av1 monochrome",
                    Unsupported::TileCountAbove256 => "av1 tile count",
                },
            },
            shared::Error::BrokenReference(Broken::NamesEmptySlot { reference }) => {
                Self::OutOfRange { syntax: reference }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{split_obus, temporal_units};

    #[test]
    fn split_obus_cuts_sized_and_bare_units() {
        let bytes = [0x0a, 0x02, 0xaa, 0xbb, 0x12, 0x00];
        let obus = split_obus(&bytes).expect("obus");
        assert_eq!(obus.len(), 2);
        assert_eq!(obus[0].payload(), &[0xaa, 0xbb]);
        assert!(obus[1].payload().is_empty(), "empty tail");
        let bare = split_obus(&[0x08, 0xaa]).expect("bare");
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].payload(), &[0xaa]);
        assert!(split_obus(&[0x80]).is_err());
        assert!(split_obus(&[0x0b]).is_err());
    }

    #[test]
    fn split_obus_declines_nonzero_layer_ids() {
        let base = split_obus(&[0x0e, 0x00, 0x01, 0xaa]).expect("base layer");
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].payload(), &[0xaa]);
        assert!(split_obus(&[0x0e, 0x20, 0x01, 0xaa]).is_err());
        assert!(split_obus(&[0x0e, 0x08, 0x01, 0xaa]).is_err());
    }

    #[test]
    fn temporal_units_cut_on_delimiters_and_frames() {
        let bytes = [
            0x12, 0x00, 0x0a, 0x01, 0x00, 0x32, 0x01, 0x00, 0x32, 0x01, 0x00,
        ];
        let obus = split_obus(&bytes).expect("obus");
        let units = temporal_units(obus);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].obus().len(), 1);
        assert_eq!(units[1].obus().len(), 2);
        assert_eq!(units[2].obus().len(), 1);
    }
}
