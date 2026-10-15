//! GPU scale between decode and encode surfaces over the video processor.
//!
//! <https://docs.rs/windows/0.62.2/windows/Win32/Graphics/Direct3D11/>
//! <https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11videocontext-videoprocessorblt>

#![allow(unsafe_code)]

#[cfg(test)]
#[path = "../examples/common/h264.rs"]
mod fixture;

use std::mem::ManuallyDrop;

use crate::decoder::GpuSurface;
use crate::device::D3d11Device;
use crate::error::D3d11Error;
use crate::session::flag_word;

use rust_video_caps::common::PictureSize;
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};

/// NV12 scale over one source and target extent.
pub(crate) struct Scaler {
    /// Video processor behind the scale.
    processor: ID3D11VideoProcessor,
    /// Enumerator behind the views.
    enumerator: ID3D11VideoProcessorEnumerator,
    /// Output view over the target.
    output_view: ID3D11VideoProcessorOutputView,
    /// NV12 target texture.
    target: ID3D11Texture2D,
    /// Video device behind input views.
    video_device: ID3D11VideoDevice,
    /// Video context behind the scale.
    video_context: ID3D11VideoContext,
    /// Source extent.
    from: PictureSize,
    /// Target extent.
    to: PictureSize,
}

/// Scaled NV12 surface on the GPU.
pub(crate) struct ScaledSurface {
    /// NV12 target texture.
    texture: ID3D11Texture2D,
    /// Target extent.
    size: PictureSize,
}

impl ScaledSurface {
    /// NV12 target texture.
    #[must_use]
    pub(crate) fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// Target extent in luma samples.
    #[must_use]
    pub(crate) fn size(&self) -> PictureSize {
        self.size
    }
}

impl Scaler {
    /// Scaler over `device` from `from` to `to`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without video processor NV12 scale for the pair or
    /// when the driver refuses the processor.
    pub(crate) fn open(
        device: &D3d11Device,
        from: PictureSize,
        to: PictureSize,
    ) -> Result<Self, D3d11Error> {
        if !from.width().is_multiple_of(2)
            || !from.height().is_multiple_of(2)
            || !to.width().is_multiple_of(2)
            || !to.height().is_multiple_of(2)
        {
            return Err(D3d11Error::Unsupported("odd NV12 extent".to_owned()));
        }
        let rate = DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: from.width(),
            InputHeight: from.height(),
            OutputFrameRate: rate,
            OutputWidth: to.width(),
            OutputHeight: to.height(),
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // Sound: the descriptor stays live for the call.
        let enumerator = unsafe {
            device
                .video_device()
                .CreateVideoProcessorEnumerator(std::ptr::addr_of!(desc))
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateVideoProcessorEnumerator",
            hresult: error.code().0,
        })?;
        // Sound: the format is a plain input value.
        let flags =
            unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }.map_err(|error| {
                D3d11Error::Call {
                    call: "CheckVideoProcessorFormat",
                    hresult: error.code().0,
                }
            })?;
        let need = u32::try_from(
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0
                | D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0,
        )
        .unwrap_or(0);
        if flags & need != need {
            return Err(D3d11Error::Unsupported(
                "video processor without nv12 scale".to_owned(),
            ));
        }
        // Sound: the enumerator is live.
        let processor = unsafe { device.video_device().CreateVideoProcessor(&enumerator, 0) }
            .map_err(|error| D3d11Error::Call {
                call: "CreateVideoProcessor",
                hresult: error.code().0,
            })?;
        let target = create_target(device, to)?;
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view = None;
        // Sound: the texture and descriptor stay live for the call.
        unsafe {
            device.video_device().CreateVideoProcessorOutputView(
                &target,
                &enumerator,
                std::ptr::addr_of!(desc),
                Some(std::ptr::addr_of_mut!(output_view)),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateVideoProcessorOutputView",
            hresult: error.code().0,
        })?;
        let Some(output_view) = output_view else {
            return Err(D3d11Error::Call {
                call: "CreateVideoProcessorOutputView",
                hresult: windows::Win32::Foundation::E_FAIL.0,
            });
        };
        Ok(Self {
            processor,
            enumerator,
            output_view,
            target,
            video_device: device.video_device().clone(),
            video_context: device.video_context().clone(),
            from,
            to,
        })
    }

    /// Source extent behind the scaler.
    #[must_use]
    pub(crate) fn from(&self) -> PictureSize {
        self.from
    }

    /// Target extent behind the scaler.
    #[must_use]
    pub(crate) fn to(&self) -> PictureSize {
        self.to
    }

    /// `surface` scaled to the target extent on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a source outside the scaler extent or when the
    /// driver refuses the scale.
    pub(crate) fn scale(&self, surface: &GpuSurface) -> Result<ScaledSurface, D3d11Error> {
        if surface.size() != self.from {
            return Err(D3d11Error::Unsupported(
                "scale source outside scaler".to_owned(),
            ));
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: u32::from(surface.slot()),
                },
            },
        };
        let texture: &ID3D11Texture2D = surface.export();
        let mut view = None;
        // Sound: the texture and descriptor stay live for the call.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                texture,
                &self.enumerator,
                std::ptr::addr_of!(desc),
                Some(std::ptr::addr_of_mut!(view)),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateVideoProcessorInputView",
            hresult: error.code().0,
        })?;
        let Some(view): Option<ID3D11VideoProcessorInputView> = view else {
            return Err(D3d11Error::Call {
                call: "CreateVideoProcessorInputView",
                hresult: windows::Win32::Foundation::E_FAIL.0,
            });
        };
        let size = surface.size();
        let source = crop_rect(surface.left(), surface.top(), size)?;
        let target = full_rect(self.to)?;
        // Sound: the processor and rects stay live for each call.
        unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(
                &self.processor,
                0,
                true,
                Some(std::ptr::from_ref(&source)),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                &self.processor,
                0,
                true,
                Some(std::ptr::from_ref(&target)),
            );
        }
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(view)),
            ..Default::default()
        };
        // Sound: the stream borrows the live input view.
        unsafe {
            self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.output_view,
                0,
                std::slice::from_ref(&stream),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "VideoProcessorBlt",
            hresult: error.code().0,
        })?;
        Ok(ScaledSurface {
            texture: self.target.clone(),
            size: self.to,
        })
    }
}

