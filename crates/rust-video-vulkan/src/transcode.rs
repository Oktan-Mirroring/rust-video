//! GPU transcode: decode export to encoder import without readback.

use crate::decoder::{H264Decoder, H265Decoder, VulkanFrame};
use crate::device::VulkanDevice;
use crate::encoder::{H264Encoder, H265Encoder};
use crate::error::VulkanError;
use crate::ladder::CodecOp;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::{Convert, FallbackCause, GpuCaps, Outcome, Plan, Route, Scale};

/// Encoder behind one transcode route.
enum Encode {
    /// H.264 encoder.
    H264(H264Encoder),
    /// H.265 encoder.
    H265(H265Encoder),
}

impl Encode {
    /// Packets coded from the device export behind `frame`.
    fn encode_export(&mut self, frame: &VulkanFrame) -> Result<Vec<Packet>, VulkanError> {
        match self {
            Self::H264(encoder) => encoder.encode_export(frame),
            Self::H265(encoder) => encoder.encode_export(frame),
        }
    }
}

/// GPU transcode from decode picture to coded packets.
pub struct Transcoder<'a> {
    /// Decode hop.
    decoder: DecodeRouted<'a>,
    /// Encode device behind the lazy encoder.
    encode_device: &'a VulkanDevice,
    /// Encode operation behind the lazy encoder.
    encode_op: CodecOp,
    /// Encode hop, opened on the first frame.
    encoder: Option<Encode>,
    /// Per-hop capabilities.
    caps: GpuCaps,
    /// Optional scale hop.
    scale: Scale,
    /// Optional format convert.
    convert: Convert,
    /// Executed plan.
    plan: Plan,
    /// Decode adapter id.
    decode_adapter: u64,
    /// Encode adapter id.
    encode_adapter: u64,
}

