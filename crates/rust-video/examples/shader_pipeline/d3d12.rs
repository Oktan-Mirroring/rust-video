//! D3D12 pipeline: decode texture, video-process or compute filter, video encode.

// FFI boundary: video-process and compute COM calls live here.
#![allow(unsafe_code)]

use std::mem::ManuallyDrop;

use crate::content::{Codec, Op};
use crate::driver::surface::{DecodeSurface, DrainEncoder, SurfacePipeline};

use rust_video::Encoder;
use rust_video::Packet;
use rust_video::PictureSize;
use rust_video_d3d12::{
    Av1Decoder, Codec as TierCodec, D3d12Device, D3d12Encoder, GpuSurface, H264Decoder, HevcDecoder,
};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::{
    D3D12SerializeRootSignature, ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue,
    ID3D12DescriptorHeap, ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList,
    ID3D12PipelineState, ID3D12Resource, ID3D12RootSignature, D3D12_CACHED_PIPELINE_STATE,
    D3D12_COMMAND_LIST_TYPE, D3D12_COMMAND_LIST_TYPE_COMPUTE,
    D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS, D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE,
    D3D12_COMPUTE_PIPELINE_STATE_DESC, D3D12_CPU_DESCRIPTOR_HANDLE,
    D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
    D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
    D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, D3D12_DESCRIPTOR_RANGE,
    D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND, D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
    D3D12_DESCRIPTOR_RANGE_TYPE_UAV, D3D12_FENCE_FLAG_NONE, D3D12_GPU_DESCRIPTOR_HANDLE,
    D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
    D3D12_MEMORY_POOL_UNKNOWN, D3D12_PIPELINE_STATE_FLAG_NONE, D3D12_RESOURCE_BARRIER,
    D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
    D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
    D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ, D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
    D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_ROOT_CONSTANTS, D3D12_ROOT_DESCRIPTOR_TABLE,
    D3D12_ROOT_PARAMETER, D3D12_ROOT_PARAMETER_0, D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE, D3D12_ROOT_SIGNATURE_DESC,
    D3D12_ROOT_SIGNATURE_FLAG_NONE, D3D12_SHADER_BYTECODE, D3D12_SHADER_RESOURCE_VIEW_DESC,
    D3D12_SHADER_RESOURCE_VIEW_DESC_0, D3D12_SHADER_VISIBILITY_ALL, D3D12_SRV_DIMENSION_TEXTURE2D,
    D3D12_TEX2D_SRV, D3D12_TEX2D_UAV, D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12_UAV_DIMENSION_TEXTURE2D,
    D3D12_UNORDERED_ACCESS_VIEW_DESC, D3D12_UNORDERED_ACCESS_VIEW_DESC_0,
    D3D_ROOT_SIGNATURE_VERSION_1,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8_UNORM,
    DXGI_FORMAT_R8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Media::MediaFoundation::{
    ID3D12VideoDevice, ID3D12VideoProcessCommandList, ID3D12VideoProcessor,
    D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT, D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
    D3D12_VIDEO_FIELD_TYPE_NONE, D3D12_VIDEO_FORMAT, D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
    D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE, D3D12_VIDEO_PROCESS_DEINTERLACE_FLAG_NONE,
    D3D12_VIDEO_PROCESS_FILTER_FLAG_NONE, D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC, D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE,
    D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT, D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC, D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED,
    D3D12_VIDEO_SAMPLE, D3D12_VIDEO_SCALE_SUPPORT_FLAG_EVEN_DIMENSIONS_ONLY,
    D3D12_VIDEO_SCALE_SUPPORT_FLAG_POW2_ONLY, D3D12_VIDEO_SIZE_RANGE,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Grayscale compute bytecode, compiled from `shaders/gray_cs.hlsl` (`cs_5_0`).
const GRAY_CSO: &[u8] = include_bytes!("shaders/gray_cs.cso");

/// Fence wait budget in milliseconds.
const WAIT_MS: u32 = 5_000;

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
    /// AV1 decoder.
    Av1(Box<Av1Decoder<'a>>),
}

impl Filter {
    /// Filter for `op`, geometry fixed on first use.
    fn open(device: &D3d12Device, op: Op) -> Result<Self, String> {
        match op {
            Op::Gray => Ok(Self::Gray(Gray::open(device)?)),
            Op::Down | Op::Color | Op::Crop => Ok(Self::Vproc(Vproc::open(device, op)?)),
        }
    }

    /// Filtered surface for `surface`.
    fn apply(&mut self, device: &D3d12Device, surface: &GpuSurface) -> Result<GpuSurface, String> {
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
            Self::Av1(decoder) => decoder.decode_surface(packet).map_err(err),
        }
    }

    fn flush(&mut self) -> Result<Vec<GpuSurface>, String> {
        match self {
            Self::H264(decoder) => decoder.flush_surface().map_err(err),
            Self::Hevc(decoder) => decoder.flush_surface().map_err(err),
            Self::Av1(decoder) => decoder.flush_surface().map_err(err),
        }
    }
}

impl DrainEncoder for D3d12Encoder<'_> {
    fn drain(&mut self) -> Result<Vec<Packet>, String> {
        self.flush().map_err(err)
    }
}

