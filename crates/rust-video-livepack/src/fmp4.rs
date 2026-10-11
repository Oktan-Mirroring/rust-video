//! Fragmented MP4 / CMAF mux for H.264/H.265/AV1.
//!
//! Boxes: ISO/IEC 14496-12; `avcC`/`hvcC`: 14496-15; `av1C`:
//! [AV1-ISOBMFF §2.3](https://aomediacodec.github.io/av1-isobmff/#av1codecconfigurationbox-section).
//! `cmfc`/`iso6` brands follow
//! [FFmpeg n7.1 `movenc.c`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavformat/movenc.c).

use std::fmt;

use crate::codec::{
    AccessUnit, AnnexB, AvcSpsInfo, Codec, KeyframeGate, KeyframeState, PacketAction,
};
use crate::time::{ClockTracker, Ts90k, STC_WRAP};

/// Decoder configuration failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ConfigError {
    /// No SPS or no PPS supplied.
    EmptyParameterSets,
    /// First SPS unparseable.
    BadAvcSps,
    /// More entries than `avcC` holds: 31 SPS or 255 PPS.
    TooManyParameterSets,
    /// Parameter set past 65535 bytes.
    ParameterSetTooLarge,
    /// Malformed `hvcC` record; carries the cause.
    BadHvcc(&'static str),
    /// Malformed `av1C` record; carries the cause.
    BadAv1c(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyParameterSets => write!(f, "no SPS or PPS supplied"),
            Self::BadAvcSps => write!(f, "first SPS unparseable"),
            Self::TooManyParameterSets => write!(f, "more entries than avcC holds"),
            Self::ParameterSetTooLarge => write!(f, "parameter set past 65535 bytes"),
            Self::BadHvcc(cause) => write!(f, "malformed hvcc: {cause}"),
            Self::BadAv1c(cause) => write!(f, "malformed av1C: {cause}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Mux failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Fmp4Error {
    /// Decoder configuration failure.
    Config(ConfigError),
    /// Sample codec differs from track codec.
    CodecMismatch,
    /// Sample carries no bytes.
    EmptySample,
    /// Sample past the `u32` box-size budget.
    SampleTooLarge(usize),
    /// Zero sample duration.
    ZeroDuration,
    /// Zero or oversized frame dimensions.
    BadDimensions,
    /// H.264/H.265 sample holds no NALU.
    NoNalus,
    /// Composition offset past the `trun` field range.
    CtsOutOfRange,
}

impl fmt::Display for Fmp4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(inner) => write!(f, "{inner}"),
            Self::CodecMismatch => write!(f, "sample codec differs from track"),
            Self::EmptySample => write!(f, "sample carries no bytes"),
            Self::SampleTooLarge(len) => write!(f, "sample too large: {len} bytes"),
            Self::ZeroDuration => write!(f, "zero sample duration"),
            Self::BadDimensions => write!(f, "zero or oversized dimensions"),
            Self::NoNalus => write!(f, "sample holds no NALU"),
            Self::CtsOutOfRange => write!(f, "composition offset out of range"),
        }
    }
}

impl std::error::Error for Fmp4Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(inner) => Some(inner),
            _ => None,
        }
    }
}

impl From<ConfigError> for Fmp4Error {
    fn from(inner: ConfigError) -> Self {
        Self::Config(inner)
    }
}

/// H.264 decoder configuration: parameter sets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvcConfig {
    /// Sequence parameter sets, headers included.
    sps: Vec<Vec<u8>>,
    /// Picture parameter sets, headers included.
    pps: Vec<Vec<u8>>,
    /// First SPS fields.
    info: AvcSpsInfo,
}

/// H.265 decoder configuration: opaque `hvcC` record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HevcConfig {
    /// Full `HEVCDecoderConfigurationRecord` bytes.
    record: Vec<u8>,
}

/// AV1 decoder configuration: fields plus config OBUs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Av1Config {
    /// `seq_profile`.
    profile: u8,
    /// `seq_level_idx_0`.
    level: u8,
    /// `seq_tier_0`.
    tier: bool,
    /// Bit depth: 8, 10, or 12.
    bitdepth: u8,
    /// Chroma sampling flags.
    chroma: Av1Chroma,
    /// `chroma_sample_position`.
    chroma_sample_position: u8,
    /// `configOBUs` bytes.
    config_obus: Vec<u8>,
}

