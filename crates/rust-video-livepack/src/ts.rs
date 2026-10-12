//! MPEG-TS mux and demux for H.264/H.265.
//!
//! AV1/VP9/VP8/AV2 have no registered `stream_type` and decline with
//! [`TsMuxError::CodecNotMappable`]; the demuxer skips their PMT
//! entries as unknown.
//!
//! [ITU-T H.222.0](https://www.itu.int/rec/T-REC-H.222.0) section 2.4.

use std::fmt;

use crate::codec::{AccessUnit, Codec, KeyframeGate, KeyframeState, PacketAction};
use crate::time::{Pcr, Ts90k};

/// Sync byte starting every packet; shared by [`TsMux`] and [`TsDemux`].
const SYNC_BYTE: u8 = 0x47;
/// Packet length in bytes; shared by [`TsMux`] and [`TsDemux`].
const PACKET_SIZE: usize = 188;
/// Largest section the demuxer reassembles.
const MAX_SECTION_SIZE: usize = 4096;
/// First assignable PID.
const FIRST_PID: u16 = 0x0020;
/// Last assignable PID.
const LAST_PID: u16 = 0x1FFA;
/// Null-packet PID.
const NULL_PID: u16 = 0x1FFF;

/// Mux configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TsMuxConfig {
    /// Program map table PID.
    pub pmt_pid: u16,
    /// Video elementary stream PID.
    pub video_pid: u16,
    /// Program number carried in PAT/PMT.
    pub program_number: u16,
    /// Transport stream id carried in PAT.
    pub transport_stream_id: u16,
    /// Largest PES payload accepted per access unit.
    pub max_pes_bytes: usize,
}

impl Default for TsMuxConfig {
    fn default() -> Self {
        Self {
            pmt_pid: 0x1000,
            video_pid: 0x0100,
            program_number: 1,
            transport_stream_id: 1,
            max_pes_bytes: 4 << 20,
        }
    }
}

/// Mux failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TsMuxError {
    /// PID outside `0x0020..=0x1FFA`, or PMT equals video PID.
    InvalidPid(u16),
    /// Codec has no MPEG-TS `stream_type` (AV1, VP9, VP8, AV2).
    CodecNotMappable(Codec),
    /// Payload exceeds `max_pes_bytes`.
    PayloadTooLarge(usize),
    /// Access unit carries no bytes.
    EmptyPayload,
}

impl fmt::Display for TsMuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPid(pid) => write!(f, "invalid PID {pid:#X}"),
            Self::CodecNotMappable(codec) => write!(f, "{codec:?} has no TS stream type"),
            Self::PayloadTooLarge(len) => write!(f, "PES payload too large: {len} bytes"),
            Self::EmptyPayload => write!(f, "access unit carries no bytes"),
        }
    }
}

impl std::error::Error for TsMuxError {}

/// Demux failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TsDemuxError {
    /// Section CRC mismatch on `pid`.
    BadCrc {
        /// PID carrying the corrupt section.
        pid: u16,
    },
    /// Continuity counter gap on `pid`.
    Discontinuity {
        /// PID with the gap.
        pid: u16,
        /// Expected counter.
        expected: u8,
        /// Received counter.
        got: u8,
    },
    /// PES payload past the configured cap.
    PesTooLarge {
        /// Accumulated size when rejected.
        size: usize,
    },
    /// Malformed packet or PES header; carries the cause.
    BadPacket(&'static str),
    /// Section past 4096 bytes.
    SectionTooLarge,
}

impl fmt::Display for TsDemuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadCrc { pid } => write!(f, "section CRC mismatch on PID {pid:#X}"),
            Self::Discontinuity { pid, expected, got } => {
                write!(f, "PID {pid:#X} continuity {expected} != {got}")
            }
            Self::PesTooLarge { size } => write!(f, "PES payload too large: {size} bytes"),
            Self::BadPacket(cause) => write!(f, "malformed packet: {cause}"),
            Self::SectionTooLarge => write!(f, "section past 4096 bytes"),
        }
    }
}

