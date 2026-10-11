//! Exp-Golomb bit reader over RBSP bytes.

use crate::error::ParseError;

/// Bit reader over one NAL payload, most significant bit first.
pub(crate) struct BitReader<'a> {
    /// Payload bytes.
    bytes: &'a [u8],
    /// Bit offset of the next read.
    bit: usize,
}

impl<'a> BitReader<'a> {
    /// Reader over `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit: 0 }
    }

    /// Bits consumed so far.
    #[must_use]
    pub fn consumed(&self) -> usize {
        self.bit
    }

    /// True while unread bits remain.
    #[must_use]
    pub fn more(&self) -> bool {
        self.bit < self.bytes.len() * 8
    }

    /// True while data bits remain past the stop bit and alignment.
    #[must_use]
    pub fn more_rbsp(&self) -> bool {
        if !self.more() {
            return false;
        }
        let total = self.bytes.len() * 8;
        let mut last = total;
        while last > self.bit {
            let byte = self.bytes[(last - 1) / 8];
            if (byte >> (7 - (last - 1) % 8)) & 1 == 1 {
                break;
            }
            last -= 1;
        }
        last > self.bit && last - 1 > self.bit
    }

    /// Next `n` bits as an integer.
    ///
    /// # Errors
    ///
    /// Returns `Err` when fewer than `n` bits remain or `n` exceeds 32.
    pub fn bits(&mut self, syntax: &'static str, n: u32) -> Result<u32, ParseError> {
        if n > 32 || self.bit + n as usize > self.bytes.len() * 8 {
            return Err(ParseError::Truncated { syntax });
        }
        let mut value = 0_u32;
        for _ in 0..n {
            let byte = self.bytes[self.bit / 8];
            let shift = 7 - (self.bit % 8);
            value = (value << 1) | u32::from((byte >> shift) & 1);
            self.bit += 1;
        }
        Ok(value)
    }

    /// Next bit.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no bits remain.
    pub fn bit(&mut self, syntax: &'static str) -> Result<bool, ParseError> {
        Ok(self.bits(syntax, 1)? != 0)
    }

    /// Unsigned Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation, or when the code exceeds 32 bits.
    pub fn ue(&mut self, syntax: &'static str) -> Result<u32, ParseError> {
        let mut zeros = 0_u32;
        while !self.bit(syntax)? {
            zeros += 1;
            if zeros > 31 {
                return Err(ParseError::Overflow { syntax });
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let rest = self.bits(syntax, zeros)?;
        Ok((1_u32 << zeros) - 1 + rest)
    }

    /// Signed Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or overflow.
    pub fn se(&mut self, syntax: &'static str) -> Result<i32, ParseError> {
        let code = self.ue(syntax)?;
        let sign = if code % 2 == 0 { -1 } else { 1 };
        Ok(sign * code.div_ceil(2).cast_signed())
    }

    /// Skip `n` bits.
    ///
    /// # Errors
    ///
    /// Returns `Err` when fewer than `n` bits remain.
    pub fn skip(&mut self, syntax: &'static str, n: u32) -> Result<(), ParseError> {
        let mut left = n;
        while left > 32 {
            self.bits(syntax, 32)?;
            left -= 32;
        }
        self.bits(syntax, left).map(|_| ())
    }
}

/// Raw byte sequence payload with emulation prevention removed.
pub(crate) struct Rbsp {
    /// Payload bytes after `0x03` removal.
    bytes: Vec<u8>,
}

impl Rbsp {
    /// Payload with `0x000003` collapsed to `0x0000`.
    #[must_use]
    pub fn strip(raw: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(raw.len());
        let mut zeros = 0_u32;
        for byte in raw {
            if zeros == 2 && *byte == 0x03 {
                zeros = 0;
                continue;
            }
            zeros = if *byte == 0 { zeros + 1 } else { 0 };
            bytes.push(*byte);
        }
        Self { bytes }
    }

    /// Stripped payload bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, Rbsp};
    use crate::error::ParseError;

    #[test]
    fn bits_read_msb_first() {
        let mut reader = BitReader::new(&[0b1011_0000]);
        assert_eq!(reader.bits("t", 3).expect("bits"), 0b101);
        assert!(reader.bit("t").expect("bit"));
        assert_eq!(reader.consumed(), 4);
    }

    #[test]
    fn bits_reject_overread() {
        let mut reader = BitReader::new(&[0xFF]);
        assert!(reader.bits("t", 9).is_err());
        assert!(reader.bits("t", 33).is_err());
        assert!(reader.more());
        reader.bits("t", 8).expect("byte");
        assert!(!reader.more());
    }

    #[test]
    fn ue_decodes_codes() {
        let mut reader = BitReader::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!(reader.ue("t").expect("0"), 0);
        assert_eq!(reader.ue("t").expect("1"), 1);
        assert_eq!(reader.ue("t").expect("2"), 2);
        assert_eq!(reader.ue("t").expect("3"), 3);
    }

    #[test]
    fn se_maps_sign() {
        let mut reader = BitReader::new(&[0b1010_0110, 0b0100_0010, 0b1000_0000]);
        assert_eq!(reader.se("t").expect("0"), 0);
        assert_eq!(reader.se("t").expect("1"), 1);
        assert_eq!(reader.se("t").expect("-1"), -1);
        assert_eq!(reader.se("t").expect("2"), 2);
        assert_eq!(reader.se("t").expect("-2"), -2);
    }

    #[test]
    fn ue_rejects_overflow() {
        let bytes = vec![0; 8];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(
            reader.ue("t").unwrap_err(),
            ParseError::Overflow { syntax: "t" }
        );
    }

    #[test]
    fn more_rbsp_skips_stop_bit_and_alignment() {
        let mut reader = BitReader::new(&[0b1010_0000]);
        reader.bit("t").expect("flag");
        assert!(reader.more_rbsp());
        reader.bit("t").expect("flag");
        assert!(!reader.more_rbsp());
    }

    #[test]
    fn rbsp_collapses_prevention_bytes() {
        let stripped = Rbsp::strip(&[0, 0, 3, 0, 0, 3, 1, 0, 0, 3, 3]);
        assert_eq!(stripped.bytes(), &[0, 0, 0, 0, 1, 0, 0, 3]);
    }
}