/// AV1 chroma sampling flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Av1Chroma {
    /// `monochrome`.
    monochrome: bool,
    /// `chroma_subsampling_x`.
    subsampling_x: bool,
    /// `chroma_subsampling_y`.
    subsampling_y: bool,
}

/// Track decoder configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackConfig {
    /// H.264.
    Avc(AvcConfig),
    /// H.265.
    Hevc(HevcConfig),
    /// AV1.
    Av1(Av1Config),
}

impl AvcConfig {
    /// Configuration from parameter sets with headers included.
    ///
    /// # Errors
    ///
    /// Returns `Err` on empty lists, an unparseable first SPS, entry
    /// counts past `avcC` limits, or entries past 65535 bytes.
    pub fn from_parameter_sets(sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Result<Self, ConfigError> {
        if sps.is_empty() || pps.is_empty() {
            return Err(ConfigError::EmptyParameterSets);
        }
        if sps.len() > 31 || pps.len() > 255 {
            return Err(ConfigError::TooManyParameterSets);
        }
        if sps.iter().chain(pps.iter()).any(|n| n.len() > 0xFFFF) {
            return Err(ConfigError::ParameterSetTooLarge);
        }
        let info = AvcSpsInfo::parse(&sps[0]).ok_or(ConfigError::BadAvcSps)?;
        Ok(Self { sps, pps, info })
    }

    /// `AVCDecoderConfigurationRecord` bytes with 4-byte NALU lengths.
    #[must_use]
    pub fn record_bytes(&self) -> Vec<u8> {
        let sps_count = u8::try_from(self.sps.len()).unwrap_or(u8::MAX);
        let pps_count = u8::try_from(self.pps.len()).unwrap_or(u8::MAX);
        let mut record = vec![
            1,
            self.info.profile,
            self.info.constraints,
            self.info.level,
            0xFF,
            0xE0 | sps_count,
        ];
        for sps in &self.sps {
            let len = u16::try_from(sps.len()).unwrap_or(u16::MAX);
            record.extend_from_slice(&len.to_be_bytes());
            record.extend_from_slice(sps);
        }
        record.push(pps_count);
        for pps in &self.pps {
            let len = u16::try_from(pps.len()).unwrap_or(u16::MAX);
            record.extend_from_slice(&len.to_be_bytes());
            record.extend_from_slice(pps);
        }
        if self.info.needs_extensions() {
            record.extend_from_slice(&[
                self.info.chroma,
                self.info.bit_depth_luma,
                self.info.bit_depth_chroma,
                0,
            ]);
        }
        record
    }
}

impl HevcConfig {
    /// Configuration from a full `hvcC` record.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the record is short, not version 1, or uses
    /// NALU lengths other than 4 bytes.
    pub fn from_hvcc_record(record: Vec<u8>) -> Result<Self, ConfigError> {
        if record.len() < 23 {
            return Err(ConfigError::BadHvcc("short record"));
        }
        if record[0] != 1 {
            return Err(ConfigError::BadHvcc("version"));
        }
        if record[22] & 0x03 != 0x03 {
            return Err(ConfigError::BadHvcc("length size"));
        }
        Ok(Self { record })
    }

