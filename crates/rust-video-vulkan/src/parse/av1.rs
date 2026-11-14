//! AV1 OBU splitting plus sequence and frame header parsing.
//!
//! Syntax mirrors the `AOMedia` AV1 specification sections 5.3, 5.5, and 5.9:
//! each parse function consumes one OBU payload in bitstream order, with
//! reference state borrowed from the caller for inference and validation.

use crate::error::ParseError;
use crate::parse::bits::BitReader;

/// AV1 reference frame slot count.
pub(crate) const NUM_REF_FRAMES: u32 = 8;
/// AV1 DPB slot budget: reference slots plus the picture under decode.
pub(crate) const DPB_SLOTS: u32 = NUM_REF_FRAMES + 1;
/// AV1 inter reference names per frame, `LAST` through `ALTREF`.
pub(crate) const REFS_PER_FRAME: usize = 7;
/// AV1 reference slots including the intra slot.
pub(crate) const TOTAL_REFS: usize = 8;
/// AV1 segmentation segment count.
const MAX_SEGMENTS: usize = 8;
/// AV1 segmentation feature count.
const SEG_LVL_MAX: usize = 8;
/// AV1 maximum tile columns.
const MAX_TILE_COLS: usize = 64;
/// AV1 maximum tile rows.
const MAX_TILE_ROWS: usize = 64;
/// AV1 frame size selected per frame.
const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;
/// AV1 motion vector precision selected per frame.
const SELECT_INTEGER_MV: u8 = 2;
/// AV1 primary reference indicating past independence.
const PRIMARY_REF_NONE: u8 = 7;
/// AV1 key frame type.
const FRAME_KEY: u8 = 0;
/// AV1 inter frame type.
const FRAME_INTER: u8 = 1;
/// AV1 intra-only frame type.
const FRAME_INTRA_ONLY: u8 = 2;
/// AV1 switch frame type.
const FRAME_SWITCH: u8 = 3;
/// AV1 maximum tile width in luma samples.
const MAX_TILE_WIDTH: u32 = 4096;
/// AV1 maximum tile area in luma samples.
const MAX_TILE_AREA: u32 = 4096 * 2304;
/// AV1 superres scale numerator.
const SUPERRES_NUM: u32 = 8;
/// AV1 superres denominator minimum.
const SUPERRES_DENOM_MIN: u32 = 9;
/// AV1 global motion signed parameter bit counts.
const GM_ABS_ALPHA_BITS: u32 = 12;
/// AV1 translation-only global motion bit count.
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
/// AV1 translation global motion bit count.
const GM_ABS_TRANS_BITS: u32 = 12;
/// AV1 warped model precision bits.
const WARPEDMODEL_PREC_BITS: u32 = 16;
/// AV1 translation-only precision bits.
const TRANS_ONLY_PREC_BITS: u32 = 3;
/// AV1 translation precision bits.
const TRANS_PREC_BITS: u32 = 6;
/// AV1 alpha precision bits.
const ALPHA_PREC_BITS: u32 = 15;

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

/// Unsigned LEB128 value plus its byte length.
fn uleb128(bytes: &[u8], syntax: &'static str) -> Result<(u64, usize), ParseError> {
    let mut value: u64 = 0;
    for (index, byte) in bytes.iter().enumerate().take(8) {
        let low = u64::from(byte & 0x7f);
        let shift = u32::try_from(index * 7).map_err(|_| ParseError::OutOfRange { syntax })?;
        value |= low
            .checked_shl(shift)
            .ok_or(ParseError::OutOfRange { syntax })?;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(ParseError::Overflow { syntax })
}

/// OBUs of one buffer in order.
///
/// A unit without a size field runs to the buffer end, so low-overhead
/// streams split correctly only when fed one temporal unit per call.
pub(crate) fn split_obus(bytes: &[u8]) -> Result<Vec<Obu<'_>>, ParseError> {
    let mut obus = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let header = rest[0];
        if header & 0x80 != 0 {
            return Err(ParseError::OutOfRange {
                syntax: "obu header",
            });
        }
        let kind = ObuType::of((header >> 3) & 0x0f)?;
        let extension = (header >> 2) & 1 == 1;
        let has_size = (header >> 1) & 1 == 1;
        if header & 1 != 0 {
            return Err(ParseError::OutOfRange {
                syntax: "obu header",
            });
        }
        let mut at = 1;
        if extension {
            if rest.len() < 2 {
                return Err(ParseError::Truncated {
                    syntax: "obu header",
                });
            }
            if rest[1] & 0x07 != 0 {
                return Err(ParseError::OutOfRange {
                    syntax: "obu header",
                });
            }
            if rest[1] & 0xf8 != 0 {
                return Err(ParseError::Unsupported {
                    what: "av1 layered stream",
                });
            }
            at = 2;
        }
        let payload;
        if has_size {
            let (size, len) = uleb128(&rest[at..], "obu size")?;
            at += len;
            let end = at
                .checked_add(
                    usize::try_from(size)
                        .map_err(|_| ParseError::OutOfRange { syntax: "obu size" })?,
                )
                .ok_or(ParseError::OutOfRange { syntax: "obu size" })?;
            if rest.len() < end {
                return Err(ParseError::Truncated {
                    syntax: "obu payload",
                });
            }
            payload = &rest[at..end];
            at = end;
        } else {
            payload = &rest[at..];
            at = rest.len();
        }
        obus.push(Obu { kind, payload });
        rest = &rest[at..];
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

/// Checked narrowing of a parsed value.
fn narrow_u8(value: u32, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a counted value.
fn narrow_u32(value: usize, syntax: &'static str) -> Result<u32, ParseError> {
    u32::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a parsed value.
fn narrow_u16(value: u32, syntax: &'static str) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a parsed value.
fn narrow_i8(value: i32, syntax: &'static str) -> Result<i8, ParseError> {
    i8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// AV1 timing info of one sequence header.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TimingInfo {
    /// Display tick units per time scale.
    pub num_units_in_display_tick: u32,
    /// Ticks per second.
    pub time_scale: u32,
    /// Pictures share one tick count.
    pub equal_picture_interval: bool,
    /// Ticks per picture minus one.
    pub num_ticks_per_picture_minus_1: u32,
}

/// AV1 decoder model info of one sequence header.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DecoderModelInfo {
    /// Buffer delay field width minus one.
    pub buffer_delay_length_minus_1: u8,
    /// Decoding tick units per time scale.
    pub num_units_in_decoding_tick: u32,
    /// Removal time field width minus one.
    pub buffer_removal_time_length_minus_1: u8,
    /// Presentation time field width minus one.
    pub frame_presentation_time_length_minus_1: u8,
}

/// AV1 operating point of one sequence header.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OperatingPoint {
    /// Temporal and spatial layer mask.
    pub idc: u16,
    /// Level index of the point.
    pub level_idx: u8,
    /// Tier of the point.
    pub tier: u8,
    /// Decoder model parameters present.
    pub decoder_present: bool,
    /// Decoder buffer delay in ticks.
    pub decoder_buffer_delay: u32,
    /// Encoder buffer delay in ticks.
    pub encoder_buffer_delay: u32,
    /// Low delay mode.
    pub low_delay: bool,
    /// Initial display delay present.
    pub initial_delay_present: bool,
    /// Initial display delay minus one.
    pub initial_delay_minus_1: u8,
}

/// AV1 color depth of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ColorDepth {
    /// Ten-bit depth, twelve with `twelve_bit`.
    pub high_bitdepth: bool,
    /// Twelve-bit depth.
    pub twelve_bit: bool,
    /// Single plane.
    pub mono_chrome: bool,
}

/// AV1 color config of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ColorConfig {
    /// Color depth.
    pub depth: ColorDepth,
    /// Color description present.
    pub color_description_present: bool,
    /// Color primaries.
    pub color_primaries: u8,
    /// Transfer characteristics.
    pub transfer_characteristics: u8,
    /// Matrix coefficients.
    pub matrix_coefficients: u8,
    /// Full range.
    pub color_range: bool,
    /// Horizontal subsampling.
    pub subsampling_x: u8,
    /// Vertical subsampling.
    pub subsampling_y: u8,
    /// Chroma sample position.
    pub chroma_sample_position: u8,
    /// Separate U and V delta quantizers.
    pub separate_uv_delta_q: bool,
}

/// AV1 still picture flags of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeqStill {
    /// Still picture sequence.
    pub still_picture: bool,
    /// Reduced still picture header.
    pub reduced: bool,
}

/// AV1 metadata presence of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeqPresence {
    /// Timing info present.
    pub timing_present: bool,
    /// Decoder model info present.
    pub decoder_model_present: bool,
    /// Initial display delay present.
    pub initial_display_delay_present: bool,
}

/// AV1 block coding tools of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeqBlocks {
    /// 128x128 superblocks.
    pub use_128x128_superblock: bool,
    /// Filter intra prediction.
    pub enable_filter_intra: bool,
    /// Intra edge filter.
    pub enable_intra_edge_filter: bool,
}

/// AV1 compound prediction tools of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeqCompound {
    /// Inter-intra compound prediction.
    pub enable_interintra_compound: bool,
    /// Masked compound prediction.
    pub enable_masked_compound: bool,
    /// Warped motion compensation.
    pub enable_warped_motion: bool,
}

/// AV1 loop filters of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeqFilters {
    /// Dual filter.
    pub enable_dual_filter: bool,
    /// Constrained directional enhancement filter.
    pub enable_cdef: bool,
    /// Loop restoration.
    pub enable_restoration: bool,
}

/// AV1 order hint tools of one sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeqOrder {
    /// Order hints.
    pub enable_order_hint: bool,
    /// Joint compound prediction.
    pub enable_jnt_comp: bool,
    /// Reference frame motion vectors.
    pub enable_ref_frame_mvs: bool,
}

/// Parsed AV1 sequence header.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SequenceHeader {
    /// Sequence profile, 0 through 2.
    profile: u8,
    /// Still picture flags.
    still: SeqStill,
    /// Metadata presence.
    presence: SeqPresence,
    /// Timing info.
    timing: TimingInfo,
    /// Decoder model info.
    decoder_model: DecoderModelInfo,
    /// Operating points.
    operating_points: Vec<OperatingPoint>,
    /// Frame width field width minus one.
    frame_width_bits_minus_1: u8,
    /// Frame height field width minus one.
    frame_height_bits_minus_1: u8,
    /// Maximum frame width minus one.
    max_frame_width_minus_1: u16,
    /// Maximum frame height minus one.
    max_frame_height_minus_1: u16,
    /// Frame id numbers present.
    frame_id_numbers_present: bool,
    /// Frame id delta width minus two.
    delta_frame_id_length_minus_2: u8,
    /// Extra frame id width minus one.
    additional_frame_id_length_minus_1: u8,
    /// Block coding tools.
    blocks: SeqBlocks,
    /// Compound prediction tools.
    compound: SeqCompound,
    /// Loop filters.
    filters: SeqFilters,
    /// Order hint tools.
    order: SeqOrder,
    /// Forced screen content tools, 2 selects per frame.
    seq_force_screen_content_tools: u8,
    /// Forced integer motion vectors, 2 selects per frame.
    seq_force_integer_mv: u8,
    /// Order hint width minus one.
    order_hint_bits_minus_1: u8,
    /// Super-resolution.
    enable_superres: bool,
    /// Color config.
    color: ColorConfig,
    /// Film grain parameters present.
    film_grain_params_present: bool,
    /// Bit depth, 8 through 12.
    bit_depth: u8,
    /// Plane count, 1 or 3.
    num_planes: u8,
}

impl SequenceHeader {
    /// Sequence profile, 0 through 2.
    pub(crate) fn profile(&self) -> u8 {
        self.profile
    }

    /// Level index of the first operating point.
    pub(crate) fn level_idx(&self) -> u8 {
        self.operating_points
            .first()
            .map_or(0, |point| point.level_idx)
    }

    /// Maximum frame size in luma samples.
    pub(crate) fn coded_size(&self) -> (u32, u32) {
        (
            u32::from(self.max_frame_width_minus_1) + 1,
            u32::from(self.max_frame_height_minus_1) + 1,
        )
    }

    /// Single plane.
    pub(crate) fn mono_chrome(&self) -> bool {
        self.color.depth.mono_chrome
    }

    /// Horizontal subsampling.
    pub(crate) fn subsampling_x(&self) -> u8 {
        self.color.subsampling_x
    }

    /// Vertical subsampling.
    pub(crate) fn subsampling_y(&self) -> u8 {
        self.color.subsampling_y
    }

    /// Bit depth, 8 through 12.
    pub(crate) fn bit_depth(&self) -> u8 {
        self.bit_depth
    }

    /// Still picture sequence.
    pub(crate) fn still_picture(&self) -> bool {
        self.still.still_picture
    }

    /// Reduced still picture header.
    pub(crate) fn reduced(&self) -> bool {
        self.still.reduced
    }

    /// Timing info present.
    pub(crate) fn timing_present(&self) -> bool {
        self.presence.timing_present
    }

    /// Timing info.
    pub(crate) fn timing(&self) -> &TimingInfo {
        &self.timing
    }

    /// Decoder model info present.
    pub(crate) fn decoder_model_present(&self) -> bool {
        self.presence.decoder_model_present
    }

    /// Decoder model info.
    pub(crate) fn decoder_model(&self) -> &DecoderModelInfo {
        &self.decoder_model
    }

    /// Initial display delay present.
    pub(crate) fn initial_display_delay_present(&self) -> bool {
        self.presence.initial_display_delay_present
    }

    /// Operating points.
    pub(crate) fn operating_points(&self) -> &[OperatingPoint] {
        &self.operating_points
    }

    /// Frame width field width minus one.
    pub(crate) fn frame_width_bits_minus_1(&self) -> u8 {
        self.frame_width_bits_minus_1
    }

    /// Frame height field width minus one.
    pub(crate) fn frame_height_bits_minus_1(&self) -> u8 {
        self.frame_height_bits_minus_1
    }

    /// Maximum frame width minus one.
    pub(crate) fn max_frame_width_minus_1(&self) -> u16 {
        self.max_frame_width_minus_1
    }

    /// Maximum frame height minus one.
    pub(crate) fn max_frame_height_minus_1(&self) -> u16 {
        self.max_frame_height_minus_1
    }

    /// Frame id numbers present.
    pub(crate) fn frame_id_numbers_present(&self) -> bool {
        self.frame_id_numbers_present
    }

    /// Frame id delta width minus two.
    pub(crate) fn delta_frame_id_length_minus_2(&self) -> u8 {
        self.delta_frame_id_length_minus_2
    }

    /// Extra frame id width minus one.
    pub(crate) fn additional_frame_id_length_minus_1(&self) -> u8 {
        self.additional_frame_id_length_minus_1
    }

    /// 128x128 superblocks.
    pub(crate) fn use_128x128_superblock(&self) -> bool {
        self.blocks.use_128x128_superblock
    }

    /// Filter intra prediction.
    pub(crate) fn enable_filter_intra(&self) -> bool {
        self.blocks.enable_filter_intra
    }

    /// Intra edge filter.
    pub(crate) fn enable_intra_edge_filter(&self) -> bool {
        self.blocks.enable_intra_edge_filter
    }

