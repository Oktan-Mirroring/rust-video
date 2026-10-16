//! GPU transcode: decode surface to encoder input without readback.

use crate::decoder::{H264Decoder, VaApiFrame};
use crate::device::VaApiDevice;
use crate::encoder::{EncodeCodec, VaApiEncoder};
use crate::error::VaApiError;
use crate::ffi;
use crate::scale::{ScaledSurface, Scaler};

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::{
    Convert, FallbackCause, GpuCaps, Outcome, Plan, Scale, ShareMechanism,
};

/// GPU transcode from H.264 decode to encoder input.
///
/// The encoder opens lazily on the first picture, when the decode
/// size is known, and re-opens when a reconfig changes it; every
/// coded picture stays an IDR, so the stream stays valid across the
/// swap. Stays on its thread with the decoder it owns.
pub struct Transcoder<'a> {
    /// H.264 decoder; the only wired frontend.
    decoder: H264Decoder<'a>,
    /// Codec the transcode writes.
    encode_codec: EncodeCodec,
    /// Encoder behind device surfaces, opened on first picture.
    encoder: Option<VaApiEncoder>,
    /// Per-hop capabilities.
    caps: GpuCaps,
    /// Optional scale hop.
    scale: Scale,
    /// Optional format convert.
    convert: Convert,
    /// Device behind the hops.
    device: &'a VaApiDevice,
    /// Scale hop, opened on first use.
    scaler: Option<Scaler>,
    /// Executed plan for passthrough frames.
    plan: Plan,
}

impl<'a> Transcoder<'a> {
    /// Transcoder over `device` from H.264 decode to `encode`.
    #[must_use]
    pub fn open(device: &'a VaApiDevice, encode: EncodeCodec) -> Self {
        let caps = GpuCaps::new(
            Some(ShareMechanism::DmaBuf),
            device.dmabuf_export(),
            device.supports(encode.profile(), ffi::VA_ENTRYPOINT_ENC_SLICE),
        );
        let plan = Plan::for_caps(&caps);
        Self {
            decoder: H264Decoder::open(device),
            encode_codec: encode,
            encoder: None,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            device,
            scaler: None,
            plan,
        }
    }

    /// Transcoder retargeted to `target`.
    ///
    /// A size change scales on the GPU where the box supports it, else
    /// fails loud at `transcode` and `flush` with `Unsupported`, never
    /// silently.
    #[must_use]
    pub fn with_target(mut self, target: PictureSize) -> Self {
        self.scale = Scale::to(target);
        self
    }

    /// Transcoder retargeted to `format`.
    ///
    /// A layout change converts on the GPU where the box supports it, else
    /// fails loud at `transcode` and `flush` with `Unsupported`, never
    /// silently.
    #[must_use]
    pub fn with_target_format(mut self, format: PixelFormat) -> Self {
        self.convert = Convert::to(format);
        self
    }

    /// Per-hop capabilities.
    #[must_use]
    pub fn caps(&self) -> GpuCaps {
        self.caps
    }

    /// Packets transcoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the scale hop misses its target or when either
    /// backend refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Outcome, VaApiError> {
        if self.caps.ready() {
            self.transcode_gpu(packet)
        } else {
            let frames = self.decoder.decode(packet)?;
            let packets = self.encode_cpu(&frames)?;
            Ok(Outcome::new(packets, self.plan))
        }
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the scale hop misses its target or when either
    /// backend refuses the drain.
    pub fn flush(&mut self) -> Result<Outcome, VaApiError> {
        if self.caps.ready() {
            self.flush_gpu()
        } else {
            let frames = self.decoder.flush()?;
            let mut packets = self.encode_cpu(&frames)?;
            if let Some(encoder) = self.encoder.as_mut() {
                packets.append(&mut encoder.flush()?);
            }
            Ok(Outcome::new(packets, self.plan))
        }
    }

