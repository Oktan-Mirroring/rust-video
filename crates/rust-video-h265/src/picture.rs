//! Annex B ingest: parameter sets plus picture assembly.

use core::marker::PhantomData;

use crate::bits::BitReader;
use crate::error::{H265Error, H265Gate, NalAction, ParseReason};
use crate::framing::{
    is_cra_bla, is_idr, is_irap, is_rasl, rbsp, rbsp_prefix, split_nals, HEADER_PREFIX, MAX_PPS,
    MAX_SPS,
};
use crate::pps::{parse_pps, Pps};
use crate::slice::{parse_slice_prefix, parse_slice_rest, SliceHeader};
use crate::sps::{parse_sps, Sps};

/// One coded slice segment: header plus its NAL bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSlice {
    /// Parsed slice header.
    pub header: SliceHeader,
    /// NAL bytes without the start code.
    pub nal: Vec<u8>,
    /// NAL unit type.
    pub nal_type: u8,
    /// Temporal id.
    pub temporal_id: u8,
    /// Picture set id.
    pub pps_id: u8,
}

/// One coded picture: segments sharing set and type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Picture {
    /// Picture slices in bitstream order.
    pub slices: Vec<PendingSlice>,
    /// True for IDR pictures.
    pub idr: bool,
    /// True for random access pictures.
    pub irap: bool,
    /// True for RASL pictures.
    pub rasl: bool,
}

impl Picture {
    /// Picture set id.
    #[must_use]
    pub fn pps_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.pps_id)
    }

    /// Temporal id.
    #[must_use]
    pub fn temporal_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.temporal_id)
    }

    /// True when prior pictures drop without output.
    #[must_use]
    pub fn no_output_prior(&self) -> bool {
        self.slices
            .first()
            .is_some_and(|slice| slice.header.no_output_prior)
    }

    /// Picture order count LSB, absent on IDR.
    #[must_use]
    pub fn poc_lsb(&self) -> Option<u32> {
        self.slices.first()?.header.poc_lsb
    }
}

/// Annex B ingest: parameter sets plus picture assembly.
pub struct Ingest<E: H265Error, G: H265Gate<E>> {
    /// Seen video sets by id.
    vps: [bool; MAX_SPS],
    /// Stored sequence sets by id.
    sps: Box<[Option<Sps>; MAX_SPS]>,
    /// Stored picture sets by id.
    pps: Box<[Option<Pps>; MAX_PPS]>,
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
    /// Completed pictures in decode order.
    pictures: u64,
    /// Leading pictures of the open random access point undecodable.
    rasl_undecodable: bool,
    /// Sequence store writes behind mirror sync.
    sps_version: u64,
    /// Picture store writes behind mirror sync.
    pps_version: u64,
    /// Tier error and gate under ingest.
    gate: PhantomData<(E, G)>,
}

impl<E: H265Error, G: H265Gate<E>> Default for Ingest<E, G> {
    /// Empty ingest.
    fn default() -> Self {
        Self::new()
    }
}

impl<E: H265Error, G: H265Gate<E>> Ingest<E, G> {
    /// Empty ingest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vps: [false; MAX_SPS],
            sps: Box::new(core::array::from_fn(|_| None)),
            pps: Box::new(core::array::from_fn(|_| None)),
            pending: Vec::new(),
            pictures: 0,
            rasl_undecodable: false,
            sps_version: 0,
            pps_version: 0,
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

    /// Sequence and picture store versions behind mirror sync.
    #[must_use]
    pub fn versions(&self) -> (u64, u64) {
        (self.sps_version, self.pps_version)
    }

    /// Pictures completed by `data`; trailing slices complete the picture.
    ///
    /// # Errors
    ///
    /// Returns `Err` on malformed NAL units or tier-declined constructs.
    ///
    /// # Arguments
    ///
    /// * `data` - Annex B bytes under ingest.
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
    /// Returns `Err` when the open picture fails validation.
    pub fn finish(&mut self) -> Result<Option<Picture>, E> {
        self.complete_picture()
    }

