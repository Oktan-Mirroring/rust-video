//! Byte vectors shared by grammar tests.

use crate::error::test_support::TestError;
use crate::error::StrictGate;
use crate::pps::{parse_pps, Pps};
use crate::sps::{parse_sps, Sps};

/// Test bit writer with emulation prevention.
pub(crate) struct BitWriter {
    /// Finished bytes.
    bytes: Vec<u8>,
    /// Open byte under fill.
    current: u8,
    /// Bits filled in the open byte.
    filled: u8,
}

impl BitWriter {
    /// Empty writer.
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    /// Single bit.
    pub(crate) fn bit(&mut self, set: bool) {
        if set {
            self.current |= 1 << (7 - self.filled);
        }
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.filled = 0;
        }
    }

    /// Low `count` bits of `value`, most significant first.
    pub(crate) fn bits(&mut self, value: u32, count: u32) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 != 0);
        }
    }

    /// Unsigned Exp-Golomb code.
    pub(crate) fn ue(&mut self, value: u32) {
        let code = value + 1;
        let width = 32 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        self.bits(code, width);
    }

    /// Signed Exp-Golomb code.
    pub(crate) fn se(&mut self, value: i32) {
        let code = if value > 0 {
            u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
        } else {
            u32::try_from(-value).unwrap_or(u32::MAX) * 2
        };
        self.ue(code);
    }

    /// Stop bit plus byte alignment.
    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.filled != 0 {
            self.bit(false);
        }
        self.bytes
    }

    /// Finished RBSP with emulation prevention inserted.
    pub(crate) fn finish_ebsp(self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut zeros = 0;
        for byte in self.finish() {
            if zeros == 2 && byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        out
    }
}

/// NAL unit with header over the EBSP body.
pub(crate) fn nal(nal_type: u8, ref_idc: u8, body: &[u8]) -> Vec<u8> {
    let mut nal = vec![ref_idc << 5 | nal_type];
    nal.extend_from_slice(body);
    nal
}

/// Packet of NAL units with four-byte start codes.
pub(crate) fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut packet = Vec::new();
    for nal in nals {
        packet.extend_from_slice(&[0, 0, 0, 1]);
        packet.extend_from_slice(nal);
    }
    packet
}

/// Baseline prologue through the crop flag.
pub(crate) fn sps_prologue(writer: &mut BitWriter) {
    sps_prologue_no_crop(writer);
    writer.bit(false);
}

/// Baseline prologue through direct 8x8 inference.
fn sps_prologue_no_crop(writer: &mut BitWriter) {
    writer.bits(66, 8);
    writer.bits(0, 8);
    writer.bits(30, 8);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(1);
    writer.bit(false);
    writer.ue(119);
    writer.ue(67);
    writer.bit(true);
    writer.bit(true);
}

/// Baseline 1920x1088 sequence set body.
pub(crate) fn sps_body() -> Vec<u8> {
    let mut writer = BitWriter::new();
    sps_prologue(&mut writer);
    writer.bit(false);
    writer.finish_ebsp()
}

/// Baseline 1920x1088 sequence set body with VUI carrying `restriction`.
pub(crate) fn sps_body_vui(restriction: Option<u32>) -> Vec<u8> {
    let mut writer = BitWriter::new();
    sps_prologue(&mut writer);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    let Some(reorder) = restriction else {
        writer.bit(false);
        return writer.finish_ebsp();
    };
    writer.bit(true);
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(reorder);
    writer.ue(0);
    writer.finish_ebsp()
}

/// High 320x240 sequence set body with chroma sampling `chroma`.
pub(crate) fn fidelity_sps_body(chroma: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.bits(100, 8);
    writer.bits(0, 8);
    writer.bits(40, 8);
    writer.ue(0);
    writer.ue(chroma);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
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
    writer.finish_ebsp()
}

/// Baseline 1920x1088 sequence set body with crop offsets.
pub(crate) fn crop_sps_body(left: u32, right: u32, top: u32, bottom: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    sps_prologue_no_crop(&mut writer);
    writer.bit(true);
    writer.ue(left);
    writer.ue(right);
    writer.ue(top);
    writer.ue(bottom);
    writer.bit(false);
    writer.finish_ebsp()
}

/// Sequence set used across tests.
pub(crate) fn sps() -> Sps {
    parse_sps::<TestError, StrictGate>(&sps_body()).expect("valid sps")
}

/// Picture set body over sequence set zero.
pub(crate) fn pps_body() -> Vec<u8> {
    pps_body_bipred(0)
}

/// Picture set body with biprediction mode `idc`.
pub(crate) fn pps_body_bipred(idc: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bits(idc, 2);
    writer.se(0);
    writer.se(0);
    writer.se(0);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.se(0);
    writer.finish_ebsp()
}

/// Picture set body with redundant pictures present.
pub(crate) fn pps_body_redundant() -> Vec<u8> {
    let mut writer = BitWriter::new();
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
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.se(0);
    writer.finish_ebsp()
}

/// Picture set used across tests.
pub(crate) fn pps() -> Pps {
    parse_pps::<TestError, StrictGate>(&pps_body(), Some(&sps())).expect("valid pps")
}

/// Picture set body with chroma offset `qp` and no tail.
pub(crate) fn pps_body_chroma_no_tail(qp: i32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bits(0, 2);
    writer.se(0);
    writer.se(0);
    writer.se(qp);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.finish_ebsp()
}

/// IDR slice header body over the test sets.
pub(crate) fn idr_slice_body() -> Vec<u8> {
    let mut writer = BitWriter::new();
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
    writer.finish_ebsp()
}

/// P slice header body with `frame_num` and `lsb`.
pub(crate) fn p_slice_body(frame_num: u32, lsb: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
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
    writer.finish_ebsp()
}

/// P slice header body with redundant picture count `count`.
pub(crate) fn p_slice_body_redundant(count: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bits(1, 4);
    writer.bits(2, 4);
    writer.ue(count);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.se(0);
    writer.ue(0);
    writer.se(0);
    writer.se(0);
    writer.finish_ebsp()
}

/// P slice header body with adaptive marking assigning long-term `index`.
pub(crate) fn p_slice_body_mmco(index: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bits(1, 4);
    writer.bits(2, 4);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.ue(6);
    writer.ue(index);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.ue(0);
    writer.se(0);
    writer.se(0);
    writer.finish_ebsp()
}
