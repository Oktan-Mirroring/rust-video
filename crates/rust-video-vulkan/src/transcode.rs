//! GPU transcode: decode export to encoder import without readback.

use crate::decoder::{Av1Decoder, H264Decoder, H265Decoder, VulkanFrame};
use crate::device::VulkanDevice;
use crate::encoder::{H264Encoder, H265Encoder};
use crate::error::VulkanError;
use crate::ladder::CodecOp;

use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::{EncodeConfig, RateControl};
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, EncodeSurface, Encoder, SurfaceFilter};
use rust_video_caps::transcode::{Convert, GpuCaps, Plan, Route, Scale};

/// Encoder behind one transcode route.
enum Encode {
    /// H.264 encoder.
    H264(H264Encoder),
    /// H.265 encoder.
    H265(H265Encoder),
}

impl Encode {
    /// Packets coded from the GPU surface behind `frame`.
    fn encode_surface(&mut self, frame: &VulkanFrame) -> Result<Vec<Packet>, VulkanError> {
        match self {
            Self::H264(encoder) => encoder.encode_surface(frame),
            Self::H265(encoder) => encoder.encode_surface(frame),
        }
    }

    /// Packets coded from CPU `frame`.
    fn encode_frame(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, VulkanError> {
        match self {
            Self::H264(encoder) => encoder.encode(frame),
            Self::H265(encoder) => encoder.encode(frame),
        }
    }

    /// Packets held back for reordering or buffering.
    fn flush(&mut self) -> Result<Vec<Packet>, VulkanError> {
        match self {
            Self::H264(encoder) => encoder.flush(),
            Self::H265(encoder) => encoder.flush(),
        }
    }
}

/// Legacy H.264 encode config: auto rate over the defaults.
fn legacy_h264() -> EncodeConfig {
    EncodeConfig {
        rate_control: RateControl::Auto,
        ..Default::default()
    }
}

/// Legacy H.265 encode config: auto rate over the codec defaults.
fn legacy_h265() -> EncodeConfig {
    EncodeConfig {
        rate_control: RateControl::Auto,
        profile: rust_video_caps::encode::Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: rust_video_caps::encode::Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..Default::default()
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
    /// Encode config behind the lazy encoder.
    config: EncodeConfig,
    /// Per-hop capabilities.
    caps: GpuCaps,
    /// Optional scale hop.
    scale: Scale,
    /// Optional format convert.
    convert: Convert,
    /// Executed plan.
    plan: Plan,
}

/// Decoder with its borrowed device lifetime.
enum DecodeRouted<'a> {
    /// H.264 decoder.
    H264(H264Decoder<'a>),
    /// H.265 decoder.
    H265(H265Decoder<'a>),
    /// AV1 decoder.
    Av1(Av1Decoder<'a>),
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
            CodecOp::Av1Decode => DecodeRouted::Av1(Av1Decoder::open(decode_device)?),
            CodecOp::H264Encode | CodecOp::H265Encode => {
                return Err(VulkanError::Unsupported {
                    what: "transcode direction".to_owned(),
                });
            }
        };
        let config = match encode_op {
            CodecOp::H264Encode => legacy_h264(),
            CodecOp::H265Encode => legacy_h265(),
            CodecOp::H264Decode | CodecOp::H265Decode | CodecOp::Av1Decode => {
                return Err(VulkanError::Unsupported {
                    what: "transcode direction".to_owned(),
                });
            }
        };
        let share = decode_device.share_mechanism();
        let decode_export = share.is_some()
            && decode_device.surface_export()
            && decode_two_plane(decode_device, decode_op);
        let caps = GpuCaps::new(share, decode_export, encode_device.surface_import());
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
            config,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            plan,
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

    /// Transcoder encoding under `config` instead of the legacy recipe.
    #[must_use]
    pub fn with_config(mut self, config: EncodeConfig) -> Self {
        self.config = config;
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
    /// Returns `Err` when the plan missed a hop or the target, or when
    /// either backend refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Vec<Packet>, VulkanError> {
        self.check_ready()?;
        self.transcode_gpu(packet)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the plan missed a hop or the target, or when
    /// either backend refuses the drain.
    pub fn flush(&mut self) -> Result<Vec<Packet>, VulkanError> {
        self.check_ready()?;
        self.flush_gpu()
    }

    /// Packets transcoded from `packet` with `filter` over each CPU frame.
    ///
    /// Round trip: GPU decode, readback download to [`CpuFrame`],
    /// `filter` in place, CPU upload encode. Replace the frame inside
    /// `filter` to change pixels. The GPU plan gates this path too: a
    /// hop miss declines before any frame moves, and a scale or convert
    /// hop fails loud with the same typed miss as the surface path.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale or convert hop misses its target, when the readback
    /// truncates, or when either backend refuses the picture.
    pub fn transcode_with(
        &mut self,
        packet: &Packet,
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, VulkanError> {
        self.check_ready()?;
        let frames = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.decode(packet)?,
            DecodeRouted::H265(decoder) => decoder.decode(packet)?,
            DecodeRouted::Av1(decoder) => decoder.decode(packet)?,
        };
        self.encode_cpu(&frames, filter)
    }

    /// Drained packets with `filter` over each CPU frame.
    ///
    /// Round trip: GPU decode, readback download to [`CpuFrame`],
    /// `filter` in place, CPU upload encode. Replace the frame inside
    /// `filter` to change pixels. The GPU plan gates this path too: a
    /// hop miss declines before any frame moves, and a scale or convert
    /// hop fails loud with the same typed miss as the surface path.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale or convert hop misses its target, when the readback
    /// truncates, or when either backend refuses the drain.
    pub fn flush_with(
        &mut self,
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, VulkanError> {
        self.check_ready()?;
        let frames = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.flush()?,
            DecodeRouted::H265(decoder) => decoder.flush()?,
            DecodeRouted::Av1(decoder) => decoder.flush()?,
        };
        let mut packets = self.encode_cpu(&frames, filter)?;
        if let Some(encoder) = self.encoder.as_mut() {
            packets.append(&mut encoder.flush()?);
        }
        Ok(packets)
    }

    /// Packets transcoded from `packet` with `filter` over each GPU surface.
    ///
    /// `filter` runs between surface decode and surface encode; closures
    /// work through the blanket [`SurfaceFilter`] impl.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the picture.
    pub fn transcode_surface_with(
        &mut self,
        packet: &Packet,
        filter: &mut impl SurfaceFilter<VulkanFrame>,
    ) -> Result<Vec<Packet>, VulkanError> {
        self.check_ready()?;
        let mut surfaces = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.decode_surface(packet)?,
            DecodeRouted::H265(decoder) => decoder.decode_surface(packet)?,
            DecodeRouted::Av1(decoder) => decoder.decode_surface(packet)?,
        };
        for surface in &mut surfaces {
            filter.filter(surface);
        }
        self.encode_surfaces(&surfaces)
    }

    /// GPU route unless the plan missed, else the typed decline naming the miss.
    fn check_ready(&self) -> Result<(), VulkanError> {
        if self.plan.route() == Route::Gpu {
            Ok(())
        } else if let Some(cause) = self.plan.cause() {
            Err(VulkanError::Unsupported {
                what: cause.to_string(),
            })
        } else {
            // open routes Cpu only with a named cause.
            Ok(())
        }
    }

    /// Packets transcoded from `packet` through device exports.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Vec<Packet>, VulkanError> {
        let frames = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.decode_surface(packet)?,
            DecodeRouted::H265(decoder) => decoder.decode_surface(packet)?,
            DecodeRouted::Av1(decoder) => decoder.decode_surface(packet)?,
        };
        self.encode_surfaces(&frames)
    }

    /// Drained packets through device exports.
    fn flush_gpu(&mut self) -> Result<Vec<Packet>, VulkanError> {
        let frames = match &mut self.decoder {
            DecodeRouted::H264(decoder) => decoder.flush_surface()?,
            DecodeRouted::H265(decoder) => decoder.flush_surface()?,
            DecodeRouted::Av1(decoder) => decoder.flush_surface()?,
        };
        let mut packets = self.encode_surfaces(&frames)?;
        if let Some(encoder) = self.encoder.as_mut() {
            packets.append(&mut encoder.flush()?);
        }
        Ok(packets)
    }

    /// Packets coded from downloaded `frames` through `filter`.
    fn encode_cpu(
        &mut self,
        frames: &[VulkanFrame],
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, VulkanError> {
        let mut packets = Vec::new();
        for frame in frames {
            let mut cpu = frame.to_cpu().ok_or_else(|| {
                let size = frame.identity().size();
                let luma = u64::from(size.width()) * u64::from(size.height());
                VulkanError::ShortReadback {
                    expected: usize::try_from(luma + luma / 2).unwrap_or(usize::MAX),
                    found: frame.nv12().len(),
                }
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
            filter(&mut cpu);
            packets.append(&mut self.encoder_for(cpu.size())?.encode_frame(&cpu)?);
        }
        Ok(packets)
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
            packets.append(&mut encoder.encode_surface(frame)?);
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
                CodecOp::H264Encode => Encode::H264(H264Encoder::open(
                    self.encode_device,
                    width,
                    height,
                    self.config,
                )?),
                CodecOp::H265Encode => Encode::H265(H265Encoder::open(
                    self.encode_device,
                    width,
                    height,
                    self.config,
                )?),
                CodecOp::H264Decode | CodecOp::H265Decode | CodecOp::Av1Decode => {
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
        CodecOp::Av1Decode => {
            let chain = crate::session::params::Av1ProfileChain::new(0);
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
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::packet::Packet;
    use rust_video_caps::transcode::{
        Convert, FallbackCause, GpuCaps, Plan, Route, Scale, ShareMechanism,
    };

    use crate::error::VulkanError;
    use crate::ladder::CodecOp;

    /// Live H.264 transcode pair, absent without hardware.
    fn pair<'a>(
        decode: &'a crate::device::VulkanDevice,
        encode: &'a crate::device::VulkanDevice,
    ) -> Option<super::Transcoder<'a>> {
        super::Transcoder::open(decode, CodecOp::H264Decode, encode, CodecOp::H264Encode).ok()
    }

    /// Transcoder with `caps` and `plan` forced past `open`, absent without hardware.
    fn forced<'a>(
        decode: &'a crate::device::VulkanDevice,
        encode: &'a crate::device::VulkanDevice,
        caps: GpuCaps,
        plan: Plan,
    ) -> Option<super::Transcoder<'a>> {
        let decoder = super::DecodeRouted::H264(crate::decoder::H264Decoder::open(decode).ok()?);
        Some(super::Transcoder {
            decoder,
            encode_device: encode,
            encode_op: CodecOp::H264Encode,
            encoder: None,
            config: super::legacy_h264(),
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            plan,
        })
    }

    /// Live H.264 decode plus encode devices, absent without hardware.
    fn devices() -> Option<(crate::device::VulkanDevice, crate::device::VulkanDevice)> {
        let decode = match crate::device::VulkanDevice::open_or_skip(CodecOp::H264Decode) {
            Ok(device) => device,
            Err(VulkanError::BackendAbsent(reason)) => {
                println!("declined: h264 decode device: {reason}");
                return None;
            }
            Err(error) => panic!("negotiation runs: {error}"),
        };
        let encode = match crate::device::VulkanDevice::open_or_skip(CodecOp::H264Encode) {
            Ok(device) => device,
            Err(VulkanError::BackendAbsent(reason)) => {
                println!("declined: h264 encode device: {reason}");
                return None;
            }
            Err(error) => panic!("negotiation runs: {error}"),
        };
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
        if transcoder.plan().route() != Route::Gpu {
            return;
        }
        let packets = transcoder.flush().expect("flush drains");
        assert_eq!(packets, []);
    }

    #[test]
    fn double_flush_stays_empty() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
            return;
        };
        if transcoder.plan().route() != Route::Gpu {
            return;
        }
        assert_eq!(transcoder.flush().expect("first flush"), []);
        assert_eq!(transcoder.flush().expect("second flush"), []);
    }

    #[test]
    fn empty_packet_yields_no_packets() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
            return;
        };
        if transcoder.plan().route() != Route::Gpu {
            return;
        }
        let packet = Packet::new(Bytes::new());
        let packets = transcoder.transcode(&packet).expect("empty packet");
        assert_eq!(packets, []);
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
    fn flush_keeps_plan() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let Some(mut transcoder) = pair(&decode, &encode) else {
            return;
        };
        if transcoder.plan().route() != Route::Gpu {
            return;
        }
        let plan = transcoder.plan();
        let packets = transcoder.flush().expect("flush drains");
        assert_eq!(packets, []);
        assert_eq!(transcoder.plan(), plan);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn caps_miss_declines_typed() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        #[cfg(windows)]
        let caps = GpuCaps::new(Some(ShareMechanism::NtHandle), true, false);
        #[cfg(unix)]
        let caps = GpuCaps::new(Some(ShareMechanism::DmaBuf), true, false);
        assert!(!caps.ready());
        let Some(mut transcoder) = forced(
            &decode,
            &encode,
            caps,
            Plan::cpu(FallbackCause::EncoderCpuOnly),
        ) else {
            return;
        };
        let packet = Packet::new(Bytes::new());
        assert_eq!(
            transcoder
                .transcode(&packet)
                .expect_err("caps miss declines")
                .to_string(),
            "unsupported encoder without gpu import"
        );
        assert_eq!(
            transcoder.flush().expect_err("drain declines").to_string(),
            "unsupported encoder without gpu import"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn device_mismatch_declines_typed() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        #[cfg(windows)]
        let caps = GpuCaps::new(Some(ShareMechanism::NtHandle), true, true);
        #[cfg(unix)]
        let caps = GpuCaps::new(Some(ShareMechanism::DmaBuf), true, true);
        assert!(caps.ready());
        let Some(mut transcoder) = forced(
            &decode,
            &encode,
            caps,
            Plan::cpu(FallbackCause::DeviceMismatch),
        ) else {
            return;
        };
        let packet = Packet::new(Bytes::new());
        assert_eq!(
            transcoder
                .transcode(&packet)
                .expect_err("mismatch declines")
                .to_string(),
            "unsupported decode and encode devices differ"
        );
        assert_eq!(
            transcoder.flush().expect_err("drain declines").to_string(),
            "unsupported decode and encode devices differ"
        );
    }

