//! Video usability information.

use crate::bits::BitReader;
use crate::error::HevcError;
use crate::hrd::{parse_hrd, HrdParams};

/// Aspect-ratio block of one VUI record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AspectRatio {
    /// Aspect ratio present.
    pub present: bool,
    /// Aspect ratio idc.
    pub idc: u8,
    /// Sample aspect width.
    pub sar_width: u32,
    /// Sample aspect height.
    pub sar_height: u32,
}

/// Overscan block of one VUI record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Overscan {
    /// Overscan info present.
    pub present: bool,
    /// Overscan appropriate.
    pub appropriate: bool,
}

/// Video-signal block of one VUI record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VideoSignal {
    /// Video signal present.
    pub present: bool,
    /// Video format.
    pub format: u8,
    /// Full range.
    pub full_range: bool,
    /// Colour description present.
    pub colour_present: bool,
    /// Colour primaries.
    pub primaries: u8,
    /// Transfer characteristics.
    pub transfer: u8,
    /// Matrix coefficients.
    pub matrix: u8,
}

/// Chroma-location block of one VUI record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChromaLoc {
    /// Chroma location present.
    pub present: bool,
    /// Chroma top-field location.
    pub top: u32,
    /// Chroma bottom-field location.
    pub bottom: u32,
}

/// Timing block of one VUI record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VuiTiming {
    /// Timing present.
    pub present: bool,
    /// Ticks per second.
    pub num_units_in_tick: u32,
    /// Time scale.
    pub time_scale: u32,
    /// POC proportional to timing.
    pub proportional: bool,
    /// Ticks per POC step minus one.
    pub num_ticks_poc_diff_one_minus1: u32,
}

/// VUI bitstream restriction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitstreamRestriction {
    /// Tiles fixed structure.
    pub tiles_fixed: bool,
    /// Motion vectors over picture boundaries.
    pub mv_over_pic: bool,
    /// Restricted reference pictures.
    pub restricted_ref: bool,
    /// Minimum spatial segmentation idc.
    pub min_spatial_segmentation: u32,
    /// Maximum bytes per picture denominator.
    pub max_bytes_per_pic_denom: u32,
    /// Maximum bits per MCU denominator.
    pub max_bits_per_mcu_denom: u32,
    /// Horizontal motion vector length bits.
    pub max_mv_horizontal: u32,
    /// Vertical motion vector length bits.
    pub max_mv_vertical: u32,
}

/// VUI parameters of one sequence set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VuiParams {
    /// Aspect-ratio block.
    pub aspect: AspectRatio,
    /// Overscan block.
    pub overscan: Overscan,
    /// Video-signal block.
    pub signal: VideoSignal,
    /// Chroma-location block.
    pub chroma_loc: ChromaLoc,
    /// Neutral chroma indication.
    pub neutral_chroma: bool,
    /// Field sequence flag.
    pub field_seq: bool,
    /// Frame field info present.
    pub frame_field_info: bool,
    /// Default display window left, right, top, bottom.
    pub display_window: Option<[u32; 4]>,
    /// Timing block.
    pub timing: VuiTiming,
    /// HRD parameters, when present.
    pub hrd: Option<HrdParams>,
    /// Bitstream restriction, when present.
    pub restriction: Option<BitstreamRestriction>,
}

/// Video usability information, H.265 E.2.1.
///
/// # Errors
///
/// Returns `Err` on malformed syntax.
///
/// # Arguments
///
/// * `reader` - bit reader over the VUI syntax.
/// * `max_sub_layers` - sublayer count behind HRD schedules.
pub fn parse_vui<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    max_sub_layers: u8,
) -> Result<VuiParams, E> {
    let aspect = parse_vui_aspect(reader)?;
    let overscan = parse_vui_overscan(reader)?;
    let signal = parse_vui_signal(reader)?;
    let chroma_loc = parse_vui_chroma_loc(reader)?;
    let neutral_chroma = reader.read_bit("neutral_chroma_indication_flag")?;
    let field_seq = reader.read_bit("field_seq_flag")?;
    let frame_field_info = reader.read_bit("frame_field_info_present_flag")?;
    let display_window = parse_vui_window(reader)?;
    let (timing, hrd) = parse_vui_timing(reader, max_sub_layers)?;
    let restriction = parse_vui_restriction(reader)?;
    Ok(VuiParams {
        aspect,
        overscan,
        signal,
        chroma_loc,
        neutral_chroma,
        field_seq,
        frame_field_info,
        display_window,
        timing,
        hrd,
        restriction,
    })
}

/// VUI aspect ratio section.
fn parse_vui_aspect<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<AspectRatio, E> {
    let present = reader.read_bit("aspect_ratio_info_present_flag")?;
    let mut aspect = AspectRatio {
        present,
        ..Default::default()
    };
    if !aspect.present {
        return Ok(aspect);
    }
    aspect.idc = narrow_u8(reader.read_bits(8, "aspect_ratio_idc")?);
    if aspect.idc == 255 {
        aspect.sar_width = reader.read_bits(16, "sar_width")?;
        aspect.sar_height = reader.read_bits(16, "sar_height")?;
    }
    Ok(aspect)
}

