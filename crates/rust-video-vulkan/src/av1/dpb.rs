//! AV1 reference context and decode instructions.

use std::rc::Rc;

use crate::av1;
use crate::dpb::{CodingFlags, Picture, ReferenceId};
use crate::error::ParseError;

/// One AV1 reference slot occupant.
#[derive(Clone)]
struct Av1Entry {
    /// Stored frame header.
    header: Rc<av1::FrameHeader>,
    /// Stable identity.
    id: ReferenceId,
}

/// One AV1 picture ready for the session.
pub(crate) struct Av1Picture {
    /// Shared picture shell: bytes, order, marking.
    picture: Picture,
    /// Parsed frame header.
    header: Rc<av1::FrameHeader>,
    /// Byte offset of each tile within the picture bytes.
    tile_offsets: Vec<u32>,
    /// Byte size of each tile.
    tile_sizes: Vec<u32>,
    /// Reference id per name, `LAST` through `ALTREF`.
    name_slots: [Option<ReferenceId>; av1::REFS_PER_FRAME],
    /// Skip-mode frame pair, 1-based reference names.
    skip_frames: [u8; 2],
    /// Evicted ids no name cites, released before the submit.
    pre: Vec<ReferenceId>,
    /// Evicted ids a name still cites, released after the submit.
    post: Vec<ReferenceId>,
}

impl Av1Picture {
    /// Shared picture shell: bytes, order, marking.
    pub fn picture(&self) -> &Picture {
        &self.picture
    }

    /// Parsed frame header.
    pub fn header(&self) -> &Rc<av1::FrameHeader> {
        &self.header
    }

    /// Byte offset of each tile within the picture bytes.
    pub fn tile_offsets(&self) -> &[u32] {
        &self.tile_offsets
    }

    /// Byte size of each tile.
    pub fn tile_sizes(&self) -> &[u32] {
        &self.tile_sizes
    }

    /// Reference id per name, `LAST` through `ALTREF`.
    pub fn name_slots(&self) -> &[Option<ReferenceId>; av1::REFS_PER_FRAME] {
        &self.name_slots
    }

    /// Skip-mode frame pair, 1-based reference names.
    pub fn skip_frames(&self) -> [u8; 2] {
        self.skip_frames
    }

    /// Evicted ids no name cites, released before the submit.
    pub fn pre(&self) -> &[ReferenceId] {
        &self.pre
    }

    /// Evicted ids a name still cites, released after the submit.
    pub fn post(&self) -> &[ReferenceId] {
        &self.post
    }
}

/// AV1 decoder instruction.
pub(crate) enum Av1Instruction {
    /// Sequence header for the session parameters.
    Sequence(Rc<av1::SequenceHeader>),
    /// Picture for the session.
    Picture(Box<Av1Picture>),
    /// Stored frame to display without decoding.
    ShowExisting {
        /// Stable identity of the stored frame.
        id: ReferenceId,
        /// Ids leaving the DPB with this display.
        releases: Vec<ReferenceId>,
    },
}

/// AV1 reference context over temporal units.
pub(crate) struct Av1Context {
    /// Active sequence header.
    seq: Option<Rc<av1::SequenceHeader>>,
    /// Reference slot occupants.
    slots: [Option<Av1Entry>; av1::TOTAL_REFS],
    /// Fresh reference ids.
    next_id: ReferenceId,
    /// Display order counter.
    display_seq: i32,
}

