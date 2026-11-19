//! Profile, tier, and level.

use crate::bits::BitReader;
use crate::error::{HevcError, ParseKind};

/// Source flags of one profile record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceFlags {
    /// Progressive source.
    pub progressive: bool,
    /// Interlaced source.
    pub interlaced: bool,
}

/// Packing constraint flags of one profile record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConstraintFlags {
    /// Non-packed constraint.
    pub non_packed: bool,
    /// Frame-only constraint.
    pub frame_only: bool,
}

/// Sub-layer profile record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SubPtl {
    /// Profile record present.
    pub present: bool,
    /// Tier flag.
    pub tier: bool,
    /// Profile indicator.
    pub profile_idc: u8,
    /// Compatibility flags.
    pub compat: u32,
    /// Source flags.
    pub source: SourceFlags,
    /// Packing constraint flags.
    pub constraints: ConstraintFlags,
    /// Level indicator, absent without a level record.
    pub level_idc: Option<u8>,
}

/// Profile, tier and level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileTierLevel {
    /// Tier flag.
    pub tier: bool,
    /// Profile indicator.
    pub profile_idc: u8,
    /// Compatibility flags.
    pub compat: u32,
    /// Source flags.
    pub source: SourceFlags,
    /// Packing constraint flags.
    pub constraints: ConstraintFlags,
    /// Level indicator.
    pub level_idc: u8,
    /// Sub-layer records in coded order.
    pub sub_layers: Vec<SubPtl>,
}

/// Profile, tier, and level, H.265 7.3.3.
pub fn parse_profile_tier_level<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    max_sub_layers: u8,
) -> Result<ProfileTierLevel, E> {
    let general = parse_ptl_layer(reader)?;
    let level_idc = reader.read_bits(8, "general_level_idc")?;
    let mut profile_present = Vec::new();
    let mut level_present = Vec::new();
    for _ in 1..max_sub_layers {
        profile_present.push(reader.read_bit("sub_layer_profile_present_flag")?);
        level_present.push(reader.read_bit("sub_layer_level_present_flag")?);
    }
    if max_sub_layers > 1 {
        for _ in max_sub_layers..9 {
            reader.read_bits(2, "reserved_zero_2bits")?;
        }
    }
    let mut sub_layers = Vec::new();
    for index in 0..profile_present.len() {
        let mut layer = if profile_present[index] {
            parse_ptl_layer(reader)?
        } else {
            SubPtl::default()
        };
        if level_present[index] {
            layer.level_idc = Some(
                u8::try_from(reader.read_bits(8, "sub_layer_level_idc")?).map_err(|_| {
                    E::parse(ParseKind::Range {
                        syntax: "sub_layer_level_idc",
                    })
                })?,
            );
        }
        sub_layers.push(layer);
    }
    Ok(ProfileTierLevel {
        tier: general.tier,
        profile_idc: general.profile_idc,
        compat: general.compat,
        source: general.source,
        constraints: general.constraints,
        level_idc: u8::try_from(level_idc).map_err(|_| {
            E::parse(ParseKind::Range {
                syntax: "general_level_idc",
            })
        })?,
        sub_layers,
    })
}

/// One profile-tier layer: record plus constraint flags.
fn parse_ptl_layer<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<SubPtl, E> {
    reader.read_bits(2, "general_profile_space")?;
    let tier = reader.read_bit("general_tier_flag")?;
    let idc = reader.read_bits(5, "general_profile_idc")?;
    let mut compat_bits = [false; 32];
    for flag in &mut compat_bits {
        *flag = reader.read_bit("general_profile_compatibility_flag")?;
    }
    let mut compat = 0u32;
    for flag in &compat_bits {
        compat = (compat << 1) | u32::from(*flag);
    }
    let progressive = reader.read_bit("general_progressive_source_flag")?;
    let interlaced = reader.read_bit("general_interlaced_source_flag")?;
    let non_packed = reader.read_bit("general_non_packed_constraint_flag")?;
    let frame_only = reader.read_bit("general_frame_only_constraint_flag")?;
    let idc = usize::try_from(idc).unwrap_or(usize::MAX);
    parse_ptl_constraints(reader, idc, &compat_bits)?;
    Ok(SubPtl {
        present: true,
        tier,
        profile_idc: u8::try_from(idc).unwrap_or(u8::MAX),
        compat,
        source: SourceFlags {
            progressive,
            interlaced,
        },
        constraints: ConstraintFlags {
            non_packed,
            frame_only,
        },
        level_idc: None,
    })
}

/// Constraint flags for profile `idc` with compatibility `compat`.
fn parse_ptl_constraints<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    idc: usize,
    compat: &[bool; 32],
) -> Result<(), E> {
    let matches = |profile: usize| idc == profile || compat[profile];
    if [4, 5, 6, 7, 8, 9, 10, 11]
        .iter()
        .any(|profile| matches(*profile))
    {
        for _ in 0..9 {
            reader.read_bit("general_constraint_flag")?;
        }
        if [5, 9, 10, 11].iter().any(|profile| matches(*profile)) {
            reader.read_bit("general_max_14bit_constraint_flag")?;
            reader.read_bits(24, "general_reserved_zero_33bits")?;
            reader.read_bits(9, "general_reserved_zero_33bits")?;
        } else {
            reader.read_bits(24, "general_reserved_zero_34bits")?;
            reader.read_bits(10, "general_reserved_zero_34bits")?;
        }
    } else if matches(2) {
        reader.read_bits(7, "general_reserved_zero_7bits")?;
        reader.read_bit("general_one_picture_only_constraint_flag")?;
        reader.read_bits(24, "general_reserved_zero_35bits")?;
        reader.read_bits(11, "general_reserved_zero_35bits")?;
    } else {
        reader.read_bits(24, "general_reserved_zero_43bits")?;
        reader.read_bits(19, "general_reserved_zero_43bits")?;
    }
    if [1, 2, 3, 4, 5, 9, 11]
        .iter()
        .any(|profile| matches(*profile))
    {
        reader.read_bit("general_inbld_flag")?;
    } else {
        reader.read_bits(1, "general_reserved_zero_bit")?;
    }
    Ok(())
}