    /// Record bytes.
    #[must_use]
    pub fn record_bytes(&self) -> &[u8] {
        &self.record
    }
}

impl Av1Config {
    /// Configuration parsed from a full `av1C` record.
    ///
    /// # Errors
    ///
    /// Returns `Err` on short records, bad marker/version, or reserved
    /// bits set.
    pub fn from_av1c_record(record: &[u8]) -> Result<Self, ConfigError> {
        if record.len() < 4 {
            return Err(ConfigError::BadAv1c("short record"));
        }
        if record[0] != 0x81 {
            return Err(ConfigError::BadAv1c("marker/version"));
        }
        if record[3] & 0xE0 != 0 {
            return Err(ConfigError::BadAv1c("reserved bits"));
        }
        let high = record[2] & 0x40 != 0;
        let twelve = record[2] & 0x20 != 0;
        Ok(Self {
            profile: (record[1] >> 5) & 0x07,
            level: record[1] & 0x1F,
            tier: record[2] & 0x80 != 0,
            bitdepth: match (high, twelve) {
                (false, _) => 8,
                (true, false) => 10,
                (true, true) => 12,
            },
            chroma: Av1Chroma {
                monochrome: record[2] & 0x10 != 0,
                subsampling_x: record[2] & 0x08 != 0,
                subsampling_y: record[2] & 0x04 != 0,
            },
            chroma_sample_position: record[2] & 0x03,
            config_obus: record[4..].to_vec(),
        })
    }

    /// `AV1CodecConfigurationRecord` bytes.
    #[must_use]
    pub fn record_bytes(&self) -> Vec<u8> {
        let mut record = vec![
            0x81,
            (self.profile << 5) | (self.level & 0x1F),
            (u8::from(self.tier) << 7)
                | (u8::from(self.bitdepth > 8) << 6)
                | (u8::from(self.bitdepth == 12) << 5)
                | (u8::from(self.chroma.monochrome) << 4)
                | (u8::from(self.chroma.subsampling_x) << 3)
                | (u8::from(self.chroma.subsampling_y) << 2)
                | (self.chroma_sample_position & 0x03),
            0,
        ];
        record.extend_from_slice(&self.config_obus);
        record
    }
}

impl TrackConfig {
    /// Track codec.
    #[must_use]
    pub fn codec(&self) -> Codec {
        match self {
            Self::Avc(_) => Codec::H264,
            Self::Hevc(_) => Codec::H265,
            Self::Av1(_) => Codec::Av1,
        }
    }

    /// Decoder configuration record bytes.
    #[must_use]
    fn record_bytes(&self) -> Vec<u8> {
        match self {
            Self::Avc(config) => config.record_bytes(),
            Self::Hevc(config) => config.record_bytes().to_vec(),
            Self::Av1(config) => config.record_bytes(),
        }
    }

    /// `ftyp` compatible brands for the track codec.
    fn brands(&self) -> [[u8; 4]; 6] {
        let codec = match self {
            Self::Avc(_) => *b"avc1",
            Self::Hevc(_) => *b"hvc1",
            Self::Av1(_) => *b"av01",
        };
        [*b"isom", *b"iso2", codec, *b"cmfc", *b"iso6", *b"mp41"]
    }
}

/// Live fragmented-MP4 mux: init segment plus one-sample CMAF chunks.
#[derive(Debug)]
pub struct FragmentMux {
    /// Track configuration.
    track: TrackConfig,
    /// Frame width in pixels.
    width: u16,
    /// Frame height in pixels.
    height: u16,
    /// Next fragment sequence number.
    sequence: u32,
    /// Decode clock unwrapped past the 33-bit wrap.
    tracker: ClockTracker,
}

/// Big-endian box byte sink.
#[derive(Debug, Default)]
struct ByteSink {
    /// Emitted bytes.
    buf: Vec<u8>,
}

impl FragmentMux {
    /// Media timescale in ticks per second.
    const TIMESCALE: u32 = 90_000;

    /// Mux for one video track.
    ///
    /// # Errors
    ///
    /// Returns `Err` on zero or oversized dimensions.
    pub fn new(track: TrackConfig, width: u32, height: u32) -> Result<Self, Fmp4Error> {
        let width = u16::try_from(width).map_err(|_| Fmp4Error::BadDimensions)?;
        let height = u16::try_from(height).map_err(|_| Fmp4Error::BadDimensions)?;
        if width == 0 || height == 0 {
            return Err(Fmp4Error::BadDimensions);
        }
        Ok(Self {
            track,
            width,
            height,
            sequence: 1,
            tracker: ClockTracker::new(),
        })
    }

    /// Next fragment sequence number.
    #[must_use]
    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// Initialization segment: `ftyp` plus `moov`.
    #[must_use]
    pub fn init_segment(&self) -> Vec<u8> {
        let mut sink = ByteSink::default();
        sink.ftyp(&self.track);
        sink.moov(&self.track, self.width, self.height);
        sink.finish()
    }

