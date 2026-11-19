//! H.265 reference context and decode instructions.

use std::rc::Rc;

use crate::au::AccessUnit;
use crate::dpb::{join_coded, CodingFlags, Picture, RefPicture, ReferenceId};
use crate::h265;

/// H.265 driver echo of the slice reference sets.
#[derive(Clone, Debug)]
pub(crate) struct RpsEcho {
    /// Short-term set comes from the SPS.
    sps_flag: bool,
    /// Delta pictures of the predicted-from set.
    ref_count: u8,
    /// Syntax bit length of an explicit short-term set.
    bits: u16,
    /// Current-before ids in list order.
    before: Vec<Option<ReferenceId>>,
    /// Current-after ids in list order.
    after: Vec<Option<ReferenceId>>,
    /// Current long-term ids in list order.
    lt: Vec<Option<ReferenceId>>,
    /// Picture is an IRAP.
    is_irap: bool,
    /// Picture is an IDR.
    is_idr: bool,
    /// Active video set id.
    vps_id: u8,
}

impl RpsEcho {
    /// Short-term set comes from the SPS.
    #[must_use]
    pub fn sps_flag(&self) -> bool {
        self.sps_flag
    }

    /// Delta pictures of the predicted-from set.
    #[must_use]
    pub fn ref_count(&self) -> u8 {
        self.ref_count
    }

    /// Syntax bit length of an explicit short-term set.
    #[must_use]
    pub fn bits(&self) -> u16 {
        self.bits
    }

    /// Current-before ids in list order.
    #[must_use]
    pub fn before(&self) -> &[Option<ReferenceId>] {
        &self.before
    }

    /// Current-after ids in list order.
    #[must_use]
    pub fn after(&self) -> &[Option<ReferenceId>] {
        &self.after
    }

    /// Current long-term ids in list order.
    #[must_use]
    pub fn lt(&self) -> &[Option<ReferenceId>] {
        &self.lt
    }

    /// Picture is an IRAP.
    #[must_use]
    pub fn is_irap(&self) -> bool {
        self.is_irap
    }

    /// Picture is an IDR.
    #[must_use]
    pub fn is_idr(&self) -> bool {
        self.is_idr
    }

    /// Active video set id.
    #[must_use]
    pub fn vps_id(&self) -> u8 {
        self.vps_id
    }
}

/// H.265 picture with its driver echo.
#[derive(Clone, Debug)]
pub(crate) struct H265Picture {
    /// Shared picture payload.
    picture: Picture,
    /// Driver echo of the reference sets.
    echo: RpsEcho,
}

impl H265Picture {
    /// Shared picture payload.
    #[must_use]
    pub fn picture(&self) -> &Picture {
        &self.picture
    }

    /// Driver echo of the reference sets.
    #[must_use]
    pub fn echo(&self) -> &RpsEcho {
        &self.echo
    }
}

/// H.265 decoder instruction.
pub(crate) enum H265Instruction {
    /// Video set for the session parameters.
    Vps(Rc<h265::Vps>),
    /// Sequence set for the session parameters.
    Sps(Rc<h265::Sps>),
    /// Picture set for the session parameters.
    Pps(Rc<h265::Pps>),
    /// Picture for the session.
    Picture(Box<H265Picture>),
}

/// One H.265 DPB entry.
struct H265Entry {
    /// Stable identity.
    id: ReferenceId,
    /// Picture order count.
    poc: i32,
}

/// H.265 reference context: POC, RPS marking, reference union.
pub(crate) struct H265Context {
    /// Decoded pictures by order count.
    dpb: Vec<H265Entry>,
    /// Next reference identity.
    next_id: ReferenceId,
    /// POC anchor of the last temporal-0 picture.
    poc_tid0: Option<i32>,
    /// Leading pictures drop until the next IRAP.
    suppress_rasl: bool,
    /// Pictures drop until the next IDR or BLA.
    waiting: bool,
    /// First picture since reset.
    seen_first: bool,
}

