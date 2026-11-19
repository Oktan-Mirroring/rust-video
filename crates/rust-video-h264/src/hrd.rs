//! Hypothetical reference decoder parameters, H.264 E.1.2.

use crate::bits::BitReader;
use crate::error::{narrow_u8, H264Error, ParseKind};

/// HRD parameters of one set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HrdParams {
    /// CPB count minus 1.
    pub cpb_count_minus1: u8,
    /// Bitrate scale.
    pub bitrate_scale: u8,
    /// CPB size scale.
    pub cpb_scale: u8,
    /// Bitrate minus 1 per CPB.
    pub bitrate_minus1: Vec<u32>,
    /// CPB size minus 1 per CPB.
    pub cpb_size_minus1: Vec<u32>,
    /// CBR flag per CPB.
    pub cbr: Vec<bool>,
    /// Initial CPB removal delay length minus 1.
    pub initial_delay_minus1: u8,
    /// CPB removal delay length minus 1.
    pub removal_delay_minus1: u8,
    /// DPB output delay length minus 1.
    pub output_delay_minus1: u8,
    /// Time offset length.
    pub time_offset_length: u8,
}

/// Hypothetical reference decoder parameters, H.264 E.1.2.
pub(crate) fn parse_hrd<E: H264Error>(reader: &mut BitReader<E>) -> Result<HrdParams, E> {
    let mut hrd = HrdParams::default();
    let count = reader.read_ue("cpb_cnt_minus1")?;
    if count > 31 {
        return Err(E::parse(ParseKind::Range {
            syntax: "cpb_cnt_minus1",
        }));
    }
    hrd.cpb_count_minus1 = narrow_u8(count, "cpb_cnt_minus1")?;
    hrd.bitrate_scale = narrow_u8(reader.read_bits(4, "bit_rate_scale")?, "bit_rate_scale")?;
    hrd.cpb_scale = narrow_u8(reader.read_bits(4, "cpb_size_scale")?, "cpb_size_scale")?;
    for _ in 0..=count {
        hrd.bitrate_minus1
            .push(reader.read_ue("bit_rate_value_minus1")?);
        hrd.cpb_size_minus1
            .push(reader.read_ue("cpb_size_value_minus1")?);
        hrd.cbr.push(reader.read_bit("cbr_flag")?);
    }
    hrd.initial_delay_minus1 = narrow_u8(
        reader.read_bits(5, "initial_cpb_removal_delay_length_minus1")?,
        "initial_cpb_removal_delay_length_minus1",
    )?;
    hrd.removal_delay_minus1 = narrow_u8(
        reader.read_bits(5, "cpb_removal_delay_length_minus1")?,
        "cpb_removal_delay_length_minus1",
    )?;
    hrd.output_delay_minus1 = narrow_u8(
        reader.read_bits(5, "dpb_output_delay_length_minus1")?,
        "dpb_output_delay_length_minus1",
    )?;
    hrd.time_offset_length = narrow_u8(
        reader.read_bits(5, "time_offset_length")?,
        "time_offset_length",
    )?;
    Ok(hrd)
}
