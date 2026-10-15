//! H.264 and HEVC decode plus probed shells behind the caps `Decoder` trait.

use crate::decode::{DecodeSession, HevcSubmission, PictureSubmission};
use crate::device::D3d12Device;
use crate::dpb::{Decoded, Dpb};
use crate::error::{D3d12Error, SkipReason};
use crate::guid::Codec;
use crate::h264::{Crop, Ingest, Picture};
#[cfg(target_os = "windows")]
use crate::texture::GpuTexture;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport, GpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Lease, SharedDevice};

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct D3d12Frame {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
}

impl D3d12Frame {
    /// Frame identity.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        self.identity
    }

    /// NV12 frame bytes in row-major order.
    #[must_use]
    pub fn nv12(&self) -> &[u8] {
        &self.data
    }

    /// Budget bytes the frame holds.
    #[must_use]
    pub fn lease_bytes(&self) -> u64 {
        self.lease.bytes()
    }

    /// Platform export handle for the frame.
    ///
    /// Opaque backend descriptor.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        GpuExport::Opaque { name: "d3d12" }
    }

    /// CPU copy split into NV12 planes.
    #[must_use]
    pub fn to_cpu(&self) -> Option<CpuFrame> {
        let size = self.identity.size();
        let luma = u64::from(size.width()) * u64::from(size.height());
        let y_len = usize::try_from(luma).ok()?;
        let uv_len = y_len / 2;
        if self.data.len() < y_len + uv_len {
            return None;
        }
        let y = self.data.slice(..y_len);
        let uv = self.data.slice(y_len..y_len + uv_len);
        let stride = size.width();
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![Plane::new(y, stride)?, Plane::new(uv, stride)?],
        )
    }
}

/// Active sequence behind the live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Active {
    /// Sequence set id.
    sps_id: u8,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Visible rectangle, absent when uncropped.
    crop: Option<Crop>,
    /// Visible size in luma samples.
    visible: PictureSize,
}

