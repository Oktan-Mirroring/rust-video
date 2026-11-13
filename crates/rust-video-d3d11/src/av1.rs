//! AV1 OBU ingest: sequence and frame headers plus tile assembly.
//!
//! Reference slots behind the AV1 DPB; the DPB owns surfaces and output.

use crate::bits::BitReader;
use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason, UnsupportedReason};

/// Sequence header OBU type.
const OBU_SEQUENCE_HEADER: u8 = 1;

/// Temporal delimiter OBU type.
const OBU_TEMPORAL_DELIMITER: u8 = 2;

/// Frame header OBU type.
const OBU_FRAME_HEADER: u8 = 3;

/// Tile group OBU type.
const OBU_TILE_GROUP: u8 = 4;

/// Metadata OBU type.
const OBU_METADATA: u8 = 5;

/// Frame OBU type.
const OBU_FRAME: u8 = 6;

/// Redundant frame header OBU type.
const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

/// Tile list OBU type.
const OBU_TILE_LIST: u8 = 8;

/// Padding OBU type.
const OBU_PADDING: u8 = 15;

/// Operating point count.
pub(crate) const MAX_OPERATING_POINTS: usize = 32;

/// Reference frame count.
pub(crate) const NUM_REF_FRAMES: usize = 8;

/// References per frame.
pub(crate) const REFS_PER_FRAME: usize = 7;

/// Last reference slot.
const REF_LAST: usize = 1;

/// Second last reference slot.
const REF_LAST2: usize = 2;

/// Third last reference slot.
const REF_LAST3: usize = 3;

/// Golden reference slot.
const REF_GOLDEN: usize = 4;

/// Backward reference slot.
const REF_BWDREF: usize = 5;

/// Second alt reference slot.
const REF_ALTREF2: usize = 6;

/// Alt reference slot.
const REF_ALTREF: usize = 7;

/// Segments per frame.
pub(crate) const MAX_SEGMENTS: usize = 8;

/// Segment levels per segment.
pub(crate) const SEG_LEVELS: usize = 8;

/// Reference deltas per frame.
pub(crate) const TOTAL_REFS: usize = 8;

/// Tile column ceiling.
const MAX_TILE_COLS: u32 = 64;

/// Tile row ceiling.
const MAX_TILE_ROWS: u32 = 64;

/// Tile width ceiling in luma samples.
const MAX_TILE_WIDTH: u32 = 4096;

/// Tile area ceiling in luma samples.
const MAX_TILE_AREA: u32 = 4096 * 2304;

/// Tile grid ceiling behind the DXVA grid.
const MAX_TILES: usize = 256;

/// Superres numerator.
const SUPERRES_NUM: u8 = 8;

/// Superres denominator floor.
const SUPERRES_DENOM_MIN: u8 = 9;

/// No primary reference.
const PRIMARY_REF_NONE: u8 = 7;

/// Screen content tools selected per frame.
const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;

/// Integer motion vectors selected per frame.
const SELECT_INTEGER_MV: u8 = 2;

/// Switchable interpolation filter.
const INTERP_SWITCHABLE: u8 = 4;

/// Largest transform mode.
const TX_MODE_LARGEST: u8 = 1;

/// Transform mode selected per block.
const TX_MODE_SELECT: u8 = 2;

/// Identity warp model.
const WARP_IDENTITY: u8 = 0;

/// Translation warp model.
const WARP_TRANSLATION: u8 = 1;

/// Rotzoom warp model.
const WARP_ROTZOOM: u8 = 2;

/// Affine warp model.
const WARP_AFFINE: u8 = 3;

/// Global motion alpha absolute bits.
const GM_ABS_ALPHA_BITS: u32 = 12;

/// Global motion alpha precision bits.
const GM_ALPHA_PREC_BITS: u32 = 15;

/// Global motion translation-only absolute bits.
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;

/// Global motion translation-only precision bits.
const GM_TRANS_ONLY_PREC_BITS: u32 = 3;

/// Global motion translation absolute bits.
const GM_ABS_TRANS_BITS: u32 = 12;

/// Global motion translation precision bits.
const GM_TRANS_PREC_BITS: u32 = 6;

/// Warped model precision bits.
const WARPEDMODEL_PREC_BITS: u32 = 16;

/// Divisor lookup bits.
const DIV_LUT_BITS: u32 = 8;

/// Divisor lookup precision bits.
const DIV_LUT_PREC_BITS: u32 = 14;

/// Warp parameter reduce bits.
const WARP_PARAM_REDUCE_BITS: u32 = 6;

/// No restoration.
const RESTORE_NONE: u8 = 0;

/// Wiener restoration.
const RESTORE_WIENER: u8 = 1;

/// Self-guided restoration.
const RESTORE_SGRPROJ: u8 = 2;

/// Switchable restoration.
const RESTORE_SWITCHABLE: u8 = 3;

/// True for coded restoration types.
fn is_restoration_type(lr_type: u8) -> bool {
    matches!(
        lr_type,
        RESTORE_NONE | RESTORE_WIENER | RESTORE_SGRPROJ | RESTORE_SWITCHABLE
    )
}

/// Divisor lookup, AV1 7.11.3.7.
const DIV_LUT: [u16; 257] = [
    16384, 16320, 16257, 16194, 16132, 16070, 16009, 15948, 15888, 15828, 15768, 15709, 15650,
    15592, 15534, 15477, 15420, 15364, 15308, 15252, 15197, 15142, 15087, 15033, 14980, 14926,
    14873, 14821, 14769, 14717, 14665, 14614, 14564, 14513, 14463, 14413, 14364, 14315, 14266,
    14218, 14170, 14122, 14075, 14028, 13981, 13935, 13888, 13843, 13797, 13752, 13707, 13662,
    13618, 13574, 13530, 13487, 13443, 13400, 13358, 13315, 13273, 13231, 13190, 13148, 13107,
    13066, 13026, 12985, 12945, 12906, 12866, 12827, 12788, 12749, 12710, 12672, 12633, 12596,
    12558, 12520, 12483, 12446, 12409, 12373, 12336, 12300, 12264, 12228, 12193, 12157, 12122,
    12087, 12053, 12018, 11984, 11950, 11916, 11882, 11848, 11815, 11782, 11749, 11716, 11683,
    11651, 11619, 11586, 11555, 11523, 11491, 11460, 11429, 11398, 11367, 11336, 11305, 11275,
    11245, 11215, 11185, 11155, 11125, 11096, 11067, 11038, 11009, 10980, 10951, 10923, 10894,
    10866, 10838, 10810, 10782, 10755, 10727, 10700, 10673, 10645, 10618, 10592, 10565, 10538,
    10512, 10486, 10460, 10434, 10408, 10382, 10356, 10331, 10305, 10280, 10255, 10230, 10205,
    10180, 10156, 10131, 10107, 10082, 10058, 10034, 10010, 9986, 9963, 9939, 9916, 9892, 9869,
    9846, 9823, 9800, 9777, 9754, 9732, 9709, 9687, 9664, 9642, 9620, 9598, 9576, 9554, 9533, 9511,
    9489, 9468, 9447, 9425, 9404, 9383, 9362, 9341, 9321, 9300, 9279, 9259, 9239, 9218, 9198, 9178,
    9158, 9138, 9118, 9098, 9079, 9059, 9039, 9020, 9001, 8981, 8962, 8943, 8924, 8905, 8886, 8867,
    8849, 8830, 8812, 8793, 8775, 8756, 8738, 8720, 8702, 8684, 8666, 8648, 8630, 8613, 8595, 8577,
    8560, 8542, 8525, 8508, 8490, 8473, 8456, 8439, 8422, 8405, 8389, 8372, 8355, 8339, 8322, 8306,
    8289, 8273, 8257, 8240, 8224, 8208, 8192,
];

/// Unsigned variable-length code.
fn read_uvlc(reader: &mut BitReader, syntax: &'static str) -> Result<u32, D3d11Error> {
    let mut zeroes = 0u32;
    while !reader.read_bit(syntax)? {
        zeroes += 1;
        if zeroes >= 32 {
            return Err(D3d11Error::Parse(ParseReason::OverflowU32 { syntax }));
        }
    }
    if zeroes == 0 {
        return Ok(0);
    }
    let rest = reader.read_bits(zeroes, syntax)?;
    rest.checked_add((1u32 << zeroes) - 1)
        .ok_or(D3d11Error::Parse(ParseReason::OverflowU32 { syntax }))
}

/// Little-endian base-128 integer below the u32 ceiling.
fn read_leb128(reader: &mut BitReader, syntax: &'static str) -> Result<u32, D3d11Error> {
    let mut value: u64 = 0;
    for index in 0..8 {
        let byte = u64::from(reader.read_bits(8, syntax)?);
        value |= (byte & 0x7F) << (index * 7);
        if byte & 0x80 == 0 {
            return u32::try_from(value)
                .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }));
        }
    }
    Err(D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Nonsymmetric value below `n`.
fn read_ns(reader: &mut BitReader, n: u32, syntax: &'static str) -> Result<u32, D3d11Error> {
    if n <= 1 {
        return Ok(0);
    }
    let width = 32 - (n - 1).leading_zeros();
    let m = (1u32 << width) - n;
    let value = if width > 1 {
        reader.read_bits(width - 1, syntax)?
    } else {
        0
    };
    if value < m {
        return Ok(value);
    }
    let extra = u32::from(reader.read_bit(syntax)?);
    Ok((value << 1) - m + extra)
}

/// Unary value between `min` and `max`.
fn read_increment(
    reader: &mut BitReader,
    min: u32,
    max: u32,
    syntax: &'static str,
) -> Result<u32, D3d11Error> {
    let mut value = min;
    while value < max {
        if reader.read_bit(syntax)? {
            value += 1;
        } else {
            break;
        }
    }
    Ok(value)
}

/// Subexponential value below `max`.
fn read_subexp(reader: &mut BitReader, max: u32, syntax: &'static str) -> Result<u32, D3d11Error> {
    if max < 9 {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax }));
    }
    let max_len = 32 - (max - 1).leading_zeros() - 3 - 1;
    let len = read_increment(reader, 0, max_len, syntax)?;
    let (bits, offset) = if len > 0 {
        (2 + len, 1u32 << (2 + len))
    } else {
        (3, 0)
    };
    let value = if len < max_len {
        reader.read_bits(bits, syntax)?
    } else {
        read_ns(reader, max - offset, syntax)?
    };
    value
        .checked_add(offset)
        .ok_or(D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Signed two's complement value of `width` bits.
fn read_su(reader: &mut BitReader, width: u32, syntax: &'static str) -> Result<i32, D3d11Error> {
    if width == 0 || width > 32 {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax }));
    }
    let raw = reader.read_bits(width, syntax)?;
    Ok((raw << (32 - width)).cast_signed() >> (32 - width))
}

/// Fixed-width value below 256.
fn read_small(reader: &mut BitReader, width: u32, syntax: &'static str) -> Result<u8, D3d11Error> {
    let value = reader.read_bits(width, syntax)?;
    u8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Smallest shift with `block_size` shifted past `target`.
fn tile_log2(block_size: u32, target: u32) -> u32 {
    let mut shift = 0u32;
    while u64::from(block_size) << shift < u64::from(target) {
        shift += 1;
    }
    shift
}

/// Relative order hint distance of `a` behind `b`.
fn relative_dist(a: u8, b: u8, order_hint_bits_minus1: u8) -> i32 {
    let diff = u32::from(a).wrapping_sub(u32::from(b));
    let m = 1u32 << order_hint_bits_minus1;
    (diff & (m - 1)).wrapping_sub(diff & m).cast_signed()
}

/// Still picture flag bit.
pub(crate) const SEQ_STILL_PICTURE: u32 = 1;
/// Reduced still picture header flag bit.
pub(crate) const SEQ_REDUCED_HEADER: u32 = 1 << 1;
/// Timing info present flag bit.
pub(crate) const SEQ_TIMING_PRESENT: u32 = 1 << 2;
/// Decoder model info present flag bit.
pub(crate) const SEQ_DECODER_MODEL_PRESENT: u32 = 1 << 3;
/// Initial display delay present flag bit.
pub(crate) const SEQ_INITIAL_DISPLAY_PRESENT: u32 = 1 << 4;
/// Frame id numbers present flag bit.
pub(crate) const SEQ_FRAME_ID_NUMBERS: u32 = 1 << 5;
/// 128x128 superblocks flag bit.
pub(crate) const SEQ_SUPERBLOCK_128: u32 = 1 << 6;
/// Filter intra flag bit.
pub(crate) const SEQ_FILTER_INTRA: u32 = 1 << 7;
/// Intra edge filter flag bit.
pub(crate) const SEQ_INTRA_EDGE: u32 = 1 << 8;
/// Interintra compound flag bit.
pub(crate) const SEQ_INTERINTRA: u32 = 1 << 9;
/// Masked compound flag bit.
pub(crate) const SEQ_MASKED_COMPOUND: u32 = 1 << 10;
/// Warped motion flag bit.
pub(crate) const SEQ_WARPED_MOTION: u32 = 1 << 11;
/// Dual filter flag bit.
pub(crate) const SEQ_DUAL_FILTER: u32 = 1 << 12;
/// Order hint flag bit.
pub(crate) const SEQ_ORDER_HINT: u32 = 1 << 13;
/// Joint compound flag bit.
pub(crate) const SEQ_JNT_COMP: u32 = 1 << 14;
/// Reference motion vectors flag bit.
pub(crate) const SEQ_REF_MVS: u32 = 1 << 15;
/// Screen content tools selectable flag bit.
pub(crate) const SEQ_CHOOSE_SCREEN_CONTENT: u32 = 1 << 16;
/// Integer motion vectors selectable flag bit.
pub(crate) const SEQ_CHOOSE_INTEGER_MV: u32 = 1 << 17;
/// Superres flag bit.
pub(crate) const SEQ_SUPERRES: u32 = 1 << 18;
/// CDEF flag bit.
pub(crate) const SEQ_CDEF: u32 = 1 << 19;
/// Restoration flag bit.
pub(crate) const SEQ_RESTORATION: u32 = 1 << 20;
/// Film grain present flag bit.
pub(crate) const SEQ_FILM_GRAIN: u32 = 1 << 21;
/// High bit depth flag bit.
pub(crate) const SEQ_HIGH_BITDEPTH: u32 = 1 << 22;
/// Twelve bit flag bit.
pub(crate) const SEQ_TWELVE_BIT: u32 = 1 << 23;
/// Monochrome flag bit.
pub(crate) const SEQ_MONO_CHROME: u32 = 1 << 24;
/// Color range flag bit.
pub(crate) const SEQ_COLOR_RANGE: u32 = 1 << 26;
/// Separate UV delta quantizer flag bit.
pub(crate) const SEQ_SEPARATE_UV_DELTA_Q: u32 = 1 << 27;
/// Horizontal subsampling flag bit.
pub(crate) const SEQ_SUBSAMPLING_X: u32 = 1 << 28;
/// Vertical subsampling flag bit.
pub(crate) const SEQ_SUBSAMPLING_Y: u32 = 1 << 29;
/// Equal picture interval flag bit.
pub(crate) const SEQ_EQUAL_INTERVAL: u32 = 1 << 30;

/// Operating point behind decoder model and display timing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperatingPoint {
    /// Operating point idc.
    pub(crate) idc: u16,
    /// Level index.
    pub(crate) level: u8,
    /// High tier.
    pub(crate) tier: bool,
    /// Decoder model present for this point.
    pub(crate) decoder_model_present: bool,
    /// Display delay present for this point.
    pub(crate) display_delay_present: bool,
    /// Display delay minus one.
    pub(crate) display_delay_minus1: u8,
}

/// Parsed sequence header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SequenceHeader {
    /// Sequence profile.
    pub(crate) profile: u8,
    /// Packed tool and color flags.
    pub(crate) flags: u32,
    /// Operating points.
    pub(crate) operating: Vec<OperatingPoint>,
    /// Buffer delay length minus one.
    pub(crate) buffer_delay_length_minus1: u8,
    /// Buffer removal length minus one.
    pub(crate) buffer_removal_length_minus1: u8,
    /// Presentation time length minus one.
    pub(crate) presentation_length_minus1: u8,
    /// Frame width bit count minus one.
    pub(crate) frame_width_bits_minus1: u8,
    /// Frame height bit count minus one.
    pub(crate) frame_height_bits_minus1: u8,
    /// Maximum frame width minus one.
    pub(crate) max_frame_width_minus1: u32,
    /// Maximum frame height minus one.
    pub(crate) max_frame_height_minus1: u32,
    /// Frame id delta length minus two.
    pub(crate) delta_frame_id_length_minus2: u8,
    /// Additional frame id length minus one.
    pub(crate) additional_frame_id_length_minus1: u8,
    /// Forced screen content tools.
    pub(crate) force_screen_content_tools: u8,
    /// Forced integer motion vectors.
    pub(crate) force_integer_mv: u8,
    /// Order hint bit count minus one.
    pub(crate) order_hint_bits_minus1: u8,
    /// Color primaries.
    pub(crate) color_primaries: u8,
    /// Transfer characteristics.
    pub(crate) transfer_characteristics: u8,
    /// Matrix coefficients.
    pub(crate) matrix_coefficients: u8,
    /// Chroma sample position.
    pub(crate) chroma_sample_position: u8,
}