impl std::error::Error for TsDemuxError {}

/// Single-program MPEG-TS mux: PAT/PMT, PES, PCR, continuity.
#[derive(Debug)]
pub struct TsMux {
    /// Validated configuration.
    config: TsMuxConfig,
    /// PAT continuity counter.
    pat_cc: u8,
    /// PMT continuity counter.
    pmt_cc: u8,
    /// Video continuity counter.
    video_cc: u8,
    /// Tables already emitted once.
    started: bool,
}

impl TsMux {
    /// PAT table id.
    const PAT_TID: u8 = 0x00;
    /// PMT table id.
    const PMT_TID: u8 = 0x02;
    /// Video PES stream id.
    const VIDEO_SID: u8 = 0xE0;

    /// Mux over `config`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a PID is unassignable or both PIDs match.
    pub fn new(config: TsMuxConfig) -> Result<Self, TsMuxError> {
        for pid in [config.pmt_pid, config.video_pid] {
            if !(FIRST_PID..=LAST_PID).contains(&pid) {
                return Err(TsMuxError::InvalidPid(pid));
            }
        }
        if config.pmt_pid == config.video_pid {
            return Err(TsMuxError::InvalidPid(config.pmt_pid));
        }
        Ok(Self {
            config,
            pat_cc: 0,
            pmt_cc: 0,
            video_cc: 0,
            started: false,
        })
    }

    /// Packetize one access unit into `out`.
    ///
    /// Emits PAT/PMT before the first and every keyframe unit, stamps PCR
    /// from DTS on the first packet, and sets `random_access_indicator`
    /// on keyframes.
    ///
    /// # Arguments
    ///
    /// * `unit` - access unit to packetize.
    /// * `gate` - keyframe gate; a held unit appends nothing.
    /// * `out` - packet bytes sink.
    ///
    /// # Errors
    ///
    /// Returns `Err` on AV1/VP9/VP8/AV2 payloads, empty payloads, and
    /// payloads past `max_pes_bytes`.
    pub fn push(
        &mut self,
        unit: &AccessUnit,
        gate: Option<&KeyframeGate>,
        out: &mut Vec<u8>,
    ) -> Result<PacketAction, TsMuxError> {
        let keyframe = unit.is_keyframe();
        if gate.is_some_and(|g| g.poll(keyframe) == KeyframeState::AwaitingKeyframe) {
            return Ok(PacketAction::HeldForKeyframe);
        }
        let stream_type = unit
            .codec()
            .ts_stream_type()
            .ok_or(TsMuxError::CodecNotMappable(unit.codec()))?;
        if unit.payload().is_empty() {
            return Err(TsMuxError::EmptyPayload);
        }
        if unit.payload().len() > self.config.max_pes_bytes {
            return Err(TsMuxError::PayloadTooLarge(unit.payload().len()));
        }
        let start = out.len();
        if !self.started || keyframe {
            let pat = self.pat_section();
            self.packetize_section(0x0000, &pat, out);
            let pmt = self.pmt_section(stream_type);
            let pmt_pid = self.config.pmt_pid;
            self.packetize_section(pmt_pid, &pmt, out);
            self.started = true;
        }
        self.packetize_pes(unit, keyframe, out);
        Ok(PacketAction::Emitted(out.len() - start))
    }

    /// Program association section with CRC.
    fn pat_section(&self) -> Vec<u8> {
        let tsid = self.config.transport_stream_id.to_be_bytes();
        let program = self.config.program_number.to_be_bytes();
        let pmt = self.config.pmt_pid.to_be_bytes();
        let mut section = vec![
            Self::PAT_TID,
            0xB0,
            0x0D,
            tsid[0],
            tsid[1],
            0xC1,
            0x00,
            0x00,
            program[0],
            program[1],
            0xE0 | (pmt[0] & 0x1F),
            pmt[1],
        ];
        section.extend_from_slice(&Mpeg2Crc::checksum(&section).to_be_bytes());
        section
    }

