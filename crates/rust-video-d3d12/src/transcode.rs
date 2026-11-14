//! GPU transcode: decode texture to encoder texture without readback.

use crate::decoder::{H264Decoder, HevcDecoder};
use crate::device::D3d12Device;
use crate::encoder::D3d12Encoder;
use crate::error::{D3d12Error, UnsupportedReason};
use crate::guid::Codec;
use crate::scale::Scaler;
use crate::texture::GpuTexture;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::FallbackCause;
use rust_video_caps::transcode::{Convert, GpuCaps, Outcome, Plan, Scale, ShareMechanism};
#[cfg(test)]
use rust_video_harness::H264Synth;

/// Decode source behind transcode packets.
enum Source<'a> {
    /// Heap H.264 decoder; video UMD creation overflows beside by-value frames.
    H264(Box<H264Decoder<'a>>),
    /// Heap HEVC decoder; video UMD creation overflows beside by-value frames.
    Hevc(Box<HevcDecoder<'a>>),
}

impl Source<'_> {
    /// GPU textures decoded from `packet`, possibly none.
    fn decode_texture(&mut self, packet: &Packet) -> Result<Vec<GpuTexture>, D3d12Error> {
        match self {
            Self::H264(decoder) => decoder.decode_texture(packet),
            Self::Hevc(decoder) => decoder.decode_texture(packet),
        }
    }

    /// GPU textures held back for reordering or buffering.
    fn flush_texture(&mut self) -> Result<Vec<GpuTexture>, D3d12Error> {
        match self {
            Self::H264(decoder) => decoder.flush_texture(),
            Self::Hevc(decoder) => decoder.flush_texture(),
        }
    }
}

