//! Bit reader over RBSP bytes.

use crate::error::D3d12Error;

/// Exp-Golomb bit reader.
pub(crate) struct BitReader<'a> {
    /// RBSP bytes under read.
    data: &'a [u8],
    /// Bit offset of the next read.
    bit: usize,
}

impl<'a> BitReader<'a> {
    /// Reader over `data`.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    /// Bits consumed so far.
    pub(crate) fn position(&self) -> usize {
        self.bit
    }

    /// True while payload bits remain past the RBSP stop bit.
    pub(crate) fn more_data(&self) -> bool {
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
    pub(crate) fn read_bits(
        &mut self,
        count: u32,
        syntax: &'static str,
    ) -> Result<u32, D3d12Error> {
        if count > 32 {
            return Err(D3d12Error::Parse(format!("{syntax} wider than 32 bits")));
        }
        let mut value: u32 = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit(syntax)?);
        }
        Ok(value)
    }

    /// Single bit.
    pub(crate) fn read_bit(&mut self, syntax: &'static str) -> Result<bool, D3d12Error> {
        let total = self.data.len() * 8;
        if self.bit >= total {
            return Err(D3d12Error::Parse(format!("truncated {syntax}")));
        }
        let bit = self.peek(self.bit);
        self.bit += 1;
        Ok(bit != 0)
    }

    /// Unsigned Exp-Golomb code.
    pub(crate) fn read_ue(&mut self, syntax: &'static str) -> Result<u32, D3d12Error> {
        let mut leading = 0u32;
        while !self.read_bit(syntax)? {
            leading += 1;
            if leading > 31 {
                return Err(D3d12Error::Parse(format!("{syntax} overflows u32")));
            }
        }
        if leading == 0 {
            return Ok(0);
        }
        let rest = self.read_bits(leading, syntax)?;
        rest.checked_add((1u32 << leading) - 1)
            .ok_or_else(|| D3d12Error::Parse(format!("{syntax} overflows u32")))
    }

    /// Signed Exp-Golomb code.
    pub(crate) fn read_se(&mut self, syntax: &'static str) -> Result<i32, D3d12Error> {
        let code = self.read_ue(syntax)?;
        let value = i32::try_from(code >> 1)
            .map_err(|_| D3d12Error::Parse(format!("{syntax} overflows i32")))?;
        if code & 1 == 0 {
            Ok(-value)
        } else {
            value
                .checked_add(1)
                .ok_or_else(|| D3d12Error::Parse(format!("{syntax} overflows i32")))
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

    #[test]
    fn reads_bits_across_bytes() {
        let mut reader = BitReader::new(&[0b1011_0110, 0b0100_0000]);
        assert!(reader.read_bit("flag").expect("bit"));
        assert_eq!(reader.read_bits(3, "field").expect("bits"), 0b011);
        assert_eq!(reader.read_bits(8, "field").expect("bits"), 0b0110_0100);
        assert!(reader.more_data());
    }

    #[test]
    fn reads_exp_golomb_pair() {
        let mut reader = BitReader::new(&[0x10, 0x50]);
        assert_eq!(reader.read_ue("ue").expect("ue"), 7);
        assert_eq!(reader.read_se("se").expect("se"), -2);
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
        assert!(reader.read_bits(9, "field").is_err());
        let mut reader = BitReader::new(&[0x00]);
        assert!(reader.read_ue("ue").is_err());
    }
}
