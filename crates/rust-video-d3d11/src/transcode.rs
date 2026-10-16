//! GPU transcode: decode surface to encoder texture without readback.

use crate::decoder::GpuSurface;
use crate::decoder::{D3d11Frame, H264Decoder};
use crate::device::D3d11Device;
use crate::encoder::D3d11Encoder;
use crate::error::D3d11Error;
use crate::guid::Codec;
use crate::scale::{ScaledSurface, Scaler};

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::FallbackCause;
use rust_video_caps::transcode::{Convert, GpuCaps, Outcome, Plan, Scale, ShareMechanism};

/// GPU transcode from decode surface to encoder texture.
pub struct Transcoder<'a> {
    /// Heap decoder; video UMD creation overflows beside by-value frames.
    decoder: Box<H264Decoder<'a>>,
    /// Encoder behind surfaces.
    encoder: D3d11Encoder,
    /// Per-hop capabilities.
    caps: GpuCaps,
    /// Optional scale hop.
    scale: Scale,
    /// Optional format convert.
    convert: Convert,
    /// Device behind the scale hop.
    device: &'a D3d11Device,
    /// Scale hop, opened on first use.
    scaler: Option<Scaler>,
    /// Executed plan for passthrough frames.
    plan: Plan,
}

impl<'a> Transcoder<'a> {
    /// Transcoder over `device` from `decode` to `encode`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a `decode` picture builder or without an
    /// `encode` encoder MFT on the machine.
    pub fn open(device: &'a D3d11Device, decode: Codec, encode: Codec) -> Result<Self, D3d11Error> {
        if decode != Codec::H264 {
            return Err(D3d11Error::Unsupported(format!("{decode} decode pending")));
        }
        let encoder = D3d11Encoder::open(device, encode)?;
        let caps = GpuCaps::new(
            Some(ShareMechanism::NtHandle),
            device.supports(decode),
            encoder.gpu_ready(),
        );
        let plan = Plan::for_caps(&caps);
        let decoder = Box::new(H264Decoder::open(device));
        Ok(Self {
            decoder,
            encoder,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            device,
            scaler: None,
            plan,
        })
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
    pub fn transcode(&mut self, packet: &Packet) -> Result<Outcome, D3d11Error> {
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
    pub fn flush(&mut self) -> Result<Outcome, D3d11Error> {
        if self.caps.ready() {
            self.flush_gpu()
        } else {
            let frames = self.decoder.flush()?;
            let mut packets = self.encode_cpu(&frames)?;
            packets.append(&mut self.encoder.flush()?);
            Ok(Outcome::new(packets, self.plan))
        }
    }

    /// Packets transcoded from `packet` through GPU surfaces.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Outcome, D3d11Error> {
        let surfaces = self.decoder.decode_surface(packet)?;
        let packets = self.encode_surfaces(&surfaces)?;
        Ok(Outcome::new(packets, self.plan))
    }

    /// Drained packets through GPU surfaces.
    fn flush_gpu(&mut self) -> Result<Outcome, D3d11Error> {
        let surfaces = self.decoder.flush_surface()?;
        let mut packets = self.encode_surfaces(&surfaces)?;
        packets.append(&mut self.encoder.flush()?);
        Ok(Outcome::new(packets, self.plan))
    }

    /// Packets coded from GPU `surfaces`.
    fn encode_surfaces(&mut self, surfaces: &[GpuSurface]) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        for surface in surfaces {
            if let Some(cause) = self.convert.miss(PixelFormat::Nv12) {
                return Err(D3d11Error::Unsupported(cause.to_string()));
            }
            if let Some(cause) = self.scale.miss(surface.size()) {
                let scaled = self.scaled_or_miss(surface, cause)?;
                packets.append(&mut self.encoder.encode_scaled(&scaled)?);
            } else {
                packets.append(&mut self.encoder.encode_surface(surface)?);
            }
        }
        Ok(packets)
    }

    /// Scaled `surface` for a `cause` miss, or the loud miss itself.
    fn scaled_or_miss(
        &mut self,
        surface: &GpuSurface,
        cause: FallbackCause,
    ) -> Result<ScaledSurface, D3d11Error> {
        let FallbackCause::ScaleNeeded { from, to } = cause else {
            return Err(D3d11Error::Unsupported(cause.to_string()));
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != from || scaler.to() != to)
        {
            match Scaler::open(self.device, from, to) {
                Ok(scaler) => self.scaler = Some(scaler),
                Err(D3d11Error::Unsupported(_)) => {
                    return Err(D3d11Error::Unsupported(cause.to_string()));
                }
                Err(error) => return Err(error),
            }
        }
        let scaler = self.scaler.as_ref().ok_or(D3d11Error::NoSession)?;
        scaler.scale(surface)
    }

    /// `frames` through the proven CPU path.
    fn encode_cpu(&mut self, frames: &[D3d11Frame]) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        for frame in frames {
            let cpu = frame
                .to_cpu()
                .ok_or_else(|| D3d11Error::Unsupported("nv12 short of its extent".to_owned()))?;
            if let Some(cause) = self.convert.miss(cpu.format()) {
                return Err(D3d11Error::Unsupported(cause.to_string()));
            }
            if let Some(cause) = self.scale.miss(cpu.size()) {
                return Err(D3d11Error::Unsupported(cause.to_string()));
            }
            packets.append(&mut self.encoder.encode(&cpu)?);
        }
        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rust_video_caps::packet::Packet;

    /// Live H.264 transcoder, absent without hardware.
    fn transcoder(device: &crate::device::D3d11Device) -> Option<super::Transcoder<'_>> {
        super::Transcoder::open(device, crate::guid::Codec::H264, crate::guid::Codec::H264).ok()
    }

    #[test]
    fn open_rejects_pending_decode_codec() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let result =
            super::Transcoder::open(&device, crate::guid::Codec::Vp9, crate::guid::Codec::H264);
        assert!(matches!(
            result,
            Err(crate::error::D3d11Error::Unsupported(_))
        ));
    }

    #[test]
    fn flush_before_decode_drains_empty() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        let outcome = transcoder.flush().expect("flush drains");
        assert!(outcome.packets().is_empty());
    }

    #[test]
    fn double_flush_stays_empty() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        assert!(transcoder
            .flush()
            .expect("first flush")
            .packets()
            .is_empty());
        assert!(transcoder
            .flush()
            .expect("second flush")
            .packets()
            .is_empty());
    }

    #[test]
    fn empty_packet_yields_no_packets() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        let packet = Packet::new(Bytes::new());
        let outcome = transcoder.transcode(&packet).expect("empty packet");
        assert!(outcome.packets().is_empty());
    }

    #[test]
    fn truncated_packet_yields_no_packets() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1]));
        let outcome = transcoder.transcode(&packet).expect("truncated packet");
        assert!(outcome.packets().is_empty());
    }

    #[test]
    fn garbage_packet_fails_loud() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
        assert!(transcoder.transcode(&packet).is_err());
    }

    #[test]
    fn forbidden_packet_fails_loud() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00]));
        assert!(transcoder.transcode(&packet).is_err());
    }
}
