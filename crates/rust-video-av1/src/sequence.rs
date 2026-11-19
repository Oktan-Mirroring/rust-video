//! AV1 sequence headers.

use crate::bits::BitReader;
use crate::error::{Error, ParseKind};
use crate::syntax::{read_small, read_uvlc};
use crate::MAX_OPERATING_POINTS;
use crate::{SELECT_INTEGER_MV, SELECT_SCREEN_CONTENT_TOOLS};

/// Still picture flag bit.
pub const SEQ_STILL_PICTURE: u32 = 1;
/// Reduced still picture header flag bit.
pub const SEQ_REDUCED_HEADER: u32 = 1 << 1;
/// Timing info present flag bit.
pub const SEQ_TIMING_PRESENT: u32 = 1 << 2;
/// Decoder model info present flag bit.
pub const SEQ_DECODER_MODEL_PRESENT: u32 = 1 << 3;
/// Initial display delay present flag bit.
pub const SEQ_INITIAL_DISPLAY_PRESENT: u32 = 1 << 4;
/// Frame id numbers present flag bit.
pub const SEQ_FRAME_ID_NUMBERS: u32 = 1 << 5;
/// 128x128 superblocks flag bit.
pub const SEQ_SUPERBLOCK_128: u32 = 1 << 6;
/// Filter intra flag bit.
pub const SEQ_FILTER_INTRA: u32 = 1 << 7;
/// Intra edge filter flag bit.
pub const SEQ_INTRA_EDGE: u32 = 1 << 8;
/// Interintra compound flag bit.
pub const SEQ_INTERINTRA: u32 = 1 << 9;
/// Masked compound flag bit.
pub const SEQ_MASKED_COMPOUND: u32 = 1 << 10;
/// Warped motion flag bit.
pub const SEQ_WARPED_MOTION: u32 = 1 << 11;
/// Dual filter flag bit.
pub const SEQ_DUAL_FILTER: u32 = 1 << 12;
/// Order hint flag bit.
pub const SEQ_ORDER_HINT: u32 = 1 << 13;
/// Joint compound flag bit.
pub const SEQ_JNT_COMP: u32 = 1 << 14;
/// Reference motion vectors flag bit.
pub const SEQ_REF_MVS: u32 = 1 << 15;
/// Screen content tools selectable flag bit.
pub const SEQ_CHOOSE_SCREEN_CONTENT: u32 = 1 << 16;
/// Integer motion vectors selectable flag bit.
pub const SEQ_CHOOSE_INTEGER_MV: u32 = 1 << 17;
/// Superres flag bit.
pub const SEQ_SUPERRES: u32 = 1 << 18;
/// CDEF flag bit.
pub const SEQ_CDEF: u32 = 1 << 19;
/// Restoration flag bit.
pub const SEQ_RESTORATION: u32 = 1 << 20;
/// Film grain present flag bit.
pub const SEQ_FILM_GRAIN: u32 = 1 << 21;
/// High bit depth flag bit.
pub const SEQ_HIGH_BITDEPTH: u32 = 1 << 22;
/// Twelve bit flag bit.
pub const SEQ_TWELVE_BIT: u32 = 1 << 23;
/// Monochrome flag bit.
pub const SEQ_MONO_CHROME: u32 = 1 << 24;
/// Color range flag bit.
pub const SEQ_COLOR_RANGE: u32 = 1 << 26;
/// Color description present flag bit.
pub const SEQ_COLOR_DESCRIPTION: u32 = 1 << 25;
/// Separate UV delta quantizer flag bit.
pub const SEQ_SEPARATE_UV_DELTA_Q: u32 = 1 << 27;
/// Horizontal subsampling flag bit.
pub const SEQ_SUBSAMPLING_X: u32 = 1 << 28;
/// Vertical subsampling flag bit.
pub const SEQ_SUBSAMPLING_Y: u32 = 1 << 29;
/// Equal picture interval flag bit.
pub const SEQ_EQUAL_INTERVAL: u32 = 1 << 30;

/// Decoder model delays of one operating point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecoderModelPoint {
    /// Decoder buffer delay in ticks.
    pub decoder_buffer_delay: u32,
    /// Encoder buffer delay in ticks.
    pub encoder_buffer_delay: u32,
    /// Low delay mode.
    pub low_delay: bool,
}

