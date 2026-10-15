//! Synthetic H.264 Annex B for d3d12 targets.

use super::bits::{packet, Bits};

/// One packet: sets plus IDR.
pub(crate) fn annex_b_h264_packets() -> Vec<Vec<u8>> {
    vec![packet(&[
        nal(7, 3, &sps_body()),
        nal(8, 3, &pps_body()),
        nal(5, 2, &idr_slice_body()),
    ])]
}

/// NAL unit with header over the EBSP body.
fn nal(nal_type: u8, ref_idc: u8, body: &[u8]) -> Vec<u8> {
    let mut nal = vec![ref_idc << 5 | nal_type];
    nal.extend_from_slice(body);
    nal
}

/// Baseline 320x240 sequence set body.
fn sps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(66, 8);
    writer.bits(0, 8);
    writer.bits(30, 8);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(1);
    writer.bit(false);
    writer.ue(19);
    writer.ue(14);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
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