/// Decoder with its borrowed device lifetime.
enum DecodeRouted<'a> {
    /// H.264 decoder.
    H264(H264Decoder<'a>),
    /// H.265 decoder.
    H265(H265Decoder<'a>),
}

impl<'a> Transcoder<'a> {
    /// Transcoder from `decode_op` on `decode_device` to `encode_op` on `encode_device`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when either op names the wrong side or the decode
    /// hop refuses its device.
    pub fn open(
        decode_device: &'a VulkanDevice,
        decode_op: CodecOp,
        encode_device: &'a VulkanDevice,
        encode_op: CodecOp,
    ) -> Result<Self, VulkanError> {
        if decode_op.is_encode() || !encode_op.is_encode() {
            return Err(VulkanError::Unsupported {
                what: "transcode direction".to_owned(),
            });
        }
        let decoder = match decode_op {
            CodecOp::H264Decode => DecodeRouted::H264(H264Decoder::open(decode_device)?),
            CodecOp::H265Decode => DecodeRouted::H265(H265Decoder::open(decode_device)?),
            CodecOp::H264Encode | CodecOp::H265Encode => {
                return Err(VulkanError::Unsupported {
                    what: "transcode direction".to_owned(),
                });
            }
        };
        let share = decode_device.share_mechanism();
        let decode_export = share.is_some()
            && decode_device.dmabuf_export()
            && decode_two_plane(decode_device, decode_op);
        let caps = GpuCaps::new(share, decode_export, encode_device.dmabuf_import());
        let decode_adapter = decode_device.adapter_id();
        let encode_adapter = encode_device.adapter_id();
        let plan = if decode_adapter == encode_adapter {
            Plan::for_caps(&caps)
        } else {
            Plan::for_devices(decode_adapter, encode_adapter)
        };
        Ok(Self {
            decoder,
            encode_device,
            encode_op,
            encoder: None,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            plan,
            decode_adapter,
            encode_adapter,
        })
    }

    /// Transcoder retargeted to `target`.
    ///
    /// A size change fails loud at `transcode` and `flush` with
    /// `Unsupported`, never silently.
    #[must_use]
    pub fn with_target(mut self, target: PictureSize) -> Self {
        self.scale = Scale::to(target);
        self
    }

    /// Transcoder retargeted to `format`.
    ///
    /// A layout change fails loud at `transcode` and `flush` with
    /// `Unsupported`, never silently.
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

    /// Executed plan.
    #[must_use]
    pub fn plan(&self) -> Plan {
        self.plan
    }

    /// Packets transcoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a hop misses its target or either backend
    /// refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Outcome, VulkanError> {
        if self.plan.route() == Route::Gpu {
            self.transcode_gpu(packet)
        } else {
            let frames = self.decode(packet)?;
            let packets = self.encode_cpu(&frames)?;
            Ok(Outcome::new(packets, self.plan))
        }
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a hop misses its target or either backend
    /// refuses the drain.
    pub fn flush(&mut self) -> Result<Outcome, VulkanError> {
        if self.plan.route() == Route::Gpu {
            self.flush_gpu()
        } else {
            let frames = match &mut self.decoder {
                DecodeRouted::H264(decoder) => decoder.flush()?,
                DecodeRouted::H265(decoder) => decoder.flush()?,
            };
            let mut packets = self.encode_cpu(&frames)?;
            if let Some(encoder) = self.encoder.as_mut() {
                match encoder {
                    Encode::H264(encoder) => packets.append(&mut encoder.flush()?),
                    Encode::H265(encoder) => packets.append(&mut encoder.flush()?),
                }
            }
            Ok(Outcome::new(packets, self.plan))
        }
    }

    /// Frames decoded from `packet` through the decode hop.
    fn decode(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.decode(packet),
            DecodeRouted::H265(decoder) => decoder.decode(packet),
        }
    }

    /// Packets transcoded from `packet` through device exports.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Outcome, VulkanError> {
        self.check_devices()?;
        let frames = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.decode_surface(packet)?,
            DecodeRouted::H265(decoder) => decoder.decode_surface(packet)?,
        };
        let packets = self.encode_surfaces(&frames)?;
        Ok(Outcome::new(packets, self.plan))
    }

    /// Drained packets through device exports.
    fn flush_gpu(&mut self) -> Result<Outcome, VulkanError> {
        self.check_devices()?;
        let frames = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.flush_surface()?,
            DecodeRouted::H265(decoder) => decoder.flush_surface()?,
        };
        let mut packets = self.encode_surfaces(&frames)?;
        if let Some(encoder) = self.encoder.as_mut() {
            match encoder {
                Encode::H264(encoder) => packets.append(&mut encoder.flush()?),
                Encode::H265(encoder) => packets.append(&mut encoder.flush()?),
            }
        }
        Ok(Outcome::new(packets, self.plan))
    }

    /// Packets coded from device `frames`.
    fn encode_surfaces(&mut self, frames: &[VulkanFrame]) -> Result<Vec<Packet>, VulkanError> {
        let mut packets = Vec::new();
        for frame in frames {
            if let Some(cause) = self.convert.miss(PixelFormat::Nv12) {
                return Err(VulkanError::Unsupported {
                    what: cause.to_string(),
                });
            }
            if let Some(cause) = self.scale.miss(frame.identity().size()) {
                return Err(VulkanError::Unsupported {
                    what: cause.to_string(),
                });
            }
            let encoder = self.encoder_for(frame.identity().size())?;
            packets.append(&mut encoder.encode_export(frame)?);
        }
        Ok(packets)
    }

    /// Loud refusal when decode and encode adapters differ.
    fn check_devices(&self) -> Result<(), VulkanError> {
        if self.decode_adapter == self.encode_adapter {
            Ok(())
        } else {
            Err(VulkanError::Unsupported {
                what: FallbackCause::DeviceMismatch.to_string(),
            })
        }
    }

    /// `frames` through the proven CPU path.
    fn encode_cpu(&mut self, frames: &[VulkanFrame]) -> Result<Vec<Packet>, VulkanError> {
        if let Some(cause) = self.plan.cause() {
            eprintln!("vulkan transcode: cpu split ({cause})");
        }
        let mut packets = Vec::new();
        for frame in frames {
            let cpu = frame.to_cpu().ok_or_else(|| VulkanError::Unsupported {
                what: "nv12 short of its extent".to_owned(),
            })?;
            if let Some(cause) = self.convert.miss(cpu.format()) {
                return Err(VulkanError::Unsupported {
                    what: cause.to_string(),
                });
            }
            if let Some(cause) = self.scale.miss(cpu.size()) {
                return Err(VulkanError::Unsupported {
                    what: cause.to_string(),
                });
            }
            let encoder = self.encoder_for(cpu.size())?;
            match encoder {
                Encode::H264(encoder) => packets.append(&mut encoder.encode(&cpu)?),
                Encode::H265(encoder) => packets.append(&mut encoder.encode(&cpu)?),
            }
        }
        Ok(packets)
    }

    /// Encode hop for `size`, opened or reopened on mismatch.
    fn encoder_for(&mut self, size: PictureSize) -> Result<&mut Encode, VulkanError> {
        let width = size.width();
        let height = size.height();
        let matches = match self.encoder.as_ref() {
            Some(Encode::H264(encoder)) => {
                self.encode_op == CodecOp::H264Encode && encoder.extent() == (width, height)
            }
            Some(Encode::H265(encoder)) => {
                self.encode_op == CodecOp::H265Encode && encoder.extent() == (width, height)
            }
            None => false,
        };
        if !matches {
            let encoder = match self.encode_op {
                CodecOp::H264Encode => {
                    Encode::H264(H264Encoder::open(self.encode_device, width, height)?)
                }
                CodecOp::H265Encode => {
                    Encode::H265(H265Encoder::open(self.encode_device, width, height)?)
                }
                CodecOp::H264Decode | CodecOp::H265Decode => {
                    return Err(VulkanError::OverCaps { what: "codec op" });
                }
            };
            self.encoder = Some(encoder);
        }
        self.encoder
            .as_mut()
            .ok_or(VulkanError::OverCaps { what: "codec op" })
    }
}