/// GPU transcode from decode texture to encoder texture.
pub struct Transcoder<'a> {
    /// Decode source behind packets.
    source: Source<'a>,
    /// Encoder behind textures.
    encoder: D3d12Encoder<'a>,
    /// Per-hop capabilities.
    caps: GpuCaps,
    /// Optional scale hop.
    scale: Scale,
    /// Optional format convert.
    convert: Convert,
    /// Device behind the scale hop.
    device: &'a D3d12Device,
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
    /// `encode` video encoder on the adapter.
    pub fn open(device: &'a D3d12Device, decode: Codec, encode: Codec) -> Result<Self, D3d12Error> {
        let source = match decode {
            Codec::H264 => Source::H264(Box::new(H264Decoder::open(device))),
            Codec::Hevc => Source::Hevc(Box::new(HevcDecoder::open(device))),
            codec => {
                return Err(D3d12Error::Unsupported(UnsupportedReason::DecodePending {
                    codec,
                }));
            }
        };
        let encoder = D3d12Encoder::open(device, encode)?;
        let caps = GpuCaps::new(
            Some(ShareMechanism::NtHandle),
            device.supports(decode),
            device.supports_encode(encode),
        );
        let plan = Plan::for_caps(&caps);
        Ok(Self {
            source,
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
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Outcome, D3d12Error> {
        self.check_ready()?;
        self.transcode_gpu(packet)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the drain.
    pub fn flush(&mut self) -> Result<Outcome, D3d12Error> {
        self.check_ready()?;
        self.flush_gpu()
    }

    /// GPU route unless a hop missed, else the typed decline naming the miss.
    fn check_ready(&self) -> Result<(), D3d12Error> {
        if self.caps.ready() {
            Ok(())
        } else if let Some(cause) = self.plan.cause() {
            Err(D3d12Error::Unsupported(UnsupportedReason::Fallback {
                cause,
            }))
        } else {
            // open derives plan from caps, so a miss names its cause.
            Ok(())
        }
    }

    /// Packets transcoded from `packet` through GPU textures.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Outcome, D3d12Error> {
        let textures = self.source.decode_texture(packet)?;
        let packets = self.encode_textures(&textures)?;
        Ok(Outcome::new(packets, self.plan))
    }

    /// Drained packets through GPU textures.
    fn flush_gpu(&mut self) -> Result<Outcome, D3d12Error> {
        let textures = self.source.flush_texture()?;
        let mut packets = self.encode_textures(&textures)?;
        packets.append(&mut self.encoder.flush()?);
        Ok(Outcome::new(packets, self.plan))
    }

    /// Packets coded from GPU `textures`.
    fn encode_textures(&mut self, textures: &[GpuTexture]) -> Result<Vec<Packet>, D3d12Error> {
        let mut packets = Vec::new();
        for texture in textures {
            if let Some(cause) = self.convert.miss(PixelFormat::Nv12) {
                return Err(D3d12Error::Unsupported(UnsupportedReason::Fallback {
                    cause,
                }));
            }
            let scaled;
            let input = if let Some(cause) = self.scale.miss(texture.size()) {
                scaled = self.scaled_or_miss(texture, cause)?;
                &scaled
            } else {
                texture
            };
            packets.append(&mut self.encoder.encode_texture(input)?);
        }
        Ok(packets)
    }

    /// Scaled `texture` for a `cause` miss, or the loud miss itself.
    fn scaled_or_miss(
        &mut self,
        texture: &GpuTexture,
        cause: FallbackCause,
    ) -> Result<GpuTexture, D3d12Error> {
        let FallbackCause::ScaleNeeded { from, to } = cause else {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Fallback {
                cause,
            }));
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != from || scaler.to() != to)
        {
            match Scaler::open(self.device, from, to) {
                Ok(scaler) => self.scaler = Some(scaler),
                Err(D3d12Error::Unsupported(_)) => {
                    return Err(D3d12Error::Unsupported(UnsupportedReason::Fallback {
                        cause,
                    }));
                }
                Err(error) => return Err(error),
            }
        }
        let scaler = self.scaler.as_mut().ok_or(D3d12Error::NoSession)?;
        scaler.scale(texture).inspect_err(|_| {
            self.scaler = None;
        })
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::transcode::{
        Convert, FallbackCause, GpuCaps, Plan, Scale, ShareMechanism,
    };

    /// Live H.264 transcoder, absent without hardware.
    fn transcoder(device: &crate::device::D3d12Device) -> Option<super::Transcoder<'_>> {
        super::Transcoder::open(device, crate::guid::Codec::H264, crate::guid::Codec::H264).ok()
    }

    /// Transcoder with `caps` forced past `open`, absent without hardware.
    fn forced(
        device: &crate::device::D3d12Device,
        caps: GpuCaps,
        cause: FallbackCause,
    ) -> Option<super::Transcoder<'_>> {
        let encoder = crate::encoder::D3d12Encoder::open(device, crate::guid::Codec::H264).ok()?;
        Some(super::Transcoder {
            source: super::Source::H264(Box::new(crate::decoder::H264Decoder::open(device))),
            encoder,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            device,
            scaler: None,
            plan: Plan::cpu(cause),
        })
    }

    #[test]
    fn open_rejects_pending_decode_codec() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let result =
            super::Transcoder::open(&device, crate::guid::Codec::Vp9, crate::guid::Codec::H264);
        assert!(matches!(
            result,
            Err(crate::error::D3d12Error::Unsupported(_))
        ));
    }

    #[test]
    fn flush_before_decode_drains_empty() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        let outcome = transcoder.flush().expect("flush drains");
        assert_eq!(outcome.packets(), []);
    }

    #[test]
    fn double_flush_stays_empty() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        assert_eq!(transcoder.flush().expect("first flush").packets(), []);
        assert_eq!(transcoder.flush().expect("second flush").packets(), []);
    }

    #[test]
    fn empty_packet_yields_no_packets() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        let packet = Packet::new(Bytes::new());
        let outcome = transcoder.transcode(&packet).expect("empty packet");
        assert_eq!(outcome.packets(), []);
    }

    #[test]
    fn truncated_packet_yields_no_packets() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1]));
        let outcome = transcoder.transcode(&packet).expect("truncated packet");
        assert_eq!(outcome.packets(), []);
    }

    #[test]
    fn garbage_packet_fails_loud() {
        let Ok(device) = crate::device::D3d12Device::open() else {
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
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00]));
        assert!(transcoder.transcode(&packet).is_err());
    }

    #[test]
    fn caps_miss_declines_typed() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let caps = GpuCaps::new(Some(ShareMechanism::NtHandle), true, false);
        assert!(!caps.ready());
        let Some(mut transcoder) = forced(&device, caps, FallbackCause::EncoderCpuOnly) else {
            return;
        };
        for raw in super::H264Synth::annex_b_h264_packets()
            .into_iter()
            .chain(super::H264Synth::annex_b_h264_packets_cropped())
        {
            let packet = Packet::new(Bytes::from(raw));
            assert_eq!(
                transcoder
                    .transcode(&packet)
                    .expect_err("caps miss declines")
                    .to_string(),
                "unsupported stream: encoder without gpu import"
            );
        }
        assert_eq!(
            transcoder.flush().expect_err("drain declines").to_string(),
            "unsupported stream: encoder without gpu import"
        );
    }

    #[test]
    fn caps_miss_precedes_scale_miss() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let caps = GpuCaps::new(Some(ShareMechanism::NtHandle), true, false);
        let Some(mut transcoder) = forced(&device, caps, FallbackCause::EncoderCpuOnly) else {
            return;
        };
        transcoder.scale = Scale::to(PictureSize::new(160, 112).expect("nonzero extent"));
        let raw = super::H264Synth::annex_b_h264_packets_cropped_sized(39, 22, 4, 22)
            .into_iter()
            .next()
            .expect("sets plus IDR");
        let packet = Packet::new(Bytes::from(raw));
        assert_eq!(
            transcoder
                .transcode(&packet)
                .expect_err("caps miss first")
                .to_string(),
            "unsupported stream: encoder without gpu import"
        );
        assert_eq!(
            transcoder.flush().expect_err("drain declines").to_string(),
            "unsupported stream: encoder without gpu import"
        );
    }
}