/// D3D12 backend over the default hardware adapter.
pub struct Backend {
    /// Device behind decoders, filters, and encoders.
    device: D3d12Device,
}

impl Backend {
    /// Backend over the default hardware adapter.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a hardware device.
    pub fn open() -> Result<Self, String> {
        D3d12Device::open()
            .map(|device| Self { device })
            .map_err(err)
    }
}

impl SurfacePipeline for Backend {
    type Surface = GpuSurface;
    type Source<'s> = Source<'s>;
    type Filter<'s> = Filter;
    type Encoder<'s> = D3d12Encoder<'s>;

    fn name(&self) -> &'static str {
        "d3d12"
    }

    fn source(&self, codec: Codec) -> Result<Source<'_>, String> {
        match codec {
            Codec::H264 => Ok(Source::H264(Box::new(H264Decoder::open(&self.device)))),
            Codec::H265 => Ok(Source::Hevc(Box::new(HevcDecoder::open(&self.device)))),
            Codec::Av1 => Ok(Source::Av1(Box::new(Av1Decoder::open(&self.device)))),
            Codec::Vp9 => Err("tier without vp9 decode".to_owned()),
        }
    }

    fn filter(&self, op: Op) -> Result<Filter, String> {
        Filter::open(&self.device, op)
    }

    fn encoder(&self, codec: Codec) -> Result<D3d12Encoder<'_>, String> {
        match codec {
            Codec::H264 | Codec::Av1 => {
                D3d12Encoder::open(&self.device, TierCodec::H264).map_err(err)
            }
            Codec::H265 => D3d12Encoder::open(&self.device, TierCodec::Hevc)
                .or_else(|_| D3d12Encoder::open(&self.device, TierCodec::H264))
                .map_err(err),
            Codec::Vp9 => Err("tier without vp9 decode".to_owned()),
        }
    }

    fn note(codec: Codec, encoder: &D3d12Encoder) -> String {
        let target = match encoder.codec() {
            TierCodec::H264 => "h264",
            TierCodec::Hevc => "hevc",
            _ => "?",
        };
        format!("{}->{target} via d3d12 video encode", codec.name())
    }

    fn ext(encoder: &D3d12Encoder) -> &'static str {
        ext(encoder.codec())
    }

    fn encode_surface(
        &self,
        filter: &mut Filter,
        encoder: &mut D3d12Encoder,
        surface: &GpuSurface,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        surface.wait().map_err(err)?;
        let filtered = filter.apply(&self.device, surface)?;
        for packet in encoder.encode_surface(&filtered).map_err(err)? {
            out.extend_from_slice(packet.data());
        }
        Ok(())
    }
}