/// Real decoder over ingest plus a video decode session.
macro_rules! real_decoder {
    ($decoder:ident, $codec:ident, $ingest:ty, $picture:ty, $submission:ty, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device owning the decode session.
            device: &'a D3d12Device,
            /// Annex B ingest: sets plus picture assembly.
            ingest: $ingest,
            /// Live decode session, created from the first sequence set.
            session: Option<DecodeSession>,
            /// VRAM budget shared with the device.
            shared: SharedDevice,
            /// Sequence behind the live session.
            active: Option<Active>,
            /// Driver-echoed picture serial.
            feedback: u32,
            /// Next frame generation number.
            generation: u64,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; the session starts on the first picture.
            #[must_use]
            pub fn open(device: &'a D3d12Device) -> Self {
                Self {
                    device,
                    ingest: <$ingest>::new(),
                    session: None,
                    shared: device.shared().clone(),
                    active: None,
                    feedback: 1,
                    generation: 0,
                }
            }

            /// Decode `picture` into `frames`.
            fn decode_picture(
                &mut self,
                picture: &$picture,
                frames: &mut Vec<D3d12Frame>,
            ) -> Result<(), D3d12Error> {
                let outcome = self.decode_picture_inner(picture, frames);
                if outcome.is_err() {
                    self.reset_stream();
                }
                outcome
            }

            /// Decode `picture` into `frames` without resetting on failure.
            fn decode_picture_inner(
                &mut self,
                picture: &$picture,
                frames: &mut Vec<D3d12Frame>,
            ) -> Result<(), D3d12Error> {
                let (active, submission) = self.prepare_picture(picture)?;
                let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
                let nv12 = submission.submit(session)?;
                let luma = u64::from(active.visible.width()) * u64::from(active.visible.height());
                let y_len = usize::try_from(luma)
                    .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
                if nv12.len() != y_len + y_len / 2 {
                    return Err(D3d12Error::Unsupported(
                        "readback short of extent".to_owned(),
                    ));
                }
                let identity = GpuFrame::new(active.visible, PixelFormat::Nv12, self.generation);
                self.generation = self.generation.wrapping_add(1);
                let bytes = Bytes::from(nv12);
                let lease = self.shared.try_acquire(bytes.len() as u64)?;
                frames.push(D3d12Frame {
                    identity,
                    data: bytes,
                    lease,
                });
                Ok(())
            }

            /// Session plus arguments for `picture`.
            fn prepare_picture(
                &mut self,
                picture: &$picture,
            ) -> Result<(Active, $submission), D3d12Error> {
                let active = {
                    let pps = self
                        .ingest
                        .pps(picture.pps_id())
                        .ok_or(D3d12Error::NoSession)?;
                    let sps = self
                        .ingest
                        .sps(pps.seq_parameter_set_id)
                        .ok_or(D3d12Error::NoSession)?;
                    let visible = PictureSize::new(sps.visible_width(), sps.visible_height())
                        .ok_or_else(|| D3d12Error::Parse("crop empties picture".to_owned()))?;
                    Active {
                        sps_id: sps.seq_parameter_set_id,
                        width: sps.width(),
                        height: sps.height(),
                        crop: sps.crop,
                        visible,
                    }
                };
                if self.active != Some(active) {
                    let session = DecodeSession::open(
                        self.device,
                        Codec::$codec,
                        active.width,
                        active.height,
                        active.crop,
                    )?;
                    self.session = Some(session);
                    self.active = Some(active);
                }
                let active = self.active.ok_or(D3d12Error::NoSession)?;
                let submission = {
                    let pps = self
                        .ingest
                        .pps(picture.pps_id())
                        .ok_or(D3d12Error::NoSession)?;
                    let sps = self
                        .ingest
                        .sps(pps.seq_parameter_set_id)
                        .ok_or(D3d12Error::NoSession)?;
                    <$submission>::build(sps, pps, picture, self.feedback)?
                };
                self.feedback = self.feedback.wrapping_add(1);
                Ok((active, submission))
            }

            /// `picture` into GPU `textures` without resetting on failure.
            #[cfg(target_os = "windows")]
            fn texture_inner(
                &mut self,
                picture: &$picture,
                textures: &mut Vec<GpuTexture>,
            ) -> Result<(), D3d12Error> {
                let (active, submission) = self.prepare_picture(picture)?;
                let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
                let resource = submission.submit_texture(session)?;
                let texture = GpuTexture::new(resource, active.visible, self.generation);
                self.generation = self.generation.wrapping_add(1);
                textures.push(texture);
                Ok(())
            }

            /// `picture` into GPU `textures`.
            #[cfg(target_os = "windows")]
            fn texture_picture(
                &mut self,
                picture: &$picture,
                textures: &mut Vec<GpuTexture>,
            ) -> Result<(), D3d12Error> {
                let outcome = self.texture_inner(picture, textures);
                if outcome.is_err() {
                    self.reset_stream();
                }
                outcome
            }

            /// Drop the live session and sequence after a mid-picture failure.
            fn reset_stream(&mut self) {
                self.session = None;
                self.active = None;
            }

            /// GPU textures decoded from `packet`, possibly none.
            ///
            /// # Errors
            ///
            /// Returns `Err` without a `codec` profile or when the backend
            /// cannot decode `packet`.
            #[cfg(target_os = "windows")]
            pub(crate) fn decode_texture(
                &mut self,
                packet: &Packet,
            ) -> Result<Vec<GpuTexture>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                let pictures = self.ingest.ingest_packet(packet.data())?;
                if pictures.len() > 1 {
                    return Err(D3d12Error::Unsupported(
                        "multi-picture packet needs per-picture encode".to_owned(),
                    ));
                }
                let mut textures = Vec::new();
                for picture in &pictures {
                    self.texture_picture(picture, &mut textures)?;
                }
                Ok(textures)
            }

            /// GPU textures held back for reordering or buffering.
            ///
            /// # Errors
            ///
            /// Returns `Err` when the backend cannot release its held frames.
            #[cfg(target_os = "windows")]
            pub(crate) fn flush_texture(&mut self) -> Result<Vec<GpuTexture>, D3d12Error> {
                let mut textures = Vec::new();
                if let Some(picture) = self.ingest.finish()? {
                    self.texture_picture(&picture, &mut textures)?;
                }
                Ok(textures)
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d12Frame;
            type Error = D3d12Error;

            fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                let pictures = self.ingest.ingest_packet(packet.data())?;
                let mut frames = Vec::new();
                for picture in &pictures {
                    self.decode_picture(picture, &mut frames)?;
                }
                Ok(frames)
            }

            fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
                let mut frames = Vec::new();
                if let Some(picture) = self.ingest.finish()? {
                    self.decode_picture(&picture, &mut frames)?;
                }
                Ok(frames)
            }
        }
    };
}

