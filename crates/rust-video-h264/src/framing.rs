//! Annex B framing over NAL units.

pub use rust_video_core::nalu::{rbsp, rbsp_prefix, split_nals, Crop, Nalu, NaluSplitter};

/// Sequence set count, H.264 7.4.2.1.
pub const MAX_SPS: usize = 32;

/// Picture set count, H.264 7.4.2.2.
pub const MAX_PPS: usize = 256;

/// Slice-header scan bound in RBSP bytes.
pub const HEADER_PREFIX: usize = 1024;

/// NAL unit type of `header`, H.264 7.4.1.
#[must_use]
pub const fn nal_unit_type(header: u8) -> u8 {
    header & 0x1F
}

/// NAL reference idc of `header`, H.264 7.4.1.
#[must_use]
pub const fn nal_ref_idc(header: u8) -> u8 {
    (header >> 5) & 3
}

/// True for IDR slice NAL types.
#[must_use]
pub const fn is_idr(nal_type: u8) -> bool {
    nal_type == 5
}

#[cfg(test)]
mod tests {
    use super::{is_idr, nal_ref_idc, nal_unit_type, rbsp, rbsp_prefix, split_nals, NaluSplitter};

    #[test]
    fn split_nals_strips_start_codes() {
        let nals = split_nals(b"\0\0\0\x01\x67abc\0\0\x01\x68de");
        assert_eq!(nals, vec![b"\x67abc".as_slice(), b"\x68de".as_slice()]);
    }

    #[test]
    fn split_nals_edges_match() {
        assert_eq!(split_nals(b""), [] as [&[u8]; 0]);
        assert_eq!(split_nals(b"\0\0\x01"), [] as [&[u8]; 0]);
        assert_eq!(split_nals(b"\0\0\x01\0\0\x01"), [] as [&[u8]; 0]);
        assert_eq!(split_nals(b"\x01\0\0\x01"), vec![b"\x01".as_slice()]);
        assert_eq!(
            split_nals(b"\x67\0\x01\x68\0\0\0\x01\x69"),
            vec![b"\x67\0\x01\x68".as_slice(), b"\x69".as_slice()]
        );
    }

    #[test]
    fn rbsp_removes_prevention_bytes() {
        assert_eq!(
            rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 5]).into_owned(),
            vec![0, 0, 1, 0, 0, 0, 5]
        );
    }

    #[test]
    fn rbsp_borrows_without_prevention_bytes() {
        let nal = [0x65, 0x88, 0x00, 0x02];
        assert!(matches!(rbsp(&nal), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn rbsp_prefix_matches_full_strip() {
        let nal = [0x65, 0, 0, 3, 1, 0, 0, 3, 0, 5, 9, 9, 9];
        let full = rbsp(&nal).into_owned();
        let mut buf = [0u8; 8];
        assert_eq!(rbsp_prefix(&nal, &mut buf), &full[..8]);
        let mut buf = [0u8; 64];
        assert_eq!(rbsp_prefix(&nal, &mut buf), full.as_slice());
    }

    #[test]
    fn header_classifiers_split_idc_and_type() {
        assert_eq!(nal_unit_type(0x65), 5);
        assert_eq!(nal_ref_idc(0x65), 3);
        assert!(is_idr(nal_unit_type(0x65)));
        assert_eq!(nal_unit_type(0x41), 1);
        assert_eq!(nal_ref_idc(0x41), 2);
        assert!(!is_idr(nal_unit_type(0x41)));
        assert!(!is_idr(nal_unit_type(0x67)));
    }

    /// Payload bytes of each emitted unit.
    fn payloads(units: &[super::Nalu]) -> Vec<Vec<u8>> {
        units.iter().map(|unit| unit.bytes().to_vec()).collect()
    }

    #[test]
    fn splits_mixed_start_codes() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2]);
        assert_eq!(payloads(&units), vec![vec![0x67, 1]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x68, 2]]);
    }

    #[test]
    fn start_code_split_across_pushes_survives() {
        let mut splitter = NaluSplitter::new();
        assert!(splitter.push(&[0, 0, 0, 1, 0x67]).is_empty());
        let units = splitter.push(&[9, 0, 0]);
        assert!(units.is_empty());
        let units = splitter.push(&[1, 0x68]);
        assert_eq!(payloads(&units), vec![vec![0x67, 9]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x68]]);
    }

    #[test]
    fn leading_junk_before_first_start_code_drops() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0xFF, 0xFF, 0, 0, 1, 0x65]);
        assert!(units.is_empty());
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x65]]);
    }

    #[test]
    fn flush_of_empty_emits_nothing() {
        let mut splitter = NaluSplitter::default();
        assert!(splitter.flush().is_empty());
    }
}