/// Operating point behind decoder model and display timing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperatingPoint {
    /// Operating point idc.
    pub idc: u16,
    /// Level index.
    pub level: u8,
    /// High tier.
    pub tier: bool,
    /// Decoder model delays, if signaled for this point.
    pub decoder_model: Option<DecoderModelPoint>,
    /// Display delay present for this point.
    pub display_delay_present: bool,
    /// Display delay minus one.
    pub display_delay_minus1: u8,
}

/// Parsed sequence header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceHeader {
    /// Sequence profile.
    pub profile: u8,
    /// Packed tool and color flags.
    pub flags: u32,
    /// Operating points.
    pub operating: Vec<OperatingPoint>,
    /// Display tick units per time scale.
    pub num_units_in_display_tick: u32,
    /// Ticks per second.
    pub time_scale: u32,
    /// Ticks per picture minus one.
    pub num_ticks_per_picture_minus1: u32,
    /// Buffer delay length minus one.
    pub buffer_delay_length_minus1: u8,
    /// Decoding tick units per time scale.
    pub num_units_in_decoding_tick: u32,
    /// Buffer removal length minus one.
    pub buffer_removal_length_minus1: u8,
    /// Presentation time length minus one.
    pub presentation_length_minus1: u8,
    /// Frame width bit count minus one.
    pub frame_width_bits_minus1: u8,
    /// Frame height bit count minus one.
    pub frame_height_bits_minus1: u8,
    /// Maximum frame width minus one.
    pub max_frame_width_minus1: u32,
    /// Maximum frame height minus one.
    pub max_frame_height_minus1: u32,
    /// Frame id delta length minus two.
    pub delta_frame_id_length_minus2: u8,
    /// Additional frame id length minus one.
    pub additional_frame_id_length_minus1: u8,
    /// Forced screen content tools.
    pub force_screen_content_tools: u8,
    /// Forced integer motion vectors.
    pub force_integer_mv: u8,
    /// Order hint bit count minus one.
    pub order_hint_bits_minus1: u8,
    /// Color primaries.
    pub color_primaries: u8,
    /// Transfer characteristics.
    pub transfer_characteristics: u8,
    /// Matrix coefficients.
    pub matrix_coefficients: u8,
    /// Chroma sample position.
    pub chroma_sample_position: u8,
}

impl SequenceHeader {
    /// Still picture sequence.
    #[must_use]
    pub const fn still_picture(&self) -> bool {
        self.flags & SEQ_STILL_PICTURE != 0
    }

    /// Reduced still picture header.
    #[must_use]
    pub const fn reduced_header(&self) -> bool {
        self.flags & SEQ_REDUCED_HEADER != 0
    }

    /// Timing info present.
    #[must_use]
    pub const fn timing_info_present(&self) -> bool {
        self.flags & SEQ_TIMING_PRESENT != 0
    }

    /// Decoder model info present.
    #[must_use]
    pub const fn decoder_model_present(&self) -> bool {
        self.flags & SEQ_DECODER_MODEL_PRESENT != 0
    }

    /// Initial display delay present.
    #[must_use]
    pub const fn initial_display_delay_present(&self) -> bool {
        self.flags & SEQ_INITIAL_DISPLAY_PRESENT != 0
    }

    /// Color description present.
    #[must_use]
    pub const fn color_description_present(&self) -> bool {
        self.flags & SEQ_COLOR_DESCRIPTION != 0
    }

    /// Frame id numbers present.
    #[must_use]
    pub const fn frame_id_numbers_present(&self) -> bool {
        self.flags & SEQ_FRAME_ID_NUMBERS != 0
    }

    /// 128x128 superblocks.
    #[must_use]
    pub const fn use_128x128_superblock(&self) -> bool {
        self.flags & SEQ_SUPERBLOCK_128 != 0
    }

    /// Filter intra.
    #[must_use]
    pub const fn enable_filter_intra(&self) -> bool {
        self.flags & SEQ_FILTER_INTRA != 0
    }

    /// Intra edge filter.
    #[must_use]
    pub const fn enable_intra_edge_filter(&self) -> bool {
        self.flags & SEQ_INTRA_EDGE != 0
    }

    /// Interintra compound.
    #[must_use]
    pub const fn enable_interintra_compound(&self) -> bool {
        self.flags & SEQ_INTERINTRA != 0
    }

