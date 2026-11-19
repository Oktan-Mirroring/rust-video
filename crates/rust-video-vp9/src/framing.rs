//! VP9 superframe framing over coded chunks.

use crate::error::Error;

/// Superframe index marker closing the chunk.
const SUPERFRAME_MARKER: u8 = 0x06;

/// Chunk frames: superframe members or the chunk alone.
pub(crate) fn split_superframe<E: From<Error>>(data: &[u8]) -> Result<Vec<&[u8]>, E> {
    let Some(last) = data.last().copied() else {
        return Ok(Vec::new());
    };
    if last >> 5 != SUPERFRAME_MARKER {
        return Ok(vec![data]);
    }
    let bytes_per_size = usize::from((last >> 3) & 3) + 1;
    let count = usize::from(last & 7) + 1;
    let index = 2 + count * bytes_per_size;
    let Some(coded) = data.len().checked_sub(index) else {
        return Ok(vec![data]);
    };
    if data[coded] != last {
        return Ok(vec![data]);
    }
    let mut frames = Vec::with_capacity(count);
    let mut offset = 0usize;
    for position in 0..count {
        let mut size = 0usize;
        for byte in 0..bytes_per_size {
            size |= usize::from(data[coded + 1 + position * bytes_per_size + byte]) << (byte * 8);
        }
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= coded)
            .ok_or(E::from(Error::Range {
                syntax: "superframe_index",
            }))?;
        frames.push(&data[offset..end]);
        offset = end;
    }
    if offset != coded {
        return Err(E::from(Error::Range {
            syntax: "superframe_index",
        }));
    }
    Ok(frames)
}
