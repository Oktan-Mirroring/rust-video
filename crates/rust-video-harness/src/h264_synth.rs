use crate::{AnnexB, Bits};

/// Synthetic H.264 Annex B for tier targets.
pub struct H264Synth;

impl H264Synth {
    /// Two packets: sets plus IDR, then one P picture.
    #[must_use]
    pub fn annex_b_packets() -> Vec<Vec<u8>> {
        let sets = AnnexB::packet(&[
            Self::nal(7, 3, &Self::sps_body_sized(3, 3, 0, 30)),
            Self::nal(8, 3, &Self::pps_body()),
            Self::nal(5, 2, &Self::idr_slice_body()),
        ]);
        let inter = AnnexB::packet(&[Self::nal(1, 2, &Self::p_slice_body(1, 2))]);
        vec![sets, inter]
    }

    /// One packet: sets plus IDR.
    #[must_use]
    pub fn annex_b_h264_packets() -> Vec<Vec<u8>> {
        Self::sets_packet(&Self::sps_body_crop(0))
    }

    /// One packet: bottom-cropped sets plus IDR, 320x224 visible in 320x240.
    #[must_use]
    pub fn annex_b_h264_packets_cropped() -> Vec<Vec<u8>> {
        Self::sets_packet(&Self::sps_body_crop(8))
    }

    /// One packet: bottom-cropped sets plus IDR at coded macroblock extents.
    #[must_use]
    pub fn annex_b_h264_packets_cropped_sized(
        width_mbs_minus1: u32,
        height_mbs_minus1: u32,
        bottom: u32,
        level_idc: u8,
    ) -> Vec<Vec<u8>> {
        Self::sets_packet(&Self::sps_body_sized(
            width_mbs_minus1,
            height_mbs_minus1,
            bottom,
            level_idc,
        ))
    }

    /// One packet: `sps` plus picture set plus IDR.
    fn sets_packet(sps: &[u8]) -> Vec<Vec<u8>> {
        vec![AnnexB::packet(&[
            Self::nal(7, 3, sps),
            Self::nal(8, 3, &Self::pps_body()),
            Self::nal(5, 2, &Self::idr_slice_body()),
        ])]
    }

    /// NAL unit with header over the EBSP body.
    fn nal(nal_type: u8, ref_idc: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![ref_idc << 5 | nal_type];
        nal.extend_from_slice(body);
        nal
    }

    /// Baseline 320x240 sequence set body with `bottom` crop units.
    fn sps_body_crop(bottom: u32) -> Vec<u8> {
        Self::sps_body_sized(19, 14, bottom, 30)
    }

    /// Baseline sequence set body at coded extents with `bottom` crop units.
    fn sps_body_sized(
        width_mbs_minus1: u32,
        height_mbs_minus1: u32,
        bottom: u32,
        level_idc: u8,
    ) -> Vec<u8> {
        let mut writer = Bits::new();
        writer.bits(66, 8);
        writer.bits(0, 8);
        writer.bits(u32::from(level_idc), 8);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(1);
        writer.bit(false);
        writer.ue(width_mbs_minus1);
        writer.ue(height_mbs_minus1);
        writer.bit(true);
        writer.bit(true);
        if bottom == 0 {
            writer.bit(false);
        } else {
            writer.bit(true);
            writer.ue(0);
            writer.ue(0);
            writer.ue(0);
            writer.ue(bottom);
        }
        writer.bit(false);
        writer.finish()
    }

    /// Picture set body over sequence set zero.
    fn pps_body() -> Vec<u8> {
        let mut writer = Bits::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bits(0, 2);
        writer.se(0);
        writer.se(0);
        writer.se(0);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.se(0);
        writer.finish()
    }

    /// IDR slice header body over the synthetic sets.
    fn idr_slice_body() -> Vec<u8> {
        let mut writer = Bits::new();
        writer.ue(0);
        writer.ue(2);
        writer.ue(0);
        writer.bits(0, 4);
        writer.ue(0);
        writer.bits(0, 4);
        writer.bit(false);
        writer.bit(false);
        writer.se(0);
        writer.ue(0);
        writer.se(0);
        writer.se(0);
        writer.finish()
    }

    /// P slice header body with `frame_num` and `lsb`.
    fn p_slice_body(frame_num: u32, lsb: u32) -> Vec<u8> {
        let mut writer = Bits::new();
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bits(frame_num, 4);
        writer.bits(lsb, 4);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.ue(0);
        writer.se(0);
        writer.se(0);
        writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::H264Synth;

    #[test]
    fn packets_carry_sets_then_pictures() {
        let packets = H264Synth::annex_b_packets();
        assert_eq!(packets.len(), 2);
        assert_eq!(&packets[0][..5], &[0, 0, 0, 1, 0x67]);
        assert_eq!(&packets[1][..5], &[0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn crop_flows_into_sps() {
        let plain = H264Synth::annex_b_h264_packets();
        let cropped = H264Synth::annex_b_h264_packets_cropped();
        assert_eq!(plain.len(), 1);
        assert_eq!(cropped.len(), 1);
        assert_ne!(plain, cropped);
        let sized = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
        assert_eq!(sized.len(), 1);
        assert_eq!(&sized[0][..5], &[0, 0, 0, 1, 0x67]);
    }

    #[test]
    fn sized_blob_matches_builder() {
        let built = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
        let blob =
            std::fs::read(crate::bench_dir().join("h264-1080p-synth.h264")).expect("blob readable");
        assert_eq!(built, vec![blob]);
    }
}
