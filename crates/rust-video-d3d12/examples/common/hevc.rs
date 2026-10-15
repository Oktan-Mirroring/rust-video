//! Synthetic HEVC Annex B for d3d12 targets.

use super::bits::{packet, Bits};

/// Two packets: sets plus IDR, then one more IDR.
pub(crate) fn annex_b_packets() -> Vec<Vec<u8>> {
    let sets = packet(&[
        nal(33, &sps_body()),
        nal(34, &pps_body()),
        nal(19, &idr_slice_body()),
    ]);
    let second = packet(&[nal(19, &idr_slice_body())]);
    vec![sets, second]
}

/// NAL unit with two-byte header over the body.
fn nal(nal_type: u8, body: &[u8]) -> Vec<u8> {
    let mut nal = vec![nal_type << 1, 1];
    nal.extend_from_slice(body);
    nal
}

/// Main-profile 320x240 sequence set body.
fn sps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.bits(0, 2);
    writer.bit(false);
    writer.bits(1, 5);
    writer.bits(0x4000_0000, 32);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bits(0, 32);
    writer.bits(0, 11);
    writer.bit(false);
    writer.bits(93, 8);
    writer.ue(0);
    writer.ue(1);
    writer.ue(320);
    writer.ue(240);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(1);
    writer.ue(1);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
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
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.se(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// IDR slice header body over the synthetic sets.
fn idr_slice_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(2);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}
