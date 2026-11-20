//! AV1 OBU assembly, reference slots, and output orchestration.
//!
//! Portable: parses low-overhead OBU streams, groups tile groups under
//! frame headers, tracks the eight reference slots, and resolves
//! show-existing frames. A VA-API backend submits finished frames.

use std::rc::Rc;

use crate::av1::{
    frame_obu_tile_offset, parse_frame_header, parse_sequence_header, refresh_refs,
    show_existing_refs, split_obus, split_tiles, tile_group_range, FrameHeader, RefInfo,
    SequenceHeader, NUM_REF_FRAMES, OBU_FRAME, OBU_FRAME_HEADER, OBU_SEQUENCE_HEADER,
    OBU_TILE_GROUP, OBU_TILE_LIST,
};
use crate::error::VaApiError;
use crate::h264::frontend::SurfaceSlot;

use bytes::Bytes;
use rust_video_caps::reason::{
    BrokenReferenceReason as CapsBroken, ParseReason as CapsParse,
    UnsupportedReason as CapsUnsupported,
};

/// Number of AV1 reference slots.
pub const NUM_SLOTS: usize = 8;

/// One coded tile: byte span plus grid position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    /// Byte offset inside the group payload.
    pub offset: u32,
    /// Byte size inside the group payload.
    pub size: u32,
    /// Tile row in the grid.
    pub row: u32,
    /// Tile column in the grid.
    pub col: u32,
}

/// One tile group: payload bytes plus the parsed tiles within.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingTileGroup {
    /// Payload bytes the tile offsets are relative to.
    data: Bytes,
    /// Tiles in decode order.
    tiles: Vec<Tile>,
}

impl PendingTileGroup {
    /// Payload bytes the tile offsets are relative to.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Tiles in decode order.
    #[must_use]
    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }
}

/// Body of a pending frame: tiles to decode or a past frame to show.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingKind {
    /// Regular frame with its tile groups.
    Regular {
        /// Tile groups in bitstream order.
        groups: Vec<PendingTileGroup>,
    },
    /// Show-existing frame without new tiles.
    Existing,
}

/// Coded frame with its sequence and tile groups.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingFrame {
    /// Active sequence header.
    seq: Rc<SequenceHeader>,
    /// Parsed frame header.
    header: FrameHeader,
    /// Frame body.
    kind: PendingKind,
}

/// Coded frame with reference slots resolved.
pub struct Picture {
    /// Active sequence header.
    seq: Rc<SequenceHeader>,
    /// Parsed frame header.
    header: FrameHeader,
    /// Tile groups in bitstream order.
    groups: Vec<PendingTileGroup>,
    /// Reference slots with render sizes.
    refs: [Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS],
}

impl Picture {
    /// Active sequence header.
    #[must_use]
    pub fn seq(&self) -> &Rc<SequenceHeader> {
        &self.seq
    }

    /// Parsed frame header.
    #[must_use]
    pub fn header(&self) -> &FrameHeader {
        &self.header
    }

    /// Tile groups in bitstream order.
    #[must_use]
    pub fn groups(&self) -> &[PendingTileGroup] {
        &self.groups
    }

    /// Reference slots with render sizes.
    #[must_use]
    pub fn refs(&self) -> &[Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS] {
        &self.refs
    }
}

/// Begun frame: a picture to submit or a past frame to show.
pub enum Begun {
    /// Regular picture with resolved references.
    Picture(Box<Picture>),
    /// Show-existing frame on a live slot.
    Existing {
        /// Slot holding the frame to show.
        slot: SurfaceSlot,
        /// Render size of the frame to show.
        size: (u32, u32),
    },
}

/// Frames finished by one ingest call.
pub struct Ingest {
    /// Frames finished by the new bytes, in decode order.
    frames: Vec<PendingFrame>,
    /// True when the sequence changed mid-stream.
    reconfigured: bool,
}

impl Ingest {
    /// Frames finished by the new bytes, in decode order.
    pub fn frames(&mut self) -> Vec<PendingFrame> {
        std::mem::take(&mut self.frames)
    }

    /// True when the sequence changed mid-stream.
    #[must_use]
    pub fn reconfigured(&self) -> bool {
        self.reconfigured
    }
}