impl SequenceHeader {
    /// Reduced still picture header.
    pub(crate) const fn reduced_header(&self) -> bool {
        self.flags & SEQ_REDUCED_HEADER != 0
    }

    /// Decoder model info present.
    pub(crate) const fn decoder_model_present(&self) -> bool {
        self.flags & SEQ_DECODER_MODEL_PRESENT != 0
    }

    /// Frame id numbers present.
    pub(crate) const fn frame_id_numbers_present(&self) -> bool {
        self.flags & SEQ_FRAME_ID_NUMBERS != 0
    }

    /// 128x128 superblocks.
    pub(crate) const fn use_128x128_superblock(&self) -> bool {
        self.flags & SEQ_SUPERBLOCK_128 != 0
    }

    /// Filter intra.
    pub(crate) const fn enable_filter_intra(&self) -> bool {
        self.flags & SEQ_FILTER_INTRA != 0
    }

    /// Intra edge filter.
    pub(crate) const fn enable_intra_edge_filter(&self) -> bool {
        self.flags & SEQ_INTRA_EDGE != 0
    }

    /// Interintra compound.
    pub(crate) const fn enable_interintra_compound(&self) -> bool {
        self.flags & SEQ_INTERINTRA != 0
    }

    /// Masked compound.
    pub(crate) const fn enable_masked_compound(&self) -> bool {
        self.flags & SEQ_MASKED_COMPOUND != 0
    }

    /// Warped motion.
    pub(crate) const fn enable_warped_motion(&self) -> bool {
        self.flags & SEQ_WARPED_MOTION != 0
    }

    /// Dual filter.
    pub(crate) const fn enable_dual_filter(&self) -> bool {
        self.flags & SEQ_DUAL_FILTER != 0
    }

    /// Order hint.
    pub(crate) const fn enable_order_hint(&self) -> bool {
        self.flags & SEQ_ORDER_HINT != 0
    }

    /// Joint compound.
    pub(crate) const fn enable_jnt_comp(&self) -> bool {
        self.flags & SEQ_JNT_COMP != 0
    }

    /// Reference motion vectors.
    pub(crate) const fn enable_ref_frame_mvs(&self) -> bool {
        self.flags & SEQ_REF_MVS != 0
    }

    /// Superres.
    pub(crate) const fn enable_superres(&self) -> bool {
        self.flags & SEQ_SUPERRES != 0
    }

    /// CDEF.
    pub(crate) const fn enable_cdef(&self) -> bool {
        self.flags & SEQ_CDEF != 0
    }

    /// Restoration.
    pub(crate) const fn enable_restoration(&self) -> bool {
        self.flags & SEQ_RESTORATION != 0
    }

    /// Film grain parameters present.
    pub(crate) const fn film_grain_params_present(&self) -> bool {
        self.flags & SEQ_FILM_GRAIN != 0
    }

    /// Separate UV delta quantizer.
    pub(crate) const fn separate_uv_delta_q(&self) -> bool {
        self.flags & SEQ_SEPARATE_UV_DELTA_Q != 0
    }

    /// Horizontal subsampling.
    pub(crate) const fn subsampling_x(&self) -> bool {
        self.flags & SEQ_SUBSAMPLING_X != 0
    }

    /// Vertical subsampling.
    pub(crate) const fn subsampling_y(&self) -> bool {
        self.flags & SEQ_SUBSAMPLING_Y != 0
    }

    /// Monochrome.
    pub(crate) const fn mono_chrome(&self) -> bool {
        self.flags & SEQ_MONO_CHROME != 0
    }

    /// Equal picture interval.
    pub(crate) const fn equal_picture_interval(&self) -> bool {
        self.flags & SEQ_EQUAL_INTERVAL != 0
    }

    /// Bit depth from profile and color flags.
    pub(crate) const fn bit_depth(&self) -> u8 {
        if self.flags & SEQ_HIGH_BITDEPTH == 0 {
            8
        } else if self.profile == 2 && self.flags & SEQ_TWELVE_BIT != 0 {
            12
        } else {
            10
        }
    }

    /// Plane count from the monochrome flag.
    pub(crate) const fn num_planes(&self) -> u8 {
        if self.flags & SEQ_MONO_CHROME != 0 {
            1
        } else {
            3
        }
    }

    /// Maximum frame width in luma samples.
    pub(crate) const fn max_frame_width(&self) -> u32 {
        self.max_frame_width_minus1 + 1
    }

    /// Maximum frame height in luma samples.
    pub(crate) const fn max_frame_height(&self) -> u32 {
        self.max_frame_height_minus1 + 1
    }

    /// Order hint bit count.
    pub(crate) const fn order_hint_bits(&self) -> u8 {
        if self.flags & SEQ_ORDER_HINT != 0 {
            self.order_hint_bits_minus1 + 1
        } else {
            0
        }
    }

    /// Frame id bit count.
    pub(crate) const fn id_len(&self) -> u8 {
        self.additional_frame_id_length_minus1 + self.delta_frame_id_length_minus2 + 3
    }
}

/// Sequence header from OBU `payload`.
pub(crate) fn parse_sequence_header(payload: &[u8]) -> Result<SequenceHeader, D3d11Error> {
    let mut reader = BitReader::new(payload);
    let profile = reader.read_bits(3, "seq_profile")?;
    if profile > 2 {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "seq_profile",
        }));
    }
    let profile = u8::try_from(profile).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
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
        buffer_delay_length_minus1: 0,
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
fn parse_seq_timing(reader: &mut BitReader, seq: &mut SequenceHeader) -> Result<(), D3d11Error> {
    if seq.reduced_header() {
        return Ok(());
    }
    if !reader.read_bit("timing_info_present_flag")? {
        return Ok(());
    }
    seq.flags |= SEQ_TIMING_PRESENT;
    reader.read_bits(32, "num_units_in_display_tick")?;
    reader.read_bits(32, "time_scale")?;
    if reader.read_bit("equal_picture_interval")? {
        seq.flags |= SEQ_EQUAL_INTERVAL;
        read_uvlc(reader, "num_ticks_per_picture_minus1")?;
    }
    if !reader.read_bit("decoder_model_info_present_flag")? {
        return Ok(());
    }
    seq.flags |= SEQ_DECODER_MODEL_PRESENT;
    seq.buffer_delay_length_minus1 = read_small(reader, 5, "buffer_delay_length_minus_1")?;
    reader.read_bits(32, "num_units_in_decoding_tick")?;
    seq.buffer_removal_length_minus1 = read_small(reader, 5, "buffer_removal_time_length_minus_1")?;
    seq.presentation_length_minus1 =
        read_small(reader, 5, "frame_presentation_time_length_minus_1")?;
    Ok(())
}

/// Sequence operating points.
fn parse_seq_operating_points(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), D3d11Error> {
    if seq.reduced_header() {
        let level = read_small(reader, 5, "seq_level_idx")?;
        seq.operating.push(OperatingPoint {
            idc: 0,
            level,
            tier: false,
            decoder_model_present: false,
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
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "operating_points_cnt",
            }));
        }
        seq.operating.push(parse_operating_point(reader, seq)?);
    }
    Ok(())
}

/// One operating point against the sequence timing flags.
fn parse_operating_point(
    reader: &mut BitReader,
    seq: &SequenceHeader,
) -> Result<OperatingPoint, D3d11Error> {
    let idc = reader.read_bits(12, "operating_point_idc")?;
    let idc = u16::try_from(idc).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "operating_point_idc",
        })
    })?;
    let level = read_small(reader, 5, "seq_level_idx")?;
    let tier = level > 7 && reader.read_bit("seq_tier")?;
    let mut point = OperatingPoint {
        idc,
        level,
        tier,
        decoder_model_present: false,
        display_delay_present: false,
        display_delay_minus1: 0,
    };
    if seq.decoder_model_present() && reader.read_bit("decoder_model_present_for_this_op")? {
        point.decoder_model_present = true;
        let width = u32::from(seq.buffer_delay_length_minus1) + 1;
        reader.read_bits(width, "decoder_buffer_delay")?;
        reader.read_bits(width, "encoder_buffer_delay")?;
        reader.read_bit("low_delay_mode_flag")?;
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
fn parse_seq_frame_size_bits(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), D3d11Error> {
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
fn parse_seq_frame_ids(reader: &mut BitReader, seq: &mut SequenceHeader) -> Result<(), D3d11Error> {
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
fn parse_seq_tools(reader: &mut BitReader, seq: &mut SequenceHeader) -> Result<(), D3d11Error> {
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
fn parse_seq_screen_tools(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), D3d11Error> {
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

/// Sequence color config with 8-bit 4:2:0 gating.
fn parse_color_config(reader: &mut BitReader, seq: &mut SequenceHeader) -> Result<(), D3d11Error> {
    if reader.read_bit("high_bitdepth")? {
        seq.flags |= SEQ_HIGH_BITDEPTH;
    }
    if seq.profile == 2 && seq.flags & SEQ_HIGH_BITDEPTH != 0 && reader.read_bit("twelve_bit")? {
        seq.flags |= SEQ_TWELVE_BIT;
    }
    if seq.bit_depth() == 12 {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::Av1BitDepthNeedsTarget { depth: 12 },
        ));
    }
    if seq.bit_depth() == 10 {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::Av1BitDepthNeedsTarget { depth: 10 },
        ));
    }
    if seq.profile != 1 && reader.read_bit("mono_chrome")? {
        seq.flags |= SEQ_MONO_CHROME;
    }
    if seq.flags & SEQ_MONO_CHROME != 0 {
        return Err(D3d11Error::Unsupported(UnsupportedReason::Av1Monochrome));
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
fn parse_color_description(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), D3d11Error> {
    if !reader.read_bit("color_description_present_flag")? {
        return Ok(());
    }
    seq.color_primaries = read_small(reader, 8, "color_primaries")?;
    seq.transfer_characteristics = read_small(reader, 8, "transfer_characteristics")?;
    seq.matrix_coefficients = read_small(reader, 8, "matrix_coefficients")?;
    Ok(())
}

/// Sequence chroma subsampling with 4:2:0 gating.
fn parse_color_subsampling(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), D3d11Error> {
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
    if !seq.subsampling_x() || !seq.subsampling_y() {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::Av1ChromaOutside420,
        ));
    }
    Ok(())
}

/// Sequence separate UV delta quantizer flag.
fn parse_color_separate_uv(
    reader: &mut BitReader,
    seq: &mut SequenceHeader,
) -> Result<(), D3d11Error> {
    if reader.read_bit("separate_uv_delta_q")? {
        seq.flags |= SEQ_SEPARATE_UV_DELTA_Q;
    }
    Ok(())
}

/// Frame type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FrameType {
    /// Key frame.
    #[default]
    Key,
    /// Inter frame.
    Inter,
    /// Intra-only frame.
    IntraOnly,
    /// Switch frame.
    Switch,
}

impl FrameType {
    /// Type for the `frame_type` value.
    fn new(value: u32) -> Result<Self, D3d11Error> {
        match value {
            0 => Ok(Self::Key),
            1 => Ok(Self::Inter),
            2 => Ok(Self::IntraOnly),
            3 => Ok(Self::Switch),
            _ => Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "frame_type",
            })),
        }
    }

    /// True for key and intra-only frames.
    pub(crate) const fn is_intra(self) -> bool {
        matches!(self, Self::Key | Self::IntraOnly)
    }

    /// DXVA frame type number.
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::Key => 0,
            Self::Inter => 1,
            Self::IntraOnly => 2,
            Self::Switch => 3,
        }
    }
}

/// Show existing frame flag bit.
pub(crate) const FH_SHOW_EXISTING: u32 = 1;
/// Show frame flag bit.
pub(crate) const FH_SHOW_FRAME: u32 = 1 << 1;
/// Showable frame flag bit.
pub(crate) const FH_SHOWABLE_FRAME: u32 = 1 << 2;
/// Error resilient mode flag bit.
pub(crate) const FH_ERROR_RESILIENT: u32 = 1 << 3;
/// CDF update disabled flag bit.
pub(crate) const FH_DISABLE_CDF_UPDATE: u32 = 1 << 4;
/// Screen content tools flag bit.
pub(crate) const FH_SCREEN_CONTENT_TOOLS: u32 = 1 << 5;
/// Integer motion vectors flag bit.
pub(crate) const FH_FORCE_INTEGER_MV: u32 = 1 << 6;
/// Frame size override flag bit.
pub(crate) const FH_SIZE_OVERRIDE: u32 = 1 << 7;
/// Short reference signaling flag bit.
pub(crate) const FH_SHORT_SIGNALING: u32 = 1 << 8;
/// Render size differs flag bit.
pub(crate) const FH_RENDER_DIFFERENT: u32 = 1 << 9;
/// Intra block copy flag bit.
pub(crate) const FH_ALLOW_INTRABC: u32 = 1 << 10;
/// High precision motion vectors flag bit.
pub(crate) const FH_HIGH_PRECISION_MV: u32 = 1 << 11;
/// Switchable motion mode flag bit.
pub(crate) const FH_MOTION_MODE_SWITCHABLE: u32 = 1 << 12;
/// Reference motion vectors flag bit.
pub(crate) const FH_USE_REF_MVS: u32 = 1 << 13;
/// Frame-end CDF update disabled flag bit.
pub(crate) const FH_DISABLE_FRAME_END_UPDATE_CDF: u32 = 1 << 14;
/// UV delta differs flag bit.
pub(crate) const FH_DIFF_UV_DELTA: u32 = 1 << 16;
/// Quantizer matrix flag bit.
pub(crate) const FH_USING_QMATRIX: u32 = 1 << 17;
/// Segmentation enabled flag bit.
pub(crate) const FH_SEG_ENABLED: u32 = 1 << 18;
/// Segmentation map update flag bit.
pub(crate) const FH_SEG_UPDATE_MAP: u32 = 1 << 19;
/// Segmentation data update flag bit.
pub(crate) const FH_SEG_UPDATE_DATA: u32 = 1 << 20;
/// Segmentation temporal update flag bit.
pub(crate) const FH_SEG_TEMPORAL_UPDATE: u32 = 1 << 21;
/// Delta quantizer present flag bit.
pub(crate) const FH_DELTA_Q_PRESENT: u32 = 1 << 22;
/// Delta loop filter present flag bit.
pub(crate) const FH_DELTA_LF_PRESENT: u32 = 1 << 23;
/// Multi-delta loop filter flag bit.
pub(crate) const FH_DELTA_LF_MULTI: u32 = 1 << 24;
/// Loop filter delta enabled flag bit.
pub(crate) const FH_LOOP_DELTA_ENABLED: u32 = 1 << 25;
/// Loop filter delta update flag bit.
pub(crate) const FH_LOOP_DELTA_UPDATE: u32 = 1 << 26;
/// Compound reference mode flag bit.
pub(crate) const FH_REFERENCE_SELECT: u32 = 1 << 27;
/// Skip mode present flag bit.
pub(crate) const FH_SKIP_MODE_PRESENT: u32 = 1 << 28;
/// Warped motion flag bit.
pub(crate) const FH_ALLOW_WARPED_MOTION: u32 = 1 << 29;
/// Reduced transform set flag bit.
pub(crate) const FH_REDUCED_TX_SET: u32 = 1 << 30;

/// Global motion behind one reference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct GlobalMotion {
    /// Warp model.
    pub(crate) warp_type: u8,
    /// Warp matrix.
    pub(crate) params: [i32; 6],
    /// Parameters invalid.
    pub(crate) invalid: bool,
}

/// Film grain applied flag bit.
pub(crate) const FG_APPLY: u8 = 1;
/// Film grain chroma from luma flag bit.
pub(crate) const FG_CHROMA_FROM_LUMA: u8 = 1 << 1;
/// Film grain overlap flag bit.
pub(crate) const FG_OVERLAP: u8 = 1 << 2;
/// Film grain restricted range flag bit.
pub(crate) const FG_RESTRICTED_RANGE: u8 = 1 << 3;

/// Film grain parameters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FilmGrain {
    /// Packed grain flags.
    pub(crate) flags: u8,
    /// Grain seed.
    pub(crate) grain_seed: u16,
    /// Y scaling points.
    pub(crate) scaling_points_y: [[u8; 2]; 14],
    /// Y scaling point count.
    pub(crate) num_y_points: u8,
    /// Cb scaling points.
    pub(crate) scaling_points_cb: [[u8; 2]; 10],
    /// Cb scaling point count.
    pub(crate) num_cb_points: u8,
    /// Cr scaling points.
    pub(crate) scaling_points_cr: [[u8; 2]; 10],
    /// Cr scaling point count.
    pub(crate) num_cr_points: u8,
    /// Y autoregressive coefficients.
    pub(crate) ar_coeffs_y: [u8; 24],
    /// Cb autoregressive coefficients.
    pub(crate) ar_coeffs_cb: [u8; 25],
    /// Cr autoregressive coefficients.
    pub(crate) ar_coeffs_cr: [u8; 25],
    /// Grain scaling minus 8.
    pub(crate) grain_scaling_minus_8: u8,
    /// Coefficient lag.
    pub(crate) ar_coeff_lag: u8,
    /// Coefficient shift minus 6.
    pub(crate) ar_coeff_shift_minus_6: u8,
    /// Grain scale shift.
    pub(crate) grain_scale_shift: u8,
    /// Cb multiplier.
    pub(crate) cb_mult: u8,
    /// Cb luma multiplier.
    pub(crate) cb_luma_mult: u8,
    /// Cr multiplier.
    pub(crate) cr_mult: u8,
    /// Cr luma multiplier.
    pub(crate) cr_luma_mult: u8,
    /// Cb offset.
    pub(crate) cb_offset: u16,
    /// Cr offset.
    pub(crate) cr_offset: u16,
}

