//! D3D11 pipeline: decode surface, video-process or compute filter, MF encode.

// FFI boundary: video-processor and compute COM calls live here.
#![allow(unsafe_code)]

use std::mem::ManuallyDrop;

use crate::content::{Codec, Op};
use crate::driver::surface::{DecodeSurface, DrainEncoder, SurfacePipeline};

use rust_video::Encoder;
use rust_video::Packet;
use rust_video_d3d11::{
    Av1Decoder, Codec as TierCodec, D3d11Device, D3d11Encoder, GpuSurface, H264Decoder,
    HevcDecoder, Vp9Decoder,
};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2DARRAY;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Buffer, ID3D11ComputeShader, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11UnorderedAccessView, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView, D3D11_BIND_CONSTANT_BUFFER,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS,
    D3D11_BUFFER_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
    D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_ARRAY_SRV, D3D11_TEX2D_UAV, D3D11_TEX2D_VPIV,
    D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_UAV_DIMENSION_TEXTURE2D,
    D3D11_UNORDERED_ACCESS_VIEW_DESC, D3D11_UNORDERED_ACCESS_VIEW_DESC_0, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

/// Grayscale compute bytecode, compiled from `shaders/gray_cs.hlsl` (`cs_5_0`).
const GRAY_CSO: &[u8] = include_bytes!("shaders/gray_cs.cso");

/// GPU filter behind one combo.
pub(crate) enum Filter {
    /// Compute grayscale at full extent.
    Gray(Gray),
    /// Video-process downscale, colorspace, or crop.
    Vproc(Vproc),
}