/// Compute grayscale over one extent.
pub(crate) struct Gray {
    /// Compute queue behind the dispatch.
    queue: ID3D12CommandQueue,
    /// Fence behind the queue.
    fence: ID3D12Fence,
    /// Completion event.
    event: HANDLE,
    /// Next fence value.
    ticket: u64,
    /// Compute pipeline state.
    pso: ID3D12PipelineState,
    /// Root signature behind the pipeline.
    root: ID3D12RootSignature,
    /// Descriptor heap: luma SRV, luma UAV, chroma UAV.
    heap: ID3D12DescriptorHeap,
    /// Command allocator behind the list.
    allocator: ID3D12CommandAllocator,
    /// Compute command list.
    list: ID3D12GraphicsCommandList,
    /// NV12 target texture.
    target: ID3D12Resource,
    /// Target extent.
    size: (u32, u32),
}

impl Gray {
    /// Grayscale pipeline plus a probe target; the target follows the first surface.
    fn open(device: &D3d12Device) -> Result<Self, String> {
        let queue = compute_queue(device.device())?;
        let fence = fence(device.device())?;
        let event = event()?;
        let (root, pso) = pipeline(device.device())?;
        let heap = heap(device.device())?;
        let (allocator, list) = compute_list(device.device())?;
        let target = gray_target(device.device(), 64, 64)?;
        gray_views(device.device(), &heap, &target);
        Ok(Self {
            queue,
            fence,
            event,
            ticket: 1,
            pso,
            root,
            heap,
            allocator,
            list,
            target,
            size: (64, 64),
        })
    }

    /// Grayscaled surface for `surface`.
    fn apply(&mut self, device: &D3d12Device, surface: &GpuSurface) -> Result<GpuSurface, String> {
        let size = surface.size();
        let (width, height) = (size.width(), size.height());
        if (width, height) != self.size {
            self.target = gray_target(device.device(), width, height)?;
            gray_views(device.device(), &self.heap, &self.target);
            self.size = (width, height);
        }
        gray_srv(device.device(), &self.heap, surface.texture());
        dispatch(self, device.device(), surface)?;
        let size = PictureSize::new(width, height).ok_or_else(|| "zero extent".to_owned())?;
        Ok(GpuSurface::wrap(self.target.clone(), size))
    }
}

impl Drop for Gray {
    /// Closes the completion event.
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.event) };
    }
}

/// Video-process filter over one source and target extent.
pub(crate) struct Vproc {
    /// Processor, queue, list, fence, and target behind the blt.
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
    fn open(device: &D3d12Device, op: Op) -> Result<Self, String> {
        Ok(Self {
            state: vproc_state(device, op, (128, 128), (64, 64))?,
            op,
            from: (128, 128),
            to: (64, 64),
        })
    }

    /// Filtered surface for `surface`.
    fn apply(&mut self, device: &D3d12Device, surface: &GpuSurface) -> Result<GpuSurface, String> {
        let size = surface.size();
        let (width, height) = (size.width(), size.height());
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err("odd source extent".to_owned());
        }
        let to = (width / 2, height / 2);
        if (width, height) != self.from || to != self.to {
            self.state = vproc_state(device, self.op, (width, height), to)?;
            self.from = (width, height);
            self.to = to;
        }
        blt(self, surface)?;
        let size = PictureSize::new(to.0, to.1).ok_or_else(|| "zero extent".to_owned())?;
        Ok(GpuSurface::wrap(self.state.target.clone(), size))
    }
}

/// Processor, queue, list, fence, and target behind one geometry.
pub(crate) struct VprocState {
    /// Video processor behind the blt.
    processor: ID3D12VideoProcessor,
    /// Video process queue.
    queue: ID3D12CommandQueue,
    /// Command allocator behind the list.
    allocator: ID3D12CommandAllocator,
    /// Video process command list.
    list: ID3D12VideoProcessCommandList,
    /// Fence behind the queue.
    fence: ID3D12Fence,
    /// Completion event.
    event: HANDLE,
    /// Next fence value.
    ticket: u64,
    /// NV12 target texture.
    target: ID3D12Resource,
}

impl Drop for VprocState {
    /// Closes the completion event.
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.event) };
    }
}

/// Transition barrier for `resource` between states.
fn barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