    /// Masked compound.
    #[must_use]
    pub const fn enable_masked_compound(&self) -> bool {
        self.flags & SEQ_MASKED_COMPOUND != 0
    }

    /// Warped motion.
    #[must_use]
    pub const fn enable_warped_motion(&self) -> bool {
        self.flags & SEQ_WARPED_MOTION != 0
    }

    /// Dual filter.
    #[must_use]
    pub const fn enable_dual_filter(&self) -> bool {
        self.flags & SEQ_DUAL_FILTER != 0
    }

    /// Order hint.
    #[must_use]
    pub const fn enable_order_hint(&self) -> bool {
        self.flags & SEQ_ORDER_HINT != 0
    }

    /// Joint compound.
    #[must_use]
    pub const fn enable_jnt_comp(&self) -> bool {
        self.flags & SEQ_JNT_COMP != 0
    }

    /// Reference motion vectors.
    #[must_use]
    pub const fn enable_ref_frame_mvs(&self) -> bool {
        self.flags & SEQ_REF_MVS != 0
    }

    /// Superres.
    #[must_use]
    pub const fn enable_superres(&self) -> bool {
        self.flags & SEQ_SUPERRES != 0
    }

    /// CDEF.
    #[must_use]
    pub const fn enable_cdef(&self) -> bool {
        self.flags & SEQ_CDEF != 0
    }

    /// Restoration.
    #[must_use]
    pub const fn enable_restoration(&self) -> bool {
        self.flags & SEQ_RESTORATION != 0
    }

    /// Film grain parameters present.
    #[must_use]
    pub const fn film_grain_params_present(&self) -> bool {
        self.flags & SEQ_FILM_GRAIN != 0
    }

    /// Separate UV delta quantizer.
    #[must_use]
    pub const fn separate_uv_delta_q(&self) -> bool {
        self.flags & SEQ_SEPARATE_UV_DELTA_Q != 0
    }

    /// Horizontal subsampling.
    #[must_use]
    pub const fn subsampling_x(&self) -> bool {
        self.flags & SEQ_SUBSAMPLING_X != 0
    }

    /// Vertical subsampling.
    #[must_use]
    pub const fn subsampling_y(&self) -> bool {
        self.flags & SEQ_SUBSAMPLING_Y != 0
    }

    /// Monochrome.
    #[must_use]
    pub const fn mono_chrome(&self) -> bool {
        self.flags & SEQ_MONO_CHROME != 0
    }

    /// Equal picture interval.
    #[must_use]
    pub const fn equal_picture_interval(&self) -> bool {
        self.flags & SEQ_EQUAL_INTERVAL != 0
    }

    /// Bit depth from profile and color flags.
    #[must_use]
    pub const fn bit_depth(&self) -> u8 {
        if self.flags & SEQ_HIGH_BITDEPTH == 0 {
            8
        } else if self.profile == 2 && self.flags & SEQ_TWELVE_BIT != 0 {
            12
        } else {
            10
        }
    }

    /// Plane count from the monochrome flag.
    #[must_use]
    pub const fn num_planes(&self) -> u8 {
        if self.flags & SEQ_MONO_CHROME != 0 {
            1
        } else {
            3
        }
    }

    /// Maximum frame width in luma samples.
    #[must_use]
    pub const fn max_frame_width(&self) -> u32 {
        self.max_frame_width_minus1 + 1
    }

    /// Maximum frame height in luma samples.
    #[must_use]
    pub const fn max_frame_height(&self) -> u32 {
        self.max_frame_height_minus1 + 1
    }

    /// Order hint bit count.
    #[must_use]
    pub const fn order_hint_bits(&self) -> u8 {
        if self.flags & SEQ_ORDER_HINT != 0 {
            self.order_hint_bits_minus1 + 1
        } else {
            0
        }
    }

    /// Frame id bit count.
    #[must_use]
    pub const fn id_len(&self) -> u8 {
        self.additional_frame_id_length_minus1 + self.delta_frame_id_length_minus2 + 3
    }
}

