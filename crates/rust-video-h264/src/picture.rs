//! Picture assembly over Annex B NAL units.

use core::marker::PhantomData;

use crate::bits::BitReader;
use crate::error::{H264Error, H264Gate, NalAction, ParseKind};
use crate::nal::{rbsp, rbsp_prefix, split_nals, HEADER_PREFIX, MAX_PPS, MAX_SPS};
use crate::pps::{parse_pps, parse_pps_ids, Pps};
use crate::slice::{parse_slice_prefix, parse_slice_rest, SliceHeader};
use crate::sps::{parse_sps, Sps};

/// Annex B ingest: parameter sets plus picture assembly.
pub struct Ingest<E: H264Error, G: H264Gate<E>> {
    /// Stored sequence sets by id.
    sps: Box<[Option<Sps>; MAX_SPS]>,
    /// Stored picture sets by id.
    pps: Box<[Option<Pps>; MAX_PPS]>,
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
    /// Tier error under construction.
    error: PhantomData<E>,
    /// Tier gate under enforcement.
    gate: PhantomData<G>,
}

impl<E: H264Error, G: H264Gate<E>> Default for Ingest<E, G> {
    /// Empty ingest.
    fn default() -> Self {
        Self::new()
    }
}

impl<E: H264Error, G: H264Gate<E>> Ingest<E, G> {
    /// Empty ingest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sps: Box::new(core::array::from_fn(|_| None)),
            pps: Box::new(core::array::from_fn(|_| None)),
            pending: Vec::new(),
            error: PhantomData,
            gate: PhantomData,
        }
    }

    /// Sequence set `id`, if stored.
    #[must_use]
    pub fn sps(&self, id: u8) -> Option<&Sps> {
        self.sps.get(usize::from(id))?.as_ref()
    }

    /// Picture set `id`, if stored.
    #[must_use]
    pub fn pps(&self, id: u8) -> Option<&Pps> {
        self.pps.get(usize::from(id))?.as_ref()
    }

    /// Pictures completed by `data`; trailing slices complete the picture.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncated or out-of-range syntax, slices citing
    /// unseen sets, disagreeing picture slices, or a unit the gate declines.
    pub fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Picture>, E> {
        let mut pictures = Vec::new();
        for nal in split_nals(data) {
            self.ingest_nal(nal, &mut pictures)?;
        }
        if let Some(picture) = self.complete_picture()? {
            pictures.push(picture);
        }
        Ok(pictures)
    }

    /// Open picture, if any slices arrived without a boundary.
    ///
    /// # Errors
    ///
    /// Returns `Err` on disagreeing picture slices or slices out of order.
    pub fn finish(&mut self) -> Result<Option<Picture>, E> {
        self.complete_picture()
    }

    /// One NAL unit into the ingest state.
    fn ingest_nal(&mut self, nal: &[u8], pictures: &mut Vec<Picture>) -> Result<(), E> {
        let Some(&header) = nal.first() else {
            return Ok(());
        };
        if header >> 7 != 0 {
            return Err(E::parse(ParseKind::NalForbiddenBit));
        }
        let ref_idc = (header >> 5) & 3;
        match G::classify_nal(header & 0x1F)? {
            NalAction::Vcl => self.ingest_vcl(nal, header, ref_idc, pictures),
            NalAction::Sps => self.ingest_sps(nal),
            NalAction::Pps => self.ingest_pps(nal),
            NalAction::Complete => {
                if let Some(picture) = self.complete_picture()? {
                    pictures.push(picture);
                }
                Ok(())
            }
            NalAction::Ignore => Ok(()),
        }
    }

    /// Coded slice into the open picture.
    fn ingest_vcl(
        &mut self,
        nal: &[u8],
        header: u8,
        ref_idc: u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), E> {
        let idr = header & 0x1F == 5;
        let mut prefix_buf = [0u8; HEADER_PREFIX];
        let prefix_body = rbsp_prefix(nal, &mut prefix_buf);
        let Some(prefix_body) = prefix_body.get(1..) else {
            return Err(E::parse(ParseKind::SliceWithoutHeader));
        };
        // Prefix failure retries whole: a prefix error reproduces on the full bytes.
        let parsed = match self.parse_vcl(prefix_body, idr, ref_idc) {
            Err(_) => {
                let bytes = rbsp(nal);
                let Some(body) = bytes.get(1..) else {
                    return Err(E::parse(ParseKind::SliceWithoutHeader));
                };
                self.parse_vcl(body, idr, ref_idc)?
            }
            parsed => parsed?,
        };
        if parsed.first_mb == 0 && !self.pending.is_empty() {
            if let Some(picture) = self.complete_picture()? {
                pictures.push(picture);
            }
        }
        self.pending.push(PendingSlice {
            header: parsed,
            nal: nal.to_vec(),
            ref_idc,
            idr,
        });
        Ok(())
    }

    /// Slice header over an RBSP `body` with the NAL header stripped.
    fn parse_vcl(&self, body: &[u8], idr: bool, ref_idc: u8) -> Result<SliceHeader, E> {
        let mut reader = BitReader::new(body);
        let prefix = parse_slice_prefix(&mut reader)?;
        let pps = self
            .pps(prefix.3)
            .ok_or(E::parse(ParseKind::SliceCitesUnknownPps))?;
        let sps = self
            .sps(pps.seq_parameter_set_id)
            .ok_or(E::parse(ParseKind::SliceCitesUnknownSps))?;
        parse_slice_rest::<E, G>(&mut reader, prefix, idr, ref_idc, sps, pps)
    }

    /// Sequence set into the store.
    fn ingest_sps(&mut self, nal: &[u8]) -> Result<(), E> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(1..) else {
            return Err(E::parse(ParseKind::SpsWithoutBody));
        };
        let sps = parse_sps::<E, G>(body)?;
        let id = sps.seq_parameter_set_id;
        self.sps[usize::from(id)] = Some(sps);
        Ok(())
    }

    /// Picture set into the store.
    fn ingest_pps(&mut self, nal: &[u8]) -> Result<(), E> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(1..) else {
            return Err(E::parse(ParseKind::PpsWithoutBody));
        };
        let (_, sps_id) = parse_pps_ids(body)?;
        let sps = self
            .sps(sps_id)
            .ok_or(E::parse(ParseKind::PpsCitesUnknownSps))?
            .clone();
        let pps = parse_pps::<E, G>(body, Some(&sps))?;
        let id = pps.pic_parameter_set_id;
        self.pps[usize::from(id)] = Some(pps);
        Ok(())
    }

    /// Open slices as one validated picture.
    fn complete_picture(&mut self) -> Result<Option<Picture>, E> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let slices = core::mem::take(&mut self.pending);
        let first = &slices[0];
        for slice in &slices {
            if slice.header.frame_num != first.header.frame_num
                || slice.header.pps_id != first.header.pps_id
                || slice.idr != first.idr
                || slice.ref_idc != first.ref_idc
            {
                return Err(E::parse(ParseKind::PictureSlicesDisagree));
            }
        }
        let first_mbs: Vec<u32> = slices.iter().map(|slice| slice.header.first_mb).collect();
        G::check_slice_order(&first_mbs)?;
        Ok(Some(Picture {
            idr: first.idr,
            slices,
        }))
    }
}

