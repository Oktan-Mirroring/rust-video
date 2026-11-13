//! GPU transcode: decode surface to encoder texture without readback.
//!
//! Export probe over COM/D3D11: live descriptors, checked handles.

#![allow(unsafe_code)]

use crate::decoder::GpuSurface;
use crate::decoder::{D3d11Frame, H264Decoder};
use crate::device::D3d11Device;
use crate::encoder::D3d11Encoder;
use crate::error::D3d11Error;
use crate::guid::Codec;
use crate::scale::{ScaledSurface, Scaler};
use crate::submit::flag_word;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::FallbackCause;
use rust_video_caps::transcode::{Convert, GpuCaps, Outcome, Plan, Scale, ShareMechanism};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource;

/// Probe texture extent in luma samples.
const PROBE_EXTENT: u32 = 64;

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
    /// Returns `Err` without a `decode` picture builder, without
    /// shared-texture export on the device, or without an `encode`
    /// encoder MFT on the machine.
    pub fn open(device: &'a D3d11Device, decode: Codec, encode: Codec) -> Result<Self, D3d11Error> {
        if decode != Codec::H264 {
            return Err(D3d11Error::Unsupported(format!("{decode} decode pending")));
        }
        let share = shared_export(device).ok_or_else(declined_export)?;
        let encoder = D3d11Encoder::open(device, encode)?;
        let caps = GpuCaps::new(Some(share), device.supports(decode), encoder.gpu_ready());
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

/// NT-handle share behind `device`, absent without shared-texture export.
fn shared_export(device: &D3d11Device) -> Option<ShareMechanism> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: PROBE_EXTENT,
        Height: PROBE_EXTENT,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: flag_word(D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0),
        CPUAccessFlags: 0,
        MiscFlags: flag_word(D3D11_RESOURCE_MISC_SHARED.0),
    };
    let mut texture = None;
    // Sound: the descriptor stays live for the call.
    let created = unsafe {
        device.device().CreateTexture2D(
            std::ptr::addr_of!(desc),
            None,
            Some(std::ptr::addr_of_mut!(texture)),
        )
    };
    let (Ok(()), Some(texture)) = (created, texture) else {
        return None;
    };
    let resource: IDXGIResource = texture.cast().ok()?;
    // Sound: the resource is live.
    let handle = unsafe { resource.GetSharedHandle() }.ok()?;
    (!handle.0.is_null()).then_some(ShareMechanism::NtHandle)
}

/// Typed decline for a device without shared-texture export.
fn declined_export() -> D3d11Error {
    D3d11Error::Unsupported(format!(
        "export declined: {}",
        FallbackCause::NoSharedExport
    ))
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
    fn declined_export_names_export() {
        let error = super::declined_export();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported stream: export declined: decode texture without share handle"
        );
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
        assert_eq!(outcome.packets(), []);
    }

    #[test]
    fn double_flush_stays_empty() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Some(mut transcoder) = transcoder(&device) else {
            return;
        };
        assert_eq!(transcoder.flush().expect("first flush").packets(), []);
        assert_eq!(transcoder.flush().expect("second flush").packets(), []);
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
        assert_eq!(outcome.packets(), []);
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
        assert_eq!(outcome.packets(), []);
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
