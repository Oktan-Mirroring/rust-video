//! AV1 frame headers plus reference slots.

use crate::bits::BitReader;
use crate::error::{BrokenReferenceReason, Error, ParseReason};
use crate::sequence::SequenceHeader;
use crate::syntax::{
    read_increment, read_ns, read_small, read_su, read_subexp, relative_dist, tile_log2,
};
use crate::{
    MAX_SEGMENTS, NUM_REF_FRAMES, REFS_PER_FRAME, SEG_LEVELS, SELECT_INTEGER_MV,
    SELECT_SCREEN_CONTENT_TOOLS, SUPERRES_NUM, TOTAL_REFS,
};

/// Last reference slot.
pub(crate) const REF_LAST: usize = 1;

/// Second last reference slot.
pub(crate) const REF_LAST2: usize = 2;

/// Third last reference slot.
pub(crate) const REF_LAST3: usize = 3;

/// Golden reference slot.
pub(crate) const REF_GOLDEN: usize = 4;

/// Backward reference slot.
pub(crate) const REF_BWDREF: usize = 5;

/// Second alt reference slot.
pub(crate) const REF_ALTREF2: usize = 6;

/// Alt reference slot.
pub(crate) const REF_ALTREF: usize = 7;
/// Tile column ceiling.
const MAX_TILE_COLS: u32 = 64;

/// Tile row ceiling.
const MAX_TILE_ROWS: u32 = 64;

/// Tile width ceiling in luma samples.
const MAX_TILE_WIDTH: u32 = 4096;

/// Tile area ceiling in luma samples.
const MAX_TILE_AREA: u32 = 4096 * 2304;

/// Superres denominator floor.
pub const SUPERRES_DENOM_MIN: u8 = 9;

/// No primary reference.
const PRIMARY_REF_NONE: u8 = 7;

/// Switchable interpolation filter.
pub const INTERP_SWITCHABLE: u8 = 4;

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
/// Frame type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FrameType {
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
    ///
    /// # Errors
    ///
    /// Returns `Err` when `value` exceeds 3.
    pub fn new<E: From<Error>>(value: u32) -> Result<Self, E> {
        match value {
            0 => Ok(Self::Key),
            1 => Ok(Self::Inter),
            2 => Ok(Self::IntraOnly),
            3 => Ok(Self::Switch),
            _ => Err(Error::parse(ParseReason::Range {
                syntax: "frame_type",
            })),
        }
    }

    /// True for key and intra-only frames.
    #[must_use]
    pub const fn is_intra(self) -> bool {
        matches!(self, Self::Key | Self::IntraOnly)
    }

    /// DXVA frame type number.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Key => 0,
            Self::Inter => 1,
            Self::IntraOnly => 2,
            Self::Switch => 3,
        }
    }
}

/// Show existing frame flag bit.
pub const FH_SHOW_EXISTING: u32 = 1;
/// Show frame flag bit.
pub const FH_SHOW_FRAME: u32 = 1 << 1;
/// Showable frame flag bit.
pub const FH_SHOWABLE_FRAME: u32 = 1 << 2;
/// Error resilient mode flag bit.
pub const FH_ERROR_RESILIENT: u32 = 1 << 3;
/// CDF update disabled flag bit.
pub const FH_DISABLE_CDF_UPDATE: u32 = 1 << 4;
/// Screen content tools flag bit.
pub const FH_SCREEN_CONTENT_TOOLS: u32 = 1 << 5;
/// Integer motion vectors flag bit.
pub const FH_FORCE_INTEGER_MV: u32 = 1 << 6;
/// Frame size override flag bit.
pub const FH_SIZE_OVERRIDE: u32 = 1 << 7;
/// Short reference signaling flag bit.
pub const FH_SHORT_SIGNALING: u32 = 1 << 8;
/// Render size differs flag bit.
pub const FH_RENDER_DIFFERENT: u32 = 1 << 9;
/// Intra block copy flag bit.
pub const FH_ALLOW_INTRABC: u32 = 1 << 10;
/// High precision motion vectors flag bit.
pub const FH_HIGH_PRECISION_MV: u32 = 1 << 11;
/// Switchable motion mode flag bit.
pub const FH_MOTION_MODE_SWITCHABLE: u32 = 1 << 12;
/// Reference motion vectors flag bit.
pub const FH_USE_REF_MVS: u32 = 1 << 13;
/// Frame-end CDF update disabled flag bit.
pub const FH_DISABLE_FRAME_END_UPDATE_CDF: u32 = 1 << 14;
/// UV delta differs flag bit.
pub const FH_DIFF_UV_DELTA: u32 = 1 << 16;
/// Quantizer matrix flag bit.
pub const FH_USING_QMATRIX: u32 = 1 << 17;
/// Segmentation enabled flag bit.
pub const FH_SEG_ENABLED: u32 = 1 << 18;
/// Segmentation map update flag bit.
pub const FH_SEG_UPDATE_MAP: u32 = 1 << 19;
/// Segmentation data update flag bit.
pub const FH_SEG_UPDATE_DATA: u32 = 1 << 20;
/// Segmentation temporal update flag bit.
pub const FH_SEG_TEMPORAL_UPDATE: u32 = 1 << 21;
/// Delta quantizer present flag bit.
pub const FH_DELTA_Q_PRESENT: u32 = 1 << 22;
/// Delta loop filter present flag bit.
pub const FH_DELTA_LF_PRESENT: u32 = 1 << 23;
/// Multi-delta loop filter flag bit.
pub const FH_DELTA_LF_MULTI: u32 = 1 << 24;
/// Loop filter delta enabled flag bit.
pub const FH_LOOP_DELTA_ENABLED: u32 = 1 << 25;
/// Loop filter delta update flag bit.
pub const FH_LOOP_DELTA_UPDATE: u32 = 1 << 26;
/// Compound reference mode flag bit.
pub const FH_REFERENCE_SELECT: u32 = 1 << 27;
/// Skip mode present flag bit.
pub const FH_SKIP_MODE_PRESENT: u32 = 1 << 28;
/// Warped motion flag bit.
pub const FH_ALLOW_WARPED_MOTION: u32 = 1 << 29;
/// Reduced transform set flag bit.
pub const FH_REDUCED_TX_SET: u32 = 1 << 30;

