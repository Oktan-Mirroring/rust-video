// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Cursor;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;

/// Bit-level parse failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum BitstreamError {
    /// More than 31 bits requested at once.
    #[error("more than 31 ({0}) bits were requested")]
    TooManyBits(usize),
    /// No bits left in the stream.
    #[error("reader ran out of bits")]
    OutOfBits,
    /// Raw bits miss the target type.
    #[error("failed to convert read input to target type")]
    BitsConversion,
    /// Exponential-Golomb leading-zero run over 31.
    #[error("invalid stream")]
    InvalidCode,
    /// Exponential-Golomb value overflows `u32`.
    #[error("read number cannot fit in 32 bits")]
    CodeOverflow,
    /// Exponential-Golomb value misses the target type.
    #[error("conversion error")]
    CodeConversion,
    /// Unsigned Exponential-Golomb value outside its bound.
    #[error("Value out of bounds: expected {min} - {max}, got {value}")]
    UeRange {
        /// Lower bound, inclusive.
        min: u32,
        /// Upper bound, inclusive.
        max: u32,
        /// Parsed value.
        value: u32,
    },
    /// Bounded value misses the target type.
    #[error("Conversion error")]
    BoundedConversion,
    /// Signed Exponential-Golomb value outside its bound.
    #[error("Value out of bounds, expected between {min}-{max}, got {value}")]
    SeRange {
        /// Lower bound, inclusive.
        min: i32,
        /// Upper bound, inclusive.
        max: i32,
        /// Parsed value.
        value: i32,
    },
}

impl From<BitstreamError> for String {
    fn from(error: BitstreamError) -> Self {
        error.to_string()
    }
}

