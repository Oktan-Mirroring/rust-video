//! GPU transcode: decode surface to encoder texture without readback.
//!
//! Export probe over COM/D3D11: live descriptors, checked handles.

use crate::device::D3d11Device;
use crate::encoder::D3d11Encoder;
use crate::error::{D3d11Error, UnsupportedReason};
use crate::frame::D3d11Frame;
use crate::guid::Codec;
use crate::scale::{ScaledSurface, Scaler};
use crate::texture::GpuSurface;
// Crate root re-exports the codec decoder; the module path differs.
use crate::H264Decoder;

use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::EncodeConfig;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::reason::UnsupportedReason as StreamCore;
use rust_video_caps::traits::{Decoder, EncodeSurface, Encoder, SurfaceFilter};
use rust_video_caps::transcode::FallbackCause;
use rust_video_caps::transcode::{Convert, GpuCaps, Plan, Scale};

/// GPU transcode from decode surface to encoder texture.
pub struct Transcoder<'a> {
    /// Heap decoder; video UMD creation overflows beside by-value frames.
    decoder: Box<H264Decoder<'a>>,
    /// Decode codec behind caps recompute.
    decode: Codec,
    /// Encoder behind surfaces.
    encoder: D3d11Encoder,
    /// Encode config behind the live encoder.
    config: EncodeConfig,
    /// Pending encode config, applied at the next transcode call.
    pending: Option<EncodeConfig>,
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
    /// Returns `Err` without surface decode for `decode`, without
    /// shared-texture export on the device, or without an `encode`
    /// encoder MFT on the machine.
    pub fn open(device: &'a D3d11Device, decode: Codec, encode: Codec) -> Result<Self, D3d11Error> {
        if decode != Codec::H264 {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::DecodeWithoutSurfaceDecode { codec: decode },
            ));
        }
        let share = device.share_mechanism().ok_or_else(declined_export)?;
        let config = D3d11Encoder::auto_config(encode).ok_or(D3d11Error::Unsupported(
            UnsupportedReason::NoEncoder { codec: encode },
        ))?;
        let encoder = D3d11Encoder::open(device, config)?;
        let caps = GpuCaps::new(Some(share), device.supports(decode), encoder.gpu_ready());
        let plan = Plan::for_caps(&caps);
        let decoder = Box::new(H264Decoder::open(device));
        Ok(Self {
            decoder,
            decode,
            encoder,
            config,
            pending: None,
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
    ///
    /// The encoder re-opens from `config` at the next transcode call, so
    /// the stream restarts on fresh headers plus an IDR; `caps` and `plan`
    /// refresh then too. The tier mapping behind [`D3d11Encoder::open`]
    /// applies unchanged, pass-through with no re-mapping.
    ///
    /// # Arguments
    ///
    /// * `config` - rate, structure, quality, profile, level, depth,
    ///   chroma, frame rate, and color behind the encoder.
    #[must_use]
    pub fn with_config(mut self, config: EncodeConfig) -> Self {
        self.pending = Some(config);
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
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the picture.
    pub fn transcode(&mut self, packet: &Packet) -> Result<Vec<Packet>, D3d11Error> {
        self.apply_pending()?;
        self.check_ready()?;
        self.transcode_gpu(packet)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a transcode hop missed its capability, when the
    /// scale hop misses its target, or when either backend refuses the drain.
    pub fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        self.apply_pending()?;
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
    /// scale or convert hop misses its target, when the download
    /// truncates, or when either backend refuses the picture.
    pub fn transcode_with(
        &mut self,
        packet: &Packet,
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, D3d11Error> {
        self.apply_pending()?;
        self.check_ready()?;
        let frames = self.decoder.decode(packet)?;
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
    /// scale or convert hop misses its target, when the download
    /// truncates, or when either backend refuses the drain.
    pub fn flush_with(
        &mut self,
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, D3d11Error> {
        self.apply_pending()?;
        self.check_ready()?;
        let frames = self.decoder.flush()?;
        let mut packets = self.encode_cpu(&frames, filter)?;
        packets.append(&mut self.encoder.flush()?);
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
        filter: &mut impl SurfaceFilter<GpuSurface>,
    ) -> Result<Vec<Packet>, D3d11Error> {
        self.apply_pending()?;
        self.check_ready()?;
        let mut surfaces = self.decoder.decode_surface(packet)?;
        for surface in &mut surfaces {
            filter.filter(surface);
        }
        self.encode_surfaces(&surfaces)
    }

    /// Encoder re-opened from the pending config, if any.
    fn apply_pending(&mut self) -> Result<(), D3d11Error> {
        let Some(config) = self.pending.take() else {
            return Ok(());
        };
        if config == self.config {
            return Ok(());
        }
        let encoder = D3d11Encoder::open(self.device, config)?;
        self.config = config;
        self.encoder = encoder;
        self.caps = GpuCaps::new(
            self.device.share_mechanism(),
            self.device.supports(self.decode),
            self.encoder.gpu_ready(),
        );
        self.plan = Plan::for_caps(&self.caps);
        Ok(())
    }

    /// GPU route unless a hop missed, else the typed decline naming the miss.
    fn check_ready(&self) -> Result<(), D3d11Error> {
        if self.caps.ready() {
            Ok(())
        } else if let Some(cause) = self.plan.cause() {
            Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::Fallback { cause },
            )))
        } else {
            // open derives plan from caps, so a miss names its cause.
            Ok(())
        }
    }

    /// Packets transcoded from `packet` through GPU surfaces.
    fn transcode_gpu(&mut self, packet: &Packet) -> Result<Vec<Packet>, D3d11Error> {
        let surfaces = self.decoder.decode_surface(packet)?;
        self.encode_surfaces(&surfaces)
    }

    /// Drained packets through GPU surfaces.
    fn flush_gpu(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        let surfaces = self.decoder.flush_surface()?;
        let mut packets = self.encode_surfaces(&surfaces)?;
        packets.append(&mut self.encoder.flush()?);
        Ok(packets)
    }

    /// Packets coded from downloaded `frames` through `filter`.
    fn encode_cpu(
        &mut self,
        frames: &[D3d11Frame],
        filter: &mut impl FnMut(&mut CpuFrame),
    ) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        for frame in frames {
            let mut cpu =
                frame
                    .to_cpu()
                    .ok_or(D3d11Error::Unsupported(UnsupportedReason::Stream(
                        StreamCore::Nv12ShortOfExtent,
                    )))?;
            if let Some(cause) = self.convert.miss(cpu.format()) {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                    StreamCore::Fallback { cause },
                )));
            }
            if let Some(cause) = self.scale.miss(cpu.size()) {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                    StreamCore::Fallback { cause },
                )));
            }
            filter(&mut cpu);
            packets.append(&mut self.encoder.encode(&cpu)?);
        }
        Ok(packets)
    }

    /// Packets coded from GPU `surfaces`.
    fn encode_surfaces(&mut self, surfaces: &[GpuSurface]) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        for surface in surfaces {
            if let Some(cause) = self.convert.miss(PixelFormat::Nv12) {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                    StreamCore::Fallback { cause },
                )));
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
            return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::Fallback { cause },
            )));
        };
        if self
            .scaler
            .as_ref()
            .is_none_or(|scaler| scaler.from() != from || scaler.to() != to)
        {
            match Scaler::open(self.device, from, to) {
                Ok(scaler) => self.scaler = Some(scaler),
                Err(D3d11Error::Unsupported(_)) => {
                    return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                        StreamCore::Fallback { cause },
                    )));
                }
                Err(error) => return Err(error),
            }
        }
        let scaler = self.scaler.as_ref().ok_or(D3d11Error::NoSession)?;
        scaler.scale(surface)
    }
}