    /// Inter-intra compound prediction.
    pub(crate) fn enable_interintra_compound(&self) -> bool {
        self.compound.enable_interintra_compound
    }

    /// Masked compound prediction.
    pub(crate) fn enable_masked_compound(&self) -> bool {
        self.compound.enable_masked_compound
    }

    /// Warped motion compensation.
    pub(crate) fn enable_warped_motion(&self) -> bool {
        self.compound.enable_warped_motion
    }

    /// Dual filter.
    pub(crate) fn enable_dual_filter(&self) -> bool {
        self.filters.enable_dual_filter
    }

    /// Order hints.
    pub(crate) fn enable_order_hint(&self) -> bool {
        self.order.enable_order_hint
    }

    /// Joint compound prediction.
    pub(crate) fn enable_jnt_comp(&self) -> bool {
        self.order.enable_jnt_comp
    }

    /// Reference frame motion vectors.
    pub(crate) fn enable_ref_frame_mvs(&self) -> bool {
        self.order.enable_ref_frame_mvs
    }

    /// Forced screen content tools, 2 selects per frame.
    pub(crate) fn seq_force_screen_content_tools(&self) -> u8 {
        self.seq_force_screen_content_tools
    }

    /// Forced integer motion vectors, 2 selects per frame.
    pub(crate) fn seq_force_integer_mv(&self) -> u8 {
        self.seq_force_integer_mv
    }

    /// Order hint width minus one.
    pub(crate) fn order_hint_bits_minus_1(&self) -> u8 {
        self.order_hint_bits_minus_1
    }

    /// Order hint width in bits, zero without order hints.
    pub(crate) fn order_hint_bits(&self) -> u32 {
        if self.order.enable_order_hint {
            u32::from(self.order_hint_bits_minus_1) + 1
        } else {
            0
        }
    }

    /// Super-resolution.
    pub(crate) fn enable_superres(&self) -> bool {
        self.enable_superres
    }

    /// Constrained directional enhancement filter.
    pub(crate) fn enable_cdef(&self) -> bool {
        self.filters.enable_cdef
    }

    /// Loop restoration.
    pub(crate) fn enable_restoration(&self) -> bool {
        self.filters.enable_restoration
    }

    /// Color config.
    pub(crate) fn color(&self) -> &ColorConfig {
        &self.color
    }

    /// Film grain parameters present.
    pub(crate) fn film_grain_params_present(&self) -> bool {
        self.film_grain_params_present
    }

    /// Plane count, 1 or 3.
    pub(crate) fn num_planes(&self) -> u8 {
        self.num_planes
    }

    /// Main profile 4:2:0 8-bit sequence header for session tests.
    #[cfg(test)]
    pub(crate) fn test_main_420() -> Self {
        let mut seq = Self::test_with_frame_ids(0, 0);
        seq.frame_id_numbers_present = false;
        seq.color.depth.mono_chrome = false;
        seq.color.subsampling_x = 1;
        seq.color.subsampling_y = 1;
        seq.bit_depth = 8;
        seq.num_planes = 3;
        seq.max_frame_width_minus_1 = 319;
        seq.max_frame_height_minus_1 = 239;
        seq.operating_points = vec![OperatingPoint {
            level_idx: 4,
            ..OperatingPoint::default()
        }];
        seq
    }

    /// Sequence header with frame ids for fill tests.
    #[cfg(test)]
    pub(crate) fn test_with_frame_ids(additional: u8, delta: u8) -> Self {
        Self {
            profile: 0,
            still: SeqStill::default(),
            presence: SeqPresence::default(),
            timing: TimingInfo::default(),
            decoder_model: DecoderModelInfo::default(),
            operating_points: Vec::new(),
            frame_width_bits_minus_1: 0,
            frame_height_bits_minus_1: 0,
            max_frame_width_minus_1: 0,
            max_frame_height_minus_1: 0,
            frame_id_numbers_present: true,
            delta_frame_id_length_minus_2: delta,
            additional_frame_id_length_minus_1: additional,
            blocks: SeqBlocks::default(),
            compound: SeqCompound::default(),
            filters: SeqFilters::default(),
            order: SeqOrder::default(),
            seq_force_screen_content_tools: 0,
            seq_force_integer_mv: 0,
            order_hint_bits_minus_1: 0,
            enable_superres: false,
            color: ColorConfig::default(),
            film_grain_params_present: false,
            bit_depth: 8,
            num_planes: 3,
        }
    }
}

/// Sequence header of one OBU payload.
pub(crate) fn parse_sequence_header(payload: &[u8]) -> Result<SequenceHeader, ParseError> {
    let mut reader = BitReader::new(payload);
    let profile = narrow_u8(reader.bits("seq profile", 3)?, "seq profile")?;
    if profile > 2 {
        return Err(ParseError::OutOfRange {
            syntax: "seq profile",
        });
    }
    let still_picture = reader.bit("seq still")?;
    let reduced = reader.bit("seq reduced")?;
    let mut header = SequenceHeader {
        profile,
        still: SeqStill {
            still_picture,
            reduced,
        },
        presence: SeqPresence::default(),
        timing: TimingInfo::default(),
        decoder_model: DecoderModelInfo::default(),
        operating_points: Vec::new(),
        frame_width_bits_minus_1: 0,
        frame_height_bits_minus_1: 0,
        max_frame_width_minus_1: 0,
        max_frame_height_minus_1: 0,
        frame_id_numbers_present: false,
        delta_frame_id_length_minus_2: 0,
        additional_frame_id_length_minus_1: 0,
        blocks: SeqBlocks::default(),
        compound: SeqCompound::default(),
        filters: SeqFilters::default(),
        order: SeqOrder::default(),
        seq_force_screen_content_tools: 0,
        seq_force_integer_mv: 0,
        order_hint_bits_minus_1: 0,
        enable_superres: false,
        color: ColorConfig::default(),
        film_grain_params_present: false,
        bit_depth: 8,
        num_planes: 3,
    };
    parse_operating_points(&mut reader, &mut header)?;
    header.frame_width_bits_minus_1 =
        narrow_u8(reader.bits("seq width bits", 4)?, "seq width bits")?;
    header.frame_height_bits_minus_1 =
        narrow_u8(reader.bits("seq height bits", 4)?, "seq height bits")?;
    header.max_frame_width_minus_1 = narrow_u16(
        reader.bits(
            "seq max width",
            u32::from(header.frame_width_bits_minus_1) + 1,
        )?,
        "seq max width",
    )?;
    header.max_frame_height_minus_1 = narrow_u16(
        reader.bits(
            "seq max height",
            u32::from(header.frame_height_bits_minus_1) + 1,
        )?,
        "seq max height",
    )?;
    header.frame_id_numbers_present = if reduced {
        false
    } else {
        reader.bit("seq frame ids")?
    };
    if header.frame_id_numbers_present {
        header.delta_frame_id_length_minus_2 =
            narrow_u8(reader.bits("seq delta id bits", 4)?, "seq delta id bits")?;
        header.additional_frame_id_length_minus_1 =
            narrow_u8(reader.bits("seq extra id bits", 3)?, "seq extra id bits")?;
    }
    header.blocks.use_128x128_superblock = reader.bit("seq superblock")?;
    header.blocks.enable_filter_intra = reader.bit("seq filter intra")?;
    header.blocks.enable_intra_edge_filter = reader.bit("seq edge filter")?;
    parse_seq_enables(&mut reader, &mut header)?;
    header.enable_superres = reader.bit("seq superres")?;
    header.filters.enable_cdef = reader.bit("seq cdef")?;
    header.filters.enable_restoration = reader.bit("seq restoration")?;
    parse_color_config(&mut reader, &mut header)?;
    header.film_grain_params_present = reader.bit("seq film grain")?;
    Ok(header)
}

/// Operating points of the sequence header into `header`.
fn parse_operating_points(
    reader: &mut BitReader<'_>,
    header: &mut SequenceHeader,
) -> Result<(), ParseError> {
    if header.still.reduced {
        let level = narrow_u8(reader.bits("seq level", 5)?, "seq level")?;
        header.operating_points.push(OperatingPoint {
            level_idx: level,
            ..OperatingPoint::default()
        });
        return Ok(());
    }
    header.presence.timing_present = reader.bit("seq timing present")?;
    if header.presence.timing_present {
        header.timing.num_units_in_display_tick = reader.bits("seq tick units", 32)?;
        if header.timing.num_units_in_display_tick == 0 {
            return Err(ParseError::OutOfRange {
                syntax: "seq tick units",
            });
        }
        header.timing.time_scale = reader.bits("seq time scale", 32)?;
        if header.timing.time_scale == 0 {
            return Err(ParseError::OutOfRange {
                syntax: "seq time scale",
            });
        }
        header.timing.equal_picture_interval = reader.bit("seq equal interval")?;
        if header.timing.equal_picture_interval {
            header.timing.num_ticks_per_picture_minus_1 = reader.uvlc("seq ticks per picture")?;
        }
        header.presence.decoder_model_present = reader.bit("seq decoder model")?;
        if header.presence.decoder_model_present {
            header.decoder_model.buffer_delay_length_minus_1 = narrow_u8(
                reader.bits("seq buffer delay bits", 5)?,
                "seq buffer delay bits",
            )?;
            header.decoder_model.num_units_in_decoding_tick =
                reader.bits("seq decoding tick", 32)?;
            if header.decoder_model.num_units_in_decoding_tick == 0 {
                return Err(ParseError::OutOfRange {
                    syntax: "seq decoding tick",
                });
            }
            header.decoder_model.buffer_removal_time_length_minus_1 =
                narrow_u8(reader.bits("seq removal bits", 5)?, "seq removal bits")?;
            header.decoder_model.frame_presentation_time_length_minus_1 = narrow_u8(
                reader.bits("seq presentation bits", 5)?,
                "seq presentation bits",
            )?;
        }
    }
    header.presence.initial_display_delay_present = reader.bit("seq delay present")?;
    let count = reader.bits("seq operating points", 5)? + 1;
    for _ in 0..count {
        let idc = narrow_u16(reader.bits("seq op idc", 12)?, "seq op idc")?;
        let level_idx = narrow_u8(reader.bits("seq op level", 5)?, "seq op level")?;
        let tier = if level_idx > 7 {
            u8::from(reader.bit("seq op tier")?)
        } else {
            0
        };
        let mut point = OperatingPoint {
            idc,
            level_idx,
            tier,
            ..OperatingPoint::default()
        };
        if header.presence.decoder_model_present {
            point.decoder_present = reader.bit("seq op decoder model")?;
            if point.decoder_present {
                let width = u32::from(header.decoder_model.buffer_delay_length_minus_1) + 1;
                point.decoder_buffer_delay = reader.bits("seq op buffer delay", width)?;
                point.encoder_buffer_delay = reader.bits("seq op encoder delay", width)?;
                point.low_delay = reader.bit("seq op low delay")?;
            }
        }
        if header.presence.initial_display_delay_present {
            point.initial_delay_present = reader.bit("seq op delay present")?;
            if point.initial_delay_present {
                point.initial_delay_minus_1 =
                    narrow_u8(reader.bits("seq op delay", 4)?, "seq op delay")?;
            }
        }
        header.operating_points.push(point);
    }
    Ok(())
}

/// Enable flags plus screen content tool selection into `header`.
fn parse_seq_enables(
    reader: &mut BitReader<'_>,
    header: &mut SequenceHeader,
) -> Result<(), ParseError> {
    if header.still.reduced {
        header.seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS;
        header.seq_force_integer_mv = SELECT_INTEGER_MV;
        return Ok(());
    }
    header.compound.enable_interintra_compound = reader.bit("seq interintra")?;
    header.compound.enable_masked_compound = reader.bit("seq masked")?;
    header.compound.enable_warped_motion = reader.bit("seq warped")?;
    header.filters.enable_dual_filter = reader.bit("seq dual filter")?;
    header.order.enable_order_hint = reader.bit("seq order hint")?;
    if header.order.enable_order_hint {
        header.order.enable_jnt_comp = reader.bit("seq jnt comp")?;
        header.order.enable_ref_frame_mvs = reader.bit("seq ref mvs")?;
    }
    header.seq_force_screen_content_tools = if reader.bit("seq choose tools")? {
        SELECT_SCREEN_CONTENT_TOOLS
    } else {
        u8::from(reader.bit("seq force tools")?)
    };
    header.seq_force_integer_mv =
        if header.seq_force_screen_content_tools != 0 && !reader.bit("seq choose integer mv")? {
            u8::from(reader.bit("seq force integer mv")?)
        } else {
            SELECT_INTEGER_MV
        };
    if header.order.enable_order_hint {
        header.order_hint_bits_minus_1 =
            narrow_u8(reader.bits("seq hint bits", 3)?, "seq hint bits")?;
    }
    Ok(())
}

/// Color config of the sequence header into `header`.
fn parse_color_config(
    reader: &mut BitReader<'_>,
    header: &mut SequenceHeader,
) -> Result<(), ParseError> {
    let color = &mut header.color;
    color.depth.high_bitdepth = reader.bit("seq high bitdepth")?;
    if header.profile == 2 && color.depth.high_bitdepth {
        color.depth.twelve_bit = reader.bit("seq twelve bit")?;
        header.bit_depth = if color.depth.twelve_bit { 12 } else { 10 };
    } else {
        header.bit_depth = if color.depth.high_bitdepth { 10 } else { 8 };
    }
    color.depth.mono_chrome = if header.profile == 1 {
        false
    } else {
        reader.bit("seq monochrome")?
    };
    header.num_planes = if color.depth.mono_chrome { 1 } else { 3 };
    color.color_description_present = reader.bit("seq color desc")?;
    if color.color_description_present {
        color.color_primaries = narrow_u8(reader.bits("seq primaries", 8)?, "seq primaries")?;
        color.transfer_characteristics =
            narrow_u8(reader.bits("seq transfer", 8)?, "seq transfer")?;
        color.matrix_coefficients = narrow_u8(reader.bits("seq matrix", 8)?, "seq matrix")?;
    } else {
        color.color_primaries = 2;
        color.transfer_characteristics = 2;
        color.matrix_coefficients = 2;
    }
    if color.depth.mono_chrome {
        color.color_range = reader.bit("seq color range")?;
        color.subsampling_x = 1;
        color.subsampling_y = 1;
        return Ok(());
    }
    if color.color_primaries == 1
        && color.transfer_characteristics == 13
        && color.matrix_coefficients == 0
    {
        color.color_range = true;
        color.subsampling_x = 0;
        color.subsampling_y = 0;
        color.separate_uv_delta_q = reader.bit("seq separate uv")?;
        return Ok(());
    }
    color.color_range = reader.bit("seq color range")?;
    if header.profile == 0 {
        color.subsampling_x = 1;
        color.subsampling_y = 1;
    } else if header.profile == 1 {
        color.subsampling_x = 0;
        color.subsampling_y = 0;
    } else if header.bit_depth == 12 {
        color.subsampling_x = u8::from(reader.bit("seq subsampling x")?);
        color.subsampling_y = if color.subsampling_x == 1 {
            u8::from(reader.bit("seq subsampling y")?)
        } else {
            0
        };
    } else {
        color.subsampling_x = 1;
        color.subsampling_y = 0;
    }
    if color.subsampling_x == 1 && color.subsampling_y == 1 {
        color.chroma_sample_position =
            narrow_u8(reader.bits("seq chroma pos", 2)?, "seq chroma pos")?;
    }
    color.separate_uv_delta_q = reader.bit("seq separate uv")?;
    Ok(())
}