    /// Segment header: `styp` with the CMAF brand.
    #[must_use]
    pub fn start_segment() -> Vec<u8> {
        let mut sink = ByteSink::default();
        let at = sink.open(*b"styp");
        sink.tag(*b"cmfc");
        sink.u32(0);
        sink.tag(*b"cmfc");
        sink.close(at);
        sink.finish()
    }

    /// One access unit as a CMAF chunk: `moof` plus `mdat`.
    ///
    /// H.264/H.265 payloads convert annex-B to 4-byte lengths; AV1
    /// payloads pass through untouched.
    ///
    /// # Arguments
    ///
    /// * `unit` - access unit to fragment.
    /// * `duration_90k` - sample duration in 90 kHz ticks.
    /// * `gate` - keyframe gate; a held unit advances nothing.
    /// * `out` - chunk bytes sink.
    ///
    /// # Errors
    ///
    /// Returns `Err` on codec mismatch, empty samples, oversized
    /// samples, zero duration, and NALU-less H.264/H.265 payloads.
    pub fn push_sample(
        &mut self,
        unit: &AccessUnit,
        duration_90k: u32,
        gate: Option<&KeyframeGate>,
        out: &mut Vec<u8>,
    ) -> Result<PacketAction, Fmp4Error> {
        let keyframe = unit.is_keyframe();
        if gate.is_some_and(|g| g.poll(keyframe) == KeyframeState::AwaitingKeyframe) {
            return Ok(PacketAction::HeldForKeyframe);
        }
        if unit.codec() != self.track.codec() {
            return Err(Fmp4Error::CodecMismatch);
        }
        if unit.payload().is_empty() {
            return Err(Fmp4Error::EmptySample);
        }
        if duration_90k == 0 {
            return Err(Fmp4Error::ZeroDuration);
        }
        let sample = Self::sample_bytes(unit)?;
        if u64::try_from(sample.len()).unwrap_or(u64::MAX) > u64::from(u32::MAX) - 8 {
            return Err(Fmp4Error::SampleTooLarge(sample.len()));
        }
        let decode_time = self.tracker.observe(unit.dts());
        let raw_cts = Ts90k::new(unit.dts()).forward_distance(Ts90k::new(unit.pts()));
        let cts = if raw_cts > STC_WRAP / 2 {
            -i64::try_from(STC_WRAP - raw_cts).unwrap_or(i64::MAX)
        } else {
            i64::try_from(raw_cts).unwrap_or(i64::MAX)
        };
        if (cts < 0 && i32::try_from(cts).is_err()) || (cts >= 0 && u32::try_from(cts).is_err()) {
            return Err(Fmp4Error::CtsOutOfRange);
        }
        let start = out.len();
        let mut sink = ByteSink::default();
        sink.moof(
            self.sequence,
            decode_time,
            duration_90k,
            &sample,
            cts,
            keyframe,
        );
        sink.mdatsample(&sample);
        out.extend_from_slice(&sink.finish());
        self.sequence += 1;
        Ok(PacketAction::Emitted(out.len() - start))
    }

    /// Sample bytes for the track codec.
    fn sample_bytes(unit: &AccessUnit) -> Result<Vec<u8>, Fmp4Error> {
        if unit.codec() == Codec::Av1 {
            return Ok(unit.payload().to_vec());
        }
        let mut sample = Vec::with_capacity(unit.payload().len());
        for nalu in AnnexB::new(unit.payload()) {
            let len = u32::try_from(nalu.len()).unwrap_or(u32::MAX);
            sample.extend_from_slice(&len.to_be_bytes());
            sample.extend_from_slice(nalu);
        }
        (!sample.is_empty())
            .then_some(sample)
            .ok_or(Fmp4Error::NoNalus)
    }
}

impl ByteSink {
    /// One byte.
    fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    /// Big-endian 16-bit integer.
    fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Low 24 bits big-endian.
    fn u24(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes()[1..]);
    }