    /// Program map section with CRC for one video stream.
    fn pmt_section(&self, stream_type: u8) -> Vec<u8> {
        let program = self.config.program_number.to_be_bytes();
        let video = self.config.video_pid.to_be_bytes();
        let mut section = vec![
            Self::PMT_TID,
            0xB0,
            0x12,
            program[0],
            program[1],
            0xC1,
            0x00,
            0x00,
            0xE0 | (video[0] & 0x1F),
            video[1],
            0xF0,
            0x00,
            stream_type,
            0xE0 | (video[0] & 0x1F),
            video[1],
            0xF0,
            0x00,
        ];
        section.extend_from_slice(&Mpeg2Crc::checksum(&section).to_be_bytes());
        section
    }

    /// Section bytes wrapped in 188-byte packets.
    fn packetize_section(&mut self, pid: u16, section: &[u8], out: &mut Vec<u8>) {
        let pid_bytes = pid.to_be_bytes();
        let mut rest = section;
        let mut first = true;
        while !rest.is_empty() || first {
            let counter = if pid == 0 {
                &mut self.pat_cc
            } else {
                &mut self.pmt_cc
            };
            let mut packet = [0xFF; PACKET_SIZE];
            packet[0] = SYNC_BYTE;
            packet[1] = (u8::from(first) << 6) | (pid_bytes[0] & 0x1F);
            packet[2] = pid_bytes[1];
            packet[3] = 0x10 | (*counter & 0x0F);
            let mut pos = 4;
            if first {
                packet[pos] = 0;
                pos += 1;
            }
            let room = PACKET_SIZE - pos;
            let take = rest.len().min(room);
            packet[pos..pos + take].copy_from_slice(&rest[..take]);
            rest = &rest[take..];
            out.extend_from_slice(&packet);
            *counter = (*counter + 1) & 0x0F;
            first = false;
        }
    }

    /// PES packet wrapped in video packets with PCR on the first.
    ///
    /// Short final packets stuff via the adaptation field; payload
    /// stuffing corrupts unbounded PES lengths.
    fn packetize_pes(&mut self, unit: &AccessUnit, keyframe: bool, out: &mut Vec<u8>) {
        let pes = Self::pes_bytes(unit);
        let pid = self.config.video_pid.to_be_bytes();
        let mut rest = pes.as_slice();
        let mut first = true;
        while !rest.is_empty() {
            let mut packet = [0xFF; PACKET_SIZE];
            packet[0] = SYNC_BYTE;
            packet[1] = (u8::from(first) << 6) | (pid[0] & 0x1F);
            packet[2] = pid[1];
            let adapt = if first {
                183 - rest.len().min(176)
            } else if rest.len() < 183 {
                183 - rest.len()
            } else {
                0
            };
            let mut pos = 4;
            if adapt > 0 {
                packet[3] = 0x30 | (self.video_cc & 0x0F);
                packet[4] = u8::try_from(adapt).unwrap_or(u8::MAX);
                packet[5] = (u8::from(first) * 0x10) | (u8::from(first && keyframe) << 6);
                if first {
                    let pcr = Pcr::from_base_ext(Ts90k::new(unit.dts()), 0);
                    packet[6..12].copy_from_slice(&pcr.bytes());
                }
                pos = 5 + adapt;
            } else {
                packet[3] = 0x10 | (self.video_cc & 0x0F);
            }
            let room = PACKET_SIZE - pos;
            let take = rest.len().min(room);
            packet[pos..pos + take].copy_from_slice(&rest[..take]);
            rest = &rest[take..];
            self.video_cc = (self.video_cc + 1) & 0x0F;
            out.extend_from_slice(&packet);
            first = false;
        }
    }