/// One video-process blt of `surface` through `vproc`.
fn blt(vproc: &mut Vproc, surface: &GpuSurface) -> Result<(), String> {
    // Sound: the slot is idle past the fence wait below.
    unsafe {
        vproc
            .state
            .allocator
            .Reset()
            .map_err(|error| format!("allocator reset: {error}"))?;
        vproc
            .state
            .list
            .Reset(&vproc.state.allocator)
            .map_err(|error| format!("list reset: {error}"))?;
    }
    let source = surface.texture();
    // Sound: the barriers name live textures; the list is open.
    unsafe {
        vproc.state.list.ResourceBarrier(&[
            barrier(
                source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
            ),
            barrier(
                &vproc.state.target,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
            ),
        ]);
    }
    let (left, top) = surface.origin();
    let size = surface.size();
    let (source_rect, dest_rect) = match vproc.op {
        Op::Crop => (
            rect(
                left + size.width() / 4,
                top + size.height() / 4,
                vproc.to.0,
                vproc.to.1,
            )?,
            rect(0, 0, vproc.to.0, vproc.to.1)?,
        ),
        _ => (
            rect(left, top, size.width(), size.height())?,
            rect(0, 0, vproc.to.0, vproc.to.1)?,
        ),
    };
    let mut input = D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS::default();
    input.InputStream[0].pTexture2D = ManuallyDrop::new(Some(source.clone()));
    input.Transform.SourceRectangle = source_rect;
    input.Transform.DestinationRectangle = dest_rect;
    input.Transform.Orientation = D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT;
    input.Flags = D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE;
    let mut output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS::default();
    output.OutputStream[0].pTexture2D = ManuallyDrop::new(Some(vproc.state.target.clone()));
    output.TargetRectangle = rect(0, 0, vproc.to.0, vproc.to.1)?;
    // Sound: the argument structs outlive the fence wait below.
    unsafe {
        vproc.state.list.ProcessFrames(
            &vproc.state.processor,
            std::ptr::from_ref(&output),
            std::slice::from_ref(&input),
        );
        vproc.state.list.ResourceBarrier(&[
            barrier(
                source,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
                D3D12_RESOURCE_STATE_COMMON,
            ),
            barrier(
                &vproc.state.target,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            ),
        ]);
        vproc
            .state
            .list
            .Close()
            .map_err(|error| format!("list close: {error}"))?;
    }
    // Sound: the list closed above; the queue and fence are live.
    unsafe {
        let list: ID3D12CommandList = vproc
            .state
            .list
            .cast()
            .map_err(|error| format!("list cast: {error}"))?;
        vproc.state.queue.ExecuteCommandLists(&[Some(list)]);
        let ticket = vproc.state.ticket;
        vproc.state.ticket = ticket.wrapping_add(1);
        vproc
            .state
            .queue
            .Signal(&vproc.state.fence, ticket)
            .map_err(|error| format!("queue signal: {error}"))?;
        wait(&vproc.state.fence, ticket, vproc.state.event)?;
    }
    Ok(())
}

/// Committed resource of `desc` on the default heap in `state`.
fn committed(
    device: &ID3D12Device,
    desc: &D3D12_RESOURCE_DESC,
    state: D3D12_RESOURCE_STATES,
    call: &'static str,
) -> Result<ID3D12Resource, String> {
    let props = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 1,
        VisibleNodeMask: 1,
    };
    let mut out: Option<ID3D12Resource> = None;
    // Sound: `props` and `desc` outlive the call; `out` receives the resource.
    unsafe {
        device.CreateCommittedResource(
            std::ptr::addr_of!(props),
            D3D12_HEAP_FLAG_NONE,
            std::ptr::from_ref(desc),
            state,
            None,
            std::ptr::addr_of_mut!(out),
        )
    }
    .map_err(|error| format!("{call}: {error}"))?;
    out.ok_or_else(|| format!("{call} returned null"))
}

/// Allocator and compute list, closed fresh.
fn compute_list(
    device: &ID3D12Device,
) -> Result<(ID3D12CommandAllocator, ID3D12GraphicsCommandList), String> {
    // Sound: no inputs beyond the list type.
    let allocator: ID3D12CommandAllocator =
        unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE) }
            .map_err(|error| format!("CreateCommandAllocator: {error}"))?;
    // Sound: the allocator outlives the call; no initial pipeline.
    let list: ID3D12GraphicsCommandList =
        unsafe { device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COMPUTE, &allocator, None) }
            .map_err(|error| format!("CreateCommandList: {error}"))?;
    // Sound: the fresh list is open with no recorded work.
    unsafe { list.Close() }.map_err(|error| format!("fresh list close: {error}"))?;
    Ok((allocator, list))
}

