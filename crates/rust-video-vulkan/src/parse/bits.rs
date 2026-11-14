//! Exp-Golomb bit reader over RBSP bytes.

use std::borrow::Cow;

use crate::error::ParseError;

use memchr::memchr_iter;

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
        let mut remaining = n as usize;
        while remaining > 0 {
            let byte = self.bytes[self.bit / 8];
            let offset = self.bit % 8;
            let take = (8 - offset).min(remaining);
            let shift = 8 - offset - take;
            let chunk = u32::from(byte >> shift) & ((1_u32 << take) - 1);
            value = (value << take) | chunk;
            self.bit += take;
            remaining -= take;
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

    /// Unsigned variable-length code: zeros, a one, then the value bits.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation, or when the code exceeds 32 bits.
    pub fn uvlc(&mut self, syntax: &'static str) -> Result<u32, ParseError> {
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
        Ok(rest + (1_u32 << zeros) - 1)
    }

    /// Signed `n`-bit two's-complement value.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a zero width.
    pub fn su(&mut self, syntax: &'static str, n: u32) -> Result<i32, ParseError> {
        if n == 0 || n > 32 {
            return Err(ParseError::OutOfRange { syntax });
        }
        let raw = self.bits(syntax, n)?;
        Ok((raw << (32 - n)).cast_signed() >> (32 - n))
    }

    /// Non-symmetric value over `n` symbols.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or an empty symbol count.
    pub fn ns(&mut self, syntax: &'static str, n: u32) -> Result<u32, ParseError> {
        if n == 0 || n >= (1 << 31) {
            return Err(ParseError::OutOfRange { syntax });
        }
        let w = 32 - n.leading_zeros();
        let m = (1_u32 << w) - n;
        let v = if w > 1 { self.bits(syntax, w - 1)? } else { 0 };
        if v < m {
            return Ok(v);
        }
        let extra = self.bits(syntax, 1)?;
        Ok((v << 1) - m + extra)
    }

    /// Increment code between `min` and `max`: one bits up, zero stops.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or an inverted range.
    pub fn increment(
        &mut self,
        syntax: &'static str,
        min: u32,
        max: u32,
    ) -> Result<u32, ParseError> {
        if min > max {
            return Err(ParseError::OutOfRange { syntax });
        }
        let mut value = min;
        while value < max {
            if !self.bit(syntax)? {
                break;
            }
            value += 1;
        }
        Ok(value)
    }

    /// Sub-exponential code under `range_max`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a degenerate range.
    pub fn subexp(&mut self, syntax: &'static str, range_max: u32) -> Result<u32, ParseError> {
        if range_max < 9 {
            return Err(ParseError::OutOfRange { syntax });
        }
        let max_len = 32 - (range_max - 1).leading_zeros() - 1 - 3;
        let len = self.increment(syntax, 0, max_len)?;
        let (range_bits, range_offset) = if len == 0 {
            (3, 0)
        } else {
            (2 + len, 1 << (2 + len))
        };
        let value = if len < max_len {
            self.bits(syntax, range_bits)?
        } else {
            self.ns(syntax, range_max - range_offset)?
        };
        Ok(value + range_offset)
    }
}

/// Raw byte sequence payload with emulation prevention removed.
pub(crate) struct Rbsp<'a> {
    /// Payload bytes after `0x03` removal, borrowed when clean.
    bytes: Cow<'a, [u8]>,
}

impl<'a> Rbsp<'a> {
    /// Payload with `0x000003` collapsed to `0x0000`.
    ///
    /// Borrows `raw` when it holds no prevention pattern.
    #[must_use]
    pub fn strip(raw: &'a [u8]) -> Self {
        let mut hits = memchr_iter(0x03, raw).filter(|index| Self::is_prevention(raw, *index));
        let Some(first) = hits.next() else {
            return Self {
                bytes: Cow::Borrowed(raw),
            };
        };
        let mut bytes = Vec::with_capacity(raw.len());
        let mut start = 0;
        let mut hit = first;
        loop {
            bytes.extend_from_slice(&raw[start..hit]);
            start = hit + 1;
            match hits.next() {
                Some(next) => hit = next,
                None => break,
            }
        }
        bytes.extend_from_slice(&raw[start..]);
        Self {
            bytes: Cow::Owned(bytes),
        }
    }

    /// True when `raw[index]` is a prevention byte after two zeros.
    fn is_prevention(raw: &[u8], index: usize) -> bool {
        index >= 2
            && raw[index - 2] == 0
            && raw[index - 1] == 0
            && (index < 3 || raw[index - 3] != 0)
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

    #[test]
    fn uvlc_decodes_values() {
        let mut reader = BitReader::new(&[0b1010_0110, 0b0011_1000]);
        assert_eq!(reader.uvlc("t").expect("0"), 0);
        assert_eq!(reader.uvlc("t").expect("1"), 1);
        assert_eq!(reader.uvlc("t").expect("2"), 2);
        assert_eq!(reader.uvlc("t").expect("13"), 13);
    }

    #[test]
    fn su_decodes_twos_complement() {
        let mut reader = BitReader::new(&[0b1010_0101]);
        assert_eq!(reader.su("t", 4).expect("neg"), -6);
        assert_eq!(reader.su("t", 4).expect("pos"), 5);
        let mut reader = BitReader::new(&[0b1110_0010]);
        assert_eq!(reader.su("t", 7).expect("q"), -15);
    }

    #[test]
    fn ns_covers_short_and_long_forms() {
        let mut reader = BitReader::new(&[0b0111_1000]);
        assert_eq!(reader.ns("t", 5).expect("short"), 1);
        assert_eq!(reader.ns("t", 5).expect("long"), 4);
    }

    #[test]
    fn increment_counts_ones() {
        let mut reader = BitReader::new(&[0b1100_0000]);
        assert_eq!(reader.increment("t", 1, 4).expect("mid"), 3);
        let mut reader = BitReader::new(&[0b1110_0000]);
        assert_eq!(reader.increment("t", 1, 4).expect("max"), 4);
    }

    #[test]
    fn subexp_decodes_bands() {
        let mut reader = BitReader::new(&[0b0101_0000]);
        assert_eq!(reader.subexp("t", 513).expect("low"), 5);
    }
}