impl Av1Context {
    /// Empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: None,
            slots: [None, None, None, None, None, None, None, None],
            next_id: ReferenceId::default(),
            display_seq: 0,
        }
    }

    /// Instructions for one temporal unit.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a frame header or tile group fails to parse,
    /// or a frame arrives before any sequence header.
    pub fn compile(
        &mut self,
        unit: &av1::TemporalUnit<'_>,
    ) -> Result<Vec<Av1Instruction>, crate::error::VulkanError> {
        use crate::error::{ParseError, VulkanError};
        let mut instructions = Vec::new();
        let obus = unit.obus();
        let mut index = 0;
        while index < obus.len() {
            let obu = &obus[index];
            index += 1;
            match obu.kind() {
                av1::ObuType::SequenceHeader => {
                    let seq = Rc::new(av1::parse_sequence_header::<ParseError>(obu.payload())?);
                    if self.seq.as_ref().is_some_and(|cur| cur != &seq) {
                        self.slots = [None, None, None, None, None, None, None, None];
                    }
                    self.seq = Some(Rc::clone(&seq));
                    instructions.push(Av1Instruction::Sequence(seq));
                }
                av1::ObuType::TemporalDelimiter
                | av1::ObuType::Metadata
                | av1::ObuType::Padding => {}
                av1::ObuType::TileList => {
                    return Err(ParseError::Unsupported {
                        what: "av1 tile list",
                    }
                    .into());
                }
                av1::ObuType::Frame
                | av1::ObuType::FrameHeader
                | av1::ObuType::RedundantFrameHeader => {
                    let Some(seq) = self.seq.clone() else {
                        return Err(VulkanError::NoSession);
                    };
                    let views = self.views();
                    let (parsed, group) = if obu.kind() == av1::ObuType::Frame {
                        parse_frame_obu(obu.payload(), &seq, &views)?
                    } else {
                        let parsed = parse_frame_header(obu.payload(), &seq, &views)?;
                        (parsed, None)
                    };
                    let mut groups = group.into_iter().collect::<Vec<_>>();
                    if !parsed.header.show_existing_frame() {
                        while index < obus.len() && obus[index].kind() == av1::ObuType::TileGroup {
                            let expect = groups.last().map_or(0, |group| group.end + 1);
                            groups.push(parse_tile_group(
                                obus[index].payload(),
                                &parsed.header,
                                expect,
                            )?);
                            index += 1;
                        }
                    } else if index < obus.len() && obus[index].kind() == av1::ObuType::TileGroup {
                        return Err(ParseError::OutOfRange {
                            syntax: "tile group range",
                        }
                        .into());
                    }
                    instructions.extend(self.put_frame(parsed, &groups)?);
                }
                av1::ObuType::TileGroup => {
                    return Err(ParseError::OutOfRange {
                        syntax: "tile group range",
                    }
                    .into());
                }
            }
        }
        Ok(instructions)
    }

    /// Current slot views for frame header inference.
    fn views(&self) -> [av1::RefInfo; av1::TOTAL_REFS] {
        let mut views = core::array::from_fn(|_| av1::RefInfo::default());
        for (slot, view) in self.slots.iter().zip(views.iter_mut()) {
            let Some(entry) = slot else {
                continue;
            };
            let header = &entry.header;
            *view = av1::RefInfo {
                valid: true,
                frame_type: header.frame_type,
                frame_width: header.upscaled_width,
                frame_height: header.frame_height,
                upscaled_width: header.upscaled_width,
                render_width: header.render_width,
                render_height: header.render_height,
                order_hint: header.order_hint,
                frame_id: header.current_frame_id,
                loop_ref_deltas: header.loop_ref_deltas,
                loop_mode_deltas: header.loop_mode_deltas,
                seg_enabled: header.seg_enabled,
                seg_value: header.seg_value,
                gm_params: core::array::from_fn(|name| header.global_motion[name].params),
                grain: header.film_grain,
            };
        }
        views
    }

    /// Instructions for one parsed frame plus its tile groups.
    fn put_frame(
        &mut self,
        parsed: ParsedFrame,
        groups: &[TileGroup<'_>],
    ) -> Result<Vec<Av1Instruction>, crate::error::VulkanError> {
        use crate::error::ParseError;
        let header = Rc::new(parsed.header);
        if header.show_existing_frame() {
            return Ok(vec![self.put_show_existing(&header)?]);
        }
        let count = u32::from(header.tile_cols) * u32::from(header.tile_rows);
        if groups.is_empty()
            || groups
                .iter()
                .map(|group| group.end - group.start + 1)
                .sum::<u32>()
                != count
            || groups.last().is_some_and(|group| group.end + 1 != count)
        {
            return Err(ParseError::Truncated {
                syntax: "tile group range",
            }
            .into());
        }
        let before = self.slot_ids();
        if parsed.reset_all {
            self.slots = [None, None, None, None, None, None, None, None];
        }
        for (index, invalidated) in parsed.invalidated.iter().enumerate() {
            if *invalidated {
                self.slots[index] = None;
            }
        }
        for slot in header.ref_frame_idx {
            if !header.frame_type.is_intra() && self.slots[usize::from(slot)].is_none() {
                return Err(ParseError::MissingSet {
                    id: u32::from(slot),
                }
                .into());
            }
        }
        let id = self.next_id.next();
        let reference = header.refresh_frame_flags != 0;
        let mut name_slots = [None; av1::REFS_PER_FRAME];
        if !header.frame_type.is_intra() {
            for (index, slot) in header.ref_frame_idx.iter().enumerate() {
                name_slots[index] = self.slots[usize::from(*slot)]
                    .as_ref()
                    .map(|entry| entry.id);
            }
        }
        let joined = join_tiles(groups)?;
        let poc = self.display_seq;
        self.display_seq = self.display_seq.wrapping_add(1);
        if reference {
            let entry = Av1Entry {
                header: Rc::clone(&header),
                id,
            };
            for (index, slot) in self.slots.iter_mut().enumerate() {
                if header.refresh_frame_flags & (1_u8 << index) != 0 {
                    *slot = Some(entry.clone());
                }
            }
        }
        let releases = self.evicted(&before);
        let live: Vec<ReferenceId> = name_slots.iter().flatten().copied().collect();
        let (post, pre): (Vec<ReferenceId>, Vec<ReferenceId>) =
            releases.iter().copied().partition(|id| live.contains(id));
        let picture = Picture {
            l0: None,
            l1: None,
            poc,
            frame_num: 0,
            sps_id: 0,
            pps_id: 0,
            bytes: joined.bytes,
            slice_offsets: Vec::new(),
            coding: CodingFlags {
                is_reference: reference,
                is_intra: header.frame_type.is_intra(),
                is_idr: parsed.reset_all,
            },
            idr_pic_id: 0,
            id: reference.then_some(id),
            releases,
            drop_held: false,
            resets_dpb: parsed.reset_all,
        };
        Ok(vec![Av1Instruction::Picture(Box::new(Av1Picture {
            picture,
            header,
            tile_offsets: joined.offsets,
            tile_sizes: joined.sizes,
            name_slots,
            skip_frames: parsed.skip_frames,
            pre,
            post,
        }))])
    }

    /// Show-existing instruction for `header`, refreshing on key types.
    fn put_show_existing(
        &mut self,
        header: &av1::FrameHeader,
    ) -> Result<Av1Instruction, crate::error::VulkanError> {
        use crate::error::ParseError;
        let slot = usize::from(header.frame_to_show_map_idx);
        let Some(entry) = self.slots[slot].clone() else {
            return Err(ParseError::MissingSet {
                id: u32::from(header.frame_to_show_map_idx),
            }
            .into());
        };
        let before = self.slot_ids();
        if header.frame_type == av1::FrameType::Key {
            for slot in &mut self.slots {
                *slot = Some(entry.clone());
            }
        }
        Ok(Av1Instruction::ShowExisting {
            id: entry.id,
            releases: self.evicted(&before),
        })
    }

    /// True when `id` rides a live reference slot.
    pub(crate) fn is_live(&self, id: ReferenceId) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.as_ref().is_some_and(|entry| entry.id == id))
    }

    /// Stable identity per slot, if occupied.
    fn slot_ids(&self) -> [Option<ReferenceId>; av1::TOTAL_REFS] {
        let mut ids = [None; av1::TOTAL_REFS];
        for (slot, id) in self.slots.iter().zip(ids.iter_mut()) {
            *id = slot.as_ref().map(|entry| entry.id);
        }
        ids
    }

    /// Prior occupants of `before` held by no slot now, deduplicated.
    fn evicted(&self, before: &[Option<ReferenceId>; av1::TOTAL_REFS]) -> Vec<ReferenceId> {
        let mut releases = Vec::new();
        for old in before.iter().flatten() {
            let held = self
                .slots
                .iter()
                .any(|slot| slot.as_ref().is_some_and(|entry| entry.id == *old));
            if !held && !releases.contains(old) {
                releases.push(*old);
            }
        }
        releases
    }
}

