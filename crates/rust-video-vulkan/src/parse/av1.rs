//! AV1 headers over the shared grammar, mapped onto tier types.
//!
//! Parsing runs in `rust-video-av1`; this module converts both ways
//! between shared headers and the submit structures below.

use crate::error::ParseError;

use rust_video_av1 as shared;

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
/// AV1 key frame type.
const FRAME_KEY: u8 = 0;
/// AV1 intra-only frame type.
const FRAME_INTRA_ONLY: u8 = 2;

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

/// Checked narrowing of a parsed value.
fn narrow_u8(value: u32, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a parsed value.
fn narrow_u16(value: u32, syntax: &'static str) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
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

    /// Main profile 4:2:0 8-bit sequence header for session tests.
    #[cfg(test)]
    pub(crate) fn test_main_420() -> Self {
        let mut seq = Self::test_with_frame_ids(0, 0);
        seq.frame_id_numbers_present = false;
        seq.color.depth.mono_chrome = false;
        seq.color.subsampling_x = 1;
        seq.color.subsampling_y = 1;
        seq.bit_depth = 8;
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
        }
    }
}

/// Sequence header of one OBU payload.
pub(crate) fn parse_sequence_header(payload: &[u8]) -> Result<SequenceHeader, ParseError> {
    from_shared_seq(&shared::parse_sequence_header::<ParseError>(payload)?)
}

/// Ceiling of `log2(target / blksize)`.
pub(crate) fn tile_log2(blksize: u32, target: u32) -> u32 {
    shared::tile_log2(blksize, target)
}