/// Compute queue over `device`.
fn compute_queue(device: &ID3D12Device) -> Result<ID3D12CommandQueue, String> {
    queue(device, D3D12_COMMAND_LIST_TYPE_COMPUTE)
}

/// Queue of `queue_type` behind submissions.
fn queue(
    device: &ID3D12Device,
    queue_type: D3D12_COMMAND_LIST_TYPE,
) -> Result<ID3D12CommandQueue, String> {
    let desc = D3D12_COMMAND_QUEUE_DESC {
        Type: queue_type,
        Priority: 0,
        Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
        NodeMask: 0,
    };
    // Sound: the descriptor outlives the call.
    unsafe { device.CreateCommandQueue(std::ptr::addr_of!(desc)) }
        .map_err(|error| format!("CreateCommandQueue: {error}"))
}

/// Grayscale dispatch over `surface` into the target.
fn dispatch(gray: &mut Gray, device: &ID3D12Device, surface: &GpuSurface) -> Result<(), String> {
    // Sound: the slot is idle past the fence wait below.
    unsafe {
        gray.allocator
            .Reset()
            .map_err(|error| format!("allocator reset: {error}"))?;
        gray.list
            .Reset(&gray.allocator, None)
            .map_err(|error| format!("list reset: {error}"))?;
    }
    let source = surface.texture();
    let (width, height) = gray.size;
    let origin = [surface.origin().0, surface.origin().1];
    // Sound: the barriers name live textures; the list is open.
    unsafe {
        gray.list.ResourceBarrier(&[
            barrier(
                source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            ),
            barrier(
                &gray.target,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            ),
        ]);
        gray.list.SetPipelineState(&gray.pso);
        gray.list.SetComputeRootSignature(&gray.root);
        gray.list.SetDescriptorHeaps(&[Some(gray.heap.clone())]);
        gray.list
            .SetComputeRootDescriptorTable(0, gpu_slot(device, &gray.heap, 0));
        gray.list
            .SetComputeRootDescriptorTable(1, gpu_slot(device, &gray.heap, 1));
        gray.list
            .SetComputeRoot32BitConstants(2, 2, origin.as_ptr().cast::<std::ffi::c_void>(), 0);
        gray.list
            .Dispatch(width.div_ceil(16), height.div_ceil(16), 1);
        gray.list.ResourceBarrier(&[
            barrier(
                source,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            ),
            barrier(
                &gray.target,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COMMON,
            ),
        ]);
        gray.list
            .Close()
            .map_err(|error| format!("list close: {error}"))?;
    }
    // Sound: the list closed above; the queue and fence are live.
    unsafe {
        let list: ID3D12CommandList = gray
            .list
            .cast()
            .map_err(|error| format!("list cast: {error}"))?;
        gray.queue.ExecuteCommandLists(&[Some(list)]);
        let ticket = gray.ticket;
        gray.ticket = ticket.wrapping_add(1);
        gray.queue
            .Signal(&gray.fence, ticket)
            .map_err(|error| format!("queue signal: {error}"))?;
        wait(&gray.fence, ticket, gray.event)?;
    }
    Ok(())
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
        _ => "bin",
    }
}

/// Completion event.
fn event() -> Result<HANDLE, String> {
    // Sound: default security, auto-reset, unnamed.
    unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .map_err(|error| format!("CreateEvent: {error}"))
}

/// Fence over `device`.
fn fence(device: &ID3D12Device) -> Result<ID3D12Fence, String> {
    // Sound: no inputs beyond the initial value and flags.
    unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
        .map_err(|error| format!("CreateFence: {error}"))
}

/// UAV-capable NV12 grayscale target.
fn gray_target(device: &ID3D12Device, width: u32, height: u32) -> Result<ID3D12Resource, String> {
    let mut desc = texture_desc(width, height);
    desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    committed(
        device,
        &desc,
        D3D12_RESOURCE_STATE_COMMON,
        "CreateGrayTarget",
    )
}