/// Decode source behind one combo.
pub(crate) enum Source<'a> {
    /// H.264 decoder.
    H264(Box<H264Decoder<'a>>),
    /// HEVC decoder.
    Hevc(Box<HevcDecoder<'a>>),
    /// VP9 decoder.
    Vp9(Box<Vp9Decoder<'a>>),
    /// AV1 decoder.
    Av1(Box<Av1Decoder<'a>>),
}

impl Filter {
    /// Filter for `op`, geometry fixed on first use.
    fn open(device: &D3d11Device, op: Op) -> Result<Self, String> {
        match op {
            Op::Gray => Ok(Self::Gray(Gray::open(device)?)),
            Op::Down | Op::Color | Op::Crop => Ok(Self::Vproc(Vproc::open(device, op)?)),
        }
    }

    /// Filtered texture plus extent for `surface`.
    fn apply(
        &mut self,
        device: &D3d11Device,
        surface: &GpuSurface,
    ) -> Result<(ID3D11Texture2D, u32, u32), String> {
        match self {
            Self::Gray(gray) => gray.apply(device, surface),
            Self::Vproc(vproc) => vproc.apply(device, surface),
        }
    }
}

impl DecodeSurface for Source<'_> {
    type Surface = GpuSurface;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, String> {
        match self {
            Self::H264(decoder) => decoder.decode_surface(packet).map_err(err),
            Self::Hevc(decoder) => decoder.decode_surface(packet).map_err(err),
            Self::Vp9(decoder) => decoder.decode_surface(packet).map_err(err),
            Self::Av1(decoder) => decoder.decode_surface(packet).map_err(err),
        }
    }

    fn flush(&mut self) -> Result<Vec<GpuSurface>, String> {
        match self {
            Self::H264(decoder) => decoder.flush_surface().map_err(err),
            Self::Hevc(decoder) => decoder.flush_surface().map_err(err),
            Self::Vp9(decoder) => decoder.flush_surface().map_err(err),
            Self::Av1(decoder) => decoder.flush_surface().map_err(err),
        }
    }
}

impl DrainEncoder for D3d11Encoder {
    fn drain(&mut self) -> Result<Vec<Packet>, String> {
        self.flush().map_err(err)
    }
}

/// D3D11 backend over the default hardware adapter.
pub struct Backend {
    /// Device behind decoders, filters, and encoders.
    device: D3d11Device,
}

impl Backend {
    /// Backend over the default hardware adapter.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a hardware device.
    pub fn open() -> Result<Self, String> {
        D3d11Device::open()
            .map(|device| Self { device })
            .map_err(err)
    }
}

impl SurfacePipeline for Backend {
    type Surface = GpuSurface;
    type Source<'s> = Source<'s>;
    type Filter<'s> = Filter;
    type Encoder<'s> = D3d11Encoder;

    fn name(&self) -> &'static str {
        "d3d11"
    }

    fn source(&self, codec: Codec) -> Result<Source<'_>, String> {
        Ok(match codec {
            Codec::H264 => Source::H264(Box::new(H264Decoder::open(&self.device))),
            Codec::H265 => Source::Hevc(Box::new(HevcDecoder::open(&self.device))),
            Codec::Vp9 => Source::Vp9(Box::new(Vp9Decoder::open(&self.device))),
            Codec::Av1 => Source::Av1(Box::new(Av1Decoder::open(&self.device))),
        })
    }

    fn filter(&self, op: Op) -> Result<Filter, String> {
        Filter::open(&self.device, op)
    }

    fn encoder(&self, codec: Codec) -> Result<D3d11Encoder, String> {
        let tier = match codec {
            Codec::H264 => TierCodec::H264,
            Codec::H265 => TierCodec::Hevc,
            Codec::Vp9 => TierCodec::Vp9,
            Codec::Av1 => TierCodec::Av1,
        };
        if let Ok(encoder) = D3d11Encoder::open(&self.device, tier) {
            if encoder.uses_d3d11() {
                return Ok(encoder);
            }
        }
        if tier == TierCodec::H264 {
            return Err("no d3d11-bound h264 encoder MFT".to_owned());
        }
        match D3d11Encoder::open(&self.device, TierCodec::H264) {
            Ok(encoder) if encoder.uses_d3d11() => Ok(encoder),
            Ok(_) => Err("no d3d11-bound h264 encoder MFT".to_owned()),
            Err(error) => Err(format!("h264 fallback declined: {}", err(error))),
        }
    }

    fn note(codec: Codec, encoder: &D3d11Encoder) -> String {
        let target = match encoder.codec() {
            TierCodec::H264 => "h264",
            TierCodec::Hevc => "hevc",
            TierCodec::Vp9 => "vp9",
            TierCodec::Av1 => "av1",
            _ => "?",
        };
        format!("{}->{target} via {}", codec.name(), encoder.mft_name())
    }

    fn ext(encoder: &D3d11Encoder) -> &'static str {
        ext(encoder.codec())
    }

    fn encode_surface(
        &self,
        filter: &mut Filter,
        encoder: &mut D3d11Encoder,
        surface: &GpuSurface,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        let (texture, width, height) = filter.apply(&self.device, surface)?;
        for packet in encoder
            .encode_texture(&texture, 0, 0, 0, width, height)
            .map_err(err)?
        {
            out.extend_from_slice(packet.data());
        }
        Ok(())
    }
}

/// Compute grayscale over one extent.
pub(crate) struct Gray {
    /// Compiled grayscale shader.
    shader: ID3D11ComputeShader,
    /// NV12 target texture.
    target: ID3D11Texture2D,
    /// Target luma width.
    width: u32,
    /// Target luma height.
    height: u32,
}

impl Gray {
    /// Grayscale shader plus a probe target; the target follows the first surface.
    fn open(device: &D3d11Device) -> Result<Self, String> {
        let target = nv12_target(device, 64, 64, true)?;
        plane_uavs(device, &target)?;
        let mut shader = None;
        // Sound: the bytecode slice stays live for the call.
        unsafe {
            device.device().CreateComputeShader(
                GRAY_CSO,
                None,
                Some(std::ptr::addr_of_mut!(shader)),
            )
        }
        .map_err(|error| format!("CreateComputeShader: {error}"))?;
        let shader = shader.ok_or_else(|| "no compute shader".to_owned())?;
        Ok(Self {
            shader,
            target,
            width: 64,
            height: 64,
        })
    }

