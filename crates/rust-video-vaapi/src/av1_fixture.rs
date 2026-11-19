//! Synthetic AV1 OBU payloads for unit tests.

/// Bit writer for synthetic OBU payloads.
#[derive(Default)]
pub(crate) struct Writer {
    /// Finished bytes.
    out: Vec<u8>,
    /// Open byte, filled from the top.
    cur: u8,
    /// Bits in `cur`.
    n: u8,
}

impl Writer {
    /// Empty writer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append one bit.
    pub(crate) fn bit(&mut self, value: bool) {
        self.cur = (self.cur << 1) | u8::from(value);
        self.n += 1;
        if self.n == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.n = 0;
        }
    }

    /// Append the low `count` bits of `value`.
    pub(crate) fn bits(&mut self, value: u32, count: usize) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 == 1);
        }
    }

    /// Finished payload with stop bit and byte alignment.
    pub(crate) fn bytes(mut self) -> Vec<u8> {
        self.bit(true);
        while self.n != 0 {
            self.bit(false);
        }
        self.out
    }

    /// Finished bytes without trailing bits.
    pub(crate) fn raw(mut self) -> Vec<u8> {
        if self.n != 0 {
            self.out.push(self.cur << (8 - self.n));
        }
        self.out
    }
}

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
    let mut writer = Writer::new();
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
    writer.bits(width - 1, width_bits as usize);
    writer.bits(height - 1, height_bits as usize);
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
    writer.bytes()
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
    let mut writer = Writer::new();
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
    writer.bytes()
}

/// Minimal inter-frame header payload: hidden, single tile, lossless.
pub(crate) fn pframe_payload() -> Vec<u8> {
    let mut writer = Writer::new();
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
    writer.bytes()
}

/// Show-existing-frame header payload for reference slot `slot`.
pub(crate) fn show_existing_payload(slot: u8) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.bit(true);
    writer.bits(u32::from(slot), 3);
    writer.bytes()
}

/// Tile group payload holding `tiles` bytes of tile data.
pub(crate) fn tile_group_payload(tiles: &[u8]) -> Vec<u8> {
    tiles.to_vec()
}

/// Two-tile group payload over 128x64: flag, first size, tile bytes.
pub(crate) fn tile_group_2_payload(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.bit(false);
    let mut payload = writer.raw();
    let size = u8::try_from(first.len().saturating_sub(1)).unwrap_or(u8::MAX);
    payload.push(size);
    payload.extend_from_slice(first);
    payload.extend_from_slice(second);
    payload
}
