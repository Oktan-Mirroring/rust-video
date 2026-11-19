//! Tier errors behind the shared grammar.

use crate::framing::{
    NAL_AUD, NAL_EOB, NAL_EOS, NAL_FD, NAL_PPS, NAL_PREFIX_SEI, NAL_SPS, NAL_SUFFIX_SEI, NAL_VPS,
    VCL_FIRST, VCL_LAST,
};
use crate::pps::Pps;
use crate::slice::SliceHeader;
use crate::sps::Sps;
use crate::vps::Vps;

/// Grammar error behind a tier failure.
pub trait H265Error: Sized {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self;

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self;
}

pub use self::H265Error as HevcError;

/// Tier capability gates over shared grammar output.
pub trait H265Gate<E: H265Error> {
    /// Sequence set against tier caps.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the sequence set.
    ///
    /// # Arguments
    ///
    /// * `sps` - sequence set under check.
    fn check_sps(sps: &Sps) -> Result<(), E>;

    /// Picture set against tier caps with its sequence set, absent for orphans.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the picture set.
    ///
    /// # Arguments
    ///
    /// * `pps` - picture set under check.
    /// * `sps` - sequence set behind the check.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), E>;

    /// Video set against tier caps.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the video set.
    ///
    /// # Arguments
    ///
    /// * `vps` - video set under check.
    fn check_vps(vps: &Vps) -> Result<(), E> {
        let _ = vps;
        Ok(())
    }

    /// Slice header tail counts against tier caps.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the slice header tail.
    ///
    /// # Arguments
    ///
    /// * `header` - slice header under check.
    fn check_slice_extra(header: &SliceHeader) -> Result<(), E> {
        let _ = header;
        Ok(())
    }

    /// Sequence citation of video set `vps_id`, seen when `seen`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the video set citation.
    ///
    /// # Arguments
    ///
    /// * `vps_id` - cited video set id.
    /// * `seen` - true when the set arrived.
    fn check_sps_citation(vps_id: u8, seen: bool) -> Result<(), E> {
        let _ = (vps_id, seen);
        Ok(())
    }

    /// Slice order of one picture: treeblock addresses with first flags.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the slice order.
    ///
    /// # Arguments
    ///
    /// * `addrs` - treeblock addresses in coded order.
    /// * `firsts` - first flags in coded order.
    fn check_slice_order(addrs: &[u32], firsts: &[bool]) -> Result<(), E> {
        let _ = firsts;
        let mut previous: Option<u32> = None;
        for addr in addrs {
            match previous {
                None => previous = Some(*addr),
                Some(prev) if *addr > prev => previous = Some(*addr),
                _ => return Err(E::unsupported(UnsupportedKind::ArbitrarySliceOrder)),
            }
        }
        Ok(())
    }

    /// NAL unit action for `nal_type` on layer `layer_id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the NAL unit.
    ///
    /// # Arguments
    ///
    /// * `nal_type` - NAL unit type.
    /// * `layer_id` - NUH layer id.
    fn classify_nal(nal_type: u8, layer_id: u16) -> Result<NalAction, E> {
        if layer_id != 0 {
            return Err(E::unsupported(UnsupportedKind::Multilayer));
        }
        Ok(match nal_type {
            VCL_FIRST..=VCL_LAST => NalAction::Vcl,
            NAL_VPS => NalAction::Vps,
            NAL_SPS => NalAction::Sps,
            NAL_PPS => NalAction::Pps,
            NAL_AUD | NAL_EOS | NAL_EOB => NalAction::Complete,
            NAL_FD | NAL_PREFIX_SEI | NAL_SUFFIX_SEI | 41..=u8::MAX => NalAction::Ignore,
        })
    }
}

pub use self::H265Gate as HevcGate;

/// NAL unit action behind ingest dispatch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NalAction {
    /// Coded slice segment.
    Vcl,
    /// Video parameter set.
    Vps,
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
    /// NAL temporal id zero.
    NalTemporalId,
    /// Video set without a body.
    VpsWithoutBody,
    /// Sequence set without a body.
    SpsWithoutBody,
    /// Picture set without a body.
    PpsWithoutBody,
    /// Slice without a header.
    SliceWithoutHeader,
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
    /// Short-term set past its size.
    ShortTermRefPicSetSize,
    /// Coded size of zero.
    EmptyCodedSize,
    /// Conformance window outside the coded size.
    ConformanceOutside,
    /// Short-term table past its size.
    RpsTableOversize,
    /// Sequence long-term table past its size.
    LtSpsOversize,
    /// Slice long-term pictures past their size.
    LtTotalOversize,
    /// Slice cites a short-term table missing from the sequence.
    EmptySpsRps,
}

/// Legal construct the tier declines inside shared code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UnsupportedKind {
    /// Dependent slice segments.
    DependentSlices,
    /// Slices out of decode order.
    ArbitrarySliceOrder,
    /// Multilayer stream.
    Multilayer,
}

/// Gate accepting every construct.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PermissiveGate;

impl<E: H265Error> H265Gate<E> for PermissiveGate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &Sps) -> Result<(), E> {
        let _ = sps;
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(pps: &Pps, sps: Option<&Sps>) -> Result<(), E> {
        let _ = (pps, sps);
        Ok(())
    }

    /// Slice order of one picture: treeblock addresses with first flags.
    fn check_slice_order(addrs: &[u32], firsts: &[bool]) -> Result<(), E> {
        let _ = (addrs, firsts);
        Ok(())
    }

    /// NAL unit action for `nal_type` on layer `layer_id`.
    fn classify_nal(nal_type: u8, layer_id: u16) -> Result<NalAction, E> {
        let _ = layer_id;
        Ok(match nal_type {
            VCL_FIRST..=VCL_LAST => NalAction::Vcl,
            NAL_VPS => NalAction::Vps,
            NAL_SPS => NalAction::Sps,
            NAL_PPS => NalAction::Pps,
            NAL_AUD | NAL_EOS | NAL_EOB => NalAction::Complete,
            NAL_FD | NAL_PREFIX_SEI | NAL_SUFFIX_SEI | 41..=u8::MAX => NalAction::Ignore,
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Grammar test error.

    use super::{H265Error, ParseKind, UnsupportedKind};

    /// Test failure capturing grammar kinds.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum TestError {
        /// Parse failure.
        Parse(ParseKind),
        /// Tier-declined construct.
        Unsupported(UnsupportedKind),
        /// Sequence set cites an unseen video set.
        SpsCitesUnknownVps,
    }

    impl H265Error for TestError {
        /// Parse failure of `kind`.
        fn parse(kind: ParseKind) -> Self {
            Self::Parse(kind)
        }

        /// Tier-declined construct of `kind`.
        fn unsupported(kind: UnsupportedKind) -> Self {
            Self::Unsupported(kind)
        }
    }
}
