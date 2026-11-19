//! Video parameter sets.

use crate::bits::BitReader;
use crate::error::H265Error;
use crate::hrd::{parse_hrd, HrdParams};
use crate::ptl::{parse_profile_tier_level, ProfileTierLevel};

/// VPS timing information.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VpsTiming {
    /// Ticks per second.
    pub num_units_in_tick: u32,
    /// Time scale.
    pub time_scale: u32,
    /// POC proportional to timing.
    pub poc_proportional: bool,
    /// Ticks per POC step minus one.
    pub num_ticks_poc_diff_one_minus1: u32,
    /// HRD parameter sets.
    pub hrd: Vec<HrdParams>,
}

/// Video parameter set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vps {
    /// Own id.
    pub id: u8,
    /// Highest layer index.
    pub max_layers_minus1: u8,
    /// Highest sub-layer index.
    pub max_sub_layers_minus1: u8,
    /// Temporal id nesting.
    pub temporal_nesting: bool,
    /// Profile, tier and level.
    pub ptl: ProfileTierLevel,
    /// Sub-layer ordering info present.
    pub ordering_info_present: bool,
    /// DPB capacity per sub-layer minus one.
    pub max_dec_pic_buffering: Vec<u32>,
    /// Reorder depth per sub-layer.
    pub max_num_reorder_pics: Vec<u32>,
    /// Maximum latency per sub-layer.
    pub max_latency: Vec<u32>,
    /// Layer set count minus one.
    pub num_layer_sets: u32,
    /// Timing information, when present.
    pub timing: Option<VpsTiming>,
    /// Extension present.
    pub extension: bool,
}

/// Video set from its RBSP `body` with the NAL header stripped.
///
/// # Errors
///
/// Returns `Err` on malformed syntax.
///
/// # Arguments
///
/// * `body` - RBSP bytes with the NAL header stripped.
pub fn parse_vps<E: H265Error>(body: &[u8]) -> Result<Vps, E> {
    let mut reader = BitReader::new(body);
    let id = narrow_u8(reader.read_bits(4, "vps id")?);
    reader.read_bits(2, "vps base layer")?;
    let max_layers_minus1 = narrow_u8(reader.read_bits(6, "vps max layers")?);
    let max_sub_layers_minus1 = narrow_u8(reader.read_bits(3, "vps max sub")?);
    let temporal_nesting = reader.read_bit("vps nesting")?;
    reader.read_bits(16, "vps reserved")?;
    let ptl = parse_profile_tier_level(&mut reader, max_sub_layers_minus1 + 1)?;
    let ordering_info_present = reader.read_bit("vps ordering info")?;
    let start = if ordering_info_present {
        0
    } else {
        u32::from(max_sub_layers_minus1)
    };
    let mut max_dec_pic_buffering = Vec::new();
    let mut max_num_reorder_pics = Vec::new();
    let mut max_latency = Vec::new();
    for _ in start..=u32::from(max_sub_layers_minus1) {
        max_dec_pic_buffering.push(reader.read_ue("vps buffering")?);
        max_num_reorder_pics.push(reader.read_ue("vps reorder")?);
        max_latency.push(reader.read_ue("vps latency")?);
    }
    reader.read_bits(6, "vps max layer id")?;
    let num_layer_sets = reader.read_ue("vps layer sets")?;
    let mut timing = None;
    if reader.read_bit("vps timing present")? {
        timing = Some(parse_vps_timing(
            &mut reader,
            max_sub_layers_minus1.saturating_add(1),
        )?);
    }
    let extension = reader.more_data() && reader.read_bit("vps extension")?;
    Ok(Vps {
        id,
        max_layers_minus1,
        max_sub_layers_minus1,
        temporal_nesting,
        ptl,
        ordering_info_present,
        max_dec_pic_buffering,
        max_num_reorder_pics,
        max_latency,
        num_layer_sets,
        timing,
        extension,
    })
}

/// VPS timing information with its HRD sets.
fn parse_vps_timing<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    layers: u8,
) -> Result<VpsTiming, E> {
    let num_units_in_tick = reader.read_bits(32, "vps tick")?;
    let time_scale = reader.read_bits(32, "vps scale")?;
    let poc_proportional = reader.read_bit("vps poc proportional")?;
    let num_ticks_poc_diff_one_minus1 = if poc_proportional {
        reader.read_ue("vps poc ticks")?
    } else {
        0
    };
    let hrd_count = reader.read_ue("vps hrd count")?;
    let mut hrd = Vec::new();
    for index in 0..hrd_count {
        reader.read_ue("vps hrd layer set")?;
        let mut inherit = None;
        if index > 0 && !reader.read_bit("vps hrd common")? {
            let first: &HrdParams = &hrd[0];
            inherit = Some((first.streams, first.sub_pic));
        }
        hrd.push(parse_hrd(reader, layers, inherit)?);
    }
    Ok(VpsTiming {
        num_units_in_tick,
        time_scale,
        poc_proportional,
        num_ticks_poc_diff_one_minus1,
        hrd,
    })
}

/// Checked narrowing of a sub-8-bit field to `u8`.
fn narrow_u8(value: u32) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}
