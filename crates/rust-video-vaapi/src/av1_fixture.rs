//! Synthetic AV1 OBU payloads for unit tests.

use rust_video_harness::Bits;

/// Low-overhead OBU of `type_` with `payload`.
pub(crate) fn obu(type_: u8, payload: &[u8]) -> Vec<u8> {
    let mut obu = vec![type_ << 3 | 0x02];
    let mut size = Vec::new();
    let mut value = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        size.push(byte);
        if value == 0 {
            break;
        }
    }
    obu.extend(size);
    obu.extend_from_slice(payload);
    obu
}

/// Minimal 64x64 Profile 0 sequence header payload.
pub(crate) fn seq_payload() -> Vec<u8> {
    seq_payload_sized(64, 64)
}

/// Minimal Profile 0 sequence header payload at `width` by `height`.
pub(crate) fn seq_payload_sized(width: u32, height: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 5);
    writer.bits(0, 12);
    writer.bits(0, 5);
    let width_bits = (u32::BITS - (width - 1).leading_zeros()).max(1);
    let height_bits = (u32::BITS - (height - 1).leading_zeros()).max(1);
    writer.bits(width_bits - 1, 4);
    writer.bits(height_bits - 1, 4);
    writer.bits(width - 1, width_bits);
    writer.bits(height - 1, height_bits);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 2);
    writer.bit(false);
    writer.bit(false);
    writer.finish_rbsp()
}

/// Minimal keyframe header payload: shown, single tile, lossless.
pub(crate) fn keyframe_payload() -> Vec<u8> {
    keyframe_bits(true, false)
}

/// Hidden keyframe header payload: showable, single tile, lossless.
pub(crate) fn keyframe_hidden_payload() -> Vec<u8> {
    keyframe_bits(false, false)
}

/// Keyframe header payload over 128x64: two tiles, lossless.
pub(crate) fn keyframe_wide_payload() -> Vec<u8> {
    keyframe_bits(true, true)
}

/// Keyframe header bits: `show` controls output, `wide` adds a tile column.
fn keyframe_bits(show: bool, wide: bool) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(false);
    writer.bits(0, 2);
    writer.bit(show);
    if !show {
        writer.bit(true);
        writer.bit(false);
    }
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    if !show {
        writer.bits(0xff, 8);
    }
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    if wide {
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 2);
    }
    writer.bits(0, 8);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.finish_rbsp()
}

/// Minimal inter-frame header payload: hidden, single tile, lossless.
pub(crate) fn pframe_payload() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(false);
    writer.bits(1, 2);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bits(7, 3);
    writer.bits(1, 8);
    for _ in 0..7 {
        writer.bits(0, 3);
    }
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bits(0, 8);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    for _ in 0..7 {
        writer.bit(false);
    }
    writer.finish_rbsp()
}

/// Show-existing-frame header payload for reference slot `slot`.
pub(crate) fn show_existing_payload(slot: u8) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bits(u32::from(slot), 3);
    writer.finish_rbsp()
}

/// Tile group payload holding `tiles` bytes of tile data.
pub(crate) fn tile_group_payload(tiles: &[u8]) -> Vec<u8> {
    tiles.to_vec()
}

/// Two-tile group payload over 128x64: flag, first size, tile bytes.
pub(crate) fn tile_group_2_payload(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(false);
    let mut payload = writer.finish_raw();
    let size = u8::try_from(first.len().saturating_sub(1)).unwrap_or(u8::MAX);
    payload.push(size);
    payload.extend_from_slice(first);
    payload.extend_from_slice(second);
    payload
}