impl Default for Av1Context {
    fn default() -> Self {
        Self::new()
    }
}

/// One parsed frame: header plus slot effects.
struct ParsedFrame {
    /// Parsed frame header.
    header: av1::FrameHeader,
    /// Keyframe display cleared every slot.
    reset_all: bool,
    /// Slots invalidated during the parse.
    invalidated: [bool; av1::TOTAL_REFS],
    /// Skip-mode frame pair, 1-based reference names.
    skip_frames: [u8; 2],
}

/// Frame header of one OBU payload against `seq` and `views`.
///
/// Reads reference state for inference and validation; slot effects land
/// in the returned invalidations for the caller to apply.
fn parse_frame_header(
    payload: &[u8],
    seq: &av1::SequenceHeader,
    views: &[av1::RefInfo; av1::TOTAL_REFS],
) -> Result<ParsedFrame, ParseError> {
    let mut refs = views.clone();
    let before = valid_snapshot(&refs);
    let (header, _) = av1::parse_frame_header::<ParseError>(payload, seq, &mut refs, 0, 0)?;
    Ok(convert_parsed(header, seq, views, &refs, before))
}

/// Frame OBU: header plus its tile group over one OBU payload.
fn parse_frame_obu<'a>(
    payload: &'a [u8],
    seq: &av1::SequenceHeader,
    views: &[av1::RefInfo; av1::TOTAL_REFS],
) -> Result<(ParsedFrame, Option<TileGroup<'a>>), ParseError> {
    let mut refs = views.clone();
    let before = valid_snapshot(&refs);
    let (header, consumed) = av1::parse_frame_header::<ParseError>(payload, seq, &mut refs, 0, 0)?;
    let parsed = convert_parsed(header, seq, views, &refs, before);
    if parsed.header.show_existing_frame() {
        return Ok((parsed, None));
    }
    let aligned = check_align_zero(payload, consumed)?;
    let rest = payload.get(aligned / 8..).ok_or(ParseError::Truncated {
        syntax: "tile group align",
    })?;
    let group = parse_tile_group(rest, &parsed.header, 0)?;
    Ok((parsed, Some(group)))
}

