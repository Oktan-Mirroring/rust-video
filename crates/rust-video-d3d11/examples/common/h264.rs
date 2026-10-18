//! Synthetic H.264 Annex B for d3d11 targets.

/// Two packets: sets plus IDR, then one P picture.
pub(crate) fn annex_b_packets() -> Vec<Vec<u8>> {
    let sets = packet(&[
        nal(7, 3, &sps_body()),
        nal(8, 3, &pps_body()),
        nal(5, 2, &idr_slice_body()),
    ]);
    let inter = packet(&[nal(1, 2, &p_slice_body(1, 2))]);
    vec![sets, inter]
}

/// One packet: sets plus IDR.
pub(crate) fn annex_b_h264_packets() -> Vec<Vec<u8>> {
    sets_packet(&sps_body_crop(0))
}

/// One packet: bottom-cropped sets plus IDR, 320x224 visible in 320x240.
pub(crate) fn annex_b_h264_packets_cropped() -> Vec<Vec<u8>> {
    sets_packet(&sps_body_crop(8))
}

/// One packet: bottom-cropped sets plus IDR at coded macroblock extents.
pub(crate) fn annex_b_h264_packets_cropped_sized(
    width_mbs_minus1: u32,
    height_mbs_minus1: u32,
    bottom: u32,
    level_idc: u8,
) -> Vec<Vec<u8>> {
    sets_packet(&sps_body_sized(
        width_mbs_minus1,
        height_mbs_minus1,
        bottom,
        level_idc,
    ))
}

/// One packet: `sps` plus picture set plus IDR.
fn sets_packet(sps: &[u8]) -> Vec<Vec<u8>> {
    vec![packet(&[
        nal(7, 3, sps),
        nal(8, 3, &pps_body()),
        nal(5, 2, &idr_slice_body()),
    ])]
}

/// NAL unit with header over the EBSP body.
fn nal(nal_type: u8, ref_idc: u8, body: &[u8]) -> Vec<u8> {
    let mut nal = vec![ref_idc << 5 | nal_type];
    nal.extend_from_slice(body);
    nal
}

/// Packet of NAL units with four-byte start codes.
fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut packet = Vec::new();
    for nal in nals {
        packet.extend_from_slice(&[0, 0, 0, 1]);
        packet.extend_from_slice(nal);
    }
    packet
}

/// Baseline 64x64 sequence set body.
fn sps_body() -> Vec<u8> {
    sps_body_sized(3, 3, 0, 30)
}

/// Baseline 320x240 sequence set body with `bottom` crop units.
fn sps_body_crop(bottom: u32) -> Vec<u8> {
    sps_body_sized(19, 14, bottom, 30)
}

/// Baseline sequence set body at coded extents with `bottom` crop units.
fn sps_body_sized(
    width_mbs_minus1: u32,
    height_mbs_minus1: u32,
    bottom: u32,
    level_idc: u8,
) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(66, 8);
    writer.bits(0, 8);
    writer.bits(u32::from(level_idc), 8);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(1);
    writer.bit(false);
    writer.ue(width_mbs_minus1);
    writer.ue(height_mbs_minus1);
    writer.bit(true);
    writer.bit(true);
    if bottom == 0 {
        writer.bit(false);
    } else {
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(bottom);
    }
    writer.bit(false);
    writer.finish()
}

/// Picture set body over sequence set zero.
fn pps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bits(0, 2);
    writer.se(0);
    writer.se(0);
    writer.se(0);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.se(0);
    writer.finish()
}

/// IDR slice header body over the example sets.
fn idr_slice_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(2);
    writer.ue(0);
    writer.bits(0, 4);
    writer.ue(0);
    writer.bits(0, 4);
    writer.bit(false);
    writer.bit(false);
    writer.se(0);
    writer.ue(0);
    writer.se(0);
    writer.se(0);
    writer.finish()
}

/// P slice header body with `frame_num` and `lsb`.
fn p_slice_body(frame_num: u32, lsb: u32) -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bits(frame_num, 4);
    writer.bits(lsb, 4);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.se(0);
    writer.ue(0);
    writer.se(0);
    writer.se(0);
    writer.finish()
}

/// Test bit writer with emulation prevention.
struct Bits {
    /// Finished bytes.
    bytes: Vec<u8>,
    /// Open byte under fill.
    current: u8,
    /// Bits filled in the open byte.
    filled: u8,
}

impl Bits {
    /// Empty writer.
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    /// Single bit.
    fn bit(&mut self, set: bool) {
        if set {
            self.current |= 1 << (7 - self.filled);
        }
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.filled = 0;
        }
    }

    /// Low `count` bits of `value`, most significant first.
    fn bits(&mut self, value: u32, count: u32) {
        for index in (0..count).rev() {
            self.bit(value >> index & 1 != 0);
        }
    }

    /// Unsigned Exp-Golomb code.
    fn ue(&mut self, value: u32) {
        let code = value + 1;
        let width = 32 - code.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        self.bits(code, width);
    }

    /// Signed Exp-Golomb code.
    fn se(&mut self, value: i32) {
        let code = if value > 0 {
            u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
        } else {
            u32::try_from(-value).unwrap_or(u32::MAX) * 2
        };
        self.ue(code);
    }

    /// Finished EBSP with emulation prevention inserted.
    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.filled != 0 {
            self.bit(false);
        }
        let mut out = Vec::new();
        let mut zeros = 0;
        for byte in self.bytes {
            if zeros == 2 && byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        out
    }
}