    /// PES packet for one access unit.
    fn pes_bytes(unit: &AccessUnit) -> Vec<u8> {
        let with_dts = unit.dts() != unit.pts();
        let header_len: u8 = if with_dts { 10 } else { 5 };
        let mut pes = Vec::with_capacity(9 + usize::from(header_len) + unit.payload().len());
        pes.extend_from_slice(&[0, 0, 1, Self::VIDEO_SID, 0, 0, 0x85]);
        pes.push(if with_dts { 0xC0 } else { 0x80 });
        pes.push(header_len);
        pes.extend_from_slice(&Ts90k::new(unit.pts()).field_bytes(0x2));
        if with_dts {
            pes.extend_from_slice(&Ts90k::new(unit.dts()).field_bytes(0x1));
        }
        pes.extend_from_slice(unit.payload());
        let len = pes.len() - 6;
        if len <= 0xFFFF {
            let len = u16::try_from(len).unwrap_or(u16::MAX);
            pes[4..6].copy_from_slice(&len.to_be_bytes());
        }
        pes
    }
}

/// MPEG-2 section CRC: poly `0x04C11DB7`, init `0xFFFF_FFFF`.
///
/// H.222.0 section 2.4.4.1.
#[derive(Debug)]
struct Mpeg2Crc {
    /// Shift register.
    crc: u32,
}

impl Mpeg2Crc {
    /// Checksum over `bytes`.
    fn checksum(bytes: &[u8]) -> u32 {
        let mut crc = Self { crc: 0xFFFF_FFFF };
        for &byte in bytes {
            crc.push_byte(byte);
        }
        crc.crc
    }

    /// One byte through the register.
    fn push_byte(&mut self, byte: u8) {
        for i in (0..8).rev() {
            let bit = u32::from((byte >> i) & 1);
            let top = (self.crc >> 31) & 1;
            self.crc = (self.crc << 1) ^ ((top ^ bit) * 0x04C1_1DB7);
        }
    }
}

/// Streaming single-program MPEG-TS demux: tables, PES, PCR.
#[derive(Debug)]
pub struct TsDemux {
    /// Largest PES payload reassembled.
    max_pes_bytes: usize,
    /// Unprocessed tail from the previous push.
    stash: Vec<u8>,
    /// PMT PID learned from PAT.
    pmt_pid: Option<u16>,
    /// Video PID and codec learned from PMT.
    video: Option<(u16, Codec)>,
    /// Last continuity counter per PID.
    cc: [Option<u8>; 8192],
    /// PAT reassembly.
    pat: SectionAsm,
    /// PMT reassembly.
    pmt: SectionAsm,
    /// Video PES reassembly.
    pes: PesAsm,
    /// Most recent PCR.
    last_pcr: Option<Pcr>,
}

/// One section reassembled across packets.
#[derive(Debug, Default)]
struct SectionAsm {
    /// Accumulated bytes.
    bytes: Vec<u8>,
    /// Total length once the header lands.
    total: Option<usize>,
}

/// One PES packet reassembled across packets.
#[derive(Debug, Default)]
struct PesAsm {
    /// Accumulated payload bytes.
    payload: Vec<u8>,
    /// PTS from the PES header.
    pts: u64,
    /// DTS from the PES header.
    dts: u64,
    /// Payload bytes still expected; `None` when unbounded.
    remaining: Option<usize>,
    /// Header seen for the current packet.
    open: bool,
}

impl TsDemux {
    /// Demux capping PES payloads at `max_pes_bytes`.
    #[must_use]
    pub fn new(max_pes_bytes: usize) -> Self {
        Self {
            max_pes_bytes,
            stash: Vec::new(),
            pmt_pid: None,
            video: None,
            cc: [None; 8192],
            pat: SectionAsm::default(),
            pmt: SectionAsm::default(),
            pes: PesAsm::default(),
            last_pcr: None,
        }
    }

    /// Most recent program clock reference, if any.
    #[must_use]
    pub fn last_pcr(&self) -> Option<Pcr> {
        self.last_pcr
    }

    /// End-of-stream flush: the trailing open unit, if it has payload.
    ///
    /// Real muxers leave the final PES unbounded; that frame completes
    /// only here.
    pub fn finish(&mut self) -> Option<AccessUnit> {
        let (_, codec) = self.video?;
        let unit = self.pes.take(codec);
        self.pes = PesAsm::default();
        unit
    }