/// Luma SRV over the decode texture in heap slot 0.
fn gray_srv(device: &ID3D12Device, heap: &ID3D12DescriptorHeap, texture: &ID3D12Resource) {
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8_UNORM,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                PlaneSlice: 0,
                ResourceMinLODClamp: 0.0,
            },
        },
    };
    // Sound: the texture and descriptor stay live for the call.
    unsafe {
        device.CreateShaderResourceView(
            texture,
            Some(std::ptr::addr_of!(desc)),
            cpu_slot(device, heap, 0),
        );
    }
}

/// Luma and chroma UAVs over the target in heap slots 1 and 2.
fn gray_views(device: &ID3D12Device, heap: &ID3D12DescriptorHeap, target: &ID3D12Resource) {
    for (slot, format, plane) in [(1, DXGI_FORMAT_R8_UNORM, 0), (2, DXGI_FORMAT_R8G8_UNORM, 1)] {
        let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_UAV {
                    MipSlice: 0,
                    PlaneSlice: plane,
                },
            },
        };
        // Sound: the texture and descriptor stay live for the call.
        unsafe {
            device.CreateUnorderedAccessView(
                target,
                None,
                Some(std::ptr::addr_of!(desc)),
                cpu_slot(device, heap, slot),
            );
        }
    }
}

/// CPU handle for heap `slot`.
fn cpu_slot(
    device: &ID3D12Device,
    heap: &ID3D12DescriptorHeap,
    slot: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let inc = usize::try_from(device_increment(device)).unwrap_or(0);
    let offset = usize::try_from(slot).unwrap_or(0).saturating_mul(inc);
    // Sound: the heap is live with three descriptors.
    let start = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    D3D12_CPU_DESCRIPTOR_HANDLE {
        ptr: start.ptr.saturating_add(offset),
    }
}

/// GPU handle for heap `slot`.
fn gpu_slot(
    device: &ID3D12Device,
    heap: &ID3D12DescriptorHeap,
    slot: u32,
) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let inc = u64::from(device_increment(device));
    let offset = u64::from(slot).saturating_mul(inc);
    // Sound: the heap is live with three descriptors.
    let start = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    D3D12_GPU_DESCRIPTOR_HANDLE {
        ptr: start.ptr.saturating_add(offset),
    }
}

/// Descriptor increment for the SRV/UAV heap type.
fn device_increment(device: &ID3D12Device) -> u32 {
    // Sound: the heap type is a plain input value.
    unsafe { device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) }
}

/// Shader-visible descriptor heap for one SRV and two UAVs.
fn heap(device: &ID3D12Device) -> Result<ID3D12DescriptorHeap, String> {
    let desc = D3D12_DESCRIPTOR_HEAP_DESC {
        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
        NumDescriptors: 3,
        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
        NodeMask: 0,
    };
    // Sound: the descriptor outlives the call.
    unsafe { device.CreateDescriptorHeap(std::ptr::addr_of!(desc)) }
        .map_err(|error| format!("CreateDescriptorHeap: {error}"))
}