/// True when the decode profile predicts the two-plane NV12 picture format.
fn decode_two_plane(device: &VulkanDevice, op: CodecOp) -> bool {
    use crate::session::geometry::{h264_profile_to_std, h265_profile_to_std};
    use crate::session::params::{H264ProfileChain, H265ProfileChain};

    let usage = ash::vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR
        | ash::vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR;
    let formats = match op {
        CodecOp::H264Decode => {
            let Some(profile) = h264_profile_to_std(100) else {
                return false;
            };
            let chain = H264ProfileChain::new(profile);
            device
                .backend()
                .video_formats(device.adapter(), &chain.base(), usage)
        }
        CodecOp::H265Decode => {
            let Some(profile) = h265_profile_to_std(1) else {
                return false;
            };
            let chain = H265ProfileChain::new(profile);
            device
                .backend()
                .video_formats(device.adapter(), &chain.base(), usage)
        }
        CodecOp::H264Encode | CodecOp::H265Encode => return false,
    };
    formats.is_ok_and(|formats| {
        crate::raw::pick_video_format(&formats).is_some_and(crate::raw::two_plane_nv12)
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::transcode::Route;

    use crate::ladder::CodecOp;

    /// Live H.264 transcode pair, absent without hardware.
    fn pair<'a>(
        decode: &'a crate::device::VulkanDevice,
        encode: &'a crate::device::VulkanDevice,
    ) -> Option<super::Transcoder<'a>> {
        super::Transcoder::open(decode, CodecOp::H264Decode, encode, CodecOp::H264Encode).ok()
    }

    /// Live H.264 decode plus encode devices, absent without hardware.
    fn devices() -> Option<(crate::device::VulkanDevice, crate::device::VulkanDevice)> {
        let decode =
            crate::device::VulkanDevice::open(CodecOp::H264Decode).expect("negotiation runs")?;
        let encode =
            crate::device::VulkanDevice::open(CodecOp::H264Encode).expect("negotiation runs")?;
        Some((decode, encode))
    }

    #[test]
    fn open_rejects_flipped_ops() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let result =
            super::Transcoder::open(&encode, CodecOp::H264Encode, &decode, CodecOp::H264Decode);
        assert!(matches!(
            result,
            Err(crate::error::VulkanError::Unsupported { .. })
        ));
    }

    #[test]
    fn flush_before_decode_drains_empty() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
            return;
        };
        let outcome = transcoder.flush().expect("flush drains");
        assert!(outcome.packets().is_empty());
    }

    #[test]
    fn double_flush_stays_empty() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
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
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
            return;
        };
        let packet = Packet::new(Bytes::new());
        let outcome = transcoder.transcode(&packet).expect("empty packet");
        assert!(outcome.packets().is_empty());
    }

    #[test]
    fn plan_matches_caps() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(transcoder) = pair(&decode, &encode) else {
            return;
        };
        if transcoder.caps().ready() {
            assert_eq!(transcoder.plan().route(), Route::Gpu);
            assert!(transcoder.plan().cause().is_none());
        } else {
            assert_eq!(transcoder.plan().route(), Route::Cpu);
            assert!(transcoder.plan().cause().is_some());
        }
    }

    #[test]
    fn flush_reports_plan() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
            return;
        };
        let plan = transcoder.plan();
        let outcome = transcoder.flush().expect("flush drains");
        assert!(outcome.packets().is_empty());
        assert_eq!(outcome.plan(), plan);
    }
}
