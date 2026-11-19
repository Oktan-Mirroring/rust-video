//! Bit reader over RBSP bytes.

use core::marker::PhantomData;

use crate::error::{HevcError, ParseKind};

/// Exp-Golomb bit reader.
pub struct BitReader<'a, E: HevcError> {
    /// RBSP bytes under read.
    data: &'a [u8],
    /// Bit offset of the next read.
    bit: usize,
    /// Tier error under construction.
    error: PhantomData<E>,
}

impl<'a, E: HevcError> BitReader<'a, E> {
    /// Reader over `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit: 0,
            error: PhantomData,
        }
    }

    /// Bits consumed so far.
    #[must_use]
    pub fn position(&self) -> usize {
        self.bit
    }

    /// True while payload bits remain past the RBSP stop bit.
    #[must_use]
    pub fn more_data(&self) -> bool {
        let total = self.data.len() * 8;
        if self.bit >= total {
            return false;
        }
        let mut offset = self.bit;
        if self.peek(offset) == 0 {
            return true;
        }
        offset += 1;
        while offset < total {
            if self.peek(offset) != 0 {
                return true;
            }
            offset += 1;
        }
        false
    }

    /// Next `count` bits as an unsigned value.
    ///
    /// # Errors
    ///
    /// Returns `Err` when fewer than `count` bits remain or `count` exceeds 32.
    ///
    /// # Arguments
    ///
    /// * `count` - bit count to read.
    /// * `syntax` - element under parse.
    pub fn read_bits(&mut self, count: u32, syntax: &'static str) -> Result<u32, E> {
        if count > 32 {
            return Err(E::parse(ParseKind::TooWide { syntax }));
        }
        let mut value: u32 = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit(syntax)?);
        }
        Ok(value)
    }

    /// Single bit.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no bits remain.
    ///
    /// # Arguments
    ///
    /// * `syntax` - element under parse.
    pub fn read_bit(&mut self, syntax: &'static str) -> Result<bool, E> {
        let total = self.data.len() * 8;
        if self.bit >= total {
            return Err(E::parse(ParseKind::Truncated { syntax }));
        }
        let bit = self.peek(self.bit);
        self.bit += 1;
        Ok(bit != 0)
    }

    /// Unsigned Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a code exceeding 32 bits.
    ///
    /// # Arguments
    ///
    /// * `syntax` - element under parse.
    pub fn read_ue(&mut self, syntax: &'static str) -> Result<u32, E> {
        let mut leading = 0u32;
        while !self.read_bit(syntax)? {
            leading += 1;
            if leading > 31 {
                return Err(E::parse(ParseKind::OverflowU32 { syntax }));
            }
        }
        if leading == 0 {
            return Ok(0);
        }
        let rest = self.read_bits(leading, syntax)?;
        rest.checked_add((1u32 << leading) - 1)
            .ok_or(E::parse(ParseKind::OverflowU32 { syntax }))
    }

    /// Signed Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a value exceeding `i32`.
    ///
    /// # Arguments
    ///
    /// * `syntax` - element under parse.
    pub fn read_se(&mut self, syntax: &'static str) -> Result<i32, E> {
        let code = self.read_ue(syntax)?;
        let value =
            i32::try_from(code >> 1).map_err(|_| E::parse(ParseKind::OverflowI32 { syntax }))?;
        if code & 1 == 0 {
            Ok(-value)
        } else {
            value
                .checked_add(1)
                .ok_or(E::parse(ParseKind::OverflowI32 { syntax }))
        }
    }

    /// Bit at `offset` without consuming it.
    fn peek(&self, offset: usize) -> u8 {
        let byte = self.data.get(offset / 8).copied().unwrap_or(0);
        (byte >> (7 - offset % 8)) & 1
    }
}

#[cfg(test)]
mod tests {
    use crate::error::test_support::TestError;

    use super::BitReader;

    #[test]
    fn reads_bits_across_bytes() {
        let mut reader = BitReader::<TestError>::new(&[0b1011_0110, 0b0100_0000]);
        assert!(reader.read_bit("flag").expect("bit"));
        assert_eq!(reader.read_bits(3, "field").expect("bits"), 0b011);
        assert_eq!(reader.read_bits(8, "field").expect("bits"), 0b0110_0100);
        assert!(reader.more_data());
    }

    #[test]
    fn reads_exp_golomb_pair() {
        let mut reader = BitReader::<TestError>::new(&[0x10, 0x50]);
        assert_eq!(reader.read_ue("ue").expect("ue"), 7);
        assert_eq!(reader.read_se("se").expect("se"), -2);
    }

    #[test]
    fn more_data_skips_stop_bit() {
        let aligned = BitReader::<TestError>::new(&[0x80]);
        assert!(!aligned.more_data());
        let payload = BitReader::<TestError>::new(&[0x40]);
        assert!(payload.more_data());
        let empty = BitReader::<TestError>::new(&[]);
        assert!(!empty.more_data());
    }

    #[test]
    fn truncated_reads_fail() {
        let mut reader = BitReader::<TestError>::new(&[0xFF]);
        assert!(reader.read_bits(9, "field").is_err());
        let mut reader = BitReader::<TestError>::new(&[0x00]);
        assert!(reader.read_ue("ue").is_err());
    }
}