    /// Feed bytes; returns access units completed by this chunk.
    ///
    /// The corrupt packet is dropped; bytes after it stay buffered,
    /// so the caller resumes by pushing again, even with no new bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` on CRC mismatch, continuity gaps, oversized
    /// sections or payloads, and malformed headers. State stays
    /// consistent; the caller keeps pushing after an error.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<AccessUnit>, TsDemuxError> {
        self.stash.extend_from_slice(bytes);
        let mut units = Vec::new();
        while let Some(at) = self.sync_at() {
            self.stash.drain(..at);
            if self.stash.len() < PACKET_SIZE {
                break;
            }
            let packet: [u8; PACKET_SIZE] = self.stash[..PACKET_SIZE]
                .try_into()
                .map_err(|_| TsDemuxError::BadPacket("short packet"))?;
            self.stash.drain(..PACKET_SIZE);
            self.push_packet(&packet, &mut units)?;
        }
        Ok(units)
    }

    /// Offset of the next trusted sync byte; `None` wants more data.
    ///
    /// Two buffered packets demand double sync; a lone trailing packet
    /// is trusted, else clean EOF holds one packet back forever.
    fn sync_at(&self) -> Option<usize> {
        let mut i = 0;
        while i < self.stash.len() {
            if self.stash[i] != SYNC_BYTE {
                i += 1;
                continue;
            }
            if i + 2 * PACKET_SIZE <= self.stash.len() {
                if self.stash[i + PACKET_SIZE] == SYNC_BYTE {
                    return Some(i);
                }
                i += 1;
                continue;
            }
            return Some(i);
        }
        if self.stash.len() >= PACKET_SIZE {
            Some(self.stash.len() - (PACKET_SIZE - 1))
        } else {
            None
        }
    }

    /// One packet; completed units appended to `out`.
    fn push_packet(
        &mut self,
        packet: &[u8; PACKET_SIZE],
        out: &mut Vec<AccessUnit>,
    ) -> Result<(), TsDemuxError> {
        if packet[1] & 0x80 != 0 {
            return Ok(());
        }
        let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
        if pid == NULL_PID {
            return Ok(());
        }
        if packet[3] & 0xC0 != 0 {
            return Err(TsDemuxError::BadPacket("scrambled"));
        }
        let pusi = packet[1] & 0x40 != 0;
        let afc = (packet[3] >> 4) & 0x03;
        if afc == 0 {
            return Err(TsDemuxError::BadPacket("reserved adaptation control"));
        }
        let cc = packet[3] & 0x0F;
        let discontinuous = afc >= 2 && packet[4] > 0 && packet[5] & 0x80 != 0;
        let slot = &mut self.cc[usize::from(pid)];
        match *slot {
            Some(last) if last == cc => return Ok(()),
            Some(last) if !discontinuous => {
                let expected = (last + 1) & 0x0F;
                if expected != cc {
                    self.pes = PesAsm::default();
                    *slot = Some(cc);
                    return Err(TsDemuxError::Discontinuity {
                        pid,
                        expected,
                        got: cc,
                    });
                }
            }
            _ => {}
        }
        *slot = Some(cc);
        let mut pos = 4;
        if afc >= 2 {
            pos = self.skip_adaptation(packet, pos)?;
        }
        if afc == 1 || afc == 3 {
            return self.push_payload(pid, pusi, &packet[pos..], out);
        }
        Ok(())
    }

    /// Adaptation field skipped; PCR recorded.
    fn skip_adaptation(
        &mut self,
        packet: &[u8; PACKET_SIZE],
        pos: usize,
    ) -> Result<usize, TsDemuxError> {
        let len = usize::from(packet[pos]);
        if pos + 1 + len > PACKET_SIZE {
            return Err(TsDemuxError::BadPacket("adaptation overruns packet"));
        }
        if len > 0 {
            let flags = packet[pos + 1];
            if flags & 0x10 != 0 {
                if len < 7 {
                    return Err(TsDemuxError::BadPacket("short PCR"));
                }
                let raw: [u8; 6] = packet[pos + 2..pos + 8]
                    .try_into()
                    .map_err(|_| TsDemuxError::BadPacket("short PCR"))?;
                self.last_pcr = Some(Pcr::parse(raw));
            }
        }
        Ok(pos + 1 + len)
    }

    /// Payload routed by PID; completed units appended to `out`.
    fn push_payload(
        &mut self,
        pid: u16,
        pusi: bool,
        payload: &[u8],
        out: &mut Vec<AccessUnit>,
    ) -> Result<(), TsDemuxError> {
        if pid == 0 {
            if let Some(section) = self.pat.feed(pid, pusi, payload)? {
                self.pmt_pid = Self::parse_pat(&section);
            }
            return Ok(());
        }
        if Some(pid) == self.pmt_pid {
            if let Some(section) = self.pmt.feed(pid, pusi, payload)? {
                self.video = Self::parse_pmt(&section);
            }
            return Ok(());
        }
        if let Some((video_pid, codec)) = self.video {
            if pid == video_pid {
                let cap = self.max_pes_bytes;
                self.pes.feed(codec, pusi, payload, cap, out)?;
            }
        }
        Ok(())
    }

    /// PMT PID of the first program, if any.
    fn parse_pat(section: &[u8]) -> Option<u16> {
        if section.first() != Some(&TsMux::PAT_TID) || section.len() < 12 {
            return None;
        }
        let mut pos = 8;
        while pos + 4 <= section.len() - 4 {
            let program = u16::from_be_bytes([section[pos], section[pos + 1]]);
            let pid = (u16::from(section[pos + 2] & 0x1F) << 8) | u16::from(section[pos + 3]);
            if program != 0 {
                return Some(pid);
            }
            pos += 4;
        }
        None
    }

    /// Video PID and codec of the first video stream, if any.
    fn parse_pmt(section: &[u8]) -> Option<(u16, Codec)> {
        if section.first() != Some(&TsMux::PMT_TID) || section.len() < 12 {
            return None;
        }
        let info_len = (usize::from(section[10] & 0x0F) << 8) | usize::from(section[11]);
        let mut pos = 12 + info_len;
        while pos + 5 <= section.len() - 4 {
            let codec = match section[pos] {
                0x1B => Codec::H264,
                0x24 => Codec::H265,
                _ => {
                    let skip =
                        (usize::from(section[pos + 3] & 0x0F) << 8) | usize::from(section[pos + 4]);
                    pos += 5 + skip;
                    continue;
                }
            };
            let pid = (u16::from(section[pos + 1] & 0x1F) << 8) | u16::from(section[pos + 2]);
            return Some((pid, codec));
        }
        None
    }
}