/// Global motion behind one reference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GlobalMotion {
    /// Warp model.
    pub warp_type: u8,
    /// Warp matrix.
    pub params: [i32; 6],
    /// Parameters invalid.
    pub invalid: bool,
}

/// Film grain applied flag bit.
pub const FG_APPLY: u8 = 1;
/// Film grain chroma from luma flag bit.
pub const FG_CHROMA_FROM_LUMA: u8 = 1 << 1;
/// Film grain overlap flag bit.
pub const FG_OVERLAP: u8 = 1 << 2;
/// Film grain restricted range flag bit.
pub const FG_RESTRICTED_RANGE: u8 = 1 << 3;

/// Film grain parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilmGrain {
    /// Packed grain flags.
    pub flags: u8,
    /// Grain parameters update.
    pub update_grain: bool,
    /// Reference slot for inherited parameters.
    pub ref_idx: u8,
    /// Grain seed.
    pub grain_seed: u16,
    /// Y scaling points.
    pub scaling_points_y: [[u8; 2]; 14],
    /// Y scaling point count.
    pub num_y_points: u8,
    /// Cb scaling points.
    pub scaling_points_cb: [[u8; 2]; 10],
    /// Cb scaling point count.
    pub num_cb_points: u8,
    /// Cr scaling points.
    pub scaling_points_cr: [[u8; 2]; 10],
    /// Cr scaling point count.
    pub num_cr_points: u8,
    /// Y autoregressive coefficients.
    pub ar_coeffs_y: [u8; 24],
    /// Cb autoregressive coefficients.
    pub ar_coeffs_cb: [u8; 25],
    /// Cr autoregressive coefficients.
    pub ar_coeffs_cr: [u8; 25],
    /// Grain scaling minus 8.
    pub grain_scaling_minus_8: u8,
    /// Coefficient lag.
    pub ar_coeff_lag: u8,
    /// Coefficient shift minus 6.
    pub ar_coeff_shift_minus_6: u8,
    /// Grain scale shift.
    pub grain_scale_shift: u8,
    /// Cb multiplier.
    pub cb_mult: u8,
    /// Cb luma multiplier.
    pub cb_luma_mult: u8,
    /// Cr multiplier.
    pub cr_mult: u8,
    /// Cr luma multiplier.
    pub cr_luma_mult: u8,
    /// Cb offset.
    pub cb_offset: u16,
    /// Cr offset.
    pub cr_offset: u16,
}

impl Default for FilmGrain {
    /// Empty grain with update set.
    fn default() -> Self {
        Self {
            flags: 0,
            update_grain: true,
            ref_idx: 0,
            grain_seed: 0,
            scaling_points_y: [[0; 2]; 14],
            num_y_points: 0,
            scaling_points_cb: [[0; 2]; 10],
            num_cb_points: 0,
            scaling_points_cr: [[0; 2]; 10],
            num_cr_points: 0,
            ar_coeffs_y: [0; 24],
            ar_coeffs_cb: [0; 25],
            ar_coeffs_cr: [0; 25],
            grain_scaling_minus_8: 0,
            ar_coeff_lag: 0,
            ar_coeff_shift_minus_6: 0,
            grain_scale_shift: 0,
            cb_mult: 0,
            cb_luma_mult: 0,
            cr_mult: 0,
            cr_luma_mult: 0,
            cb_offset: 0,
            cr_offset: 0,
        }
    }
}

impl FilmGrain {
    /// Grain applied.
    #[must_use]
    pub const fn apply_grain(&self) -> bool {
        self.flags & FG_APPLY != 0
    }

    /// Chroma scaling from luma.
    #[must_use]
    pub const fn chroma_scaling_from_luma(&self) -> bool {
        self.flags & FG_CHROMA_FROM_LUMA != 0
    }

    /// Overlap flag.
    #[must_use]
    pub const fn overlap_flag(&self) -> bool {
        self.flags & FG_OVERLAP != 0
    }

    /// Restricted range flag.
    #[must_use]
    pub const fn clip_to_restricted_range(&self) -> bool {
        self.flags & FG_RESTRICTED_RANGE != 0
    }
}

