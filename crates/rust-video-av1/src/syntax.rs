//! AV1 bit-syntax readers.

use crate::bits::BitReader;
use crate::error::{Error, ParseKind};

/// Unsigned variable-length code.
pub(crate) fn read_uvlc<E: From<Error>>(
    reader: &mut BitReader,
    syntax: &'static str,
) -> Result<u32, E> {
    let mut zeroes = 0u32;
    while !reader.read_bit(syntax)? {
        zeroes += 1;
        if zeroes >= 32 {
            return Err(Error::parse(ParseKind::OverflowU32 { syntax }));
        }
    }
    if zeroes == 0 {
        return Ok(0);
    }
    let rest = reader.read_bits(zeroes, syntax)?;
    rest.checked_add((1u32 << zeroes) - 1)
        .ok_or(Error::parse(ParseKind::OverflowU32 { syntax }))
}

/// Little-endian base-128 integer below the u32 ceiling.
pub(crate) fn read_leb128<E: From<Error>>(
    reader: &mut BitReader,
    syntax: &'static str,
) -> Result<u32, E> {
    let mut value: u64 = 0;
    for index in 0..8 {
        let byte = u64::from(reader.read_bits(8, syntax)?);
        value |= (byte & 0x7F) << (index * 7);
        if byte & 0x80 == 0 {
            return u32::try_from(value).map_err(|_| Error::parse(ParseKind::Range { syntax }));
        }
    }
    Err(Error::parse(ParseKind::Range { syntax }))
}

/// Nonsymmetric value below `n`.
pub(crate) fn read_ns<E: From<Error>>(
    reader: &mut BitReader,
    n: u32,
    syntax: &'static str,
) -> Result<u32, E> {
    if n <= 1 {
        return Ok(0);
    }
    let width = 32 - (n - 1).leading_zeros();
    let m = (1u32 << width) - n;
    let value = if width > 1 {
        reader.read_bits(width - 1, syntax)?
    } else {
        0
    };
    if value < m {
        return Ok(value);
    }
    let extra = u32::from(reader.read_bit(syntax)?);
    Ok((value << 1) - m + extra)
}

/// Unary value between `min` and `max`.
pub(crate) fn read_increment<E: From<Error>>(
    reader: &mut BitReader,
    min: u32,
    max: u32,
    syntax: &'static str,
) -> Result<u32, E> {
    let mut value = min;
    while value < max {
        if reader.read_bit(syntax)? {
            value += 1;
        } else {
            break;
        }
    }
    Ok(value)
}

/// Subexponential value below `max`.
pub(crate) fn read_subexp<E: From<Error>>(
    reader: &mut BitReader,
    max: u32,
    syntax: &'static str,
) -> Result<u32, E> {
    if max < 9 {
        return Err(Error::parse(ParseKind::Range { syntax }));
    }
    let max_len = 32 - (max - 1).leading_zeros() - 3 - 1;
    let len = read_increment(reader, 0, max_len, syntax)?;
    let (bits, offset) = if len > 0 {
        (2 + len, 1u32 << (2 + len))
    } else {
        (3, 0)
    };
    let value = if len < max_len {
        reader.read_bits(bits, syntax)?
    } else {
        read_ns(reader, max - offset, syntax)?
    };
    value
        .checked_add(offset)
        .ok_or(Error::parse(ParseKind::Range { syntax }))
}

/// Signed two's complement value of `width` bits.
pub(crate) fn read_su<E: From<Error>>(
    reader: &mut BitReader,
    width: u32,
    syntax: &'static str,
) -> Result<i32, E> {
    if width == 0 || width > 32 {
        return Err(Error::parse(ParseKind::Range { syntax }));
    }
    let raw = reader.read_bits(width, syntax)?;
    Ok((raw << (32 - width)).cast_signed() >> (32 - width))
}

/// Fixed-width value below 256.
pub(crate) fn read_small<E: From<Error>>(
    reader: &mut BitReader,
    width: u32,
    syntax: &'static str,
) -> Result<u8, E> {
    let value = reader.read_bits(width, syntax)?;
    u8::try_from(value).map_err(|_| Error::parse(ParseKind::Range { syntax }))
}

/// Smallest shift with `block_size` shifted past `target`.
#[must_use]
pub fn tile_log2(block_size: u32, target: u32) -> u32 {
    let mut shift = 0u32;
    while u64::from(block_size) << shift < u64::from(target) {
        shift += 1;
    }
    shift
}

/// Relative order hint distance of `a` behind `b`.
#[must_use]
pub fn relative_dist(a: u8, b: u8, order_hint_bits_minus1: u8) -> i32 {
    let diff = u32::from(a).wrapping_sub(u32::from(b));
    let m = 1u32 << order_hint_bits_minus1;
    (diff & (m - 1)).wrapping_sub(diff & m).cast_signed()
}