/// VUI overscan section.
fn parse_vui_overscan<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<Overscan, E> {
    let present = reader.read_bit("overscan_info_present_flag")?;
    let mut overscan = Overscan {
        present,
        ..Default::default()
    };
    if overscan.present {
        overscan.appropriate = reader.read_bit("overscan_appropriate_flag")?;
    }
    Ok(overscan)
}

/// VUI video signal section.
fn parse_vui_signal<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<VideoSignal, E> {
    let present = reader.read_bit("video_signal_type_present_flag")?;
    let mut signal = VideoSignal {
        present,
        ..Default::default()
    };
    if !signal.present {
        return Ok(signal);
    }
    signal.format = narrow_u8(reader.read_bits(3, "video_format")?);
    signal.full_range = reader.read_bit("video_full_range_flag")?;
    signal.colour_present = reader.read_bit("colour_description_present_flag")?;
    if signal.colour_present {
        signal.primaries = narrow_u8(reader.read_bits(8, "colour_primaries")?);
        signal.transfer = narrow_u8(reader.read_bits(8, "transfer_characteristics")?);
        signal.matrix = narrow_u8(reader.read_bits(8, "matrix_coefficients")?);
    }
    Ok(signal)
}

/// VUI chroma location section.
fn parse_vui_chroma_loc<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<ChromaLoc, E> {
    let present = reader.read_bit("chroma_loc_info_present_flag")?;
    let mut loc = ChromaLoc {
        present,
        ..Default::default()
    };
    if !loc.present {
        return Ok(loc);
    }
    loc.top = reader.read_ue("chroma_sample_loc_type_top_field")?;
    loc.bottom = reader.read_ue("chroma_sample_loc_type_bottom_field")?;
    Ok(loc)
}

/// VUI default display window section.
fn parse_vui_window<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<Option<[u32; 4]>, E> {
    if !reader.read_bit("default_display_window_flag")? {
        return Ok(None);
    }
    let mut window = [0u32; 4];
    for (slot, syntax) in window.iter_mut().zip([
        "def_disp_win_left_offset",
        "def_disp_win_right_offset",
        "def_disp_win_top_offset",
        "def_disp_win_bottom_offset",
    ]) {
        *slot = reader.read_ue(syntax)?;
    }
    Ok(Some(window))
}

/// VUI timing and hypothetical reference decoder section.
fn parse_vui_timing<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    max_sub_layers: u8,
) -> Result<(VuiTiming, Option<HrdParams>), E> {
    let present = reader.read_bit("vui_timing_info_present_flag")?;
    let mut timing = VuiTiming {
        present,
        ..Default::default()
    };
    if !timing.present {
        return Ok((timing, None));
    }
    timing.num_units_in_tick = reader.read_bits(32, "vui_num_units_in_tick")?;
    timing.time_scale = reader.read_bits(32, "vui_time_scale")?;
    timing.proportional = reader.read_bit("vui_poc_proportional_to_timing_flag")?;
    if timing.proportional {
        timing.num_ticks_poc_diff_one_minus1 =
            reader.read_ue("vui_num_ticks_poc_diff_one_minus1")?;
    }
    let hrd = if reader.read_bit("vui_hrd_parameters_present_flag")? {
        Some(parse_hrd(reader, max_sub_layers, None)?)
    } else {
        None
    };
    Ok((timing, hrd))
}

/// VUI bitstream restriction section.
fn parse_vui_restriction<E: HevcError>(
    reader: &mut BitReader<'_, E>,
) -> Result<Option<BitstreamRestriction>, E> {
    if !reader.read_bit("bitstream_restriction_flag")? {
        return Ok(None);
    }
    let tiles_fixed = reader.read_bit("tiles_fixed_structure_flag")?;
    let mv_over_pic = reader.read_bit("motion_vectors_over_pic_boundaries_flag")?;
    let restricted_ref = reader.read_bit("restricted_ref_pic_lists_flag")?;
    let min_spatial_segmentation = reader.read_ue("min_spatial_segmentation_idc")?;
    let max_bytes_per_pic_denom = reader.read_ue("max_bytes_per_pic_denom")?;
    let max_bits_per_mcu_denom = reader.read_ue("max_bits_per_min_cu_denom")?;
    let max_mv_horizontal = reader.read_ue("log2_max_mv_length_horizontal")?;
    let max_mv_vertical = reader.read_ue("log2_max_mv_length_vertical")?;
    Ok(Some(BitstreamRestriction {
        tiles_fixed,
        mv_over_pic,
        restricted_ref,
        min_spatial_segmentation,
        max_bytes_per_pic_denom,
        max_bits_per_mcu_denom,
        max_mv_horizontal,
        max_mv_vertical,
    }))
}

/// Checked narrowing of a sub-8-bit field to `u8`.
fn narrow_u8(value: u32) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}
