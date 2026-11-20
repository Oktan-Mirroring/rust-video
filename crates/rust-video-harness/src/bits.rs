use std::fmt;

use crate::annexb::AnnexB;

/// Test bit writer with emulation prevention.
#[derive(Default)]
pub struct Bits {
    /// Finished bytes.
    bytes: Vec<u8>,
    /// Open byte under fill.
    current: u8,
    /// Bits filled in the open byte.
    filled: u8,
}

impl Bits {
    /// Empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    /// Single bit.
    pub fn bit(&mut self, set: bool) {
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
    pub fn bits(&mut self, value: u32, count: u32) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 != 0);
        }
    }

    /// Eight bits of `value`, most significant first.
    pub fn byte(&mut self, value: u8) {
        self.bits(u32::from(value), 8);
    }

    /// `bits` verbatim, most significant first.
    pub fn copy(&mut self, bits: &[bool]) {
        for bit in bits {
            self.bit(*bit);
        }
    }

    /// Unsigned Exp-Golomb code.
    pub fn ue(&mut self, value: u32) {
        let code = value + 1;
        let width = 32 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        self.bits(code, width);
    }

    /// Signed Exp-Golomb code.
    pub fn se(&mut self, value: i32) {
        let code = if value > 0 {
            u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
        } else {
            u32::try_from(-value).unwrap_or(u32::MAX) * 2
        };
        self.ue(code);
    }

    /// Finished EBSP with emulation prevention inserted.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        AnnexB::insert_emulation(&self.finish_rbsp())
    }

    /// Finished bytes, zero-padded without a stop bit.
    #[must_use]
    pub fn finish_raw(mut self) -> Vec<u8> {
        while self.filled != 0 {
            self.bit(false);
        }
        self.bytes
    }

    /// Finished RBSP with stop bit and alignment.
    #[must_use]
    pub fn finish_rbsp(mut self) -> Vec<u8> {
        self.bit(true);
        while self.filled != 0 {
            self.bit(false);
        }
        self.bytes
    }
}

/// Test-only bit reader over RBSP bytes, not a grammar reader.
#[derive(Clone, Copy, Debug)]
pub struct BitReader<'a> {
    /// Bytes behind the read position.
    bytes: &'a [u8],
    /// Bits consumed.
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// Reader over `bytes`.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Bits consumed.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Next bit, most significant first.
    ///
    /// # Errors
    ///
    /// Returns [`BitOverrun`] past the last bit.
    pub fn bit(&mut self) -> Result<u32, BitOverrun> {
        if self.pos >= self.bytes.len() * 8 {
            return Err(BitOverrun);
        }
        let byte = self.bytes[self.pos / 8];
        let shift = 7 - (self.pos % 8);
        self.pos += 1;
        Ok(u32::from((byte >> shift) & 1))
    }

    /// Next `count` bits as a value, most significant first.
    ///
    /// # Errors
    ///
    /// Returns [`BitOverrun`] past the last bit.
    pub fn bits(&mut self, count: usize) -> Result<u32, BitOverrun> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }

    /// Next eight bits, most significant first.
    ///
    /// # Errors
    ///
    /// Returns [`BitOverrun`] past the last bit.
    pub fn byte(&mut self) -> Result<u8, BitOverrun> {
        let mut value = 0u8;
        for _ in 0..8 {
            value = (value << 1) | u8::from(self.bit()? == 1);
        }
        Ok(value)
    }

    /// Unsigned Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns [`BitOverrun`] past the last bit or past a 31-zero prefix.
    pub fn ue(&mut self) -> Result<u32, BitOverrun> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(BitOverrun);
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let rest = self.bits(zeros)?;
        Ok(((1 << zeros) - 1) + rest)
    }

    /// Remaining bits verbatim.
    #[must_use]
    pub fn rest(&self) -> Vec<bool> {
        let total = self.bytes.len() * 8;
        (self.pos..total)
            .map(|at| self.bytes[at / 8] >> (7 - at % 8) & 1 == 1)
            .collect()
    }

    /// `bits` without RBSP trailing: stop bit plus alignment zeros.
    #[must_use]
    pub fn strip_trailing(mut bits: Vec<bool>) -> Vec<bool> {
        while bits.pop() == Some(false) {}
        bits
    }
}

/// Bit-reader exhaustion past the RBSP end.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BitOverrun;

impl fmt::Display for BitOverrun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bitstream overrun")
    }
}

impl std::error::Error for BitOverrun {}

impl From<BitOverrun> for String {
    fn from(_: BitOverrun) -> Self {
        BitOverrun.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{BitOverrun, BitReader, Bits};

    #[test]
    fn golomb_codes_match_bits() {
        let mut writer = Bits::new();
        writer.ue(0);
        assert_eq!(writer.finish(), vec![0xC0]);
        let mut writer = Bits::new();
        writer.se(-1);
        assert_eq!(writer.finish(), vec![0x70]);
    }

    #[test]
    fn finish_inserts_emulation_prevention() {
        let mut writer = Bits::new();
        writer.bits(0, 24);
        assert_eq!(writer.finish(), vec![0, 0, 3, 0, 0x80]);
    }

    #[test]
    fn finish_raw_zero_pads_without_stop_bit() {
        let mut writer = Bits::new();
        writer.bits(0b101, 3);
        assert_eq!(writer.finish_raw(), vec![0b1010_0000]);
    }

    #[test]
    fn finish_rbsp_stops_without_prevention() {
        let mut writer = Bits::new();
        writer.ue(0);
        assert_eq!(writer.finish_rbsp(), vec![0xC0]);
        let mut writer = Bits::new();
        writer.bits(0, 24);
        assert_eq!(writer.finish_rbsp(), vec![0, 0, 0, 0x80]);
    }

    #[test]
    fn byte_and_copy_roundtrip_through_reader() {
        let mut writer = Bits::new();
        writer.byte(0xA5);
        writer.copy(&[true, false]);
        writer.ue(1);
        let bytes = writer.finish_raw();
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.byte().expect("byte"), 0xA5);
        assert_eq!(reader.pos(), 8);
        assert_eq!(reader.bit().expect("bit"), 1);
        assert_eq!(reader.bit().expect("bit"), 0);
        assert_eq!(reader.ue().expect("ue"), 1);
    }

    #[test]
    fn reader_reports_overrun() {
        let mut reader = BitReader::new(&[0xFF]);
        assert_eq!(reader.bits(8).expect("byte"), 0xFF);
        assert_eq!(reader.bit().unwrap_err(), BitOverrun);
        assert_eq!(BitOverrun.to_string(), "bitstream overrun");
        let message: String = BitOverrun.into();
        assert_eq!(message, "bitstream overrun");
    }

    #[test]
    fn strip_trailing_drops_stop_and_alignment() {
        let mut writer = Bits::new();
        writer.bits(0b101, 3);
        let bytes = writer.finish_rbsp();
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.bits(3).expect("head"), 0b101);
        assert_eq!(BitReader::strip_trailing(reader.rest()), Vec::<bool>::new());
    }
}