impl H265Context {
    /// Empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dpb: Vec::new(),
            next_id: ReferenceId::default(),
            poc_tid0: None,
            suppress_rasl: false,
            waiting: false,
            seen_first: false,
        }
    }

    /// Instructions for one access unit.
    pub fn compile(&mut self, au: AccessUnit<h265::ParsedNalu>) -> Vec<H265Instruction> {
        let mut instructions = Vec::new();
        let mut slices = Vec::new();
        for nalu in au.into_nalus() {
            match nalu {
                h265::ParsedNalu::Vps(vps) => {
                    instructions.push(H265Instruction::Vps(vps));
                }
                h265::ParsedNalu::Sps(sps) => {
                    instructions.push(H265Instruction::Sps(sps));
                }
                h265::ParsedNalu::Pps(pps) => {
                    instructions.push(H265Instruction::Pps(pps));
                }
                h265::ParsedNalu::Slice(slice) => slices.push(*slice),
                h265::ParsedNalu::Other => {}
            }
        }
        if slices.is_empty() {
            return instructions;
        }
        if let Some(picture) = self.put_picture(&slices) {
            instructions.push(H265Instruction::Picture(Box::new(picture)));
        }
        instructions
    }

    /// One picture from its slices, or `None` when the picture drops.
    fn put_picture(&mut self, slices: &[h265::Slice]) -> Option<H265Picture> {
        let first = &slices[0];
        let unit = first.unit_type;
        if h265::is_rasl(unit) && self.suppress_rasl {
            return None;
        }
        if self.waiting && !(h265::is_idr(unit) || h265::is_bla(unit)) {
            return None;
        }
        let sps = Rc::clone(&first.sps);
        let header = &first.header;
        let new_sequence =
            h265::is_idr(unit) || h265::is_bla(unit) || (h265::is_cra(unit) && !self.seen_first);
        let Some(poc) = self.picture_poc(header, unit, first.temporal_id, &sps) else {
            self.resync();
            return None;
        };
        let mut releases = Vec::new();
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut lt = Vec::new();
        if new_sequence {
            releases.extend(self.dpb.iter().map(|entry| entry.id));
            self.dpb.clear();
        } else {
            if self.dpb.iter().any(|entry| entry.poc == poc) {
                self.resync();
                return None;
            }
            let Some(rps) = self.apply_rps(poc, header, &sps, &mut releases) else {
                self.resync();
                return None;
            };
            before = rps.before;
            after = rps.after;
            lt = rps.lt;
        }
        if header.slice_type != h265::SliceType::I
            && before.is_empty()
            && after.is_empty()
            && lt.is_empty()
        {
            self.resync();
            return None;
        }
        let id = self.next_id.next();
        self.dpb.push(H265Entry { id, poc });
        if h265::is_irap(unit) {
            self.suppress_rasl = new_sequence;
        }
        self.seen_first = true;
        self.waiting = false;
        let l0 = if header.slice_type == h265::SliceType::I {
            None
        } else {
            let mut union = Vec::new();
            union.extend(before.iter().copied());
            union.extend(after.iter().copied());
            union.extend(lt.iter().copied());
            Some(union)
        };
        let echo = RpsEcho {
            sps_flag: header.rps_bits.as_ref().is_some_and(|bits| !bits.explicit),
            ref_count: header
                .rps_bits
                .as_ref()
                .map_or(0, |bits| bits.ref_num_delta_pocs),
            bits: header.rps_bits.as_ref().map_or(0, |bits| bits.bits),
            before: before.iter().map(RefPicture::id).collect(),
            after: after.iter().map(RefPicture::id).collect(),
            lt: lt.iter().map(RefPicture::id).collect(),
            is_irap: h265::is_irap(unit),
            is_idr: h265::is_idr(unit),
            vps_id: first.vps.id,
        };
        let (bytes, slice_offsets) = join_coded(slices, &[0, 0, 1], |slice| &slice.payload);
        let picture = Picture {
            l0,
            l1: None,
            poc,
            frame_num: 0,
            sps_id: sps.id,
            pps_id: first.pps.id,
            bytes,
            slice_offsets,
            coding: CodingFlags {
                is_reference: !h265::is_slnr(unit),
                is_intra: header.slice_type == h265::SliceType::I,
                is_idr: h265::is_idr(unit),
            },
            idr_pic_id: 0,
            id: Some(id),
            releases,
            drop_held: header.no_output_prior && h265::is_irap(unit),
            resets_dpb: new_sequence,
        };
        Some(H265Picture { picture, echo })
    }

    /// Picture order count of the current picture.
    fn picture_poc(
        &mut self,
        header: &h265::SliceHeader,
        unit: u8,
        temporal_id: u8,
        sps: &h265::Sps,
    ) -> Option<i32> {
        let poc = if h265::is_idr(unit) {
            0
        } else {
            let lsb = header.poc_lsb?;
            sps.poc_from_lsb(self.poc_tid0, lsb, h265::is_bla(unit))?
        };
        if h265::anchors_tid0(unit, temporal_id) {
            self.poc_tid0 = Some(poc);
        }
        Some(poc)
    }

    /// Reference sets resolved against the DPB with evictions collected.
    fn apply_rps(
        &mut self,
        poc: i32,
        header: &h265::SliceHeader,
        sps: &h265::Sps,
        releases: &mut Vec<ReferenceId>,
    ) -> Option<ResolvedSets> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut unused = Vec::new();
        if let Some(rps) = header.rps.as_ref() {
            let negative = usize::from(rps.negative_count);
            let positive = usize::from(rps.positive_count);
            for (delta, used) in rps.negative[..negative]
                .iter()
                .zip(rps.negative_used[..negative].iter())
                .chain(
                    rps.positive[..positive]
                        .iter()
                        .zip(rps.positive_used[..positive].iter()),
                )
            {
                let entry_poc = poc.checked_add(*delta)?;
                if *delta == 0 {
                    return None;
                }
                let entry = RefPicture {
                    id: self.find_exact(entry_poc),
                    frame_num: 0,
                    poc: entry_poc,
                    long_term: None,
                };
                if *used {
                    if *delta < 0 {
                        before.push(entry);
                    } else {
                        after.push(entry);
                    }
                } else {
                    unused.push(entry);
                }
            }
        }
        let max = sps.max_poc_lsb();
        let mask = i32::try_from(max - 1).ok()?;
        let mut lt = Vec::new();
        for lts in &header.lt {
            let (entry_poc, id) = if lts.msb_present {
                let narrowed = lts.poc_msb_present(poc, max)?;
                (narrowed, self.find_exact(narrowed))
            } else if let Some(found) = self.find_lsb(i32::try_from(lts.poc_lsb).ok()?, mask) {
                found
            } else {
                (i32::try_from(lts.poc_lsb).ok()?, None)
            };
            let entry = RefPicture {
                id,
                frame_num: 0,
                poc: entry_poc,
                long_term: Some(0),
            };
            if lts.used {
                lt.push(entry);
            } else {
                unused.push(entry);
            }
        }
        let mut keep = Vec::new();
        for entry in before
            .iter()
            .chain(after.iter())
            .chain(unused.iter())
            .chain(lt.iter())
        {
            if let Some(id) = entry.id() {
                if !keep.contains(&id) {
                    keep.push(id);
                }
            }
        }
        for entry in &self.dpb {
            if !keep.contains(&entry.id) {
                releases.push(entry.id);
            }
        }
        self.dpb.retain(|entry| keep.contains(&entry.id));
        Some(ResolvedSets { before, after, lt })
    }

    /// DPB identity of the picture with `poc`, if present.
    fn find_exact(&self, poc: i32) -> Option<ReferenceId> {
        self.dpb
            .iter()
            .find(|entry| entry.poc == poc)
            .map(|entry| entry.id)
    }

    /// DPB picture matching `lsb` under `mask`, if present.
    fn find_lsb(&self, lsb: i32, mask: i32) -> Option<(i32, Option<ReferenceId>)> {
        self.dpb
            .iter()
            .find(|entry| entry.poc & mask == lsb)
            .map(|entry| (entry.poc, Some(entry.id)))
    }

    /// Drop the DPB and wait for the next IDR or BLA.
    fn resync(&mut self) {
        self.dpb.clear();
        self.poc_tid0 = None;
        self.waiting = true;
    }
}