/// Ceiling of `log2(target / blksize)`.
pub(crate) fn tile_log2(blksize: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blksize << k) < target {
        k += 1;
    }
    k
}

/// Signed order hint distance from `a` to `b`, zero without order hints.
pub(crate) fn relative_dist(seq: &SequenceHeader, a: u8, b: u8) -> i32 {
    if !seq.enable_order_hint() {
        return 0;
    }
    let diff = u32::from(a).wrapping_sub(u32::from(b));
    let m = 1 << seq.order_hint_bits_minus_1();
    (diff & (m - 1)).cast_signed() - (diff & m).cast_signed()
}

/// AV1 show-existing state of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameShow {
    /// Show a stored frame without decoding.
    pub show_existing: bool,
    /// Stored slot to show.
    pub map_idx: u8,
    /// Presentation time in ticks.
    pub presentation_time: u32,
    /// Display frame id.
    pub display_frame_id: u32,
}

/// AV1 frame sizes, parsed plus derived.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameSizes {
    /// Frame width minus one.
    pub width_minus_1: u32,
    /// Frame height minus one.
    pub height_minus_1: u32,
    /// Render width minus one.
    pub render_width_minus_1: u32,
    /// Render height minus one.
    pub render_height_minus_1: u32,
    /// Coded frame width.
    pub frame_width: u32,
    /// Coded frame height.
    pub frame_height: u32,
    /// Superres upscaled width.
    pub upscaled_width: u32,
    /// Render width.
    pub render_width: u32,
    /// Render height.
    pub render_height: u32,
}

/// AV1 reference selection of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameRefs {
    /// Short reference signaling.
    pub short_signaling: bool,
    /// Last frame slot under short signaling.
    pub last_idx: u8,
    /// Golden frame slot under short signaling.
    pub golden_idx: u8,
    /// Reference slot per name, `LAST` through `ALTREF`.
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    /// Frame id delta minus one per name.
    pub delta_frame_id_minus1: [u32; REFS_PER_FRAME],
    /// Size inherited from the named reference.
    pub found_ref: [bool; REFS_PER_FRAME],
}

/// AV1 tile layout of one frame header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TileLayout {
    /// Uniform tile spacing.
    pub uniform: bool,
    /// Tile column count.
    pub cols: u8,
    /// Tile row count.
    pub rows: u8,
    /// Context update tile id.
    pub context_update_tile_id: u16,
    /// Tile size field width minus one.
    pub size_bytes_minus_1: u8,
    /// Superblock column of each tile column.
    pub start_col_sb: [u8; MAX_TILE_COLS],
    /// Superblock row of each tile row.
    pub start_row_sb: [u8; MAX_TILE_ROWS],
    /// Superblock width minus one per tile column.
    pub width_in_sbs_minus_1: [u8; MAX_TILE_COLS],
    /// Superblock height minus one per tile row.
    pub height_in_sbs_minus_1: [u8; MAX_TILE_ROWS],
}

impl Default for TileLayout {
    fn default() -> Self {
        Self {
            uniform: false,
            cols: 1,
            rows: 1,
            context_update_tile_id: 0,
            size_bytes_minus_1: 0,
            start_col_sb: [0; MAX_TILE_COLS],
            start_row_sb: [0; MAX_TILE_ROWS],
            width_in_sbs_minus_1: [0; MAX_TILE_COLS],
            height_in_sbs_minus_1: [0; MAX_TILE_ROWS],
        }
    }
}

/// AV1 quantizer parameters of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct QuantParams {
    /// Base quantizer index.
    pub base_q_idx: u8,
    /// Luma DC delta.
    pub delta_q_y_dc: i8,
    /// Chroma U DC delta.
    pub delta_q_u_dc: i8,
    /// Chroma U AC delta.
    pub delta_q_u_ac: i8,
    /// Chroma V DC delta.
    pub delta_q_v_dc: i8,
    /// Chroma V AC delta.
    pub delta_q_v_ac: i8,
    /// Quantizer matrix in use.
    pub using_qmatrix: bool,
    /// Separate U and V deltas.
    pub diff_uv_delta: bool,
    /// Luma matrix id.
    pub qm_y: u8,
    /// Chroma U matrix id.
    pub qm_u: u8,
    /// Chroma V matrix id.
    pub qm_v: u8,
}

/// AV1 segmentation update flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SegUpdate {
    /// Segmentation map update.
    pub update_map: bool,
    /// Temporal segmentation update.
    pub temporal_update: bool,
    /// Segmentation data update.
    pub update_data: bool,
}

/// AV1 segmentation parameters of one frame header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SegParams {
    /// Segmentation enabled.
    pub enabled: bool,
    /// Segmentation update flags.
    pub update: SegUpdate,
    /// Feature enabled per segment and level.
    pub feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// Feature value per segment and level.
    pub feature_value: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
}

impl Default for SegParams {
    fn default() -> Self {
        Self {
            enabled: false,
            update: SegUpdate::default(),
            feature_enabled: [[false; SEG_LVL_MAX]; MAX_SEGMENTS],
            feature_value: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
        }
    }
}

/// AV1 loop filter parameters of one frame header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LoopFilterParams {
    /// Filter level per plane pair.
    pub level: [u8; 4],
    /// Filter sharpness.
    pub sharpness: u8,
    /// Filter deltas enabled.
    pub delta_enabled: bool,
    /// Filter deltas updated.
    pub delta_update: bool,
    /// Reference deltas.
    pub ref_deltas: [i8; TOTAL_REFS],
    /// Mode deltas.
    pub mode_deltas: [i8; 2],
}

impl Default for LoopFilterParams {
    fn default() -> Self {
        Self {
            level: [0; 4],
            sharpness: 0,
            delta_enabled: false,
            delta_update: false,
            ref_deltas: [1, 0, 0, 0, -1, 0, -1, -1],
            mode_deltas: [0; 2],
        }
    }
}

/// AV1 CDEF parameters of one frame header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CdefParams {
    /// Damping minus three.
    pub damping_minus_3: u8,
    /// CDEF bits.
    pub bits: u8,
    /// Luma primary strengths.
    pub y_pri: [u8; MAX_SEGMENTS],
    /// Luma secondary strengths.
    pub y_sec: [u8; MAX_SEGMENTS],
    /// Chroma primary strengths.
    pub uv_pri: [u8; MAX_SEGMENTS],
    /// Chroma secondary strengths.
    pub uv_sec: [u8; MAX_SEGMENTS],
}

impl Default for CdefParams {
    fn default() -> Self {
        Self {
            damping_minus_3: 0,
            bits: 0,
            y_pri: [0; MAX_SEGMENTS],
            y_sec: [0; MAX_SEGMENTS],
            uv_pri: [0; MAX_SEGMENTS],
            uv_sec: [0; MAX_SEGMENTS],
        }
    }
}

/// AV1 loop restoration parameters of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RestorationParams {
    /// Restoration type per plane.
    pub lr_type: [u8; 3],
    /// Restoration unit shift.
    pub unit_shift: u8,
    /// Chroma restoration shift.
    pub uv_shift: u8,
}

/// AV1 global motion syntax of one frame header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GlobalMotionSyntax {
    /// Global motion per reference name.
    pub is_global: [bool; TOTAL_REFS],
    /// Rotation and zoom per reference name.
    pub is_rot_zoom: [bool; TOTAL_REFS],
    /// Translation per reference name.
    pub is_translation: [bool; TOTAL_REFS],
    /// Raw sub-exponential parameters per name and coefficient.
    pub params: [[u32; 6]; TOTAL_REFS],
}

impl Default for GlobalMotionSyntax {
    fn default() -> Self {
        Self {
            is_global: [false; TOTAL_REFS],
            is_rot_zoom: [false; TOTAL_REFS],
            is_translation: [false; TOTAL_REFS],
            params: [[0; 6]; TOTAL_REFS],
        }
    }
}

/// AV1 film grain output clamping of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GrainClip {
    /// Overlap flag.
    pub overlap_flag: bool,
    /// Clip to restricted range.
    pub clip_to_restricted_range: bool,
}

/// AV1 film grain parameters of one frame header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FilmGrainParams {
    /// Grain applies to the output.
    pub apply_grain: bool,
    /// Grain parameters update.
    pub update_grain: bool,
    /// Grain seed.
    pub grain_seed: u16,
    /// Reference slot for inherited parameters.
    pub ref_idx: u8,
    /// Luma scaling points.
    pub num_y_points: u8,
    /// Luma point values.
    pub point_y_value: [u8; 14],
    /// Luma point scalings.
    pub point_y_scaling: [u8; 14],
    /// Chroma scales from luma.
    pub chroma_scaling_from_luma: bool,
    /// Chroma U scaling points.
    pub num_cb_points: u8,
    /// Chroma U point values.
    pub point_cb_value: [u8; 10],
    /// Chroma U point scalings.
    pub point_cb_scaling: [u8; 10],
    /// Chroma V scaling points.
    pub num_cr_points: u8,
    /// Chroma V point values.
    pub point_cr_value: [u8; 10],
    /// Chroma V point scalings.
    pub point_cr_scaling: [u8; 10],
    /// Grain scaling minus eight.
    pub grain_scaling_minus_8: u8,
    /// Autoregressive lag.
    pub ar_coeff_lag: u8,
    /// Luma autoregressive coefficients plus 128.
    pub ar_coeffs_y: [u8; 24],
    /// Chroma U autoregressive coefficients plus 128.
    pub ar_coeffs_cb: [u8; 25],
    /// Chroma V autoregressive coefficients plus 128.
    pub ar_coeffs_cr: [u8; 25],
    /// Coefficient shift minus six.
    pub ar_coeff_shift_minus_6: u8,
    /// Grain scale shift.
    pub grain_scale_shift: u8,
    /// Chroma U multiplier.
    pub cb_mult: u8,
    /// Chroma U luma multiplier.
    pub cb_luma_mult: u8,
    /// Chroma U offset.
    pub cb_offset: u16,
    /// Chroma V multiplier.
    pub cr_mult: u8,
    /// Chroma V luma multiplier.
    pub cr_luma_mult: u8,
    /// Chroma V offset.
    pub cr_offset: u16,
    /// Output clamping.
    pub clip: GrainClip,
}

impl Default for FilmGrainParams {
    fn default() -> Self {
        Self {
            apply_grain: false,
            update_grain: true,
            grain_seed: 0,
            ref_idx: 0,
            num_y_points: 0,
            point_y_value: [0; 14],
            point_y_scaling: [0; 14],
            chroma_scaling_from_luma: false,
            num_cb_points: 0,
            point_cb_value: [0; 10],
            point_cb_scaling: [0; 10],
            num_cr_points: 0,
            point_cr_value: [0; 10],
            point_cr_scaling: [0; 10],
            grain_scaling_minus_8: 0,
            ar_coeff_lag: 0,
            ar_coeffs_y: [0; 24],
            ar_coeffs_cb: [0; 25],
            ar_coeffs_cr: [0; 25],
            ar_coeff_shift_minus_6: 0,
            grain_scale_shift: 0,
            cb_mult: 0,
            cb_luma_mult: 0,
            cb_offset: 0,
            cr_mult: 0,
            cr_luma_mult: 0,
            cr_offset: 0,
            clip: GrainClip::default(),
        }
    }
}

/// Borrowed reference slot state behind frame header inference.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotView {
    /// Slot holds a decoded frame.
    pub valid: bool,
    /// Order hint of the stored frame.
    pub order_hint: u8,
    /// Frame id of the stored frame.
    pub frame_id: u32,
    /// Frame type of the stored frame.
    pub frame_type: u8,
    /// Upscaled width of the stored frame.
    pub upscaled_width: u32,
    /// Frame height of the stored frame.
    pub frame_height: u32,
    /// Render width of the stored frame.
    pub render_width: u32,
    /// Render height of the stored frame.
    pub render_height: u32,
    /// Segmentation features of the stored frame.
    pub segmentation: SegParams,
    /// Loop filter parameters of the stored frame.
    pub loop_filter: LoopFilterParams,
    /// Global motion parameters of the stored frame.
    pub gm_params: [[i32; 6]; TOTAL_REFS],
    /// Film grain parameters of the stored frame.
    pub grain: FilmGrainParams,
}

impl Default for SlotView {
    fn default() -> Self {
        Self {
            valid: false,
            order_hint: 0,
            frame_id: 0,
            frame_type: 0,
            upscaled_width: 0,
            frame_height: 0,
            render_width: 0,
            render_height: 0,
            segmentation: SegParams::default(),
            loop_filter: LoopFilterParams::default(),
            gm_params: [[0; 6]; TOTAL_REFS],
            grain: FilmGrainParams::default(),
        }
    }
}

/// AV1 display flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameDisplay {
    /// Frame displays on decode.
    pub show_frame: bool,
    /// Frame may display later.
    pub showable_frame: bool,
}

/// AV1 resilience flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameResilience {
    /// Error resilient mode.
    pub error_resilient_mode: bool,
    /// CDF update disabled.
    pub disable_cdf_update: bool,
    /// End-of-frame CDF update disabled.
    pub frame_end_update_disabled: bool,
}

/// AV1 content tool flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameContent {
    /// Screen content tools allowed.
    pub allow_screen_content_tools: bool,
    /// Integer motion vectors forced.
    pub force_integer_mv: bool,
    /// Intra block copy allowed.
    pub allow_intrabc: bool,
}

/// AV1 sizing flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameSizing {
    /// Frame size override.
    pub frame_size_override_flag: bool,
    /// Super-resolution in use.
    pub use_superres: bool,
    /// Render size differs from frame size.
    pub render_and_frame_size_different: bool,
}

/// AV1 motion flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameMotion {
    /// High precision motion vectors allowed.
    pub allow_high_precision_mv: bool,
    /// Switchable motion mode.
    pub is_motion_mode_switchable: bool,
    /// Reference frame motion vectors in use.
    pub use_ref_frame_mvs: bool,
}

/// AV1 prediction flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FramePrediction {
    /// Switchable interpolation filter.
    pub is_filter_switchable: bool,
    /// Warped motion allowed.
    pub allow_warped_motion: bool,
    /// Reference selection mode.
    pub reference_select: bool,
}

/// AV1 delta flags of one frame header.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameDelta {
    /// Delta quantizer present.
    pub delta_q_present: bool,
    /// Delta loop filter present.
    pub delta_lf_present: bool,
    /// Multiple delta loop filters.
    pub delta_lf_multi: bool,
}