impl FilmGrain {
    /// Grain applied.
    pub(crate) const fn apply_grain(&self) -> bool {
        self.flags & FG_APPLY != 0
    }

    /// Chroma scaling from luma.
    pub(crate) const fn chroma_scaling_from_luma(&self) -> bool {
        self.flags & FG_CHROMA_FROM_LUMA != 0
    }

    /// Overlap flag.
    pub(crate) const fn overlap_flag(&self) -> bool {
        self.flags & FG_OVERLAP != 0
    }

    /// Restricted range flag.
    pub(crate) const fn clip_to_restricted_range(&self) -> bool {
        self.flags & FG_RESTRICTED_RANGE != 0
    }
}

/// Parsed frame header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrameHeader {
    /// Packed frame flags.
    pub(crate) flags: u32,
    /// Frame type.
    pub(crate) frame_type: FrameType,
    /// Shown slot for existing-frame display.
    pub(crate) frame_to_show_map_idx: u8,
    /// Display frame id.
    pub(crate) display_frame_id: u32,
    /// Current frame id.
    pub(crate) current_frame_id: u32,
    /// Frame order hint.
    pub(crate) order_hint: u8,
    /// Primary reference frame.
    pub(crate) primary_ref_frame: u8,
    /// Refresh flags per slot.
    pub(crate) refresh_frame_flags: u8,
    /// Reference slot per position.
    pub(crate) ref_frame_idx: [u8; REFS_PER_FRAME],
    /// Reference order hints.
    pub(crate) ref_order_hint: [u8; NUM_REF_FRAMES],
    /// Coded frame width in luma samples.
    pub(crate) frame_width: u32,
    /// Coded frame height in luma samples.
    pub(crate) frame_height: u32,
    /// Upscaled width in luma samples.
    pub(crate) upscaled_width: u32,
    /// Render width in luma samples.
    pub(crate) render_width: u32,
    /// Render height in luma samples.
    pub(crate) render_height: u32,
    /// Superres denominator.
    pub(crate) superres_denom: u8,
    /// Tile column count.
    pub(crate) tile_cols: u8,
    /// Tile row count.
    pub(crate) tile_rows: u8,
    /// Tile column shift.
    pub(crate) tile_cols_log2: u8,
    /// Tile row shift.
    pub(crate) tile_rows_log2: u8,
    /// Context update tile id.
    pub(crate) context_update_tile_id: u16,
    /// Tile size field width in bytes.
    pub(crate) tile_size_bytes: u8,
    /// Tile column widths in superblocks minus one.
    pub(crate) width_in_sbs_minus_1: [u16; 64],
    /// Tile row heights in superblocks minus one.
    pub(crate) height_in_sbs_minus_1: [u16; 64],
    /// Base quantizer index.
    pub(crate) base_q_idx: u8,
    /// Y DC delta.
    pub(crate) delta_q_y_dc: i8,
    /// U DC delta.
    pub(crate) delta_q_u_dc: i8,
    /// U AC delta.
    pub(crate) delta_q_u_ac: i8,
    /// V DC delta.
    pub(crate) delta_q_v_dc: i8,
    /// V AC delta.
    pub(crate) delta_q_v_ac: i8,
    /// Y matrix id.
    pub(crate) qm_y: u8,
    /// U matrix id.
    pub(crate) qm_u: u8,
    /// V matrix id.
    pub(crate) qm_v: u8,
    /// Delta quantizer resolution.
    pub(crate) delta_q_res: u8,
    /// Delta loop filter resolution.
    pub(crate) delta_lf_res: u8,
    /// Loop filter levels.
    pub(crate) loop_filter_level: [u8; 4],
    /// Loop filter sharpness.
    pub(crate) loop_filter_sharpness: u8,
    /// Loop filter reference deltas.
    pub(crate) loop_ref_deltas: [i8; TOTAL_REFS],
    /// Loop filter mode deltas.
    pub(crate) loop_mode_deltas: [i8; 2],
    /// Segmentation feature enables per segment and level.
    pub(crate) seg_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Segmentation feature values per segment and level.
    pub(crate) seg_value: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
    /// CDEF damping minus 3.
    pub(crate) cdef_damping_minus_3: u8,
    /// CDEF strength bits.
    pub(crate) cdef_bits: u8,
    /// CDEF Y primary strengths.
    pub(crate) cdef_y_pri: [u8; 8],
    /// CDEF Y secondary strengths.
    pub(crate) cdef_y_sec: [u8; 8],
    /// CDEF UV primary strengths.
    pub(crate) cdef_uv_pri: [u8; 8],
    /// CDEF UV secondary strengths.
    pub(crate) cdef_uv_sec: [u8; 8],
    /// Restoration types per plane.
    pub(crate) lr_type: [u8; 3],
    /// Restoration unit shift.
    pub(crate) lr_unit_shift: u8,
    /// Restoration chroma shift.
    pub(crate) lr_uv_shift: u8,
    /// Transform mode.
    pub(crate) tx_mode: u8,
    /// Interpolation filter.
    pub(crate) interp_filter: u8,
    /// Global motion per reference.
    pub(crate) global_motion: [GlobalMotion; REFS_PER_FRAME],
    /// Film grain parameters.
    pub(crate) film_grain: FilmGrain,
}

impl Default for FrameHeader {
    /// Empty header with key type and default tools.
    fn default() -> Self {
        Self {
            flags: 0,
            frame_type: FrameType::Key,
            frame_to_show_map_idx: 0,
            display_frame_id: 0,
            current_frame_id: 0,
            order_hint: 0,
            primary_ref_frame: PRIMARY_REF_NONE,
            refresh_frame_flags: 0,
            ref_frame_idx: [0; REFS_PER_FRAME],
            ref_order_hint: [0; NUM_REF_FRAMES],
            frame_width: 0,
            frame_height: 0,
            upscaled_width: 0,
            render_width: 0,
            render_height: 0,
            superres_denom: SUPERRES_NUM,
            tile_cols: 0,
            tile_rows: 0,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
            context_update_tile_id: 0,
            tile_size_bytes: 1,
            width_in_sbs_minus_1: [0; 64],
            height_in_sbs_minus_1: [0; 64],
            base_q_idx: 0,
            delta_q_y_dc: 0,
            delta_q_u_dc: 0,
            delta_q_u_ac: 0,
            delta_q_v_dc: 0,
            delta_q_v_ac: 0,
            qm_y: 0,
            qm_u: 0,
            qm_v: 0,
            delta_q_res: 0,
            delta_lf_res: 0,
            loop_filter_level: [0; 4],
            loop_filter_sharpness: 0,
            loop_ref_deltas: [0; TOTAL_REFS],
            loop_mode_deltas: [0; 2],
            seg_enabled: [[false; SEG_LEVELS]; MAX_SEGMENTS],
            seg_value: [[0; SEG_LEVELS]; MAX_SEGMENTS],
            cdef_damping_minus_3: 0,
            cdef_bits: 0,
            cdef_y_pri: [0; 8],
            cdef_y_sec: [0; 8],
            cdef_uv_pri: [0; 8],
            cdef_uv_sec: [0; 8],
            lr_type: [RESTORE_NONE; 3],
            lr_unit_shift: 0,
            lr_uv_shift: 0,
            tx_mode: 0,
            interp_filter: 0,
            global_motion: [GlobalMotion {
                warp_type: WARP_IDENTITY,
                params: [
                    0,
                    0,
                    1 << WARPEDMODEL_PREC_BITS,
                    0,
                    0,
                    1 << WARPEDMODEL_PREC_BITS,
                ],
                invalid: true,
            }; REFS_PER_FRAME],
            film_grain: FilmGrain::default(),
        }
    }
}

impl FrameHeader {
    /// Show existing frame display.
    pub(crate) const fn show_existing_frame(&self) -> bool {
        self.flags & FH_SHOW_EXISTING != 0
    }

    /// Show frame display.
    pub(crate) const fn show_frame(&self) -> bool {
        self.flags & FH_SHOW_FRAME != 0
    }

    /// Showable frame.
    pub(crate) const fn showable_frame(&self) -> bool {
        self.flags & FH_SHOWABLE_FRAME != 0
    }

    /// Error resilient mode.
    pub(crate) const fn error_resilient_mode(&self) -> bool {
        self.flags & FH_ERROR_RESILIENT != 0
    }

    /// CDF update disabled.
    pub(crate) const fn disable_cdf_update(&self) -> bool {
        self.flags & FH_DISABLE_CDF_UPDATE != 0
    }

    /// Screen content tools.
    pub(crate) const fn allow_screen_content_tools(&self) -> bool {
        self.flags & FH_SCREEN_CONTENT_TOOLS != 0
    }

    /// Integer motion vectors.
    pub(crate) const fn force_integer_mv(&self) -> bool {
        self.flags & FH_FORCE_INTEGER_MV != 0
    }

    /// Frame size override.
    pub(crate) const fn frame_size_override_flag(&self) -> bool {
        self.flags & FH_SIZE_OVERRIDE != 0
    }

    /// Short reference signaling.
    pub(crate) const fn frame_refs_short_signaling(&self) -> bool {
        self.flags & FH_SHORT_SIGNALING != 0
    }

    /// Render size differs.
    pub(crate) const fn render_and_frame_size_different(&self) -> bool {
        self.flags & FH_RENDER_DIFFERENT != 0
    }

    /// Intra block copy.
    pub(crate) const fn allow_intrabc(&self) -> bool {
        self.flags & FH_ALLOW_INTRABC != 0
    }

    /// High precision motion vectors.
    pub(crate) const fn allow_high_precision_mv(&self) -> bool {
        self.flags & FH_HIGH_PRECISION_MV != 0
    }

    /// Switchable motion mode.
    pub(crate) const fn is_motion_mode_switchable(&self) -> bool {
        self.flags & FH_MOTION_MODE_SWITCHABLE != 0
    }

    /// Reference motion vectors.
    pub(crate) const fn use_ref_frame_mvs(&self) -> bool {
        self.flags & FH_USE_REF_MVS != 0
    }

    /// Frame-end CDF update disabled.
    pub(crate) const fn disable_frame_end_update_cdf(&self) -> bool {
        self.flags & FH_DISABLE_FRAME_END_UPDATE_CDF != 0
    }

    /// UV delta differs.
    pub(crate) const fn diff_uv_delta(&self) -> bool {
        self.flags & FH_DIFF_UV_DELTA != 0
    }

    /// Quantizer matrix.
    pub(crate) const fn using_qmatrix(&self) -> bool {
        self.flags & FH_USING_QMATRIX != 0
    }

    /// Segmentation enabled.
    pub(crate) const fn segmentation_enabled(&self) -> bool {
        self.flags & FH_SEG_ENABLED != 0
    }

    /// Segmentation map update.
    pub(crate) const fn segmentation_update_map(&self) -> bool {
        self.flags & FH_SEG_UPDATE_MAP != 0
    }

    /// Segmentation data update.
    pub(crate) const fn segmentation_update_data(&self) -> bool {
        self.flags & FH_SEG_UPDATE_DATA != 0
    }

    /// Segmentation temporal update.
    pub(crate) const fn segmentation_temporal_update(&self) -> bool {
        self.flags & FH_SEG_TEMPORAL_UPDATE != 0
    }

    /// Delta quantizer present.
    pub(crate) const fn delta_q_present(&self) -> bool {
        self.flags & FH_DELTA_Q_PRESENT != 0
    }

    /// Delta loop filter present.
    pub(crate) const fn delta_lf_present(&self) -> bool {
        self.flags & FH_DELTA_LF_PRESENT != 0
    }

    /// Multi-delta loop filter.
    pub(crate) const fn delta_lf_multi(&self) -> bool {
        self.flags & FH_DELTA_LF_MULTI != 0
    }

    /// Loop filter delta enabled.
    pub(crate) const fn loop_filter_delta_enabled(&self) -> bool {
        self.flags & FH_LOOP_DELTA_ENABLED != 0
    }

    /// Loop filter delta update.
    pub(crate) const fn loop_filter_delta_update(&self) -> bool {
        self.flags & FH_LOOP_DELTA_UPDATE != 0
    }

    /// Compound reference mode.
    pub(crate) const fn reference_select(&self) -> bool {
        self.flags & FH_REFERENCE_SELECT != 0
    }

    /// Skip mode present.
    pub(crate) const fn skip_mode_present(&self) -> bool {
        self.flags & FH_SKIP_MODE_PRESENT != 0
    }

    /// Warped motion.
    pub(crate) const fn allow_warped_motion(&self) -> bool {
        self.flags & FH_ALLOW_WARPED_MOTION != 0
    }

    /// Reduced transform set.
    pub(crate) const fn reduced_tx_set(&self) -> bool {
        self.flags & FH_REDUCED_TX_SET != 0
    }
}

/// Reference slot state behind frame header parsing.
#[derive(Clone)]
pub(crate) struct RefInfo {
    /// Slot holds a decodable picture.
    pub(crate) valid: bool,
    /// Stored frame type.
    pub(crate) frame_type: FrameType,
    /// Stored frame width in luma samples.
    pub(crate) frame_width: u32,
    /// Stored frame height in luma samples.
    pub(crate) frame_height: u32,
    /// Stored upscaled width in luma samples.
    pub(crate) upscaled_width: u32,
    /// Stored render width in luma samples.
    pub(crate) render_width: u32,
    /// Stored render height in luma samples.
    pub(crate) render_height: u32,
    /// Stored order hint.
    pub(crate) order_hint: u8,
    /// Stored frame id.
    pub(crate) frame_id: u32,
    /// Stored loop filter reference deltas.
    pub(crate) loop_ref_deltas: [i8; TOTAL_REFS],
    /// Stored loop filter mode deltas.
    pub(crate) loop_mode_deltas: [i8; 2],
    /// Stored segmentation enables.
    pub(crate) seg_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Stored segmentation values.
    pub(crate) seg_value: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
    /// Stored global motion parameters per reference.
    pub(crate) gm_params: [[i32; 6]; REFS_PER_FRAME],
    /// Stored film grain parameters.
    pub(crate) grain: FilmGrain,
}

impl Default for RefInfo {
    /// Empty slot with identity motion.
    fn default() -> Self {
        Self {
            valid: false,
            frame_type: FrameType::Key,
            frame_width: 0,
            frame_height: 0,
            upscaled_width: 0,
            render_width: 0,
            render_height: 0,
            order_hint: 0,
            frame_id: 0,
            loop_ref_deltas: [0; TOTAL_REFS],
            loop_mode_deltas: [0; 2],
            seg_enabled: [[false; SEG_LEVELS]; MAX_SEGMENTS],
            seg_value: [[0; SEG_LEVELS]; MAX_SEGMENTS],
            gm_params: [[
                0,
                0,
                1 << WARPEDMODEL_PREC_BITS,
                0,
                0,
                1 << WARPEDMODEL_PREC_BITS,
            ]; REFS_PER_FRAME],
            grain: FilmGrain::default(),
        }
    }
}

/// Uncompressed header reader over one frame header payload.
struct HeaderReader<'a> {
    /// Payload bits under read.
    reader: BitReader<'a>,
    /// Sequence header behind conditional syntax.
    seq: &'a SequenceHeader,
    /// Reference slots behind inheritance and validation.
    refs: &'a mut [RefInfo; NUM_REF_FRAMES],
    /// Temporal id of the header OBU.
    temporal_id: u8,
    /// Spatial id of the header OBU.
    spatial_id: u8,
    /// Accumulating header.
    header: FrameHeader,
    /// Coded lossless flag behind tool gating.
    coded_lossless: bool,
    /// Fully lossless flag behind restoration gating.
    all_lossless: bool,
}

impl<'a> HeaderReader<'a> {
    /// Segmentation feature bit widths per level.
    const SEG_BITS: [u8; SEG_LEVELS] = [8, 6, 6, 6, 6, 3, 0, 0];

    /// Segmentation feature signs per level.
    const SEG_SIGN: [bool; SEG_LEVELS] = [true, true, true, true, true, false, false, false];

    /// Default loop filter reference deltas.
    const DEFAULT_REF_DELTAS: [i8; TOTAL_REFS] = [1, 0, 0, 0, -1, 0, -1, -1];

    /// Reader over `payload` against `seq` and `refs`.
    fn new(
        payload: &'a [u8],
        seq: &'a SequenceHeader,
        refs: &'a mut [RefInfo; NUM_REF_FRAMES],
        temporal_id: u8,
        spatial_id: u8,
    ) -> Self {
        Self {
            reader: BitReader::new(payload),
            seq,
            refs,
            temporal_id,
            spatial_id,
            header: FrameHeader::default(),
            coded_lossless: false,
            all_lossless: false,
        }
    }