/// Parsed frame header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    /// Packed frame flags.
    pub flags: u32,
    /// Frame type.
    pub frame_type: FrameType,
    /// Shown slot for existing-frame display.
    pub frame_to_show_map_idx: u8,
    /// Display frame id.
    pub display_frame_id: u32,
    /// Current frame id.
    pub current_frame_id: u32,
    /// Frame order hint.
    pub order_hint: u8,
    /// Primary reference frame.
    pub primary_ref_frame: u8,
    /// Refresh flags per slot.
    pub refresh_frame_flags: u8,
    /// Reference slot per position.
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    /// Reference order hints.
    pub ref_order_hint: [u8; NUM_REF_FRAMES],
    /// Coded frame width in luma samples.
    pub frame_width: u32,
    /// Coded frame height in luma samples.
    pub frame_height: u32,
    /// Upscaled width in luma samples.
    pub upscaled_width: u32,
    /// Render width in luma samples.
    pub render_width: u32,
    /// Render height in luma samples.
    pub render_height: u32,
    /// Superres denominator.
    pub superres_denom: u8,
    /// Tile column count.
    pub tile_cols: u8,
    /// Tile row count.
    pub tile_rows: u8,
    /// Tile column shift.
    pub tile_cols_log2: u8,
    /// Tile row shift.
    pub tile_rows_log2: u8,
    /// Context update tile id.
    pub context_update_tile_id: u16,
    /// Tile size field width in bytes.
    pub tile_size_bytes: u8,
    /// Tile column widths in superblocks minus one.
    pub width_in_sbs_minus_1: [u16; 64],
    /// Tile row heights in superblocks minus one.
    pub height_in_sbs_minus_1: [u16; 64],
    /// Base quantizer index.
    pub base_q_idx: u8,
    /// Y DC delta.
    pub delta_q_y_dc: i8,
    /// U DC delta.
    pub delta_q_u_dc: i8,
    /// U AC delta.
    pub delta_q_u_ac: i8,
    /// V DC delta.
    pub delta_q_v_dc: i8,
    /// V AC delta.
    pub delta_q_v_ac: i8,
    /// Y matrix id.
    pub qm_y: u8,
    /// U matrix id.
    pub qm_u: u8,
    /// V matrix id.
    pub qm_v: u8,
    /// Delta quantizer resolution.
    pub delta_q_res: u8,
    /// Delta loop filter resolution.
    pub delta_lf_res: u8,
    /// Loop filter levels.
    pub loop_filter_level: [u8; 4],
    /// Loop filter sharpness.
    pub loop_filter_sharpness: u8,
    /// Loop filter reference deltas.
    pub loop_ref_deltas: [i8; TOTAL_REFS],
    /// Loop filter mode deltas.
    pub loop_mode_deltas: [i8; 2],
    /// Segmentation feature enables per segment and level.
    pub seg_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Segmentation feature values per segment and level.
    pub seg_value: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
    /// CDEF damping minus 3.
    pub cdef_damping_minus_3: u8,
    /// CDEF strength bits.
    pub cdef_bits: u8,
    /// CDEF Y primary strengths.
    pub cdef_y_pri: [u8; 8],
    /// CDEF Y secondary strengths.
    pub cdef_y_sec: [u8; 8],
    /// CDEF UV primary strengths.
    pub cdef_uv_pri: [u8; 8],
    /// CDEF UV secondary strengths.
    pub cdef_uv_sec: [u8; 8],
    /// Restoration types per plane.
    pub lr_type: [u8; 3],
    /// Restoration unit shift.
    pub lr_unit_shift: u8,
    /// Restoration chroma shift.
    pub lr_uv_shift: u8,
    /// Transform mode.
    pub tx_mode: u8,
    /// Interpolation filter.
    pub interp_filter: u8,
    /// Global motion per reference.
    pub global_motion: [GlobalMotion; REFS_PER_FRAME],
    /// Film grain parameters.
    pub film_grain: FilmGrain,
    /// Buffer removal time present.
    pub buffer_removal_time_present: bool,
    /// Frame id delta minus one per reference.
    pub delta_frame_id_minus1: [u32; REFS_PER_FRAME],
    /// Size inherited from the named reference.
    pub found_ref: [bool; REFS_PER_FRAME],
    /// Last slot under short signaling.
    pub short_last_idx: u8,
    /// Golden slot under short signaling.
    pub short_golden_idx: u8,
    /// Presentation time in ticks.
    pub presentation_time: u32,
    /// Uniform tile spacing.
    pub uniform_tile_spacing: bool,
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
            buffer_removal_time_present: false,
            delta_frame_id_minus1: [0; REFS_PER_FRAME],
            found_ref: [false; REFS_PER_FRAME],
            short_last_idx: 0,
            short_golden_idx: 0,
            presentation_time: 0,
            uniform_tile_spacing: false,
        }
    }
}

impl FrameHeader {
    /// Show existing frame display.
    #[must_use]
    pub const fn show_existing_frame(&self) -> bool {
        self.flags & FH_SHOW_EXISTING != 0
    }

    /// Show frame display.
    #[must_use]
    pub const fn show_frame(&self) -> bool {
        self.flags & FH_SHOW_FRAME != 0
    }

