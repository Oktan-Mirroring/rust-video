//! Coded-video access units and keyframe hooks.
//!
//! NAL types: [FFmpeg n7.1 `h264.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/h264.h)
//! and [`hevc.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/hevc/hevc.h).
//! OBU types: [FFmpeg n7.1 `av1.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/av1.h);
//! frame-header bit order: [dav1d 1.5.1 `obu.c`](https://github.com/videolan/dav1d/blob/1.5.1/src/obu.c).
//! VP8 tag: [RFC 6386 §9.1](https://datatracker.ietf.org/doc/html/rfc6386#section-9.1).
//! VP9 header: [VP9 spec §6.2/§7.2](https://www.webmproject.org/vp9/); sync
//! `0x498342`, bit order: [FFmpeg master `vpcc.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/vpcc.c).

use std::sync::atomic::{AtomicBool, Ordering};

/// Coded video codec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Codec {
    /// ITU-T H.264.
    H264,
    /// ITU-T H.265.
    H265,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
    /// VP8.
    Vp8,
    /// AV2 packing stub; every mux path declines it typed.
    Av2,
}

/// One decodable frame: NALUs or OBUs sharing a timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessUnit {
    /// Codec of `payload`.
    codec: Codec,
    /// Annex-B byte stream (`H264`/`H265`) or OBU temporal unit (`Av1`).
    payload: Vec<u8>,
    /// Presentation timestamp in 90 kHz ticks.
    pts: u64,
    /// Decode timestamp in 90 kHz ticks.
    dts: u64,
    /// Caller assertion; `None` classifies from `payload`.
    keyframe: Option<bool>,
}

/// Keyframe gate verdict for one access unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PacketAction {
    /// Bytes were appended; carries the count.
    Emitted(usize),
    /// Dropped while a requested keyframe is still outstanding.
    HeldForKeyframe,
}

/// Gate state after polling one access unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KeyframeState {
    /// Unit may flow.
    Live,
    /// Unit must wait for the requested keyframe.
    AwaitingKeyframe,
}

/// Keyframe-on-demand gate for the realtime path.
///
/// A join or loss event calls [`KeyframeGate::request`]; each mux polls per
/// access unit and holds non-keyframes until the keyframe clears the gate.
#[derive(Debug, Default)]
pub struct KeyframeGate {
    /// Outstanding keyframe request.
    pending: AtomicBool,
}

/// Annex-B NALU splitter over one access unit.
#[derive(Debug)]
pub struct AnnexB<'a> {
    /// Remaining unscanned bytes.
    rest: &'a [u8],
}

/// Parsed H.264 sequence parameter set fields needed for `avcC`.
///
/// Field order: ITU-T H.264 section 7.3.2.1.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AvcSpsInfo {
    /// `profile_idc`.
    pub profile: u8,
    /// `constraint_set` flags plus reserved bits.
    pub constraints: u8,
    /// `level_idc`.
    pub level: u8,
    /// `chroma_format_idc`; 1 when absent from the RBSP.
    pub chroma: u8,
    /// `bit_depth_luma_minus8`; 0 when absent.
    pub bit_depth_luma: u8,
    /// `bit_depth_chroma_minus8`; 0 when absent.
    pub bit_depth_chroma: u8,
}

/// Big-endian bit reader over a NALU RBSP.
#[derive(Debug)]
struct BitReader<'a> {
    /// Source bytes.
    bytes: &'a [u8],
    /// Consumed bit count.
    pos: usize,
}

impl Codec {
    /// MPEG-TS `stream_type`; `None` has no registered value.
    ///
    /// `0x1B`/`0x24`: H.222.0 section 2.4.4.9 table 2-34. AV1/VP9/VP8/AV2
    /// have none: [FFmpeg master `mpegts.h`](https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/mpegts.h)
    /// carries no video type for them (`0xCF` there is HLS AAC audio).
    #[must_use]
    pub fn ts_stream_type(self) -> Option<u8> {
        match self {
            Self::H264 => Some(0x1B),
            Self::H265 => Some(0x24),
            Self::Av1 | Self::Vp9 | Self::Vp8 | Self::Av2 => None,
        }
    }