/// One coded slice: header plus its NAL bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSlice {
    /// Parsed slice header.
    pub header: SliceHeader,
    /// NAL bytes without the start code.
    pub nal: Vec<u8>,
    /// NAL reference idc.
    pub ref_idc: u8,
    /// True for IDR slices.
    pub idr: bool,
}

/// One coded picture: slices sharing frame number and set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// Picture slices in bitstream order.
    pub slices: Vec<PendingSlice>,
    /// True for IDR pictures.
    pub idr: bool,
}

impl Picture {
    /// True when the picture is used for reference.
    #[must_use]
    pub fn ref_pic(&self) -> bool {
        self.slices.first().is_some_and(|slice| slice.ref_idc != 0)
    }

    /// Picture frame number.
    #[must_use]
    pub fn frame_num(&self) -> u32 {
        self.slices
            .first()
            .map_or(0, |slice| slice.header.frame_num)
    }

    /// Picture set id.
    #[must_use]
    pub fn pps_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.header.pps_id)
    }

    /// True when every slice is intra.
    #[must_use]
    pub fn intra(&self) -> bool {
        !self.slices.is_empty()
            && self
                .slices
                .iter()
                .all(|slice| slice.header.slice_type.is_intra())
    }
}

#[cfg(test)]
mod tests {
    use crate::error::test_support::TestError;
    use crate::error::StrictGate;
    use crate::test_support::{
        idr_slice_body, nal, p_slice_body, packet, pps_body, sps_body, BitWriter,
    };