    /// Big-endian 32-bit integer.
    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Big-endian 64-bit integer.
    fn u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Big-endian 32-bit signed integer.
    fn i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    /// Four-character code.
    fn tag(&mut self, tag: [u8; 4]) {
        self.buf.extend_from_slice(&tag);
    }

    /// Raw bytes.
    fn bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Open box; returns the size-field offset for [`ByteSink::close`].
    fn open(&mut self, tag: [u8; 4]) -> usize {
        let at = self.buf.len();
        self.u32(0);
        self.tag(tag);
        at
    }

    /// Backpatch the size opened at `at`.
    ///
    /// Sizes past `u32::MAX` saturate; every caller caps inputs below it.
    fn close(&mut self, at: usize) {
        let size = u32::try_from(self.buf.len() - at).unwrap_or(u32::MAX);
        self.buf[at..at + 4].copy_from_slice(&size.to_be_bytes());
    }

    /// Full box header: open, version, flags.
    fn full(&mut self, tag: [u8; 4], version: u8, flags: u32) -> usize {
        let at = self.open(tag);
        self.u8(version);
        self.u24(flags);
        at
    }

    /// Finished bytes.
    fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// File-type box.
    fn ftyp(&mut self, track: &TrackConfig) {
        let at = self.open(*b"ftyp");
        self.tag(*b"isom");
        self.u32(0x200);
        for brand in track.brands() {
            self.tag(brand);
        }
        self.close(at);
    }

    /// Movie box with one video track and fragment extensions.
    fn moov(&mut self, track: &TrackConfig, width: u16, height: u16) {
        let at = self.open(*b"moov");
        self.mvhd();
        self.trak(track, width, height);
        let mvex = self.open(*b"mvex");
        let trex = self.full(*b"trex", 0, 0);
        self.u32(1);
        self.u32(1);
        self.u32(0);
        self.u32(0);
        self.u32(0);
        self.close(trex);
        self.close(mvex);
        self.close(at);
    }

    /// Movie header: 1000 Hz clock, unknown duration.
    fn mvhd(&mut self) {
        let at = self.full(*b"mvhd", 0, 0);
        self.u32(0);
        self.u32(0);
        self.u32(1000);
        self.u32(0);
        self.u32(0x0001_0000);
        self.u16(0x0100);
        self.u16(0);
        self.u32(0);
        self.u32(0);
        Self::matrix(self);
        for _ in 0..6 {
            self.u32(0);
        }
        self.u32(2);
        self.close(at);
    }