impl SectionAsm {
    /// Payload bytes in; the completed section with CRC verified, if any.
    fn feed(
        &mut self,
        pid: u16,
        pusi: bool,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>, TsDemuxError> {
        let mut bytes = payload;
        if pusi {
            *self = Self::default();
            if bytes.is_empty() {
                return Err(TsDemuxError::BadPacket("missing pointer field"));
            }
            let skip = 1 + usize::from(bytes[0]);
            if skip > bytes.len() {
                return Err(TsDemuxError::BadPacket("pointer past payload"));
            }
            bytes = &bytes[skip..];
        } else if self.bytes.is_empty() && self.total.is_none() {
            return Ok(None);
        }
        self.bytes.extend_from_slice(bytes);
        if self.total.is_none() && self.bytes.len() >= 3 {
            let len = ((usize::from(self.bytes[1] & 0x0F)) << 8) | usize::from(self.bytes[2]);
            self.total = Some(3 + len);
        }
        if let Some(total) = self.total {
            if total > MAX_SECTION_SIZE {
                return Err(TsDemuxError::SectionTooLarge);
            }
            if self.bytes.len() >= total {
                let section = self.bytes[..total].to_vec();
                *self = Self::default();
                let body = &section[..section.len() - 4];
                let want = u32::from_be_bytes(
                    section[section.len() - 4..]
                        .try_into()
                        .map_err(|_| TsDemuxError::BadPacket("short section"))?,
                );
                if Mpeg2Crc::checksum(body) != want {
                    return Err(TsDemuxError::BadCrc { pid });
                }
                return Ok(Some(section));
            }
        }
        if self.bytes.len() > MAX_SECTION_SIZE {
            return Err(TsDemuxError::SectionTooLarge);
        }
        Ok(None)
    }
}

impl PesAsm {
    /// Payload bytes in; completed units appended to `out`.
    ///
    /// A unit completes when its declared length lands or the next
    /// header arrives with nothing outstanding; a short arrival errors.
    /// One packet can complete two units: the old at the new header
    /// plus the new when its declared bytes land in the same packet.
    fn feed(
        &mut self,
        codec: Codec,
        pusi: bool,
        payload: &[u8],
        cap: usize,
        out: &mut Vec<AccessUnit>,
    ) -> Result<(), TsDemuxError> {
        if pusi {
            if self.open {
                if self.remaining.is_some_and(|r| r > 0) {
                    *self = Self::default();
                    return Err(TsDemuxError::BadPacket("short PES"));
                }
                out.extend(self.take(codec));
            }
            *self = Self::default();
            self.open_header(payload, cap)?;
        } else if !self.open {
            return Ok(());
        } else {
            self.push_body(payload, cap)?;
        }
        if self.open && self.remaining == Some(0) {
            out.extend(self.take(codec));
            *self = Self::default();
        }
        Ok(())
    }