    /// Grayscaled texture plus extent for `surface`.
    fn apply(
        &mut self,
        device: &D3d11Device,
        surface: &GpuSurface,
    ) -> Result<(ID3D11Texture2D, u32, u32), String> {
        let size = surface.size();
        let (width, height) = (size.width(), size.height());
        if (width, height) != (self.width, self.height) {
            self.target = nv12_target(device, width, height, true)?;
            self.width = width;
            self.height = height;
        }
        let source = plane_srv(device, surface.texture(), surface.slice())?;
        let (uav_y, uav_uv) = plane_uavs(device, &self.target)?;
        let origin = origin_cb(device, surface.origin())?;
        let context = device.context();
        let _submit = device.submit_lock().enter().map_err(err)?;
        // Sound: every object stays live through the dispatch.
        let uavs = [Some(uav_y), Some(uav_uv)];
        let no_uavs: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
        unsafe {
            context.CSSetShader(Some(&self.shader), None);
            context.CSSetShaderResources(0, Some(&[Some(source)]));
            context.CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
            context.CSSetConstantBuffers(0, Some(&[Some(origin)]));
            context.Dispatch(width.div_ceil(16), height.div_ceil(16), 1);
            context.CSSetUnorderedAccessViews(0, 2, Some(no_uavs.as_ptr()), None);
            context.CSSetShaderResources(0, Some(&[None]));
            context.CSSetConstantBuffers(0, Some(&[None]));
            let shader: Option<&ID3D11ComputeShader> = None;
            context.CSSetShader(shader, None);
        }
        Ok((self.target.clone(), width, height))
    }
}

/// Video-process filter over one source and target extent.
pub(crate) struct Vproc {
    /// Processor, views, and target behind the blt.
    state: VprocState,
    /// Operation behind the rects and colorspaces.
    op: Op,
    /// Source extent.
    from: (u32, u32),
    /// Target extent.
    to: (u32, u32),
}

impl Vproc {
    /// Video-process filter for `op`, geometry fixed on first use.
    fn open(device: &D3d11Device, op: Op) -> Result<Self, String> {
        Ok(Self {
            state: vproc_state(device, (128, 128), (64, 64))?,
            op,
            from: (128, 128),
            to: (64, 64),
        })
    }

    /// Filtered texture plus extent for `surface`.
    fn apply(
        &mut self,
        device: &D3d11Device,
        surface: &GpuSurface,
    ) -> Result<(ID3D11Texture2D, u32, u32), String> {
        let size = surface.size();
        let (width, height) = (size.width(), size.height());
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err("odd source extent".to_owned());
        }
        let to = (width / 2, height / 2);
        if (width, height) != self.from || to != self.to {
            self.state = vproc_state(device, (width, height), to)?;
            self.from = (width, height);
            self.to = to;
        }
        blt(device, self, surface)?;
        Ok((self.state.target.clone(), to.0, to.1))
    }
}

/// Processor, views, and target behind one geometry.
pub(crate) struct VprocState {
    /// Video processor behind the blt.
    processor: ID3D11VideoProcessor,
    /// Enumerator behind the views.
    enumerator: ID3D11VideoProcessorEnumerator,
    /// Output view over the target.
    output_view: ID3D11VideoProcessorOutputView,
    /// NV12 target texture.
    target: ID3D11Texture2D,
}