    /// H.264 packets coding the synth IDR plus P picture.
    fn input_packets() -> Vec<Packet> {
        rust_video_harness::H264Synth::annex_b_packets()
            .iter()
            .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
            .collect()
    }

    /// Concatenated bytes transcoding `packets` plus the flush drain.
    fn transcode_all(transcoder: &mut super::Transcoder<'_>, packets: &[Packet]) -> Vec<u8> {
        let mut out = Vec::new();
        for packet in packets {
            for coded in transcoder.transcode(packet).expect("transcode runs") {
                out.extend_from_slice(coded.data());
            }
        }
        for coded in transcoder.flush().expect("flush runs") {
            out.extend_from_slice(coded.data());
        }
        out
    }

    /// Solid-black NV12 frame at `size`.
    fn black(size: PictureSize) -> CpuFrame {
        let pixels = usize::try_from(size.width()).expect("width fits")
            * usize::try_from(size.height()).expect("height fits");
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(vec![0; pixels]), size.width()).expect("luma"),
                Plane::new(Bytes::from(vec![128; pixels / 2]), size.width()).expect("chroma"),
            ],
        )
        .expect("black frame")
    }

    #[test]
    fn config_changes_output_size_class() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let packets = input_packets();
        let mut sizes = Vec::new();
        for bitrate in [500_000u32, 8_000_000] {
            let config = rust_video_caps::encode::EncodeConfig {
                rate_control: rust_video_caps::encode::RateControl::Cbr {
                    bitrate,
                    maxrate: bitrate,
                    bufsize: bitrate,
                },
                ..rust_video_caps::encode::EncodeConfig::default()
            };
            let Some(unconfigured) = pair(&decode, &encode) else {
                return;
            };
            let mut transcoder = unconfigured.with_config(config);
            if transcoder.plan().route() != Route::Gpu {
                return;
            }
            let mut out = Vec::new();
            let mut run = || -> Result<(), crate::error::VulkanError> {
                for packet in &packets {
                    for coded in transcoder.transcode(packet)? {
                        out.extend_from_slice(coded.data());
                    }
                }
                for coded in transcoder.flush()? {
                    out.extend_from_slice(coded.data());
                }
                Ok(())
            };
            match run() {
                Ok(()) => {}
                Err(crate::error::VulkanError::Declined(decline)) => {
                    println!("declined: cbr size class without driver mode: {decline}");
                    return;
                }
                Err(error) => panic!("transcode runs: {error}"),
            }
            assert!(!out.is_empty(), "cbr {bitrate} emits bytes");
            sizes.push(out.len());
        }
        assert!(
            sizes[1] * 2 > sizes[0] * 3,
            "bitrate knob moves size class past 1.5x"
        );
    }

    #[test]
    fn cpu_callback_alters_output_bytes() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let packets = input_packets();
        let Some(mut plain) = pair(&decode, &encode) else {
            return;
        };
        if plain.plan().route() != Route::Gpu {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        assert!(!expected.is_empty(), "plain transcode emits bytes");
        let Some(mut filtered) = pair(&decode, &encode) else {
            return;
        };
        let mut blacken = |frame: &mut CpuFrame| {
            *frame = black(frame.size());
        };
        let mut out = Vec::new();
        for packet in &packets {
            for coded in filtered
                .transcode_with(packet, &mut blacken)
                .expect("filter runs")
            {
                out.extend_from_slice(coded.data());
            }
        }
        for coded in filtered.flush_with(&mut blacken).expect("drain runs") {
            out.extend_from_slice(coded.data());
        }
        assert!(!out.is_empty(), "filtered transcode emits bytes");
        assert_ne!(
            out, expected,
            "replaced frames re-encode to different bytes"
        );
    }

    #[test]
    fn noop_callback_matches_plain_transcode() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let packets = input_packets();
        let Some(mut plain) = pair(&decode, &encode) else {
            return;
        };
        if plain.plan().route() != Route::Gpu {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        let Some(mut filtered) = pair(&decode, &encode) else {
            return;
        };
        let mut noop = |_frame: &mut CpuFrame| {};
        let mut out = Vec::new();
        for packet in &packets {
            for coded in filtered
                .transcode_with(packet, &mut noop)
                .expect("pass runs")
            {
                out.extend_from_slice(coded.data());
            }
        }
        for coded in filtered.flush_with(&mut noop).expect("drain runs") {
            out.extend_from_slice(coded.data());
        }
        assert_eq!(out, expected, "no-op callback preserves bytes");
    }

    #[test]
    fn surface_filter_observes_every_picture() {
        let Some((decode, encode)) = devices() else {
            return;
        };
        let packets = input_packets();
        let Some(mut plain) = pair(&decode, &encode) else {
            return;
        };
        if plain.plan().route() != Route::Gpu {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        let Some(mut hooked) = pair(&decode, &encode) else {
            return;
        };
        let mut sizes = Vec::new();
        let mut out = Vec::new();
        let mut observe = |surface: &mut crate::decoder::VulkanFrame| {
            sizes.push(surface.size());
        };
        for packet in &packets {
            for coded in hooked
                .transcode_surface_with(packet, &mut observe)
                .expect("hook runs")
            {
                out.extend_from_slice(coded.data());
            }
        }
        for coded in hooked.flush().expect("flush runs") {
            out.extend_from_slice(coded.data());
        }
        assert!(!sizes.is_empty(), "hook observes decoded pictures");
        let extent = PictureSize::new(64, 64).expect("nonzero extent");
        assert!(
            sizes.iter().all(|size| *size == extent),
            "hook observes decoded extents"
        );
        assert_eq!(out, expected, "observing filter preserves bytes");
    }
}