    /// PES header parsed; its inline payload kept.
    fn open_header(&mut self, payload: &[u8], cap: usize) -> Result<(), TsDemuxError> {
        if payload.len() < 9 {
            return Err(TsDemuxError::BadPacket("short PES header"));
        }
        if payload[0..3] != [0, 0, 1] {
            return Err(TsDemuxError::BadPacket("bad PES start code"));
        }
        if !(0xE0..=0xEF).contains(&payload[3]) {
            return Err(TsDemuxError::BadPacket("non-video PES id"));
        }
        if payload[6] >> 6 != 0x2 {
            return Err(TsDemuxError::BadPacket("bad PES flags"));
        }
        let header_len = usize::from(payload[8]);
        if payload.len() < 9 + header_len {
            return Err(TsDemuxError::BadPacket("short PES header data"));
        }
        match (payload[7] >> 6) & 0x03 {
            0x2 => {
                self.pts = Self::field(payload, 9)?;
                self.dts = self.pts;
            }
            0x3 => {
                if header_len < 10 {
                    return Err(TsDemuxError::BadPacket("short PTS/DTS"));
                }
                self.pts = Self::field(payload, 9)?;
                self.dts = Self::field(payload, 14)?;
            }
            _ => return Err(TsDemuxError::BadPacket("PES without PTS")),
        }
        self.open = true;
        let declared = u16::from_be_bytes([payload[4], payload[5]]);
        self.remaining =
            (declared != 0).then(|| usize::from(declared).saturating_sub(3 + header_len));
        let body = &payload[9 + header_len..];
        self.push_body(body, cap)?;
        Ok(())
    }

    /// Timestamp field parsed with marker check.
    fn field(payload: &[u8], at: usize) -> Result<u64, TsDemuxError> {
        let raw: [u8; 5] = payload[at..at + 5]
            .try_into()
            .map_err(|_| TsDemuxError::BadPacket("short timestamp"))?;
        Ts90k::parse_field(raw)
            .map(Ts90k::get)
            .ok_or(TsDemuxError::BadPacket("bad timestamp marker"))
    }

    /// Payload bytes appended within `cap`.
    fn push_body(&mut self, body: &[u8], cap: usize) -> Result<(), TsDemuxError> {
        let mut body = body;
        if let Some(remaining) = self.remaining {
            body = &body[..body.len().min(remaining)];
            self.remaining = Some(remaining - body.len());
        }
        if self.payload.len() + body.len() > cap {
            return Err(TsDemuxError::PesTooLarge {
                size: self.payload.len() + body.len(),
            });
        }
        self.payload.extend_from_slice(body);
        Ok(())
    }