/// One video-process blt of `surface` through `vproc`.
fn blt(device: &D3d11Device, vproc: &Vproc, surface: &GpuSurface) -> Result<(), String> {
    let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV {
                MipSlice: 0,
                ArraySlice: surface.slice(),
            },
        },
    };
    let mut view = None;
    // Sound: the texture and descriptor stay live for the call.
    unsafe {
        device.video_device().CreateVideoProcessorInputView(
            surface.texture(),
            &vproc.state.enumerator,
            std::ptr::addr_of!(desc),
            Some(std::ptr::addr_of_mut!(view)),
        )
    }
    .map_err(|error| format!("CreateVideoProcessorInputView: {error}"))?;
    let view: ID3D11VideoProcessorInputView = view.ok_or_else(|| "no input view".to_owned())?;
    let (left, top) = surface.origin();
    let size = surface.size();
    let source = match vproc.op {
        Op::Crop => crop_rect(left, top, size.width(), size.height())?,
        _ => full_rect(left, top, size.width(), size.height())?,
    };
    let target = rect(0, 0, vproc.to.0, vproc.to.1)?;
    let _submit = device.submit_lock().enter().map_err(err)?;
    // Sound: the processor and rects stay live for each call.
    unsafe {
        device.video_context().VideoProcessorSetStreamSourceRect(
            &vproc.state.processor,
            0,
            true,
            Some(std::ptr::from_ref(&source)),
        );
        device.video_context().VideoProcessorSetStreamDestRect(
            &vproc.state.processor,
            0,
            true,
            Some(std::ptr::from_ref(&target)),
        );
        let output_nominal = if vproc.op == Op::Color { 2 } else { 1 };
        let input = color_space(1, 1);
        let output = color_space(1, output_nominal);
        device.video_context().VideoProcessorSetStreamColorSpace(
            &vproc.state.processor,
            0,
            std::ptr::addr_of!(input),
        );
        device
            .video_context()
            .VideoProcessorSetOutputColorSpace(&vproc.state.processor, std::ptr::addr_of!(output));
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(view)),
            ..Default::default()
        };
        device
            .video_context()
            .VideoProcessorBlt(
                &vproc.state.processor,
                &vproc.state.output_view,
                0,
                std::slice::from_ref(&stream),
            )
            .map_err(|error| format!("VideoProcessorBlt: {error}"))?;
    }
    Ok(())
}

/// Color space: BT.709 `matrix` 1, `nominal` 1 studio else 2 full.
///
/// Bit layout per `D3D11_VIDEO_PROCESSOR_COLOR_SPACE`: bit 0 usage,
/// bit 1 RGB range, bit 2 YCbCr matrix, bit 3 xvYCC, bits 4-5 range.
fn color_space(matrix: u32, nominal: u32) -> D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
        _bitfield: (matrix << 2) | (nominal << 4),
    }
}

/// Center quarter rectangle inside the visible frame.
fn crop_rect(left: u32, top: u32, width: u32, height: u32) -> Result<RECT, String> {
    rect(left + width / 4, top + height / 4, width / 2, height / 2)
}

/// Display string of a tier error.
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Dump extension for `codec`.
fn ext(codec: TierCodec) -> &'static str {
    match codec {
        TierCodec::H264 => "h264",
        TierCodec::Hevc => "h265",
        TierCodec::Vp9 => "ivf",
        TierCodec::Av1 => "obu",
        _ => "bin",
    }
}

/// Flag word for bind and misc flag constants.
fn flag_word(bits: i32) -> u32 {
    u32::try_from(bits).unwrap_or(0)
}

/// Visible rectangle at (`left`, `top`) sized `width` by `height`.
fn full_rect(left: u32, top: u32, width: u32, height: u32) -> Result<RECT, String> {
    rect(left, top, width, height)
}

/// NV12 target texture, UAV-capable for compute.
fn nv12_target(
    device: &D3d11Device,
    width: u32,
    height: u32,
    uav: bool,
) -> Result<ID3D11Texture2D, String> {
    let bind = if uav {
        D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0
    } else {
        D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0
    };
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: flag_word(bind),
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
    .map_err(|error| format!("CreateTexture2D: {error}"))?;
    target.ok_or_else(|| "no target texture".to_owned())
}

/// Constant buffer carrying the decode crop `origin`.
fn origin_cb(device: &D3d11Device, origin: (u32, u32)) -> Result<ID3D11Buffer, String> {
    let data = [origin.0, origin.1, 0, 0];
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: 16,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: flag_word(D3D11_BIND_CONSTANT_BUFFER.0),
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr().cast::<std::ffi::c_void>(),
        SysMemPitch: 16,
        SysMemSlicePitch: 16,
    };
    let mut buffer = None;
    // Sound: the descriptor and bytes stay live for the call.
    unsafe {
        device.device().CreateBuffer(
            std::ptr::addr_of!(desc),
            Some(std::ptr::addr_of!(init)),
            Some(std::ptr::addr_of_mut!(buffer)),
        )
    }
    .map_err(|error| format!("CreateBuffer: {error}"))?;
    buffer.ok_or_else(|| "no origin buffer".to_owned())
}