/// NV12 target texture at `to` behind the output view.
fn create_target(device: &D3d11Device, to: PictureSize) -> Result<ID3D11Texture2D, D3d11Error> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: to.width(),
        Height: to.height(),
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: flag_word(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0),
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut target = None;
    // Sound: the descriptor stays live for the call.
    unsafe {
        device.device().CreateTexture2D(
            std::ptr::addr_of!(desc),
            None,
            Some(std::ptr::addr_of_mut!(target)),
        )
    }
    .map_err(|error| D3d11Error::Call {
        call: "CreateTexture2D",
        hresult: error.code().0,
    })?;
    target.ok_or(D3d11Error::Call {
        call: "CreateTexture2D",
        hresult: windows::Win32::Foundation::E_FAIL.0,
    })
}

/// Visible rectangle at (`left`, `top`) sized `size`.
fn crop_rect(left: u32, top: u32, size: PictureSize) -> Result<RECT, D3d11Error> {
    let left = i32::try_from(left).map_err(|_| D3d11Error::Parse("extent range".to_owned()))?;
    let top = i32::try_from(top).map_err(|_| D3d11Error::Parse("extent range".to_owned()))?;
    let right = left
        .checked_add(i32::try_from(size.width()).unwrap_or(i32::MAX))
        .ok_or_else(|| D3d11Error::Parse("extent range".to_owned()))?;
    let bottom = top
        .checked_add(i32::try_from(size.height()).unwrap_or(i32::MAX))
        .ok_or_else(|| D3d11Error::Parse("extent range".to_owned()))?;
    Ok(RECT {
        left,
        top,
        right,
        bottom,
    })
}

/// Full-frame rectangle for `size`.
fn full_rect(size: PictureSize) -> Result<RECT, D3d11Error> {
    crop_rect(0, 0, size)
}

#[cfg(test)]
mod tests {
    #[test]
    fn scale_blits_decoded_surface() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let mut decoder = crate::decoder::H264Decoder::open(&device);
        let mut surfaces = Vec::new();
        for raw in super::fixture::annex_b_packets() {
            let packet = rust_video_caps::packet::Packet::new(bytes::Bytes::from(raw));
            match decoder.decode_surface(&packet) {
                Ok(more) => surfaces.extend(more),
                Err(crate::error::D3d11Error::Skipped(_)) => return,
                Err(error) => panic!("decode: {error}"),
            }
        }
        match decoder.flush_surface() {
            Ok(more) => surfaces.extend(more),
            Err(crate::error::D3d11Error::Skipped(_)) => return,
            Err(error) => panic!("flush: {error}"),
        }
        assert!(!surfaces.is_empty());
        let from = surfaces[0].size();
        let to = rust_video_caps::common::PictureSize::new(32, 32).expect("nonzero extent");
        let scaler = match super::Scaler::open(&device, from, to) {
            Ok(scaler) => scaler,
            Err(crate::error::D3d11Error::Unsupported(_)) => return,
            Err(error) => panic!("scale probe failed: {error}"),
        };
        let out = scaler.scale(&surfaces[0]).expect("blt");
        assert_eq!(out.size(), to);
    }

    #[test]
    fn open_probes_video_processor_support() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let from = rust_video_caps::common::PictureSize::new(320, 240).expect("nonzero extent");
        let to = rust_video_caps::common::PictureSize::new(160, 120).expect("nonzero extent");
        match super::Scaler::open(&device, from, to) {
            Ok(scaler) => {
                assert_eq!(scaler.from(), from);
                assert_eq!(scaler.to(), to);
            }
            Err(crate::error::D3d11Error::Unsupported(_)) => {}
            Err(error) => panic!("scale probe failed: {error}"),
        }
    }

    #[test]
    fn open_rejects_odd_extent() {
        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let even = rust_video_caps::common::PictureSize::new(320, 240).expect("nonzero extent");
        let odd = rust_video_caps::common::PictureSize::new(161, 120).expect("nonzero extent");
        assert!(matches!(
            super::Scaler::open(&device, odd, even),
            Err(crate::error::D3d11Error::Unsupported(_))
        ));
        assert!(matches!(
            super::Scaler::open(&device, even, odd),
            Err(crate::error::D3d11Error::Unsupported(_))
        ));
    }
}