    /// Showable frame.
    #[must_use]
    pub const fn showable_frame(&self) -> bool {
        self.flags & FH_SHOWABLE_FRAME != 0
    }

    /// Error resilient mode.
    #[must_use]
    pub const fn error_resilient_mode(&self) -> bool {
        self.flags & FH_ERROR_RESILIENT != 0
    }

    /// CDF update disabled.
    #[must_use]
    pub const fn disable_cdf_update(&self) -> bool {
        self.flags & FH_DISABLE_CDF_UPDATE != 0
    }

    /// Screen content tools.
    #[must_use]
    pub const fn allow_screen_content_tools(&self) -> bool {
        self.flags & FH_SCREEN_CONTENT_TOOLS != 0
    }

    /// Integer motion vectors.
    #[must_use]
    pub const fn force_integer_mv(&self) -> bool {
        self.flags & FH_FORCE_INTEGER_MV != 0
    }

    /// Frame size override.
    #[must_use]
    pub const fn frame_size_override_flag(&self) -> bool {
        self.flags & FH_SIZE_OVERRIDE != 0
    }

    /// Short reference signaling.
    #[must_use]
    pub const fn frame_refs_short_signaling(&self) -> bool {
        self.flags & FH_SHORT_SIGNALING != 0
    }

    /// Render size differs.
    #[must_use]
    pub const fn render_and_frame_size_different(&self) -> bool {
        self.flags & FH_RENDER_DIFFERENT != 0
    }

    /// Intra block copy.
    #[must_use]
    pub const fn allow_intrabc(&self) -> bool {
        self.flags & FH_ALLOW_INTRABC != 0
    }

    /// High precision motion vectors.
    #[must_use]
    pub const fn allow_high_precision_mv(&self) -> bool {
        self.flags & FH_HIGH_PRECISION_MV != 0
    }

    /// Switchable motion mode.
    #[must_use]
    pub const fn is_motion_mode_switchable(&self) -> bool {
        self.flags & FH_MOTION_MODE_SWITCHABLE != 0
    }

    /// Reference motion vectors.
    #[must_use]
    pub const fn use_ref_frame_mvs(&self) -> bool {
        self.flags & FH_USE_REF_MVS != 0
    }

    /// Frame-end CDF update disabled.
    #[must_use]
    pub const fn disable_frame_end_update_cdf(&self) -> bool {
        self.flags & FH_DISABLE_FRAME_END_UPDATE_CDF != 0
    }

    /// UV delta differs.
    #[must_use]
    pub const fn diff_uv_delta(&self) -> bool {
        self.flags & FH_DIFF_UV_DELTA != 0
    }

    /// Quantizer matrix.
    #[must_use]
    pub const fn using_qmatrix(&self) -> bool {
        self.flags & FH_USING_QMATRIX != 0
    }

    /// Segmentation enabled.
    #[must_use]
    pub const fn segmentation_enabled(&self) -> bool {
        self.flags & FH_SEG_ENABLED != 0
    }

    /// Segmentation map update.
    #[must_use]
    pub const fn segmentation_update_map(&self) -> bool {
        self.flags & FH_SEG_UPDATE_MAP != 0
    }

    /// Segmentation data update.
    #[must_use]
    pub const fn segmentation_update_data(&self) -> bool {
        self.flags & FH_SEG_UPDATE_DATA != 0
    }

    /// Segmentation temporal update.
    #[must_use]
    pub const fn segmentation_temporal_update(&self) -> bool {
        self.flags & FH_SEG_TEMPORAL_UPDATE != 0
    }

    /// Delta quantizer present.
    #[must_use]
    pub const fn delta_q_present(&self) -> bool {
        self.flags & FH_DELTA_Q_PRESENT != 0
    }

    /// Delta loop filter present.
    #[must_use]
    pub const fn delta_lf_present(&self) -> bool {
        self.flags & FH_DELTA_LF_PRESENT != 0
    }

    /// Multi-delta loop filter.
    #[must_use]
    pub const fn delta_lf_multi(&self) -> bool {
        self.flags & FH_DELTA_LF_MULTI != 0
    }

    /// Loop filter delta enabled.
    #[must_use]
    pub const fn loop_filter_delta_enabled(&self) -> bool {
        self.flags & FH_LOOP_DELTA_ENABLED != 0
    }

    /// Loop filter delta update.
    #[must_use]
    pub const fn loop_filter_delta_update(&self) -> bool {
        self.flags & FH_LOOP_DELTA_UPDATE != 0
    }

    /// Compound reference mode.
    #[must_use]
    pub const fn reference_select(&self) -> bool {
        self.flags & FH_REFERENCE_SELECT != 0
    }

    /// Skip mode present.
    #[must_use]
    pub const fn skip_mode_present(&self) -> bool {
        self.flags & FH_SKIP_MODE_PRESENT != 0
    }

    /// Warped motion.
    #[must_use]
    pub const fn allow_warped_motion(&self) -> bool {
        self.flags & FH_ALLOW_WARPED_MOTION != 0
    }

    /// Reduced transform set.
    #[must_use]
    pub const fn reduced_tx_set(&self) -> bool {
        self.flags & FH_REDUCED_TX_SET != 0
    }
}

