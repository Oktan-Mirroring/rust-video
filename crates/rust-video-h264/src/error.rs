//! Tier errors behind the shared grammar.

use crate::pps::Pps;
use crate::slice::{RefMod, SliceHeader};
use crate::sps::Sps;

/// Grammar error behind a tier failure.
pub trait H264Error: Sized {
    /// Parse failure of `kind`.
    fn parse(kind: ParseReason) -> Self;

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedReason) -> Self;
}

/// Tier capability gates over shared grammar output.
pub trait H264Gate<E: H264Error>: Default {
    /// Sequence set against tier caps.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the set exceeds tier caps.
    fn check_sps(sps: &Sps) -> Result<(), E>;

    /// Picture set against tier caps with its sequence set.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the set exceeds tier caps.
    fn check_pps(pps: &Pps, sps: &Sps) -> Result<(), E>;

    /// Slice header counts against tier caps with its sequence set.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the header exceeds tier caps.
    fn check_slice_extra(header: &SliceHeader, sps: &Sps) -> Result<(), E> {
        let _ = (header, sps);
        Ok(())
    }

    /// Slice order of one picture: first macroblock addresses.
    ///
    /// # Errors
    ///
    /// Returns `Err` when slices run out of decode order.
    fn check_slice_order(first_mbs: &[u32]) -> Result<(), E> {
        let mut previous: Option<u32> = None;
        for addr in first_mbs {
            match previous {
                None => previous = Some(*addr),
                Some(prev) if *addr > prev => previous = Some(*addr),
                _ => return Err(E::unsupported(UnsupportedReason::ArbitrarySliceOrder)),
            }
        }
        Ok(())
    }

    /// NAL unit action for `nal_type`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the NAL type.
    fn classify_nal(nal_type: u8) -> Result<NalAction, E> {
        Ok(match nal_type {
            1 | 5 => NalAction::Vcl,
            7 => NalAction::Sps,
            8 => NalAction::Pps,
            9..=11 => NalAction::Complete,
            2..=4 => return Err(E::unsupported(UnsupportedReason::DataPartitions)),
            13..=15 | 20 => return Err(E::unsupported(UnsupportedReason::Multiview)),
            _ => NalAction::Ignore,
        })
    }
}

/// NAL unit action behind ingest dispatch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NalAction {
    /// Coded slice.
    Vcl,
    /// Sequence parameter set.
    Sps,
    /// Picture parameter set.
    Pps,
    /// Boundary completing the open picture.
    Complete,
    /// Anything the ingest ignores.
    Ignore,
}

