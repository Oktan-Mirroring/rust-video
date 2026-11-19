//! Video usability information, H.264 E.1.1.

use crate::bits::BitReader;
use crate::error::{narrow_u16, narrow_u8, H264Error};
use crate::hrd::{parse_hrd, HrdParams};

/// Aspect-ratio description of a sequence set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AspectRatio {
    /// Aspect ratio idc.
    pub idc: u8,
    /// Sample aspect width.
    pub sar_width: u16,
    /// Sample aspect height.
    pub sar_height: u16,
}

/// Chroma sample locations of a sequence set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromaLoc {
    /// Top-field location.
    pub top: u32,
    /// Bottom-field location.
    pub bottom: u32,
}

/// Colour description of a video signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColourDesc {
    /// Colour primaries.
    pub primaries: u8,
    /// Transfer characteristics.
    pub transfer: u8,
    /// Matrix coefficients.
    pub matrix: u8,
}

/// Overscan description of a sequence set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overscan {
    /// Overscan appropriate for display.
    pub appropriate: bool,
}

/// Timing description of a sequence set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VuiTiming {
    /// Ticks per second.
    pub num_units_in_tick: u32,
    /// Time scale.
    pub time_scale: u32,
    /// Fixed frame rate.
    pub fixed_rate: bool,
}

pub use self::VuiTiming as TimingInfo;

/// Video signal description of a sequence set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoSignal {
    /// Video format.
    pub format: u8,
    /// Full range.
    pub full_range: bool,
    /// Colour description, when present.
    pub colour: Option<ColourDesc>,
}

/// VUI parameters of one sequence set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VuiParams {
    /// Aspect ratio, when present.
    pub aspect: Option<AspectRatio>,
    /// Overscan, when present.
    pub overscan: Option<Overscan>,
    /// Video signal, when present.
    pub signal: Option<VideoSignal>,
    /// Chroma locations, when present.
    pub chroma_loc: Option<ChromaLoc>,
    /// Timing, when present.
    pub timing: Option<VuiTiming>,
    /// NAL HRD parameters, when present.
    pub nal_hrd: Option<HrdParams>,
    /// VCL HRD parameters, when present.
    pub vcl_hrd: Option<HrdParams>,
    /// Low delay HRD operation.
    pub low_delay_hrd: bool,
    /// Picture structure present.
    pub pic_struct_present: bool,
    /// Motion vectors over picture boundaries.
    pub mv_over_pic: bool,
    /// Maximum bytes per picture denominator.
    pub max_bytes_per_pic_denom: u32,
    /// Maximum bits per MCU denominator.
    pub max_bits_per_mcu_denom: u32,
    /// Horizontal motion vector length bits.
    pub max_mv_horizontal: u32,
    /// Vertical motion vector length bits.
    pub max_mv_vertical: u32,
    /// Reorder depth, when the restriction is signaled.
    pub max_num_reorder_frames: Option<u32>,
    /// DPB capacity, when the restriction is signaled.
    pub max_dec_frame_buffering: Option<u32>,
}

/// VUI parameters, absent past the crop syntax.
pub(crate) fn parse_vui<E: H264Error>(reader: &mut BitReader<E>) -> Result<Option<VuiParams>, E> {
    if !reader.more_data() {
        return Ok(None);
    }
    if !reader.read_bit("vui_parameters_present_flag")? {
        return Ok(None);
    }
    let mut vui = VuiParams::default();
    if reader.read_bit("aspect_ratio_info_present_flag")? {
        let idc = narrow_u8(reader.read_bits(8, "aspect_ratio_idc")?, "aspect_ratio_idc")?;
        let (sar_width, sar_height) = if idc == 255 {
            (
                narrow_u16(reader.read_bits(16, "sar_width")?, "sar_width")?,
                narrow_u16(reader.read_bits(16, "sar_height")?, "sar_height")?,
            )
        } else {
            (0, 0)
        };
        vui.aspect = Some(AspectRatio {
            idc,
            sar_width,
            sar_height,
        });
    }
    if reader.read_bit("overscan_info_present_flag")? {
        vui.overscan = Some(Overscan {
            appropriate: reader.read_bit("overscan_appropriate_flag")?,
        });
    }
    if reader.read_bit("video_signal_type_present_flag")? {
        let format = narrow_u8(reader.read_bits(3, "video_format")?, "video_format")?;
        let full_range = reader.read_bit("video_full_range_flag")?;
        let colour = if reader.read_bit("colour_description_present_flag")? {
            Some(ColourDesc {
                primaries: narrow_u8(reader.read_bits(8, "colour_primaries")?, "colour_primaries")?,
                transfer: narrow_u8(
                    reader.read_bits(8, "transfer_characteristics")?,
                    "transfer_characteristics",
                )?,
                matrix: narrow_u8(
                    reader.read_bits(8, "matrix_coefficients")?,
                    "matrix_coefficients",
                )?,
            })
        } else {
            None
        };
        vui.signal = Some(VideoSignal {
            format,
            full_range,
            colour,
        });
    }
    if reader.read_bit("chroma_loc_info_present_flag")? {
        vui.chroma_loc = Some(ChromaLoc {
            top: reader.read_ue("chroma_sample_loc_type_top_field")?,
            bottom: reader.read_ue("chroma_sample_loc_type_bottom_field")?,
        });
    }
    if reader.read_bit("timing_info_present_flag")? {
        vui.timing = Some(VuiTiming {
            num_units_in_tick: reader.read_bits(32, "num_units_in_tick")?,
            time_scale: reader.read_bits(32, "time_scale")?,
            fixed_rate: reader.read_bit("fixed_frame_rate_flag")?,
        });
    }
    let mut hrd = false;
    if reader.read_bit("nal_hrd_parameters_present_flag")? {
        vui.nal_hrd = Some(parse_hrd(reader)?);
        hrd = true;
    }
    if reader.read_bit("vcl_hrd_parameters_present_flag")? {
        vui.vcl_hrd = Some(parse_hrd(reader)?);
        hrd = true;
    }
    if hrd {
        vui.low_delay_hrd = reader.read_bit("low_delay_hrd_flag")?;
    }
    vui.pic_struct_present = reader.read_bit("pic_struct_present_flag")?;
    if reader.read_bit("bitstream_restriction_flag")? {
        vui.mv_over_pic = reader.read_bit("motion_vectors_over_pic_boundaries_flag")?;
        vui.max_bytes_per_pic_denom = reader.read_ue("max_bytes_per_pic_denom")?;
        vui.max_bits_per_mcu_denom = reader.read_ue("max_bits_per_mb_denom")?;
        vui.max_mv_horizontal = reader.read_ue("log2_max_mv_length_horizontal")?;
        vui.max_mv_vertical = reader.read_ue("log2_max_mv_length_vertical")?;
        vui.max_num_reorder_frames = Some(reader.read_ue("num_reorder_frames")?);
        vui.max_dec_frame_buffering = Some(reader.read_ue("max_dec_frame_buffering")?);
    }
    Ok(Some(vui))
}