impl Default for H265Context {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolved reference sets of one H.265 picture.
struct ResolvedSets {
    /// Current-before references in list order.
    before: Vec<RefPicture>,
    /// Current-after references in list order.
    after: Vec<RefPicture>,
    /// Current long-term references in list order.
    lt: Vec<RefPicture>,
}

#[cfg(test)]
mod tests {
    use super::H265Context;
    use crate::au::{AccessUnit, AuSplitter};
    use crate::fixtures::{hex, split265};

    /// Fork `demo.h265` VPS.
    const VPS265: &str = "40010c01ffff01600000030090000003000003003c92809000";
    /// Fork `demo.h265` SPS.
    const SPS265: &str =
        "42010101600000030090000003000003003ca00a080f16592a4932bc05a0200000030020000003032100";
    /// Fork `demo.h265` PPS.
    const PPS265: &str = "4401c172b4624000";
    /// Fork `demo.h265` IDR slice head.
    const IDR265: &str =
        "2801af1d20a965478be0af3f4ae63151b2fffd273e7fa20b405b9ec15d0337a20427f6e32f9f583c57fee84d42429d33ffe3ff4aff62991873852f99fb8bf1a358a444ab906f2a97e66aa67a1c";
    /// Fork `demo.h265` first inter slice head.
    const T265: &str =
        "0201d0097e10c63a430a3d70fd7fb96259b8fc398744b4135036fbd45ed7fcdb5f3d45c649013c88aa2c8c2fdf";