/// Parsed frame with slot effects and skip-mode frames.
fn convert_parsed(
    header: av1::FrameHeader,
    seq: &av1::SequenceHeader,
    views: &[av1::RefInfo; av1::TOTAL_REFS],
    refs: &[av1::RefInfo; av1::TOTAL_REFS],
    before: [bool; av1::TOTAL_REFS],
) -> ParsedFrame {
    let reset_all = header.frame_type == av1::FrameType::Key && header.show_frame();
    let mut invalidated = [false; av1::TOTAL_REFS];
    if !reset_all {
        for (index, slot) in refs.iter().enumerate() {
            invalidated[index] = before[index] && !slot.valid;
        }
    }
    let skip_frames = if header.show_existing_frame() {
        [0, 0]
    } else {
        av1::skip_mode_frames(seq, &header, views).unwrap_or([0, 0])
    };
    ParsedFrame {
        header,
        reset_all,
        invalidated,
        skip_frames,
    }
}

/// Validity of `refs` per slot.
fn valid_snapshot(refs: &[av1::RefInfo; av1::TOTAL_REFS]) -> [bool; av1::TOTAL_REFS] {
    core::array::from_fn(|index| refs[index].valid)
}

/// Byte alignment past `consumed` bits, rejecting nonzero padding.
fn check_align_zero(payload: &[u8], consumed: usize) -> Result<usize, ParseError> {
    let aligned = consumed.next_multiple_of(8);
    for bit in consumed..aligned {
        if payload[bit / 8] >> (7 - bit % 8) & 1 != 0 {
            return Err(ParseError::OutOfRange {
                syntax: "tile group align",
            });
        }
    }
    Ok(aligned)
}

/// Tile group tile ranges over one OBU payload.
struct TileGroup<'a> {
    /// First tile index of the group.
    start: u32,
    /// Last tile index of the group.
    end: u32,
    /// Tile data past the group header.
    data: &'a [u8],
    /// Byte offset and size of each tile within `data`.
    tiles: Vec<(usize, usize)>,
}