    /// Parsed frame header plus consumed bit count.
    fn parse(mut self) -> Result<(FrameHeader, usize), D3d11Error> {
        if self.seq.reduced_header() {
            self.header.flags |= FH_SHOW_FRAME;
        } else if self.reader.read_bit("show_existing_frame")? {
            let header = self.parse_show_existing()?;
            let consumed = self.reader.position();
            return Ok((header, consumed));
        } else {
            self.parse_frame_type()?;
        }
        if self.header.frame_type == FrameType::Key && self.header.show_frame() {
            self.reset_key_refs();
        }
        self.parse_screen_content()?;
        self.parse_frame_id()?;
        self.parse_size_override()?;
        self.parse_order_hint()?;
        self.parse_primary_ref()?;
        self.parse_buffer_removal()?;
        self.parse_refresh_flags()?;
        self.parse_ref_order_hints()?;
        self.parse_frame_size_and_refs()?;
        self.parse_frame_end_update()?;
        self.parse_tile_info()?;
        self.parse_quantization()?;
        self.parse_segmentation()?;
        self.parse_delta_q()?;
        self.parse_delta_lf()?;
        self.compute_lossless();
        self.parse_loop_filter()?;
        self.parse_cdef()?;
        self.parse_restoration()?;
        self.parse_tx_mode()?;
        self.parse_reference_mode()?;
        self.parse_skip_mode()?;
        self.parse_warped_motion()?;
        if self.reader.read_bit("reduced_tx_set")? {
            self.header.flags |= FH_REDUCED_TX_SET;
        }
        self.parse_global_motion()?;
        self.parse_film_grain()?;
        let consumed = self.reader.position();
        Ok((self.header, consumed))
    }

    /// Existing-frame display header.
    fn parse_show_existing(&mut self) -> Result<FrameHeader, D3d11Error> {
        self.header.flags |= FH_SHOW_EXISTING;
        let index = read_small(&mut self.reader, 3, "frame_to_show_map_idx")?;
        let slot = self
            .refs
            .get(usize::from(index))
            .filter(|slot| slot.valid)
            .ok_or({
                D3d11Error::BrokenReference(BrokenReferenceReason::NamesEmptySlot {
                    reference: "show_existing_frame",
                })
            })?;
        self.header.frame_to_show_map_idx = index;
        self.header.frame_type = slot.frame_type;
        self.header.order_hint = slot.order_hint;
        if self.seq.decoder_model_present() && !self.seq.equal_picture_interval() {
            let width = u32::from(self.seq.presentation_length_minus1) + 1;
            self.reader.read_bits(width, "frame_presentation_time")?;
        }
        if self.seq.frame_id_numbers_present() {
            self.header.display_frame_id = self
                .reader
                .read_bits(u32::from(self.seq.id_len()), "display_frame_id")?;
        }
        if self.header.frame_type == FrameType::Key {
            self.header.refresh_frame_flags = 0xFF;
        }
        Ok(self.header.clone())
    }

    /// Frame type plus show and resilience flags.
    fn parse_frame_type(&mut self) -> Result<(), D3d11Error> {
        let frame_type = FrameType::new(self.reader.read_bits(2, "frame_type")?)?;
        self.header.frame_type = frame_type;
        if self.reader.read_bit("show_frame")? {
            self.header.flags |= FH_SHOW_FRAME;
            if self.seq.decoder_model_present() && !self.seq.equal_picture_interval() {
                let width = u32::from(self.seq.presentation_length_minus1) + 1;
                self.reader.read_bits(width, "frame_presentation_time")?;
            }
            if frame_type != FrameType::Key {
                self.header.flags |= FH_SHOWABLE_FRAME;
            }
        } else if self.reader.read_bit("showable_frame")? {
            self.header.flags |= FH_SHOWABLE_FRAME;
        }
        let resilient = frame_type == FrameType::Switch
            || (frame_type == FrameType::Key && self.header.show_frame())
            || self.reader.read_bit("error_resilient_mode")?;
        if resilient {
            self.header.flags |= FH_ERROR_RESILIENT;
        }
        Ok(())
    }

    /// Reference slots emptied ahead of a shown key frame.
    fn reset_key_refs(&mut self) {
        for slot in self.refs.iter_mut() {
            slot.valid = false;
            slot.order_hint = 0;
        }
    }

    /// Screen content tools plus integer motion vectors.
    fn parse_screen_content(&mut self) -> Result<(), D3d11Error> {
        if self.reader.read_bit("disable_cdf_update")? {
            self.header.flags |= FH_DISABLE_CDF_UPDATE;
        }
        if self.seq.force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS {
            if self.reader.read_bit("allow_screen_content_tools")? {
                self.header.flags |= FH_SCREEN_CONTENT_TOOLS;
            }
        } else if self.seq.force_screen_content_tools != 0 {
            self.header.flags |= FH_SCREEN_CONTENT_TOOLS;
        }
        if self.header.allow_screen_content_tools()
            && self.seq.force_integer_mv == SELECT_INTEGER_MV
            && self.reader.read_bit("force_integer_mv")?
        {
            self.header.flags |= FH_FORCE_INTEGER_MV;
        }
        Ok(())
    }

    /// Current frame id plus stale reference invalidation.
    fn parse_frame_id(&mut self) -> Result<(), D3d11Error> {
        if !self.seq.frame_id_numbers_present() {
            return Ok(());
        }
        let id_len = u32::from(self.seq.id_len());
        let diff_len = u32::from(self.seq.delta_frame_id_length_minus2) + 2;
        let current = self.reader.read_bits(id_len, "current_frame_id")?;
        self.header.current_frame_id = current;
        for slot in self.refs.iter_mut() {
            if !slot.valid {
                continue;
            }
            if current > (1 << diff_len) {
                if slot.frame_id > current || slot.frame_id < current - (1 << diff_len) {
                    slot.valid = false;
                }
            } else if slot.frame_id > current
                && slot.frame_id < ((1 << id_len) + current - (1 << diff_len))
            {
                slot.valid = false;
            }
        }
        Ok(())
    }

    /// Frame size override flag.
    fn parse_size_override(&mut self) -> Result<(), D3d11Error> {
        let override_flag = self.header.frame_type == FrameType::Switch
            || (!self.seq.reduced_header() && self.reader.read_bit("frame_size_override_flag")?);
        if override_flag {
            self.header.flags |= FH_SIZE_OVERRIDE;
        }
        Ok(())
    }

    /// Frame order hint.
    fn parse_order_hint(&mut self) -> Result<(), D3d11Error> {
        let bits = self.seq.order_hint_bits();
        if bits == 0 {
            return Ok(());
        }
        self.header.order_hint = read_small(&mut self.reader, u32::from(bits), "order_hint")?;
        Ok(())
    }

    /// Primary reference frame.
    fn parse_primary_ref(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_type.is_intra() || self.header.error_resilient_mode() {
            self.header.primary_ref_frame = PRIMARY_REF_NONE;
        } else {
            self.header.primary_ref_frame = read_small(&mut self.reader, 3, "primary_ref_frame")?;
        }
        Ok(())
    }

    /// Buffer removal times per matching operating point.
    fn parse_buffer_removal(&mut self) -> Result<(), D3d11Error> {
        if !self.seq.decoder_model_present() {
            return Ok(());
        }
        if !self.reader.read_bit("buffer_removal_time_present_flag")? {
            return Ok(());
        }
        let width = u32::from(self.seq.buffer_removal_length_minus1) + 1;
        for point in &self.seq.operating {
            if !point.decoder_model_present {
                continue;
            }
            let idc = u32::from(point.idc);
            let in_temporal = idc >> self.temporal_id & 1 != 0;
            let in_spatial = idc >> (self.spatial_id + 8) & 1 != 0;
            if idc == 0 || (in_temporal && in_spatial) {
                self.reader.read_bits(width, "buffer_removal_time")?;
            }
        }
        Ok(())
    }

    /// Refresh flags per slot.
    fn parse_refresh_flags(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_type == FrameType::Switch
            || (self.header.frame_type == FrameType::Key && self.header.show_frame())
        {
            self.header.refresh_frame_flags = 0xFF;
        } else {
            self.header.refresh_frame_flags =
                read_small(&mut self.reader, 8, "refresh_frame_flags")?;
        }
        Ok(())
    }

    /// Reference order hints with mismatch invalidation.
    fn parse_ref_order_hints(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_type.is_intra() && self.header.refresh_frame_flags == 0xFF {
            return Ok(());
        }
        if !self.seq.enable_order_hint() {
            return Ok(());
        }
        let bits = u32::from(self.seq.order_hint_bits());
        for (index, slot) in self.refs.iter_mut().enumerate() {
            let hint = if self.header.error_resilient_mode() {
                read_small(&mut self.reader, bits, "ref_order_hint")?
            } else {
                slot.order_hint
            };
            self.header.ref_order_hint[index] = hint;
            if hint != slot.order_hint {
                slot.valid = false;
            }
        }
        Ok(())
    }

    /// Frame sizes plus reference selection.
    fn parse_frame_size_and_refs(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_type.is_intra() {
            self.parse_frame_size()?;
            self.parse_render_size()?;
            if self.header.allow_screen_content_tools()
                && self.header.upscaled_width == self.header.frame_width
                && self.reader.read_bit("allow_intrabc")?
            {
                self.header.flags |= FH_ALLOW_INTRABC;
            }
            return Ok(());
        }
        self.parse_ref_frames()?;
        if self.header.frame_size_override_flag() && !self.header.error_resilient_mode() {
            self.parse_frame_size_with_refs()?;
        } else {
            self.parse_frame_size()?;
            self.parse_render_size()?;
        }
        if !self.header.force_integer_mv() && self.reader.read_bit("allow_high_precision_mv")? {
            self.header.flags |= FH_HIGH_PRECISION_MV;
        }
        self.parse_interpolation_filter()?;
        if self.reader.read_bit("is_motion_mode_switchable")? {
            self.header.flags |= FH_MOTION_MODE_SWITCHABLE;
        }
        if !self.header.error_resilient_mode()
            && self.seq.enable_ref_frame_mvs()
            && self.reader.read_bit("use_ref_frame_mvs")?
        {
            self.header.flags |= FH_USE_REF_MVS;
        }
        Ok(())
    }

    /// Reference slots per position with validity checks.
    fn parse_ref_frames(&mut self) -> Result<(), D3d11Error> {
        if self.seq.enable_order_hint() && self.reader.read_bit("frame_refs_short_signaling")? {
            self.header.flags |= FH_SHORT_SIGNALING;
            let last = read_small(&mut self.reader, 3, "last_frame_idx")?;
            let golden = read_small(&mut self.reader, 3, "golden_frame_idx")?;
            self.set_frame_refs(last, golden)?;
        } else {
            for index in 0..REFS_PER_FRAME {
                self.header.ref_frame_idx[index] =
                    read_small(&mut self.reader, 3, "ref_frame_idx")?;
                self.parse_delta_frame_id()?;
            }
        }
        if self.header.frame_refs_short_signaling() {
            for _ in 0..REFS_PER_FRAME {
                self.parse_delta_frame_id()?;
            }
        }
        for index in 0..REFS_PER_FRAME {
            let slot = usize::from(self.header.ref_frame_idx[index]);
            if !self.refs.get(slot).is_some_and(|slot| slot.valid) {
                return Err(D3d11Error::BrokenReference(
                    BrokenReferenceReason::NamesEmptySlot {
                        reference: "ref_frame_idx",
                    },
                ));
            }
        }
        Ok(())
    }

    /// One reference delta frame id.
    fn parse_delta_frame_id(&mut self) -> Result<(), D3d11Error> {
        if !self.seq.frame_id_numbers_present() {
            return Ok(());
        }
        let width = u32::from(self.seq.delta_frame_id_length_minus2) + 2;
        self.reader.read_bits(width, "delta_frame_id_minus1")?;
        Ok(())
    }

    /// Short-signaled reference slots from order hints.
    fn set_frame_refs(&mut self, last: u8, golden: u8) -> Result<(), D3d11Error> {
        for slot in [last, golden] {
            if !self
                .refs
                .get(usize::from(slot))
                .is_some_and(|slot| slot.valid)
            {
                return Err(D3d11Error::BrokenReference(
                    BrokenReferenceReason::NamesEmptySlot {
                        reference: "short reference",
                    },
                ));
            }
        }
        let mut idx = [0xFFu8; REFS_PER_FRAME];
        idx[REF_LAST - 1] = last;
        idx[REF_GOLDEN - REF_LAST] = golden;
        let mut used = [false; NUM_REF_FRAMES];
        used[usize::from(last)] = true;
        used[usize::from(golden)] = true;
        let hints = self.shifted_order_hints();
        let current = 1 << self.seq.order_hint_bits_minus1;
        let mut latest = hints[usize::from(last)];
        let mut earliest = hints[usize::from(golden)];
        if let Some(found) = Self::find_latest_future(used, &hints, current, latest) {
            idx[REF_ALTREF - REF_LAST] = found;
            used[usize::from(found)] = true;
            latest = hints[usize::from(found)];
        }
        for position in [REF_BWDREF - REF_LAST, REF_ALTREF2 - REF_LAST] {
            if let Some(found) = Self::find_earliest_unused(used, &hints, current, earliest) {
                idx[position] = found;
                used[usize::from(found)] = true;
                earliest = hints[usize::from(found)];
            }
        }
        for position in [
            REF_LAST2 - REF_LAST,
            REF_LAST3 - REF_LAST,
            REF_BWDREF - REF_LAST,
            REF_ALTREF2 - REF_LAST,
            REF_ALTREF - REF_LAST,
        ] {
            if idx[position] != 0xFF {
                continue;
            }
            if let Some(found) = Self::find_latest_past(used, &hints, current, latest) {
                idx[position] = found;
                used[usize::from(found)] = true;
                latest = hints[usize::from(found)];
            }
        }
        let mut fallback = 0u8;
        for (slot, hint) in hints.iter().enumerate() {
            if *hint < earliest {
                earliest = *hint;
                fallback = u8::try_from(slot).unwrap_or(0);
            }
        }
        for slot in &mut idx {
            if *slot == 0xFF {
                *slot = fallback;
            }
        }
        self.header.ref_frame_idx = idx;
        Ok(())
    }

    /// Order hints shifted around the current hint.
    fn shifted_order_hints(&self) -> [i32; NUM_REF_FRAMES] {
        let current = 1 << self.seq.order_hint_bits_minus1;
        let mut hints = [0i32; NUM_REF_FRAMES];
        for (index, slot) in self.refs.iter().enumerate() {
            hints[index] = current
                + relative_dist(
                    slot.order_hint,
                    self.header.order_hint,
                    self.seq.order_hint_bits_minus1,
                );
        }
        hints
    }

    /// Latest unused future slot at or past `floor`.
    fn find_latest_future(
        used: [bool; NUM_REF_FRAMES],
        hints: &[i32; NUM_REF_FRAMES],
        current: i32,
        floor: i32,
    ) -> Option<u8> {
        let mut found: Option<u8> = None;
        let mut best = floor;
        for (slot, hint) in hints.iter().enumerate() {
            if used[slot] || *hint < current {
                continue;
            }
            if found.is_none() || *hint >= best {
                found = u8::try_from(slot).ok();
                best = *hint;
            }
        }
        found
    }

    /// Earliest unused future slot below `ceiling`.
    fn find_earliest_unused(
        used: [bool; NUM_REF_FRAMES],
        hints: &[i32; NUM_REF_FRAMES],
        current: i32,
        ceiling: i32,
    ) -> Option<u8> {
        let mut found: Option<u8> = None;
        let mut best = ceiling;
        for (slot, hint) in hints.iter().enumerate() {
            if used[slot] || *hint < current {
                continue;
            }
            if found.is_none() || *hint < best {
                found = u8::try_from(slot).ok();
                best = *hint;
            }
        }
        found
    }

    /// Latest unused past slot at or past `floor`.
    fn find_latest_past(
        used: [bool; NUM_REF_FRAMES],
        hints: &[i32; NUM_REF_FRAMES],
        current: i32,
        floor: i32,
    ) -> Option<u8> {
        let mut found: Option<u8> = None;
        let mut best = floor;
        for (slot, hint) in hints.iter().enumerate() {
            if used[slot] || *hint >= current {
                continue;
            }
            if found.is_none() || *hint >= best {
                found = u8::try_from(slot).ok();
                best = *hint;
            }
        }
        found
    }

    /// Frame size from a found reference or explicit syntax.
    fn parse_frame_size_with_refs(&mut self) -> Result<(), D3d11Error> {
        for position in 0..REFS_PER_FRAME {
            if !self.reader.read_bit("found_ref")? {
                continue;
            }
            let slot = usize::from(self.header.ref_frame_idx[position]);
            let reference = self.refs.get(slot).filter(|slot| slot.valid).ok_or({
                D3d11Error::BrokenReference(BrokenReferenceReason::NamesEmptySlot {
                    reference: "frame size",
                })
            })?;
            self.header.frame_width = reference.upscaled_width;
            self.header.frame_height = reference.frame_height;
            self.header.upscaled_width = reference.upscaled_width;
            self.header.render_width = reference.render_width;
            self.header.render_height = reference.render_height;
            return self.parse_superres();
        }
        self.parse_frame_size()?;
        self.parse_render_size()
    }