/// A bit reader for codec bitstreams. It properly handles emulation-prevention
/// bytes and stop bits for H264.
#[derive(Clone)]
pub(crate) struct BitReader<'a> {
    /// A reference into the next unread byte in the stream.
    data: Cursor<&'a [u8]>,
    /// Contents of the current byte. First unread bit starting at position 8 -
    /// `num_remaining_bits_in_curr_bytes`.
    curr_byte: u8,
    /// Number of bits remaining in `curr_byte`
    num_remaining_bits_in_curr_byte: usize,
    /// Used in emulation prevention byte detection.
    prev_two_bytes: u16,
    /// Number of emulation prevention bytes (i.e. 0x000003) we found.
    num_epb: usize,
    /// Whether or not we need emulation prevention logic.
    needs_epb: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8], needs_epb: bool) -> Self {
        Self {
            data: Cursor::new(data),
            curr_byte: Default::default(),
            num_remaining_bits_in_curr_byte: Default::default(),
            prev_two_bytes: 0xffff,
            num_epb: Default::default(),
            needs_epb,
        }
    }

    /// Read a single bit from the stream.
    pub fn read_bit(&mut self) -> Result<bool, BitstreamError> {
        Ok(self.read_bits::<u32>(1)? == 1)
    }

    /// Read up to 31 bits from the stream. Note that we don't want to read 32
    /// bits even though we're returning a u32 because that would break the
    /// `read_bits_signed()` function. 31 bits should be overkill for compressed
    /// header parsing anyway.
    pub fn read_bits<U: TryFrom<u32>>(&mut self, num_bits: usize) -> Result<U, BitstreamError> {
        if num_bits > 31 {
            return Err(BitstreamError::TooManyBits(num_bits));
        }

        let mut bits_left = num_bits;
        let mut out = 0u32;

        while self.num_remaining_bits_in_curr_byte < bits_left {
            out |= u32::from(self.curr_byte) << (bits_left - self.num_remaining_bits_in_curr_byte);
            bits_left -= self.num_remaining_bits_in_curr_byte;
            self.move_to_next_byte()?;
        }

        out |= u32::from(self.curr_byte >> (self.num_remaining_bits_in_curr_byte - bits_left));
        out &= (1 << num_bits) - 1;
        self.num_remaining_bits_in_curr_byte -= bits_left;

        U::try_from(out).map_err(|_| BitstreamError::BitsConversion)
    }

    /// Skip `num_bits` bits from the stream.
    pub fn skip_bits(&mut self, mut num_bits: usize) -> Result<(), BitstreamError> {
        while num_bits > 0 {
            let n = std::cmp::min(num_bits, 31);
            self.read_bits::<u32>(n)?;
            num_bits -= n;
        }

        Ok(())
    }

    /// Returns the amount of bits left in the stream
    pub fn num_bits_left(&mut self) -> usize {
        let cur_pos = self.data.position();
        // This should always be safe to unwrap.
        let end_pos = self.data.seek(SeekFrom::End(0)).unwrap();
        let _ = self.data.seek(SeekFrom::Start(cur_pos));
        (usize::try_from(end_pos - cur_pos).unwrap_or(usize::MAX)) * 8
            + self.num_remaining_bits_in_curr_byte
    }

    /// Returns the number of emulation-prevention bytes read so far.
    pub fn num_epb(&self) -> usize {
        self.num_epb
    }

    /// Whether the stream still has RBSP data. Implements `more_rbsp_data()`. See
    /// the spec for more details.
    pub fn has_more_rsbp_data(&mut self) -> bool {
        if self.num_remaining_bits_in_curr_byte == 0 && self.move_to_next_byte().is_err() {
            // no more data at all in the rbsp
            return false;
        }

        // If the next bit is the stop bit, then we should only see unset bits
        // until the end of the data.
        if (self.curr_byte & ((1 << (self.num_remaining_bits_in_curr_byte - 1)) - 1)) != 0 {
            return true;
        }

        let mut buf = [0u8; 1];
        let orig_pos = self.data.position();
        while let Ok(()) = self.data.read_exact(&mut buf) {
            if buf[0] != 0 {
                self.data.set_position(orig_pos);
                return true;
            }
        }
        false
    }

    /// Reads an Unsigned Exponential golomb coding number from the next bytes in the
    /// bitstream. This may advance the state of position within the bitstream even if the
    /// read operation is unsuccessful. See H264 Annex B specification 9.1 for details.
    pub fn read_ue<U: TryFrom<u32>>(&mut self) -> Result<U, BitstreamError> {
        let mut num_bits = 0;

        while self.read_bits::<u32>(1)? == 0 {
            num_bits += 1;
            if num_bits > 31 {
                return Err(BitstreamError::InvalidCode);
            }
        }

        let value = ((1u32 << num_bits) - 1)
            .checked_add(self.read_bits::<u32>(num_bits)?)
            .ok_or(BitstreamError::CodeOverflow)?;

        U::try_from(value).map_err(|_| BitstreamError::CodeConversion)
    }

    pub fn read_ue_bounded<U: TryFrom<u32>>(
        &mut self,
        min: u32,
        max: u32,
    ) -> Result<U, BitstreamError> {
        let ue: u32 = self.read_ue()?;
        if ue > max || ue < min {
            Err(BitstreamError::UeRange {
                min,
                max,
                value: ue,
            })
        } else {
            Ok(U::try_from(ue).map_err(|_| BitstreamError::BoundedConversion)?)
        }
    }

    pub fn read_ue_max<U: TryFrom<u32>>(&mut self, max: u32) -> Result<U, BitstreamError> {
        self.read_ue_bounded(0, max)
    }

    /// Reads a signed exponential golomb coding number. Instead of using two's
    /// complement, this scheme maps even integers to positive numbers and odd
    /// integers to negative numbers. The least significant bit indicates the
    /// sign. See H264 Annex B specification 9.1.1 for details.
    pub fn read_se<U: TryFrom<i32>>(&mut self) -> Result<U, BitstreamError> {
        let ue = self.read_ue::<u32>()?.cast_signed();

        if ue % 2 == 0 {
            Ok(U::try_from(-(ue / 2)).map_err(|_| BitstreamError::BoundedConversion)?)
        } else {
            Ok(U::try_from(ue / 2 + 1).map_err(|_| BitstreamError::BoundedConversion)?)
        }
    }

    pub fn read_se_bounded<U: TryFrom<i32>>(
        &mut self,
        min: i32,
        max: i32,
    ) -> Result<U, BitstreamError> {
        let se = self.read_se()?;
        if se < min || se > max {
            Err(BitstreamError::SeRange {
                min,
                max,
                value: se,
            })
        } else {
            Ok(U::try_from(se).map_err(|_| BitstreamError::BoundedConversion)?)
        }
    }

    fn get_byte(&mut self) -> Result<u8, BitstreamError> {
        let mut buf = [0u8; 1];
        self.data
            .read_exact(&mut buf)
            .map_err(|_| BitstreamError::OutOfBits)?;
        Ok(buf[0])
    }

    fn move_to_next_byte(&mut self) -> Result<(), BitstreamError> {
        let mut byte = self.get_byte()?;

        if self.needs_epb {
            if self.prev_two_bytes == 0 && byte == 0x03 {
                // We found an epb
                self.num_epb += 1;
                // Read another byte
                byte = self.get_byte()?;
                // We need another 3 bytes before another epb can happen.
                self.prev_two_bytes = 0xffff;
            }
            self.prev_two_bytes = (self.prev_two_bytes << 8) | u16::from(byte);
        }

        self.num_remaining_bits_in_curr_byte = 8;
        self.curr_byte = byte;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, BitstreamError};

    #[test]
    fn empty_input_reads_fail() {
        let mut reader = BitReader::new(&[], false);
        assert_eq!(reader.read_bit(), Err(BitstreamError::OutOfBits));
        let mut reader = BitReader::new(&[], false);
        assert_eq!(reader.read_bits::<u32>(8), Err(BitstreamError::OutOfBits));
        let mut reader = BitReader::new(&[], true);
        assert_eq!(reader.read_ue::<u32>(), Err(BitstreamError::OutOfBits));
        let mut reader = BitReader::new(&[], true);
        assert_eq!(reader.read_se::<i32>(), Err(BitstreamError::OutOfBits));
    }

    #[test]
    fn overlong_request_rejected() {
        let mut reader = BitReader::new(&[0xFF; 4], false);
        assert_eq!(
            reader.read_bits::<u32>(32),
            Err(BitstreamError::TooManyBits(32))
        );
    }

    #[test]
    fn truncated_exp_golomb_fails() {
        let mut reader = BitReader::new(&[0x00], false);
        assert_eq!(reader.read_ue::<u32>(), Err(BitstreamError::OutOfBits));
    }

    #[test]
    fn overlong_exp_golomb_prefix_rejected() {
        let mut reader = BitReader::new(&[0x00; 4], false);
        assert_eq!(reader.read_ue::<u32>(), Err(BitstreamError::InvalidCode));
    }

    #[test]
    fn narrow_target_rejected() {
        let mut reader = BitReader::new(&[0xFF, 0xFF], false);
        assert_eq!(
            reader.read_bits::<u8>(9),
            Err(BitstreamError::BitsConversion)
        );
    }

    #[test]
    fn out_of_range_bounded_reads_fail() {
        let mut reader = BitReader::new(&[0b0011_0000], false);
        assert_eq!(
            reader.read_ue_bounded::<u32>(0, 4),
            Err(BitstreamError::UeRange {
                min: 0,
                max: 4,
                value: 5
            })
        );
        let mut reader = BitReader::new(&[0b0010_1000], false);
        assert_eq!(
            reader.read_se_bounded::<i32>(-1, 8),
            Err(BitstreamError::SeRange {
                min: -1,
                max: 8,
                value: -2
            })
        );
    }

    #[test]
    fn garbage_bits_decode_deterministically() {
        let mut reader = BitReader::new(&[0xFF; 8], false);
        assert_eq!(reader.read_ue::<u32>(), Ok(0));
        assert_eq!(reader.read_bit(), Ok(true));
        assert_eq!(reader.skip_bits(7), Ok(()));
    }

    #[test]
    fn emulation_prevention_byte_skipped() {
        let mut reader = BitReader::new(&[0x00, 0x00, 0x03, 0x00], true);
        assert_eq!(reader.read_bits::<u32>(8), Ok(0));
        assert_eq!(reader.read_bits::<u32>(8), Ok(0));
        assert_eq!(reader.read_bits::<u32>(8), Ok(0));
        assert_eq!(reader.num_epb(), 1);
    }

    #[test]
    fn messages_match_legacy_text() {
        assert_eq!(
            BitstreamError::TooManyBits(32).to_string(),
            "more than 31 (32) bits were requested"
        );
        assert_eq!(
            BitstreamError::OutOfBits.to_string(),
            "reader ran out of bits"
        );
        assert_eq!(
            BitstreamError::BitsConversion.to_string(),
            "failed to convert read input to target type"
        );
        assert_eq!(BitstreamError::InvalidCode.to_string(), "invalid stream");
        assert_eq!(
            BitstreamError::CodeOverflow.to_string(),
            "read number cannot fit in 32 bits"
        );
        assert_eq!(
            BitstreamError::CodeConversion.to_string(),
            "conversion error"
        );
        assert_eq!(
            BitstreamError::UeRange {
                min: 0,
                max: 4,
                value: 5
            }
            .to_string(),
            "Value out of bounds: expected 0 - 4, got 5"
        );
        assert_eq!(
            BitstreamError::BoundedConversion.to_string(),
            "Conversion error"
        );
        assert_eq!(
            BitstreamError::SeRange {
                min: -1,
                max: 8,
                value: -2
            }
            .to_string(),
            "Value out of bounds, expected between -1-8, got -2"
        );
    }

    #[test]
    fn boundary_map_keeps_text() {
        assert_eq!(
            String::from(BitstreamError::OutOfBits),
            "reader ran out of bits"
        );
    }
}