/// Sequence header from OBU `payload`.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range syntax.
pub fn parse_sequence_header<E: From<Error>>(payload: &[u8]) -> Result<SequenceHeader, E> {
    let mut reader = BitReader::new(payload);
    let profile = reader.read_bits(3, "seq_profile")?;
    if profile > 2 {
        return Err(Error::parse(ParseKind::Range {
            syntax: "seq_profile",
        }));
    }
    let profile = u8::try_from(profile).map_err(|_| {
        Error::parse(ParseKind::Range {
            syntax: "seq_profile",
        })
    })?;
    let mut flags = 0u32;
    if reader.read_bit("still_picture")? {
        flags |= SEQ_STILL_PICTURE;
    }
    if reader.read_bit("reduced_still_picture_header")? {
        flags |= SEQ_REDUCED_HEADER;
    }
    let mut seq = SequenceHeader {
        profile,
        flags,
        operating: Vec::new(),
        num_units_in_display_tick: 0,
        time_scale: 0,
        num_ticks_per_picture_minus1: 0,
        buffer_delay_length_minus1: 0,
        num_units_in_decoding_tick: 0,
        buffer_removal_length_minus1: 0,
        presentation_length_minus1: 0,
        frame_width_bits_minus1: 0,
        frame_height_bits_minus1: 0,
        max_frame_width_minus1: 0,
        max_frame_height_minus1: 0,
        delta_frame_id_length_minus2: 0,
        additional_frame_id_length_minus1: 0,
        force_screen_content_tools: SELECT_SCREEN_CONTENT_TOOLS,
        force_integer_mv: SELECT_INTEGER_MV,
        order_hint_bits_minus1: 0,
        color_primaries: 2,
        transfer_characteristics: 2,
        matrix_coefficients: 2,
        chroma_sample_position: 0,
    };
    parse_seq_timing(&mut reader, &mut seq)?;
    parse_seq_operating_points(&mut reader, &mut seq)?;
    parse_seq_frame_size_bits(&mut reader, &mut seq)?;
    parse_seq_frame_ids(&mut reader, &mut seq)?;
    parse_seq_tools(&mut reader, &mut seq)?;
    if reader.read_bit("enable_superres")? {
        seq.flags |= SEQ_SUPERRES;
    }
    if reader.read_bit("enable_cdef")? {
        seq.flags |= SEQ_CDEF;
    }
    if reader.read_bit("enable_restoration")? {
        seq.flags |= SEQ_RESTORATION;
    }
    parse_color_config(&mut reader, &mut seq)?;
    if reader.read_bit("film_grain_params_present")? {
        seq.flags |= SEQ_FILM_GRAIN;
    }
    Ok(seq)
}

/// Sequence timing and decoder model info.
fn parse_seq_timing<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if seq.reduced_header() {
        return Ok(());
    }
    if !reader.read_bit("timing_info_present_flag")? {
        return Ok(());
    }
    seq.flags |= SEQ_TIMING_PRESENT;
    seq.num_units_in_display_tick = reader.read_bits(32, "num_units_in_display_tick")?;
    seq.time_scale = reader.read_bits(32, "time_scale")?;
    if reader.read_bit("equal_picture_interval")? {
        seq.flags |= SEQ_EQUAL_INTERVAL;
        seq.num_ticks_per_picture_minus1 = read_uvlc(reader, "num_ticks_per_picture_minus1")?;
    }
    if !reader.read_bit("decoder_model_info_present_flag")? {
        return Ok(());
    }
    seq.flags |= SEQ_DECODER_MODEL_PRESENT;
    seq.buffer_delay_length_minus1 = read_small(reader, 5, "buffer_delay_length_minus_1")?;
    seq.num_units_in_decoding_tick = reader.read_bits(32, "num_units_in_decoding_tick")?;
    seq.buffer_removal_length_minus1 = read_small(reader, 5, "buffer_removal_time_length_minus_1")?;
    seq.presentation_length_minus1 =
        read_small(reader, 5, "frame_presentation_time_length_minus_1")?;
    Ok(())
}

