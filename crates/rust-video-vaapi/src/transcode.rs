//! GPU transcode: decode surface to encoder input without readback.

use crate::device::VaApiDevice;
use crate::encoder::{va_profile, VaApiEncoder};
use crate::error::VaApiError;
use crate::ffi;
use crate::h264::decoder::{H264Decoder, VaApiFrame};
use crate::scale::{ScaledSurface, Scaler};

use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::EncodeConfig;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::reason::{ParseReason as CapsParse, UnsupportedReason as CapsUnsupported};
use rust_video_caps::traits::{Decoder, Encoder, SurfaceFilter};
use rust_video_caps::transcode::{Convert, FallbackCause, GpuCaps, Plan, Scale, ShareMechanism};
use rust_video_core::Codec;

/// GPU transcode from decode surface to encoder input.
///
/// The encoder opens lazily on the first picture, when the decode
/// size is known, and re-opens when a reconfig changes it; the fresh
/// session starts on an IDR, so the stream stays valid across the
/// swap. Stays on its thread with the decoder it owns.
pub struct Transcoder<'a> {
    /// H.264 decoder; the only wired frontend.
    decoder: H264Decoder<'a>,
    /// Encode config behind the lazy session.
    config: EncodeConfig,
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
    /// Transcoder over `device` from `decode` to `encode`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a `decode` frontend, or without
    /// an `encode` encoder on the driver.
    pub fn open(device: &'a VaApiDevice, decode: Codec, encode: Codec) -> Result<Self, VaApiError> {
        if decode != Codec::H264 {
            return Err(VaApiError::DecodePending { codec: decode });
        }
        let config =
            VaApiEncoder::auto_config(encode).ok_or(VaApiError::NoEncoder { codec: encode })?;
        let caps = GpuCaps::new(
            Some(ShareMechanism::DmaBuf),
            device.dmabuf_export(),
            device.supports(va_profile(encode)?, ffi::VA_ENTRYPOINT_ENC_SLICE),
        );
        let plan = Plan::for_caps(&caps);
        Ok(Self {
            decoder: H264Decoder::open(device),
            config,
            encoder: None,
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

    /// Executed transcode plan.
    #[must_use]
    pub fn plan(&self) -> Plan {
        self.plan
    }

    /// Packets transcoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Vec<Packet>, VaApiError> {
        self.check_ready()?;
        self.transcode_gpu(packet)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the drain.
    pub fn flush(&mut self) -> Result<Vec<Packet>, VaApiError> {
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
    ) -> Result<Vec<Packet>, VaApiError> {
        self.check_ready()?;
        let frames = self.decoder.decode(packet)?;
        let mut packets = self.encode_cpu(&frames, filter)?;
        // The upload path holds one picture back; release it per call.
        if let Some(encoder) = self.encoder.as_mut() {
            packets.append(&mut encoder.flush()?);
        }
        Ok(packets)
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
    ) -> Result<Vec<Packet>, VaApiError> {
        self.check_ready()?;
        let frames = self.decoder.flush()?;
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
        filter: &mut impl SurfaceFilter<VaApiFrame>,
    ) -> Result<Vec<Packet>, VaApiError> {
        self.check_ready()?;
        let mut surfaces = self.decoder.decode_surface(packet)?;
        for surface in &mut surfaces {
            filter.filter(surface);
        }
        self.encode_surfaces(&surfaces)
    }

    /// GPU route unless a hop missed, else the typed decline naming the miss.
    fn check_ready(&self) -> Result<(), VaApiError> {
        if self.caps.ready() {
            Ok(())
        } else if let Some(cause) = self.plan.cause() {
            Err(VaApiError::Unsupported(CapsUnsupported::Fallback { cause }))
        } else {
            // open derives plan from caps, so a miss names its cause.
            Ok(())
        }
    }

    /// Packets transcoded from `packet` through device surfaces.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Vec<Packet>, VaApiError> {
        let surfaces = self.decoder.decode_surface(packet)?;
        self.encode_surfaces(&surfaces)
    }

    /// Drained packets through device surfaces.
    fn flush_gpu(&mut self) -> Result<Vec<Packet>, VaApiError> {
        let surfaces = self.decoder.flush_surface()?;
        let mut packets = self.encode_surfaces(&surfaces)?;
        if let Some(encoder) = self.encoder.as_mut() {
            packets.append(&mut encoder.flush()?);
        }
        Ok(packets)
    }

    /// Packets coded from downloaded `frames` through `filter`.
    fn encode_cpu(
        &mut self,
        frames: &[VaApiFrame],
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, VaApiError> {
        let mut packets = Vec::new();
        for frame in frames {
            let mut cpu = frame
                .to_cpu()
                .ok_or(VaApiError::Unsupported(CapsUnsupported::Nv12ShortOfExtent))?;
            if let Some(cause) = self.convert.miss(cpu.format()) {
                return Err(VaApiError::Unsupported(CapsUnsupported::Fallback { cause }));
            }
            if let Some(cause) = self.scale.miss(cpu.size()) {
                return Err(VaApiError::Unsupported(CapsUnsupported::Fallback { cause }));
            }
            filter(&mut cpu);
            packets.append(&mut self.encoder_for(cpu.size())?.encode(&cpu)?);
        }
        Ok(packets)
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
                return Err(VaApiError::Unsupported(CapsUnsupported::Fallback { cause }));
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
            return Err(VaApiError::Unsupported(CapsUnsupported::Fallback { cause }));
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != from || scaler.to() != to)
        {
            match Scaler::open(self.device, from, to) {
                Ok(scaler) => self.scaler = Some(scaler),
                Err(VaApiError::Unsupported(_)) => {
                    return Err(VaApiError::Unsupported(CapsUnsupported::Fallback { cause }));
                }
                Err(error) => return Err(error),
            }
        }
        let scaler = self
            .scaler
            .as_mut()
            .ok_or_else(|| VaApiError::Parse(CapsParse::Range { syntax: "scaler" }))?;
        scaler.scale(surface)
    }

    /// Encoder for `size`, opened or re-opened on demand.
    fn encoder_for(&mut self, size: PictureSize) -> Result<&mut VaApiEncoder, VaApiError> {
        if self
            .encoder
            .as_ref()
            .is_none_or(|encoder| encoder.size() != size)
        {
            let encoder = VaApiEncoder::open(self.device, size, self.config)?;
            self.encoder = Some(encoder);
        }
        match self.encoder.as_mut() {
            Some(encoder) => Ok(encoder),
            None => Err(VaApiError::Parse(CapsParse::Range { syntax: "encoder" })),
        }
    }
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
    use rust_video_core::Codec;

    /// Live H.264 transcoder over `device`.
    fn pair(device: &crate::device::VaApiDevice) -> super::Transcoder<'_> {
        super::Transcoder::open(device, Codec::H264, Codec::H264).expect("open runs")
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

    /// Transcoder with `caps` forced past `open`.
    fn forced(
        device: &crate::device::VaApiDevice,
        caps: GpuCaps,
        cause: FallbackCause,
    ) -> super::Transcoder<'_> {
        super::Transcoder {
            decoder: crate::h264::decoder::H264Decoder::open(device),
            config: crate::encoder::VaApiEncoder::auto_config(Codec::H264).expect("tier recipe"),
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

    #[test]
    fn config_changes_output_size_class() {
        let Ok(device) = crate::device::VaApiDevice::open() else {
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
                profile: rust_video_caps::encode::Profile::H264(
                    rust_video_caps::h264::EncodeProfile::High,
                ),
                ..rust_video_caps::encode::EncodeConfig::default()
            };
            let mut transcoder = pair(&device).with_config(config);
            if transcoder.plan().route() != Route::Gpu {
                return;
            }
            let mut out = Vec::new();
            let mut run = || -> Result<(), crate::error::VaApiError> {
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
                Err(crate::error::VaApiError::Declined(decline)) => {
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
        let Ok(device) = crate::device::VaApiDevice::open() else {
            return;
        };
        let packets = input_packets();
        let mut plain = pair(&device);
        if plain.plan().route() != Route::Gpu {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        assert!(!expected.is_empty(), "plain transcode emits bytes");
        let mut filtered = pair(&device);
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
        let Ok(device) = crate::device::VaApiDevice::open() else {
            return;
        };
        let packets = input_packets();
        let mut plain = pair(&device);
        if plain.plan().route() != Route::Gpu {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        let mut filtered = pair(&device);
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
        let Ok(device) = crate::device::VaApiDevice::open() else {
            return;
        };
        let packets = input_packets();
        let mut plain = pair(&device);
        if plain.plan().route() != Route::Gpu {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        let mut hooked = pair(&device);
        let mut sizes = Vec::new();
        let mut out = Vec::new();
        let mut observe = |surface: &mut crate::h264::decoder::VaApiFrame| {
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