    /// Frame size from override or sequence maximum.
    fn parse_frame_size(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_size_override_flag() {
            let width_bits = u32::from(self.seq.frame_width_bits_minus1) + 1;
            let height_bits = u32::from(self.seq.frame_height_bits_minus1) + 1;
            self.header.frame_width = self.reader.read_bits(width_bits, "frame_width_minus_1")? + 1;
            self.header.frame_height =
                self.reader.read_bits(height_bits, "frame_height_minus_1")? + 1;
        } else {
            self.header.frame_width = self.seq.max_frame_width();
            self.header.frame_height = self.seq.max_frame_height();
        }
        self.parse_superres()
    }

    /// Render size from the flag or the frame size.
    fn parse_render_size(&mut self) -> Result<(), D3d11Error> {
        if !self.reader.read_bit("render_and_frame_size_different")? {
            self.header.render_width = self.header.frame_width;
            self.header.render_height = self.header.frame_height;
            return Ok(());
        }
        self.header.flags |= FH_RENDER_DIFFERENT;
        self.header.render_width = self.reader.read_bits(16, "render_width_minus_1")? + 1;
        self.header.render_height = self.reader.read_bits(16, "render_height_minus_1")? + 1;
        Ok(())
    }

    /// Superres denominator plus downscaled frame width.
    fn parse_superres(&mut self) -> Result<(), D3d11Error> {
        let mut denom = u32::from(SUPERRES_NUM);
        if self.seq.enable_superres() && self.reader.read_bit("use_superres")? {
            let coded = self.reader.read_bits(3, "coded_denom")?;
            denom = coded + u32::from(SUPERRES_DENOM_MIN);
        }
        self.header.superres_denom = u8::try_from(denom).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "coded_denom",
            })
        })?;
        self.header.upscaled_width = self.header.frame_width;
        self.header.frame_width =
            (self.header.upscaled_width * u32::from(SUPERRES_NUM) + denom / 2) / denom;
        Ok(())
    }

    /// Interpolation filter.
    fn parse_interpolation_filter(&mut self) -> Result<(), D3d11Error> {
        if self.reader.read_bit("is_filter_switchable")? {
            self.header.interp_filter = INTERP_SWITCHABLE;
        } else {
            self.header.interp_filter = read_small(&mut self.reader, 2, "interpolation_filter")?;
        }
        Ok(())
    }

    /// Frame-end CDF update flag.
    fn parse_frame_end_update(&mut self) -> Result<(), D3d11Error> {
        let disable = self.seq.reduced_header()
            || self.header.disable_cdf_update()
            || self.reader.read_bit("disable_frame_end_update_cdf")?;
        if disable {
            self.header.flags |= FH_DISABLE_FRAME_END_UPDATE_CDF;
        }
        Ok(())
    }

    /// Tile grid geometry.
    fn parse_tile_info(&mut self) -> Result<(), D3d11Error> {
        let mi_cols = 2 * ((self.header.frame_width + 7) >> 3);
        let mi_rows = 2 * ((self.header.frame_height + 7) >> 3);
        let (sb_cols, sb_rows, sb_size) = if self.seq.use_128x128_superblock() {
            ((mi_cols + 31) >> 5, (mi_rows + 31) >> 5, 7u32)
        } else {
            ((mi_cols + 15) >> 4, (mi_rows + 15) >> 4, 6u32)
        };
        let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
        let max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
        let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
        let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
        let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
        let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
        if self.reader.read_bit("uniform_tile_spacing_flag")? {
            self.parse_uniform_tiles(
                sb_cols,
                sb_rows,
                min_log2_tile_cols,
                max_log2_tile_cols,
                max_log2_tile_rows,
                min_log2_tiles,
            )?;
        } else {
            self.parse_explicit_tiles(sb_cols, sb_rows, max_tile_width_sb, min_log2_tiles)?;
        }
        if self.header.tile_cols_log2 > 0 || self.header.tile_rows_log2 > 0 {
            let width =
                u32::from(self.header.tile_cols_log2) + u32::from(self.header.tile_rows_log2);
            self.header.context_update_tile_id = u16::try_from(
                self.reader.read_bits(width, "context_update_tile_id")?,
            )
            .map_err(|_| {
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "context_update_tile_id",
                })
            })?;
            self.header.tile_size_bytes =
                read_small(&mut self.reader, 2, "tile_size_bytes_minus1")? + 1;
        } else {
            self.header.tile_size_bytes = 1;
        }
        Ok(())
    }

    /// Uniform tile grid from column and row shifts.
    fn parse_uniform_tiles(
        &mut self,
        sb_cols: u32,
        sb_rows: u32,
        min_cols: u32,
        max_cols: u32,
        max_rows: u32,
        min_tiles: u32,
    ) -> Result<(), D3d11Error> {
        let cols_log2 = read_increment(&mut self.reader, min_cols, max_cols, "tile_cols_log2")?;
        let tile_width_sb = (sb_cols + (1 << cols_log2) - 1) >> cols_log2;
        let cols = sb_cols.div_ceil(tile_width_sb);
        let min_rows = min_tiles.saturating_sub(cols_log2);
        let rows_log2 = read_increment(&mut self.reader, min_rows, max_rows, "tile_rows_log2")?;
        let tile_height_sb = (sb_rows + (1 << rows_log2) - 1) >> rows_log2;
        let rows = sb_rows.div_ceil(tile_height_sb);
        self.header.tile_cols_log2 = u8::try_from(cols_log2).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile_cols_log2",
            })
        })?;
        self.header.tile_rows_log2 = u8::try_from(rows_log2).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile_rows_log2",
            })
        })?;
        self.header.tile_cols = u8::try_from(cols).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile_cols",
            })
        })?;
        self.header.tile_rows = u8::try_from(rows).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile_rows",
            })
        })?;
        let cols = usize::from(self.header.tile_cols);
        let rows = usize::from(self.header.tile_rows);
        let width_minus1 = u16::try_from(tile_width_sb - 1).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile width",
            })
        })?;
        let height_minus1 = u16::try_from(tile_height_sb - 1).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile height",
            })
        })?;
        for index in 0..cols {
            self.header.width_in_sbs_minus_1[index] = if index + 1 < cols {
                width_minus1
            } else {
                let last = sb_cols - u32::from(self.header.tile_cols - 1) * tile_width_sb - 1;
                u16::try_from(last).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "tile width",
                    })
                })?
            };
        }
        for index in 0..rows {
            self.header.height_in_sbs_minus_1[index] = if index + 1 < rows {
                height_minus1
            } else {
                let last = sb_rows - u32::from(self.header.tile_rows - 1) * tile_height_sb - 1;
                u16::try_from(last).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "tile height",
                    })
                })?
            };
        }
        Ok(())
    }

    /// Explicit tile grid from superblock widths.
    fn parse_explicit_tiles(
        &mut self,
        sb_cols: u32,
        sb_rows: u32,
        max_tile_width_sb: u32,
        min_log2_tiles: u32,
    ) -> Result<(), D3d11Error> {
        let (cols, widest) = self.parse_tile_axis(sb_cols, max_tile_width_sb, true)?;
        let max_area = if min_log2_tiles > 0 {
            (sb_rows * sb_cols) >> (min_log2_tiles + 1)
        } else {
            sb_rows * sb_cols
        };
        let max_height_sb = (max_area / widest).max(1);
        let (rows, _) = self.parse_tile_axis(sb_rows, max_height_sb, false)?;
        self.header.tile_cols_log2 = u8::try_from(tile_log2(1, u32::from(cols))).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile_cols_log2",
            })
        })?;
        self.header.tile_rows_log2 = u8::try_from(tile_log2(1, u32::from(rows))).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "tile_rows_log2",
            })
        })?;
        self.header.tile_cols = cols;
        self.header.tile_rows = rows;
        Ok(())
    }

    /// One tile axis: count plus widest tile in superblocks.
    fn parse_tile_axis(
        &mut self,
        sb_count: u32,
        max_extent_sb: u32,
        columns: bool,
    ) -> Result<(u8, u32), D3d11Error> {
        let mut start_sb = 0u32;
        let mut count = 0u8;
        let mut widest = 0u32;
        while start_sb < sb_count {
            if u32::from(count) >= MAX_TILE_COLS {
                return Err(D3d11Error::Parse(ParseReason::Range {
                    syntax: "tile count",
                }));
            }
            let ceiling = (sb_count - start_sb).min(max_extent_sb);
            let minus1 = read_ns(&mut self.reader, ceiling, "tile_width_minus_1")?;
            let size_sb = minus1 + 1;
            let stored = u16::try_from(minus1).map_err(|_| {
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "tile width",
                })
            })?;
            if columns {
                self.header.width_in_sbs_minus_1[usize::from(count)] = stored;
            } else {
                self.header.height_in_sbs_minus_1[usize::from(count)] = stored;
            }
            widest = widest.max(size_sb);
            start_sb += size_sb;
            count += 1;
        }
        if start_sb != sb_count {
            return Err(D3d11Error::Parse(ParseReason::TileGridCoverage));
        }
        Ok((count, widest))
    }

    /// Quantization parameters.
    fn parse_quantization(&mut self) -> Result<(), D3d11Error> {
        self.header.base_q_idx = read_small(&mut self.reader, 8, "base_q_idx")?;
        self.header.delta_q_y_dc = self.parse_delta_q_value("delta_q_y_dc")?;
        if self.seq.num_planes() > 1 {
            if self.seq.separate_uv_delta_q() && self.reader.read_bit("diff_uv_delta")? {
                self.header.flags |= FH_DIFF_UV_DELTA;
            }
            self.header.delta_q_u_dc = self.parse_delta_q_value("delta_q_u_dc")?;
            self.header.delta_q_u_ac = self.parse_delta_q_value("delta_q_u_ac")?;
            if self.header.diff_uv_delta() {
                self.header.delta_q_v_dc = self.parse_delta_q_value("delta_q_v_dc")?;
                self.header.delta_q_v_ac = self.parse_delta_q_value("delta_q_v_ac")?;
            } else {
                self.header.delta_q_v_dc = self.header.delta_q_u_dc;
                self.header.delta_q_v_ac = self.header.delta_q_u_ac;
            }
        }
        if self.reader.read_bit("using_qmatrix")? {
            self.header.flags |= FH_USING_QMATRIX;
            self.header.qm_y = read_small(&mut self.reader, 4, "qm_y")?;
            self.header.qm_u = read_small(&mut self.reader, 4, "qm_u")?;
            if self.seq.separate_uv_delta_q() {
                self.header.qm_v = read_small(&mut self.reader, 4, "qm_v")?;
            } else {
                self.header.qm_v = self.header.qm_u;
            }
        }
        Ok(())
    }

    /// One quantizer delta.
    fn parse_delta_q_value(&mut self, syntax: &'static str) -> Result<i8, D3d11Error> {
        if !self.reader.read_bit("delta_coded")? {
            return Ok(0);
        }
        let value = read_su(&mut self.reader, 7, syntax)?;
        i8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
    }

    /// Segmentation parameters.
    fn parse_segmentation(&mut self) -> Result<(), D3d11Error> {
        if !self.reader.read_bit("segmentation_enabled")? {
            return Ok(());
        }
        self.header.flags |= FH_SEG_ENABLED;
        if self.header.primary_ref_frame == PRIMARY_REF_NONE {
            self.header.flags |= FH_SEG_UPDATE_MAP | FH_SEG_UPDATE_DATA;
        } else {
            if self.reader.read_bit("segmentation_update_map")? {
                self.header.flags |= FH_SEG_UPDATE_MAP;
                if self.reader.read_bit("segmentation_temporal_update")? {
                    self.header.flags |= FH_SEG_TEMPORAL_UPDATE;
                }
            }
            if self.reader.read_bit("segmentation_update_data")? {
                self.header.flags |= FH_SEG_UPDATE_DATA;
            }
        }
        for segment in 0..MAX_SEGMENTS {
            for level in 0..SEG_LEVELS {
                self.parse_segment_feature(
                    segment,
                    level,
                    Self::SEG_BITS[level],
                    Self::SEG_SIGN[level],
                )?;
            }
        }
        Ok(())
    }

    /// One segmentation feature value.
    fn parse_segment_feature(
        &mut self,
        segment: usize,
        level: usize,
        bits: u8,
        signed: bool,
    ) -> Result<(), D3d11Error> {
        if !self.header.segmentation_update_data() {
            let primary =
                usize::from(self.header.ref_frame_idx[usize::from(self.header.primary_ref_frame)]);
            let slot = self.refs.get(primary).filter(|slot| slot.valid).ok_or({
                D3d11Error::BrokenReference(BrokenReferenceReason::NamesEmptySlot {
                    reference: "segmentation",
                })
            })?;
            self.header.seg_enabled[segment][level] = slot.seg_enabled[segment][level];
            self.header.seg_value[segment][level] = slot.seg_value[segment][level];
            return Ok(());
        }
        if !self.reader.read_bit("feature_enabled")? {
            return Ok(());
        }
        self.header.seg_enabled[segment][level] = true;
        if bits == 0 {
            return Ok(());
        }
        let value = if signed {
            read_su(&mut self.reader, u32::from(bits) + 1, "feature_value")?
        } else {
            i32::from(read_small(
                &mut self.reader,
                u32::from(bits),
                "feature_value",
            )?)
        };
        self.header.seg_value[segment][level] = i16::try_from(value).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "feature_value",
            })
        })?;
        Ok(())
    }

    /// Delta quantizer parameters.
    fn parse_delta_q(&mut self) -> Result<(), D3d11Error> {
        if self.header.base_q_idx == 0 {
            return Ok(());
        }
        if !self.reader.read_bit("delta_q_present")? {
            return Ok(());
        }
        self.header.flags |= FH_DELTA_Q_PRESENT;
        self.header.delta_q_res = read_small(&mut self.reader, 2, "delta_q_res")?;
        Ok(())
    }

    /// Delta loop filter parameters.
    fn parse_delta_lf(&mut self) -> Result<(), D3d11Error> {
        if !self.header.delta_q_present() || self.header.allow_intrabc() {
            return Ok(());
        }
        if !self.reader.read_bit("delta_lf_present")? {
            return Ok(());
        }
        self.header.flags |= FH_DELTA_LF_PRESENT;
        self.header.delta_lf_res = read_small(&mut self.reader, 2, "delta_lf_res")?;
        if self.reader.read_bit("delta_lf_multi")? {
            self.header.flags |= FH_DELTA_LF_MULTI;
        }
        Ok(())
    }

    /// Lossless flags behind tool gating.
    fn compute_lossless(&mut self) {
        self.coded_lossless = true;
        for segment in 0..MAX_SEGMENTS {
            let mut qindex = i32::from(self.header.base_q_idx);
            if self.header.seg_enabled[segment][0] {
                qindex += i32::from(self.header.seg_value[segment][0]);
            }
            let qindex = qindex.clamp(0, 255);
            if qindex != 0
                || self.header.delta_q_y_dc != 0
                || self.header.delta_q_u_dc != 0
                || self.header.delta_q_u_ac != 0
                || self.header.delta_q_v_dc != 0
                || self.header.delta_q_v_ac != 0
            {
                self.coded_lossless = false;
                break;
            }
        }
        self.all_lossless =
            self.coded_lossless && self.header.frame_width == self.header.upscaled_width;
    }

    /// Loop filter parameters.
    fn parse_loop_filter(&mut self) -> Result<(), D3d11Error> {
        if self.coded_lossless || self.header.allow_intrabc() {
            self.header.loop_ref_deltas = Self::DEFAULT_REF_DELTAS;
            return Ok(());
        }
        self.header.loop_filter_level[0] = read_small(&mut self.reader, 6, "loop_filter_level")?;
        self.header.loop_filter_level[1] = read_small(&mut self.reader, 6, "loop_filter_level")?;
        if self.seq.num_planes() > 1
            && (self.header.loop_filter_level[0] != 0 || self.header.loop_filter_level[1] != 0)
        {
            self.header.loop_filter_level[2] =
                read_small(&mut self.reader, 6, "loop_filter_level")?;
            self.header.loop_filter_level[3] =
                read_small(&mut self.reader, 6, "loop_filter_level")?;
        }
        self.header.loop_filter_sharpness =
            read_small(&mut self.reader, 3, "loop_filter_sharpness")?;
        if !self.reader.read_bit("loop_filter_delta_enabled")? {
            self.header.loop_ref_deltas = Self::DEFAULT_REF_DELTAS;
            return Ok(());
        }
        self.header.flags |= FH_LOOP_DELTA_ENABLED;
        if self.reader.read_bit("loop_filter_delta_update")? {
            self.header.flags |= FH_LOOP_DELTA_UPDATE;
        }
        for index in 0..TOTAL_REFS {
            self.parse_loop_delta(index, true, Self::DEFAULT_REF_DELTAS[index])?;
        }
        for index in 0..2 {
            self.parse_loop_delta(index, false, 0)?;
        }
        Ok(())
    }

    /// One loop filter reference or mode delta.
    fn parse_loop_delta(
        &mut self,
        index: usize,
        reference: bool,
        default: i8,
    ) -> Result<(), D3d11Error> {
        if !self.header.loop_filter_delta_update() {
            self.store_loop_delta(
                index,
                reference,
                self.inherited_loop_delta(index, reference, default)?,
            );
            return Ok(());
        }
        if !self.reader.read_bit("update_delta")? {
            self.store_loop_delta(
                index,
                reference,
                self.inherited_loop_delta(index, reference, default)?,
            );
            return Ok(());
        }
        let value = read_su(&mut self.reader, 7, "loop_filter_delta")?;
        let value = i8::try_from(value).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "loop_filter_delta",
            })
        })?;
        self.store_loop_delta(index, reference, value);
        Ok(())
    }

    /// Inherited loop filter delta from the primary reference.
    fn inherited_loop_delta(
        &self,
        index: usize,
        reference: bool,
        default: i8,
    ) -> Result<i8, D3d11Error> {
        if self.header.primary_ref_frame == PRIMARY_REF_NONE {
            return Ok(default);
        }
        let primary =
            usize::from(self.header.ref_frame_idx[usize::from(self.header.primary_ref_frame)]);
        let slot = self.refs.get(primary).filter(|slot| slot.valid).ok_or({
            D3d11Error::BrokenReference(BrokenReferenceReason::NamesEmptySlot {
                reference: "loop filter",
            })
        })?;
        Ok(if reference {
            slot.loop_ref_deltas[index]
        } else {
            slot.loop_mode_deltas[index]
        })
    }

    /// Loop filter delta stored by kind.
    fn store_loop_delta(&mut self, index: usize, reference: bool, value: i8) {
        if reference {
            self.header.loop_ref_deltas[index] = value;
        } else {
            self.header.loop_mode_deltas[index] = value;
        }
    }

    /// CDEF parameters.
    fn parse_cdef(&mut self) -> Result<(), D3d11Error> {
        if self.coded_lossless || self.header.allow_intrabc() || !self.seq.enable_cdef() {
            return Ok(());
        }
        self.header.cdef_damping_minus_3 = read_small(&mut self.reader, 2, "cdef_damping_minus_3")?;
        self.header.cdef_bits = read_small(&mut self.reader, 2, "cdef_bits")?;
        for index in 0..1usize << u32::from(self.header.cdef_bits) {
            self.header.cdef_y_pri[index] = read_small(&mut self.reader, 4, "cdef_y_pri")?;
            self.header.cdef_y_sec[index] = read_small(&mut self.reader, 2, "cdef_y_sec")?;
            if self.seq.num_planes() > 1 {
                self.header.cdef_uv_pri[index] = read_small(&mut self.reader, 4, "cdef_uv_pri")?;
                self.header.cdef_uv_sec[index] = read_small(&mut self.reader, 2, "cdef_uv_sec")?;
            }
        }
        Ok(())
    }

    /// Restoration parameters.
    fn parse_restoration(&mut self) -> Result<(), D3d11Error> {
        if self.all_lossless || self.header.allow_intrabc() || !self.seq.enable_restoration() {
            return Ok(());
        }
        let mut uses_lr = false;
        let mut uses_chroma_lr = false;
        for plane in 0..usize::from(self.seq.num_planes()) {
            let lr_type = read_small(&mut self.reader, 2, "lr_type")?;
            if !is_restoration_type(lr_type) {
                return Err(D3d11Error::Parse(ParseReason::Range { syntax: "lr_type" }));
            }
            self.header.lr_type[plane] = lr_type;
            if lr_type != RESTORE_NONE {
                uses_lr = true;
                if plane > 0 {
                    uses_chroma_lr = true;
                }
            }
        }
        if !uses_lr {
            return Ok(());
        }
        let (min, max) = if self.seq.use_128x128_superblock() {
            (1, 2)
        } else {
            (0, 2)
        };
        self.header.lr_unit_shift = read_increment(&mut self.reader, min, max, "lr_unit_shift")
            .and_then(|shift| {
                u8::try_from(shift).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "lr_unit_shift",
                    })
                })
            })?;
        if self.seq.subsampling_x() && self.seq.subsampling_y() && uses_chroma_lr {
            self.header.lr_uv_shift = u8::from(self.reader.read_bit("lr_uv_shift")?);
        }
        Ok(())
    }

    /// Transform mode.
    fn parse_tx_mode(&mut self) -> Result<(), D3d11Error> {
        if self.coded_lossless {
            return Ok(());
        }
        let tx_mode = read_increment(
            &mut self.reader,
            u32::from(TX_MODE_LARGEST),
            u32::from(TX_MODE_SELECT),
            "tx_mode",
        )?;
        self.header.tx_mode = u8::try_from(tx_mode)
            .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "tx_mode" }))?;
        Ok(())
    }

    /// Frame reference mode.
    fn parse_reference_mode(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_type.is_intra() {
            return Ok(());
        }
        if self.reader.read_bit("reference_select")? {
            self.header.flags |= FH_REFERENCE_SELECT;
        }
        Ok(())
    }

    /// Skip mode parameters.
    fn parse_skip_mode(&mut self) -> Result<(), D3d11Error> {
        if !self.skip_mode_allowed() {
            return Ok(());
        }
        if self.reader.read_bit("skip_mode_present")? {
            self.header.flags |= FH_SKIP_MODE_PRESENT;
        }
        Ok(())
    }

    /// True when skip mode syntax is present.
    fn skip_mode_allowed(&self) -> bool {
        if self.header.frame_type.is_intra()
            || !self.header.reference_select()
            || !self.seq.enable_order_hint()
        {
            return false;
        }
        let (forward, backward) = self.skip_mode_refs();
        if forward.is_none() {
            return false;
        }
        if backward.is_some() {
            return true;
        }
        self.second_forward_ref(forward).is_some()
    }

    /// Nearest forward and backward references by order hint.
    fn skip_mode_refs(&self) -> (Option<u8>, Option<u8>) {
        let mut forward: Option<u8> = None;
        let mut forward_hint = 0u8;
        let mut backward: Option<u8> = None;
        let mut backward_hint = 0u8;
        for position in 0..REFS_PER_FRAME {
            let slot = usize::from(self.header.ref_frame_idx[position]);
            let hint = self.refs[slot].order_hint;
            let dist = relative_dist(
                hint,
                self.header.order_hint,
                self.seq.order_hint_bits_minus1,
            );
            if dist < 0
                && forward.is_none_or(|_| {
                    relative_dist(hint, forward_hint, self.seq.order_hint_bits_minus1) > 0
                })
            {
                forward = Some(self.header.ref_frame_idx[position]);
                forward_hint = hint;
            } else if dist > 0
                && backward.is_none_or(|_| {
                    relative_dist(hint, backward_hint, self.seq.order_hint_bits_minus1) < 0
                })
            {
                backward = Some(self.header.ref_frame_idx[position]);
                backward_hint = hint;
            }
        }
        (forward, backward)
    }

    /// Second forward reference behind the first.
    fn second_forward_ref(&self, forward: Option<u8>) -> Option<u8> {
        let forward_hint = forward.map(|slot| self.refs[usize::from(slot)].order_hint)?;
        let mut second: Option<u8> = None;
        let mut second_hint = 0u8;
        for position in 0..REFS_PER_FRAME {
            let slot = usize::from(self.header.ref_frame_idx[position]);
            let hint = self.refs[slot].order_hint;
            if relative_dist(hint, forward_hint, self.seq.order_hint_bits_minus1) < 0
                && second.is_none_or(|_| {
                    relative_dist(hint, second_hint, self.seq.order_hint_bits_minus1) > 0
                })
            {
                second = Some(self.header.ref_frame_idx[position]);
                second_hint = hint;
            }
        }
        second
    }

    /// Warped motion flag.
    fn parse_warped_motion(&mut self) -> Result<(), D3d11Error> {
        if self.header.frame_type.is_intra()
            || self.header.error_resilient_mode()
            || !self.seq.enable_warped_motion()
        {
            return Ok(());
        }
        if self.reader.read_bit("allow_warped_motion")? {
            self.header.flags |= FH_ALLOW_WARPED_MOTION;
        }
        Ok(())
    }

    /// Global motion parameters per reference.
    fn parse_global_motion(&mut self) -> Result<(), D3d11Error> {
        for position in 0..REFS_PER_FRAME {
            self.header.global_motion[position] = GlobalMotion {
                warp_type: WARP_IDENTITY,
                params: identity_motion(),
                invalid: self.header.frame_type.is_intra(),
            };
        }
        if self.header.frame_type.is_intra() {
            return Ok(());
        }
        for position in 0..REFS_PER_FRAME {
            self.parse_reference_motion(position)?;
        }
        Ok(())
    }

    /// Global motion for one reference position.
    fn parse_reference_motion(&mut self, position: usize) -> Result<(), D3d11Error> {
        let warp_type = if !self.reader.read_bit("is_global")? {
            WARP_IDENTITY
        } else if self.reader.read_bit("is_rot_zoom")? {
            WARP_ROTZOOM
        } else if self.reader.read_bit("is_translation")? {
            WARP_TRANSLATION
        } else {
            WARP_AFFINE
        };
        let mut params = identity_motion();
        if warp_type >= WARP_ROTZOOM {
            params[2] = self.parse_motion_param(warp_type, position, 2)?;
            params[3] = self.parse_motion_param(warp_type, position, 3)?;
            if warp_type == WARP_AFFINE {
                params[4] = self.parse_motion_param(warp_type, position, 4)?;
                params[5] = self.parse_motion_param(warp_type, position, 5)?;
            } else {
                params[4] = params[3].wrapping_neg();
                params[5] = params[2];
            }
        }
        if warp_type >= WARP_TRANSLATION {
            params[0] = self.parse_motion_param(warp_type, position, 0)?;
            params[1] = self.parse_motion_param(warp_type, position, 1)?;
        }
        self.header.global_motion[position] = GlobalMotion {
            warp_type,
            params,
            invalid: !shear_valid(&params),
        };
        Ok(())
    }

    /// One reconstructed global motion parameter.
    fn parse_motion_param(
        &mut self,
        warp_type: u8,
        position: usize,
        index: usize,
    ) -> Result<i32, D3d11Error> {
        let (abs_bits, prec_bits) =
            motion_precision(warp_type, index, self.header.allow_high_precision_mv());
        let num_syms = 2 * (1 << abs_bits) + 1;
        let symbol = read_subexp(&mut self.reader, num_syms, "gm_params")?;
        let previous = if self.header.primary_ref_frame == PRIMARY_REF_NONE {
            identity_motion()[index]
        } else {
            let slot =
                usize::from(self.header.ref_frame_idx[usize::from(self.header.primary_ref_frame)]);
            self.refs[slot].gm_params[position][index]
        };
        Ok(reconstruct_motion_param(
            symbol, previous, index, abs_bits, prec_bits,
        ))
    }

    /// Film grain parameters.
    fn parse_film_grain(&mut self) -> Result<(), D3d11Error> {
        if !self.seq.film_grain_params_present()
            || (!self.header.show_frame() && !self.header.showable_frame())
        {
            return Ok(());
        }
        if !self.reader.read_bit("apply_grain")? {
            return Ok(());
        }
        let seed = self.reader.read_bits(16, "grain_seed")?;
        let update =
            self.header.frame_type != FrameType::Inter || self.reader.read_bit("update_grain")?;
        if !update {
            let index = read_small(&mut self.reader, 3, "film_grain_params_ref_idx")?;
            let slot = self
                .refs
                .get(usize::from(index))
                .filter(|slot| slot.valid)
                .ok_or({
                    D3d11Error::BrokenReference(BrokenReferenceReason::NamesEmptySlot {
                        reference: "film grain",
                    })
                })?;
            let mut grain = slot.grain;
            grain.grain_seed = u16::try_from(seed).map_err(|_| {
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "grain_seed",
                })
            })?;
            self.header.film_grain = grain;
            return Ok(());
        }
        let mut grain = FilmGrain {
            flags: FG_APPLY,
            grain_seed: u16::try_from(seed).map_err(|_| {
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "grain_seed",
                })
            })?,
            ..FilmGrain::default()
        };
        self.parse_grain_scaling(&mut grain)?;
        self.parse_grain_coefficients(&mut grain)?;
        self.header.film_grain = grain;
        Ok(())
    }

    /// Film grain scaling points.
    fn parse_grain_scaling(&mut self, grain: &mut FilmGrain) -> Result<(), D3d11Error> {
        grain.num_y_points = read_small(&mut self.reader, 4, "num_y_points")?;
        if grain.num_y_points > 14 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "num_y_points",
            }));
        }
        for index in 0..usize::from(grain.num_y_points) {
            grain.scaling_points_y[index] = self.parse_scaling_point(
                usize::from(grain.num_y_points),
                index,
                index
                    .checked_sub(1)
                    .map(|prev| grain.scaling_points_y[prev][0]),
            )?;
        }
        if self.seq.num_planes() > 1 && self.reader.read_bit("chroma_scaling_from_luma")? {
            grain.flags |= FG_CHROMA_FROM_LUMA;
        }
        if self.seq.num_planes() == 1
            || grain.chroma_scaling_from_luma()
            || (grain.num_y_points == 0)
        {
            return Ok(());
        }
        grain.num_cb_points = read_small(&mut self.reader, 4, "num_cb_points")?;
        if grain.num_cb_points > 10 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "num_cb_points",
            }));
        }
        for index in 0..usize::from(grain.num_cb_points) {
            grain.scaling_points_cb[index] = self.parse_scaling_point(
                usize::from(grain.num_cb_points),
                index,
                index
                    .checked_sub(1)
                    .map(|prev| grain.scaling_points_cb[prev][0]),
            )?;
        }
        grain.num_cr_points = read_small(&mut self.reader, 4, "num_cr_points")?;
        if grain.num_cr_points > 10 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "num_cr_points",
            }));
        }
        for index in 0..usize::from(grain.num_cr_points) {
            grain.scaling_points_cr[index] = self.parse_scaling_point(
                usize::from(grain.num_cr_points),
                index,
                index
                    .checked_sub(1)
                    .map(|prev| grain.scaling_points_cr[prev][0]),
            )?;
        }
        Ok(())
    }

    /// One film grain scaling point with range checks.
    fn parse_scaling_point(
        &mut self,
        count: usize,
        index: usize,
        previous: Option<u8>,
    ) -> Result<[u8; 2], D3d11Error> {
        let value = read_small(&mut self.reader, 8, "point_value")?;
        let floor = previous.map_or(0, |prev| u16::from(prev) + 1);
        let ceiling = 255 - u16::try_from(count - index - 1).unwrap_or(255);
        if u16::from(value) < floor || u16::from(value) > ceiling {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "point_value",
            }));
        }
        let scaling = read_small(&mut self.reader, 8, "point_scaling")?;
        Ok([value, scaling])
    }

    /// Film grain coefficients and multipliers.
    fn parse_grain_coefficients(&mut self, grain: &mut FilmGrain) -> Result<(), D3d11Error> {
        grain.grain_scaling_minus_8 = read_small(&mut self.reader, 2, "grain_scaling_minus_8")?;
        grain.ar_coeff_lag = read_small(&mut self.reader, 2, "ar_coeff_lag")?;
        let num_pos_luma =
            2 * usize::from(grain.ar_coeff_lag) * (usize::from(grain.ar_coeff_lag) + 1);
        let num_pos_chroma = if grain.num_y_points > 0 {
            for slot in grain.ar_coeffs_y.iter_mut().take(num_pos_luma) {
                *slot = read_small(&mut self.reader, 8, "ar_coeffs_y")?;
            }
            num_pos_luma + 1
        } else {
            num_pos_luma
        };
        if grain.chroma_scaling_from_luma() || grain.num_cb_points > 0 {
            for slot in grain.ar_coeffs_cb.iter_mut().take(num_pos_chroma) {
                *slot = read_small(&mut self.reader, 8, "ar_coeffs_cb")?;
            }
        }
        if grain.chroma_scaling_from_luma() || grain.num_cr_points > 0 {
            for slot in grain.ar_coeffs_cr.iter_mut().take(num_pos_chroma) {
                *slot = read_small(&mut self.reader, 8, "ar_coeffs_cr")?;
            }
        }
        grain.ar_coeff_shift_minus_6 = read_small(&mut self.reader, 2, "ar_coeff_shift_minus_6")?;
        grain.grain_scale_shift = read_small(&mut self.reader, 2, "grain_scale_shift")?;
        if grain.num_cb_points > 0 {
            grain.cb_mult = read_small(&mut self.reader, 8, "cb_mult")?;
            grain.cb_luma_mult = read_small(&mut self.reader, 8, "cb_luma_mult")?;
            grain.cb_offset = self.reader.read_bits(9, "cb_offset").and_then(|offset| {
                u16::try_from(offset).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "cb_offset",
                    })
                })
            })?;
        }
        if grain.num_cr_points > 0 {
            grain.cr_mult = read_small(&mut self.reader, 8, "cr_mult")?;
            grain.cr_luma_mult = read_small(&mut self.reader, 8, "cr_luma_mult")?;
            grain.cr_offset = self.reader.read_bits(9, "cr_offset").and_then(|offset| {
                u16::try_from(offset).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "cr_offset",
                    })
                })
            })?;
        }
        if self.reader.read_bit("overlap_flag")? {
            grain.flags |= FG_OVERLAP;
        }
        if self.reader.read_bit("clip_to_restricted_range")? {
            grain.flags |= FG_RESTRICTED_RANGE;
        }
        Ok(())
    }
}

