//! Synthetic HEVC Annex B plus a mid-grey frame for d3d12 targets.

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

/// Mid-grey NV12 frame of `width` x `height`.
pub(crate) fn grey_nv12(width: u32, height: u32) -> CpuFrame {
    let size = PictureSize::new(width, height).expect("nonzero extent");
    let y_len = width as usize * height as usize;
    CpuFrame::new(
        size,
        PixelFormat::Nv12,
        vec![
            Plane::new(Bytes::from(vec![0x80; y_len]), width).expect("luma"),
            Plane::new(Bytes::from(vec![0x80; y_len / 2]), width).expect("chroma"),
        ],
    )
    .expect("frame")
}

/// Two packets: sets plus IDR, then one more IDR.
pub(crate) fn annex_b_packets() -> Vec<Vec<u8>> {
    let sets = packet(&[
        nal(33, &sps_body()),
        nal(34, &pps_body()),
        nal(19, &idr_slice_body()),
    ]);
    let second = packet(&[nal(19, &idr_slice_body())]);
    vec![sets, second]
}

/// NAL unit with two-byte header over the body.
fn nal(nal_type: u8, body: &[u8]) -> Vec<u8> {
    let mut nal = vec![nal_type << 1, 1];
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

/// Main-profile 320x240 sequence set body.
fn sps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bits(0, 4);
    writer.bits(0, 3);
    writer.bit(true);
    writer.bits(0, 2);
    writer.bit(false);
    writer.bits(1, 5);
    writer.bits(0x4000_0000, 32);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bits(0, 32);
    writer.bits(0, 11);
    writer.bit(false);
    writer.bits(93, 8);
    writer.ue(0);
    writer.ue(1);
    writer.ue(320);
    writer.ue(240);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.bit(true);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(0);
    writer.ue(1);
    writer.ue(1);
    writer.bit(false);
    writer.bit(true);
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(true);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// Picture set body over sequence set zero.
fn pps_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.ue(0);
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.bits(0, 3);
    writer.bit(false);
    writer.bit(false);
    writer.ue(0);
    writer.ue(0);
    writer.se(0);
    writer.bit(false);
    writer.bit(false);
    writer.bit(false);
    writer.se(0);
    writer.se(0);
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
    writer.ue(0);
    writer.bit(false);
    writer.bit(false);
    writer.finish()
}

/// IDR slice header body over the synthetic sets.
fn idr_slice_body() -> Vec<u8> {
    let mut writer = Bits::new();
    writer.bit(true);
    writer.bit(false);
    writer.ue(0);
    writer.ue(2);
    writer.bit(false);
    writer.bit(false);
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
