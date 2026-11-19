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

    /// H.264 NAL unit type of `nal`, absent when empty.
    #[must_use]
    pub fn h264_nal_type(nal: &[u8]) -> Option<u8> {
        nal.first().map(|head| head & 0x1f)
    }

    /// HEVC NAL unit type of `nal`, absent when empty.
    #[must_use]
    pub fn h265_nal_type(nal: &[u8]) -> Option<u8> {
        nal.first().map(|head| (head >> 1) & 0x3f)
    }

    /// NAL units without start codes.
    #[must_use]
    pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
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

    /// Owned NAL units without start codes, preamble dropped.
    ///
    /// Unlike [`split_nals`](Self::split_nals), bytes before the first
    /// start code never surface, empty units stay, and input without a
    /// start code yields nothing.
    #[must_use]
    pub fn split_owned(data: &[u8]) -> Vec<Vec<u8>> {
        let mut starts = Vec::new();
        let mut at = 0;
        while at + 3 <= data.len() {
            if data[at] == 0 && data[at + 1] == 0 {
                if data[at + 2] == 1 {
                    starts.push((at, 3));
                    at += 3;
                    continue;
                }
                if at + 4 <= data.len() && data[at + 2] == 0 && data[at + 3] == 1 {
                    starts.push((at, 4));
                    at += 4;
                    continue;
                }
            }
            at += 1;
        }
        let mut nals = Vec::new();
        for (index, (start, len)) in starts.iter().enumerate() {
            let end = starts.get(index + 1).map_or(data.len(), |(next, _)| *next);
            nals.push(data[start + len..end].to_vec());
        }
        nals
    }

    /// NAL units joined into Annex B: four-byte first start code, three-byte rest.
    #[must_use]
    pub fn join_units(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, nal) in nals.iter().enumerate() {
            if index == 0 {
                out.extend_from_slice(&[0, 0, 0, 1]);
            } else {
                out.extend_from_slice(&[0, 0, 1]);
            }
            out.extend_from_slice(nal);
        }
        out
    }

    /// RBSP without H.264/H.265 emulation-prevention bytes.
    #[must_use]
    pub fn remove_emulation(nal: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for byte in nal {
            if zeros == 2 && *byte == 0x03 {
                zeros = 0;
                continue;
            }
            if *byte == 0x00 {
                zeros += 1;
            } else {
                zeros = 0;
            }
            out.push(*byte);
        }
        out
    }

    /// EBSP with emulation-prevention bytes inserted.
    #[must_use]
    pub fn insert_emulation(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rbsp.len() + 8);
        let mut zeros = 0;
        for byte in rbsp {
            if zeros == 2 && *byte <= 0x03 {
                out.push(0x03);
                zeros = 0;
            }
            if *byte == 0x00 {
                zeros += 1;
            } else {
                zeros = 0;
            }
            out.push(*byte);
        }
        out
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
    fn split_strips_start_codes() {
        assert_eq!(
            AnnexB::split_nals(&[0, 0, 0, 1, 0x67, 0, 0, 1, 0x41]),
            vec![vec![0x67], vec![0x41]]
        );
        assert_eq!(AnnexB::split_nals(&[]), Vec::<Vec<u8>>::new());
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

    #[test]
    fn types_name_nal_headers() {
        assert_eq!(AnnexB::h264_nal_type(&[0x67]), Some(7));
        assert_eq!(AnnexB::h264_nal_type(&[0x41]), Some(1));
        assert_eq!(AnnexB::h264_nal_type(&[]), None);
        assert_eq!(AnnexB::h265_nal_type(&[0x42, 0x01]), Some(33));
        assert_eq!(AnnexB::h265_nal_type(&[]), None);
    }

    #[test]
    fn split_owned_drops_preamble_and_keeps_empties() {
        assert_eq!(
            AnnexB::split_owned(&[0xFF, 0, 0, 1, 0x67, 0, 0, 1, 0, 0, 1, 0x41]),
            vec![vec![0x67], Vec::<u8>::new(), vec![0x41]]
        );
        assert_eq!(AnnexB::split_owned(&[0x41, 0xBB]), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn join_units_mixes_start_codes() {
        assert_eq!(
            AnnexB::join_units(&[vec![0x67], vec![0x41, 0x9A]]),
            vec![0, 0, 0, 1, 0x67, 0, 0, 1, 0x41, 0x9A]
        );
        assert_eq!(AnnexB::join_units(&[]), Vec::<u8>::new());
    }

    #[test]
    fn emulation_roundtrips() {
        let rbsp = vec![0, 0, 1, 0x67, 0, 0, 0, 0x80];
        let ebsp = AnnexB::insert_emulation(&rbsp);
        assert_eq!(ebsp, vec![0, 0, 3, 1, 0x67, 0, 0, 3, 0, 0x80]);
        assert_eq!(AnnexB::remove_emulation(&ebsp), rbsp);
    }
}