/// Root signature and grayscale pipeline state.
fn pipeline(device: &ID3D12Device) -> Result<(ID3D12RootSignature, ID3D12PipelineState), String> {
    let srv_range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
        NumDescriptors: 1,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
    };
    let uav_range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
        NumDescriptors: 2,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
    };
    let params = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: std::ptr::addr_of!(srv_range),
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: std::ptr::addr_of!(uav_range),
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                    Num32BitValues: 2,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
    ];
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: 3,
        pParameters: params.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    let mut blob: Option<ID3DBlob> = None;
    let mut error_blob: Option<ID3DBlob> = None;
    // Sound: the descriptor and ranges outlive the call.
    unsafe {
        D3D12SerializeRootSignature(
            std::ptr::addr_of!(desc),
            D3D_ROOT_SIGNATURE_VERSION_1,
            std::ptr::addr_of_mut!(blob),
            Some(std::ptr::addr_of_mut!(error_blob)),
        )
    }
    .map_err(|error| format!("D3D12SerializeRootSignature: {error}"))?;
    let blob = blob.ok_or_else(|| "no root signature blob".to_owned())?;
    // Sound: the blob is live; the slice spans its bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
    };
    // Sound: the bytes are the serialized signature.
    let root: ID3D12RootSignature = unsafe { device.CreateRootSignature(0, bytes) }
        .map_err(|error| format!("CreateRootSignature: {error}"))?;
    let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
        pRootSignature: ManuallyDrop::new(Some(root.clone())),
        CS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: GRAY_CSO.as_ptr().cast::<std::ffi::c_void>(),
            BytecodeLength: GRAY_CSO.len(),
        },
        NodeMask: 0,
        CachedPSO: D3D12_CACHED_PIPELINE_STATE::default(),
        Flags: D3D12_PIPELINE_STATE_FLAG_NONE,
    };
    // Sound: the descriptor and bytecode outlive the call.
    let pso: ID3D12PipelineState =
        unsafe { device.CreateComputePipelineState(std::ptr::addr_of!(desc)) }
            .map_err(|error| format!("CreateComputePipelineState: {error}"))?;
    Ok((root, pso))
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

/// NV12 texture descriptor of `width` by `height`.
fn texture_desc(width: u32, height: u32) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(width),
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    }
}

/// Colorspace pair for `op`: studio in, full out on color, studio both else.
fn colorspaces(op: Op) -> (DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_TYPE) {
    if op == Op::Color {
        (
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
        )
    } else {
        (
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        )
    }
}

/// Processor, queue, list, fence, and target for `op` from `from` to `to`.
fn vproc_state(
    device: &D3d12Device,
    op: Op,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<VprocState, String> {
    let (input_cs, output_cs) = colorspaces(op);
    check_support(device.video(), input_cs, output_cs, from, to)?;
    let processor = create_processor(device.video(), input_cs, output_cs, op, from, to)?;
    let queue = queue(device.device(), D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)?;
    // Sound: no inputs beyond the list type.
    let allocator: ID3D12CommandAllocator = unsafe {
        device
            .device()
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)
    }
    .map_err(|error| format!("CreateCommandAllocator: {error}"))?;
    // Sound: the allocator outlives the call; no initial pipeline.
    let list: ID3D12VideoProcessCommandList = unsafe {
        device.device().CreateCommandList(
            0,
            D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
            &allocator,
            None,
        )
    }
    .map_err(|error| format!("CreateCommandList: {error}"))?;
    // Sound: the fresh list is open with no recorded work.
    unsafe { list.Close() }.map_err(|error| format!("fresh list close: {error}"))?;
    let fence = fence(device.device())?;
    let event = event()?;
    let desc = texture_desc(to.0, to.1);
    let target = committed(
        device.device(),
        &desc,
        D3D12_RESOURCE_STATE_COMMON,
        "CreateVprocTarget",
    )?;
    Ok(VprocState {
        processor,
        queue,
        allocator,
        list,
        fence,
        event,
        ticket: 1,
        target,
    })
}

