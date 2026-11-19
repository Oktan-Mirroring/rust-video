//! Bit reader over AV1 payload bytes.

use crate::error::{Error, ParseReason};

/// AV1 bit reader.
pub struct BitReader<'a> {
    /// Payload bytes under read.
    data: &'a [u8],
    /// Bit offset of the next read.
    bit: usize,
}

impl<'a> BitReader<'a> {
    /// Reader over `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    /// Bits consumed so far.
    #[must_use]
    pub fn position(&self) -> usize {
        self.bit
    }

    /// True while payload bits remain past the stop bit.
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
    /// Returns `Err` when `count` exceeds 32 or fewer bits remain.
    pub fn read_bits<E: From<Error>>(
        &mut self,
        count: u32,
        syntax: &'static str,
    ) -> Result<u32, E> {
        if count > 32 {
            return Err(Error::parse(ParseReason::TooWide { syntax }));
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
    pub fn read_bit<E: From<Error>>(&mut self, syntax: &'static str) -> Result<bool, E> {
        let total = self.data.len() * 8;
        if self.bit >= total {
            return Err(Error::parse(ParseReason::Truncated { syntax }));
        }
        let bit = self.peek(self.bit);
        self.bit += 1;
        Ok(bit != 0)
    }

    /// Unsigned Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a code above 32 bits.
    pub fn read_ue<E: From<Error>>(&mut self, syntax: &'static str) -> Result<u32, E> {
        let mut leading = 0u32;
        while !self.read_bit(syntax)? {
            leading += 1;
            if leading > 31 {
                return Err(Error::parse(ParseReason::OverflowU32 { syntax }));
            }
        }
        if leading == 0 {
            return Ok(0);
        }
        let rest = self.read_bits(leading, syntax)?;
        rest.checked_add((1u32 << leading) - 1)
            .ok_or_else(|| Error::parse(ParseReason::OverflowU32 { syntax }))
    }

    /// Signed Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a value outside `i32`.
    pub fn read_se<E: From<Error>>(&mut self, syntax: &'static str) -> Result<i32, E> {
        let code = self.read_ue(syntax)?;
        let value = i32::try_from(code >> 1)
            .map_err(|_| Error::parse(ParseReason::OverflowI32 { syntax }))?;
        if code & 1 == 0 {
            Ok(-value)
        } else {
            value
                .checked_add(1)
                .ok_or_else(|| Error::parse(ParseReason::OverflowI32 { syntax }))
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
    use super::BitReader;
    use crate::error::Error;

    #[test]
    fn reads_bits_across_bytes() {
        let mut reader = BitReader::new(&[0b1011_0110, 0b0100_0000]);
        assert!(reader.read_bit::<Error>("flag").expect("bit"));
        assert_eq!(reader.read_bits::<Error>(3, "field").expect("bits"), 0b011);
        assert_eq!(
            reader.read_bits::<Error>(8, "field").expect("bits"),
            0b0110_0100
        );
        assert!(reader.more_data());
    }

    #[test]
    fn reads_exp_golomb_pair() {
        let mut reader = BitReader::new(&[0x10, 0x50]);
        assert_eq!(reader.read_ue::<Error>("ue").expect("ue"), 7);
        assert_eq!(reader.read_se::<Error>("se").expect("se"), -2);
    }

    #[test]
    fn more_data_skips_stop_bit() {
        let aligned = BitReader::new(&[0x80]);
        assert!(!aligned.more_data());
        let payload = BitReader::new(&[0x40]);
        assert!(payload.more_data());
        let empty = BitReader::new(&[]);
        assert!(!empty.more_data());
    }

    #[test]
    fn truncated_reads_fail() {
        let mut reader = BitReader::new(&[0xFF]);
        assert!(reader.read_bits::<Error>(9, "field").is_err());
        let mut reader = BitReader::new(&[0x00]);
        assert!(reader.read_ue::<Error>("ue").is_err());
    }
}