/// Parsed AV1 frame header.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FrameHeader {
    /// Show-existing state.
    show: FrameShow,
    /// Frame type, 0 through 3.
    frame_type: u8,
    /// Display flags.
    display: FrameDisplay,
    /// Resilience flags.
    resilience: FrameResilience,
    /// Content tool flags.
    content: FrameContent,
    /// Current frame id.
    current_frame_id: u32,
    /// Sizing flags.
    sizing: FrameSizing,
    /// Order hint.
    order_hint: u8,
    /// Primary reference frame.
    primary_ref_frame: u8,
    /// Buffer removal time present.
    buffer_removal_time_present_flag: bool,
    /// Reference slots refreshed by the frame.
    refresh_frame_flags: u8,
    /// Frame sizes.
    sizes: FrameSizes,
    /// Superres denominator code.
    coded_denom: u8,
    /// Reference selection.
    refs: FrameRefs,
    /// Motion flags.
    motion: FrameMotion,
    /// Interpolation filter.
    interpolation_filter: u8,
    /// Prediction flags.
    prediction: FramePrediction,
    /// Reduced transform set.
    reduced_tx_set: bool,
    /// Transform mode.
    tx_mode: u8,
    /// Skip mode present.
    skip_mode_present: bool,
    /// Tile layout.
    tiles: TileLayout,
    /// Quantizer parameters.
    quant: QuantParams,
    /// Segmentation parameters.
    segmentation: SegParams,
    /// Delta flags.
    delta: FrameDelta,
    /// Delta quantizer resolution.
    delta_q_res: u8,
    /// Delta loop filter resolution.
    delta_lf_res: u8,
    /// Loop filter parameters.
    loop_filter: LoopFilterParams,
    /// CDEF parameters.
    cdef: CdefParams,
    /// Restoration parameters.
    restoration: RestorationParams,
    /// Global motion syntax.
    global: GlobalMotionSyntax,
    /// Film grain parameters.
    grain: FilmGrainParams,
    /// Derived global motion types per reference name.
    gm_type: [u8; TOTAL_REFS],
    /// Derived global motion parameters per name and coefficient.
    gm_params: [[i32; 6]; TOTAL_REFS],
    /// Skip mode reference names.
    skip_mode_frames: [u8; 2],
}

impl FrameHeader {
    /// Show-existing state.
    pub(crate) fn show(&self) -> &FrameShow {
        &self.show
    }

    /// Frame type, 0 through 3.
    pub(crate) fn frame_type(&self) -> u8 {
        self.frame_type
    }

    /// True for key and intra-only frames.
    pub(crate) fn is_intra(&self) -> bool {
        self.frame_type == FRAME_KEY || self.frame_type == FRAME_INTRA_ONLY
    }

    /// True for key frames.
    pub(crate) fn is_key(&self) -> bool {
        self.frame_type == FRAME_KEY
    }

    /// Frame displays on decode.
    pub(crate) fn show_frame(&self) -> bool {
        self.display.show_frame
    }

    /// Error resilient mode.
    pub(crate) fn error_resilient_mode(&self) -> bool {
        self.resilience.error_resilient_mode
    }

    /// CDF update disabled.
    pub(crate) fn disable_cdf_update(&self) -> bool {
        self.resilience.disable_cdf_update
    }

    /// End-of-frame CDF update disabled.
    pub(crate) fn disable_frame_end_update_cdf(&self) -> bool {
        self.resilience.disable_cdf_update || self.resilience.frame_end_update_disabled
    }

    /// Screen content tools allowed.
    pub(crate) fn allow_screen_content_tools(&self) -> bool {
        self.content.allow_screen_content_tools
    }

    /// Integer motion vectors forced.
    pub(crate) fn force_integer_mv(&self) -> bool {
        self.content.force_integer_mv
    }

    /// Current frame id.
    pub(crate) fn current_frame_id(&self) -> u32 {
        self.current_frame_id
    }

    /// Frame size override.
    pub(crate) fn frame_size_override_flag(&self) -> bool {
        self.sizing.frame_size_override_flag
    }

    /// Order hint.
    pub(crate) fn order_hint(&self) -> u8 {
        self.order_hint
    }

    /// Primary reference frame.
    pub(crate) fn primary_ref_frame(&self) -> u8 {
        self.primary_ref_frame
    }

    /// Buffer removal time present.
    pub(crate) fn buffer_removal_time_present_flag(&self) -> bool {
        self.buffer_removal_time_present_flag
    }

    /// Reference slots refreshed by the frame.
    pub(crate) fn refresh_frame_flags(&self) -> u8 {
        self.refresh_frame_flags
    }

    /// Frame sizes.
    pub(crate) fn sizes(&self) -> &FrameSizes {
        &self.sizes
    }

    /// Display size in luma samples.
    pub(crate) fn display_size(&self) -> (u32, u32) {
        (self.sizes.upscaled_width, self.sizes.frame_height)
    }

    /// Super-resolution in use.
    pub(crate) fn use_superres(&self) -> bool {
        self.sizing.use_superres
    }

    /// Superres denominator code.
    pub(crate) fn coded_denom(&self) -> u8 {
        self.coded_denom
    }

    /// Render size differs from frame size.
    pub(crate) fn render_and_frame_size_different(&self) -> bool {
        self.sizing.render_and_frame_size_different
    }

    /// Intra block copy allowed.
    pub(crate) fn allow_intrabc(&self) -> bool {
        self.content.allow_intrabc
    }

    /// Reference selection.
    pub(crate) fn refs(&self) -> &FrameRefs {
        &self.refs
    }

    /// High precision motion vectors allowed.
    pub(crate) fn allow_high_precision_mv(&self) -> bool {
        self.motion.allow_high_precision_mv
    }

    /// Switchable interpolation filter.
    pub(crate) fn is_filter_switchable(&self) -> bool {
        self.prediction.is_filter_switchable
    }

    /// Interpolation filter.
    pub(crate) fn interpolation_filter(&self) -> u8 {
        self.interpolation_filter
    }

    /// Switchable motion mode.
    pub(crate) fn is_motion_mode_switchable(&self) -> bool {
        self.motion.is_motion_mode_switchable
    }

    /// Reference frame motion vectors in use.
    pub(crate) fn use_ref_frame_mvs(&self) -> bool {
        self.motion.use_ref_frame_mvs
    }

    /// Warped motion allowed.
    pub(crate) fn allow_warped_motion(&self) -> bool {
        self.prediction.allow_warped_motion
    }

    /// Reduced transform set.
    pub(crate) fn reduced_tx_set(&self) -> bool {
        self.reduced_tx_set
    }

    /// Transform mode.
    pub(crate) fn tx_mode(&self) -> u8 {
        self.tx_mode
    }

    /// Reference selection mode.
    pub(crate) fn reference_select(&self) -> bool {
        self.prediction.reference_select
    }

    /// Skip mode present.
    pub(crate) fn skip_mode_present(&self) -> bool {
        self.skip_mode_present
    }

    /// Tile layout.
    pub(crate) fn tiles(&self) -> &TileLayout {
        &self.tiles
    }

    /// Quantizer parameters.
    pub(crate) fn quant(&self) -> &QuantParams {
        &self.quant
    }

    /// Segmentation parameters.
    pub(crate) fn segmentation(&self) -> &SegParams {
        &self.segmentation
    }

    /// Delta quantizer present.
    pub(crate) fn delta_q_present(&self) -> bool {
        self.delta.delta_q_present
    }

    /// Delta quantizer resolution.
    pub(crate) fn delta_q_res(&self) -> u8 {
        self.delta_q_res
    }

    /// Delta loop filter present.
    pub(crate) fn delta_lf_present(&self) -> bool {
        self.delta.delta_lf_present
    }

    /// Delta loop filter resolution.
    pub(crate) fn delta_lf_res(&self) -> u8 {
        self.delta_lf_res
    }

    /// Multiple delta loop filters.
    pub(crate) fn delta_lf_multi(&self) -> bool {
        self.delta.delta_lf_multi
    }

    /// Loop filter parameters.
    pub(crate) fn loop_filter(&self) -> &LoopFilterParams {
        &self.loop_filter
    }

    /// CDEF parameters.
    pub(crate) fn cdef(&self) -> &CdefParams {
        &self.cdef
    }

    /// Restoration parameters.
    pub(crate) fn restoration(&self) -> &RestorationParams {
        &self.restoration
    }

    /// Film grain parameters.
    pub(crate) fn grain(&self) -> &FilmGrainParams {
        &self.grain
    }

    /// Derived global motion types per reference name.
    pub(crate) fn gm_type(&self) -> &[u8; TOTAL_REFS] {
        &self.gm_type
    }

    /// Derived global motion parameters per name and coefficient.
    pub(crate) fn gm_params(&self) -> &[[i32; 6]; TOTAL_REFS] {
        &self.gm_params
    }

    /// Skip mode reference names.
    pub(crate) fn skip_mode_frames(&self) -> &[u8; 2] {
        &self.skip_mode_frames
    }

    /// Frame header with frame ids for fill tests.
    #[cfg(test)]
    pub(crate) fn test_with_ids(current: u32, deltas: [u32; REFS_PER_FRAME]) -> Self {
        Self {
            current_frame_id: current,
            refs: FrameRefs {
                delta_frame_id_minus1: deltas,
                ..FrameRefs::default()
            },
            ..Self::default()
        }
    }
}

/// Frame header plus the slot invalidations its parse applied.
pub(crate) struct ParsedFrame {
    /// Parsed frame header.
    pub header: FrameHeader,
    /// Keyframe display cleared every slot.
    pub reset_all: bool,
    /// Slots invalidated during the parse.
    pub invalidated: [bool; TOTAL_REFS],
}

/// Frame id width in bits behind `seq`.
fn id_len(seq: &SequenceHeader) -> u32 {
    u32::from(seq.additional_frame_id_length_minus_1())
        + u32::from(seq.delta_frame_id_length_minus_2())
        + 3
}

/// Frame header of one OBU payload against `seq` and `slots`.
///
/// Reads reference state for inference and validation; slot effects land
/// in the returned invalidations for the caller to apply.
pub(crate) fn parse_frame_header(
    payload: &[u8],
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
) -> Result<ParsedFrame, ParseError> {
    let mut reader = BitReader::new(payload);
    parse_frame_header_with(&mut reader, seq, slots)
}

/// Frame type plus show flags into `header`.
fn parse_show_frame(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    if seq.reduced() {
        header.frame_type = FRAME_KEY;
        header.display.show_frame = true;
        return Ok(());
    }
    header.show.show_existing = reader.bit("frame show existing")?;
    if header.show.show_existing {
        header.show.map_idx = narrow_u8(reader.bits("frame show map", 3)?, "frame show map")?;
        let view = &views[usize::from(header.show.map_idx)];
        if !view.valid {
            return Err(ParseError::MissingSet {
                id: u32::from(header.show.map_idx),
            });
        }
        if seq.decoder_model_present()
            && seq.timing_present()
            && !seq.timing().equal_picture_interval
        {
            let width = u32::from(seq.decoder_model().frame_presentation_time_length_minus_1) + 1;
            header.show.presentation_time = reader.bits("frame presentation time", width)?;
        }
        if seq.frame_id_numbers_present() {
            header.show.display_frame_id = reader.bits("frame display id", id_len(seq))?;
        }
        header.frame_type = view.frame_type;
        header.refresh_frame_flags = if view.frame_type == FRAME_KEY {
            0xff
        } else {
            0
        };
        return Ok(());
    }
    header.frame_type = narrow_u8(reader.bits("frame type", 2)?, "frame type")?;
    if header.frame_type > FRAME_SWITCH {
        return Err(ParseError::OutOfRange {
            syntax: "frame type",
        });
    }
    header.display.show_frame = reader.bit("frame show")?;
    if header.display.show_frame
        && seq.decoder_model_present()
        && seq.timing_present()
        && !seq.timing().equal_picture_interval
    {
        let width = u32::from(seq.decoder_model().frame_presentation_time_length_minus_1) + 1;
        header.show.presentation_time = reader.bits("frame presentation time", width)?;
    }
    if header.display.show_frame {
        header.display.showable_frame = header.frame_type != FRAME_KEY;
    } else {
        header.display.showable_frame = reader.bit("frame showable")?;
    }
    if header.frame_type == FRAME_SWITCH
        || (header.frame_type == FRAME_KEY && header.display.show_frame)
    {
        header.resilience.error_resilient_mode = true;
    } else {
        header.resilience.error_resilient_mode = reader.bit("frame resilient")?;
    }
    Ok(())
}

/// Show-existing frame: sizes from the shown slot, no further syntax.
fn show_existing_frame(
    seq: &SequenceHeader,
    views: &[SlotView; TOTAL_REFS],
    mut header: FrameHeader,
) -> ParsedFrame {
    let view = &views[usize::from(header.show.map_idx)];
    header.order_hint = if seq.enable_order_hint() {
        view.order_hint
    } else {
        0
    };
    header.sizes.width_minus_1 = view.upscaled_width.saturating_sub(1);
    header.sizes.height_minus_1 = view.frame_height.saturating_sub(1);
    header.sizes.render_width_minus_1 = view.render_width.saturating_sub(1);
    header.sizes.render_height_minus_1 = view.render_height.saturating_sub(1);
    header.sizes.upscaled_width = view.upscaled_width;
    header.sizes.frame_width = view.upscaled_width;
    header.sizes.frame_height = view.frame_height;
    header.sizes.render_width = view.render_width;
    header.sizes.render_height = view.render_height;
    ParsedFrame {
        header,
        reset_all: false,
        invalidated: [false; TOTAL_REFS],
    }
}

/// Screen content tools plus integer motion vectors into `header`.
fn parse_tools(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    header.resilience.disable_cdf_update = reader.bit("frame cdf update")?;
    if seq.seq_force_screen_content_tools() == SELECT_SCREEN_CONTENT_TOOLS {
        header.content.allow_screen_content_tools = reader.bit("frame tools")?;
    } else {
        header.content.allow_screen_content_tools = seq.seq_force_screen_content_tools() != 0;
    }
    if header.content.allow_screen_content_tools {
        if seq.seq_force_integer_mv() == SELECT_INTEGER_MV {
            header.content.force_integer_mv = reader.bit("frame integer mv")?;
        } else {
            header.content.force_integer_mv = seq.seq_force_integer_mv() != 0;
        }
    }
    Ok(())
}

/// Current frame id plus range invalidation of stale slots.
fn parse_frame_id(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &mut [SlotView; TOTAL_REFS],
    invalidated: &mut [bool; TOTAL_REFS],
) -> Result<(), ParseError> {
    if !seq.frame_id_numbers_present() {
        return Ok(());
    }
    header.current_frame_id = reader.bits("frame id", id_len(seq))?;
    let diff_len = u32::from(seq.delta_frame_id_length_minus_2()) + 2;
    for (index, view) in views.iter_mut().enumerate() {
        if !view.valid {
            continue;
        }
        let stale = if header.current_frame_id > (1 << diff_len) {
            view.frame_id > header.current_frame_id
                || view.frame_id < header.current_frame_id - (1 << diff_len)
        } else {
            view.frame_id > header.current_frame_id
                && view.frame_id < (1 << id_len(seq)) + header.current_frame_id - (1 << diff_len)
        };
        if stale {
            view.valid = false;
            invalidated[index] = true;
        }
    }
    Ok(())
}