/// AV1 frontend: OBU stream in, ordered frames out.
pub struct Frontend {
    /// Reference slots behind header parsing.
    parse_refs: [RefInfo; NUM_REF_FRAMES],
    /// Active sequence header.
    seq: Option<Rc<SequenceHeader>>,
    /// Reference slots with render sizes.
    refs: [Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS],
    /// Header of the open frame.
    open: Option<(Rc<SequenceHeader>, FrameHeader)>,
    /// Tile groups of the open frame.
    groups: Vec<PendingTileGroup>,
}

impl Frontend {
    /// Empty frontend, no sequence active.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parse_refs: core::array::from_fn(|_| RefInfo::default()),
            seq: None,
            refs: Default::default(),
            open: None,
            groups: Vec::new(),
        }
    }

    /// Coded picture size in luma samples, if a sequence is active.
    #[must_use]
    pub fn coded_size(&self) -> Option<(u32, u32)> {
        self.seq.as_ref().map(|seq| {
            (
                seq.max_frame_width_minus1 + 1,
                seq.max_frame_height_minus1 + 1,
            )
        })
    }

    /// Active sequence header.
    #[must_use]
    pub fn seq(&self) -> Option<&Rc<SequenceHeader>> {
        self.seq.as_ref()
    }

    /// True when `index` backs a live reference slot.
    #[must_use]
    pub fn slot_in_use(&self, index: u32) -> bool {
        self.refs
            .iter()
            .flatten()
            .any(|(slot, _)| slot.index() == index)
    }

    /// Frames finished by the OBU stream in `bytes`.
    ///
    /// Tile payloads sub-slice `bytes` without copying.
    ///
    /// # Errors
    ///
    /// Returns `Err` when an OBU fails to parse, arrives out of order,
    /// or needs large-scale tiles.
    pub fn ingest(&mut self, bytes: &Bytes) -> Result<Ingest, VaApiError> {
        let mut out = Ingest {
            frames: Vec::new(),
            reconfigured: false,
        };
        for obu in split_obus::<VaApiError>(bytes.as_ref())? {
            match obu.unit_type {
                OBU_SEQUENCE_HEADER => {
                    self.finish_open(&mut out)?;
                    self.parse_refs = core::array::from_fn(|_| RefInfo::default());
                }
                OBU_FRAME_HEADER | OBU_FRAME => {
                    self.finish_open(&mut out)?;
                }
                _ => {}
            }
            self.ingest_obu(&obu, bytes, &mut out)?;
        }
        Ok(out)
    }

    /// Open frame, if any tile groups arrived.
    ///
    /// A lone header stays open: its tiles may arrive later.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the open frame's reference update fails.
    pub fn take_pending(&mut self) -> Result<Option<PendingFrame>, VaApiError> {
        if self.groups.is_empty() {
            return Ok(None);
        }
        let Some((seq, header)) = self.open.take() else {
            return Ok(None);
        };
        let groups = std::mem::take(&mut self.groups);
        refresh_refs::<VaApiError>(&mut self.parse_refs, &header)?;
        Ok(Some(PendingFrame {
            seq,
            header,
            kind: PendingKind::Regular { groups },
        }))
    }

    /// Store `picture` on `slot`, refreshing reference slots.
    pub fn commit(&mut self, picture: &Picture, slot: SurfaceSlot) {
        let size = (picture.header.render_width, picture.header.render_height);
        self.refresh(slot, size, u32::from(picture.header.refresh_frame_flags));
    }

    /// Order `pending` against the reference slots.
    ///
    /// Show-existing frames refresh reference slots inline: they hold
    /// no new surface, so there is no later commit to do it.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame leaves Profile 0 8-bit 4:2:0 or
    /// names a reference slot that holds no frame.
    pub fn begin(&mut self, pending: PendingFrame) -> Result<Begun, VaApiError> {
        Self::check_supported(&pending.seq)?;
        match pending.kind {
            PendingKind::Existing => {
                let index = usize::from(pending.header.frame_to_show_map_idx);
                let (slot, size) = self.refs.get(index).copied().flatten().ok_or({
                    VaApiError::BrokenReference(CapsBroken::NamesEmptySlot {
                        reference: "show_existing_frame",
                    })
                })?;
                self.refresh(slot, size, u32::from(pending.header.refresh_frame_flags));
                Ok(Begun::Existing { slot, size })
            }
            PendingKind::Regular { groups } => Ok(Begun::Picture(Box::new(Picture {
                seq: pending.seq,
                header: pending.header,
                groups,
                refs: self.refs,
            }))),
        }
    }

    /// Point refreshed slots at `slot` with render `size`.
    fn refresh(&mut self, slot: SurfaceSlot, size: (u32, u32), flags: u32) {
        let mut flags = flags;
        for entry in &mut self.refs {
            if flags & 1 == 1 {
                *entry = Some((slot, size));
            }
            flags >>= 1;
        }
    }

    /// Route one OBU into the open frame or the finished list.
    fn ingest_obu(
        &mut self,
        obu: &rust_video_av1::Obu<'_>,
        bytes: &Bytes,
        out: &mut Ingest,
    ) -> Result<(), VaApiError> {
        match obu.unit_type {
            OBU_SEQUENCE_HEADER => {
                let seq = Rc::new(parse_sequence_header::<VaApiError>(obu.payload)?);
                self.adopt_seq(seq, out);
            }
            OBU_FRAME_HEADER => {
                let seq = self.active_seq()?;
                let (header, _) = parse_frame_header::<VaApiError>(
                    obu.payload,
                    &seq,
                    &mut self.parse_refs,
                    obu.temporal_id,
                    obu.spatial_id,
                )?;
                if header.show_existing_frame() {
                    show_existing_refs(&mut self.parse_refs, &header);
                    out.frames.push(PendingFrame {
                        seq,
                        header,
                        kind: PendingKind::Existing,
                    });
                } else {
                    self.open = Some((seq, header));
                }
            }
            OBU_TILE_GROUP => {
                let Some((_, header)) = self.open.clone() else {
                    return Err(VaApiError::Parse(CapsParse::TileGroupWithoutFrameHeader));
                };
                self.groups
                    .push(Self::split_group(obu.payload, bytes, &header)?);
            }
            OBU_FRAME => {
                let seq = self.active_seq()?;
                let (header, consumed) = parse_frame_header::<VaApiError>(
                    obu.payload,
                    &seq,
                    &mut self.parse_refs,
                    obu.temporal_id,
                    obu.spatial_id,
                )?;
                let start = frame_obu_tile_offset::<VaApiError>(obu.payload, consumed)?;
                let rest = obu
                    .payload
                    .get(start..)
                    .ok_or(VaApiError::Parse(CapsParse::ObuSizeExceedsPayload))?;
                if header.show_existing_frame() {
                    show_existing_refs(&mut self.parse_refs, &header);
                    if !rest.is_empty() {
                        return Err(VaApiError::Parse(CapsParse::Range {
                            syntax: "show_existing_frame",
                        }));
                    }
                    out.frames.push(PendingFrame {
                        seq,
                        header,
                        kind: PendingKind::Existing,
                    });
                } else {
                    let group = Self::split_group(rest, bytes, &header)?;
                    refresh_refs::<VaApiError>(&mut self.parse_refs, &header)?;
                    out.frames.push(PendingFrame {
                        seq,
                        header,
                        kind: PendingKind::Regular {
                            groups: vec![group],
                        },
                    });
                }
            }
            OBU_TILE_LIST => {
                return Err(VaApiError::Unsupported(CapsUnsupported::TileList));
            }
            _ => {}
        }
        Ok(())
    }

    /// Tile spans of `payload` sub-sliced from `bytes` under `header`.
    fn split_group(
        payload: &[u8],
        bytes: &Bytes,
        header: &FrameHeader,
    ) -> Result<PendingTileGroup, VaApiError> {
        let range = |what: &'static str| VaApiError::Parse(CapsParse::Range { syntax: what });
        let count = usize::from(header.tile_cols).saturating_mul(usize::from(header.tile_rows));
        let head = tile_group_range::<VaApiError>(
            payload,
            count,
            header.tile_cols_log2,
            header.tile_rows_log2,
        )?;
        let base = payload.len() - head.data.len();
        let ranges =
            split_tiles::<VaApiError>(head.data, head.start, head.end, header.tile_size_bytes)?;
        let mut tiles = Vec::with_capacity(ranges.len());
        let cols = u32::from(header.tile_cols);
        for (offset, (payload_offset, size)) in ranges.iter().enumerate() {
            let tile_num = u32::try_from(head.start + offset).map_err(|_| range("index"))?;
            tiles.push(Tile {
                offset: u32::try_from(base + payload_offset).map_err(|_| range("offset"))?,
                size: u32::try_from(*size).map_err(|_| range("size"))?,
                row: tile_num / cols,
                col: tile_num % cols,
            });
        }
        let base_ptr = bytes.as_ptr() as usize;
        let offset = payload.as_ptr() as usize - base_ptr;
        debug_assert!(offset + payload.len() <= bytes.len());
        Ok(PendingTileGroup {
            data: bytes.slice(offset..offset + payload.len()),
            tiles,
        })
    }

    /// Active sequence header or a parse error.
    fn active_seq(&self) -> Result<Rc<SequenceHeader>, VaApiError> {
        self.seq
            .clone()
            .ok_or(VaApiError::Parse(CapsParse::FrameBeforeSequenceHeader))
    }

    /// Finish the open frame into `out`.
    fn finish_open(&mut self, out: &mut Ingest) -> Result<(), VaApiError> {
        if let Some(frame) = self.take_pending()? {
            out.frames.push(frame);
        }
        Ok(())
    }

    /// Adopt the freshly parsed sequence header.
    fn adopt_seq(&mut self, seq: Rc<SequenceHeader>, out: &mut Ingest) {
        if self.seq.as_ref().is_some_and(|active| **active == *seq) {
            return;
        }
        self.refs = Default::default();
        self.seq = Some(seq);
        out.reconfigured = true;
    }

    /// Reject streams outside Profile 0 8-bit 4:2:0 without scalability.
    fn check_supported(seq: &SequenceHeader) -> Result<(), VaApiError> {
        if seq.profile != 0 {
            return Err(VaApiError::Unsupported(CapsUnsupported::ProfileOutsideMain));
        }
        if seq.bit_depth() != 8 {
            return Err(VaApiError::Unsupported(
                CapsUnsupported::Av1BitDepthNeedsTarget {
                    depth: seq.bit_depth(),
                },
            ));
        }
        if seq.mono_chrome() {
            return Err(VaApiError::Unsupported(CapsUnsupported::Av1Monochrome));
        }
        if !seq.subsampling_x() || !seq.subsampling_y() {
            return Err(VaApiError::Unsupported(
                CapsUnsupported::Av1ChromaOutside420,
            ));
        }
        if seq.operating.len() > 1 {
            return Err(VaApiError::Unsupported(CapsUnsupported::SpatialLayers));
        }
        Ok(())
    }
}

