//! Annex B framing over NAL units.

pub use rust_video_core::nalu::{rbsp, rbsp_prefix, split_nals, Crop, Nalu, NaluSplitter};

/// Sequence set count, H.265 7.4.3.2.1.
pub const MAX_SPS: usize = 16;

/// Picture set count, H.265 7.4.3.3.1.
pub const MAX_PPS: usize = 64;

/// Reference set count, H.265 7.4.8.
pub const MAX_RPS: usize = 64;

/// Pictures per reference set, H.265 7.4.8.
pub const MAX_RPS_PICS: usize = 16;

/// Sequence long-term picture count, H.265 7.4.3.2.1.
pub const MAX_LT_SPS: usize = 32;

/// Slice-header scan bound in RBSP bytes.
pub const HEADER_PREFIX: usize = 1024;

/// VPS NAL type, H.265 7.4.2.2.
pub(crate) const NAL_VPS: u8 = 32;

/// SPS NAL type.
pub(crate) const NAL_SPS: u8 = 33;

/// PPS NAL type.
pub(crate) const NAL_PPS: u8 = 34;

/// Access unit delimiter NAL type.
pub(crate) const NAL_AUD: u8 = 35;

/// End-of-sequence NAL type.
pub(crate) const NAL_EOS: u8 = 36;

/// End-of-bitstream NAL type.
pub(crate) const NAL_EOB: u8 = 37;

/// Filler NAL type.
pub(crate) const NAL_FD: u8 = 38;

/// Prefix SEI NAL type.
pub(crate) const NAL_PREFIX_SEI: u8 = 39;

/// Suffix SEI NAL type.
pub(crate) const NAL_SUFFIX_SEI: u8 = 40;

/// First VCL NAL type.
pub(crate) const VCL_FIRST: u8 = 0;

/// Last VCL NAL type.
pub(crate) const VCL_LAST: u8 = 31;

/// First random access NAL type.
const IRAP_FIRST: u8 = 16;

/// Last random access NAL type.
const IRAP_LAST: u8 = 23;

/// First IDR NAL type.
const IDR_FIRST: u8 = 19;

/// Last IDR NAL type.
const IDR_LAST: u8 = 20;

/// First RASL NAL type.
const RASL_FIRST: u8 = 8;

/// Last RASL NAL type.
const RASL_LAST: u8 = 9;

/// CRA NAL type.
pub(crate) const NAL_CRA: u8 = 21;

/// True for random access NAL types.
#[must_use]
pub fn is_irap(nal_type: u8) -> bool {
    (IRAP_FIRST..=IRAP_LAST).contains(&nal_type)
}

/// True for IDR NAL types.
#[must_use]
pub fn is_idr(nal_type: u8) -> bool {
    (IDR_FIRST..=IDR_LAST).contains(&nal_type)
}

/// True for RASL NAL types.
#[must_use]
pub fn is_rasl(nal_type: u8) -> bool {
    (RASL_FIRST..=RASL_LAST).contains(&nal_type)
}

/// True for CRA and BLA NAL types.
#[must_use]
pub fn is_cra_bla(nal_type: u8) -> bool {
    matches!(nal_type, 16..=18 | NAL_CRA)
}

/// True for BLA NAL types.
#[must_use]
pub fn is_bla(nal_type: u8) -> bool {
    (16..=18).contains(&nal_type)
}

/// True for the CRA NAL type.
#[must_use]
pub fn is_cra(nal_type: u8) -> bool {
    nal_type == NAL_CRA
}

/// True for RADL NAL types.
#[must_use]
pub fn is_radl(nal_type: u8) -> bool {
    matches!(nal_type, 6 | 7)
}

/// True for sub-layer non-reference NAL types.
#[must_use]
pub fn is_slnr(nal_type: u8) -> bool {
    matches!(nal_type, 0 | 2 | 4 | 6 | 8 | 10 | 12 | 14)
}

/// True for pictures anchoring the temporal-0 POC carry, H.265 8.3.1.
#[must_use]
pub fn anchors_tid0(nal_type: u8, temporal_id: u8) -> bool {
    temporal_id == 0 && !is_slnr(nal_type) && !is_radl(nal_type) && !is_rasl(nal_type)
}

/// NAL unit type of the two-byte header's first byte, H.265 7.4.2.2.
#[must_use]
pub const fn nal_unit_type(header: u8) -> u8 {
    (header >> 1) & 0x3F
}

/// Temporal id of the two-byte header's second byte, H.265 7.4.2.2.
///
/// Lenient: a forbidden zero `temporal_id_plus1` reads as id zero.
/// The ingest path rejects it instead.
#[must_use]
pub const fn temporal_id(second: u8) -> u8 {
    (second & 7).saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::{anchors_tid0, nal_unit_type, temporal_id, NaluSplitter};

    #[test]
    fn header_extractors_split_type_and_id() {
        assert_eq!(nal_unit_type(0x28), 20);
        assert_eq!(temporal_id(0x01), 0);
        assert_eq!(temporal_id(0x03), 2);
        assert_eq!(temporal_id(0x00), 0);
    }

    #[test]
    fn anchors_reject_leading_and_sublayer() {
        assert!(anchors_tid0(1, 0));
        assert!(anchors_tid0(19, 0));
        assert!(!anchors_tid0(1, 1));
        for unit in [0, 2, 4, 10, 12, 14] {
            assert!(!anchors_tid0(unit, 0), "sublayer {unit} skips");
        }
        for unit in [6, 7, 8, 9] {
            assert!(!anchors_tid0(unit, 0), "leading {unit} skips");
        }
    }

    /// Payload bytes of each emitted unit.
    fn payloads(units: &[super::Nalu]) -> Vec<Vec<u8>> {
        units.iter().map(|unit| unit.bytes().to_vec()).collect()
    }

    #[test]
    fn splits_mixed_start_codes() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0, 0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 2]);
        assert_eq!(payloads(&units), vec![vec![0x40, 1]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x42, 2]]);
    }

    #[test]
    fn start_code_split_across_pushes_survives() {
        let mut splitter = NaluSplitter::new();
        assert!(splitter.push(&[0, 0, 0, 1, 0x40]).is_empty());
        let units = splitter.push(&[9, 0, 0]);
        assert!(units.is_empty());
        let units = splitter.push(&[1, 0x42]);
        assert_eq!(payloads(&units), vec![vec![0x40, 9]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x42]]);
    }

    #[test]
    fn leading_junk_before_first_start_code_drops() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0xFF, 0xFF, 0, 0, 1, 0x28, 0x01]);
        assert!(units.is_empty());
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x28, 0x01]]);
    }

    #[test]
    fn flush_of_empty_emits_nothing() {
        let mut splitter = NaluSplitter::default();
        assert!(splitter.flush().is_empty());
    }
}