    use super::Ingest;

    #[test]
    fn ingest_assembles_packet_picture() {
        let sps_nal = nal(7, 3, &sps_body());
        let pps_nal = nal(8, 3, &pps_body());
        let idr_nal = nal(5, 2, &idr_slice_body());
        let data = packet(&[sps_nal, pps_nal, idr_nal]);
        let mut ingest = Ingest::<TestError, StrictGate>::new();
        let pictures = ingest.ingest_packet(&data).expect("packet");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].idr);
        assert!(pictures[0].ref_pic());
        assert!(pictures[0].intra());
        assert_eq!(pictures[0].frame_num(), 0);

        let p_nal = nal(1, 2, &p_slice_body(1, 2));
        let pictures = ingest.ingest_packet(&packet(&[p_nal])).expect("packet");
        assert_eq!(pictures.len(), 1);
        assert!(!pictures[0].idr);
        assert_eq!(pictures[0].pps_id(), 0);
    }

    #[test]
    fn ingest_splits_on_aud() {
        let first = nal(5, 2, &idr_slice_body());
        let aud = nal(9, 0, &[0x10, 0x80]);
        let second = nal(5, 2, &idr_slice_body());
        let data = packet(&[first, aud, second]);
        let mut ingest = Ingest::<TestError, StrictGate>::new();
        ingest
            .ingest_packet(&packet(&[nal(7, 3, &sps_body()), nal(8, 3, &pps_body())]))
            .expect("sets");
        let pictures = ingest.ingest_packet(&data).expect("packet");
        assert_eq!(pictures.len(), 2);
    }

    #[test]
    fn ingest_rejects_unknown_set_and_aso() {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(2);
        writer.ue(9);
        let body = writer.finish_ebsp();
        let mut ingest = Ingest::<TestError, StrictGate>::new();
        assert!(ingest.ingest_packet(&packet(&[nal(5, 2, &body)])).is_err());

        let mut first = BitWriter::new();
        first.ue(50);
        first.ue(2);
        first.ue(0);
        first.bits(0, 4);
        first.bit(false);
        first.ue(0);
        first.bits(0, 4);
        first.ue(0);
        first.bit(false);
        first.bit(false);
        first.se(0);
        first.ue(0);
        first.se(0);
        first.se(0);
        let mut second = BitWriter::new();
        second.ue(10);
        second.ue(2);
        second.ue(0);
        second.bits(0, 4);
        second.bit(false);
        second.ue(0);
        second.bits(0, 4);
        second.ue(0);
        second.bit(false);
        second.bit(false);
        second.se(0);
        second.ue(0);
        second.se(0);
        second.se(0);
        let mut ingest = Ingest::<TestError, StrictGate>::new();
        ingest
            .ingest_packet(&packet(&[nal(7, 3, &sps_body()), nal(8, 3, &pps_body())]))
            .expect("sets");
        let data = packet(&[
            nal(5, 2, &first.finish_ebsp()),
            nal(5, 2, &second.finish_ebsp()),
        ]);
        assert!(ingest.ingest_packet(&data).is_err());
    }

    #[test]
    fn ingest_rejects_garbage() {
        let mut ingest = Ingest::<TestError, StrictGate>::new();
        assert!(ingest
            .ingest_packet(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00])
            .is_err());
    }
}