real_decoder!(
    HevcDecoder,
    Hevc,
    crate::hevc::Ingest,
    crate::hevc::Picture,
    HevcSubmission,
    "HEVC decoder over a D3D12 device. Decodes IDR pictures only; inter pictures and non-IDR intra pictures miss loud."
);

/// H.264 decoder over a D3D12 device.
pub struct H264Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d12Device,
    /// Annex B ingest: sets plus picture assembly.
    ingest: Ingest,
    /// Live picture buffer, created from the first sequence set.
    dpb: Option<Dpb>,
    /// Live decode session, created from the first sequence set.
    session: Option<DecodeSession>,
    /// VRAM budget sized to the adapter memory.
    shared: SharedDevice,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
}

impl<'a> H264Decoder<'a> {
    /// Decoder over `device`; the session starts on the first picture.
    #[must_use]
    pub fn open(device: &'a D3d12Device) -> Self {
        Self {
            device,
            ingest: Ingest::new(),
            dpb: None,
            session: None,
            shared: device.shared().clone(),
            active: None,
            generation: 0,
        }
    }

    /// Decode `picture` into `frames`.
    fn decode_picture(
        &mut self,
        picture: &Picture,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.reconfigure(active, frames)?;
        }
        let active = self.active.ok_or(D3d12Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Err(error) = self.submit_frames(&decoded, &active, frames) {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// `decoded` submission plus display-order outputs into `frames`.
    fn submit_frames(
        &mut self,
        decoded: &Decoded,
        active: &Active,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any readback: outputs may name the submission slot.
        session.submit(&decoded.submission)?;
        for output in &decoded.outputs {
            let nv12 = session.download(output.slot())?;
            let frame = emit_frame(&self.shared, nv12, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Drop the live session, buffer, and sequence after a mid-picture failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.active = None;
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure(
        &mut self,
        active: Active,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        if let Some(previous) = self.active {
            self.drain_live(&previous, frames)?;
        }
        self.open_session(active)
    }

    /// Session plus picture buffer for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d12Error> {
        let session = DecodeSession::open(
            self.device,
            Codec::H264,
            active.width,
            active.height,
            active.crop,
        )?;
        self.session = Some(session);
        let sps = self
            .ingest
            .sps(active.sps_id)
            .ok_or(D3d12Error::NoSession)?;
        self.dpb = Some(Dpb::new(sps));
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `picture`'s parameter sets.
    fn sequence(&self, picture: &Picture) -> Result<Active, D3d12Error> {
        let pps = self
            .ingest
            .pps(picture.pps_id())
            .ok_or(D3d12Error::NoSession)?;
        let sps = self
            .ingest
            .sps(pps.seq_parameter_set_id)
            .ok_or(D3d12Error::NoSession)?;
        let visible = PictureSize::new(sps.visible_width(), sps.visible_height())
            .ok_or_else(|| D3d12Error::Parse("crop empties picture".to_owned()))?;
        Ok(Active {
            sps_id: sps.seq_parameter_set_id,
            width: sps.width(),
            height: sps.height(),
            crop: sps.crop,
            visible,
        })
    }

    /// Submission plus display-order outputs for `picture`.
    fn decode_dpb(&mut self, picture: &Picture) -> Result<Decoded, D3d12Error> {
        let pps = self
            .ingest
            .pps(picture.pps_id())
            .ok_or(D3d12Error::NoSession)?;
        let sps = self
            .ingest
            .sps(pps.seq_parameter_set_id)
            .ok_or(D3d12Error::NoSession)?;
        self.dpb
            .as_mut()
            .ok_or(D3d12Error::NoSession)?
            .decode(picture, sps, pps)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        for output in dpb.drain() {
            let nv12 = session.download(output.slot())?;
            let frame = emit_frame(&self.shared, nv12, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Decode `picture` into GPU `textures`.
    #[cfg(target_os = "windows")]
    fn texture_picture(
        &mut self,
        picture: &Picture,
        textures: &mut Vec<GpuTexture>,
    ) -> Result<(), D3d12Error> {
        let outcome = self.texture_inner(picture, textures);
        if outcome.is_err() {
            self.reset_stream();
        }
        outcome
    }

    /// Decode `picture` into GPU `textures` without resetting on failure.
    #[cfg(target_os = "windows")]
    fn texture_inner(
        &mut self,
        picture: &Picture,
        textures: &mut Vec<GpuTexture>,
    ) -> Result<(), D3d12Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.reconfigure_texture(active, textures)?;
        }
        let active = self.active.ok_or(D3d12Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Err(error) = self.submit_textures(&decoded, &active, textures) {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// `decoded` submission plus display-order outputs into GPU `textures`.
    #[cfg(target_os = "windows")]
    fn submit_textures(
        &mut self,
        decoded: &Decoded,
        active: &Active,
        textures: &mut Vec<GpuTexture>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any texture handoff: outputs may name the submission slot.
        session.submit(&decoded.submission)?;
        for output in &decoded.outputs {
            let resource = session.texture_cropped(output.slot())?;
            textures.push(GpuTexture::new(resource, active.visible, self.generation));
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    /// Session for `active`, draining the previous sequence first.
    #[cfg(target_os = "windows")]
    fn reconfigure_texture(
        &mut self,
        active: Active,
        textures: &mut Vec<GpuTexture>,
    ) -> Result<(), D3d12Error> {
        if let Some(previous) = self.active {
            self.drain_live_texture(&previous, textures)?;
        }
        self.open_session(active)
    }

    /// Live DPB outputs into GPU `textures`.
    #[cfg(target_os = "windows")]
    fn drain_live_texture(
        &mut self,
        active: &Active,
        textures: &mut Vec<GpuTexture>,
    ) -> Result<(), D3d12Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        for output in dpb.drain() {
            let resource = session.texture_cropped(output.slot())?;
            textures.push(GpuTexture::new(resource, active.visible, self.generation));
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    /// GPU textures decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` without an H.264 profile or when the backend
    /// cannot decode `packet`.
    #[cfg(target_os = "windows")]
    pub(crate) fn decode_texture(
        &mut self,
        packet: &Packet,
    ) -> Result<Vec<GpuTexture>, D3d12Error> {
        if !self.device.supports(Codec::H264) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::H264 }.into());
        }
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut textures = Vec::new();
        for picture in &pictures {
            self.texture_picture(picture, &mut textures)?;
        }
        Ok(textures)
    }

    /// GPU textures held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    #[cfg(target_os = "windows")]
    pub(crate) fn flush_texture(&mut self) -> Result<Vec<GpuTexture>, D3d12Error> {
        let mut textures = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            self.texture_picture(&picture, &mut textures)?;
        }
        if let Some(active) = self.active {
            self.drain_live_texture(&active, &mut textures)?;
        }
        Ok(textures)
    }
}

impl Decoder for H264Decoder<'_> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
        if !self.device.supports(Codec::H264) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::H264 }.into());
        }
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut frames = Vec::new();
        for picture in &pictures {
            self.decode_picture(picture, &mut frames)?;
        }
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
        let mut frames = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            self.decode_picture(&picture, &mut frames)?;
        }
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

/// Frame for NV12 `nv12` at `generation`.
fn emit_frame(
    shared: &SharedDevice,
    nv12: Vec<u8>,
    active: &Active,
    generation: u64,
) -> Result<D3d12Frame, D3d12Error> {
    let luma = u64::from(active.visible.width()) * u64::from(active.visible.height());
    let y_len = usize::try_from(luma)
        .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
    if nv12.len() != y_len + y_len / 2 {
        return Err(D3d12Error::Unsupported(
            "readback short of extent".to_owned(),
        ));
    }
    let identity = GpuFrame::new(active.visible, PixelFormat::Nv12, generation);
    let bytes = Bytes::from(nv12);
    let lease = shared.try_acquire(bytes.len() as u64)?;
    Ok(D3d12Frame {
        identity,
        data: bytes,
        lease,
    })
}

/// Probed decoder shell; the picture builder is pending.
macro_rules! shell {
    ($decoder:ident, $codec:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device behind the profile probe.
            device: &'a D3d12Device,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; decode reports the pending builder.
            #[must_use]
            pub fn open(device: &'a D3d12Device) -> Self {
                Self { device }
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d12Frame;
            type Error = D3d12Error;

            fn decode(&mut self, _packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                Err(D3d12Error::Unsupported(
                    concat!(stringify!($codec), " picture builder pending").to_lowercase(),
                ))
            }

            fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
                Ok(Vec::new())
            }
        }
    };
}

shell!(
    Av1Decoder,
    Av1,
    "AV1 decoder; the picture builder is pending."
);
shell!(
    Mpeg2Decoder,
    Mpeg2,
    "MPEG-2 decoder; the picture builder is pending."
);
shell!(
    Vc1Decoder,
    Vc1,
    "VC-1 decoder; the picture builder is pending."
);
shell!(
    Vp9Decoder,
    Vp9,
    "VP9 decoder; the picture builder is pending."
);

#[cfg(test)]
mod tests {
    use super::D3d12Frame;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
    use rust_video_caps::traits::SharedDevice;

    /// Frame over `data` leased against `shared`.
    fn frame(shared: &SharedDevice, width: u32, height: u32, data: Vec<u8>) -> D3d12Frame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let identity = GpuFrame::new(size, PixelFormat::Nv12, 3);
        let bytes = Bytes::from(data);
        let lease = shared
            .try_acquire(bytes.len() as u64)
            .expect("within budget");
        D3d12Frame {
            identity,
            data: bytes,
            lease,
        }
    }

    #[test]
    fn frame_holds_lease_against_budget() {
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.lease_bytes(), 12);
        assert_eq!(shared.used(), 12);
        assert_eq!(held.nv12(), &[7; 12]);
        assert_eq!(held.identity().generation(), 3);
    }

    #[test]
    fn dropped_frame_releases_budget() {
        let shared = SharedDevice::new("d3d12", 100);
        {
            let _held = frame(&shared, 4, 2, vec![0; 12]);
            assert_eq!(shared.used(), 12);
        }
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn export_without_sharing_names_backend() {
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert!(matches!(held.export(), GpuExport::Opaque { name: "d3d12" }));
    }

    #[test]
    fn to_cpu_splits_nv12_planes() {
        let shared = SharedDevice::new("d3d12", 100);
        let mut data = vec![1; 8];
        data.extend_from_slice(&[2; 4]);
        let held = frame(&shared, 4, 2, data);
        let cpu = held.to_cpu().expect("full nv12");
        assert_eq!(cpu.format(), PixelFormat::Nv12);
        assert_eq!(cpu.planes().len(), 2);
        assert_eq!(cpu.planes()[0].data(), &[1; 8]);
        assert_eq!(cpu.planes()[1].data(), &[2; 4]);
    }

    #[test]
    fn to_cpu_rejects_truncated_bytes() {
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![0; 11]);
        assert!(held.to_cpu().is_none());
    }
}