/// Video processor support for `from` to `to` in the colorspace pair.
fn check_support(
    video: &ID3D12VideoDevice,
    input_cs: DXGI_COLOR_SPACE_TYPE,
    output_cs: DXGI_COLOR_SPACE_TYPE,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<(), String> {
    let rate = DXGI_RATIONAL {
        Numerator: 30,
        Denominator: 1,
    };
    let mut support = D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT {
        InputSample: D3D12_VIDEO_SAMPLE {
            Width: from.0,
            Height: from.1,
            Format: D3D12_VIDEO_FORMAT {
                Format: DXGI_FORMAT_NV12,
                ColorSpace: input_cs,
            },
        },
        InputFieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
        InputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
        InputFrameRate: rate,
        OutputFormat: D3D12_VIDEO_FORMAT {
            Format: DXGI_FORMAT_NV12,
            ColorSpace: output_cs,
        },
        OutputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
        OutputFrameRate: rate,
        ..Default::default()
    };
    // Sound: `support` outlives the call; the size matches its type.
    unsafe {
        video.CheckFeatureSupport(
            D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
            std::ptr::addr_of_mut!(support).cast::<std::ffi::c_void>(),
            u32::try_from(std::mem::size_of::<D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT>())
                .unwrap_or(u32::MAX),
        )
    }
    .map_err(|error| format!("CheckFeatureSupport: {error}"))?;
    if !support
        .SupportFlags
        .contains(D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED)
    {
        return Err("video process unsupported".to_owned());
    }
    let range = support.ScaleSupport.OutputSizeRange;
    let flags = support.ScaleSupport.Flags;
    if to.0 < range.MinWidth
        || to.0 > range.MaxWidth
        || to.1 < range.MinHeight
        || to.1 > range.MaxHeight
    {
        return Err(format!(
            "scale target {}x{} outside process range {}x{}-{}x{}",
            to.0, to.1, range.MinWidth, range.MinHeight, range.MaxWidth, range.MaxHeight
        ));
    }
    if flags.contains(D3D12_VIDEO_SCALE_SUPPORT_FLAG_POW2_ONLY)
        && (!to.0.is_power_of_two() || !to.1.is_power_of_two())
    {
        return Err("scale target outside power of two".to_owned());
    }
    if flags.contains(D3D12_VIDEO_SCALE_SUPPORT_FLAG_EVEN_DIMENSIONS_ONLY)
        && (!to.0.is_multiple_of(2) || !to.1.is_multiple_of(2))
    {
        return Err("scale target outside even extents".to_owned());
    }
    Ok(())
}

/// Video processor for `op` from `from` to `to` in the colorspace pair.
fn create_processor(
    video: &ID3D12VideoDevice,
    input_cs: DXGI_COLOR_SPACE_TYPE,
    output_cs: DXGI_COLOR_SPACE_TYPE,
    op: Op,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<ID3D12VideoProcessor, String> {
    let rate = DXGI_RATIONAL {
        Numerator: 30,
        Denominator: 1,
    };
    let output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC {
        Format: DXGI_FORMAT_NV12,
        ColorSpace: output_cs,
        AlphaFillMode: D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE,
        FrameRate: rate,
        ..Default::default()
    };
    let source = if op == Op::Crop { to } else { from };
    let input = D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC {
        Format: DXGI_FORMAT_NV12,
        ColorSpace: input_cs,
        FrameRate: rate,
        SourceSizeRange: D3D12_VIDEO_SIZE_RANGE {
            MaxWidth: source.0,
            MaxHeight: source.1,
            MinWidth: source.0,
            MinHeight: source.1,
        },
        DestinationSizeRange: D3D12_VIDEO_SIZE_RANGE {
            MaxWidth: to.0,
            MaxHeight: to.1,
            MinWidth: to.0,
            MinHeight: to.1,
        },
        FilterFlags: D3D12_VIDEO_PROCESS_FILTER_FLAG_NONE,
        StereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
        FieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
        DeinterlaceMode: D3D12_VIDEO_PROCESS_DEINTERLACE_FLAG_NONE,
        ..Default::default()
    };
    // Sound: the descriptors outlive the call.
    unsafe {
        video.CreateVideoProcessor(0, std::ptr::addr_of!(output), std::slice::from_ref(&input))
    }
    .map_err(|error| format!("CreateVideoProcessor: {error}"))
}

/// Fence wait through `event`.
fn wait(fence: &ID3D12Fence, ticket: u64, event: HANDLE) -> Result<(), String> {
    // Sound: polling a live fence for a signalled value.
    if unsafe { fence.GetCompletedValue() } >= ticket {
        return Ok(());
    }
    // Sound: the fence and event are live.
    unsafe { fence.SetEventOnCompletion(ticket, event) }
        .map_err(|error| format!("SetEventOnCompletion: {error}"))?;
    // Sound: the event is live with a bounded wait.
    let outcome = unsafe { WaitForSingleObject(event, WAIT_MS) };
    if outcome == WAIT_OBJECT_0 {
        return Ok(());
    }
    if outcome == WAIT_TIMEOUT {
        return Err("fence wait timed out".to_owned());
    }
    Err(format!("WaitForSingleObject: {}", outcome.0))
}