/// Identity warp matrix.
fn identity_motion() -> [i32; 6] {
    [
        0,
        0,
        1 << WARPEDMODEL_PREC_BITS,
        0,
        0,
        1 << WARPEDMODEL_PREC_BITS,
    ]
}

/// Absolute and precision bits behind one motion parameter.
fn motion_precision(warp_type: u8, index: usize, high_precision: bool) -> (u32, u32) {
    if index >= 2 {
        return (GM_ABS_ALPHA_BITS, GM_ALPHA_PREC_BITS);
    }
    if warp_type == WARP_TRANSLATION {
        let adjust = u32::from(!high_precision);
        (
            GM_ABS_TRANS_ONLY_BITS - adjust,
            GM_TRANS_ONLY_PREC_BITS - adjust,
        )
    } else {
        (GM_ABS_TRANS_BITS, GM_TRANS_PREC_BITS)
    }
}

/// Reconstructed motion parameter from its subexponential symbol.
fn reconstruct_motion_param(
    symbol: u32,
    previous: i32,
    index: usize,
    abs_bits: u32,
    prec_bits: u32,
) -> i32 {
    let round = if index % 3 == 2 {
        1 << WARPEDMODEL_PREC_BITS
    } else {
        0
    };
    let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
    let sub = if index % 3 == 2 { 1 << prec_bits } else { 0 };
    let mx = 1 << abs_bits;
    let reference = (previous >> prec_diff) - sub;
    decode_signed_subexp(symbol, -mx, mx + 1, reference)
        .wrapping_shl(prec_diff)
        .wrapping_add(round)
}