    /// ISO BMFF sample entry fourcc.
    ///
    /// `Av2` never reaches a mux: no `TrackConfig` carries it, so this
    /// arm is BLOCKED and pinned by decline tests, not by bytes.
    #[must_use]
    pub fn sample_entry(self) -> [u8; 4] {
        match self {
            Self::H264 => *b"avc1",
            Self::H265 => *b"hvc1",
            Self::Av1 => *b"av01",
            Self::Vp9 => *b"vp09",
            Self::Vp8 => *b"vp08",
            Self::Av2 => *b"av02",
        }
    }

    /// True when `payload` starts a random access point.
    #[must_use]
    pub fn is_keyframe(self, payload: &[u8]) -> bool {
        match self {
            Self::H264 => AnnexB::new(payload)
                .filter_map(|nalu| nalu.first().map(|b| b & 0x1F))
                .find(|t| (1..=5).contains(t))
                .is_some_and(|t| t == 5),
            Self::H265 => AnnexB::new(payload)
                .filter_map(|nalu| nalu.first().map(|b| (b >> 1) & 0x3F))
                .find(|t| *t <= 23)
                .is_some_and(|t| (16..=23).contains(&t)),
            Self::Av1 => Self::av1_frame_is_key(payload),
            Self::Vp9 => Self::vp9_frame_is_key(payload),
            Self::Vp8 => Self::vp8_frame_is_key(payload),
            Self::Av2 => false,
        }
    }

    /// True when the first AV1 frame OBU is a key frame.
    ///
    /// Reads `show_existing_frame` then `frame_type` at the frame header
    /// start; key iff bits are `0,00`. Streams using
    /// `reduced_still_picture_header` omit the first bit and always
    /// classify `false`; that shape never appears on the live path.
    fn av1_frame_is_key(payload: &[u8]) -> bool {
        let mut rest = payload;
        while !rest.is_empty() {
            let Some((obu_type, header_len, size)) = Self::obu_header(rest) else {
                return false;
            };
            let body = &rest[header_len..];
            let end = size.map_or(body.len(), |len| len.min(body.len()));
            let body = &body[..end];
            if obu_type == 3 || obu_type == 6 {
                return Self::av1_header_is_key(body);
            }
            if !matches!(obu_type, 1 | 2 | 4 | 5 | 7 | 15) {
                return false;
            }
            if size.is_none() {
                return false;
            }
            rest = &rest[header_len + end..];
        }
        false
    }

    /// OBU header: type, header length, payload length when sized.
    fn obu_header(bytes: &[u8]) -> Option<(u8, usize, Option<usize>)> {
        let header = *bytes.first()?;
        if header >> 7 != 0 {
            return None;
        }
        let obu_type = (header >> 3) & 0x0F;
        let mut len = 1;
        if header & 0x04 != 0 {
            len += 1;
            if bytes.len() < 2 {
                return None;
            }
        }
        if header & 0x02 == 0 {
            return Some((obu_type, len, None));
        }
        let (size, size_len) = Self::leb128(bytes.get(len..)?)?;
        Some((obu_type, len + size_len, Some(size)))
    }

    /// Unsigned LEB128 value and its byte length.
    fn leb128(bytes: &[u8]) -> Option<(usize, usize)> {
        let mut value: usize = 0;
        for (i, &b) in bytes.iter().take(8).enumerate() {
            value |= usize::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Some((value, i + 1));
            }
        }
        None
    }

    /// True when a frame header body opens with `0,00`.
    fn av1_header_is_key(body: &[u8]) -> bool {
        BitReader::new(body).read_bits(3).is_some_and(|v| v == 0)
    }

    /// True when a VP8 frame tag opens a key frame.
    ///
    /// Bit 0 of byte 0 is `frame_type`: 0 key, 1 inter; empty payload
    /// carries no tag.
    fn vp8_frame_is_key(payload: &[u8]) -> bool {
        payload.first().is_some_and(|b| b & 0x01 == 0)
    }

    /// True when a VP9 uncompressed header opens a key frame.
    ///
    /// Key iff `frame_marker` is `0b10`, `show_existing_frame` is 0,
    /// and `frame_type` is 0. Truncation or marker mismatch reads
    /// `false`.
    fn vp9_frame_is_key(payload: &[u8]) -> bool {
        let mut reader = BitReader::new(payload);
        if reader.read_bits(2) != Some(0b10) {
            return false;
        }
        let low = reader.read_bits(1);
        let high = reader.read_bits(1);
        let (Some(low), Some(high)) = (low, high) else {
            return false;
        };
        if (high << 1) | low == 3 && reader.read_bits(1).is_none() {
            return false;
        }
        if reader.read_bits(1) != Some(0) {
            return false;
        }
        reader.read_bits(1) == Some(0)
    }
}

