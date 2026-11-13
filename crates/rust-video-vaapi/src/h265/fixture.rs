//! Synthetic H.265 NAL payloads for unit tests.

use crate::h265::parser::NaluType;

/// Bit writer for synthetic NAL payloads.
#[derive(Default)]
pub(crate) struct Writer {
    /// Finished bytes.
    out: Vec<u8>,
    /// Open byte, filled from the top.
    cur: u8,
    /// Bits in `cur`.
    n: u8,
}

impl Writer {
    /// Empty writer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append one bit.
    pub(crate) fn bit(&mut self, value: bool) {
        self.cur = (self.cur << 1) | u8::from(value);
        self.n += 1;
        if self.n == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.n = 0;
        }
    }

    /// Append the low `count` bits of `value`.
    pub(crate) fn bits(&mut self, value: u32, count: usize) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 == 1);
        }
    }

    /// Append an unsigned Exp-Golomb code.
    pub(crate) fn ue(&mut self, value: u32) {
        let code = value + 1;
        let len = 32 - code.leading_zeros() as usize;
        for _ in 1..len {
            self.bit(false);
        }
        self.bits(code, len);
    }

    /// Append a signed Exp-Golomb code.
    pub(crate) fn se(&mut self, value: i32) {
        let code = u64::from(value.unsigned_abs()) * 2 - u64::from(value > 0);
        self.ue(u32::try_from(code).unwrap_or(u32::MAX));
    }

    /// Finished payload with stop bit, alignment, and emulation prevention.
    pub(crate) fn bytes(mut self) -> Vec<u8> {
        self.bit(true);
        while self.n != 0 {
            self.bit(false);
        }
        let mut ebsp = Vec::with_capacity(self.out.len() + 2);
        let mut zeros = 0;
        for byte in self.out {
            if zeros == 2 && byte <= 0x03 {
                ebsp.push(0x03);
                zeros = 0;
            }
            ebsp.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        ebsp
    }
}

/// Annex B NAL of `type_` with `payload`, start code included.
pub(crate) fn nal(type_: NaluType, payload: &[u8]) -> Vec<u8> {
    let mut nal = vec![0, 0, 0, 1, (type_ as u8) << 1, 1];
    nal.extend_from_slice(payload);
    nal
}

/// Minimal 64x64 Main sequence set payload.
pub(crate) fn sps_payload() -> Vec<u8> {
    let mut writer = Writer::new();
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
    writer.bytes()
}

/// Minimal picture set payload over sequence set 0.
pub(crate) fn pps_payload() -> Vec<u8> {
    let mut writer = Writer::new();
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
    writer.bytes()
}

/// IDR slice payload.
pub(crate) fn idr_payload() -> Vec<u8> {
    let mut writer = Writer::new();
    writer.bit(true);
    writer.bit(true);
    writer.ue(0);
    writer.ue(2);
    writer.se(0);
    writer.bytes()
}

/// P slice payload with order lsb `lsb`.
///
/// The set holds one negative picture `delta_minus1` plus one below
/// the current order; the fixture sequence set carries no set to
/// predict from, so no prediction flag is emitted.
pub(crate) fn p_payload(lsb: u32, delta_minus1: u32) -> Vec<u8> {
    let mut writer = Writer::new();
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
    writer.bytes()
}