    /// Identity display matrix.
    fn matrix(&mut self) {
        for value in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            self.u32(value);
        }
    }

    /// Track box.
    fn trak(&mut self, track: &TrackConfig, width: u16, height: u16) {
        let at = self.open(*b"trak");
        let tkhd = self.full(*b"tkhd", 0, 0x07);
        self.u32(0);
        self.u32(0);
        self.u32(1);
        self.u32(0);
        self.u32(0);
        self.u32(0);
        self.u32(0);
        self.u16(0);
        self.u16(0);
        self.u16(0);
        self.u16(0);
        Self::matrix(self);
        self.u32(u32::from(width) << 16);
        self.u32(u32::from(height) << 16);
        self.close(tkhd);
        let mdia = self.open(*b"mdia");
        let mdhd = self.full(*b"mdhd", 0, 0);
        self.u32(0);
        self.u32(0);
        self.u32(FragmentMux::TIMESCALE);
        self.u32(0);
        self.u16(0x55C4);
        self.u16(0);
        self.close(mdhd);
        let hdlr = self.full(*b"hdlr", 0, 0);
        self.u32(0);
        self.tag(*b"vide");
        self.u32(0);
        self.u32(0);
        self.u32(0);
        self.bytes(b"VideoHandler\0");
        self.close(hdlr);
        self.minf(track, width, height);
        self.close(mdia);
        self.close(at);
    }

    /// Media information box.
    fn minf(&mut self, track: &TrackConfig, width: u16, height: u16) {
        let at = self.open(*b"minf");
        let vmhd = self.full(*b"vmhd", 0, 1);
        self.u16(0);
        self.u16(0);
        self.u16(0);
        self.u16(0);
        self.close(vmhd);
        let dinf = self.open(*b"dinf");
        let dref = self.full(*b"dref", 0, 0);
        self.u32(1);
        let url = self.full(*b"url ", 0, 1);
        self.close(url);
        self.close(dref);
        self.close(dinf);
        self.stbl(track, width, height);
        self.close(at);
    }

    /// Sample table with one entry and empty run tables.
    fn stbl(&mut self, track: &TrackConfig, width: u16, height: u16) {
        let at = self.open(*b"stbl");
        let entries = self.full(*b"stsd", 0, 0);
        self.u32(1);
        self.sample_entry(track, width, height);
        self.close(entries);
        for tag in [*b"stts", *b"stsc"] {
            let empty = self.full(tag, 0, 0);
            self.u32(0);
            self.close(empty);
        }
        let sizes = self.full(*b"stsz", 0, 0);
        self.u32(0);
        self.u32(0);
        self.close(sizes);
        let offsets = self.full(*b"stco", 0, 0);
        self.u32(0);
        self.close(offsets);
        self.close(at);
    }

    /// Visual sample entry with its decoder configuration.
    fn sample_entry(&mut self, track: &TrackConfig, width: u16, height: u16) {
        let at = self.open(track.codec().sample_entry());
        self.bytes(&[0; 6]);
        self.u16(1);
        self.u16(0);
        self.u16(0);
        self.u32(0);
        self.u32(0);
        self.u32(0);
        self.u16(width);
        self.u16(height);
        self.u32(0x0048_0000);
        self.u32(0x0048_0000);
        self.u32(0);
        self.u16(1);
        self.bytes(&[0; 32]);
        self.u16(0x0018);
        self.u16(0xFFFF);
        let config_tag = match track.codec() {
            Codec::H264 => *b"avcC",
            Codec::H265 => *b"hvcC",
            Codec::Av1 => *b"av1C",
        };
        let config = self.open(config_tag);
        self.bytes(&track.record_bytes());
        self.close(config);
        self.close(at);
    }

    /// Movie fragment box: header, track run, decode time.
    fn moof(
        &mut self,
        sequence: u32,
        decode_time: u64,
        duration: u32,
        sample: &[u8],
        cts: i64,
        keyframe: bool,
    ) {
        let at = self.open(*b"moof");
        let mfhd = self.full(*b"mfhd", 0, 0);
        self.u32(sequence);
        self.close(mfhd);
        let traf = self.open(*b"traf");
        let tfhd = self.full(*b"tfhd", 0, 0x02_0000);
        self.u32(1);
        self.close(tfhd);
        let tfdt = self.full(*b"tfdt", 1, 0);
        self.u64(decode_time);
        self.close(tfdt);
        let with_cts = cts != 0;
        let version = u8::from(cts < 0);
        let mut flags = 0x01 | 0x100 | 0x200 | 0x400;
        if with_cts {
            flags |= 0x800;
        }
        let trun = self.full(*b"trun", version, flags);
        self.u32(1);
        let cts_len = usize::from(with_cts) * 4;
        let data_offset = self.buf.len() - at + 4 + 12 + cts_len + 8;
        self.i32(i32::try_from(data_offset).unwrap_or(i32::MAX));
        self.u32(duration);
        let size = u32::try_from(sample.len()).unwrap_or(u32::MAX);
        self.u32(size);
        self.u32(if keyframe { 0x0200_0000 } else { 0x0101_0000 });
        if with_cts {
            if cts < 0 {
                self.i32(i32::try_from(cts).unwrap_or(i32::MIN));
            } else {
                self.u32(u32::try_from(cts).unwrap_or(u32::MAX));
            }
        }
        self.close(trun);
        self.close(traf);
        self.close(at);
    }

    /// Media data box holding one sample.
    fn mdatsample(&mut self, sample: &[u8]) {
        let at = self.open(*b"mdat");
        self.bytes(sample);
        self.close(at);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Av1Config, AvcConfig, ConfigError, Fmp4Error, FragmentMux, HevcConfig, TrackConfig,
    };
    use crate::codec::{AccessUnit, Codec, KeyframeGate, PacketAction};

    /// Baseline SPS plus PPS with headers.
    fn baseline_sets() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        (
            vec![vec![0x67, 0x42, 0x00, 0x1E, 0xF4, 0x02, 0x80, 0x2D, 0xC8]],
            vec![vec![0x68, 0xCE, 0x38, 0x80]],
        )
    }

    /// IDR plus P-slice access unit.
    fn annexb_idr() -> Vec<u8> {
        vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB, 0, 0, 0, 1, 0x41, 0xCC]
    }

    #[test]
    fn avcc_record_matches_layout() {
        let (sps, pps) = baseline_sets();
        let config = AvcConfig::from_parameter_sets(sps, pps).expect("valid sets");
        let record = config.record_bytes();
        assert_eq!(&record[..6], &[1, 0x42, 0x00, 0x1E, 0xFF, 0xE1]);
        assert_eq!(u16::from_be_bytes([record[6], record[7]]), 9);
        assert_eq!(record[17], 1);
        assert_eq!(u16::from_be_bytes([record[18], record[19]]), 4);
        assert_eq!(record.len(), 6 + 2 + 9 + 1 + 2 + 4);
    }

    #[test]
    fn avc_config_rejects_bad_sets() {
        assert_eq!(
            AvcConfig::from_parameter_sets(Vec::new(), vec![vec![1]]).unwrap_err(),
            ConfigError::EmptyParameterSets
        );
        assert_eq!(
            AvcConfig::from_parameter_sets(vec![vec![0x67]], vec![vec![1]]).unwrap_err(),
            ConfigError::BadAvcSps
        );
        let big = vec![0u8; 0x1_0000];
        let (mut sps, pps) = baseline_sets();
        sps.push(big);
        assert_eq!(
            AvcConfig::from_parameter_sets(sps, pps).unwrap_err(),
            ConfigError::ParameterSetTooLarge
        );
    }

    #[test]
    fn hvcc_validates_shape() {
        let mut record = vec![0u8; 23];
        record[0] = 1;
        record[22] = 0x03;
        assert!(HevcConfig::from_hvcc_record(record.clone()).is_ok());
        assert_eq!(
            HevcConfig::from_hvcc_record(vec![1, 2]).unwrap_err(),
            ConfigError::BadHvcc("short record")
        );
        record[0] = 2;
        assert_eq!(
            HevcConfig::from_hvcc_record(record.clone()).unwrap_err(),
            ConfigError::BadHvcc("version")
        );
        record[0] = 1;
        record[22] = 0x00;
        assert_eq!(
            HevcConfig::from_hvcc_record(record).unwrap_err(),
            ConfigError::BadHvcc("length size")
        );
    }

    #[test]
    fn av1c_round_trips() {
        let record = [0x81, 0x08, 0x0C, 0x00, 0x0A, 0x0B, 0x00, 0x00];
        let config = Av1Config::from_av1c_record(&record).expect("valid record");
        assert_eq!(config.record_bytes(), record);
        assert_eq!(
            Av1Config::from_av1c_record(&[0x00, 0x08, 0x0C]).unwrap_err(),
            ConfigError::BadAv1c("short record")
        );
        assert_eq!(
            Av1Config::from_av1c_record(&[0x00, 0x08, 0x0C, 0x00]).unwrap_err(),
            ConfigError::BadAv1c("marker/version")
        );
    }

    #[test]
    fn init_segment_opens_with_ftyp_moov() {
        let (sps, pps) = baseline_sets();
        let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid"));
        let mux = FragmentMux::new(track, 320, 240).expect("valid dims");
        let init = mux.init_segment();
        assert_eq!(&init[4..8], b"ftyp");
        let ftyp_len =
            usize::try_from(u32::from_be_bytes(init[..4].try_into().expect("len"))).expect("len");
        assert_eq!(&init[ftyp_len + 4..ftyp_len + 8], b"moov");
        assert!(init.windows(4).any(|w| w == b"avcC"));
    }

    #[test]
    fn chunk_carries_sample_with_keyframe_flags() {
        let (sps, pps) = baseline_sets();
        let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid"));
        let mut mux = FragmentMux::new(track, 320, 240).expect("valid dims");
        let unit = AccessUnit::classified(Codec::H264, annexb_idr(), 3000, 0);
        let mut out = Vec::new();
        let action = mux
            .push_sample(&unit, 3000, None, &mut out)
            .expect("fragment");
        assert!(matches!(action, PacketAction::Emitted(n) if n == out.len()));
        assert_eq!(&out[4..8], b"moof");
        let moof_len =
            usize::try_from(u32::from_be_bytes(out[..4].try_into().expect("len"))).expect("len");
        assert_eq!(&out[moof_len + 4..moof_len + 8], b"mdat");
        assert!(out.windows(4).any(|w| w == b"trun"));
        assert!(out.windows(4).any(|w| w == [0x02, 0x00, 0x00, 0x00]));
        assert_eq!(mux.sequence(), 2);
    }

    #[test]
    fn negative_cts_selects_trun_v1() {
        let (sps, pps) = baseline_sets();
        let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid"));
        let mut mux = FragmentMux::new(track, 320, 240).expect("valid dims");
        let unit = AccessUnit::classified(Codec::H264, annexb_idr(), 0, 3000);
        let mut out = Vec::new();
        mux.push_sample(&unit, 3000, None, &mut out)
            .expect("fragment");
        let at = out.windows(4).position(|w| w == b"trun").expect("trun");
        assert_eq!(out[at + 4], 1);
        assert_eq!(&out[at + 28..at + 32], &(-3000i32).to_be_bytes());
    }

    #[test]
    fn push_rejects_mismatch_and_empty() {
        let (sps, pps) = baseline_sets();
        let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid"));
        let mut mux = FragmentMux::new(track, 320, 240).expect("valid dims");
        let mut out = Vec::new();
        let hevc = AccessUnit::new(Codec::H265, vec![0, 0, 0, 1, 0x26], 0, 0, true);
        assert_eq!(
            mux.push_sample(&hevc, 3000, None, &mut out).unwrap_err(),
            Fmp4Error::CodecMismatch
        );
        let empty = AccessUnit::new(Codec::H264, Vec::new(), 0, 0, true);
        assert_eq!(
            mux.push_sample(&empty, 3000, None, &mut out).unwrap_err(),
            Fmp4Error::EmptySample
        );
        let unit = AccessUnit::classified(Codec::H264, annexb_idr(), 0, 0);
        assert_eq!(
            mux.push_sample(&unit, 0, None, &mut out).unwrap_err(),
            Fmp4Error::ZeroDuration
        );
        let naluless = AccessUnit::new(Codec::H264, vec![0xAA, 0xBB], 0, 0, false);
        assert_eq!(
            mux.push_sample(&naluless, 3000, None, &mut out)
                .unwrap_err(),
            Fmp4Error::NoNalus
        );
        assert!(out.is_empty());
        assert_eq!(mux.sequence(), 1);
    }

    #[test]
    fn gate_holds_non_keyframe_sample() {
        let (sps, pps) = baseline_sets();
        let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid"));
        let mut mux = FragmentMux::new(track, 320, 240).expect("valid dims");
        let gate = KeyframeGate::new();
        gate.request();
        let inter = AccessUnit::new(Codec::H264, vec![0, 0, 0, 1, 0x41, 0xCC], 3000, 3000, false);
        let mut out = Vec::new();
        assert_eq!(
            mux.push_sample(&inter, 3000, Some(&gate), &mut out)
                .expect("polled"),
            PacketAction::HeldForKeyframe
        );
        assert!(out.is_empty());
        assert_eq!(mux.sequence(), 1);
    }

    #[test]
    fn mux_rejects_bad_dimensions() {
        let (sps, pps) = baseline_sets();
        let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid"));
        assert_eq!(
            FragmentMux::new(track.clone(), 0, 240).unwrap_err(),
            Fmp4Error::BadDimensions
        );
        assert_eq!(
            FragmentMux::new(track, 320, 0x1_0000).unwrap_err(),
            Fmp4Error::BadDimensions
        );
    }
}