impl AccessUnit {
    /// Access unit with an explicit keyframe flag.
    #[must_use]
    pub fn new(codec: Codec, payload: Vec<u8>, pts: u64, dts: u64, keyframe: bool) -> Self {
        Self {
            codec,
            payload,
            pts,
            dts,
            keyframe: Some(keyframe),
        }
    }

    /// Access unit classified from its payload.
    #[must_use]
    pub fn classified(codec: Codec, payload: Vec<u8>, pts: u64, dts: u64) -> Self {
        Self {
            codec,
            payload,
            pts,
            dts,
            keyframe: None,
        }
    }

    /// Codec of the payload.
    #[must_use]
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Coded bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Presentation timestamp in 90 kHz ticks.
    #[must_use]
    pub fn pts(&self) -> u64 {
        self.pts
    }

    /// Decode timestamp in 90 kHz ticks.
    #[must_use]
    pub fn dts(&self) -> u64 {
        self.dts
    }

    /// Caller flag, else payload classification.
    #[must_use]
    pub fn is_keyframe(&self) -> bool {
        self.keyframe
            .unwrap_or_else(|| self.codec.is_keyframe(&self.payload))
    }
}

impl KeyframeGate {
    /// Gate with no outstanding request.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Demand the next keyframe; non-keyframes hold until it lands.
    pub fn request(&self) {
        self.pending.store(true, Ordering::SeqCst);
    }

    /// Poll one unit; a keyframe clears a pending request.
    #[must_use]
    pub fn poll(&self, is_keyframe: bool) -> KeyframeState {
        if !self.pending.load(Ordering::SeqCst) {
            return KeyframeState::Live;
        }
        if is_keyframe {
            self.pending.store(false, Ordering::SeqCst);
            KeyframeState::Live
        } else {
            KeyframeState::AwaitingKeyframe
        }
    }
}

impl<'a> AnnexB<'a> {
    /// Splitter over annex-B bytes.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    /// Next start code as `(code_start, content_start)`, if any.
    fn start_code(bytes: &[u8]) -> Option<(usize, usize)> {
        let mut zeros = 0;
        for (i, &b) in bytes.iter().enumerate() {
            if b == 1 && zeros >= 2 {
                return Some((i - zeros, i + 1));
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        None
    }
}

impl<'a> Iterator for AnnexB<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let bytes = self.rest;
            let (_, start) = Self::start_code(bytes)?;
            let tail = &bytes[start..];
            let rel_end = Self::start_code(tail).map_or(tail.len(), |(s, _)| s);
            let end = start + rel_end;
            self.rest = &bytes[end..];
            if start < end {
                return Some(&bytes[start..end]);
            }
        }
    }
}

