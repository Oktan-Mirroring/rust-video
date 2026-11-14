use std::{fs, path::Path};

/// Annex B NAL splitting and picture grouping.
pub struct AnnexB;

impl AnnexB {
    /// Start-coded NALs grouped on `is_vcl` picture boundaries.
    #[must_use]
    pub fn group_pictures(data: &[u8], is_vcl: impl Fn(&[u8]) -> bool) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut current = Vec::new();
        let mut seen_vcl = false;
        for nal in Self::split_nals(data) {
            let vcl = is_vcl(nal);
            if vcl && seen_vcl {
                packets.push(std::mem::take(&mut current));
                seen_vcl = false;
            }
            seen_vcl |= vcl;
            current.extend_from_slice(&[0, 0, 0, 1]);
            current.extend_from_slice(nal);
        }
        if !current.is_empty() {
            packets.push(current);
        }
        packets
    }

    /// True for H.264 VCL NAL units.
    #[must_use]
    pub fn is_h264_vcl(nal: &[u8]) -> bool {
        matches!(nal.first().map(|header| header & 0x1F), Some(1 | 5))
    }

    /// True for HEVC VCL NAL units.
    #[must_use]
    pub fn is_hevc_vcl(nal: &[u8]) -> bool {
        matches!(nal.first().map(|header| header >> 1), Some(0..=31))
    }

    /// File bytes at `path` grouped into one packet per picture.
    ///
    /// # Panics
    ///
    /// Panics when `path` is unreadable.
    #[must_use]
    pub fn picture_packets(path: &Path, is_vcl: impl Fn(&[u8]) -> bool) -> Vec<Vec<u8>> {
        let data = fs::read(path).expect("vector readable");
        Self::group_pictures(&data, is_vcl)
    }

    /// Packet of NAL units with four-byte start codes.
    #[must_use]
    pub fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut packet = Vec::new();
        for nal in nals {
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(nal);
        }
        packet
    }

    /// NAL units without start codes.
    fn split_nals(data: &[u8]) -> Vec<&[u8]> {
        let mut nals = Vec::new();
        let mut start = 0;
        let mut zeros = 0;
        let mut index = 0;
        while index < data.len() {
            if data[index] == 1 && zeros >= 2 {
                let end = index - zeros;
                if end > start {
                    nals.push(&data[start..end]);
                }
                start = index + 1;
                zeros = 0;
            } else if data[index] == 0 {
                zeros += 1;
            } else {
                zeros = 0;
            }
            index += 1;
        }
        if start < data.len() {
            nals.push(&data[start..]);
        }
        nals
    }
}

#[cfg(test)]
mod tests {
    use super::AnnexB;

    #[test]
    fn predicates_name_vcl() {
        assert!(AnnexB::is_h264_vcl(&[0x41, 0x9A]));
        assert!(AnnexB::is_h264_vcl(&[0x65, 0x88]));
        assert!(!AnnexB::is_h264_vcl(&[0x67, 0x42]));
        assert!(!AnnexB::is_h264_vcl(&[]));
        assert!(AnnexB::is_hevc_vcl(&[0x28, 0x01]));
        assert!(!AnnexB::is_hevc_vcl(&[0x40, 0x01]));
        assert!(!AnnexB::is_hevc_vcl(&[]));
    }

    #[test]
    fn packet_joins_nals_with_start_codes() {
        assert_eq!(
            AnnexB::packet(&[vec![0x67], vec![0x41, 0x9A]]),
            vec![0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x41, 0x9A]
        );
        assert_eq!(AnnexB::packet(&[]), Vec::<u8>::new());
    }

    #[test]
    fn group_splits_on_second_vcl() {
        let data = vec![
            0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x41, 0xBB, 0, 0, 0, 1, 0x41, 0xCC,
        ];
        assert_eq!(
            AnnexB::group_pictures(&data, AnnexB::is_h264_vcl),
            vec![
                vec![0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x41, 0xBB],
                vec![0, 0, 0, 1, 0x41, 0xCC],
            ]
        );
    }

    #[test]
    fn group_handles_degenerate_input() {
        assert_eq!(
            AnnexB::group_pictures(&[], AnnexB::is_h264_vcl),
            Vec::<Vec<u8>>::new()
        );
        assert_eq!(
            AnnexB::group_pictures(&[0x41, 0xBB], AnnexB::is_h264_vcl),
            vec![vec![0, 0, 0, 1, 0x41, 0xBB]]
        );
    }
}