/// Typed decline for a device without shared-texture export.
fn declined_export() -> D3d11Error {
    D3d11Error::Unsupported(UnsupportedReason::Stream(StreamCore::Fallback {
        cause: FallbackCause::NoSharedExport,
    }))
}

#[cfg(test)]
mod tests {
    use crate::GpuSurface;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::encode::{EncodeConfig, RateControl};
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Encoder;
    use rust_video_caps::transcode::{
        Convert, FallbackCause, GpuCaps, Plan, Scale, ShareMechanism,
    };
    use rust_video_harness::Synth;

    /// Live H.264 transcoder, absent without hardware.
    fn transcoder(device: &crate::device::D3d11Device) -> Option<super::Transcoder<'_>> {
        super::Transcoder::open(device, crate::guid::Codec::H264, crate::guid::Codec::H264).ok()
    }

    /// Transcoder with `caps` forced past `open`, absent without hardware.
    fn forced(
        device: &crate::device::D3d11Device,
        caps: GpuCaps,
        cause: FallbackCause,
    ) -> Option<super::Transcoder<'_>> {
        let config =
            crate::encoder::D3d11Encoder::auto_config(crate::guid::Codec::H264).expect("h264 auto");
        let encoder = crate::encoder::D3d11Encoder::open(device, config).ok()?;
        Some(super::Transcoder {
            decoder: Box::new(crate::H264Decoder::open(device)),
            decode: crate::guid::Codec::H264,
            encoder,
            config,
            pending: None,
            caps,
            scale: Scale::passthrough(),
            convert: Convert::passthrough(),
            device,
            scaler: None,
            plan: Plan::cpu(cause),
        })
    }

    /// H.264 packets coding 8 noise frames plus the flush drain, absent without hardware.
    fn input_packets(device: &crate::device::D3d11Device) -> Option<Vec<Packet>> {
        let config =
            crate::encoder::D3d11Encoder::auto_config(crate::guid::Codec::H264).expect("h264 auto");
        let mut encoder = crate::encoder::D3d11Encoder::open(device, config).ok()?;
        let mut packets = Vec::new();
        for seed in 0..8u64 {
            packets.extend(encoder.encode(&Synth::noise(320, 240, seed)).ok()?);
        }
        packets.extend(encoder.flush().ok()?);
        Some(packets)
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
    fn declined_export_names_export() {
        let error = super::declined_export();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported stream: decode texture without share handle"
        );
    }

    #[test]
    fn open_rejects_decode_without_surface_decode() {
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
        if !transcoder.caps().ready() {
            return;
        }
        assert_eq!(transcoder.flush().expect("flush drains"), []);
    }

    #[test]
    fn double_flush_stays_empty() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        assert_eq!(transcoder.flush().expect("first flush"), []);
        assert_eq!(transcoder.flush().expect("second flush"), []);
    }

    #[test]
    fn empty_packet_yields_no_packets() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        let packet = Packet::new(Bytes::new());
        assert_eq!(transcoder.transcode(&packet).expect("empty packet"), []);
    }

    #[test]
    fn truncated_packet_yields_no_packets() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        if !transcoder.caps().ready() {
            return;
        }
        let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1]));
        assert_eq!(transcoder.transcode(&packet).expect("truncated packet"), []);
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

    #[test]
    fn caps_miss_declines_typed() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let caps = GpuCaps::new(Some(ShareMechanism::NtHandle), true, false);
        assert!(!caps.ready());
        let Some(mut transcoder) = forced(&device, caps, FallbackCause::EncoderCpuOnly) else {
            return;
        };
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
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(packets) = input_packets(&device) else {
            return;
        };
        let mut sizes = Vec::new();
        for bitrate in [500_000u32, 8_000_000] {
            let config = EncodeConfig {
                rate_control: RateControl::Cbr {
                    bitrate,
                    maxrate: bitrate,
                    bufsize: bitrate,
                },
                ..EncodeConfig::default()
            };
            let mut transcoder = match super::Transcoder::open(
                &device,
                crate::guid::Codec::H264,
                crate::guid::Codec::H264,
            ) {
                Ok(transcoder) => transcoder.with_config(config),
                Err(_) => return,
            };
            if !transcoder.caps().ready() {
                return;
            }
            let bytes = transcode_all(&mut transcoder, &packets);
            assert!(!bytes.is_empty(), "cbr {bitrate} emits bytes");
            sizes.push(bytes.len());
        }
        assert!(
            sizes[1] * 2 > sizes[0] * 3,
            "bitrate knob moves size class past 1.5x"
        );
    }

    #[test]
    fn cpu_callback_alters_output_bytes() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(packets) = input_packets(&device) else {
            return;
        };
        let Some(mut plain) = transcoder(&device) else {
            return;
        };
        if !plain.caps().ready() {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        assert!(!expected.is_empty(), "plain transcode emits bytes");
        let Some(mut filtered) = transcoder(&device) else {
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
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(packets) = input_packets(&device) else {
            return;
        };
        let Some(mut plain) = transcoder(&device) else {
            return;
        };
        if !plain.caps().ready() {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        let Some(mut filtered) = transcoder(&device) else {
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
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(packets) = input_packets(&device) else {
            return;
        };
        let Some(mut plain) = transcoder(&device) else {
            return;
        };
        if !plain.caps().ready() {
            return;
        }
        let expected = transcode_all(&mut plain, &packets);
        let Some(mut hooked) = transcoder(&device) else {
            return;
        };
        let mut sizes = Vec::new();
        let mut out = Vec::new();
        let mut observe = |surface: &mut GpuSurface| {
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
        let extent = PictureSize::new(320, 240).expect("nonzero extent");
        assert!(
            sizes.iter().all(|size| *size == extent),
            "hook observes decoded extents"
        );
        assert_eq!(out, expected, "observing filter preserves bytes");
    }
}