impl<'a> BitReader<'a> {
    /// Reader over `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Next `count` bits, `None` past the end.
    fn read_bits(&mut self, count: usize) -> Option<u32> {
        if count > 32 || self.pos + count > self.bytes.len() * 8 {
            return None;
        }
        let mut value = 0;
        for _ in 0..count {
            let byte = self.bytes[self.pos / 8];
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            value = (value << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(value)
    }

    /// Unsigned Exp-Golomb code.
    fn read_ue(&mut self) -> Option<u32> {
        let mut zeros: usize = 0;
        while self.read_bits(1)? == 0 {
            zeros += 1;
            if zeros > 30 {
                return None;
            }
        }
        let rest = self.read_bits(zeros).unwrap_or(0);
        Some((1 << zeros) - 1 + rest)
    }

    /// Remaining bit count.
    fn remaining(&self) -> usize {
        self.bytes.len() * 8 - self.pos
    }
}

impl AvcSpsInfo {
    /// Fields parsed from a sequence parameter set NALU.
    ///
    /// # Arguments
    ///
    /// * `sps` - full NALU including its header byte.
    ///
    /// # Returns
    ///
    /// `None` when the RBSP ends before the needed fields.
    #[must_use]
    pub fn parse(sps: &[u8]) -> Option<Self> {
        let body = sps.get(1..)?;
        let (&profile, body) = body.split_first()?;
        let (&constraints, body) = body.split_first()?;
        let (&level, body) = body.split_first()?;
        let mut reader = BitReader::new(body);
        reader.read_ue()?;
        let ext_profile = matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128);
        let (chroma, luma, chroma_depth) = if ext_profile {
            let chroma = u8::try_from(reader.read_ue()?).ok()?;
            if chroma == 3 && reader.remaining() > 0 {
                reader.read_bits(1)?;
            }
            let luma = u8::try_from(reader.read_ue()?).ok()?;
            let chroma_depth = u8::try_from(reader.read_ue()?).ok()?;
            (chroma, luma, chroma_depth)
        } else {
            (1, 0, 0)
        };
        Some(Self {
            profile,
            constraints,
            level,
            chroma,
            bit_depth_luma: luma,
            bit_depth_chroma: chroma_depth,
        })
    }

    /// True when `avcC` must carry chroma/bit-depth extensions.
    #[must_use]
    pub fn needs_extensions(self) -> bool {
        matches!(
            self.profile,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessUnit, AnnexB, AvcSpsInfo, Codec, KeyframeGate, KeyframeState};

    /// Annex-B access unit from `(header, body_len)` NALUs.
    fn annexb(nalus: &[(u8, usize)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (header, len) in nalus {
            out.extend_from_slice(&[0, 0, 0, 1, *header]);
            out.extend(std::iter::repeat_n(0xAB, *len));
        }
        out
    }

    #[test]
    fn annexb_splits_mixed_start_codes() {
        let bytes = [0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x68, 0xBB, 0xCC];
        let units: Vec<&[u8]> = AnnexB::new(&bytes).collect();
        assert_eq!(units, vec![&[0x67, 0xAA][..], &[0x68, 0xBB, 0xCC][..]]);
    }

    #[test]
    fn annexb_skips_empty_units() {
        let bytes = [0, 0, 1, 0, 0, 1, 0x65, 0xFF];
        let units: Vec<&[u8]> = AnnexB::new(&bytes).collect();
        assert_eq!(units, vec![&[0x65, 0xFF][..]]);
    }

    #[test]
    fn annexb_needs_start_code() {
        assert_eq!(AnnexB::new(&[0x65, 0xFF]).count(), 0);
        assert_eq!(AnnexB::new(&[]).count(), 0);
    }

    #[test]
    fn h264_idr_is_key_p_slice_is_not() {
        assert!(Codec::H264.is_keyframe(&annexb(&[(0x67, 4), (0x68, 2), (0x65, 16)])));
        assert!(!Codec::H264.is_keyframe(&annexb(&[(0x41, 16)])));
        assert!(!Codec::H264.is_keyframe(&annexb(&[(0x67, 4)])));
    }

    #[test]
    fn h264_first_vcl_wins() {
        assert!(!Codec::H264.is_keyframe(&annexb(&[(0x41, 8), (0x65, 8)])));
    }

    #[test]
    fn h265_irap_is_key_trail_is_not() {
        assert!(Codec::H265.is_keyframe(&annexb(&[(0x26, 16)])));
        assert!(Codec::H265.is_keyframe(&annexb(&[(0x28, 16)])));
        assert!(Codec::H265.is_keyframe(&annexb(&[(0x2A, 16)])));
        assert!(!Codec::H265.is_keyframe(&annexb(&[(0x02, 16)])));
        assert!(!Codec::H265.is_keyframe(&annexb(&[(0x42, 8)])));
    }

    /// Sized OBU with `header`, extension byte, and `body`.
    fn obu(obu_type: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![
            (obu_type << 3) | 0x02,
            u8::try_from(body.len()).unwrap_or(u8::MAX),
        ];
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn av1_key_frame_is_key_inter_is_not() {
        assert!(Codec::Av1.is_keyframe(&obu(6, &[0x00, 0x00])));
        assert!(!Codec::Av1.is_keyframe(&obu(6, &[0x20, 0x00])));
        assert!(!Codec::Av1.is_keyframe(&obu(6, &[0x80, 0x00])));
    }

    #[test]
    fn av1_skips_leading_obus() {
        let mut payload = obu(1, &[0xAA, 0xBB]);
        payload.extend_from_slice(&obu(6, &[0x00]));
        assert!(Codec::Av1.is_keyframe(&payload));
        assert!(!Codec::Av1.is_keyframe(&obu(1, &[0xAA])));
    }

    #[test]
    fn av1_rejects_bad_headers() {
        assert!(!Codec::Av1.is_keyframe(&[0x80]));
        assert!(!Codec::Av1.is_keyframe(&[]));
    }

    #[test]
    fn ts_stream_types_match_spec() {
        assert_eq!(Codec::H264.ts_stream_type(), Some(0x1B));
        assert_eq!(Codec::H265.ts_stream_type(), Some(0x24));
        assert_eq!(Codec::Av1.ts_stream_type(), None);
        assert_eq!(Codec::Vp9.ts_stream_type(), None);
        assert_eq!(Codec::Vp8.ts_stream_type(), None);
        assert_eq!(Codec::Av2.ts_stream_type(), None);
    }

    #[test]
    fn sample_entries_match_fourcc() {
        assert_eq!(Codec::H264.sample_entry(), *b"avc1");
        assert_eq!(Codec::H265.sample_entry(), *b"hvc1");
        assert_eq!(Codec::Av1.sample_entry(), *b"av01");
        assert_eq!(Codec::Vp9.sample_entry(), *b"vp09");
        assert_eq!(Codec::Vp8.sample_entry(), *b"vp08");
    }

    #[test]
    fn vp8_tag_bit_selects_key() {
        assert!(Codec::Vp8.is_keyframe(&[0x10, 0x9D, 0x01, 0x2A]));
        assert!(!Codec::Vp8.is_keyframe(&[0x01]));
        assert!(!Codec::Vp8.is_keyframe(&[]));
    }

    #[test]
    fn vp9_header_bits_select_key() {
        assert!(Codec::Vp9.is_keyframe(&[0x80, 0x00]));
        assert!(!Codec::Vp9.is_keyframe(&[0x84, 0x00]));
        assert!(!Codec::Vp9.is_keyframe(&[0x88, 0x00]));
        assert!(!Codec::Vp9.is_keyframe(&[0x00, 0x00]));
        assert!(!Codec::Vp9.is_keyframe(&[]));
    }

    #[test]
    fn av2_never_classifies_key() {
        assert!(!Codec::Av2.is_keyframe(&[0x00]));
        assert!(!Codec::Av2.is_keyframe(&[]));
    }

    #[test]
    fn explicit_flag_beats_classifier() {
        let payload = annexb(&[(0x41, 8)]);
        assert!(AccessUnit::new(Codec::H264, payload.clone(), 0, 0, true).is_keyframe());
        assert!(!AccessUnit::classified(Codec::H264, payload, 0, 0).is_keyframe());
    }

    #[test]
    fn gate_holds_until_keyframe() {
        let gate = KeyframeGate::new();
        assert_eq!(gate.poll(false), KeyframeState::Live);
        gate.request();
        assert_eq!(gate.poll(false), KeyframeState::AwaitingKeyframe);
        assert_eq!(gate.poll(false), KeyframeState::AwaitingKeyframe);
        assert_eq!(gate.poll(true), KeyframeState::Live);
        assert_eq!(gate.poll(false), KeyframeState::Live);
    }

    #[test]
    fn sps_baseline_parses_without_extensions() {
        let sps = [0x67, 0x42, 0x00, 0x1E, 0xF4, 0x02, 0x80, 0x2D, 0xC8];
        let info = AvcSpsInfo::parse(&sps).expect("valid prefix");
        assert_eq!(info.profile, 0x42);
        assert_eq!(info.chroma, 1);
        assert!(!info.needs_extensions());
    }

    #[test]
    fn sps_high_parses_chroma_depths() {
        let sps = [
            0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x78, 0x02, 0x27, 0xE5, 0x84,
        ];
        let info = AvcSpsInfo::parse(&sps).expect("valid prefix");
        assert_eq!(info.profile, 100);
        assert!(info.needs_extensions());
    }

    #[test]
    fn sps_rejects_truncation() {
        assert_eq!(AvcSpsInfo::parse(&[0x67, 0x42]), None);
        assert_eq!(AvcSpsInfo::parse(&[]), None);
    }
}