/// Bitstream element the grammar rejects.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ParseKind {
    /// NAL ends before the element completes.
    Truncated {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Exp-Golomb code exceeds 32 bits.
    OverflowU32 {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Signed Exp-Golomb code exceeds `i32`.
    OverflowI32 {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Element wider than 32 bits.
    TooWide {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Value outside its field width.
    Range {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// NAL `forbidden_zero_bit` set.
    NalForbiddenBit,
    /// Sequence set without a body.
    SpsWithoutBody,
    /// Picture set without a body.
    PpsWithoutBody,
    /// Slice without a header.
    SliceWithoutHeader,
    /// Slice header exceeds its length cap.
    SliceHeaderTooLong,
    /// Picture set cites an unseen sequence set.
    PpsCitesUnknownSps,
    /// Slice cites an unseen picture set.
    SliceCitesUnknownPps,
    /// Slice cites an unseen sequence set.
    SliceCitesUnknownSps,
    /// Slices of one picture disagree.
    PictureSlicesDisagree,
    /// Non-intra slice inside an IDR picture.
    NonIntraSliceInIdr,
    /// Crop extends past the coded size.
    FrameCropOutsideCodedSize,
}

pub use self::ParseKind as ParseReason;

/// Legal construct the tier declines inside shared code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnsupportedKind {
    /// Interlaced fields.
    Interlaced,
    /// Chroma outside 4:2:0.
    ChromaOutside420,
    /// Luma depth above 8 bit.
    LumaDepthAbove8Bit,
    /// Chroma depth above 8 bit.
    ChromaDepthAbove8Bit,
    /// Transform bypass.
    TransformBypass,
    /// Flexible macroblock order.
    FlexibleMacroblockOrder,
    /// Partitioned slices.
    DataPartitions,
    /// Multiview slices.
    Multiview,
    /// SP/SI slices.
    SpSiSlices,
    /// Redundant pictures.
    RedundantPictures,
    /// Slices out of decode order.
    ArbitrarySliceOrder,
}

pub use self::UnsupportedKind as UnsupportedReason;

/// Gate accepting every construct.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PermissiveGate;

impl<E: H264Error> H264Gate<E> for PermissiveGate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), E> {
        let _ = sps;
        Ok(())
    }

    /// Picture set against tier caps with its sequence set.
    fn check_pps(pps: &Pps, sps: &Sps) -> Result<(), E> {
        let _ = (pps, sps);
        Ok(())
    }

    /// Slice order of one picture: first macroblock addresses.
    fn check_slice_order(first_mbs: &[u32]) -> Result<(), E> {
        let _ = first_mbs;
        Ok(())
    }

    /// NAL unit action for `nal_type`.
    fn classify_nal(nal_type: u8) -> Result<NalAction, E> {
        Ok(match nal_type {
            1 | 5 => NalAction::Vcl,
            7 => NalAction::Sps,
            8 => NalAction::Pps,
            9..=11 => NalAction::Complete,
            _ => NalAction::Ignore,
        })
    }
}

/// Gate holding 8-bit 4:2:0 frame-only pictures.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct StrictGate;

impl<E: H264Error> H264Gate<E> for StrictGate {
    /// Sequence set against tier caps.
    ///
    /// # Errors
    ///
    /// Returns `Err` on sampling outside 8-bit 4:2:0, bypass, a reference
    /// window past 16, or a crop past the coded size.
    fn check_sps(sps: &Sps) -> Result<(), E> {
        if sps.fidelity.chroma_format_idc != 1 {
            return Err(E::unsupported(UnsupportedReason::ChromaOutside420));
        }
        if sps.fidelity.bit_depth_luma_minus8 != 0 {
            return Err(E::unsupported(UnsupportedReason::LumaDepthAbove8Bit));
        }
        if sps.fidelity.bit_depth_chroma_minus8 != 0 {
            return Err(E::unsupported(UnsupportedReason::ChromaDepthAbove8Bit));
        }
        if sps.fidelity.transform_bypass {
            return Err(E::unsupported(UnsupportedReason::TransformBypass));
        }
        if sps.max_num_ref_frames > 16 {
            return Err(E::parse(ParseReason::Range {
                syntax: "max_num_ref_frames",
            }));
        }
        if sps.crop_offsets.is_some() && sps.crop.is_none() {
            return Err(E::parse(ParseReason::FrameCropOutsideCodedSize));
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a reserved biprediction mode.
    fn check_pps(pps: &Pps, sps: &Sps) -> Result<(), E> {
        let _ = sps;
        if pps.weighted_bipred_idc > 2 {
            return Err(E::parse(ParseReason::Range {
                syntax: "weighted_bipred_idc",
            }));
        }
        Ok(())
    }

    /// Slice header counts against tier caps with its sequence set.
    ///
    /// # Errors
    ///
    /// Returns `Err` on redundant pictures or a modification past the
    /// reference window.
    fn check_slice_extra(header: &SliceHeader, sps: &Sps) -> Result<(), E> {
        if header.redundant_pic_cnt != 0 {
            return Err(E::unsupported(UnsupportedReason::RedundantPictures));
        }
        for mods in [&header.ref_mod_l0, &header.ref_mod_l1] {
            for modification in mods {
                match *modification {
                    RefMod::ShortBelow(diff) | RefMod::ShortAbove(diff)
                        if diff >= sps.max_frame_num() =>
                    {
                        return Err(E::parse(ParseReason::Range {
                            syntax: "abs_diff_pic_num",
                        }));
                    }
                    RefMod::Long(index) if index > u32::from(sps.max_num_ref_frames) => {
                        return Err(E::parse(ParseReason::Range {
                            syntax: "long_term_pic_num",
                        }));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// Checked narrowing of a parsed value to `u8`.
pub(crate) fn narrow_u8<E: H264Error>(value: u32, syntax: &'static str) -> Result<u8, E> {
    u8::try_from(value).map_err(|_| E::parse(ParseReason::Range { syntax }))
}

/// Checked narrowing of a parsed value to `u16`.
pub(crate) fn narrow_u16<E: H264Error>(value: u32, syntax: &'static str) -> Result<u16, E> {
    u16::try_from(value).map_err(|_| E::parse(ParseReason::Range { syntax }))
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Grammar test error.

    use super::{H264Error, ParseReason, UnsupportedReason};

    /// Test failure capturing grammar kinds.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum TestError {
        /// Parse failure.
        Parse(ParseReason),
        /// Tier-declined construct.
        Unsupported(UnsupportedReason),
    }

    impl H264Error for TestError {
        /// Parse failure of `kind`.
        fn parse(kind: ParseReason) -> Self {
            Self::Parse(kind)
        }

        /// Tier-declined construct of `kind`.
        fn unsupported(kind: UnsupportedReason) -> Self {
            Self::Unsupported(kind)
        }
    }
}
