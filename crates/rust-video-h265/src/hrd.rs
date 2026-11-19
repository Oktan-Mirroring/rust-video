//! Hypothetical reference decoder parameters.

use crate::bits::BitReader;
use crate::error::{H265Error, ParseKind};

/// HRD stream presence of one parameter set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HrdStreams {
    /// NAL HRD present.
    pub nal: bool,
    /// VCL HRD present.
    pub vcl: bool,
}

/// HRD coded-picture-buffer data of one layer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HrdLayerData {
    /// Bitrate minus one per CPB.
    pub bitrate_minus1: Vec<u32>,
    /// CPB size minus one per CPB.
    pub cpb_size_minus1: Vec<u32>,
    /// DU CPB size minus one per CPB.
    pub du_cpb_size_minus1: Vec<u32>,
    /// DU bitrate minus one per CPB.
    pub du_bitrate_minus1: Vec<u32>,
    /// CBR flag per CPB.
    pub cbr: Vec<bool>,
}

/// HRD parameters of one layer set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HrdParams {
    /// Shared header coded in this set rather than inherited.
    pub common: bool,
    /// Stream presence flags.
    pub streams: HrdStreams,
    /// Sub-picture HRD.
    pub sub_pic: bool,
    /// Tick divisor minus two.
    pub tick_divisor_minus2: u8,
    /// Bitrate scale.
    pub bitrate_scale: u8,
    /// CPB size scale.
    pub cpb_scale: u8,
    /// DU CPB size scale.
    pub du_cpb_scale: u8,
    /// Initial CPB removal delay length minus one.
    pub initial_delay_minus1: u8,
    /// AU CPB removal delay length minus one.
    pub au_delay_minus1: u8,
    /// DPB output delay length minus one.
    pub dpb_delay_minus1: u8,
    /// Fixed picture rate per layer.
    pub fixed_rate: Vec<bool>,
    /// Fixed rate within the coded video sequence per layer.
    pub pic_duration: Vec<bool>,
    /// Elemental duration minus one per layer, absent without a fixed rate.
    pub elemental: Vec<Option<u32>>,
    /// Low-delay HRD per layer.
    pub low_delay: Vec<bool>,
    /// CPB count minus one per layer.
    pub cpb_count_minus1: Vec<u32>,
    /// NAL CPB data per layer.
    pub nal_layers: Vec<HrdLayerData>,
    /// VCL CPB data per layer.
    pub vcl_layers: Vec<HrdLayerData>,
}

/// HRD parameters, H.265 E.2.2; `inherit` carries the first set's shared header.
pub(crate) fn parse_hrd<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    max_sub_layers: u8,
    inherit: Option<(HrdStreams, bool)>,
) -> Result<HrdParams, E> {
    let mut hrd = HrdParams::default();
    if let Some((streams, sub_pic)) = inherit {
        hrd.streams = streams;
        hrd.sub_pic = sub_pic;
    } else {
        hrd.common = true;
        hrd.streams.nal = reader.read_bit("nal_hrd_parameters_present_flag")?;
        hrd.streams.vcl = reader.read_bit("vcl_hrd_parameters_present_flag")?;
        if hrd.streams.nal || hrd.streams.vcl {
            parse_hrd_common(reader, &mut hrd)?;
        }
    }
    for _ in 0..max_sub_layers {
        parse_hrd_layer(reader, &mut hrd)?;
    }
    Ok(hrd)
}

/// HRD common information into `hrd`.
fn parse_hrd_common<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    hrd: &mut HrdParams,
) -> Result<(), E> {
    hrd.sub_pic = reader.read_bit("sub_pic_hrd_params_present_flag")?;
    if hrd.sub_pic {
        hrd.tick_divisor_minus2 = narrow_u8(reader.read_bits(8, "tick_divisor_minus2")?)?;
        reader.read_bits(5, "du_cpb_removal_delay_increment_length_minus1")?;
        reader.read_bit("sub_pic_cpb_params_in_pic_timing_sei_flag")?;
        reader.read_bits(5, "dpb_output_delay_du_length_minus1")?;
    }
    hrd.bitrate_scale = narrow_u8(reader.read_bits(4, "bit_rate_scale")?)?;
    hrd.cpb_scale = narrow_u8(reader.read_bits(4, "cpb_size_scale")?)?;
    if hrd.sub_pic {
        hrd.du_cpb_scale = narrow_u8(reader.read_bits(4, "cpb_size_du_scale")?)?;
    }
    hrd.initial_delay_minus1 =
        narrow_u8(reader.read_bits(5, "initial_cpb_removal_delay_length_minus1")?)?;
    hrd.au_delay_minus1 = narrow_u8(reader.read_bits(5, "au_cpb_removal_delay_length_minus1")?)?;
    hrd.dpb_delay_minus1 = narrow_u8(reader.read_bits(5, "dpb_output_delay_length_minus1")?)?;
    Ok(())
}

/// One HRD layer schedule with its CPB data into `hrd`.
fn parse_hrd_layer<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    hrd: &mut HrdParams,
) -> Result<(), E> {
    let fixed_general = reader.read_bit("fixed_pic_rate_general_flag")?;
    let fixed_within = if fixed_general {
        true
    } else {
        reader.read_bit("fixed_pic_rate_within_cvs_flag")?
    };
    let (elemental, low_delay) = if fixed_within {
        (
            Some(reader.read_ue("elemental_duration_in_tc_minus1")?),
            false,
        )
    } else {
        (None, reader.read_bit("low_delay_hrd_flag")?)
    };
    let count = if low_delay {
        0
    } else {
        let count = reader.read_ue("cpb_cnt_minus1")?;
        if count > 31 {
            return Err(E::parse(ParseKind::Range {
                syntax: "cpb_cnt_minus1",
            }));
        }
        count
    };
    let mut nal = HrdLayerData::default();
    let mut vcl = HrdLayerData::default();
    for _ in 0..=count {
        if hrd.streams.nal {
            parse_hrd_cpb(reader, hrd.sub_pic, &mut nal)?;
        }
        if hrd.streams.vcl {
            parse_hrd_cpb(reader, hrd.sub_pic, &mut vcl)?;
        }
    }
    hrd.fixed_rate.push(fixed_general);
    hrd.pic_duration.push(fixed_within);
    hrd.elemental.push(elemental);
    hrd.low_delay.push(low_delay);
    hrd.cpb_count_minus1.push(count);
    hrd.nal_layers.push(nal);
    hrd.vcl_layers.push(vcl);
    Ok(())
}

/// One HRD coded picture buffer description into `layer`.
fn parse_hrd_cpb<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    sub_pic: bool,
    layer: &mut HrdLayerData,
) -> Result<(), E> {
    layer
        .bitrate_minus1
        .push(reader.read_ue("bit_rate_value_minus1")?);
    layer
        .cpb_size_minus1
        .push(reader.read_ue("cpb_size_value_minus1")?);
    if sub_pic {
        layer
            .du_cpb_size_minus1
            .push(reader.read_ue("cpb_size_du_value_minus1")?);
        layer
            .du_bitrate_minus1
            .push(reader.read_ue("bit_rate_du_value_minus1")?);
    }
    layer.cbr.push(reader.read_bit("cbr_flag")?);
    Ok(())
}

/// Checked narrowing of a parsed value to `u8`.
fn narrow_u8<E: H265Error>(value: u32) -> Result<u8, E> {
    u8::try_from(value).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "hrd scale",
        })
    })
}