/// Frame size override plus order hint and primary reference into `header`.
fn parse_order_hint(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    if header.frame_type == FRAME_SWITCH {
        header.sizing.frame_size_override_flag = true;
    } else if seq.reduced() {
        header.sizing.frame_size_override_flag = false;
    } else {
        header.sizing.frame_size_override_flag = reader.bit("frame size override")?;
    }
    let bits = seq.order_hint_bits();
    if bits > 0 {
        header.order_hint = narrow_u8(reader.bits("frame order hint", bits)?, "frame order hint")?;
    }
    if header.is_intra() || header.resilience.error_resilient_mode {
        header.primary_ref_frame = PRIMARY_REF_NONE;
    } else {
        header.primary_ref_frame =
            narrow_u8(reader.bits("frame primary ref", 3)?, "frame primary ref")?;
    }
    Ok(())
}

/// Decoder model buffer removal times, consumed and dropped.
fn parse_buffer_removal(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
) -> Result<(), ParseError> {
    if !seq.decoder_model_present() {
        return Ok(());
    }
    if !reader.bit("frame removal present")? {
        return Ok(());
    }
    let width = u32::from(seq.decoder_model().buffer_removal_time_length_minus_1) + 1;
    for point in seq.operating_points() {
        if point.decoder_present {
            reader.bits("frame removal time", width)?;
        }
    }
    Ok(())
}

/// Refresh flags into `header`.
fn parse_refresh(reader: &mut BitReader<'_>, header: &mut FrameHeader) -> Result<(), ParseError> {
    if header.frame_type == FRAME_SWITCH
        || (header.frame_type == FRAME_KEY && header.display.show_frame)
    {
        header.refresh_frame_flags = 0xff;
    } else {
        header.refresh_frame_flags = narrow_u8(reader.bits("frame refresh", 8)?, "frame refresh")?;
    }
    Ok(())
}

/// Reference order hints with mismatch invalidation.
fn parse_ref_order_hints(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &mut [SlotView; TOTAL_REFS],
    invalidated: &mut [bool; TOTAL_REFS],
) -> Result<(), ParseError> {
    if header.is_intra() && header.refresh_frame_flags == 0xff {
        return Ok(());
    }
    if !seq.enable_order_hint() {
        return Ok(());
    }
    let bits = seq.order_hint_bits();
    for (index, view) in views.iter_mut().enumerate() {
        let hint = if header.resilience.error_resilient_mode {
            narrow_u8(reader.bits("frame ref hint", bits)?, "frame ref hint")?
        } else {
            view.order_hint
        };
        if hint != view.order_hint {
            view.valid = false;
            invalidated[index] = true;
        }
    }
    Ok(())
}

/// Frame size plus superres into `header`.
fn parse_frame_size(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    if header.sizing.frame_size_override_flag {
        header.sizes.width_minus_1 =
            reader.bits("frame width", u32::from(seq.frame_width_bits_minus_1()) + 1)?;
        header.sizes.height_minus_1 = reader.bits(
            "frame height",
            u32::from(seq.frame_height_bits_minus_1()) + 1,
        )?;
    } else {
        header.sizes.width_minus_1 = u32::from(seq.max_frame_width_minus_1());
        header.sizes.height_minus_1 = u32::from(seq.max_frame_height_minus_1());
    }
    header.sizes.frame_width = header.sizes.width_minus_1 + 1;
    header.sizes.frame_height = header.sizes.height_minus_1 + 1;
    parse_superres(reader, seq, header)
}

/// Superres denominator plus upscaled width into `header`.
fn parse_superres(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    header.sizing.use_superres = seq.enable_superres() && reader.bit("frame superres")?;
    let denom = if header.sizing.use_superres {
        header.coded_denom = narrow_u8(reader.bits("frame denom", 3)?, "frame denom")?;
        u32::from(header.coded_denom) + SUPERRES_DENOM_MIN
    } else {
        SUPERRES_NUM
    };
    header.sizes.upscaled_width = header.sizes.frame_width;
    header.sizes.frame_width = (header.sizes.upscaled_width * SUPERRES_NUM + denom / 2) / denom;
    Ok(())
}

/// Render size into `header`.
fn parse_render_size(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    header.sizing.render_and_frame_size_different = reader.bit("frame render differs")?;
    if header.sizing.render_and_frame_size_different {
        header.sizes.render_width_minus_1 = reader.bits("frame render width", 16)?;
        header.sizes.render_height_minus_1 = reader.bits("frame render height", 16)?;
    } else {
        header.sizes.render_width_minus_1 = header.sizes.width_minus_1;
        header.sizes.render_height_minus_1 = header.sizes.height_minus_1;
    }
    header.sizes.render_width = header.sizes.render_width_minus_1 + 1;
    header.sizes.render_height = header.sizes.render_height_minus_1 + 1;
    Ok(())
}

/// Inter reference selection into `header`, validating cited slots.
fn parse_inter_refs(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    if seq.enable_order_hint() {
        header.refs.short_signaling = reader.bit("frame short refs")?;
        if header.refs.short_signaling {
            header.refs.last_idx = narrow_u8(reader.bits("frame last idx", 3)?, "frame last idx")?;
            header.refs.golden_idx =
                narrow_u8(reader.bits("frame golden idx", 3)?, "frame golden idx")?;
            set_frame_refs(seq, header, views);
        }
    } else {
        header.refs.short_signaling = false;
    }
    for index in 0..REFS_PER_FRAME {
        if !header.refs.short_signaling {
            header.refs.ref_frame_idx[index] =
                narrow_u8(reader.bits("frame ref idx", 3)?, "frame ref idx")?;
        }
        let slot = header.refs.ref_frame_idx[index];
        if usize::from(slot) >= TOTAL_REFS || !views[usize::from(slot)].valid {
            return Err(ParseError::MissingSet {
                id: u32::from(slot),
            });
        }
        if seq.frame_id_numbers_present() {
            header.refs.delta_frame_id_minus1[index] = reader.bits(
                "frame delta id",
                u32::from(seq.delta_frame_id_length_minus_2()) + 2,
            )?;
        }
    }
    Ok(())
}

/// Short-signaled reference slots from order hint distances.
fn set_frame_refs(seq: &SequenceHeader, header: &mut FrameHeader, views: &[SlotView; TOTAL_REFS]) {
    const ORDER: [usize; 5] = [2, 3, 5, 6, 7];
    let last = usize::from(header.refs.last_idx);
    let golden = usize::from(header.refs.golden_idx);
    let mut refs: [Option<u8>; REFS_PER_FRAME] = [None; REFS_PER_FRAME];
    refs[0] = Some(header.refs.last_idx);
    refs[3] = Some(header.refs.golden_idx);
    let mut used = [false; TOTAL_REFS];
    used[last] = true;
    used[golden] = true;
    let current = 1_i32 << seq.order_hint_bits_minus_1();
    let mut shifted = [0_i32; TOTAL_REFS];
    for (index, view) in views.iter().enumerate() {
        shifted[index] = current + relative_dist(seq, view.order_hint, header.order_hint);
    }
    let mut latest = shifted[last];
    let mut earliest = shifted[golden];
    latest = pick_future(&shifted, &mut used, current, latest, true, &mut refs[6]);
    earliest = pick_future(&shifted, &mut used, current, earliest, false, &mut refs[4]);
    earliest = pick_future(&shifted, &mut used, current, earliest, false, &mut refs[5]);
    for name in ORDER {
        if refs[name - 1].is_some() {
            continue;
        }
        let mut pick = None;
        for (slot, taken) in used.iter().enumerate() {
            if *taken || shifted[slot] >= current {
                continue;
            }
            if pick.is_none() || shifted[slot] >= latest {
                pick = u8::try_from(slot).ok();
                latest = shifted[slot];
            }
        }
        if let Some(slot) = pick {
            refs[name - 1] = Some(slot);
            used[usize::from(slot)] = true;
        }
    }
    let mut fallback = 0_u8;
    for (slot, hint) in shifted.iter().enumerate() {
        if slot == 0 || *hint < earliest {
            fallback = u8::try_from(slot).unwrap_or(0);
            earliest = *hint;
        }
    }
    for (index, slot) in refs.iter().enumerate() {
        header.refs.ref_frame_idx[index] = slot.unwrap_or(fallback);
    }
}

/// Latest or earliest future slot into `out`, returning the moved bound.
fn pick_future(
    shifted: &[i32; TOTAL_REFS],
    used: &mut [bool; TOTAL_REFS],
    current: i32,
    bound: i32,
    latest: bool,
    out: &mut Option<u8>,
) -> i32 {
    let mut pick = None;
    let mut edge = bound;
    for (slot, taken) in used.iter().enumerate() {
        if *taken || shifted[slot] < current {
            continue;
        }
        let better = pick.is_none()
            || (!latest && shifted[slot] < edge)
            || (latest && shifted[slot] >= edge);
        if better {
            pick = u8::try_from(slot).ok();
            edge = shifted[slot];
        }
    }
    if let Some(slot) = pick {
        *out = Some(slot);
        used[usize::from(slot)] = true;
    }
    edge
}

/// Frame size inherited from a reference into `header`.
fn parse_frame_size_with_refs(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    for index in 0..REFS_PER_FRAME {
        header.refs.found_ref[index] = reader.bit("frame found ref")?;
        if !header.refs.found_ref[index] {
            continue;
        }
        let view = &views[usize::from(header.refs.ref_frame_idx[index])];
        if !view.valid {
            return Err(ParseError::MissingSet {
                id: u32::from(header.refs.ref_frame_idx[index]),
            });
        }
        header.sizes.width_minus_1 = view.upscaled_width.saturating_sub(1);
        header.sizes.height_minus_1 = view.frame_height.saturating_sub(1);
        header.sizes.render_width_minus_1 = view.render_width.saturating_sub(1);
        header.sizes.render_height_minus_1 = view.render_height.saturating_sub(1);
        header.sizes.upscaled_width = view.upscaled_width;
        header.sizes.frame_width = view.upscaled_width;
        header.sizes.frame_height = view.frame_height;
        header.sizes.render_width = view.render_width;
        header.sizes.render_height = view.render_height;
        return parse_superres(reader, seq, header);
    }
    parse_frame_size(reader, seq, header)?;
    parse_render_size(reader, header)
}

/// Inter prediction tools into `header`.
fn parse_inter_tools(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    if header.content.force_integer_mv {
        header.motion.allow_high_precision_mv = false;
    } else {
        header.motion.allow_high_precision_mv = reader.bit("frame high precision mv")?;
    }
    header.prediction.is_filter_switchable = reader.bit("frame filter switchable")?;
    if header.prediction.is_filter_switchable {
        header.interpolation_filter = 4;
    } else {
        header.interpolation_filter = narrow_u8(reader.bits("frame filter", 2)?, "frame filter")?;
    }
    header.motion.is_motion_mode_switchable = reader.bit("frame motion switchable")?;
    if header.resilience.error_resilient_mode || !seq.enable_ref_frame_mvs() {
        header.motion.use_ref_frame_mvs = false;
    } else {
        header.motion.use_ref_frame_mvs = reader.bit("frame ref mvs")?;
    }
    Ok(())
}