/// Signed subexponential value against its reference.
fn decode_signed_subexp(sub_exp: u32, low: i32, high: i32, reference: i32) -> i32 {
    decode_unsigned_subexp(sub_exp, high - low, reference - low)
        .cast_signed()
        .wrapping_add(low)
}

/// Unsigned subexponential value against its reference.
fn decode_unsigned_subexp(sub_exp: u32, mx: i32, reference: i32) -> u32 {
    if reference.wrapping_mul(2) <= mx {
        inverse_recenter(reference, sub_exp)
    } else {
        (mx - 1)
            .wrapping_sub(inverse_recenter(mx - 1 - reference, sub_exp).cast_signed())
            .cast_unsigned()
    }
}

/// Reconstructed value around its center.
fn inverse_recenter(center: i32, value: u32) -> u32 {
    if value > center.wrapping_mul(2).cast_unsigned() {
        value
    } else if value & 1 == 1 {
        center
            .wrapping_sub(((value + 1) >> 1).cast_signed())
            .cast_unsigned()
    } else {
        center
            .wrapping_add((value >> 1).cast_signed())
            .cast_unsigned()
    }
}

/// True when the warp matrix passes the shear check.
fn shear_valid(params: &[i32; 6]) -> bool {
    if params[2] <= 0 {
        return false;
    }
    let alpha = reduce_warp_param(clip_i16(params[2] - (1 << WARPEDMODEL_PREC_BITS)));
    let beta = reduce_warp_param(clip_i16(params[3]));
    let mut shift = 0u32;
    let divisor = resolve_divisor(params[2], &mut shift);
    let magnitude = i64::from(params[4]).saturating_mul(1 << WARPEDMODEL_PREC_BITS);
    let product = i64::from(params[3]).saturating_mul(i64::from(params[4]));
    let gamma = reduce_warp_param(clip_i64(round_two_signed(
        magnitude.saturating_mul(i64::from(divisor)),
        shift,
    )));
    let delta = reduce_warp_param(clip_i64(
        i64::from(params[5])
            .saturating_sub(round_two_signed(
                product.saturating_mul(i64::from(divisor)),
                shift,
            ))
            .saturating_sub(1 << WARPEDMODEL_PREC_BITS),
    ));
    let high = 4 * warp_abs(alpha) + 7 * warp_abs(beta) >= 1 << WARPEDMODEL_PREC_BITS;
    let low = 4 * warp_abs(gamma) + 4 * warp_abs(delta) >= 1 << WARPEDMODEL_PREC_BITS;
    !high && !low
}

/// Warp parameter reduced to its check precision.
fn reduce_warp_param(value: i16) -> i16 {
    let reduced = round_two_signed(i64::from(value), WARP_PARAM_REDUCE_BITS)
        .saturating_mul(1 << WARP_PARAM_REDUCE_BITS);
    clip_i64(reduced)
}

/// Absolute value with the minimum wrapped.
fn warp_abs(value: i16) -> i32 {
    i32::from(value).wrapping_abs()
}

/// 16-bit clamping.
fn clip_i16(value: i32) -> i16 {
    i16::try_from(value).unwrap_or(if value < 0 { i16::MIN } else { i16::MAX })
}

/// 16-bit clamping from 64 bits.
fn clip_i64(value: i64) -> i16 {
    i16::try_from(value.clamp(i64::from(i16::MIN), i64::from(i16::MAX))).unwrap_or(0)
}

/// Divisor factor behind the shear check, storing its shift.
fn resolve_divisor(divisor: i32, shift: &mut u32) -> i16 {
    let divisor = u32::try_from(divisor.max(1)).unwrap_or(1);
    *shift = divisor.ilog2();
    let extra = divisor - (1 << *shift);
    let factor = if *shift > DIV_LUT_BITS {
        round_two(u64::from(extra), *shift - DIV_LUT_BITS)
    } else {
        u64::from(extra) << (DIV_LUT_BITS - *shift)
    };
    *shift += DIV_LUT_PREC_BITS;
    let index = usize::try_from(factor).unwrap_or(0).min(DIV_LUT.len() - 1);
    i16::try_from(DIV_LUT[index]).unwrap_or(i16::MAX)
}

/// Rounded division by a power of two.
fn round_two(value: u64, shift: u32) -> u64 {
    if shift == 0 {
        value
    } else {
        (value + (1 << (shift - 1))) >> shift
    }
}

/// Signed rounded division by a power of two.
fn round_two_signed(value: i64, shift: u32) -> i64 {
    if value < 0 {
        -round_two(value.unsigned_abs(), shift).cast_signed()
    } else {
        round_two(u64::try_from(value).unwrap_or(0), shift).cast_signed()
    }
}

/// One decoded tile: payload plus grid position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TileData {
    /// Tile payload bytes without the size field.
    pub(crate) bytes: Vec<u8>,
    /// Tile row.
    pub(crate) row: u16,
    /// Tile column.
    pub(crate) column: u16,
}

/// One coded frame: header plus tiles in tile order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Frame {
    /// Parsed frame header.
    pub(crate) header: FrameHeader,
    /// Tile payloads in tile order.
    pub(crate) tiles: Vec<TileData>,
}

/// OBU header with its payload span.
struct Obu<'a> {
    /// OBU type.
    unit_type: u8,
    /// Temporal id.
    temporal_id: u8,
    /// Spatial id.
    spatial_id: u8,
    /// Payload bytes.
    payload: &'a [u8],
}

/// Payload OBUs of `data` in order.
fn split_obus(data: &[u8]) -> Result<Vec<Obu<'_>>, D3d11Error> {
    let mut obus = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let header = data[offset];
        if header >> 7 != 0 {
            return Err(D3d11Error::Parse(ParseReason::ObuForbiddenBit));
        }
        let unit_type = (header >> 3) & 0x0F;
        let extension = header >> 2 & 1 != 0;
        let has_size = header >> 1 & 1 != 0;
        if header & 1 != 0 {
            return Err(D3d11Error::Parse(ParseReason::ObuReservedBit));
        }
        offset += 1;
        let (temporal_id, spatial_id) = if extension {
            let extension = *data.get(offset).ok_or({
                D3d11Error::Parse(ParseReason::Truncated {
                    syntax: "obu_extension_header",
                })
            })?;
            if extension & 7 != 0 {
                return Err(D3d11Error::Parse(ParseReason::ObuExtensionReservedBits));
            }
            offset += 1;
            (extension >> 5, (extension >> 3) & 3)
        } else {
            (0, 0)
        };
        let payload = if has_size {
            let mut size_reader = BitReader::new(&data[offset..]);
            let size = read_leb128(&mut size_reader, "obu_size")?;
            let consumed = size_reader.position() / 8;
            offset += consumed;
            let size = usize::try_from(size)
                .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "obu_size" }))?;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= data.len())
                .ok_or(D3d11Error::Parse(ParseReason::ObuSizeExceedsPayload))?;
            let payload = &data[offset..end];
            offset = end;
            payload
        } else {
            let payload = &data[offset..];
            offset = data.len();
            payload
        };
        obus.push(Obu {
            unit_type,
            temporal_id,
            spatial_id,
            payload,
        });
    }
    Ok(obus)
}

/// OBU ingest: sequence header plus frame assembly.
pub(crate) struct Ingest {
    /// Active sequence header.
    seq: Option<SequenceHeader>,
    /// Reference slots behind header parsing.
    refs: [RefInfo; NUM_REF_FRAMES],
    /// Pending frame header.
    pending_header: Option<FrameHeader>,
    /// Pending header payload bytes behind redundant comparison.
    pending_bytes: Option<Vec<u8>>,
    /// Pending tile payloads in tile order.
    pending_tiles: Vec<TileData>,
    /// Pending tiles expected from the grid.
    pending_expected: usize,
}

impl Ingest {
    /// Empty ingest.
    pub(crate) fn new() -> Self {
        Self {
            seq: None,
            refs: core::array::from_fn(|_| RefInfo::default()),
            pending_header: None,
            pending_bytes: None,
            pending_tiles: Vec::new(),
            pending_expected: 0,
        }
    }