    /// Access units out of parsed H.265 `payloads`.
    fn units265(payloads: &[Vec<u8>]) -> Vec<AccessUnit<crate::h265::ParsedNalu>> {
        let mut parser = crate::h265::H265Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split265(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            if let Some(unit) = splitter.put(parsed) {
                units.push(unit);
            }
        }
        if let Some(unit) = splitter.flush() {
            units.push(unit);
        }
        units
    }

    #[test]
    fn h265_demo_idr_then_inter() {
        let mut context = H265Context::new();
        let mut replay = units265(&[
            hex(VPS265),
            hex(SPS265),
            hex(PPS265),
            hex(IDR265),
            hex(T265),
        ]);
        assert_eq!(replay.len(), 2);
        let first = context.compile(replay.remove(0));
        assert_eq!(first.len(), 4);
        let Some(super::H265Instruction::Picture(idr)) = first.into_iter().next_back() else {
            panic!("want picture");
        };
        assert_eq!(idr.picture().poc(), 0);
        assert!(idr.picture().resets_dpb());
        assert!(idr.picture().l0().is_none());
        assert!(idr.echo().is_idr());
        let idr_id = idr.picture().id().expect("idr id");
        let second = context.compile(replay.remove(0));
        let Some(super::H265Instruction::Picture(p)) = second.into_iter().next() else {
            panic!("want picture");
        };
        assert!(p.picture().poc() > 0);
        assert!(!p.picture().resets_dpb());
        let union = p.picture().l0().expect("union");
        assert!(!union.is_empty());
        assert!(union.iter().any(|entry| entry.id() == Some(idr_id)));
        assert!(!p.echo().is_idr());
        assert_eq!(&p.picture().bytes()[..3], &[0, 0, 1]);
    }

    #[test]
    fn h265_duplicate_poc_resyncs_then_recovers() {
        let mut context = H265Context::default();
        let mut replay = units265(&[
            hex(VPS265),
            hex(SPS265),
            hex(PPS265),
            hex(IDR265),
            hex(T265),
        ]);
        let _ = context.compile(replay.remove(0));
        let _ = context.compile(replay.remove(0));
        let mut dup = units265(&[hex(VPS265), hex(SPS265), hex(PPS265), hex(T265)]);
        let dropped = context.compile(dup.remove(0));
        assert!(dropped
            .iter()
            .all(|instruction| !matches!(instruction, super::H265Instruction::Picture(_))));
        let mut idr = units265(&[hex(VPS265), hex(SPS265), hex(PPS265), hex(IDR265)]);
        let recovered = context.compile(idr.remove(0));
        let Some(super::H265Instruction::Picture(picture)) = recovered.into_iter().next_back()
        else {
            panic!("want picture");
        };
        assert!(picture.picture().resets_dpb());
    }
}