/// Sequence operating points.
fn parse_seq_operating_points<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if seq.reduced_header() {
        let level = read_small(reader, 5, "seq_level_idx")?;
        seq.operating.push(OperatingPoint {
            idc: 0,
            level,
            tier: false,
            decoder_model: None,
            display_delay_present: false,
            display_delay_minus1: 0,
        });
        return Ok(());
    }
    if reader.read_bit("initial_display_delay_present_flag")? {
        seq.flags |= SEQ_INITIAL_DISPLAY_PRESENT;
    }
    let count_minus1 = reader.read_bits(5, "operating_points_cnt_minus_1")?;
    for _ in 0..=count_minus1 {
        if seq.operating.len() >= MAX_OPERATING_POINTS {
            return Err(Error::parse(ParseKind::Range {
                syntax: "operating_points_cnt",
            }));
        }
        seq.operating.push(parse_operating_point(reader, seq)?);
    }
    Ok(())
}

/// One operating point against the sequence timing flags.
fn parse_operating_point<E: From<Error>>(
    reader: &mut BitReader,
    seq: &SequenceHeader,
) -> Result<OperatingPoint, E> {
    let idc = reader.read_bits(12, "operating_point_idc")?;
    let idc = u16::try_from(idc).map_err(|_| {
        Error::parse(ParseKind::Range {
            syntax: "operating_point_idc",
        })
    })?;
    let level = read_small(reader, 5, "seq_level_idx")?;
    let tier = level > 7 && reader.read_bit("seq_tier")?;
    let mut point = OperatingPoint {
        idc,
        level,
        tier,
        decoder_model: None,
        display_delay_present: false,
        display_delay_minus1: 0,
    };
    if seq.decoder_model_present() && reader.read_bit("decoder_model_present_for_this_op")? {
        let width = u32::from(seq.buffer_delay_length_minus1) + 1;
        let decoder_buffer_delay = reader.read_bits(width, "decoder_buffer_delay")?;
        let encoder_buffer_delay = reader.read_bits(width, "encoder_buffer_delay")?;
        let low_delay = reader.read_bit("low_delay_mode_flag")?;
        point.decoder_model = Some(DecoderModelPoint {
            decoder_buffer_delay,
            encoder_buffer_delay,
            low_delay,
        });
    }
    if seq.flags & SEQ_INITIAL_DISPLAY_PRESENT != 0
        && reader.read_bit("initial_display_delay_present_for_this_op")?
    {
        point.display_delay_present = true;
        point.display_delay_minus1 = read_small(reader, 4, "initial_display_delay_minus_1")?;
    }
    Ok(point)
}

/// Sequence frame size bit counts plus maximum extents.
fn parse_seq_frame_size_bits<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    seq.frame_width_bits_minus1 = read_small(reader, 4, "frame_width_bits_minus_1")?;
    seq.frame_height_bits_minus1 = read_small(reader, 4, "frame_height_bits_minus_1")?;
    seq.max_frame_width_minus1 = reader.read_bits(
        u32::from(seq.frame_width_bits_minus1) + 1,
        "max_frame_width_minus_1",
    )?;
    seq.max_frame_height_minus1 = reader.read_bits(
        u32::from(seq.frame_height_bits_minus1) + 1,
        "max_frame_height_minus_1",
    )?;
    Ok(())
}

/// Sequence frame id lengths.
fn parse_seq_frame_ids<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if seq.reduced_header() {
        return Ok(());
    }
    if !reader.read_bit("frame_id_numbers_present_flag")? {
        return Ok(());
    }
    seq.flags |= SEQ_FRAME_ID_NUMBERS;
    seq.delta_frame_id_length_minus2 = read_small(reader, 4, "delta_frame_id_length_minus_2")?;
    seq.additional_frame_id_length_minus1 =
        read_small(reader, 3, "additional_frame_id_length_minus_1")?;
    Ok(())
}

/// Sequence coding tools.
fn parse_seq_tools<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if reader.read_bit("use_128x128_superblock")? {
        seq.flags |= SEQ_SUPERBLOCK_128;
    }
    if reader.read_bit("enable_filter_intra")? {
        seq.flags |= SEQ_FILTER_INTRA;
    }
    if reader.read_bit("enable_intra_edge_filter")? {
        seq.flags |= SEQ_INTRA_EDGE;
    }
    if seq.reduced_header() {
        return Ok(());
    }
    if reader.read_bit("enable_interintra_compound")? {
        seq.flags |= SEQ_INTERINTRA;
    }
    if reader.read_bit("enable_masked_compound")? {
        seq.flags |= SEQ_MASKED_COMPOUND;
    }
    if reader.read_bit("enable_warped_motion")? {
        seq.flags |= SEQ_WARPED_MOTION;
    }
    if reader.read_bit("enable_dual_filter")? {
        seq.flags |= SEQ_DUAL_FILTER;
    }
    if reader.read_bit("enable_order_hint")? {
        seq.flags |= SEQ_ORDER_HINT;
        if reader.read_bit("enable_jnt_comp")? {
            seq.flags |= SEQ_JNT_COMP;
        }
        if reader.read_bit("enable_ref_frame_mvs")? {
            seq.flags |= SEQ_REF_MVS;
        }
    }
    parse_seq_screen_tools(reader, seq)?;
    if seq.enable_order_hint() {
        seq.order_hint_bits_minus1 = read_small(reader, 3, "order_hint_bits_minus_1")?;
    }
    Ok(())
}