/// Tiles, quantizer, segmentation, and delta parameters; reports lossless.
fn parse_tiles_quant_seg(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<bool, ParseError> {
    parse_tile_info(reader, seq, header)?;
    parse_quant(reader, seq, header)?;
    parse_segmentation(reader, header, views)?;
    if header.quant.base_q_idx > 0 {
        header.delta.delta_q_present = reader.bit("frame delta q")?;
    }
    if header.delta.delta_q_present {
        header.delta_q_res = narrow_u8(reader.bits("frame delta q res", 2)?, "frame delta q res")?;
    }
    if header.delta.delta_q_present {
        if header.content.allow_intrabc {
            header.delta.delta_lf_present = false;
        } else {
            header.delta.delta_lf_present = reader.bit("frame delta lf")?;
        }
        if header.delta.delta_lf_present {
            header.delta_lf_res =
                narrow_u8(reader.bits("frame delta lf res", 2)?, "frame delta lf res")?;
            header.delta.delta_lf_multi = reader.bit("frame delta lf multi")?;
        }
    }
    Ok(coded_lossless(header))
}

/// True when every segment quantizer and delta is zero.
fn coded_lossless(header: &FrameHeader) -> bool {
    if header.quant.delta_q_y_dc != 0
        || header.quant.delta_q_u_dc != 0
        || header.quant.delta_q_u_ac != 0
        || header.quant.delta_q_v_dc != 0
        || header.quant.delta_q_v_ac != 0
    {
        return false;
    }
    for segment in 0..MAX_SEGMENTS {
        let mut qindex = i32::from(header.quant.base_q_idx);
        if header.segmentation.feature_enabled[segment][0] {
            qindex += i32::from(header.segmentation.feature_value[segment][0]);
        }
        if qindex.clamp(0, 255) != 0 {
            return false;
        }
    }
    true
}

/// Tile layout into `header`.
fn parse_tile_info(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    let mi_cols = 2 * header.sizes.frame_width.div_ceil(8);
    let mi_rows = 2 * header.sizes.frame_height.div_ceil(8);
    let (sb_cols, sb_rows, sb_size) = if seq.use_128x128_superblock() {
        (mi_cols.div_ceil(32), mi_rows.div_ceil(32), 7)
    } else {
        (mi_cols.div_ceil(16), mi_rows.div_ceil(16), 6)
    };
    let max_tile_width_sb = MAX_TILE_WIDTH / (1 << sb_size);
    let max_tile_area_sb = MAX_TILE_AREA / (1 << (2 * sb_size));
    let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_cols = narrow_u32(MAX_TILE_COLS, "frame tile col")?;
    let max_rows = narrow_u32(MAX_TILE_ROWS, "frame tile row")?;
    let max_log2_tile_cols = tile_log2(1, sb_cols.min(max_cols));
    let max_log2_tile_rows = tile_log2(1, sb_rows.min(max_rows));
    let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
    header.tiles.uniform = reader.bit("frame uniform tiles")?;
    let bounds = TileBounds {
        sb_cols,
        sb_rows,
        min_cols: min_log2_tile_cols,
        max_cols: max_log2_tile_cols,
        max_rows: max_log2_tile_rows,
        min_tiles: min_log2_tiles,
        max_tile_width_sb,
    };
    let (cols_log2, rows_log2) = if header.tiles.uniform {
        parse_uniform_tiles(reader, header, &bounds)?
    } else {
        parse_explicit_tiles(reader, header, &bounds)?
    };
    if cols_log2 > 0 || rows_log2 > 0 {
        header.tiles.context_update_tile_id = narrow_u16(
            reader.bits("frame context tile", cols_log2 + rows_log2)?,
            "frame context tile",
        )?;
        header.tiles.size_bytes_minus_1 = narrow_u8(
            reader.bits("frame tile size bytes", 2)?,
            "frame tile size bytes",
        )?;
    }
    Ok(())
}

/// Superblock grid plus tile count bounds behind tile parsing.
struct TileBounds {
    /// Superblock columns of the frame.
    sb_cols: u32,
    /// Superblock rows of the frame.
    sb_rows: u32,
    /// Minimum log2 tile columns.
    min_cols: u32,
    /// Maximum log2 tile columns.
    max_cols: u32,
    /// Maximum log2 tile rows.
    max_rows: u32,
    /// Minimum log2 tile count.
    min_tiles: u32,
    /// Maximum tile width in superblocks.
    max_tile_width_sb: u32,
}

/// Uniform tile columns and rows into `header`, returning the log counts.
fn parse_uniform_tiles(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
    bounds: &TileBounds,
) -> Result<(u32, u32), ParseError> {
    let cols_log2 = reader.increment("frame tile cols", bounds.min_cols, bounds.max_cols)?;
    let tile_width_sb = bounds.sb_cols.div_ceil(1 << cols_log2);
    let mut cols = 0;
    let mut at = 0;
    while at < bounds.sb_cols {
        if cols >= MAX_TILE_COLS {
            return Err(ParseError::OutOfRange {
                syntax: "frame tile col",
            });
        }
        header.tiles.start_col_sb[cols] = narrow_u8(at, "frame tile col")?;
        cols += 1;
        at += tile_width_sb;
    }
    let cols_count = narrow_u32(cols, "frame tile col")?;
    header.tiles.cols = narrow_u8(cols_count, "frame tile col")?;
    let min_rows = bounds.min_tiles.saturating_sub(cols_log2);
    let rows_log2 = reader.increment("frame tile rows", min_rows, bounds.max_rows)?;
    let tile_height_sb = bounds.sb_rows.div_ceil(1 << rows_log2);
    let mut rows = 0;
    at = 0;
    while at < bounds.sb_rows {
        if rows >= MAX_TILE_ROWS {
            return Err(ParseError::OutOfRange {
                syntax: "frame tile row",
            });
        }
        header.tiles.start_row_sb[rows] = narrow_u8(at, "frame tile row")?;
        rows += 1;
        at += tile_height_sb;
    }
    let rows_count = narrow_u32(rows, "frame tile row")?;
    header.tiles.rows = narrow_u8(rows_count, "frame tile row")?;
    for width in header.tiles.width_in_sbs_minus_1.iter_mut().take(cols) {
        *width = narrow_u8(tile_width_sb - 1, "frame tile width")?;
    }
    header.tiles.width_in_sbs_minus_1[cols - 1] = narrow_u8(
        bounds.sb_cols - (cols_count - 1) * tile_width_sb - 1,
        "frame tile width",
    )?;
    for height in header.tiles.height_in_sbs_minus_1.iter_mut().take(rows) {
        *height = narrow_u8(tile_height_sb - 1, "frame tile height")?;
    }
    header.tiles.height_in_sbs_minus_1[rows - 1] = narrow_u8(
        bounds.sb_rows - (rows_count - 1) * tile_height_sb - 1,
        "frame tile height",
    )?;
    Ok((cols_log2, rows_log2))
}

/// Explicit tile columns and rows into `header`, returning the log counts.
fn parse_explicit_tiles(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
    bounds: &TileBounds,
) -> Result<(u32, u32), ParseError> {
    let mut widest = 0;
    let mut cols = 0;
    let mut at = 0;
    while at < bounds.sb_cols && cols < MAX_TILE_COLS {
        header.tiles.start_col_sb[cols] = narrow_u8(at, "frame tile col")?;
        let max_width = (bounds.sb_cols - at).min(bounds.max_tile_width_sb);
        let width = reader.ns("frame tile width", max_width)? + 1;
        header.tiles.width_in_sbs_minus_1[cols] = narrow_u8(width - 1, "frame tile width")?;
        widest = widest.max(width);
        at += width;
        cols += 1;
    }
    if at < bounds.sb_cols {
        return Err(ParseError::OutOfRange {
            syntax: "frame tile col",
        });
    }
    let cols_count = narrow_u32(cols, "frame tile col")?;
    header.tiles.cols = narrow_u8(cols_count, "frame tile col")?;
    let area = if bounds.min_tiles > 0 {
        (bounds.sb_rows * bounds.sb_cols) / (1 << (bounds.min_tiles + 1))
    } else {
        bounds.sb_rows * bounds.sb_cols
    };
    let max_tile_height_sb = (area / widest).max(1);
    let mut rows = 0;
    at = 0;
    while at < bounds.sb_rows && rows < MAX_TILE_ROWS {
        header.tiles.start_row_sb[rows] = narrow_u8(at, "frame tile row")?;
        let max_height = (bounds.sb_rows - at).min(max_tile_height_sb);
        let height = reader.ns("frame tile height", max_height)? + 1;
        header.tiles.height_in_sbs_minus_1[rows] = narrow_u8(height - 1, "frame tile height")?;
        at += height;
        rows += 1;
    }
    if at < bounds.sb_rows {
        return Err(ParseError::OutOfRange {
            syntax: "frame tile row",
        });
    }
    let rows_count = narrow_u32(rows, "frame tile row")?;
    header.tiles.rows = narrow_u8(rows_count, "frame tile row")?;
    Ok((tile_log2(1, cols_count), tile_log2(1, rows_count)))
}

/// Quantizer parameters into `header`.
fn parse_quant(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    header.quant.base_q_idx = narrow_u8(reader.bits("frame base q", 8)?, "frame base q")?;
    header.quant.delta_q_y_dc = parse_delta_q(reader, "frame delta y")?;
    if seq.num_planes() > 1 {
        if seq.color().separate_uv_delta_q {
            header.quant.diff_uv_delta = reader.bit("frame diff uv")?;
        }
        header.quant.delta_q_u_dc = parse_delta_q(reader, "frame delta u dc")?;
        header.quant.delta_q_u_ac = parse_delta_q(reader, "frame delta u ac")?;
        if header.quant.diff_uv_delta {
            header.quant.delta_q_v_dc = parse_delta_q(reader, "frame delta v dc")?;
            header.quant.delta_q_v_ac = parse_delta_q(reader, "frame delta v ac")?;
        } else {
            header.quant.delta_q_v_dc = header.quant.delta_q_u_dc;
            header.quant.delta_q_v_ac = header.quant.delta_q_u_ac;
        }
    }
    header.quant.using_qmatrix = reader.bit("frame qmatrix")?;
    if header.quant.using_qmatrix {
        header.quant.qm_y = narrow_u8(reader.bits("frame qm y", 4)?, "frame qm y")?;
        header.quant.qm_u = narrow_u8(reader.bits("frame qm u", 4)?, "frame qm u")?;
        if seq.color().separate_uv_delta_q {
            header.quant.qm_v = narrow_u8(reader.bits("frame qm v", 4)?, "frame qm v")?;
        } else {
            header.quant.qm_v = header.quant.qm_u;
        }
    }
    Ok(())
}

/// One delta quantizer: coded flag plus a 7-bit signed value.
fn parse_delta_q(reader: &mut BitReader<'_>, syntax: &'static str) -> Result<i8, ParseError> {
    if !reader.bit(syntax)? {
        return Ok(0);
    }
    narrow_i8(reader.su(syntax, 7)?, syntax)
}

/// Segmentation parameters into `header`.
fn parse_segmentation(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    const BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
    const SIGNS: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
    header.segmentation.enabled = reader.bit("frame seg")?;
    if !header.segmentation.enabled {
        return Ok(());
    }
    if header.primary_ref_frame == PRIMARY_REF_NONE {
        header.segmentation.update.update_map = true;
        header.segmentation.update.update_data = true;
    } else {
        header.segmentation.update.update_map = reader.bit("frame seg map")?;
        if header.segmentation.update.update_map {
            header.segmentation.update.temporal_update = reader.bit("frame seg temporal")?;
        }
        header.segmentation.update.update_data = reader.bit("frame seg data")?;
    }
    let inherit = (header.primary_ref_frame != PRIMARY_REF_NONE).then(|| {
        let slot = header.refs.ref_frame_idx[usize::from(header.primary_ref_frame)];
        &views[usize::from(slot)].segmentation
    });
    for segment in 0..MAX_SEGMENTS {
        for level in 0..SEG_LVL_MAX {
            if header.segmentation.update.update_data {
                let enabled = reader.bit("frame seg feature")?;
                header.segmentation.feature_enabled[segment][level] = enabled;
                if enabled && BITS[level] > 0 {
                    let value = if SIGNS[level] {
                        reader.su("frame seg value", 1 + BITS[level])?
                    } else {
                        reader.bits("frame seg value", BITS[level])?.cast_signed()
                    };
                    header.segmentation.feature_value[segment][level] = i16::try_from(value)
                        .map_err(|_| ParseError::OutOfRange {
                            syntax: "frame seg value",
                        })?;
                } else {
                    header.segmentation.feature_value[segment][level] = 0;
                }
            } else if let Some(prior) = inherit {
                header.segmentation.feature_enabled[segment][level] =
                    prior.feature_enabled[segment][level];
                header.segmentation.feature_value[segment][level] =
                    prior.feature_value[segment][level];
            }
        }
    }
    Ok(())
}

/// Loop filter parameters into `header`.
fn parse_loop_filter(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
    lossless: bool,
) -> Result<(), ParseError> {
    const DEFAULT_REF: [i8; TOTAL_REFS] = [1, 0, 0, 0, -1, 0, -1, -1];
    if lossless || header.content.allow_intrabc {
        header.loop_filter.level = [0; 4];
        header.loop_filter.ref_deltas = DEFAULT_REF;
        header.loop_filter.mode_deltas = [0; 2];
        return Ok(());
    }
    header.loop_filter.level[0] = narrow_u8(reader.bits("frame lf level", 6)?, "frame lf level")?;
    header.loop_filter.level[1] = narrow_u8(reader.bits("frame lf level", 6)?, "frame lf level")?;
    if seq.num_planes() > 1
        && (header.loop_filter.level[0] != 0 || header.loop_filter.level[1] != 0)
    {
        header.loop_filter.level[2] =
            narrow_u8(reader.bits("frame lf level", 6)?, "frame lf level")?;
        header.loop_filter.level[3] =
            narrow_u8(reader.bits("frame lf level", 6)?, "frame lf level")?;
    }
    header.loop_filter.sharpness =
        narrow_u8(reader.bits("frame lf sharpness", 3)?, "frame lf sharpness")?;
    header.loop_filter.delta_enabled = reader.bit("frame lf delta")?;
    if !header.loop_filter.delta_enabled {
        header.loop_filter.ref_deltas = DEFAULT_REF;
        return Ok(());
    }
    let prior = if header.primary_ref_frame == PRIMARY_REF_NONE {
        None
    } else {
        let slot = header.refs.ref_frame_idx[usize::from(header.primary_ref_frame)];
        Some(&views[usize::from(slot)].loop_filter)
    };
    header.loop_filter.delta_update = reader.bit("frame lf update")?;
    let delta_update = header.loop_filter.delta_update;
    for (index, delta) in header.loop_filter.ref_deltas.iter_mut().enumerate() {
        let update = delta_update && reader.bit("frame lf ref update")?;
        if update {
            *delta = narrow_i8(reader.su("frame lf ref delta", 7)?, "frame lf ref delta")?;
        } else if let Some(prior) = prior {
            *delta = prior.ref_deltas[index];
        } else {
            *delta = DEFAULT_REF[index];
        }
    }
    for index in 0..2 {
        let update = delta_update && reader.bit("frame lf mode update")?;
        if update {
            header.loop_filter.mode_deltas[index] =
                narrow_i8(reader.su("frame lf mode delta", 7)?, "frame lf mode delta")?;
        } else if let Some(prior) = prior {
            header.loop_filter.mode_deltas[index] = prior.mode_deltas[index];
        }
    }
    Ok(())
}

/// CDEF parameters into `header`.
fn parse_cdef(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    lossless: bool,
) -> Result<(), ParseError> {
    if lossless || header.content.allow_intrabc || !seq.enable_cdef() {
        return Ok(());
    }
    header.cdef.damping_minus_3 =
        narrow_u8(reader.bits("frame cdef damping", 2)?, "frame cdef damping")?;
    header.cdef.bits = narrow_u8(reader.bits("frame cdef bits", 2)?, "frame cdef bits")?;
    for index in 0..(1 << header.cdef.bits) {
        header.cdef.y_pri[index] =
            narrow_u8(reader.bits("frame cdef y pri", 4)?, "frame cdef y pri")?;
        header.cdef.y_sec[index] =
            narrow_u8(reader.bits("frame cdef y sec", 2)?, "frame cdef y sec")?;
        if seq.num_planes() > 1 {
            header.cdef.uv_pri[index] =
                narrow_u8(reader.bits("frame cdef uv pri", 4)?, "frame cdef uv pri")?;
            header.cdef.uv_sec[index] =
                narrow_u8(reader.bits("frame cdef uv sec", 2)?, "frame cdef uv sec")?;
        }
    }
    Ok(())
}

/// Loop restoration parameters into `header`.
fn parse_restoration(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    lossless: bool,
) -> Result<(), ParseError> {
    let all_lossless = lossless && header.sizes.frame_width == header.sizes.upscaled_width;
    if all_lossless || header.content.allow_intrabc || !seq.enable_restoration() {
        return Ok(());
    }
    let mut uses = false;
    let mut uses_chroma = false;
    for (index, lr) in header
        .restoration
        .lr_type
        .iter_mut()
        .enumerate()
        .take(usize::from(seq.num_planes()))
    {
        *lr = narrow_u8(reader.bits("frame lr type", 2)?, "frame lr type")?;
        if *lr != 0 {
            uses = true;
            if index > 0 {
                uses_chroma = true;
            }
        }
    }
    if !uses {
        return Ok(());
    }
    if seq.use_128x128_superblock() {
        header.restoration.unit_shift =
            narrow_u8(reader.increment("frame lr shift", 1, 2)?, "frame lr shift")?;
    } else {
        header.restoration.unit_shift =
            narrow_u8(reader.increment("frame lr shift", 0, 2)?, "frame lr shift")?;
    }
    if seq.color().subsampling_x == 1 && seq.color().subsampling_y == 1 && uses_chroma {
        header.restoration.uv_shift = u8::from(reader.bit("frame lr uv shift")?);
    }
    Ok(())
}

/// Transform mode into `header`.
fn parse_tx_mode(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
    lossless: bool,
) -> Result<(), ParseError> {
    if lossless {
        header.tx_mode = 0;
    } else {
        header.tx_mode = narrow_u8(reader.increment("frame tx mode", 1, 2)?, "frame tx mode")?;
    }
    Ok(())
}

/// Skip mode presence into `header`.
fn parse_skip_mode(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    if skip_mode_allowed(seq, header, views) {
        header.skip_mode_present = reader.bit("frame skip present")?;
    }
    Ok(())
}

/// True when the frame may signal skip mode.
fn skip_mode_allowed(
    seq: &SequenceHeader,
    header: &FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> bool {
    if header.is_intra() || !header.prediction.reference_select || !seq.enable_order_hint() {
        return false;
    }
    let (forward, backward) = skip_mode_refs(seq, header, views);
    let Some(first) = forward else {
        return false;
    };
    if backward.is_some() {
        return true;
    }
    let forward_hint = views[usize::from(header.refs.ref_frame_idx[first])].order_hint;
    for index in 0..REFS_PER_FRAME {
        let hint = views[usize::from(header.refs.ref_frame_idx[index])].order_hint;
        if relative_dist(seq, hint, forward_hint) < 0 {
            return true;
        }
    }
    false
}

/// Forward and backward skip mode reference indices.
fn skip_mode_refs(
    seq: &SequenceHeader,
    header: &FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> (Option<usize>, Option<usize>) {
    let mut forward = None;
    let mut backward = None;
    let mut forward_hint = 0;
    let mut backward_hint = 0;
    for index in 0..REFS_PER_FRAME {
        let hint = views[usize::from(header.refs.ref_frame_idx[index])].order_hint;
        let dist = relative_dist(seq, hint, header.order_hint);
        if dist < 0 && (forward.is_none() || relative_dist(seq, hint, forward_hint) > 0) {
            forward = Some(index);
            forward_hint = hint;
        } else if dist > 0 && (backward.is_none() || relative_dist(seq, hint, backward_hint) < 0) {
            backward = Some(index);
            backward_hint = hint;
        }
    }
    (forward, backward)
}

/// Global motion syntax into `header`.
fn parse_global_motion_syntax(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    if header.is_intra() {
        return Ok(());
    }
    for name in 1..TOTAL_REFS {
        header.global.is_global[name] = reader.bit("frame global")?;
        if !header.global.is_global[name] {
            continue;
        }
        header.global.is_rot_zoom[name] = reader.bit("frame rotzoom")?;
        let model = if header.global.is_rot_zoom[name] {
            2
        } else {
            header.global.is_translation[name] = reader.bit("frame translation")?;
            if header.global.is_translation[name] {
                1
            } else {
                3
            }
        };
        if model >= 2 {
            parse_gm_param(reader, header, model, name, 2)?;
            parse_gm_param(reader, header, model, name, 3)?;
            if model == 3 {
                parse_gm_param(reader, header, model, name, 4)?;
                parse_gm_param(reader, header, model, name, 5)?;
            }
        }
        if model >= 1 {
            parse_gm_param(reader, header, model, name, 0)?;
            parse_gm_param(reader, header, model, name, 1)?;
        }
    }
    Ok(())
}

/// One raw global motion parameter into `header`.
fn parse_gm_param(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
    model: u8,
    name: usize,
    index: usize,
) -> Result<(), ParseError> {
    let abs_bits = if index < 2 {
        if model == 1 && !header.motion.allow_high_precision_mv {
            GM_ABS_TRANS_ONLY_BITS - 1
        } else if model == 1 {
            GM_ABS_TRANS_ONLY_BITS
        } else {
            GM_ABS_TRANS_BITS
        }
    } else {
        GM_ABS_ALPHA_BITS
    };
    let symbols = 2 * (1 << abs_bits) + 1;
    header.global.params[name][index] = reader.subexp("frame gm param", symbols)?;
    Ok(())
}

/// Film grain parameters into `header`.
fn parse_film_grain(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    if !seq.film_grain_params_present()
        || (!header.display.show_frame && !header.display.showable_frame)
    {
        return Ok(());
    }
    header.grain.apply_grain = reader.bit("frame grain apply")?;
    if !header.grain.apply_grain {
        return Ok(());
    }
    header.grain.grain_seed = narrow_u16(reader.bits("frame grain seed", 16)?, "frame grain seed")?;
    if header.frame_type == FRAME_INTER {
        header.grain.update_grain = reader.bit("frame grain update")?;
    } else {
        header.grain.update_grain = true;
    }
    if !header.grain.update_grain {
        header.grain.ref_idx = narrow_u8(reader.bits("frame grain ref", 3)?, "frame grain ref")?;
        let stored = views[usize::from(header.grain.ref_idx)].grain;
        let seed = header.grain.grain_seed;
        header.grain = stored;
        header.grain.grain_seed = seed;
        header.grain.apply_grain = true;
        header.grain.update_grain = false;
        return Ok(());
    }
    parse_grain_points(reader, seq, header)?;
    parse_grain_coeffs(reader, header)?;
    header.grain.clip.overlap_flag = reader.bit("frame grain overlap")?;
    header.grain.clip.clip_to_restricted_range = reader.bit("frame grain clip")?;
    Ok(())
}

/// Film grain scaling points into `header`.
fn parse_grain_points(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    header.grain.num_y_points = narrow_u8(
        reader.bits("frame grain y points", 4)?,
        "frame grain y points",
    )?;
    if header.grain.num_y_points > 14 {
        return Err(ParseError::OutOfRange {
            syntax: "frame grain y points",
        });
    }
    for index in 0..usize::from(header.grain.num_y_points) {
        header.grain.point_y_value[index] = narrow_u8(
            reader.bits("frame grain y value", 8)?,
            "frame grain y value",
        )?;
        header.grain.point_y_scaling[index] = narrow_u8(
            reader.bits("frame grain y scaling", 8)?,
            "frame grain y scaling",
        )?;
        if index > 0 && header.grain.point_y_value[index] <= header.grain.point_y_value[index - 1] {
            return Err(ParseError::OutOfRange {
                syntax: "frame grain y value",
            });
        }
    }
    if seq.color().depth.mono_chrome {
        header.grain.chroma_scaling_from_luma = false;
    } else {
        header.grain.chroma_scaling_from_luma = reader.bit("frame grain chroma luma")?;
    }
    if seq.color().depth.mono_chrome
        || header.grain.chroma_scaling_from_luma
        || (seq.color().subsampling_x == 1
            && seq.color().subsampling_y == 1
            && header.grain.num_y_points == 0)
    {
        return Ok(());
    }
    header.grain.num_cb_points = narrow_u8(
        reader.bits("frame grain cb points", 4)?,
        "frame grain cb points",
    )?;
    if header.grain.num_cb_points > 10 {
        return Err(ParseError::OutOfRange {
            syntax: "frame grain cb points",
        });
    }
    for index in 0..usize::from(header.grain.num_cb_points) {
        header.grain.point_cb_value[index] = narrow_u8(
            reader.bits("frame grain cb value", 8)?,
            "frame grain cb value",
        )?;
        header.grain.point_cb_scaling[index] = narrow_u8(
            reader.bits("frame grain cb scaling", 8)?,
            "frame grain cb scaling",
        )?;
        if index > 0 && header.grain.point_cb_value[index] <= header.grain.point_cb_value[index - 1]
        {
            return Err(ParseError::OutOfRange {
                syntax: "frame grain cb value",
            });
        }
    }
    header.grain.num_cr_points = narrow_u8(
        reader.bits("frame grain cr points", 4)?,
        "frame grain cr points",
    )?;
    if header.grain.num_cr_points > 10 {
        return Err(ParseError::OutOfRange {
            syntax: "frame grain cr points",
        });
    }
    for index in 0..usize::from(header.grain.num_cr_points) {
        header.grain.point_cr_value[index] = narrow_u8(
            reader.bits("frame grain cr value", 8)?,
            "frame grain cr value",
        )?;
        header.grain.point_cr_scaling[index] = narrow_u8(
            reader.bits("frame grain cr scaling", 8)?,
            "frame grain cr scaling",
        )?;
        if index > 0 && header.grain.point_cr_value[index] <= header.grain.point_cr_value[index - 1]
        {
            return Err(ParseError::OutOfRange {
                syntax: "frame grain cr value",
            });
        }
    }
    Ok(())
}

/// Film grain autoregressive coefficients into `header`.
fn parse_grain_coeffs(
    reader: &mut BitReader<'_>,
    header: &mut FrameHeader,
) -> Result<(), ParseError> {
    header.grain.grain_scaling_minus_8 = narrow_u8(
        reader.bits("frame grain scaling", 2)?,
        "frame grain scaling",
    )?;
    header.grain.ar_coeff_lag = narrow_u8(reader.bits("frame grain lag", 2)?, "frame grain lag")?;
    let lag = u32::from(header.grain.ar_coeff_lag);
    let luma = usize::try_from(2 * lag * (lag + 1)).map_err(|_| ParseError::OutOfRange {
        syntax: "frame grain lag",
    })?;
    let chroma = if header.grain.num_y_points > 0 {
        for coeff in header.grain.ar_coeffs_y.iter_mut().take(luma) {
            *coeff = narrow_u8(
                reader.bits("frame grain coeff y", 8)?,
                "frame grain coeff y",
            )?;
        }
        luma + 1
    } else {
        luma
    };
    if header.grain.chroma_scaling_from_luma || header.grain.num_cb_points > 0 {
        for coeff in header.grain.ar_coeffs_cb.iter_mut().take(chroma) {
            *coeff = narrow_u8(
                reader.bits("frame grain coeff cb", 8)?,
                "frame grain coeff cb",
            )?;
        }
    }
    if header.grain.chroma_scaling_from_luma || header.grain.num_cr_points > 0 {
        for coeff in header.grain.ar_coeffs_cr.iter_mut().take(chroma) {
            *coeff = narrow_u8(
                reader.bits("frame grain coeff cr", 8)?,
                "frame grain coeff cr",
            )?;
        }
    }
    header.grain.ar_coeff_shift_minus_6 =
        narrow_u8(reader.bits("frame grain shift", 2)?, "frame grain shift")?;
    header.grain.grain_scale_shift =
        narrow_u8(reader.bits("frame grain scale", 2)?, "frame grain scale")?;
    if header.grain.num_cb_points > 0 {
        header.grain.cb_mult = narrow_u8(
            reader.bits("frame grain cb mult", 8)?,
            "frame grain cb mult",
        )?;
        header.grain.cb_luma_mult = narrow_u8(
            reader.bits("frame grain cb luma", 8)?,
            "frame grain cb luma",
        )?;
        header.grain.cb_offset = narrow_u16(
            reader.bits("frame grain cb offset", 9)?,
            "frame grain cb offset",
        )?;
    }
    if header.grain.num_cr_points > 0 {
        header.grain.cr_mult = narrow_u8(
            reader.bits("frame grain cr mult", 8)?,
            "frame grain cr mult",
        )?;
        header.grain.cr_luma_mult = narrow_u8(
            reader.bits("frame grain cr luma", 8)?,
            "frame grain cr luma",
        )?;
        header.grain.cr_offset = narrow_u16(
            reader.bits("frame grain cr offset", 9)?,
            "frame grain cr offset",
        )?;
    }
    Ok(())
}

/// Derived per-frame state: global motion parameters plus skip mode names.
fn derive_frame_state(
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    derive_global_motion(header, views)?;
    if header.is_intra() || !header.prediction.reference_select || !seq.enable_order_hint() {
        return Ok(());
    }
    let (forward, backward) = skip_mode_refs(seq, header, views);
    let Some(first) = forward else {
        return Ok(());
    };
    if let Some(second) = backward {
        header.skip_mode_frames = [
            1 + u8::try_from(first.min(second)).map_err(|_| ParseError::OutOfRange {
                syntax: "frame skip mode",
            })?,
            1 + u8::try_from(first.max(second)).map_err(|_| ParseError::OutOfRange {
                syntax: "frame skip mode",
            })?,
        ];
        return Ok(());
    }
    let forward_hint = views[usize::from(header.refs.ref_frame_idx[first])].order_hint;
    let mut second = None;
    let mut second_hint = 0;
    for index in 0..REFS_PER_FRAME {
        let hint = views[usize::from(header.refs.ref_frame_idx[index])].order_hint;
        if relative_dist(seq, hint, forward_hint) >= 0 {
            continue;
        }
        if second.is_none() || relative_dist(seq, hint, second_hint) > 0 {
            second = Some(index);
            second_hint = hint;
        }
    }
    if let Some(other) = second {
        header.skip_mode_frames = [
            1 + u8::try_from(first.min(other)).map_err(|_| ParseError::OutOfRange {
                syntax: "frame skip mode",
            })?,
            1 + u8::try_from(first.max(other)).map_err(|_| ParseError::OutOfRange {
                syntax: "frame skip mode",
            })?,
        ];
    }
    Ok(())
}

/// Derived global motion types and parameters into `header`.
fn derive_global_motion(
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
    for name in 1..TOTAL_REFS {
        header.gm_type[name] = 0;
        for (index, param) in header.gm_params[name].iter_mut().enumerate() {
            *param = if index % 3 == 2 {
                1 << WARPEDMODEL_PREC_BITS
            } else {
                0
            };
        }
    }
    if header.is_intra() {
        return Ok(());
    }
    for name in 1..TOTAL_REFS {
        let model = if !header.global.is_global[name] {
            0
        } else if header.global.is_rot_zoom[name] {
            2
        } else if header.global.is_translation[name] {
            1
        } else {
            3
        };
        header.gm_type[name] = model;
        if model >= 2 {
            derive_gm_param(header, views, name, 2)?;
            derive_gm_param(header, views, name, 3)?;
            if model == 3 {
                derive_gm_param(header, views, name, 4)?;
                derive_gm_param(header, views, name, 5)?;
            } else {
                header.gm_params[name][4] = -header.gm_params[name][3];
                header.gm_params[name][5] = header.gm_params[name][2];
            }
        }
        if model >= 1 {
            derive_gm_param(header, views, name, 0)?;
            derive_gm_param(header, views, name, 1)?;
        }
    }
    Ok(())
}

/// One derived global motion parameter into `header`.
fn derive_gm_param(
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
    name: usize,
    index: usize,
) -> Result<(), ParseError> {
    let model = header.gm_type[name];
    let precise = header.motion.allow_high_precision_mv;
    let (abs_bits, prec_bits) = if index < 2 {
        if model == 1 && !precise {
            (GM_ABS_TRANS_ONLY_BITS - 1, TRANS_ONLY_PREC_BITS - 1)
        } else if model == 1 {
            (GM_ABS_TRANS_ONLY_BITS, TRANS_ONLY_PREC_BITS)
        } else {
            (GM_ABS_TRANS_BITS, TRANS_PREC_BITS)
        }
    } else {
        (GM_ABS_ALPHA_BITS, ALPHA_PREC_BITS)
    };
    let prior = if header.primary_ref_frame == PRIMARY_REF_NONE {
        header.gm_params[name][index]
    } else {
        let slot = header.refs.ref_frame_idx[usize::from(header.primary_ref_frame)];
        views[usize::from(slot)].gm_params[name][index]
    };
    let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
    let sub = if index % 3 == 2 { 1 << prec_bits } else { 0 };
    let round = if index % 3 == 2 {
        1 << WARPEDMODEL_PREC_BITS
    } else {
        0
    };
    let mx = 1_i64 << abs_bits;
    let reference = i64::from(prior) / (1_i64 << prec_diff) - sub;
    let decoded = decode_signed_subexp(header.global.params[name][index], -mx, mx + 1, reference);
    header.gm_params[name][index] =
        i32::try_from(decoded * (1_i64 << prec_diff) + round).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "frame gm param",
            }
        })?;
    Ok(())
}