    /// Packets transcoded from `packet` through device surfaces.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Outcome, VaApiError> {
        let surfaces = self.decoder.decode_surface(packet)?;
        let packets = self.encode_surfaces(&surfaces)?;
        Ok(Outcome::new(packets, self.plan))
    }

    /// Drained packets through device surfaces.
    fn flush_gpu(&mut self) -> Result<Outcome, VaApiError> {
        let surfaces = self.decoder.flush_surface()?;
        let mut packets = self.encode_surfaces(&surfaces)?;
        if let Some(encoder) = self.encoder.as_mut() {
            packets.append(&mut encoder.flush()?);
        }
        Ok(Outcome::new(packets, self.plan))
    }

    /// Packets coded from device `surfaces`.
    fn encode_surfaces(&mut self, surfaces: &[VaApiFrame]) -> Result<Vec<Packet>, VaApiError> {
        let mut packets = Vec::new();
        for surface in surfaces {
            if let Some(cause) = self.convert.miss(PixelFormat::Nv12) {
                return Err(VaApiError::Unsupported(cause.to_string()));
            }
            if let Some(cause) = self.scale.miss(surface.identity().size()) {
                let scaled = self.scaled_or_miss(surface, cause)?;
                let size = scaled.size();
                packets.append(&mut self.encoder_for(size)?.encode_scaled(&scaled)?);
            } else {
                let size = surface.identity().size();
                packets.append(&mut self.encoder_for(size)?.encode_surface(surface)?);
            }
        }
        Ok(packets)
    }

    /// Scaled `surface` for a `cause` miss, or the loud miss itself.
    fn scaled_or_miss(
        &mut self,
        surface: &VaApiFrame,
        cause: FallbackCause,
    ) -> Result<ScaledSurface, VaApiError> {
        let FallbackCause::ScaleNeeded { from, to } = cause else {
            return Err(VaApiError::Unsupported(cause.to_string()));
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != from || scaler.to() != to)
        {
            match Scaler::open(self.device, from, to) {
                Ok(scaler) => self.scaler = Some(scaler),
                Err(VaApiError::Unsupported(_)) => {
                    return Err(VaApiError::Unsupported(cause.to_string()));
                }
                Err(error) => return Err(error),
            }
        }
        let scaler = self
            .scaler
            .as_mut()
            .ok_or_else(|| VaApiError::Parse("scale without a scaler".to_owned()))?;
        scaler.scale(surface)
    }

    /// Encoder for `size`, opened or re-opened on demand.
    fn encoder_for(&mut self, size: PictureSize) -> Result<&mut VaApiEncoder, VaApiError> {
        if self
            .encoder
            .as_ref()
            .is_none_or(|encoder| encoder.size() != size)
        {
            self.encoder = Some(VaApiEncoder::open(self.device, self.encode_codec, size)?);
        }
        match self.encoder.as_mut() {
            Some(encoder) => Ok(encoder),
            None => Err(VaApiError::Parse("encoder without a session".to_owned())),
        }
    }

    /// `frames` through the CPU split, loud about the cause.
    fn encode_cpu(&mut self, frames: &[VaApiFrame]) -> Result<Vec<Packet>, VaApiError> {
        if let Some(cause) = self.plan.cause() {
            log::warn!("va-api transcode: cpu split ({cause})");
        }
        let mut packets = Vec::new();
        for frame in frames {
            let cpu = frame
                .to_cpu()
                .ok_or_else(|| VaApiError::Unsupported("nv12 short of its extent".to_owned()))?;
            if let Some(cause) = self.convert.miss(cpu.format()) {
                return Err(VaApiError::Unsupported(cause.to_string()));
            }
            if let Some(cause) = self.scale.miss(cpu.size()) {
                return Err(VaApiError::Unsupported(cause.to_string()));
            }
            let size = cpu.size();
            packets.append(&mut self.encoder_for(size)?.encode(&cpu)?);
        }
        Ok(packets)
    }
}