    /// One NAL unit into the ingest state.
    fn ingest_nal(&mut self, nal: &[u8], pictures: &mut Vec<Picture>) -> Result<(), E> {
        let Some(header) = nal.get(0..2) else {
            return Ok(());
        };
        if header[0] >> 7 != 0 {
            return Err(E::parse(ParseReason::NalForbiddenBit));
        }
        let nal_type = (header[0] >> 1) & 0x3F;
        let layer_id = ((u16::from(header[0]) & 1) << 5) | (u16::from(header[1]) >> 3);
        let temporal_plus1 = header[1] & 7;
        if temporal_plus1 == 0 {
            return Err(E::parse(ParseReason::NalTemporalId));
        }
        let temporal_id = temporal_plus1 - 1;
        match G::classify_nal(nal_type, layer_id)? {
            NalAction::Vcl => self.ingest_vcl(nal, nal_type, temporal_id, pictures),
            NalAction::Vps => self.ingest_vps(nal),
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

    /// Coded slice segment into the open picture.
    fn ingest_vcl(
        &mut self,
        nal: &[u8],
        nal_type: u8,
        temporal_id: u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), E> {
        let mut prefix_buf = [0u8; HEADER_PREFIX];
        let prefix_body = rbsp_prefix(nal, &mut prefix_buf);
        let Some(prefix_body) = prefix_body.get(2..) else {
            return Err(E::parse(ParseReason::SliceWithoutHeader));
        };
        let (parsed, pps_id) = self.parse_vcl(prefix_body, nal_type).or_else(|_| {
            let bytes = rbsp(nal);
            let Some(body) = bytes.get(2..) else {
                return Err(E::parse(ParseReason::SliceWithoutHeader));
            };
            self.parse_vcl(body, nal_type)
        })?;
        G::check_slice_extra(&parsed)?;
        if parsed.first && !self.pending.is_empty() {
            if let Some(picture) = self.complete_picture()? {
                pictures.push(picture);
            }
        }
        self.pending.push(PendingSlice {
            header: parsed,
            nal: nal.to_vec(),
            nal_type,
            temporal_id,
            pps_id,
        });
        Ok(())
    }

    /// Slice header plus set id over an RBSP `body` with the NAL header stripped.
    fn parse_vcl(&self, body: &[u8], nal_type: u8) -> Result<(SliceHeader, u8), E> {
        let mut reader = BitReader::new(body);
        let prefix = parse_slice_prefix(&mut reader, nal_type)?;
        let pps_id = prefix.2;
        let pps = self
            .pps(pps_id)
            .ok_or(E::parse(ParseReason::SliceCitesUnknownPps))?;
        let sps = self
            .sps(pps.sps_id)
            .ok_or(E::parse(ParseReason::SliceCitesUnknownSps))?;
        Ok((
            parse_slice_rest(&mut reader, prefix, nal_type, sps, pps)?,
            pps_id,
        ))
    }

    /// Video set id into the store.
    fn ingest_vps(&mut self, nal: &[u8]) -> Result<(), E> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(E::parse(ParseReason::VpsWithoutBody));
        };
        let mut reader = BitReader::new(body);
        let id = reader.read_bits(4, "vps_video_parameter_set_id")?;
        let id =
            usize::try_from(id).map_err(|_| E::parse(ParseReason::Range { syntax: "vps id" }))?;
        if id < MAX_SPS {
            self.vps[id] = true;
        }
        Ok(())
    }

    /// Sequence set into the store.
    fn ingest_sps(&mut self, nal: &[u8]) -> Result<(), E> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(E::parse(ParseReason::SpsWithoutBody));
        };
        let mut probe = BitReader::new(body);
        let vps_id = probe.read_bits(4, "sps_video_parameter_set_id")?;
        let vps_id =
            u8::try_from(vps_id).map_err(|_| E::parse(ParseReason::Range { syntax: "vps id" }))?;
        let seen = self.vps.get(usize::from(vps_id)).copied().unwrap_or(false);
        G::check_sps_citation(vps_id, seen)?;
        let sps = parse_sps::<E, G>(body)?;
        let id = sps.id;
        self.sps[usize::from(id)] = Some(sps);
        self.sps_version = self.sps_version.wrapping_add(1);
        Ok(())
    }

    /// Picture set into the store.
    fn ingest_pps(&mut self, nal: &[u8]) -> Result<(), E> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(E::parse(ParseReason::PpsWithoutBody));
        };
        let mut probe = BitReader::new(body);
        probe.read_ue("pic_parameter_set_id")?;
        let sps_id = probe.read_ue("seq_parameter_set_id")?;
        let sps_id = u8::try_from(sps_id).map_err(|_| {
            E::parse(ParseReason::Range {
                syntax: "seq_parameter_set_id",
            })
        })?;
        let sps = self
            .sps(sps_id)
            .ok_or(E::parse(ParseReason::PpsCitesUnknownSps))?;
        let pps = parse_pps::<E, G>(body, Some(sps))?;
        let id = pps.id;
        self.pps[usize::from(id)] = Some(pps);
        self.pps_version = self.pps_version.wrapping_add(1);
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
            if slice.pps_id != first.pps_id || slice.nal_type != first.nal_type {
                return Err(E::parse(ParseReason::PictureSlicesDisagree));
            }
        }
        let addrs: Vec<u32> = slices.iter().map(|slice| slice.header.address).collect();
        let firsts: Vec<bool> = slices.iter().map(|slice| slice.header.first).collect();
        G::check_slice_order(&addrs, &firsts)?;
        let nal_type = first.nal_type;
        let picture = Picture {
            idr: is_idr(nal_type),
            irap: is_irap(nal_type),
            rasl: is_rasl(nal_type),
            slices,
        };
        if is_cra_bla(nal_type) {
            self.rasl_undecodable = self.pictures == 0;
        } else if is_idr(nal_type) {
            self.rasl_undecodable = false;
        }
        self.pictures += 1;
        if picture.rasl && self.rasl_undecodable {
            return Ok(None);
        }
        Ok(Some(picture))
    }
}
