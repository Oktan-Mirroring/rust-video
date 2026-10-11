// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A VP8 boolean decoder based on the implementation in Chromium and `GStreamer`.

use std::convert::TryFrom;
use std::fmt;

use crate::bitstream::BitReader;

const LOTS_OF_BITS: u32 = 0x4000_0000;
const U8_BITS: usize = u8::BITS as usize;
const BD_VALUE_SIZE: usize = std::mem::size_of::<usize>() * U8_BITS;

const NORM: [u8; 256] = [
    0, 7, 6, 6, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Some bits are "encoded" with a 50/50 probability.
const DEFAULT_PROBABILITY: u8 = 128;

/// A capture of the state of the boolean decoder.
///
/// A `BoolDecoder` can be consumed and turned into this structure, which captures its state. This
/// is useful to pass that state to the next decoding step, typically a hardware accelerator.
pub struct BoolDecoderState {
    pub range: usize,
    pub value: usize,
    pub count: isize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BoolDecoderError {
    EndOfInput,
    CannotConvert,
}

impl fmt::Display for BoolDecoderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            BoolDecoderError::EndOfInput => write!(f, "end of input reached"),
            BoolDecoderError::CannotConvert => {
                write!(f, "could not convert number of read bits to target type")
            }
        }
    }
}

pub type BoolDecoderResult<T> = std::result::Result<T, BoolDecoderError>;

/// The decoder state.
pub struct BoolDecoder<'a> {
    data: BitReader<'a>,
    range: usize,
    value: usize,
    count: isize,
}

impl<'a> BoolDecoder<'a> {
    /// Creates a new instance.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data: BitReader::new(data, false),
            range: 255usize,
            value: 0usize,
            count: -U8_BITS.cast_signed(),
        }
    }

    /// Fills more bits from `data` to `value`. We shall keep at least 8 bits of the current `data`
    /// in `value`.
    ///
    /// Returns `Some(())` if there was input data to fill from, `None` if we reached the end of
    /// the input.
    fn fill(&mut self) -> Option<()> {
        let mut shift = i32::try_from(
            BD_VALUE_SIZE.cast_signed()
                - U8_BITS.cast_signed()
                - (self.count + U8_BITS.cast_signed()),
        )
        .ok()?;

        let bits_left = i32::try_from(self.data.num_bits_left()).ok()?;
        let x = shift + i32::try_from(U8_BITS).ok()? - bits_left;
        let mut loop_end = 0;

        if x >= 0 {
            self.count += isize::try_from(LOTS_OF_BITS).ok()?;
            loop_end = x;
        }

        if x < 0 || bits_left != 0 {
            while shift >= loop_end {
                self.count += U8_BITS.cast_signed();
                self.value |= self.data.read_bits::<usize>(8).ok()? << shift;
                shift -= i32::try_from(U8_BITS).ok()?;
            }
            Some(())
        } else {
            None
        }
    }

    /// Reads the next bit from the coded stream. The probability of the bit to
    /// be one is probability / 256.
    fn read_bit(&mut self, probability: u8) -> BoolDecoderResult<bool> {
        let split = 1 + (((self.range - 1) * probability as usize) >> 8);

        if self.count < 0 {
            self.fill().ok_or(BoolDecoderError::EndOfInput)?;
        }

        let bigsplit = split << (BD_VALUE_SIZE - U8_BITS);

        let bit = if self.value >= bigsplit {
            self.range -= split;
            self.value -= bigsplit;
            true
        } else {
            self.range = split;
            false
        };

        let shift = NORM[self.range];
        self.range <<= shift;
        self.value <<= shift;
        self.count -= isize::from(shift);

        Ok(bit)
    }

    /// Reads a "literal", that is, a "num_bits"-wide unsigned value whose bits
    /// come high- to low-order, with each bit encoded at probability 1/2.
    ///
    /// # Panics
    ///
    /// Will panic if `nbits > 31`.
    fn read_literal(&mut self, nbits: usize) -> BoolDecoderResult<i32> {
        // This won't perform well if we read more than 31 bits.
        assert!(nbits <= 31);

        let mut ret = 0;

        for _ in 0..nbits {
            ret = (ret << 1) | i32::from(self.read_bit(DEFAULT_PROBABILITY)?);
        }

        Ok(ret)
    }

    /// Reads a boolean from the coded stream. Returns false if it has reached the
    /// end of data and failed to read the boolean. The probability of out to
    /// be true is probability / 256, e.g., when probability is 0x80, the
    /// chance is 1/2 (i.e., 0x80 / 256).
    pub fn read_bool(&mut self) -> BoolDecoderResult<bool> {
        self.read_literal(1).map(|bit| bit != 0)
    }

    /// Reads a boolean from the coded stream. Returns false if it has reached the
    /// end of data and failed to read the boolean. The probability of out to
    /// be true is probability / 256, e.g., when probability is 0x80, the
    /// chance is 1/2 (i.e., 0x80 / 256).
    pub fn read_bool_with_prob(&mut self, probability: u8) -> BoolDecoderResult<bool> {
        self.read_bit(probability)
    }

    /// Reads an unsigned literal from the coded stream.
    ///
    /// # Panics
    ///
    /// Will panic if `nbits > 31`.
    pub fn read_uint<U: TryFrom<i32>>(&mut self, nbits: usize) -> BoolDecoderResult<U> {
        let value = self.read_literal(nbits)?;
        U::try_from(value).map_err(|_| BoolDecoderError::CannotConvert)
    }

    /// Reads a literal with sign from the coded stream. This is similar to the
    /// `read_literal()`, it first read a "num_bits"-wide unsigned value, and then
    /// read an extra bit as the sign of the literal.
    ///
    /// # Panics
    ///
    /// Will panic if `nbits > 31`.
    pub fn read_sint<U: TryFrom<i32>>(&mut self, nbits: usize) -> BoolDecoderResult<U> {
        let mut value = self.read_literal(nbits)?;
        let sign = self.read_bool()?;

        if sign {
            value = -value;
        }

        U::try_from(value).map_err(|_| BoolDecoderError::CannotConvert)
    }

    /// Returns the current bit position.
    pub fn pos(&self) -> usize {
        let mut bit_count = usize::try_from(self.count + 8).unwrap_or(0);

        if bit_count > BD_VALUE_SIZE {
            bit_count = bit_count.saturating_sub(LOTS_OF_BITS as usize);
        }

        let pos = usize::try_from(self.data.position()).unwrap_or(usize::MAX);
        pos - bit_count
    }
}

impl From<BoolDecoder<'_>> for BoolDecoderState {
    fn from(mut bd: BoolDecoder) -> Self {
        if bd.count < 0 {
            let _ = bd.fill();
        }

        Self {
            value: bd.value >> (BD_VALUE_SIZE - U8_BITS),
            count: (U8_BITS.cast_signed() + bd.count) % U8_BITS.cast_signed(),
            range: bd.range,
        }
    }
}
