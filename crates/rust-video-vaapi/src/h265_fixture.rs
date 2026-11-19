//! Synthetic H.265 NAL payloads for unit tests.

use rust_video_harness::Bits;

/// Annex B NAL of `type_` with `payload`, start code included.
pub(crate) fn nal(type_: u8, payload: &[u8]) -> Vec<u8> {
    let mut nal = vec![0, 0, 0, 1, type_ << 1, 1];
    nal.extend_from_slice(payload);
    nal
}

/// Minimal 64x64 Main sequence set payload.
pub(crate) fn sps_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.bits(0, 2);
    writer.bit(false);
    writer.bits(1, 5);
    for index in 0..32 {
        writer.bit(index == 1);
    }
    writer.bit(true);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bits(0, 31);
    writer.bits(0, 12);
    writer.bit(false);
    writer.bits(120, 8);
    writer.ue(0);
    writer.ue(1);
    writer.ue(64);
    writer.ue(64);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(4);
    writer.bit(true);
    writer.ue(1);
    writer.ue(1);
    writer.ue(0);
    writer.ue(0);
    writer.ue(3);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// Minimal picture set payload over sequence set 0.
pub(crate) fn pps_payload() -> Vec<u8> {
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

/// IDR slice payload.
pub(crate) fn idr_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bit(true);
    writer.ue(0);
    writer.ue(2);
    writer.se(0);
    writer.finish()
}

/// P slice payload with order lsb `lsb`.
///
/// The set holds one negative picture `delta_minus1` plus one below
/// the current order; the fixture sequence set carries no set to
/// predict from, so no prediction flag is emitted.
pub(crate) fn p_payload(lsb: u32, delta_minus1: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.ue(0);
    writer.ue(1);
    writer.bits(lsb, 8);
    writer.bit(false);
    writer.ue(1);
    writer.ue(0);
    writer.ue(delta_minus1);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.se(0);
    writer.finish()
}
