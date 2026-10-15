//! GPU transcode: decode texture to encoder texture without readback.

use crate::decoder::{D3d12Frame, H264Decoder, HevcDecoder};
use crate::device::D3d12Device;
use crate::encoder::D3d12Encoder;
use crate::error::D3d12Error;
#[cfg(not(target_os = "windows"))]
use crate::error::SkipReason;
use crate::guid::Codec;
#[cfg(target_os = "windows")]
use crate::scale::Scaler;
#[cfg(target_os = "windows")]
use crate::texture::GpuTexture;

use rust_video_caps::common::PictureSize;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
#[cfg(target_os = "windows")]
use rust_video_caps::transcode::FallbackCause;
use rust_video_caps::transcode::{GpuCaps, Outcome, Plan, Scale};

/// Decode source behind transcode packets.
enum Source<'a> {
    /// Heap H.264 decoder; video UMD creation overflows beside by-value frames.
    H264(Box<H264Decoder<'a>>),
    /// Heap HEVC decoder; video UMD creation overflows beside by-value frames.
    Hevc(Box<HevcDecoder<'a>>),
}

impl Source<'_> {
    /// Frames decoded from `packet`, possibly none.
    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
        match self {
            Self::H264(decoder) => decoder.decode(packet),
            Self::Hevc(decoder) => decoder.decode(packet),
        }
    }

    /// GPU textures decoded from `packet`, possibly none.
    #[cfg(target_os = "windows")]
    fn decode_texture(&mut self, packet: &Packet) -> Result<Vec<GpuTexture>, D3d12Error> {
        match self {
            Self::H264(decoder) => decoder.decode_texture(packet),
            Self::Hevc(decoder) => decoder.decode_texture(packet),
        }
    }

    /// Frames held back for reordering or buffering.
    fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
        match self {
            Self::H264(decoder) => decoder.flush(),
            Self::Hevc(decoder) => decoder.flush(),
        }
    }

    /// GPU textures held back for reordering or buffering.
    #[cfg(target_os = "windows")]
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
    /// Device behind the scale hop.
    #[cfg(target_os = "windows")]
    device: &'a D3d12Device,
    /// Scale hop, opened on first use.
    #[cfg(target_os = "windows")]
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
                return Err(D3d12Error::Unsupported(format!("{codec} decode pending")));
            }
        };
        let encoder = D3d12Encoder::open(device, encode)?;
        let caps = GpuCaps::new(
            cfg!(target_os = "windows"),
            device.supports(decode),
            device.supports_encode(encode),
        );
        let plan = Plan::for_hops(caps.platform(), caps.decode_export(), caps.encode_import());
        Ok(Self {
            source,
            encoder,
            caps,
            scale: Scale::passthrough(),
            #[cfg(target_os = "windows")]
            device,
            #[cfg(target_os = "windows")]
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
    pub fn transcode(&mut self, packet: &Packet) -> Result<Outcome, D3d12Error> {
        if self.caps.ready() {
            self.transcode_gpu(packet)
        } else {
            let frames = self.source.decode(packet)?;
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
    pub fn flush(&mut self) -> Result<Outcome, D3d12Error> {
        if self.caps.ready() {
            self.flush_gpu()
        } else {
            let frames = self.source.flush()?;
            let mut packets = self.encode_cpu(&frames)?;
            packets.append(&mut self.encoder.flush()?);
            Ok(Outcome::new(packets, self.plan))
        }
    }

    /// Packets transcoded from `packet` through GPU textures.
    #[cfg(target_os = "windows")]
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Outcome, D3d12Error> {
        let textures = self.source.decode_texture(packet)?;
        let packets = self.encode_textures(&textures)?;
        Ok(Outcome::new(packets, self.plan))
    }

    /// Packets transcoded from `packet` through GPU textures.
    #[cfg(not(target_os = "windows"))]
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Outcome, D3d12Error> {
        let _ = packet;
        Err(SkipReason::PlatformUnsupported.into())
    }

    /// Drained packets through GPU textures.
    #[cfg(target_os = "windows")]
    fn flush_gpu(&mut self) -> Result<Outcome, D3d12Error> {
        let textures = self.source.flush_texture()?;
        let mut packets = self.encode_textures(&textures)?;
        packets.append(&mut self.encoder.flush()?);
        Ok(Outcome::new(packets, self.plan))
    }

    /// Drained packets through GPU textures.
    #[cfg(not(target_os = "windows"))]
    fn flush_gpu(&mut self) -> Result<Outcome, D3d12Error> {
        Err(SkipReason::PlatformUnsupported.into())
    }

    /// Packets coded from GPU `textures`.
    #[cfg(target_os = "windows")]
    fn encode_textures(&mut self, textures: &[GpuTexture]) -> Result<Vec<Packet>, D3d12Error> {
        let mut packets = Vec::new();
        for texture in textures {
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
    #[cfg(target_os = "windows")]
    fn scaled_or_miss(
        &mut self,
        texture: &GpuTexture,
        cause: FallbackCause,
    ) -> Result<GpuTexture, D3d12Error> {
        let FallbackCause::ScaleNeeded { from, to } = cause else {
            return Err(D3d12Error::Unsupported(cause.to_string()));
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != from || scaler.to() != to)
        {
            match Scaler::open(self.device, from, to) {
                Ok(scaler) => self.scaler = Some(scaler),
                Err(D3d12Error::Unsupported(_)) => {
                    return Err(D3d12Error::Unsupported(cause.to_string()));
                }
                Err(error) => return Err(error),
            }
        }
        let scaler = self.scaler.as_mut().ok_or(D3d12Error::NoSession)?;
        scaler.scale(texture)
    }

    /// `frames` through the proven CPU path.
    fn encode_cpu(&mut self, frames: &[D3d12Frame]) -> Result<Vec<Packet>, D3d12Error> {
        let mut packets = Vec::new();
        for frame in frames {
            let cpu = frame
                .to_cpu()
                .ok_or_else(|| D3d12Error::Unsupported("readback short of extent".to_owned()))?;
            if let Some(cause) = self.scale.miss(cpu.size()) {
                return Err(D3d12Error::Unsupported(cause.to_string()));
            }
            packets.append(&mut self.encoder.encode(&cpu)?);
        }
        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    #[path = "../../../examples/common/bits.rs"]
    mod bits;
    #[path = "../../../examples/common/h264.rs"]
    mod h264;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::transcode::{FallbackCause, GpuCaps, Plan, Route, Scale};

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
            #[cfg(target_os = "windows")]
            device,
            #[cfg(target_os = "windows")]
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
        let outcome = transcoder.flush().expect("flush drains");
        assert!(outcome.packets().is_empty());
    }

    #[test]
    fn double_flush_stays_empty() {
        let Ok(device) = crate::device::D3d12Device::open() else {
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
        let Ok(device) = crate::device::D3d12Device::open() else {
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
        let Ok(device) = crate::device::D3d12Device::open() else {
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
    fn cpu_fallback_transcodes_off_gpu() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let caps = GpuCaps::new(true, true, false);
        assert!(!caps.ready());
        let Some(mut transcoder) = forced(&device, caps, FallbackCause::EncoderCpuOnly) else {
            return;
        };
        let mut count = 0;
        for raw in h264::annex_b_h264_packets()
            .into_iter()
            .chain(h264::annex_b_h264_packets_cropped())
        {
            let packet = Packet::new(Bytes::from(raw));
            let outcome = transcoder.transcode(&packet).expect("cpu transcode");
            assert_eq!(outcome.plan().route(), Route::Cpu);
            assert_eq!(outcome.plan().cause(), Some(FallbackCause::EncoderCpuOnly));
            count += outcome.packets().len();
        }
        let drained = transcoder.flush().expect("cpu drain");
        assert_eq!(drained.plan().route(), Route::Cpu);
        assert!(count + drained.packets().len() > 0);
    }

    #[test]
    fn cpu_fallback_scale_miss_fails_loud() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let caps = GpuCaps::new(true, true, false);
        let Some(mut transcoder) = forced(&device, caps, FallbackCause::EncoderCpuOnly) else {
            return;
        };
        transcoder.scale = Scale::to(PictureSize::new(160, 112).expect("nonzero extent"));
        let raw = h264::annex_b_h264_packets_cropped_sized(39, 22, 4, 22)
            .into_iter()
            .next()
            .expect("sets plus IDR");
        let packet = Packet::new(Bytes::from(raw));
        let drained = transcoder
            .transcode(&packet)
            .and_then(|_| transcoder.flush());
        match drained {
            Err(crate::error::D3d12Error::Unsupported(error)) => {
                assert!(error.contains("scale"), "names the hop: {error}");
            }
            outcome => panic!("expected scale miss, got {outcome:?}"),
        }
    }
}
