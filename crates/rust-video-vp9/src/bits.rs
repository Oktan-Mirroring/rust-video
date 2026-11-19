//! MSB-first bit reader over frame bytes.

use crate::Fault;

/// Bit reader over frame bytes.
pub(crate) struct BitReader<'a> {
    /// Frame bytes under read.
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

    /// Next `count` bits as an unsigned value.
    pub(crate) fn read_bits<E: From<Fault>>(
        &mut self,
        count: u32,
        syntax: &'static str,
    ) -> Result<u32, E> {
        if count > 32 {
            return Err(E::from(Fault::TooWide { syntax }));
        }
        let mut value: u32 = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit(syntax)?);
        }
        Ok(value)
    }

    /// Single bit.
    pub(crate) fn read_bit<E: From<Fault>>(&mut self, syntax: &'static str) -> Result<bool, E> {
        let total = self.data.len() * 8;
        if self.bit >= total {
            return Err(E::from(Fault::Truncated { syntax }));
        }
        let bit = self.peek(self.bit);
        self.bit += 1;
        Ok(bit != 0)
    }

    /// Bit at `offset` without consuming it.
    fn peek(&self, offset: usize) -> u8 {
        let byte = self.data.get(offset / 8).copied().unwrap_or(0);
        (byte >> (7 - offset % 8)) & 1
    }
}