/// Signed sub-exponential value around `reference` within `low` and `high`.
fn decode_signed_subexp(sub_exp: u32, low: i64, high: i64, reference: i64) -> i64 {
    let range = high - low;
    let centered = reference - low;
    let value = if centered * 2 <= range {
        inverse_recenter(centered, i64::from(sub_exp))
    } else {
        range - 1 - inverse_recenter(range - 1 - centered, i64::from(sub_exp))
    };
    value + low
}

/// Recentering of `value` around `anchor`.
fn inverse_recenter(anchor: i64, value: i64) -> i64 {
    if value > 2 * anchor {
        value
    } else if value & 1 == 1 {
        anchor - ((value + 1) >> 1)
    } else {
        anchor + (value >> 1)
    }
}

/// Tile group tile ranges over one OBU payload.
pub(crate) struct ParsedTileGroup<'a> {
    /// First tile index of the group.
    pub start: u32,
    /// Last tile index of the group.
    pub end: u32,
    /// Tile data past the group header.
    pub data: &'a [u8],
    /// Byte offset and size of each tile within `data`.
    pub tiles: Vec<(usize, usize)>,
}

/// Tile group of one OBU payload expecting tiles from `expect_start`.
pub(crate) fn parse_tile_group_obu<'a>(
    payload: &'a [u8],
    tiles: &TileLayout,
    expect_start: u32,
) -> Result<ParsedTileGroup<'a>, ParseError> {
    let mut reader = BitReader::new(payload);
    let count = u32::from(tiles.cols) * u32::from(tiles.rows);
    let mut start = 0;
    let mut end = count.saturating_sub(1);
    if count > 1 && reader.bit("tile group present")? {
        let width = tile_log2(1, u32::from(tiles.cols)) + tile_log2(1, u32::from(tiles.rows));
        start = reader.bits("tile group start", width)?;
        end = reader.bits("tile group end", width)?;
        if start < expect_start || start >= count || end < start || end >= count {
            return Err(ParseError::OutOfRange {
                syntax: "tile group range",
            });
        }
    } else if expect_start > 0 {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    align_zero(&mut reader)?;
    let data = &payload[reader.consumed() / 8..];
    let tiles = split_tiles(data, start, end, tiles.size_bytes_minus_1)?;
    Ok(ParsedTileGroup {
        start,
        end,
        data,
        tiles,
    })
}