    /// Active sequence header, if stored.
    pub(crate) fn seq(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// Frames completed by `data`; trailing tiles complete the frame.
    pub(crate) fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Frame>, D3d11Error> {
        let mut frames = Vec::new();
        for obu in split_obus(data)? {
            self.ingest_obu(&obu, &mut frames)?;
        }
        if let Some(frame) = self.complete_frame()? {
            frames.push(frame);
        }
        Ok(frames)
    }

    /// Open frame, if any header arrived without a boundary.
    pub(crate) fn finish(&mut self) -> Result<Option<Frame>, D3d11Error> {
        self.complete_frame()
    }

    /// Reference slots reset after a mid-stream failure.
    pub(crate) fn reset_refs(&mut self) {
        self.refs = core::array::from_fn(|_| RefInfo::default());
        self.pending_header = None;
        self.pending_bytes = None;
        self.pending_tiles.clear();
        self.pending_expected = 0;
    }

    /// One OBU into the ingest state.
    fn ingest_obu(&mut self, obu: &Obu, frames: &mut Vec<Frame>) -> Result<(), D3d11Error> {
        match obu.unit_type {
            OBU_SEQUENCE_HEADER => self.ingest_sequence(obu.payload),
            OBU_TEMPORAL_DELIMITER => {
                if let Some(frame) = self.complete_frame()? {
                    frames.push(frame);
                }
                Ok(())
            }
            OBU_FRAME_HEADER | OBU_REDUNDANT_FRAME_HEADER => self.ingest_frame_header(obu, frames),
            OBU_TILE_GROUP => self.ingest_tile_group(obu, frames),
            OBU_FRAME => self.ingest_frame(obu, frames),
            OBU_METADATA | OBU_PADDING => Ok(()),
            OBU_TILE_LIST => Err(D3d11Error::Unsupported(UnsupportedReason::TileList)),
            _ => Err(D3d11Error::Parse(ParseReason::ReservedObuType)),
        }
    }

    /// Sequence header into the store, resetting slots on change.
    fn ingest_sequence(&mut self, payload: &[u8]) -> Result<(), D3d11Error> {
        let seq = parse_sequence_header(payload)?;
        if self.seq.as_ref() != Some(&seq) {
            self.refs = core::array::from_fn(|_| RefInfo::default());
        }
        self.seq = Some(seq);
        Ok(())
    }

    /// Frame header into the pending frame.
    fn ingest_frame_header(
        &mut self,
        obu: &Obu,
        frames: &mut Vec<Frame>,
    ) -> Result<(), D3d11Error> {
        if obu.unit_type == OBU_REDUNDANT_FRAME_HEADER {
            if let Some(previous) = &self.pending_bytes {
                if previous.as_slice() != obu.payload {
                    return Err(D3d11Error::Parse(ParseReason::RedundantHeaderMismatch));
                }
                return Ok(());
            }
        }
        if self.pending_header.is_some() {
            if let Some(frame) = self.complete_frame()? {
                frames.push(frame);
            }
        }
        let (header, _) = self.parse_header(obu)?;
        if header.show_existing_frame() {
            frames.push(self.show_existing_frame(&header));
            return Ok(());
        }
        self.pending_expected = usize::from(header.tile_cols) * usize::from(header.tile_rows);
        self.pending_bytes = Some(obu.payload.to_vec());
        self.pending_header = Some(header);
        Ok(())
    }

    /// Tile group appended to the pending frame.
    fn ingest_tile_group(&mut self, obu: &Obu, frames: &mut Vec<Frame>) -> Result<(), D3d11Error> {
        let (tile_cols, tile_rows, cols_log2, rows_log2, tile_size_bytes) = self.pending_grid()?;
        let tile_count = usize::from(tile_cols) * usize::from(tile_rows);
        let (start, end, data) =
            Self::tile_group_range(obu.payload, tile_count, cols_log2, rows_log2)?;
        if start != self.pending_tiles.len() {
            return Err(D3d11Error::Parse(ParseReason::TileGroupOrder));
        }
        Self::push_tiles(
            &mut self.pending_tiles,
            tile_cols,
            tile_size_bytes,
            data,
            start,
            end,
        )?;
        if self.pending_tiles.len() == tile_count {
            if let Some(frame) = self.complete_frame()? {
                frames.push(frame);
            }
        }
        Ok(())
    }

    /// Tile range plus data behind the tile group header.
    fn tile_group_range(
        payload: &[u8],
        tile_count: usize,
        cols_log2: u8,
        rows_log2: u8,
    ) -> Result<(usize, usize, &[u8]), D3d11Error> {
        let mut reader = BitReader::new(payload);
        let mut start = 0usize;
        let mut end = tile_count - 1;
        if tile_count > 1 && reader.read_bit("tile_start_and_end_present_flag")? {
            let width = u32::from(cols_log2) + u32::from(rows_log2);
            start = usize::try_from(reader.read_bits(width, "tg_start")?)
                .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "tg_start" }))?;
            end = usize::try_from(reader.read_bits(width, "tg_end")?)
                .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "tg_end" }))?;
            if start > end || end >= tile_count {
                return Err(D3d11Error::Parse(ParseReason::Range {
                    syntax: "tile group",
                }));
            }
        }
        let aligned = reader.position().next_multiple_of(8);
        let data = payload
            .get(aligned / 8..)
            .ok_or(D3d11Error::Parse(ParseReason::TileGroupWithoutData))?;
        Ok((start, end, data))
    }

    /// Pending tile grid geometry.
    fn pending_grid(&self) -> Result<(u8, u8, u8, u8, u8), D3d11Error> {
        let header = self
            .pending_header
            .as_ref()
            .ok_or(D3d11Error::Parse(ParseReason::TileGroupWithoutFrameHeader))?;
        Ok((
            header.tile_cols,
            header.tile_rows,
            header.tile_cols_log2,
            header.tile_rows_log2,
            header.tile_size_bytes,
        ))
    }

    /// Frame OBU as one complete frame.
    fn ingest_frame(&mut self, obu: &Obu, frames: &mut Vec<Frame>) -> Result<(), D3d11Error> {
        if self.pending_header.is_some() {
            if let Some(frame) = self.complete_frame()? {
                frames.push(frame);
            }
        }
        let (header, consumed_bits) = self.parse_header(obu)?;
        if header.show_existing_frame() {
            frames.push(self.show_existing_frame(&header));
            return Ok(());
        }
        let consumed = consumed_bits.div_ceil(8);
        let Some(rest) = obu.payload.get(consumed..) else {
            return Err(D3d11Error::Parse(ParseReason::FrameWithoutTileData));
        };
        let tile_count = usize::from(header.tile_cols) * usize::from(header.tile_rows);
        let (start, end, data) = Self::tile_group_range(
            rest,
            tile_count,
            header.tile_cols_log2,
            header.tile_rows_log2,
        )?;
        self.pending_expected = tile_count;
        self.pending_bytes = Some(obu.payload.to_vec());
        Self::push_tiles(
            &mut self.pending_tiles,
            header.tile_cols,
            header.tile_size_bytes,
            data,
            start,
            end,
        )?;
        self.pending_header = Some(header);
        if let Some(frame) = self.complete_frame()? {
            frames.push(frame);
        }
        Ok(())
    }

    /// Frame header plus consumed bit count against the active sequence header.
    fn parse_header(&mut self, obu: &Obu) -> Result<(FrameHeader, usize), D3d11Error> {
        if obu.spatial_id != 0 {
            return Err(D3d11Error::Unsupported(UnsupportedReason::SpatialLayers));
        }
        let seq = self
            .seq
            .as_ref()
            .ok_or(D3d11Error::Parse(ParseReason::FrameBeforeSequenceHeader))?;
        let reader = HeaderReader::new(
            obu.payload,
            seq,
            &mut self.refs,
            obu.temporal_id,
            obu.spatial_id,
        );
        let (header, consumed) = reader.parse()?;
        if header.render_and_frame_size_different()
            && (header.render_width != header.frame_width
                || header.render_height != header.frame_height)
        {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::RenderSizeDiffersFromFrameSize,
            ));
        }
        if header.superres_denom != SUPERRES_NUM {
            return Err(D3d11Error::Unsupported(UnsupportedReason::SuperresUpscale));
        }
        Ok((header, consumed))
    }

    /// Tiles `start` through `end` appended from `data`.
    fn push_tiles(
        pending: &mut Vec<TileData>,
        tile_cols: u8,
        tile_size_bytes: u8,
        data: &[u8],
        start: usize,
        end: usize,
    ) -> Result<(), D3d11Error> {
        let mut offset = 0usize;
        let size_bytes = usize::from(tile_size_bytes);
        let cols = usize::from(tile_cols);
        for tile in start..=end {
            let tail = data.get(offset..).ok_or({
                D3d11Error::Parse(ParseReason::Truncated {
                    syntax: "tile data",
                })
            })?;
            let (bytes, consumed) = if tile == end {
                (tail, tail.len())
            } else {
                let size = tail.get(..size_bytes).ok_or({
                    D3d11Error::Parse(ParseReason::Truncated {
                        syntax: "tile size",
                    })
                })?;
                let mut value: u64 = 0;
                for (index, byte) in size.iter().enumerate() {
                    value |= u64::from(*byte) << (index * 8);
                }
                let length = usize::try_from(value + 1).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "tile size",
                    })
                })?;
                let bytes = tail
                    .get(size_bytes..)
                    .and_then(|tail| tail.get(..length))
                    .ok_or({
                        D3d11Error::Parse(ParseReason::Truncated {
                            syntax: "tile data",
                        })
                    })?;
                let consumed = size_bytes.checked_add(length).ok_or({
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "tile size",
                    })
                })?;
                (bytes, consumed)
            };
            offset = offset.checked_add(consumed).ok_or({
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "tile size",
                })
            })?;
            let row = u16::try_from(tile / cols)
                .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "tile row" }))?;
            let column = u16::try_from(tile % cols).map_err(|_| {
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "tile column",
                })
            })?;
            pending.push(TileData {
                bytes: bytes.to_vec(),
                row,
                column,
            });
        }
        Ok(())
    }

    /// Display-only frame for an existing slot.
    fn show_existing_frame(&mut self, header: &FrameHeader) -> Frame {
        if header.frame_type == FrameType::Key {
            let shown = self.refs[usize::from(header.frame_to_show_map_idx)].clone();
            for slot in &mut self.refs {
                *slot = shown.clone();
            }
        }
        Frame {
            header: header.clone(),
            tiles: Vec::new(),
        }
    }

    /// Pending header plus tiles as one validated frame.
    fn complete_frame(&mut self) -> Result<Option<Frame>, D3d11Error> {
        let Some(header) = self.pending_header.take() else {
            return Ok(None);
        };
        self.pending_bytes = None;
        if self.pending_tiles.len() != self.pending_expected {
            self.pending_tiles.clear();
            self.pending_expected = 0;
            return Err(D3d11Error::Parse(ParseReason::IncompleteTileGroup));
        }
        if self.pending_tiles.len() > MAX_TILES {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::TileCountAbove256,
            ));
        }
        let tiles = core::mem::take(&mut self.pending_tiles);
        self.pending_expected = 0;
        self.refresh_refs(&header);
        Ok(Some(Frame { header, tiles }))
    }

    /// Reference slots refreshed from the decoded header.
    fn refresh_refs(&mut self, header: &FrameHeader) {
        for (index, slot) in self.refs.iter_mut().enumerate() {
            if header.refresh_frame_flags >> index & 1 == 0 {
                continue;
            }
            slot.valid = true;
            slot.frame_type = header.frame_type;
            slot.frame_width = header.frame_width;
            slot.frame_height = header.frame_height;
            slot.upscaled_width = header.upscaled_width;
            slot.render_width = header.render_width;
            slot.render_height = header.render_height;
            slot.order_hint = header.order_hint;
            slot.frame_id = header.current_frame_id;
            slot.loop_ref_deltas = header.loop_ref_deltas;
            slot.loop_mode_deltas = header.loop_mode_deltas;
            slot.seg_enabled = header.seg_enabled;
            slot.seg_value = header.seg_value;
            for (position, motion) in header.global_motion.iter().enumerate() {
                slot.gm_params[position] = motion.params;
            }
            slot.grain = header.film_grain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        inverse_recenter, parse_sequence_header, read_leb128, read_ns, read_su, read_subexp,
        read_uvlc, reconstruct_motion_param, relative_dist, shear_valid, split_obus, tile_log2,
        Ingest, SequenceHeader, REF_ALTREF, REF_BWDREF,
    };
    use crate::bits::BitReader;

    /// Test bit writer.
    struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        fn bit(&mut self, set: bool) {
            if set {
                self.current |= 1 << (7 - self.filled);
            }
            self.filled += 1;
            if self.filled == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.filled = 0;
            }
        }

        /// Low `count` bits of `value`, most significant first.
        fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Byte alignment with zero bits.
        fn finish(mut self) -> Vec<u8> {
            while self.filled != 0 {
                self.bit(false);
            }
            self.bytes
        }
    }

    /// OBU with type, size field, and payload.
    fn obu(unit_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut obu = vec![unit_type << 3 | 1 << 1];
        let mut size = payload.len();
        loop {
            let mut byte = u8::try_from(size & 0x7F).expect("seven bits fit");
            size >>= 7;
            if size != 0 {
                byte |= 0x80;
            }
            obu.push(byte);
            if size == 0 {
                break;
            }
        }
        obu.extend_from_slice(payload);
        obu
    }

    /// 320x240 sequence header payload.
    fn seq_body() -> Vec<u8> {
        seq_body_opts(false)
    }

    /// 320x240 sequence header payload with superres selection.
    fn seq_body_opts(superres: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 5);
        writer.bits(0, 12);
        writer.bits(0, 5);
        writer.bits(8, 4);
        writer.bits(7, 4);
        writer.bits(319, 9);
        writer.bits(239, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bits(0, 3);
        writer.bit(superres);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 2);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// Sequence header used across tests.
    fn seq() -> SequenceHeader {
        parse_sequence_header(&seq_body()).expect("valid seq")
    }

    /// Key frame header body over the test sequence.
    fn key_body() -> Vec<u8> {
        key_body_opts(false, false, false)
    }

    /// Key frame header body with tile, render, and superres selection.
    fn key_body_opts(two_col: bool, render_diff: bool, use_superres: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(false);
        writer.bits(0, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        if use_superres {
            writer.bit(true);
            writer.bits(0, 3);
        }
        if render_diff {
            writer.bit(true);
            writer.bits(159, 16);
            writer.bits(119, 16);
        } else {
            writer.bit(false);
        }
        writer.bit(false);
        writer.bit(true);
        if two_col {
            writer.bit(true);
            writer.bit(false);
            writer.bit(false);
            writer.bits(0, 1);
            writer.bits(0, 2);
        } else {
            writer.bit(false);
            writer.bit(false);
        }
        writer.bits(100, 8);
        writer.bit(true);
        writer.bits(0x7D, 7);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(6, 6);
        writer.bits(6, 6);
        writer.bits(11, 6);
        writer.bits(17, 6);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.finish()
    }

    /// Inter frame header body with `order_hint` over slot zero.
    fn inter_body(order_hint: u32, short_signaling: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(false);
        writer.bits(1, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(order_hint, 1);
        writer.bits(7, 3);
        writer.bits(0x02, 8);
        if short_signaling {
            writer.bit(true);
            writer.bits(0, 3);
            writer.bits(0, 3);
        } else {
            writer.bit(false);
            for _ in 0..7 {
                writer.bits(0, 3);
            }
        }
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bits(100, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(6, 6);
        writer.bits(6, 6);
        writer.bits(11, 6);
        writer.bits(17, 6);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        for _ in 0..7 {
            writer.bit(false);
        }
        writer.finish()
    }

    /// Show-existing frame header body for `slot`.
    fn show_existing_body(slot: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bits(slot, 3);
        writer.finish()
    }

    /// Frame OBU with header plus one four-byte tile.
    fn frame_obu(header: &[u8]) -> Vec<u8> {
        let mut payload = header.to_vec();
        payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        obu(6, &payload)
    }

    /// Tile group OBU covering `start` through `end` with `tiles` payloads.
    fn tile_group_obu(start: usize, end: usize, tiles: &[&[u8]]) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bits(u32::try_from(start).expect("tile index fits"), 1);
        writer.bits(u32::try_from(end).expect("tile index fits"), 1);
        let mut payload = writer.finish();
        for (index, tile) in tiles.iter().enumerate() {
            if start + index != end {
                payload.push(u8::try_from(tile.len() - 1).expect("tile size fits"));
            }
            payload.extend_from_slice(tile);
        }
        obu(4, &payload)
    }

    #[test]
    fn seq_reports_tools_and_max() {
        let seq = seq();
        assert_eq!(seq.profile, 0);
        assert_eq!(seq.operating.len(), 1);
        assert_eq!((seq.max_frame_width(), seq.max_frame_height()), (320, 240));
        assert_eq!(seq.bit_depth(), 8);
        assert_eq!(seq.num_planes(), 3);
        assert!(seq.enable_order_hint());
        assert_eq!(seq.order_hint_bits(), 1);
        assert!(seq.subsampling_x());
        assert!(seq.subsampling_y());
        assert!(!seq.enable_cdef());
        assert!(!seq.film_grain_params_present());
    }

    #[test]
    fn seq_refuses_depth_and_chroma() {
        let mut writer = BitWriter::new();
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 5);
        writer.bits(0, 12);
        writer.bits(0, 5);
        writer.bits(8, 4);
        writer.bits(7, 4);
        writer.bits(319, 9);
        writer.bits(239, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        assert!(parse_sequence_header(&writer.finish()).is_err());

        let mut writer = BitWriter::new();
        writer.bits(1, 3);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 5);
        writer.bits(0, 12);
        writer.bits(0, 5);
        writer.bits(8, 4);
        writer.bits(7, 4);
        writer.bits(319, 9);
        writer.bits(239, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        assert!(parse_sequence_header(&writer.finish()).is_err());
    }

    #[test]
    fn key_frame_parses() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        let frames = ingest.ingest_packet(&data).expect("key packet");
        assert_eq!(frames.len(), 1);
        let header = &frames[0].header;
        assert_eq!(header.frame_type, super::FrameType::Key);
        assert!(header.show_frame());
        assert!(!header.showable_frame());
        assert_eq!(header.order_hint, 0);
        assert_eq!(header.refresh_frame_flags, 0xFF);
        assert_eq!((header.frame_width, header.frame_height), (320, 240));
        assert_eq!((header.tile_cols, header.tile_rows), (1, 1));
        assert_eq!(header.base_q_idx, 100);
        assert_eq!(header.delta_q_y_dc, -3);
        assert_eq!(header.loop_filter_level, [6, 6, 11, 17]);
        assert_eq!(header.tx_mode, 2);
        assert!(!header.segmentation_enabled());
        assert!(!header.film_grain.apply_grain());
        assert!(header.global_motion.iter().all(|motion| motion.invalid));
        assert_eq!(frames[0].tiles.len(), 1);
        assert_eq!(frames[0].tiles[0].bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn inter_frame_parses_with_refs() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        data.extend(frame_obu(&inter_body(1, false)));
        let frames = ingest.ingest_packet(&data).expect("inter packet");
        assert_eq!(frames.len(), 2);
        let header = &frames[1].header;
        assert_eq!(header.frame_type, super::FrameType::Inter);
        assert!(header.show_frame());
        assert_eq!(header.order_hint, 1);
        assert_eq!(header.refresh_frame_flags, 0x02);
        assert_eq!(header.ref_frame_idx, [0; 7]);
        assert!(header.allow_high_precision_mv());
        assert!(header.is_motion_mode_switchable());
        assert_eq!(header.interp_filter, 0);
        assert!(header.global_motion.iter().all(|motion| !motion.invalid));
        assert_eq!(
            header.global_motion[0].params,
            [0, 0, 1 << 16, 0, 0, 1 << 16]
        );
    }

    #[test]
    fn short_signaling_derives_refs() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        data.extend(frame_obu(&inter_body(1, true)));
        let frames = ingest.ingest_packet(&data).expect("short packet");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].header.ref_frame_idx, [0, 7, 6, 0, 5, 4, 3]);
    }

    #[test]
    fn tile_groups_assemble_two_columns() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(obu(3, &key_body_opts(true, false, false)));
        data.extend(tile_group_obu(0, 0, &[&[0xAA; 8]]));
        data.extend(tile_group_obu(1, 1, &[&[0xBB; 6]]));
        let frames = ingest.ingest_packet(&data).expect("tiled packet");
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!((frame.header.tile_cols, frame.header.tile_rows), (2, 1));
        assert_eq!(frame.header.tile_size_bytes, 1);
        assert_eq!(frame.tiles.len(), 2);
        assert_eq!((frame.tiles[0].row, frame.tiles[0].column), (0, 0));
        assert_eq!(frame.tiles[0].bytes, vec![0xAA; 8]);
        assert_eq!((frame.tiles[1].row, frame.tiles[1].column), (0, 1));
        assert_eq!(frame.tiles[1].bytes, vec![0xBB; 6]);
    }

    #[test]
    fn superres_and_render_decline() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body_opts(true));
        data.extend(frame_obu(&key_body_opts(false, false, true)));
        let error = ingest.ingest_packet(&data).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));

        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body_opts(false, true, false)));
        let error = ingest.ingest_packet(&data).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));
    }

    #[test]
    fn spatial_layers_and_tile_lists_decline() {
        let mut ingest = Ingest::new();
        let error = ingest.ingest_packet(&[0x36, 0x08, 0x00]).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));

        let mut ingest = Ingest::new();
        let error = ingest.ingest_packet(&obu(8, &[])).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));
    }

    #[test]
    fn show_existing_frame_parses() {
        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(frame_obu(&key_body()));
        data.extend(obu(3, &show_existing_body(0)));
        let frames = ingest.ingest_packet(&data).expect("show packet");
        assert_eq!(frames.len(), 2);
        let header = &frames[1].header;
        assert!(header.show_existing_frame());
        assert_eq!(header.frame_to_show_map_idx, 0);
        assert_eq!(header.frame_type, super::FrameType::Key);
        assert!(frames[1].tiles.is_empty(), "show existing carries no tiles");

        let mut ingest = Ingest::new();
        let mut data = obu(1, &seq_body());
        data.extend(obu(3, &show_existing_body(2)));
        let error = ingest.ingest_packet(&data).unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d11Error::BrokenReference(_)
        ));
    }

    #[test]
    fn split_obus_handles_extension_and_size() {
        let mut data = obu(1, &seq_body());
        data.extend([0x36, 0x40, 0x03, 0xAA, 0xBB, 0xCC]);
        data.extend([0x10]);
        let obus = split_obus(&data).expect("obus split");
        assert_eq!(obus.len(), 3);
        assert_eq!(obus[0].unit_type, 1);
        assert_eq!(obus[1].unit_type, 6);
        assert_eq!((obus[1].temporal_id, obus[1].spatial_id), (2, 0));
        assert_eq!(obus[1].payload, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(obus[2].unit_type, 2);
        assert!(obus[2].payload.is_empty(), "delimiter carries no payload");
    }

    #[test]
    fn readers_match_spec_vectors() {
        let mut reader = BitReader::new(&[0x20]);
        assert_eq!(read_uvlc(&mut reader, "uvlc").expect("uvlc"), 3);
        let mut reader = BitReader::new(&[0xAC, 0x02]);
        assert_eq!(read_leb128(&mut reader, "leb128").expect("leb128"), 300);
        let mut reader = BitReader::new(&[0x00]);
        assert_eq!(read_ns(&mut reader, 5, "ns").expect("ns"), 0);
        let mut reader = BitReader::new(&[0xE0]);
        assert_eq!(read_ns(&mut reader, 5, "ns").expect("ns"), 4);
        let mut reader = BitReader::new(&[0x80]);
        assert_eq!(
            super::read_increment(&mut reader, 1, 3, "inc").expect("inc"),
            2
        );
        let mut reader = BitReader::new(&[0x00]);
        assert_eq!(read_subexp(&mut reader, 1025, "subexp").expect("subexp"), 0);
        let mut reader = BitReader::new(&[0x06, 0xFA]);
        assert_eq!(read_su(&mut reader, 7, "su").expect("su"), 3);
        let mut reader = BitReader::new(&[0xFA, 0x00]);
        assert_eq!(read_su(&mut reader, 7, "su").expect("su"), -3);
        assert_eq!(tile_log2(64, 5), 0);
        assert_eq!(tile_log2(1, 5), 3);
        assert_eq!(tile_log2(32, 33), 1);
        assert_eq!(relative_dist(6, 0, 6), 6);
        assert_eq!(relative_dist(0, 6, 6), -6);
    }

    #[test]
    fn motion_reconstruction_roundtrips() {
        assert_eq!(reconstruct_motion_param(0, 0, 0, 9, 3), 0);
        assert_eq!(inverse_recenter(512, 0), 512);
        assert_eq!(inverse_recenter(512, 1), 511);
        assert!(shear_valid(&[0, 0, 1 << 16, 0, 0, 1 << 16]));
        assert!(!shear_valid(&[0, 0, 0, 0, 0, 0]));
        assert_eq!((REF_BWDREF, REF_ALTREF), (5, 7));
    }
}