    /// Current payload wrapped as a classified unit.
    fn take(&mut self, codec: Codec) -> Option<AccessUnit> {
        (!self.payload.is_empty()).then(|| {
            AccessUnit::classified(codec, std::mem::take(&mut self.payload), self.pts, self.dts)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Mpeg2Crc, TsDemux, TsMux, TsMuxConfig, TsMuxError};

    #[test]
    fn crc_matches_check_value() {
        assert_eq!(Mpeg2Crc::checksum(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn mux_rejects_bad_config() {
        let config = TsMuxConfig {
            video_pid: 0x1FFF,
            ..TsMuxConfig::default()
        };
        assert_eq!(
            TsMux::new(config).unwrap_err(),
            TsMuxError::InvalidPid(0x1FFF)
        );
        let config = TsMuxConfig {
            video_pid: TsMuxConfig::default().pmt_pid,
            ..TsMuxConfig::default()
        };
        assert_eq!(
            TsMux::new(config).unwrap_err(),
            TsMuxError::InvalidPid(config.pmt_pid)
        );
    }

    #[test]
    fn mux_rejects_av1_and_empty() {
        use crate::codec::{AccessUnit, Codec};
        let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
        let mut out = Vec::new();
        let av1 = AccessUnit::new(Codec::Av1, vec![1, 2, 3], 0, 0, true);
        assert_eq!(
            mux.push(&av1, None, &mut out).unwrap_err(),
            TsMuxError::CodecNotMappable(Codec::Av1)
        );
        let empty = AccessUnit::new(Codec::H264, Vec::new(), 0, 0, true);
        assert_eq!(
            mux.push(&empty, None, &mut out).unwrap_err(),
            TsMuxError::EmptyPayload
        );
        assert!(out.is_empty());
    }

    #[test]
    fn mux_declines_unmappable_codecs() {
        use crate::codec::{AccessUnit, Codec};
        let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
        let mut out = Vec::new();
        for codec in [Codec::Vp9, Codec::Vp8, Codec::Av2] {
            let unit = AccessUnit::new(codec, vec![1, 2, 3], 0, 0, true);
            assert_eq!(
                mux.push(&unit, None, &mut out).unwrap_err(),
                TsMuxError::CodecNotMappable(codec)
            );
        }
        assert!(out.is_empty());
    }

    #[test]
    fn pmt_skips_unmappable_entries() {
        use crate::codec::Codec;
        let mut section = vec![0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xF0, 0x00];
        section.extend_from_slice(&[0xEA, 0xE1, 0x00, 0xF0, 0x00, 0, 0, 0, 0]);
        assert_eq!(TsDemux::parse_pmt(&section), None);
        let mut mixed = section[..12].to_vec();
        mixed.extend_from_slice(&[0xEA, 0xE1, 0x00, 0xF0, 0x00]);
        mixed.extend_from_slice(&[0x1B, 0xE1, 0x01, 0xF0, 0x00]);
        mixed.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(TsDemux::parse_pmt(&mixed), Some((0x0101, Codec::H264)));
    }

    #[test]
    fn demux_holds_partial_packet() {
        let mut demux = TsDemux::new(1 << 20);
        assert!(demux.push(&[0x47; 100]).expect("buffered").is_empty());
    }

    #[test]
    fn demux_skips_garbage_before_sync() {
        use crate::codec::{AccessUnit, Codec};
        let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
        let unit = AccessUnit::new(
            Codec::H264,
            vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB],
            9000,
            9000,
            true,
        );
        let mut out = vec![0x11, 0x22, 0x33];
        mux.push(&unit, None, &mut out).expect("packetized");
        let mut demux = TsDemux::new(1 << 20);
        let units = demux.push(&out).expect("parsed");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].payload(), unit.payload());
    }
}