/// Consume bit padding, rejecting nonzero bits.
fn align_zero(reader: &mut BitReader<'_>) -> Result<(), ParseError> {
    while !reader.consumed().is_multiple_of(8) {
        if reader.bit("tile group align")? {
            return Err(ParseError::OutOfRange {
                syntax: "tile group align",
            });
        }
    }
    Ok(())
}

/// Tile ranges of `data` for tiles `start` through `end`.
fn split_tiles(
    data: &[u8],
    start: u32,
    end: u32,
    size_bytes_minus_1: u8,
) -> Result<Vec<(usize, usize)>, ParseError> {
    let width = usize::from(size_bytes_minus_1) + 1;
    let mut tiles = Vec::new();
    let mut at = 0;
    for tile in start..=end {
        if tile == end {
            tiles.push((at, data.len().saturating_sub(at)));
            return Ok(tiles);
        }
        if data.len() < at + width {
            return Err(ParseError::Truncated {
                syntax: "tile size",
            });
        }
        let mut size = 0_usize;
        for index in 0..width {
            size |= usize::from(data[at + index]) << (8 * index);
        }
        size += 1;
        at += width;
        if data.len() < at + size {
            return Err(ParseError::Truncated {
                syntax: "tile data",
            });
        }
        tiles.push((at, size));
        at += size;
    }
    Ok(tiles)
}

/// Frame OBU: header plus its tile group over one OBU payload.
pub(crate) fn parse_frame_obu<'a>(
    payload: &'a [u8],
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
    expect_start: u32,
) -> Result<(ParsedFrame, Option<ParsedTileGroup<'a>>), ParseError> {
    let mut reader = BitReader::new(payload);
    let parsed = parse_frame_header_with(&mut reader, seq, slots)?;
    if parsed.header.show.show_existing {
        return Ok((parsed, None));
    }
    align_zero(&mut reader)?;
    let rest = &payload[reader.consumed() / 8..];
    let group = parse_tile_group_obu(rest, parsed.header.tiles(), expect_start)?;
    Ok((parsed, Some(group)))
}

/// Frame header over a shared reader, leaving the tile group unread.
fn parse_frame_header_with(
    reader: &mut BitReader<'_>,
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
) -> Result<ParsedFrame, ParseError> {
    let mut header = FrameHeader::default();
    let mut views = *slots;
    let mut invalidated = [false; TOTAL_REFS];
    parse_show_frame(reader, seq, &mut header, &views)?;
    if header.show.show_existing {
        return Ok(show_existing_frame(seq, &views, header));
    }
    if header.frame_type == FRAME_KEY && header.display.show_frame {
        for view in &mut views {
            *view = SlotView::default();
        }
    }
    let reset_all = header.frame_type == FRAME_KEY && header.display.show_frame;
    parse_tools(reader, seq, &mut header)?;
    parse_frame_id(reader, seq, &mut header, &mut views, &mut invalidated)?;
    parse_order_hint(reader, seq, &mut header)?;
    parse_buffer_removal(reader, seq)?;
    parse_refresh(reader, &mut header)?;
    parse_ref_order_hints(reader, seq, &mut header, &mut views, &mut invalidated)?;
    if header.is_intra() {
        parse_frame_size(reader, seq, &mut header)?;
        parse_render_size(reader, &mut header)?;
        header.content.allow_intrabc = header.content.allow_screen_content_tools
            && header.sizes.upscaled_width == header.sizes.frame_width
            && reader.bit("frame intrabc")?;
    } else {
        parse_inter_refs(reader, seq, &mut header, &views)?;
        if header.sizing.frame_size_override_flag && !header.resilience.error_resilient_mode {
            parse_frame_size_with_refs(reader, seq, &mut header, &views)?;
        } else {
            parse_frame_size(reader, seq, &mut header)?;
            parse_render_size(reader, &mut header)?;
        }
        parse_inter_tools(reader, seq, &mut header)?;
    }
    if seq.reduced() || header.resilience.disable_cdf_update {
        header.resilience.frame_end_update_disabled = true;
    } else {
        header.resilience.frame_end_update_disabled = reader.bit("frame end cdf")?;
    }
    let lossless = parse_tiles_quant_seg(reader, seq, &mut header, &views)?;
    parse_loop_filter(reader, seq, &mut header, &views, lossless)?;
    parse_cdef(reader, seq, &mut header, lossless)?;
    parse_restoration(reader, seq, &mut header, lossless)?;
    parse_tx_mode(reader, &mut header, lossless)?;
    if header.is_intra() {
        header.prediction.reference_select = false;
    } else {
        header.prediction.reference_select = reader.bit("frame ref select")?;
    }
    parse_skip_mode(reader, seq, &mut header, &views)?;
    if header.is_intra() || header.resilience.error_resilient_mode || !seq.enable_warped_motion() {
        header.prediction.allow_warped_motion = false;
    } else {
        header.prediction.allow_warped_motion = reader.bit("frame warped")?;
    }
    header.reduced_tx_set = reader.bit("frame reduced tx")?;
    parse_global_motion_syntax(reader, &mut header)?;
    parse_film_grain(reader, seq, &mut header, &views)?;
    derive_frame_state(seq, &mut header, &views)?;
    Ok(ParsedFrame {
        header,
        reset_all,
        invalidated,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        inverse_recenter, relative_dist, split_obus, split_tiles, temporal_units, tile_log2,
        uleb128, SequenceHeader,
    };

    /// Sequence header with 3-bit order hints for distance tests.
    fn seq_with_hints() -> SequenceHeader {
        SequenceHeader {
            profile: 0,
            still: super::SeqStill::default(),
            presence: super::SeqPresence::default(),
            timing: super::TimingInfo::default(),
            decoder_model: super::DecoderModelInfo::default(),
            operating_points: Vec::new(),
            frame_width_bits_minus_1: 0,
            frame_height_bits_minus_1: 0,
            max_frame_width_minus_1: 0,
            max_frame_height_minus_1: 0,
            frame_id_numbers_present: false,
            delta_frame_id_length_minus_2: 0,
            additional_frame_id_length_minus_1: 0,
            blocks: super::SeqBlocks::default(),
            compound: super::SeqCompound::default(),
            filters: super::SeqFilters::default(),
            order: super::SeqOrder {
                enable_order_hint: true,
                ..super::SeqOrder::default()
            },
            seq_force_screen_content_tools: 0,
            seq_force_integer_mv: 0,
            order_hint_bits_minus_1: 2,
            enable_superres: false,
            color: super::ColorConfig::default(),
            film_grain_params_present: false,
            bit_depth: 8,
            num_planes: 3,
        }
    }

    #[test]
    fn uleb128_decodes_multibyte() {
        assert_eq!(uleb128(&[0x05], "t").expect("one"), (5, 1));
        assert_eq!(uleb128(&[0x81, 0x01], "t").expect("two"), (129, 2));
        assert!(uleb128(&[0x80], "t").is_err());
    }

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

    #[test]
    fn tile_log2_counts_doublings() {
        assert_eq!(tile_log2(1, 1), 0);
        assert_eq!(tile_log2(1, 2), 1);
        assert_eq!(tile_log2(32, 32), 0);
        assert_eq!(tile_log2(32, 33), 1);
        assert_eq!(tile_log2(32, 64), 1);
        assert_eq!(tile_log2(32, 65), 2);
    }

    #[test]
    fn relative_dist_wraps_modulo_hints() {
        let seq = seq_with_hints();
        assert_eq!(relative_dist(&seq, 1, 0), 1);
        assert_eq!(relative_dist(&seq, 0, 1), -1);
        assert_eq!(relative_dist(&seq, 7, 0), -1);
        assert_eq!(relative_dist(&seq, 0, 7), 1);
    }

    #[test]
    fn split_tiles_strips_size_prefixes() {
        let single = split_tiles(b"abcdef", 0, 0, 0).expect("single");
        assert_eq!(single, vec![(0, 6)]);
        let data = [0x02, 0x01, 0x02, 0x03, 0x04, 0x05];
        let pair = split_tiles(&data, 0, 1, 0).expect("pair");
        assert_eq!(pair, vec![(1, 3), (4, 2)]);
        assert!(split_tiles(&[0x09, 0x01], 0, 1, 0).is_err());
    }

    #[test]
    fn inverse_recenter_round_trips_small_deltas() {
        assert_eq!(inverse_recenter(8, 0), 8);
        assert_eq!(inverse_recenter(8, 1), 7);
        assert_eq!(inverse_recenter(8, 2), 9);
        assert_eq!(inverse_recenter(8, 17), 17);
    }

    #[test]
    fn signed_subexp_decodes_around_reference() {
        assert_eq!(super::decode_signed_subexp(0, -8, 9, 0), 0);
        assert_eq!(super::decode_signed_subexp(1, -8, 9, 0), -1);
        assert_eq!(super::decode_signed_subexp(2, -8, 9, 0), 1);
    }

    #[test]
    fn libaom_sequence_header_pins_geometry() {
        let payload = crate::parse::test_support::hex("000000043cffbcfaf98040");
        let seq = super::parse_sequence_header(&payload).expect("seq");
        assert_eq!(seq.profile(), 0);
        assert_eq!(seq.level_idx(), 0);
        assert_eq!(seq.coded_size(), (320, 240));
        assert!(!seq.mono_chrome());
        assert_eq!(seq.subsampling_x(), 1);
        assert_eq!(seq.subsampling_y(), 1);
        assert_eq!(seq.bit_depth(), 8);
        assert!(seq.enable_order_hint());
        assert!(!seq.frame_id_numbers_present());
        assert!(!seq.use_128x128_superblock());
    }

    #[test]
    fn tile_group_ranges_cover_their_tiles() {
        let tiles = super::TileLayout {
            cols: 2,
            rows: 2,
            ..super::TileLayout::default()
        };
        let payload = [0xb0, 0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let group = super::parse_tile_group_obu(&payload, &tiles, 1).expect("group");
        assert_eq!((group.start, group.end), (1, 2));
        assert_eq!(group.tiles, vec![(1, 3), (4, 2)]);
        assert_eq!(&group.data[1..4], &[0xaa, 0xbb, 0xcc]);
        assert!(super::parse_tile_group_obu(&payload, &tiles, 2).is_err());
        let single = super::TileLayout::default();
        let solo = super::parse_tile_group_obu(&[0x11, 0x22], &single, 0).expect("solo");
        assert_eq!((solo.start, solo.end), (0, 0));
        assert_eq!(solo.tiles, vec![(0, 2)]);
    }

    #[test]
    fn set_frame_refs_assigns_by_hint_distance() {
        let seq = seq_with_hints();
        let mut header = super::FrameHeader {
            order_hint: 4,
            refs: super::FrameRefs {
                last_idx: 1,
                golden_idx: 2,
                ..super::FrameRefs::default()
            },
            ..super::FrameHeader::default()
        };
        let mut views = [super::SlotView::default(); super::TOTAL_REFS];
        for (index, view) in views.iter_mut().enumerate() {
            view.valid = true;
            view.order_hint = u8::try_from(index).expect("slot");
        }
        super::set_frame_refs(&seq, &mut header, &views);
        assert_eq!(header.refs.ref_frame_idx, [1, 3, 0, 2, 4, 5, 7]);
    }

    #[test]
    fn skip_mode_refs_span_current_hint() {
        let seq = seq_with_hints();
        let header = super::FrameHeader {
            order_hint: 4,
            refs: super::FrameRefs {
                ref_frame_idx: [0, 1, 2, 3, 4, 5, 6],
                ..super::FrameRefs::default()
            },
            ..super::FrameHeader::default()
        };
        let mut views = [super::SlotView::default(); super::TOTAL_REFS];
        for (index, view) in views.iter_mut().enumerate() {
            view.valid = true;
            view.order_hint = u8::try_from(index).expect("slot");
        }
        let (forward, backward) = super::skip_mode_refs(&seq, &header, &views);
        assert_eq!(forward, Some(3));
        assert_eq!(backward, Some(5));
    }

    #[test]
    fn global_motion_derives_identity_and_translation() {
        let mut header = super::FrameHeader {
            frame_type: 1,
            primary_ref_frame: 7,
            global: super::GlobalMotionSyntax {
                is_global: [false, true, true, false, false, false, false, false],
                is_rot_zoom: [false, false, true, false, false, false, false, false],
                is_translation: [false, true, false, false, false, false, false, false],
                params: [
                    [0; 6],
                    [2, 0, 0, 0, 0, 0],
                    [0; 6],
                    [0; 6],
                    [0; 6],
                    [0; 6],
                    [0; 6],
                    [0; 6],
                ],
            },
            ..super::FrameHeader::default()
        };
        let views = [super::SlotView::default(); super::TOTAL_REFS];
        super::derive_global_motion(&mut header, &views).expect("derive");
        assert_eq!(header.gm_type()[1], 1);
        assert_eq!(header.gm_params()[1][0], 16384);
        assert_eq!(header.gm_params()[1][1], 0);
        assert_eq!(header.gm_type()[2], 2);
        assert_eq!(header.gm_params()[2], [0, 0, 65536, 0, 0, 65536]);
        assert_eq!(header.gm_params()[3], [0, 0, 65536, 0, 0, 65536]);
    }
}