impl Default for Frontend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Begun, Frontend, PendingFrame, PendingKind};
    use crate::av1::{FrameHeader, FrameType};
    use crate::error::VaApiError;
    use crate::fixtures::{
        keyframe_hidden_payload, keyframe_payload, keyframe_wide_payload, obu, pframe_payload,
        seq_payload, seq_payload_sized, show_existing_payload, tile_group_2_payload,
        tile_group_payload,
    };
    use crate::h264::frontend::SurfaceSlot;

    use bytes::Bytes;

    /// Frontend that ingested the 64x64 sequence header.
    fn sequenced() -> Frontend {
        let mut frontend = Frontend::new();
        let stream = obu(1, &seq_payload());
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("sequence parses");
        assert_eq!(ingest.frames(), []);
        assert_eq!(frontend.coded_size(), Some((64, 64)));
        frontend
    }

    #[test]
    fn ingest_groups_obus_into_frames() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[9, 9, 9, 9])));
        stream.extend(obu(3, &pframe_payload()));
        stream.extend(obu(4, &tile_group_payload(&[8])));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("frames parse");
        let frames = ingest.frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].header.frame_type, FrameType::Key);
        assert!(frames[0].header.show_frame());
        assert_eq!(frames[0].header.upscaled_width, 64);
        let PendingKind::Regular { groups } = &frames[0].kind else {
            panic!("regular frame");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tiles().len(), 1);
        assert_eq!(groups[0].tiles()[0].offset, 0);
        assert_eq!(groups[0].tiles()[0].size, 4);
        assert_eq!(groups[0].data(), &[9, 9, 9, 9]);
        let pending = frontend
            .take_pending()
            .expect("take pending")
            .expect("open p frame");
        assert_eq!(pending.header.frame_type, FrameType::Inter);
        assert!(frontend.take_pending().expect("take empty").is_none());
    }

    #[test]
    fn frame_obu_completes_immediately() {
        let mut frontend = sequenced();
        let mut payload = keyframe_payload();
        payload.extend(tile_group_payload(&[7, 7]));
        let stream = obu(6, &payload);
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("frame parses");
        let frames = ingest.frames();
        assert_eq!(frames.len(), 1);
        let PendingKind::Regular { groups } = &frames[0].kind else {
            panic!("regular frame");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tiles()[0].size, 2);
        assert_eq!(groups[0].data(), &[7, 7]);
    }

    #[test]
    fn begin_resolves_reference_slots() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[1])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("key begins") else {
            panic!("regular picture");
        };
        assert!(picture.refs().iter().all(Option::is_none));
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(0));
        assert!(frontend
            .refs
            .iter()
            .all(|entry| entry.is_some_and(|(slot, size)| slot.index() == 0 && size == (64, 64))));
        let mut stream = obu(3, &pframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[2])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("p parses");
        let pending = frontend.take_pending().expect("take").expect("p frame");
        assert_eq!(pending.header.refresh_frame_flags, 0x01);
        let Begun::Picture(picture) = frontend.begin(pending).expect("p begins") else {
            panic!("regular picture");
        };
        assert_eq!(picture.refs()[0].map(|(slot, _)| slot.index()), Some(0));
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(1));
        assert_eq!(frontend.refs[0].map(|(slot, _)| slot.index()), Some(1));
        assert_eq!(frontend.refs[1].map(|(slot, _)| slot.index()), Some(0));
    }

    #[test]
    fn show_existing_outputs_past_slot() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[1])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("begins") else {
            panic!("regular picture");
        };
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(0));
        let mut stream = obu(3, &pframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[2])));
        stream.extend(obu(3, &show_existing_payload(0)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("p parses");
        let frames = ingest.frames();
        assert_eq!(frames.len(), 2);
        let mut frames = frames.into_iter();
        let Begun::Picture(picture) = frontend.begin(frames.next().expect("p")).expect("begins")
        else {
            panic!("regular picture");
        };
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(1));
        let pending = frames.next().expect("existing frame");
        assert!(matches!(pending.kind, PendingKind::Existing));
        let Begun::Existing { slot, size } = frontend.begin(pending).expect("begins") else {
            panic!("existing frame");
        };
        assert_eq!(slot.index(), 1);
        assert_eq!(size, (64, 64));
    }

    #[test]
    fn show_existing_keyframe_refreshes_all() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_hidden_payload());
        stream.extend(obu(4, &tile_group_payload(&[1])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        assert!(!pending.header.show_frame());
        let Begun::Picture(picture) = frontend.begin(pending).expect("begins") else {
            panic!("regular picture");
        };
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(5));
        let mut stream = obu(3, &pframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[2])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("p parses");
        let pending = frontend.take_pending().expect("take").expect("p frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("begins") else {
            panic!("regular picture");
        };
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(6));
        assert_eq!(frontend.refs[0].map(|(slot, _)| slot.index()), Some(6));
        let stream = obu(3, &show_existing_payload(1));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("existing parses");
        let frames = ingest.frames();
        assert_eq!(frames.len(), 1);
        let Begun::Existing { slot, .. } = frontend
            .begin(frames.into_iter().next().expect("existing"))
            .expect("begins")
        else {
            panic!("existing frame");
        };
        assert_eq!(slot.index(), 5);
        assert!(frontend
            .refs
            .iter()
            .all(|entry| entry.is_some_and(|(slot, _)| slot.index() == 5)));
    }

    #[test]
    fn show_existing_without_reference_fails() {
        let frontend = sequenced();
        let seq = frontend.seq().expect("active sequence").clone();
        let header = FrameHeader {
            frame_to_show_map_idx: 3,
            ..FrameHeader::default()
        };
        let mut frontend = sequenced();
        let pending = PendingFrame {
            seq,
            header,
            kind: PendingKind::Existing,
        };
        assert!(matches!(
            frontend.begin(pending),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn tile_group_without_header_fails() {
        let mut frontend = sequenced();
        let stream = obu(4, &tile_group_payload(&[1]));
        let bytes = Bytes::from(stream);
        assert!(matches!(frontend.ingest(&bytes), Err(VaApiError::Parse(_))));
    }

    #[test]
    fn tile_list_declines_typed() {
        let mut frontend = sequenced();
        let stream = obu(8, &[0]);
        let bytes = Bytes::from(stream);
        let Err(VaApiError::Unsupported(reason)) = frontend.ingest(&bytes) else {
            panic!("tile list declines");
        };
        assert_eq!(reason, rust_video_caps::reason::UnsupportedReason::TileList);
    }

    #[test]
    fn check_supported_declines_foreign_streams() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[1])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        let mut bad = (*pending.seq).clone();
        bad.profile = 1;
        let frame = PendingFrame {
            seq: std::rc::Rc::new(bad),
            header: pending.header.clone(),
            kind: PendingKind::Existing,
        };
        assert!(matches!(
            frontend.begin(frame),
            Err(VaApiError::Unsupported(_))
        ));
        let pending = {
            let mut stream = obu(3, &keyframe_payload());
            stream.extend(obu(4, &tile_group_payload(&[1])));
            let bytes = Bytes::from(stream);
            frontend.ingest(&bytes).expect("key parses");
            frontend.take_pending().expect("take").expect("key frame")
        };
        let mut bad = (*pending.seq).clone();
        bad.flags |= rust_video_av1::SEQ_HIGH_BITDEPTH;
        let frame = PendingFrame {
            seq: std::rc::Rc::new(bad),
            header: pending.header.clone(),
            kind: PendingKind::Existing,
        };
        assert!(matches!(
            frontend.begin(frame),
            Err(VaApiError::Unsupported(_))
        ));
        assert!(Frontend::check_supported(&pending.seq).is_ok());
    }

    #[test]
    fn sequence_change_reconfigures() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[1])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("begins") else {
            panic!("regular picture");
        };
        frontend.commit(&picture, crate::h264::frontend::SurfaceSlot::new(0));
        assert!(frontend.refs[0].is_some());
        let stream = obu(1, &seq_payload_sized(128, 64));
        let bytes = Bytes::from(stream);
        let ingest = frontend.ingest(&bytes).expect("new sequence parses");
        assert!(ingest.reconfigured());
        assert!(frontend.refs.iter().all(Option::is_none));
        assert_eq!(frontend.coded_size(), Some((128, 64)));
    }

    #[test]
    fn show_existing_frame_with_tiles_fails() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[9])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("begun") else {
            panic!("regular picture");
        };
        frontend.commit(&picture, SurfaceSlot::new(4));
        let mut payload = show_existing_payload(0);
        payload.push(9);
        let bytes = Bytes::from(obu(6, &payload));
        let Err(VaApiError::Parse(reason)) = frontend.ingest(&bytes) else {
            panic!("tiled show-existing fails");
        };
        assert_eq!(
            reason,
            rust_video_caps::reason::ParseReason::Range {
                syntax: "show_existing_frame"
            }
        );
    }

    #[test]
    fn redundant_and_reserved_obus_ignored() {
        let mut frontend = sequenced();
        let mut stream = obu(7, &[0]);
        stream.extend(obu(9, &[0]));
        stream.extend(obu(0, &[0]));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("obus ignored");
        assert_eq!(ingest.frames().len(), 0);
        assert!(frontend.take_pending().expect("take").is_none());
    }

    #[test]
    fn show_existing_frame_obu_outputs() {
        let mut frontend = sequenced();
        let mut stream = obu(3, &keyframe_payload());
        stream.extend(obu(4, &tile_group_payload(&[9])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("key parses");
        let pending = frontend.take_pending().expect("take").expect("key frame");
        let Begun::Picture(picture) = frontend.begin(pending).expect("begun") else {
            panic!("regular picture");
        };
        frontend.commit(&picture, SurfaceSlot::new(4));
        let bytes = Bytes::from(obu(6, &show_existing_payload(0)));
        let mut ingest = frontend.ingest(&bytes).expect("show-existing parses");
        let frames = ingest.frames();
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0].kind, PendingKind::Existing));
        let Begun::Existing { slot, size } = frontend
            .begin(frames.into_iter().next().expect("frame"))
            .expect("begun")
        else {
            panic!("existing output");
        };
        assert_eq!(slot.index(), 4);
        assert_eq!(size, (64, 64));
    }

    #[test]
    fn two_tiles_cover_the_frame() {
        let mut frontend = Frontend::new();
        let stream = obu(1, &seq_payload_sized(128, 64));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("sequence parses");
        let mut stream = obu(3, &keyframe_wide_payload());
        stream.extend(obu(4, &tile_group_2_payload(&[1, 2, 3, 4], &[5, 6])));
        let bytes = Bytes::from(stream);
        frontend.ingest(&bytes).expect("frame parses");
        let pending = frontend.take_pending().expect("take").expect("wide frame");
        let PendingKind::Regular { groups } = pending.kind else {
            panic!("regular frame");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tiles().len(), 2);
        assert_eq!(groups[0].tiles()[0].offset, 2);
        assert_eq!(groups[0].tiles()[0].size, 4);
        assert_eq!(groups[0].tiles()[1].offset, 6);
        assert_eq!(groups[0].tiles()[1].size, 2);
    }
}
