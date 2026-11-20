//! MSB-first bit reader over frame bytes.

use crate::Error;

/// Bit reader over frame bytes.
pub struct BitReader<'a> {
    /// Frame bytes under read.
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

    /// True while frame bits remain.
    #[must_use]
    pub fn more_data(&self) -> bool {
        self.bit < self.data.len() * 8
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
            return Err(E::from(Error::TooWide { syntax }));
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
            return Err(E::from(Error::Truncated { syntax }));
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
                return Err(E::from(Error::TooWide { syntax }));
            }
        }
        if leading == 0 {
            return Ok(0);
        }
        let rest = self.read_bits(leading, syntax)?;
        rest.checked_add((1u32 << leading) - 1)
            .ok_or_else(|| E::from(Error::TooWide { syntax }))
    }

    /// Signed Exp-Golomb code.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncation or a value outside `i32`.
    pub fn read_se<E: From<Error>>(&mut self, syntax: &'static str) -> Result<i32, E> {
        let code = self.read_ue(syntax)?;
        let value = i32::try_from(code >> 1).map_err(|_| E::from(Error::Range { syntax }))?;
        if code & 1 == 0 {
            Ok(-value)
        } else {
            value
                .checked_add(1)
                .ok_or_else(|| E::from(Error::Range { syntax }))
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
    use crate::Error;

    #[test]
    fn more_data_tracks_remaining_bits() {
        assert!(!BitReader::new(&[]).more_data());
        let mut reader = BitReader::new(&[0x80]);
        assert!(reader.more_data());
        reader.read_bits::<Error>(8, "byte").expect("byte");
        assert!(!reader.more_data());
    }

    #[test]
    fn read_ue_decodes_codes() {
        let mut reader = BitReader::new(&[0xA6, 0x42, 0x80]);
        assert_eq!(reader.read_ue::<Error>("ue"), Ok(0));
        assert_eq!(reader.read_ue::<Error>("ue"), Ok(1));
        assert_eq!(reader.read_ue::<Error>("ue"), Ok(2));
        assert_eq!(reader.read_ue::<Error>("ue"), Ok(3));
        assert_eq!(reader.read_ue::<Error>("ue"), Ok(4));
    }

    #[test]
    fn read_se_maps_codes_to_signed() {
        let mut reader = BitReader::new(&[0xA6, 0x42, 0x80]);
        assert_eq!(reader.read_se::<Error>("se"), Ok(0));
        assert_eq!(reader.read_se::<Error>("se"), Ok(1));
        assert_eq!(reader.read_se::<Error>("se"), Ok(-1));
        assert_eq!(reader.read_se::<Error>("se"), Ok(2));
        assert_eq!(reader.read_se::<Error>("se"), Ok(-2));
    }

    #[test]
    fn read_ue_rejects_truncation_and_overflow() {
        let mut reader = BitReader::new(&[]);
        assert_eq!(
            reader.read_ue::<Error>("ue"),
            Err(Error::Truncated { syntax: "ue" })
        );
        let mut reader = BitReader::new(&[0, 0, 0, 0, 0x80]);
        assert_eq!(
            reader.read_ue::<Error>("ue"),
            Err(Error::TooWide { syntax: "ue" })
        );
    }
}
