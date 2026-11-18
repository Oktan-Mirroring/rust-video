//! GPU transcode: decode surface to encoder input without readback.

use crate::decoder::{H264Decoder, VaApiFrame};
use crate::device::VaApiDevice;
use crate::encoder::{EncodeCodec, GopShape, VaApiEncoder};
use crate::error::VaApiError;
use crate::ffi;
use crate::scale::{ScaledSurface, Scaler};

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::{
    Convert, FallbackCause, GpuCaps, Outcome, Plan, Scale, ShareMechanism,
};

/// GPU transcode from H.264 decode to encoder input.
///
/// The encoder opens lazily on the first picture, when the decode
/// size is known, and re-opens when a reconfig changes it; the fresh
/// session starts on an IDR, so the stream stays valid across the
/// swap. Stays on its thread with the decoder it owns.
pub struct Transcoder<'a> {
    /// H.264 decoder; the only wired frontend.
    decoder: H264Decoder<'a>,
    /// Codec the transcode writes.
    encode_codec: EncodeCodec,
    /// GOP shape the transcode writes.
    gop: GopShape,
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
            gop: GopShape::AllIntra,
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

    /// Transcoder writing `gop` instead of all-intra.
    #[must_use]
    pub fn with_gop(mut self, gop: GopShape) -> Self {
        self.gop = gop;
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
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Outcome, VaApiError> {
        self.check_ready()?;
        self.transcode_gpu(packet)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the drain.
    pub fn flush(&mut self) -> Result<Outcome, VaApiError> {
        self.check_ready()?;
        self.flush_gpu()
    }

    /// GPU route unless a hop missed, else the typed decline naming the miss.
    fn check_ready(&self) -> Result<(), VaApiError> {
        if self.caps.ready() {
            Ok(())
        } else if let Some(cause) = self.plan.cause() {
            Err(VaApiError::Unsupported(cause.to_string()))
        } else {
            // open derives plan from caps, so a miss names its cause.
            Ok(())
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
    ///
    /// Scaled pictures pipeline one deep: the next scale submits
    /// before the previous scaled picture encodes, so the scaler and
    /// the encoder overlap across pictures.
    fn encode_surfaces(&mut self, surfaces: &[VaApiFrame]) -> Result<Vec<Packet>, VaApiError> {
        let mut packets = Vec::new();
        let mut pending: Option<ScaledSurface> = None;
        for surface in surfaces {
            if let Some(cause) = self.convert.miss(PixelFormat::Nv12) {
                return Err(VaApiError::Unsupported(cause.to_string()));
            }
            if let Some(cause) = self.scale.miss(surface.identity().size()) {
                if self.scaler_swap_needed(&cause) {
                    if let Some(previous) = pending.take() {
                        self.encode_pending(&previous, &mut packets)?;
                    }
                }
                let scaled = self.scaled_or_miss(surface, cause)?;
                if let Some(previous) = pending.replace(scaled) {
                    self.encode_pending(&previous, &mut packets)?;
                }
            } else {
                if let Some(previous) = pending.take() {
                    self.encode_pending(&previous, &mut packets)?;
                }
                let size = surface.identity().size();
                packets.append(&mut self.encoder_for(size)?.encode_surface(surface)?);
            }
        }
        if let Some(previous) = pending.take() {
            self.encode_pending(&previous, &mut packets)?;
        }
        Ok(packets)
    }

    /// Packets coded from a held-back scaled picture.
    fn encode_pending(
        &mut self,
        scaled: &ScaledSurface,
        packets: &mut Vec<Packet>,
    ) -> Result<(), VaApiError> {
        let size = scaled.size();
        packets.append(&mut self.encoder_for(size)?.encode_scaled(scaled)?);
        Ok(())
    }

    /// True when `cause` needs a different scaler than the live one.
    fn scaler_swap_needed(&self, cause: &FallbackCause) -> bool {
        let FallbackCause::ScaleNeeded { from, to } = cause else {
            return false;
        };
        self.scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != *from || scaler.to() != *to)
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
            let encoder =
                VaApiEncoder::open(self.device, self.encode_codec, size)?.with_gop(self.gop);
            self.encoder = Some(encoder);
        }
        match self.encoder.as_mut() {
            Some(encoder) => Ok(encoder),
            None => Err(VaApiError::Parse("encoder without a session".to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::transcode::{
        Convert, FallbackCause, GpuCaps, Plan, Scale, ShareMechanism,
    };

    /// Transcoder with `caps` forced past `open`.
    fn forced(
        device: &crate::device::VaApiDevice,
        caps: GpuCaps,
        cause: FallbackCause,
    ) -> super::Transcoder<'_> {
        super::Transcoder {
            decoder: crate::decoder::H264Decoder::open(device),
            encode_codec: crate::encoder::EncodeCodec::H264,
            gop: crate::encoder::GopShape::AllIntra,
            encoder: None,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            device,
            scaler: None,
            plan: Plan::cpu(cause),
        }
    }

    #[test]
    fn caps_miss_declines_typed() {
        let Ok(device) = crate::device::VaApiDevice::open() else {
            return;
        };
        let caps = GpuCaps::new(Some(ShareMechanism::DmaBuf), true, false);
        assert!(!caps.ready());
        let mut transcoder = forced(&device, caps, FallbackCause::EncoderCpuOnly);
        let packet = Packet::new(Bytes::new());
        assert_eq!(
            transcoder
                .transcode(&packet)
                .expect_err("caps miss declines")
                .to_string(),
            "unsupported stream: encoder without gpu import"
        );
        assert_eq!(
            transcoder.flush().expect_err("drain declines").to_string(),
            "unsupported stream: encoder without gpu import"
        );
    }
}