/// Signed order hint distance from `a` to `b`, zero without order hints.
pub(crate) fn relative_dist(seq: &SequenceHeader, a: u8, b: u8) -> i32 {
    if !seq.enable_order_hint() {
        return 0;
    }
    shared::relative_dist(a, b, seq.order_hint_bits_minus_1())
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

/// Frame header of one OBU payload against `seq` and `slots`.
///
/// Reads reference state for inference and validation; slot effects land
/// in the returned invalidations for the caller to apply.
pub(crate) fn parse_frame_header(
    payload: &[u8],
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
) -> Result<ParsedFrame, ParseError> {
    let shared_seq = to_shared_seq(seq);
    let mut refs = to_shared_slots(slots)?;
    let before = valid_snapshot(&refs);
    let (header, _) =
        shared::parse_frame_header::<ParseError>(payload, &shared_seq, &mut refs, 0, 0)?;
    convert_parsed(&header, seq, slots, &refs, before)
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
    let count = u32::from(tiles.cols) * u32::from(tiles.rows);
    if count == 0 {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    let head = shared::tile_group_range::<ParseError>(
        payload,
        usize::try_from(count).map_err(|_| ParseError::OutOfRange {
            syntax: "tile group range",
        })?,
        narrow_u8(tile_log2(1, u32::from(tiles.cols)), "tile group range")?,
        narrow_u8(tile_log2(1, u32::from(tiles.rows)), "tile group range")?,
    )?;
    let start = u32::try_from(head.start).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let end = u32::try_from(head.end).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    if start < expect_start || start >= count || end < start || end >= count {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    check_align_zero(payload, head.consumed_bits)?;
    let tiles = split_tiles(head.data, start, end, tiles.size_bytes_minus_1)?;
    Ok(ParsedTileGroup {
        start,
        end,
        data: head.data,
        tiles,
    })
}

/// Tile ranges of `data` for tiles `start` through `end`.
fn split_tiles(
    data: &[u8],
    start: u32,
    end: u32,
    size_bytes_minus_1: u8,
) -> Result<Vec<(usize, usize)>, ParseError> {
    let start = usize::try_from(start).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let end = usize::try_from(end).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let width = size_bytes_minus_1
        .checked_add(1)
        .ok_or(ParseError::OutOfRange {
            syntax: "tile size",
        })?;
    shared::split_tiles::<ParseError>(data, start, end, width)
}

/// Frame OBU: header plus its tile group over one OBU payload.
pub(crate) fn parse_frame_obu<'a>(
    payload: &'a [u8],
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
    expect_start: u32,
) -> Result<(ParsedFrame, Option<ParsedTileGroup<'a>>), ParseError> {
    let shared_seq = to_shared_seq(seq);
    let mut refs = to_shared_slots(slots)?;
    let before = valid_snapshot(&refs);
    let (header, consumed) =
        shared::parse_frame_header::<ParseError>(payload, &shared_seq, &mut refs, 0, 0)?;
    let parsed = convert_parsed(&header, seq, slots, &refs, before)?;
    if parsed.header.show().show_existing {
        return Ok((parsed, None));
    }
    let aligned = check_align_zero(payload, consumed)?;
    let rest = payload.get(aligned / 8..).ok_or(ParseError::Truncated {
        syntax: "tile group align",
    })?;
    let group = parse_tile_group_obu(rest, parsed.header.tiles(), expect_start)?;
    Ok((parsed, Some(group)))
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

/// Validity of `refs` per slot.
fn valid_snapshot(refs: &[shared::RefInfo; TOTAL_REFS]) -> [bool; TOTAL_REFS] {
    core::array::from_fn(|index| refs[index].valid)
}

/// Byte alignment past `consumed` bits, rejecting nonzero padding.
fn check_align_zero(payload: &[u8], consumed: usize) -> Result<usize, ParseError> {
    let aligned = consumed.next_multiple_of(8);
    for bit in consumed..aligned {
        if payload[bit / 8] >> (7 - bit % 8) & 1 != 0 {
            return Err(ParseError::OutOfRange {
                syntax: "tile group align",
            });
        }
    }
    Ok(aligned)
}

/// Shared flag word matching `seq`.
fn shared_seq_flags(seq: &SequenceHeader) -> u32 {
    let color = seq.color();
    let mut flags = 0u32;
    if seq.still_picture() {
        flags |= shared::SEQ_STILL_PICTURE;
    }
    if seq.reduced() {
        flags |= shared::SEQ_REDUCED_HEADER;
    }
    if seq.timing_present() {
        flags |= shared::SEQ_TIMING_PRESENT;
    }
    if seq.decoder_model_present() {
        flags |= shared::SEQ_DECODER_MODEL_PRESENT;
    }
    if seq.initial_display_delay_present() {
        flags |= shared::SEQ_INITIAL_DISPLAY_PRESENT;
    }
    if seq.frame_id_numbers_present() {
        flags |= shared::SEQ_FRAME_ID_NUMBERS;
    }
    if seq.use_128x128_superblock() {
        flags |= shared::SEQ_SUPERBLOCK_128;
    }
    if seq.enable_filter_intra() {
        flags |= shared::SEQ_FILTER_INTRA;
    }
    if seq.enable_intra_edge_filter() {
        flags |= shared::SEQ_INTRA_EDGE;
    }
    if seq.enable_interintra_compound() {
        flags |= shared::SEQ_INTERINTRA;
    }
    if seq.enable_masked_compound() {
        flags |= shared::SEQ_MASKED_COMPOUND;
    }
    if seq.enable_warped_motion() {
        flags |= shared::SEQ_WARPED_MOTION;
    }
    if seq.enable_dual_filter() {
        flags |= shared::SEQ_DUAL_FILTER;
    }
    if seq.enable_order_hint() {
        flags |= shared::SEQ_ORDER_HINT;
    }
    if seq.enable_jnt_comp() {
        flags |= shared::SEQ_JNT_COMP;
    }
    if seq.enable_ref_frame_mvs() {
        flags |= shared::SEQ_REF_MVS;
    }
    if seq.seq_force_screen_content_tools() == 2 {
        flags |= shared::SEQ_CHOOSE_SCREEN_CONTENT;
    }
    if seq.seq_force_integer_mv() == 2 && seq.seq_force_screen_content_tools() != 0 {
        flags |= shared::SEQ_CHOOSE_INTEGER_MV;
    }
    if seq.enable_superres() {
        flags |= shared::SEQ_SUPERRES;
    }
    if seq.enable_cdef() {
        flags |= shared::SEQ_CDEF;
    }
    if seq.enable_restoration() {
        flags |= shared::SEQ_RESTORATION;
    }
    if seq.film_grain_params_present() {
        flags |= shared::SEQ_FILM_GRAIN;
    }
    if color.depth.high_bitdepth {
        flags |= shared::SEQ_HIGH_BITDEPTH;
    }
    if color.depth.twelve_bit {
        flags |= shared::SEQ_TWELVE_BIT;
    }
    if color.depth.mono_chrome {
        flags |= shared::SEQ_MONO_CHROME;
    }
    if color.color_range {
        flags |= shared::SEQ_COLOR_RANGE;
    }
    if color.separate_uv_delta_q {
        flags |= shared::SEQ_SEPARATE_UV_DELTA_Q;
    }
    if color.subsampling_x != 0 {
        flags |= shared::SEQ_SUBSAMPLING_X;
    }
    if color.subsampling_y != 0 {
        flags |= shared::SEQ_SUBSAMPLING_Y;
    }
    if color.color_description_present {
        flags |= shared::SEQ_COLOR_DESCRIPTION;
    }
    if seq.timing().equal_picture_interval {
        flags |= shared::SEQ_EQUAL_INTERVAL;
    }
    flags
}

/// Shared sequence header matching `seq`.
fn to_shared_seq(seq: &SequenceHeader) -> shared::SequenceHeader {
    let color = seq.color();
    let mut operating = Vec::with_capacity(seq.operating_points().len());
    for point in seq.operating_points() {
        let decoder_model = if point.decoder_present {
            Some(shared::DecoderModelPoint {
                decoder_buffer_delay: point.decoder_buffer_delay,
                encoder_buffer_delay: point.encoder_buffer_delay,
                low_delay: point.low_delay,
            })
        } else {
            None
        };
        operating.push(shared::OperatingPoint {
            idc: point.idc,
            level: point.level_idx,
            tier: point.tier != 0,
            decoder_model,
            display_delay_present: point.initial_delay_present,
            display_delay_minus1: point.initial_delay_minus_1,
        });
    }
    shared::SequenceHeader {
        profile: seq.profile(),
        flags: shared_seq_flags(seq),
        operating,
        num_units_in_display_tick: seq.timing().num_units_in_display_tick,
        time_scale: seq.timing().time_scale,
        num_ticks_per_picture_minus1: seq.timing().num_ticks_per_picture_minus_1,
        buffer_delay_length_minus1: seq.decoder_model().buffer_delay_length_minus_1,
        num_units_in_decoding_tick: seq.decoder_model().num_units_in_decoding_tick,
        buffer_removal_length_minus1: seq.decoder_model().buffer_removal_time_length_minus_1,
        presentation_length_minus1: seq.decoder_model().frame_presentation_time_length_minus_1,
        frame_width_bits_minus1: seq.frame_width_bits_minus_1(),
        frame_height_bits_minus1: seq.frame_height_bits_minus_1(),
        max_frame_width_minus1: u32::from(seq.max_frame_width_minus_1()),
        max_frame_height_minus1: u32::from(seq.max_frame_height_minus_1()),
        delta_frame_id_length_minus2: seq.delta_frame_id_length_minus_2(),
        additional_frame_id_length_minus1: seq.additional_frame_id_length_minus_1(),
        force_screen_content_tools: seq.seq_force_screen_content_tools(),
        force_integer_mv: seq.seq_force_integer_mv(),
        order_hint_bits_minus1: seq.order_hint_bits_minus_1(),
        color_primaries: color.color_primaries,
        transfer_characteristics: color.transfer_characteristics,
        matrix_coefficients: color.matrix_coefficients,
        chroma_sample_position: color.chroma_sample_position,
    }
}

/// Tier sequence header matching shared `seq`.
fn from_shared_seq(seq: &shared::SequenceHeader) -> Result<SequenceHeader, ParseError> {
    let mut operating_points = Vec::with_capacity(seq.operating.len());
    for point in &seq.operating {
        let model = point.decoder_model.as_ref();
        operating_points.push(OperatingPoint {
            idc: point.idc,
            level_idx: point.level,
            tier: u8::from(point.tier),
            decoder_present: model.is_some(),
            decoder_buffer_delay: model.map_or(0, |model| model.decoder_buffer_delay),
            encoder_buffer_delay: model.map_or(0, |model| model.encoder_buffer_delay),
            low_delay: model.is_some_and(|model| model.low_delay),
            initial_delay_present: point.display_delay_present,
            initial_delay_minus_1: point.display_delay_minus1,
        });
    }
    Ok(SequenceHeader {
        profile: seq.profile,
        still: SeqStill {
            still_picture: seq.still_picture(),
            reduced: seq.reduced_header(),
        },
        presence: SeqPresence {
            timing_present: seq.timing_info_present(),
            decoder_model_present: seq.decoder_model_present(),
            initial_display_delay_present: seq.initial_display_delay_present(),
        },
        timing: TimingInfo {
            num_units_in_display_tick: seq.num_units_in_display_tick,
            time_scale: seq.time_scale,
            equal_picture_interval: seq.equal_picture_interval(),
            num_ticks_per_picture_minus_1: seq.num_ticks_per_picture_minus1,
        },
        decoder_model: DecoderModelInfo {
            buffer_delay_length_minus_1: seq.buffer_delay_length_minus1,
            num_units_in_decoding_tick: seq.num_units_in_decoding_tick,
            buffer_removal_time_length_minus_1: seq.buffer_removal_length_minus1,
            frame_presentation_time_length_minus_1: seq.presentation_length_minus1,
        },
        operating_points,
        frame_width_bits_minus_1: seq.frame_width_bits_minus1,
        frame_height_bits_minus_1: seq.frame_height_bits_minus1,
        max_frame_width_minus_1: narrow_u16(seq.max_frame_width_minus1, "seq max width")?,
        max_frame_height_minus_1: narrow_u16(seq.max_frame_height_minus1, "seq max height")?,
        frame_id_numbers_present: seq.frame_id_numbers_present(),
        delta_frame_id_length_minus_2: seq.delta_frame_id_length_minus2,
        additional_frame_id_length_minus_1: seq.additional_frame_id_length_minus1,
        blocks: SeqBlocks {
            use_128x128_superblock: seq.use_128x128_superblock(),
            enable_filter_intra: seq.enable_filter_intra(),
            enable_intra_edge_filter: seq.enable_intra_edge_filter(),
        },
        compound: SeqCompound {
            enable_interintra_compound: seq.enable_interintra_compound(),
            enable_masked_compound: seq.enable_masked_compound(),
            enable_warped_motion: seq.enable_warped_motion(),
        },
        filters: SeqFilters {
            enable_dual_filter: seq.enable_dual_filter(),
            enable_cdef: seq.enable_cdef(),
            enable_restoration: seq.enable_restoration(),
        },
        order: SeqOrder {
            enable_order_hint: seq.enable_order_hint(),
            enable_jnt_comp: seq.enable_jnt_comp(),
            enable_ref_frame_mvs: seq.enable_ref_frame_mvs(),
        },
        seq_force_screen_content_tools: seq.force_screen_content_tools,
        seq_force_integer_mv: seq.force_integer_mv,
        order_hint_bits_minus_1: seq.order_hint_bits_minus1,
        enable_superres: seq.enable_superres(),
        color: ColorConfig {
            depth: ColorDepth {
                high_bitdepth: seq.flags & shared::SEQ_HIGH_BITDEPTH != 0,
                twelve_bit: seq.flags & shared::SEQ_TWELVE_BIT != 0,
                mono_chrome: seq.mono_chrome(),
            },
            color_description_present: seq.color_description_present(),
            color_primaries: seq.color_primaries,
            transfer_characteristics: seq.transfer_characteristics,
            matrix_coefficients: seq.matrix_coefficients,
            color_range: seq.flags & shared::SEQ_COLOR_RANGE != 0,
            subsampling_x: u8::from(seq.subsampling_x()),
            subsampling_y: u8::from(seq.subsampling_y()),
            chroma_sample_position: seq.chroma_sample_position,
            separate_uv_delta_q: seq.separate_uv_delta_q(),
        },
        film_grain_params_present: seq.film_grain_params_present(),
        bit_depth: seq.bit_depth(),
    })
}

/// Shared segmentation parameters matching `seg`.
fn to_shared_seg(seg: &SegParams) -> ([[bool; 8]; 8], [[i16; 8]; 8]) {
    (seg.feature_enabled, seg.feature_value)
}

/// Tier segmentation parameters matching shared `enabled` and `value`.
fn from_shared_seg(
    enabled: [[bool; 8]; 8],
    value: [[i16; 8]; 8],
    header: &shared::FrameHeader,
) -> SegParams {
    SegParams {
        enabled: header.segmentation_enabled(),
        update: SegUpdate {
            update_map: header.segmentation_update_map(),
            temporal_update: header.segmentation_temporal_update(),
            update_data: header.segmentation_update_data(),
        },
        feature_enabled: enabled,
        feature_value: value,
    }
}

/// Value/scaling pairs of one grain plane.
fn join_points<const N: usize>(values: &[u8; N], scalings: &[u8; N]) -> [[u8; 2]; N] {
    core::array::from_fn(|index| [values[index], scalings[index]])
}

/// Shared film grain parameters matching `grain`.
fn to_shared_grain(grain: &FilmGrainParams) -> shared::FilmGrain {
    let mut flags = 0u8;
    if grain.apply_grain {
        flags |= shared::FG_APPLY;
    }
    if grain.chroma_scaling_from_luma {
        flags |= shared::FG_CHROMA_FROM_LUMA;
    }
    if grain.clip.overlap_flag {
        flags |= shared::FG_OVERLAP;
    }
    if grain.clip.clip_to_restricted_range {
        flags |= shared::FG_RESTRICTED_RANGE;
    }
    shared::FilmGrain {
        flags,
        update_grain: grain.update_grain,
        ref_idx: grain.ref_idx,
        grain_seed: grain.grain_seed,
        scaling_points_y: join_points(&grain.point_y_value, &grain.point_y_scaling),
        num_y_points: grain.num_y_points,
        scaling_points_cb: join_points(&grain.point_cb_value, &grain.point_cb_scaling),
        num_cb_points: grain.num_cb_points,
        scaling_points_cr: join_points(&grain.point_cr_value, &grain.point_cr_scaling),
        num_cr_points: grain.num_cr_points,
        ar_coeffs_y: grain.ar_coeffs_y,
        ar_coeffs_cb: grain.ar_coeffs_cb,
        ar_coeffs_cr: grain.ar_coeffs_cr,
        grain_scaling_minus_8: grain.grain_scaling_minus_8,
        ar_coeff_lag: grain.ar_coeff_lag,
        ar_coeff_shift_minus_6: grain.ar_coeff_shift_minus_6,
        grain_scale_shift: grain.grain_scale_shift,
        cb_mult: grain.cb_mult,
        cb_luma_mult: grain.cb_luma_mult,
        cr_mult: grain.cr_mult,
        cr_luma_mult: grain.cr_luma_mult,
        cb_offset: grain.cb_offset,
        cr_offset: grain.cr_offset,
    }
}

/// Tier film grain parameters matching shared `grain`.
fn from_shared_grain(grain: &shared::FilmGrain) -> FilmGrainParams {
    let mut params = FilmGrainParams {
        apply_grain: grain.apply_grain(),
        update_grain: grain.update_grain,
        grain_seed: grain.grain_seed,
        ref_idx: grain.ref_idx,
        num_y_points: grain.num_y_points,
        chroma_scaling_from_luma: grain.chroma_scaling_from_luma(),
        num_cb_points: grain.num_cb_points,
        num_cr_points: grain.num_cr_points,
        grain_scaling_minus_8: grain.grain_scaling_minus_8,
        ar_coeff_lag: grain.ar_coeff_lag,
        ar_coeffs_y: grain.ar_coeffs_y,
        ar_coeffs_cb: grain.ar_coeffs_cb,
        ar_coeffs_cr: grain.ar_coeffs_cr,
        ar_coeff_shift_minus_6: grain.ar_coeff_shift_minus_6,
        grain_scale_shift: grain.grain_scale_shift,
        cb_mult: grain.cb_mult,
        cb_luma_mult: grain.cb_luma_mult,
        cb_offset: grain.cb_offset,
        cr_mult: grain.cr_mult,
        cr_luma_mult: grain.cr_luma_mult,
        cr_offset: grain.cr_offset,
        clip: GrainClip {
            overlap_flag: grain.overlap_flag(),
            clip_to_restricted_range: grain.clip_to_restricted_range(),
        },
        ..FilmGrainParams::default()
    };
    for (index, point) in grain.scaling_points_y.iter().enumerate() {
        params.point_y_value[index] = point[0];
        params.point_y_scaling[index] = point[1];
    }
    for (index, point) in grain.scaling_points_cb.iter().enumerate() {
        params.point_cb_value[index] = point[0];
        params.point_cb_scaling[index] = point[1];
    }
    for (index, point) in grain.scaling_points_cr.iter().enumerate() {
        params.point_cr_value[index] = point[0];
        params.point_cr_scaling[index] = point[1];
    }
    params
}

/// Shared reference slots matching `slots`.
fn to_shared_slots(
    slots: &[SlotView; TOTAL_REFS],
) -> Result<[shared::RefInfo; TOTAL_REFS], ParseError> {
    let mut refs: [shared::RefInfo; TOTAL_REFS] =
        core::array::from_fn(|_| shared::RefInfo::default());
    for (view, slot) in slots.iter().zip(refs.iter_mut()) {
        let (seg_enabled, seg_value) = to_shared_seg(&view.segmentation);
        let mut gm_params = [[0i32; 6]; 7];
        for (name, params) in gm_params.iter_mut().enumerate() {
            *params = view.gm_params[name + 1];
        }
        *slot = shared::RefInfo {
            valid: view.valid,
            frame_type: shared::FrameType::new::<ParseError>(u32::from(view.frame_type))?,
            frame_width: view.upscaled_width,
            frame_height: view.frame_height,
            upscaled_width: view.upscaled_width,
            render_width: view.render_width,
            render_height: view.render_height,
            order_hint: view.order_hint,
            frame_id: view.frame_id,
            loop_ref_deltas: view.loop_filter.ref_deltas,
            loop_mode_deltas: view.loop_filter.mode_deltas,
            seg_enabled,
            seg_value,
            gm_params,
            grain: to_shared_grain(&view.grain),
        };
    }
    Ok(refs)
}

/// Tier tile layout matching shared `header`.
fn from_shared_tiles(header: &shared::FrameHeader) -> Result<TileLayout, ParseError> {
    let cols = usize::from(header.tile_cols);
    let rows = usize::from(header.tile_rows);
    let mut layout = TileLayout {
        uniform: header.uniform_tile_spacing,
        cols: header.tile_cols,
        rows: header.tile_rows,
        context_update_tile_id: header.context_update_tile_id,
        size_bytes_minus_1: header.tile_size_bytes.saturating_sub(1),
        ..TileLayout::default()
    };
    let mut at = 0u32;
    for index in 0..cols {
        layout.start_col_sb[index] = narrow_u8(at, "frame tile col")?;
        let width = u32::from(header.width_in_sbs_minus_1[index]);
        layout.width_in_sbs_minus_1[index] = narrow_u8(width, "frame tile width")?;
        at += width + 1;
    }
    at = 0;
    for index in 0..rows {
        layout.start_row_sb[index] = narrow_u8(at, "frame tile row")?;
        let height = u32::from(header.height_in_sbs_minus_1[index]);
        layout.height_in_sbs_minus_1[index] = narrow_u8(height, "frame tile height")?;
        at += height + 1;
    }
    Ok(layout)
}

/// Tier show-existing header over the shown `view`.
fn show_existing_header(
    header: &shared::FrameHeader,
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
) -> FrameHeader {
    let view = &slots[usize::from(header.frame_to_show_map_idx)];
    FrameHeader {
        show: FrameShow {
            show_existing: true,
            map_idx: header.frame_to_show_map_idx,
            presentation_time: header.presentation_time,
            display_frame_id: header.display_frame_id,
        },
        frame_type: header.frame_type.id(),
        order_hint: if seq.enable_order_hint() {
            header.order_hint
        } else {
            0
        },
        refresh_frame_flags: header.refresh_frame_flags,
        sizes: FrameSizes {
            width_minus_1: view.upscaled_width.saturating_sub(1),
            height_minus_1: view.frame_height.saturating_sub(1),
            render_width_minus_1: view.render_width.saturating_sub(1),
            render_height_minus_1: view.render_height.saturating_sub(1),
            frame_width: view.upscaled_width,
            frame_height: view.frame_height,
            upscaled_width: view.upscaled_width,
            render_width: view.render_width,
            render_height: view.render_height,
        },
        ..FrameHeader::default()
    }
}

/// Tier frame sizes matching shared `header`.
fn frame_sizes(header: &shared::FrameHeader) -> FrameSizes {
    FrameSizes {
        width_minus_1: header.upscaled_width.saturating_sub(1),
        height_minus_1: header.frame_height.saturating_sub(1),
        render_width_minus_1: header.render_width.saturating_sub(1),
        render_height_minus_1: header.render_height.saturating_sub(1),
        frame_width: header.frame_width,
        frame_height: header.frame_height,
        upscaled_width: header.upscaled_width,
        render_width: header.render_width,
        render_height: header.render_height,
    }
}

/// Tier reference selection matching shared `header`.
fn frame_refs(header: &shared::FrameHeader) -> FrameRefs {
    FrameRefs {
        short_signaling: header.frame_refs_short_signaling(),
        last_idx: header.short_last_idx,
        golden_idx: header.short_golden_idx,
        ref_frame_idx: header.ref_frame_idx,
        delta_frame_id_minus1: header.delta_frame_id_minus1,
        found_ref: header.found_ref,
    }
}

/// Tier quantizer matching shared `header`.
fn frame_quant(header: &shared::FrameHeader) -> QuantParams {
    QuantParams {
        base_q_idx: header.base_q_idx,
        delta_q_y_dc: header.delta_q_y_dc,
        delta_q_u_dc: header.delta_q_u_dc,
        delta_q_u_ac: header.delta_q_u_ac,
        delta_q_v_dc: header.delta_q_v_dc,
        delta_q_v_ac: header.delta_q_v_ac,
        using_qmatrix: header.using_qmatrix(),
        diff_uv_delta: header.diff_uv_delta(),
        qm_y: header.qm_y,
        qm_u: header.qm_u,
        qm_v: header.qm_v,
    }
}

/// Tier loop filter matching shared `header`.
fn frame_loop_filter(header: &shared::FrameHeader) -> LoopFilterParams {
    LoopFilterParams {
        level: header.loop_filter_level,
        sharpness: header.loop_filter_sharpness,
        delta_enabled: header.loop_filter_delta_enabled(),
        delta_update: header.loop_filter_delta_update(),
        ref_deltas: header.loop_ref_deltas,
        mode_deltas: header.loop_mode_deltas,
    }
}

/// Tier CDEF matching shared `header`.
fn frame_cdef(header: &shared::FrameHeader) -> CdefParams {
    CdefParams {
        damping_minus_3: header.cdef_damping_minus_3,
        bits: header.cdef_bits,
        y_pri: header.cdef_y_pri,
        y_sec: header.cdef_y_sec,
        uv_pri: header.cdef_uv_pri,
        uv_sec: header.cdef_uv_sec,
    }
}

/// Tier restoration matching shared `header`.
fn frame_restoration(header: &shared::FrameHeader) -> RestorationParams {
    RestorationParams {
        lr_type: header.lr_type,
        unit_shift: header.lr_unit_shift,
        uv_shift: header.lr_uv_shift,
    }
}

/// Tier frame header matching shared `header`.
fn from_shared_header(
    header: &shared::FrameHeader,
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
) -> Result<FrameHeader, ParseError> {
    if header.show_existing_frame() {
        return Ok(show_existing_header(header, seq, slots));
    }
    let use_superres = header.superres_denom != shared::SUPERRES_NUM;
    let mut gm_type = [0u8; TOTAL_REFS];
    let mut gm_params = [[0i32; 6]; TOTAL_REFS];
    for (name, motion) in header.global_motion.iter().enumerate() {
        gm_type[name + 1] = motion.warp_type;
        gm_params[name + 1] = motion.params;
    }
    let mut converted = FrameHeader {
        show: FrameShow {
            show_existing: false,
            map_idx: 0,
            presentation_time: header.presentation_time,
            display_frame_id: 0,
        },
        frame_type: header.frame_type.id(),
        display: FrameDisplay {
            show_frame: header.show_frame(),
            showable_frame: header.showable_frame(),
        },
        resilience: FrameResilience {
            error_resilient_mode: header.error_resilient_mode(),
            disable_cdf_update: header.disable_cdf_update(),
            frame_end_update_disabled: header.disable_frame_end_update_cdf(),
        },
        content: FrameContent {
            allow_screen_content_tools: header.allow_screen_content_tools(),
            force_integer_mv: header.force_integer_mv(),
            allow_intrabc: header.allow_intrabc(),
        },
        current_frame_id: header.current_frame_id,
        sizing: FrameSizing {
            frame_size_override_flag: header.frame_size_override_flag(),
            use_superres,
            render_and_frame_size_different: header.render_and_frame_size_different(),
        },
        order_hint: header.order_hint,
        primary_ref_frame: header.primary_ref_frame,
        buffer_removal_time_present_flag: header.buffer_removal_time_present,
        refresh_frame_flags: header.refresh_frame_flags,
        sizes: frame_sizes(header),
        coded_denom: if use_superres {
            header
                .superres_denom
                .saturating_sub(shared::SUPERRES_DENOM_MIN)
        } else {
            0
        },
        refs: frame_refs(header),
        motion: FrameMotion {
            allow_high_precision_mv: header.allow_high_precision_mv(),
            is_motion_mode_switchable: header.is_motion_mode_switchable(),
            use_ref_frame_mvs: header.use_ref_frame_mvs(),
        },
        interpolation_filter: header.interp_filter,
        prediction: FramePrediction {
            is_filter_switchable: header.interp_filter == shared::INTERP_SWITCHABLE,
            allow_warped_motion: header.allow_warped_motion(),
            reference_select: header.reference_select(),
        },
        reduced_tx_set: header.reduced_tx_set(),
        tx_mode: header.tx_mode,
        skip_mode_present: header.skip_mode_present(),
        tiles: from_shared_tiles(header)?,
        quant: frame_quant(header),
        segmentation: from_shared_seg(header.seg_enabled, header.seg_value, header),
        delta: FrameDelta {
            delta_q_present: header.delta_q_present(),
            delta_lf_present: header.delta_lf_present(),
            delta_lf_multi: header.delta_lf_multi(),
        },
        delta_q_res: header.delta_q_res,
        delta_lf_res: header.delta_lf_res,
        loop_filter: frame_loop_filter(header),
        cdef: frame_cdef(header),
        restoration: frame_restoration(header),
        global: GlobalMotionSyntax::default(),
        grain: from_shared_grain(&header.film_grain),
        gm_type,
        gm_params,
        ..FrameHeader::default()
    };
    derive_skip_mode(seq, &mut converted, slots)?;
    Ok(converted)
}

/// Tier parsed frame matching shared `header`.
fn convert_parsed(
    header: &shared::FrameHeader,
    seq: &SequenceHeader,
    slots: &[SlotView; TOTAL_REFS],
    refs: &[shared::RefInfo; TOTAL_REFS],
    before: [bool; TOTAL_REFS],
) -> Result<ParsedFrame, ParseError> {
    let reset_all = header.frame_type == shared::FrameType::Key && header.show_frame();
    let mut invalidated = [false; TOTAL_REFS];
    if !reset_all {
        for (index, slot) in refs.iter().enumerate() {
            invalidated[index] = before[index] && !slot.valid;
        }
    }
    let converted = from_shared_header(header, seq, slots)?;
    Ok(ParsedFrame {
        header: converted,
        reset_all,
        invalidated,
    })
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

/// Skip mode reference names into `header`.
fn derive_skip_mode(
    seq: &SequenceHeader,
    header: &mut FrameHeader,
    views: &[SlotView; TOTAL_REFS],
) -> Result<(), ParseError> {
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

#[cfg(test)]
mod tests {
    use super::{
        relative_dist, split_obus, split_tiles, temporal_units, tile_log2, SequenceHeader,
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
        }
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
}