/// Sequence screen content tool selection.
fn parse_seq_screen_tools<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if reader.read_bit("seq_choose_screen_content_tools")? {
        seq.flags |= SEQ_CHOOSE_SCREEN_CONTENT;
    } else {
        seq.force_screen_content_tools = read_small(reader, 1, "seq_force_screen_content_tools")?;
    }
    if seq.force_screen_content_tools == 0 {
        return Ok(());
    }
    if reader.read_bit("seq_choose_integer_mv")? {
        seq.flags |= SEQ_CHOOSE_INTEGER_MV;
        return Ok(());
    }
    seq.force_integer_mv = read_small(reader, 1, "seq_force_integer_mv")?;
    Ok(())
}

/// Sequence color config.
fn parse_color_config<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if reader.read_bit("high_bitdepth")? {
        seq.flags |= SEQ_HIGH_BITDEPTH;
    }
    if seq.profile == 2 && seq.flags & SEQ_HIGH_BITDEPTH != 0 && reader.read_bit("twelve_bit")? {
        seq.flags |= SEQ_TWELVE_BIT;
    }
    if seq.profile != 1 && reader.read_bit("mono_chrome")? {
        seq.flags |= SEQ_MONO_CHROME;
    }
    parse_color_description(reader, seq)?;
    if is_rgb(seq) {
        seq.flags |= SEQ_COLOR_RANGE;
        return parse_color_separate_uv(reader, seq);
    }
    if reader.read_bit("color_range")? {
        seq.flags |= SEQ_COLOR_RANGE;
    }
    parse_color_subsampling(reader, seq)?;
    if seq.subsampling_x() && seq.subsampling_y() {
        seq.chroma_sample_position = read_small(reader, 2, "chroma_sample_position")?;
    }
    parse_color_separate_uv(reader, seq)
}

/// True for the RGB color description with 4:4:4 inference.
fn is_rgb(seq: &SequenceHeader) -> bool {
    seq.color_primaries == 1 && seq.transfer_characteristics == 13 && seq.matrix_coefficients == 0
}

/// Sequence color description.
fn parse_color_description<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if !reader.read_bit("color_description_present_flag")? {
        return Ok(());
    }
    seq.flags |= SEQ_COLOR_DESCRIPTION;
    seq.color_primaries = read_small(reader, 8, "color_primaries")?;
    seq.transfer_characteristics = read_small(reader, 8, "transfer_characteristics")?;
    seq.matrix_coefficients = read_small(reader, 8, "matrix_coefficients")?;
    Ok(())
}

/// Sequence chroma subsampling.
fn parse_color_subsampling<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    match seq.profile {
        0 => {
            seq.flags |= SEQ_SUBSAMPLING_X | SEQ_SUBSAMPLING_Y;
        }
        1 => {}
        _ => {
            if seq.flags & SEQ_TWELVE_BIT != 0 {
                if reader.read_bit("subsampling_x")? {
                    seq.flags |= SEQ_SUBSAMPLING_X;
                    if reader.read_bit("subsampling_y")? {
                        seq.flags |= SEQ_SUBSAMPLING_Y;
                    }
                }
            } else {
                seq.flags |= SEQ_SUBSAMPLING_X;
            }
        }
    }
    Ok(())
}

/// Sequence separate UV delta quantizer flag.
fn parse_color_separate_uv<E: From<Error>>(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), E> {
    if reader.read_bit("separate_uv_delta_q")? {
        seq.flags |= SEQ_SEPARATE_UV_DELTA_Q;
    }
    Ok(())
}
