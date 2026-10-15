//! Synthetic H.264 Annex B for d3d12 targets.

use super::bits::{packet, Bits};

/// One packet: sets plus IDR.
pub(crate) fn annex_b_h264_packets() -> Vec<Vec<u8>> {
    sets_packet(&sps_body())
}

/// One packet: bottom-cropped sets plus IDR, 320x224 visible in 320x240.
pub(crate) fn annex_b_h264_packets_cropped() -> Vec<Vec<u8>> {
    sets_packet(&sps_body_crop(8))
}

/// One packet: bottom-cropped sets plus IDR at coded macroblock extents.
pub(crate) fn annex_b_h264_packets_cropped_sized(
    width_mbs_minus1: u32,
    height_mbs_minus1: u32,
    bottom: u32,
    level_idc: u8,
) -> Vec<Vec<u8>> {
    sets_packet(&sps_body_sized(
        width_mbs_minus1,
        height_mbs_minus1,
        bottom,
        level_idc,
    ))
}

/// One packet: `sps` plus picture set plus IDR.
fn sets_packet(sps: &[u8]) -> Vec<Vec<u8>> {
    vec![packet(&[
        nal(7, 3, sps),
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
    sps_body_crop(0)
}

/// Baseline 320x240 sequence set body with `bottom` crop units.
fn sps_body_crop(bottom: u32) -> Vec<u8> {
    sps_body_sized(19, 14, bottom, 30)
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