/// Luma SRV over `slice` of `texture`.
fn plane_srv(
    device: &D3d11Device,
    texture: &ID3D11Texture2D,
    slice: u32,
) -> Result<ID3D11ShaderResourceView, String> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8_UNORM,
        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                FirstArraySlice: slice,
                ArraySize: 1,
            },
        },
    };
    let mut view = None;
    // Sound: the texture and descriptor stay live for the call.
    unsafe {
        device.device().CreateShaderResourceView(
            texture,
            Some(std::ptr::addr_of!(desc)),
            Some(std::ptr::addr_of_mut!(view)),
        )
    }
    .map_err(|error| format!("CreateShaderResourceView: {error}"))?;
    view.ok_or_else(|| "no luma srv".to_owned())
}

/// Luma and chroma UAVs over `target`.
fn plane_uavs(
    device: &D3d11Device,
    target: &ID3D11Texture2D,
) -> Result<(ID3D11UnorderedAccessView, ID3D11UnorderedAccessView), String> {
    let mut y = None;
    let mut uv = None;
    // Sound: the texture and descriptors stay live for each call.
    unsafe {
        let desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
            },
        };
        device
            .device()
            .CreateUnorderedAccessView(
                target,
                Some(std::ptr::addr_of!(desc)),
                Some(std::ptr::addr_of_mut!(y)),
            )
            .map_err(|error| format!("luma uav: {error}"))?;
        let desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
            },
        };
        device
            .device()
            .CreateUnorderedAccessView(
                target,
                Some(std::ptr::addr_of!(desc)),
                Some(std::ptr::addr_of_mut!(uv)),
            )
            .map_err(|error| format!("chroma uav: {error}"))?;
    }
    match (y, uv) {
        (Some(y), Some(uv)) => Ok((y, uv)),
        _ => Err("no plane uavs".to_owned()),
    }
}

/// Rectangle at (`left`, `top`) with `width` by `height`.
fn rect(left: u32, top: u32, width: u32, height: u32) -> Result<RECT, String> {
    let left = i32::try_from(left).map_err(|_| "extent out of range".to_owned())?;
    let top = i32::try_from(top).map_err(|_| "extent out of range".to_owned())?;
    let right = left
        .checked_add(i32::try_from(width).unwrap_or(i32::MAX))
        .ok_or_else(|| "extent out of range".to_owned())?;
    let bottom = top
        .checked_add(i32::try_from(height).unwrap_or(i32::MAX))
        .ok_or_else(|| "extent out of range".to_owned())?;
    Ok(RECT {
        left,
        top,
        right,
        bottom,
    })
}

/// Video processor, enumerator, output view, and target for `from` to `to`.
fn vproc_state(
    device: &D3d11Device,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<VprocState, String> {
    let rate = DXGI_RATIONAL {
        Numerator: 30,
        Denominator: 1,
    };
    let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: rate,
        InputWidth: from.0,
        InputHeight: from.1,
        OutputFrameRate: rate,
        OutputWidth: to.0,
        OutputHeight: to.1,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    // Sound: the descriptor stays live for the call.
    let enumerator = unsafe {
        device
            .video_device()
            .CreateVideoProcessorEnumerator(std::ptr::addr_of!(desc))
    }
    .map_err(|error| format!("CreateVideoProcessorEnumerator: {error}"))?;
    // Sound: the format is a plain input value.
    let flags = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
        .map_err(|error| format!("CheckVideoProcessorFormat: {error}"))?;
    let need = flag_word(
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0
            | D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0,
    );
    if flags & need != need {
        return Err("video processor without nv12 scale".to_owned());
    }
    // Sound: the enumerator is live.
    let processor = unsafe { device.video_device().CreateVideoProcessor(&enumerator, 0) }
        .map_err(|error| format!("CreateVideoProcessor: {error}"))?;
    let target = nv12_target(device, to.0, to.1, false)?;
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
    .map_err(|error| format!("CreateVideoProcessorOutputView: {error}"))?;
    let output_view = output_view.ok_or_else(|| "no output view".to_owned())?;
    Ok(VprocState {
        processor,
        enumerator,
        output_view,
        target,
    })
}