/// Reference slot state behind frame header parsing.
#[derive(Clone)]
pub struct RefInfo {
    /// Slot holds a decodable picture.
    pub valid: bool,
    /// Stored frame type.
    pub frame_type: FrameType,
    /// Stored frame width in luma samples.
    pub frame_width: u32,
    /// Stored frame height in luma samples.
    pub frame_height: u32,
    /// Stored upscaled width in luma samples.
    pub upscaled_width: u32,
    /// Stored render width in luma samples.
    pub render_width: u32,
    /// Stored render height in luma samples.
    pub render_height: u32,
    /// Stored order hint.
    pub order_hint: u8,
    /// Stored frame id.
    pub frame_id: u32,
    /// Stored loop filter reference deltas.
    pub loop_ref_deltas: [i8; TOTAL_REFS],
    /// Stored loop filter mode deltas.
    pub loop_mode_deltas: [i8; 2],
    /// Stored segmentation enables.
    pub seg_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Stored segmentation values.
    pub seg_value: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
    /// Stored global motion parameters per reference.
    pub gm_params: [[i32; 6]; REFS_PER_FRAME],
    /// Stored film grain parameters.
    pub grain: FilmGrain,
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
    fn parse<E: From<Error>>(mut self) -> Result<(FrameHeader, usize), E> {
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
    fn parse_show_existing<E: From<Error>>(&mut self) -> Result<FrameHeader, E> {
        self.header.flags |= FH_SHOW_EXISTING;
        let index = read_small(&mut self.reader, 3, "frame_to_show_map_idx")?;
        let slot = self
            .refs
            .get(usize::from(index))
            .filter(|slot| slot.valid)
            .ok_or({
                Error::broken(BrokenReferenceReason::NamesEmptySlot {
                    reference: "show_existing_frame",
                })
            })?;
        self.header.frame_to_show_map_idx = index;
        self.header.frame_type = slot.frame_type;
        self.header.order_hint = slot.order_hint;
        if self.seq.decoder_model_present() && !self.seq.equal_picture_interval() {
            let width = u32::from(self.seq.presentation_length_minus1) + 1;
            self.header.presentation_time =
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
    fn parse_frame_type<E: From<Error>>(&mut self) -> Result<(), E> {
        let frame_type = FrameType::new(self.reader.read_bits(2, "frame_type")?)?;
        self.header.frame_type = frame_type;
        if self.reader.read_bit("show_frame")? {
            self.header.flags |= FH_SHOW_FRAME;
            if self.seq.decoder_model_present() && !self.seq.equal_picture_interval() {
                let width = u32::from(self.seq.presentation_length_minus1) + 1;
                self.header.presentation_time =
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
    fn parse_screen_content<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_frame_id<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_size_override<E: From<Error>>(&mut self) -> Result<(), E> {
        let override_flag = self.header.frame_type == FrameType::Switch
            || (!self.seq.reduced_header() && self.reader.read_bit("frame_size_override_flag")?);
        if override_flag {
            self.header.flags |= FH_SIZE_OVERRIDE;
        }
        Ok(())
    }

    /// Frame order hint.
    fn parse_order_hint<E: From<Error>>(&mut self) -> Result<(), E> {
        let bits = self.seq.order_hint_bits();
        if bits == 0 {
            return Ok(());
        }
        self.header.order_hint = read_small(&mut self.reader, u32::from(bits), "order_hint")?;
        Ok(())
    }

    /// Primary reference frame.
    fn parse_primary_ref<E: From<Error>>(&mut self) -> Result<(), E> {
        if self.header.frame_type.is_intra() || self.header.error_resilient_mode() {
            self.header.primary_ref_frame = PRIMARY_REF_NONE;
        } else {
            self.header.primary_ref_frame = read_small(&mut self.reader, 3, "primary_ref_frame")?;
        }
        Ok(())
    }

    /// Buffer removal times per matching operating point.
    fn parse_buffer_removal<E: From<Error>>(&mut self) -> Result<(), E> {
        if !self.seq.decoder_model_present() {
            return Ok(());
        }
        if !self.reader.read_bit("buffer_removal_time_present_flag")? {
            return Ok(());
        }
        self.header.buffer_removal_time_present = true;
        let width = u32::from(self.seq.buffer_removal_length_minus1) + 1;
        for point in &self.seq.operating {
            if point.decoder_model.is_none() {
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
    fn parse_refresh_flags<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_ref_order_hints<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_frame_size_and_refs<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_ref_frames<E: From<Error>>(&mut self) -> Result<(), E> {
        if self.seq.enable_order_hint() && self.reader.read_bit("frame_refs_short_signaling")? {
            self.header.flags |= FH_SHORT_SIGNALING;
            let last = read_small(&mut self.reader, 3, "last_frame_idx")?;
            let golden = read_small(&mut self.reader, 3, "golden_frame_idx")?;
            self.header.short_last_idx = last;
            self.header.short_golden_idx = golden;
            self.set_frame_refs(last, golden)?;
        } else {
            for index in 0..REFS_PER_FRAME {
                self.header.ref_frame_idx[index] =
                    read_small(&mut self.reader, 3, "ref_frame_idx")?;
                self.parse_delta_frame_id(index)?;
            }
        }
        if self.header.frame_refs_short_signaling() {
            for index in 0..REFS_PER_FRAME {
                self.parse_delta_frame_id(index)?;
            }
        }
        for index in 0..REFS_PER_FRAME {
            let slot = usize::from(self.header.ref_frame_idx[index]);
            if !self.refs.get(slot).is_some_and(|slot| slot.valid) {
                return Err(Error::broken(BrokenReferenceReason::NamesEmptySlot {
                    reference: "ref_frame_idx",
                }));
            }
        }
        Ok(())
    }

    /// One reference delta frame id.
    fn parse_delta_frame_id<E: From<Error>>(&mut self, index: usize) -> Result<(), E> {
        if !self.seq.frame_id_numbers_present() {
            return Ok(());
        }
        let width = u32::from(self.seq.delta_frame_id_length_minus2) + 2;
        self.header.delta_frame_id_minus1[index] =
            self.reader.read_bits(width, "delta_frame_id_minus1")?;
        Ok(())
    }

    /// Short-signaled reference slots from order hints.
    fn set_frame_refs<E: From<Error>>(&mut self, last: u8, golden: u8) -> Result<(), E> {
        for slot in [last, golden] {
            if !self
                .refs
                .get(usize::from(slot))
                .is_some_and(|slot| slot.valid)
            {
                return Err(Error::broken(BrokenReferenceReason::NamesEmptySlot {
                    reference: "short reference",
                }));
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
    fn parse_frame_size_with_refs<E: From<Error>>(&mut self) -> Result<(), E> {
        for position in 0..REFS_PER_FRAME {
            let found = self.reader.read_bit("found_ref")?;
            self.header.found_ref[position] = found;
            if !found {
                continue;
            }
            let slot = usize::from(self.header.ref_frame_idx[position]);
            let reference = self.refs.get(slot).filter(|slot| slot.valid).ok_or({
                Error::broken(BrokenReferenceReason::NamesEmptySlot {
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
    fn parse_frame_size<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_render_size<E: From<Error>>(&mut self) -> Result<(), E> {
        if !self.reader.read_bit("render_and_frame_size_different")? {
            self.header.render_width = self.header.upscaled_width;
            self.header.render_height = self.header.frame_height;
            return Ok(());
        }
        self.header.flags |= FH_RENDER_DIFFERENT;
        self.header.render_width = self.reader.read_bits(16, "render_width_minus_1")? + 1;
        self.header.render_height = self.reader.read_bits(16, "render_height_minus_1")? + 1;
        Ok(())
    }

    /// Superres denominator plus downscaled frame width.
    fn parse_superres<E: From<Error>>(&mut self) -> Result<(), E> {
        let mut denom = u32::from(SUPERRES_NUM);
        if self.seq.enable_superres() && self.reader.read_bit("use_superres")? {
            let coded = self.reader.read_bits(3, "coded_denom")?;
            denom = coded + u32::from(SUPERRES_DENOM_MIN);
        }
        self.header.superres_denom = u8::try_from(denom).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "coded_denom",
            })
        })?;
        self.header.upscaled_width = self.header.frame_width;
        self.header.frame_width =
            (self.header.upscaled_width * u32::from(SUPERRES_NUM) + denom / 2) / denom;
        Ok(())
    }

    /// Interpolation filter.
    fn parse_interpolation_filter<E: From<Error>>(&mut self) -> Result<(), E> {
        if self.reader.read_bit("is_filter_switchable")? {
            self.header.interp_filter = INTERP_SWITCHABLE;
        } else {
            self.header.interp_filter = read_small(&mut self.reader, 2, "interpolation_filter")?;
        }
        Ok(())
    }

    /// Frame-end CDF update flag.
    fn parse_frame_end_update<E: From<Error>>(&mut self) -> Result<(), E> {
        let disable = self.seq.reduced_header()
            || self.header.disable_cdf_update()
            || self.reader.read_bit("disable_frame_end_update_cdf")?;
        if disable {
            self.header.flags |= FH_DISABLE_FRAME_END_UPDATE_CDF;
        }
        Ok(())
    }

    /// Tile grid geometry.
    fn parse_tile_info<E: From<Error>>(&mut self) -> Result<(), E> {
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
            self.header.uniform_tile_spacing = true;
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
                Error::parse(ParseReason::Range {
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
    fn parse_uniform_tiles<E: From<Error>>(
        &mut self,
        sb_cols: u32,
        sb_rows: u32,
        min_cols: u32,
        max_cols: u32,
        max_rows: u32,
        min_tiles: u32,
    ) -> Result<(), E> {
        let cols_log2 = read_increment(&mut self.reader, min_cols, max_cols, "tile_cols_log2")?;
        let tile_width_sb = (sb_cols + (1 << cols_log2) - 1) >> cols_log2;
        let cols = sb_cols.div_ceil(tile_width_sb);
        let min_rows = min_tiles.saturating_sub(cols_log2);
        let rows_log2 = read_increment(&mut self.reader, min_rows, max_rows, "tile_rows_log2")?;
        let tile_height_sb = (sb_rows + (1 << rows_log2) - 1) >> rows_log2;
        let rows = sb_rows.div_ceil(tile_height_sb);
        self.header.tile_cols_log2 = u8::try_from(cols_log2).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile_cols_log2",
            })
        })?;
        self.header.tile_rows_log2 = u8::try_from(rows_log2).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile_rows_log2",
            })
        })?;
        self.header.tile_cols = u8::try_from(cols).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile_cols",
            })
        })?;
        self.header.tile_rows = u8::try_from(rows).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile_rows",
            })
        })?;
        let cols = usize::from(self.header.tile_cols);
        let rows = usize::from(self.header.tile_rows);
        let width_minus1 = u16::try_from(tile_width_sb - 1).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile width",
            })
        })?;
        let height_minus1 = u16::try_from(tile_height_sb - 1).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile height",
            })
        })?;
        for index in 0..cols {
            self.header.width_in_sbs_minus_1[index] = if index + 1 < cols {
                width_minus1
            } else {
                let last = sb_cols - u32::from(self.header.tile_cols - 1) * tile_width_sb - 1;
                u16::try_from(last).map_err(|_| {
                    Error::parse(ParseReason::Range {
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
                    Error::parse(ParseReason::Range {
                        syntax: "tile height",
                    })
                })?
            };
        }
        Ok(())
    }

    /// Explicit tile grid from superblock widths.
    fn parse_explicit_tiles<E: From<Error>>(
        &mut self,
        sb_cols: u32,
        sb_rows: u32,
        max_tile_width_sb: u32,
        min_log2_tiles: u32,
    ) -> Result<(), E> {
        let (cols, widest) = self.parse_tile_axis(sb_cols, max_tile_width_sb, true)?;
        let max_area = if min_log2_tiles > 0 {
            (sb_rows * sb_cols) >> (min_log2_tiles + 1)
        } else {
            sb_rows * sb_cols
        };
        let max_height_sb = (max_area / widest).max(1);
        let (rows, _) = self.parse_tile_axis(sb_rows, max_height_sb, false)?;
        self.header.tile_cols_log2 = u8::try_from(tile_log2(1, u32::from(cols))).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile_cols_log2",
            })
        })?;
        self.header.tile_rows_log2 = u8::try_from(tile_log2(1, u32::from(rows))).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile_rows_log2",
            })
        })?;
        self.header.tile_cols = cols;
        self.header.tile_rows = rows;
        Ok(())
    }

    /// One tile axis: count plus widest tile in superblocks.
    fn parse_tile_axis<E: From<Error>>(
        &mut self,
        sb_count: u32,
        max_extent_sb: u32,
        columns: bool,
    ) -> Result<(u8, u32), E> {
        let mut start_sb = 0u32;
        let mut count = 0u8;
        let mut widest = 0u32;
        while start_sb < sb_count {
            if u32::from(count) >= MAX_TILE_COLS {
                return Err(Error::parse(ParseReason::Range {
                    syntax: "tile count",
                }));
            }
            let ceiling = (sb_count - start_sb).min(max_extent_sb);
            let minus1 = read_ns(&mut self.reader, ceiling, "tile_width_minus_1")?;
            let size_sb = minus1 + 1;
            let stored = u16::try_from(minus1).map_err(|_| {
                Error::parse(ParseReason::Range {
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
            return Err(Error::parse(ParseReason::TileGridCoverage));
        }
        Ok((count, widest))
    }

    /// Quantization parameters.
    fn parse_quantization<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_delta_q_value<E: From<Error>>(&mut self, syntax: &'static str) -> Result<i8, E> {
        if !self.reader.read_bit("delta_coded")? {
            return Ok(0);
        }
        let value = read_su(&mut self.reader, 7, syntax)?;
        i8::try_from(value).map_err(|_| Error::parse(ParseReason::Range { syntax }))
    }

    /// Segmentation parameters.
    fn parse_segmentation<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_segment_feature<E: From<Error>>(
        &mut self,
        segment: usize,
        level: usize,
        bits: u8,
        signed: bool,
    ) -> Result<(), E> {
        if !self.header.segmentation_update_data() {
            let primary =
                usize::from(self.header.ref_frame_idx[usize::from(self.header.primary_ref_frame)]);
            let slot = self.refs.get(primary).filter(|slot| slot.valid).ok_or({
                Error::broken(BrokenReferenceReason::NamesEmptySlot {
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
            Error::parse(ParseReason::Range {
                syntax: "feature_value",
            })
        })?;
        Ok(())
    }

    /// Delta quantizer parameters.
    fn parse_delta_q<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_delta_lf<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_loop_filter<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_loop_delta<E: From<Error>>(
        &mut self,
        index: usize,
        reference: bool,
        default: i8,
    ) -> Result<(), E> {
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
            Error::parse(ParseReason::Range {
                syntax: "loop_filter_delta",
            })
        })?;
        self.store_loop_delta(index, reference, value);
        Ok(())
    }

    /// Inherited loop filter delta from the primary reference.
    fn inherited_loop_delta<E: From<Error>>(
        &self,
        index: usize,
        reference: bool,
        default: i8,
    ) -> Result<i8, E> {
        if self.header.primary_ref_frame == PRIMARY_REF_NONE {
            return Ok(default);
        }
        let primary =
            usize::from(self.header.ref_frame_idx[usize::from(self.header.primary_ref_frame)]);
        let slot = self.refs.get(primary).filter(|slot| slot.valid).ok_or({
            Error::broken(BrokenReferenceReason::NamesEmptySlot {
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
    fn parse_cdef<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_restoration<E: From<Error>>(&mut self) -> Result<(), E> {
        if self.all_lossless || self.header.allow_intrabc() || !self.seq.enable_restoration() {
            return Ok(());
        }
        let mut uses_lr = false;
        let mut uses_chroma_lr = false;
        for plane in 0..usize::from(self.seq.num_planes()) {
            let lr_type = read_small(&mut self.reader, 2, "lr_type")?;
            if !is_restoration_type(lr_type) {
                return Err(Error::parse(ParseReason::Range { syntax: "lr_type" }));
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
                    Error::parse(ParseReason::Range {
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
    fn parse_tx_mode<E: From<Error>>(&mut self) -> Result<(), E> {
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
            .map_err(|_| Error::parse(ParseReason::Range { syntax: "tx_mode" }))?;
        Ok(())
    }

    /// Frame reference mode.
    fn parse_reference_mode<E: From<Error>>(&mut self) -> Result<(), E> {
        if self.header.frame_type.is_intra() {
            return Ok(());
        }
        if self.reader.read_bit("reference_select")? {
            self.header.flags |= FH_REFERENCE_SELECT;
        }
        Ok(())
    }

    /// Skip mode parameters.
    fn parse_skip_mode<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_warped_motion<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_global_motion<E: From<Error>>(&mut self) -> Result<(), E> {
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
    fn parse_reference_motion<E: From<Error>>(&mut self, position: usize) -> Result<(), E> {
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
    fn parse_motion_param<E: From<Error>>(
        &mut self,
        warp_type: u8,
        position: usize,
        index: usize,
    ) -> Result<i32, E> {
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
    fn parse_film_grain<E: From<Error>>(&mut self) -> Result<(), E> {
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
                    Error::broken(BrokenReferenceReason::NamesEmptySlot {
                        reference: "film grain",
                    })
                })?;
            let mut grain = slot.grain;
            grain.grain_seed = u16::try_from(seed).map_err(|_| {
                Error::parse(ParseReason::Range {
                    syntax: "grain_seed",
                })
            })?;
            grain.update_grain = false;
            self.header.film_grain = grain;
            return Ok(());
        }
        let mut grain = FilmGrain {
            flags: FG_APPLY,
            update_grain: true,
            grain_seed: u16::try_from(seed).map_err(|_| {
                Error::parse(ParseReason::Range {
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
    fn parse_grain_scaling<E: From<Error>>(&mut self, grain: &mut FilmGrain) -> Result<(), E> {
        grain.num_y_points = read_small(&mut self.reader, 4, "num_y_points")?;
        if grain.num_y_points > 14 {
            return Err(Error::parse(ParseReason::Range {
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
            || (self.seq.subsampling_x() && self.seq.subsampling_y() && grain.num_y_points == 0)
        {
            return Ok(());
        }
        grain.num_cb_points = read_small(&mut self.reader, 4, "num_cb_points")?;
        if grain.num_cb_points > 10 {
            return Err(Error::parse(ParseReason::Range {
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
            return Err(Error::parse(ParseReason::Range {
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
    fn parse_scaling_point<E: From<Error>>(
        &mut self,
        count: usize,
        index: usize,
        previous: Option<u8>,
    ) -> Result<[u8; 2], E> {
        let value = read_small(&mut self.reader, 8, "point_value")?;
        let floor = previous.map_or(0, |prev| u16::from(prev) + 1);
        let ceiling = 255 - u16::try_from(count - index - 1).unwrap_or(255);
        if u16::from(value) < floor || u16::from(value) > ceiling {
            return Err(Error::parse(ParseReason::Range {
                syntax: "point_value",
            }));
        }
        let scaling = read_small(&mut self.reader, 8, "point_scaling")?;
        Ok([value, scaling])
    }

    /// Film grain coefficients and multipliers.
    fn parse_grain_coefficients<E: From<Error>>(&mut self, grain: &mut FilmGrain) -> Result<(), E> {
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
                    Error::parse(ParseReason::Range {
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
                    Error::parse(ParseReason::Range {
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

/// Frame header plus consumed bit count.
///
/// # Arguments
///
/// * `payload` - frame header OBU payload.
/// * `seq` - active sequence header.
/// * `refs` - reference slots behind inheritance and validation.
/// * `temporal_id` - temporal id of the header OBU.
/// * `spatial_id` - spatial id of the header OBU.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range syntax, or when a
/// reference names an empty slot.
pub fn parse_frame_header<E: From<Error>>(
    payload: &[u8],
    seq: &SequenceHeader,
    refs: &mut [RefInfo; NUM_REF_FRAMES],
    temporal_id: u8,
    spatial_id: u8,
) -> Result<(FrameHeader, usize), E> {
    HeaderReader::new(payload, seq, refs, temporal_id, spatial_id).parse()
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
pub(crate) fn reconstruct_motion_param(
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
pub(crate) fn inverse_recenter(center: i32, value: u32) -> u32 {
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
pub(crate) fn shear_valid(params: &[i32; 6]) -> bool {
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