/// Tile group of one OBU payload expecting tiles from `expect_start`.
fn parse_tile_group<'a>(
    payload: &'a [u8],
    header: &av1::FrameHeader,
    expect_start: u32,
) -> Result<TileGroup<'a>, ParseError> {
    let count = u32::from(header.tile_cols) * u32::from(header.tile_rows);
    if count == 0 {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    let head = av1::tile_group_range::<ParseError>(
        payload,
        usize::try_from(count).map_err(|_| ParseError::OutOfRange {
            syntax: "tile group range",
        })?,
        u8::try_from(av1::tile_log2(1, u32::from(header.tile_cols))).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "tile group range",
            }
        })?,
        u8::try_from(av1::tile_log2(1, u32::from(header.tile_rows))).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "tile group range",
            }
        })?,
    )?;
    let start = u32::try_from(head.start).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let end = u32::try_from(head.end).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    if start < expect_start || start >= count || end < start || end >= count {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    check_align_zero(payload, head.consumed_bits)?;
    let tiles = split_tiles(head.data, start, end, header.tile_size_bytes.max(1))?;
    Ok(TileGroup {
        start,
        end,
        data: head.data,
        tiles,
    })
}

/// Tile ranges of `data` for tiles `start` through `end`.
fn split_tiles(
    data: &[u8],
    start: u32,
    end: u32,
    size_bytes: u8,
) -> Result<Vec<(usize, usize)>, ParseError> {
    let start = usize::try_from(start).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let end = usize::try_from(end).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    av1::split_tiles::<ParseError>(data, start, end, size_bytes)
}

/// Concatenated tile bytes plus per-tile offsets and sizes.
struct JoinedTiles {
    /// Concatenated tile bytes.
    bytes: Vec<u8>,
    /// Byte offset of each tile.
    offsets: Vec<u32>,
    /// Byte size of each tile.
    sizes: Vec<u32>,
}

/// Concatenated tile bytes plus per-tile offsets and sizes.
fn join_tiles(groups: &[TileGroup<'_>]) -> Result<JoinedTiles, ParseError> {
    let tiles: usize = groups.iter().map(|group| group.tiles.len()).sum();
    let total: usize = groups
        .iter()
        .flat_map(|group| group.tiles.iter().map(|(_, size)| size))
        .sum();
    let mut joined = JoinedTiles {
        bytes: Vec::with_capacity(total),
        offsets: Vec::with_capacity(tiles),
        sizes: Vec::with_capacity(tiles),
    };
    for group in groups {
        for (offset, size) in &group.tiles {
            let end = offset.saturating_add(*size);
            if group.data.len() < end {
                return Err(ParseError::Truncated {
                    syntax: "tile data",
                });
            }
            joined
                .offsets
                .push(
                    u32::try_from(joined.bytes.len()).map_err(|_| ParseError::OutOfRange {
                        syntax: "tile offset",
                    })?,
                );
            joined
                .sizes
                .push(u32::try_from(*size).map_err(|_| ParseError::OutOfRange {
                    syntax: "tile size",
                })?);
            joined.bytes.extend_from_slice(&group.data[*offset..end]);
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::Av1Context;

    #[test]
    fn join_tiles_concatenates_groups() {
        let first = super::TileGroup {
            start: 0,
            end: 1,
            data: &[9, 1, 2, 3, 4, 5],
            tiles: vec![(1, 3), (4, 2)],
        };
        let second = super::TileGroup {
            start: 2,
            end: 2,
            data: &[6, 7, 8],
            tiles: vec![(0, 3)],
        };
        let joined = super::join_tiles(&[first, second]).expect("tiles");
        assert_eq!(joined.bytes, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(joined.offsets, vec![0, 3, 5]);
        assert_eq!(joined.sizes, vec![3, 2, 3]);
        let short = super::TileGroup {
            start: 0,
            end: 0,
            data: &[1, 2],
            tiles: vec![(1, 2)],
        };
        assert!(super::join_tiles(&[short]).is_err());
    }

    #[test]
    fn evicted_reports_only_unheld_priors() {
        use super::{Av1Entry, ReferenceId};
        use std::rc::Rc;
        let header = Rc::new(crate::av1::FrameHeader::default());
        let mut context = Av1Context::new();
        context.slots[0] = Some(Av1Entry {
            header: Rc::clone(&header),
            id: ReferenceId(3),
        });
        context.slots[1] = Some(Av1Entry {
            header: Rc::clone(&header),
            id: ReferenceId(7),
        });
        context.slots[2] = Some(Av1Entry {
            header: Rc::clone(&header),
            id: ReferenceId(7),
        });
        let before = [
            Some(ReferenceId(3)),
            Some(ReferenceId(5)),
            Some(ReferenceId(7)),
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(context.evicted(&before), vec![ReferenceId(5)]);
    }
}
