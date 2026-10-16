//! `MediaFoundation` encoder session behind the D3D11 encode path.

#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::time::Duration;

use crate::device::D3d11Device;
use crate::error::D3d11Error;
use crate::guid::Codec;

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use windows::core::Interface;
use windows::core::GUID;
use windows::core::PWSTR;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType,
    IMFSample, IMFTransform, METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MFCreateAlignedMemoryBuffer, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer,
    MFCreateMediaType, MFCreateSample, MFMediaType_Video, MFSampleExtension_CleanPoint, MFStartup,
    MFTEnumEx, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_AV1, MFVideoFormat_H264,
    MFVideoFormat_H265, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_VP90,
    MFVideoInterlace_Progressive, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MFSTARTUP_FULL,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SYNCMFT, MFT_INPUT_STREAM_INFO, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_MESSAGE_TYPE,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
    MF_EVENT_TYPE, MF_E_NOTACCEPTING, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_SA_D3D11_AWARE, MF_SA_D3D11_BINDFLAGS, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
    MF_VERSION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

/// Output frame rate in frames per second.
const FRAME_RATE: u32 = 30;

/// Sample duration in 100ns units at the output frame rate.
const FRAME_HNS: i64 = 10_000_000 / 30;

/// Input buffer alignment in bytes without a stream report.
const SAMPLE_ALIGN: u32 = 16;

/// Bitrate floor in bits per second.
const MIN_BITRATE: u64 = 100_000;

/// Bitrate ceiling in bits per second.
const MAX_BITRATE: u64 = 50_000_000;

/// Catalog walk bound in entries.
const AVAILABLE_TYPES: u32 = 64;

/// Catalog entry reader behind the type walk.
type Available = unsafe fn(&IMFTransform, u32, u32) -> windows::core::Result<IMFMediaType>;

/// Negotiated encoder session over one `MediaFoundation` transform.
pub struct Session {
    /// Encoder transform.
    transform: IMFTransform,
    /// Input stream id.
    input: u32,
    /// Output stream id.
    output: u32,
    /// Transform allocates its own output samples.
    provides: bool,
    /// Output buffer length in bytes.
    out_size: u32,
    /// Output buffer alignment in bytes.
    out_align: u32,
    /// Input buffer alignment in bytes.
    in_align: u32,
    /// Async event source, absent on synchronous transforms.
    events: Option<IMFMediaEventGenerator>,
    /// Transform D3D11 awareness.
    d3d11_aware: bool,
    /// D3D11 bind behind GPU samples, absent without a bound transform.
    gpu: Option<GpuBind>,
    /// Serving encoder MFT name.
    name: String,
    /// Untried encoder activates in priority order.
    standby: VecDeque<IMFActivate>,
    /// Output subtype behind the negotiated media type.
    subtype: GUID,
    /// Codec the session encodes.
    codec: Codec,
    /// Negotiated extent, absent before the first frame.
    size: Option<(u32, u32)>,
    /// Next input timestamp in 100ns units.
    pts: i64,
    /// Queued input credits from the event pump.
    credits: u32,
}

impl Session {
    /// Session over `device` for `codec` behind the first matching encoder MFT.
    ///
    /// # Errors
    ///
    /// Returns `Err` without `MediaFoundation`, or without a
    /// `codec` encoder MFT on the machine.
    pub fn open(device: &D3d11Device, codec: Codec) -> Result<Self, D3d11Error> {
        Self::open_windows(device, codec)
    }

    /// Session over `device` for `codec`.
    fn open_windows(device: &D3d11Device, codec: Codec) -> Result<Self, D3d11Error> {
        let _com = ComGuard::enter();
        startup()?;
        let mut standby = VecDeque::new();
        let mut subtype = MFVideoFormat_H264;
        for candidate in subtypes(codec) {
            let matches = enumerate(candidate);
            if !matches.is_empty() {
                standby = matches.into_iter().collect();
                subtype = *candidate;
                break;
            }
        }
        let Some(opened) = elect(&mut standby, codec) else {
            return Err(missing_encoder(codec));
        };
        let gpu = GpuBind::open(device, opened.d3d11_aware);
        Ok(Self {
            transform: opened.transform,
            input: opened.input,
            output: opened.output,
            provides: false,
            out_size: 0,
            out_align: 1,
            in_align: SAMPLE_ALIGN,
            events: opened.events,
            d3d11_aware: opened.d3d11_aware,
            gpu,
            name: opened.name,
            standby,
            subtype,
            codec,
            size: None,
            pts: 0,
            credits: 0,
        })
    }

    /// Next activating transform from the standby queue.
    fn adopt(&mut self) -> Result<(), D3d11Error> {
        let Some(opened) = elect(&mut self.standby, self.codec) else {
            return Err(missing_encoder(self.codec));
        };
        self.transform = opened.transform;
        self.events = opened.events;
        self.d3d11_aware = opened.d3d11_aware;
        self.name = opened.name;
        self.input = opened.input;
        self.output = opened.output;
        self.in_align = SAMPLE_ALIGN;
        self.credits = 0;
        Ok(())
    }

    /// True when the transform reports D3D11 awareness.
    #[must_use]
    pub fn uses_d3d11(&self) -> bool {
        self.d3d11_aware
    }

    /// True when the session encodes from GPU textures.
    #[must_use]
    pub(crate) fn gpu_bound(&self) -> bool {
        self.gpu.is_some()
    }

    /// Name of the serving encoder MFT.
    #[must_use]
    pub fn mft_name(&self) -> &str {
        &self.name
    }

    /// Packets coded from one NV12 frame of `width` x `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the bytes miss the extent, or when
    /// the transform refuses the frame.
    pub fn encode(
        &mut self,
        width: u32,
        height: u32,
        nv12: &[u8],
    ) -> Result<Vec<Packet>, D3d11Error> {
        self.encode_windows(width, height, nv12)
    }

    /// Packets coded from one NV12 frame of `width` x `height`.
    fn encode_windows(
        &mut self,
        width: u32,
        height: u32,
        nv12: &[u8],
    ) -> Result<Vec<Packet>, D3d11Error> {
        let _com = ComGuard::enter();
        let expect = nv12_len(width, height);
        if u64::try_from(nv12.len()).unwrap_or(0) != expect {
            return Err(D3d11Error::Unsupported(
                "packed NV12 length outside its extent".to_owned(),
            ));
        }
        let mut packets = self.ensure_size(width, height)?;
        let sample = input_sample(nv12, self.in_align, self.pts)?;
        self.submit_sample(&sample, &mut packets)?;
        Ok(packets)
    }

    /// Drained packets across the size switch to `width` x `height`.
    fn ensure_size(&mut self, width: u32, height: u32) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        if self.size != Some((width, height)) {
            if self.size.is_some() {
                packets.append(&mut self.drain()?);
                self.message(MFT_MESSAGE_COMMAND_FLUSH)?;
            }
            self.configure(width, height)?;
        }
        Ok(packets)
    }

    /// `sample` through the sync or async submit path into `packets`.
    fn submit_sample(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        self.pts = self.pts.wrapping_add(FRAME_HNS);
        if self.events.is_some() {
            self.submit_async(sample, packets)
        } else {
            self.submit_sync(sample, packets)
        }
    }

    /// Packets coded from the `slice` decode-texture region at (`left`, `top`) sized `width` by `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a D3D11 bind, for an odd extent, or when the
    /// transform refuses the frame.
    pub(crate) fn encode_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<Packet>, D3d11Error> {
        let _com = ComGuard::enter();
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(D3d11Error::Unsupported("odd NV12 extent".to_owned()));
        }
        let mut packets = self.ensure_size(width, height)?;
        let region = Region {
            slice,
            left,
            top,
            width,
            height,
        };
        let gpu = self
            .gpu
            .as_mut()
            .ok_or_else(|| D3d11Error::Unsupported("encoder without gpu import".to_owned()))?;
        let sample = gpu.sample(&self.transform, self.input, texture, &region, self.pts)?;
        self.submit_sample(&sample, &mut packets)?;
        Ok(packets)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the transform refuses the drain.
    pub fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        let _com = ComGuard::enter();
        if self.size.is_none() {
            return Ok(Vec::new());
        }
        self.drain()
    }

    /// Media types for `width` x `height`, falling back across MFTs.
    fn configure(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        loop {
            match self.configure_try(width, height) {
                Ok(()) => return Ok(()),
                Err(_) if !self.standby.is_empty() => self.adopt()?,
                Err(error) => return Err(error),
            }
        }
    }

    /// Media types for `width` x `height` plus streaming start.
    fn configure_try(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        self.set_output_type(width, height)?;
        self.set_input_type(width, height)?;
        let bound = self
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.attach(&self.transform).is_ok());
        if !bound {
            self.gpu = None;
        }
        self.message(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)?;
        self.message(MFT_MESSAGE_NOTIFY_START_OF_STREAM)?;
        let mut info: MFT_INPUT_STREAM_INFO = unsafe { std::mem::zeroed() };
        // Sound: the info stays live for the call; failure keeps the default align.
        if unsafe {
            self.transform
                .GetInputStreamInfo(self.input, std::ptr::addr_of_mut!(info))
        }
        .is_ok()
        {
            self.in_align = info.cbAlignment.max(1);
        }
        self.refresh_output_info(width, height)?;
        self.size = Some((width, height));
        Ok(())
    }

    /// Output media type for `width` x `height` behind the target bitrate.
    fn set_output_type(&self, width: u32, height: u32) -> Result<(), D3d11Error> {
        let target = bitrate(width, height);
        let output_type = find_available(
            &self.transform,
            self.output,
            &self.subtype,
            IMFTransform::GetOutputAvailableType,
        )
        .map_or_else(|| media_type(self.subtype, width, height, Some(target)), Ok)?;
        stamp(&output_type, width, height, Some(target))?;
        // Sound: the transform and type are live.
        call(
            unsafe { self.transform.SetOutputType(self.output, &output_type, 0) },
            "SetOutputType",
        )
    }

    /// Input NV12 media type for `width` x `height`.
    fn set_input_type(&self, width: u32, height: u32) -> Result<(), D3d11Error> {
        let input_type = find_available(
            &self.transform,
            self.input,
            &MFVideoFormat_NV12,
            IMFTransform::GetInputAvailableType,
        )
        .map_or_else(|| media_type(MFVideoFormat_NV12, width, height, None), Ok)?;
        stamp(&input_type, width, height, None)?;
        // Sound: the transform and type are live.
        call(
            unsafe { self.transform.SetInputType(self.input, &input_type, 0) },
            "SetInputType",
        )
    }

    /// Output buffer shape behind the current output type.
    fn refresh_output_info(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        // Sound: the transform is live past the output set.
        let info = call(
            unsafe { self.transform.GetOutputStreamInfo(self.output) },
            "GetOutputStreamInfo",
        )?;
        self.provides = info.dwFlags & u32_flag(MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0) != 0;
        let fallback = u32::try_from(nv12_len(width, height)).unwrap_or(u32::MAX);
        self.out_size = if info.cbSize == 0 {
            fallback
        } else {
            info.cbSize
        };
        self.out_align = info.cbAlignment.max(1);
        Ok(())
    }

    /// Output type behind a stream change.
    fn renegotiate(&mut self) -> Result<(), D3d11Error> {
        // Sound: the transform is live past the stream change.
        let media = call(
            unsafe { self.transform.GetOutputAvailableType(self.output, 0) },
            "GetOutputAvailableType",
        )?;
        call(
            unsafe { self.transform.SetOutputType(self.output, &media, 0) },
            "SetOutputType",
        )?;
        let (width, height) = self.size.unwrap_or((0, 0));
        self.refresh_output_info(width, height)
    }

    /// `message` through the transform.
    fn message(&self, message: MFT_MESSAGE_TYPE) -> Result<(), D3d11Error> {
        // Sound: the transform is live.
        call(
            unsafe { self.transform.ProcessMessage(message, 0) },
            "ProcessMessage",
        )
    }

    /// `sample` through a synchronous transform into `packets`.
    fn submit_sync(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        // Sound: the transform and sample are live.
        match unsafe { self.transform.ProcessInput(self.input, sample, 0) } {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_NOTACCEPTING => {
                self.collect_sync(packets)?;
                call(
                    unsafe { self.transform.ProcessInput(self.input, sample, 0) },
                    "ProcessInput",
                )?;
            }
            Err(error) => return call(Err(error), "ProcessInput"),
        }
        self.collect_sync(packets)
    }

    /// `sample` through an asynchronous transform into `packets`.
    fn submit_async(
        &mut self,
        sample: &IMFSample,
        packets: &mut Vec<Packet>,
    ) -> Result<(), D3d11Error> {
        self.await_credit(packets)?;
        // Sound: the transform and sample are live past the input credit.
        call(
            unsafe { self.transform.ProcessInput(self.input, sample, 0) },
            "ProcessInput",
        )?;
        self.poll(packets)
    }

    /// One input credit, collecting outputs queued ahead of it.
    fn await_credit(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        if self.credits > 0 {
            self.credits -= 1;
            return Ok(());
        }
        let events = self.event_source()?;
        loop {
            let typ = event_type(&events)?;
            if is_event(typ, METransformNeedInput) {
                return Ok(());
            }
            if is_event(typ, METransformHaveOutput) {
                self.try_output(packets)?;
            }
        }
    }

    /// Queued outputs into `packets` without blocking.
    fn poll(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        let events = self.event_source()?;
        loop {
            // Sound: the event source is live.
            let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(error) => return call(Err(error), "GetEvent"),
            };
            // Sound: the event is live.
            let typ = call(unsafe { event.GetType() }, "GetType")?;
            if is_event(typ, METransformHaveOutput) {
                self.try_output(packets)?;
            } else if is_event(typ, METransformNeedInput) {
                self.credits += 1;
            }
        }
    }

    /// Cloned async event source.
    fn event_source(&self) -> Result<IMFMediaEventGenerator, D3d11Error> {
        self.events
            .clone()
            .ok_or_else(|| D3d11Error::Unsupported("async MFT without an event source".to_owned()))
    }

    /// Drained outputs with the session left streaming.
    fn drain(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        let mut packets = Vec::new();
        self.message(MFT_MESSAGE_COMMAND_DRAIN)?;
        if self.events.is_some() {
            let events = self.event_source()?;
            loop {
                let typ = event_type(&events)?;
                if is_event(typ, METransformDrainComplete) {
                    break;
                }
                if is_event(typ, METransformHaveOutput) {
                    self.try_output(&mut packets)?;
                }
            }
        } else {
            self.collect_sync(&mut packets)?;
        }
        Ok(packets)
    }

    /// Available outputs into `packets` until the transform starves.
    fn collect_sync(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        while self.try_output(packets)? {}
        Ok(())
    }

    /// One output attempt; true when a sample may follow.
    fn try_output(&mut self, packets: &mut Vec<Packet>) -> Result<bool, D3d11Error> {
        loop {
            let mut buffer = self.output_buffer()?;
            let mut status = 0u32;
            // Sound: the buffer and status stay live for the call.
            let result = unsafe {
                self.transform.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut buffer),
                    std::ptr::addr_of_mut!(status),
                )
            };
            // Sound: the buffer slot is ours in every outcome.
            let held = unsafe { ManuallyDrop::take(&mut buffer.pSample) };
            match result {
                Ok(()) => {
                    if let Some(sample) = held {
                        packets.push(extract(&sample)?);
                    }
                    if buffer.dwStatus & u32_flag(MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE.0) != 0 {
                        self.renegotiate()?;
                    }
                    return Ok(true);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Ok(false);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.renegotiate()?;
                }
                Err(error) => return call(Err(error), "ProcessOutput"),
            }
        }
    }

    /// Output buffer with a sample unless the transform provides.
    fn output_buffer(&self) -> Result<MFT_OUTPUT_DATA_BUFFER, D3d11Error> {
        let sample = if self.provides {
            None
        } else {
            // Sound: the sample and buffer are live for the output call.
            let sample = call(unsafe { MFCreateSample() }, "MFCreateSample")?;
            let buffer = call(
                unsafe { MFCreateAlignedMemoryBuffer(self.out_size, self.out_align) },
                "MFCreateAlignedMemoryBuffer",
            )?;
            call(unsafe { sample.AddBuffer(&buffer) }, "AddBuffer")?;
            Some(sample)
        };
        Ok(MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        })
    }
}

/// COM apartment guard; uninitializes only a self-owned entry.
struct ComGuard {
    /// Thread entry owned by this guard.
    owned: bool,
}

impl ComGuard {
    /// Multithreaded entry for the calling thread.
    fn enter() -> Self {
        // Sound: no reserved context crosses the call.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            owned: entered == windows::Win32::Foundation::S_OK,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            // Sound: this guard owns the matching entry.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// Live parts behind one encoder activate.
struct Opened {
    /// Encoder transform.
    transform: IMFTransform,
    /// Async event source, absent on synchronous transforms.
    events: Option<IMFMediaEventGenerator>,
    /// Transform D3D11 awareness.
    d3d11_aware: bool,
    /// Serving encoder MFT name.
    name: String,
    /// Input stream id.
    input: u32,
    /// Output stream id.
    output: u32,
}

/// D3D11 bind behind GPU encoder samples.
struct GpuBind {
    /// DXGI device manager behind the bound transform.
    manager: IMFDXGIDeviceManager,
    /// Logical device behind input textures.
    device: ID3D11Device,
    /// Immediate context behind GPU copies.
    context: ID3D11DeviceContext,
    /// Input texture plus its extent, recreated per size.
    input: Option<(ID3D11Texture2D, u32, u32)>,
}

impl GpuBind {
    /// Manager over `device`, absent without D3D11 awareness.
    fn open(device: &D3d11Device, aware: bool) -> Option<Self> {
        if !aware {
            return None;
        }
        let mut token = 0u32;
        let mut manager = None;
        // Sound: the out-parameters stay live for the call.
        unsafe {
            MFCreateDXGIDeviceManager(
                std::ptr::addr_of_mut!(token),
                std::ptr::addr_of_mut!(manager),
            )
        }
        .ok()?;
        let manager: IMFDXGIDeviceManager = manager?;
        // Sound: the device is live; the token came from the create call.
        unsafe { manager.ResetDevice(device.device(), token) }.ok()?;
        Some(Self {
            manager,
            device: device.device().clone(),
            context: device.context().clone(),
            input: None,
        })
    }

    /// Manager through `transform` as its D3D11 device manager.
    fn attach(&self, transform: &IMFTransform) -> Result<(), D3d11Error> {
        send_manager(transform, &self.manager)
    }

    /// Input sample over `region` of `texture`.
    fn sample(
        &mut self,
        transform: &IMFTransform,
        input: u32,
        texture: &ID3D11Texture2D,
        region: &Region,
        pts: i64,
    ) -> Result<IMFSample, D3d11Error> {
        let target = self.texture(transform, input, region.width, region.height)?;
        // 4:2:0 crop origins stay even; odd origins break planar copies.
        let src_box = D3D11_BOX {
            left: region.left,
            top: region.top,
            front: 0,
            right: region.left.saturating_add(region.width),
            bottom: region.top.saturating_add(region.height),
            back: 1,
        };
        // Sound: both textures are live; the slice and box name the decode extent.
        unsafe {
            self.context.CopySubresourceRegion(
                &target,
                0,
                0,
                0,
                0,
                texture,
                region.slice,
                Some(std::ptr::from_ref(&src_box)),
            );
        }
        // Sound: the texture is live past the copy.
        let buffer = call(
            unsafe {
                MFCreateDXGISurfaceBuffer(
                    std::ptr::from_ref(&ID3D11Texture2D::IID),
                    &target,
                    0,
                    false,
                )
            },
            "MFCreateDXGISurfaceBuffer",
        )?;
        // Sound: the sample and buffer are live for every set below.
        let sample = call(unsafe { MFCreateSample() }, "MFCreateSample")?;
        call(unsafe { sample.AddBuffer(&buffer) }, "AddBuffer")?;
        call(unsafe { sample.SetSampleTime(pts) }, "SetSampleTime")?;
        call(
            unsafe { sample.SetSampleDuration(FRAME_HNS) },
            "SetSampleDuration",
        )?;
        Ok(sample)
    }

    /// Input texture at `width` by `height`, honoring the MFT bind flags.
    fn texture(
        &mut self,
        transform: &IMFTransform,
        input: u32,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D, D3d11Error> {
        if let Some((texture, cached_width, cached_height)) = &self.input {
            if (*cached_width, *cached_height) == (width, height) {
                return Ok(texture.clone());
            }
        }
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
            BindFlags: bind_flags(transform, input),
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        // Sound: the descriptor stays live for the call.
        unsafe {
            self.device.CreateTexture2D(
                std::ptr::addr_of!(desc),
                None,
                Some(std::ptr::addr_of_mut!(texture)),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateTexture2D",
            hresult: error.code().0,
        })?;
        let texture: ID3D11Texture2D = texture.ok_or(D3d11Error::Call {
            call: "CreateTexture2D",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        })?;
        self.input = Some((texture.clone(), width, height));
        Ok(texture)
    }
}

/// Visible NV12 region inside a decode texture.
struct Region {
    /// Array slice behind the picture.
    slice: u32,
    /// Visible left offset in luma samples.
    left: u32,
    /// Visible top offset in luma samples.
    top: u32,
    /// Visible width in luma samples.
    width: u32,
    /// Visible height in luma samples.
    height: u32,
}

/// `manager` through `transform` as its D3D11 device manager.
fn send_manager(
    transform: &IMFTransform,
    manager: &IMFDXGIDeviceManager,
) -> Result<(), D3d11Error> {
    // Sound: the manager outlives the call; the transform AddRefs its copy.
    call(
        unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) },
        "ProcessMessage",
    )
}

/// MFT input bind flags behind `stream`, shader-resource without the hint.
fn bind_flags(transform: &IMFTransform, stream: u32) -> u32 {
    // Sound: the transform is live; absent keys read as unset.
    let hint = unsafe { transform.GetInputStreamAttributes(stream) }
        .ok()
        .and_then(|attrs| unsafe { attrs.GetUINT32(&MF_SA_D3D11_BINDFLAGS) }.ok())
        .unwrap_or(0);
    hint | u32_flag(D3D11_BIND_SHADER_RESOURCE.0)
}

/// `MediaFoundation` platform, started once per process.
fn startup() -> Result<(), D3d11Error> {
    // The platform starts once per process; per-instance shutdown races sibling encoders.
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    let status = STARTED.get_or_init(|| {
        // Sound: the version and flags are plain input values.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|error| error.code().0)
    });
    match status {
        Ok(()) => Ok(()),
        Err(hresult) => Err(D3d11Error::Call {
            call: "MFStartup",
            hresult: *hresult,
        }),
    }
}

/// Unsupported error naming the absent `codec` encoder MFT.
fn missing_encoder(codec: Codec) -> D3d11Error {
    D3d11Error::Unsupported(format!("no {codec} encoder MFT on this machine"))
}

/// Output subtypes for `codec`, preferred first.
fn subtypes(codec: Codec) -> &'static [GUID] {
    match codec {
        Codec::H264 => &[MFVideoFormat_H264],
        Codec::Hevc => &[MFVideoFormat_HEVC, MFVideoFormat_H265],
        Codec::Vp9 => &[MFVideoFormat_VP90],
        Codec::Av1 => &[MFVideoFormat_AV1],
        Codec::Mpeg2 | Codec::Vc1 => &[],
    }
}

/// Encoder activates for `subtype`, hardware before software.
fn enumerate(subtype: &GUID) -> Vec<IMFActivate> {
    let info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *subtype,
    };
    let mut found = Vec::new();
    for flags in [
        MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_SYNCMFT,
        MFT_ENUM_FLAG_ASYNCMFT,
    ] {
        found.append(&mut enumerate_flags(&info, flags));
    }
    found
}

/// Encoder activates for `info` under one flag set.
fn enumerate_flags(info: &MFT_REGISTER_TYPE_INFO, flags: MFT_ENUM_FLAG) -> Vec<IMFActivate> {
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // Sound: the info and out-parameters stay live for the call.
    let listed = unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            None,
            Some(std::ptr::from_ref(info)),
            std::ptr::addr_of_mut!(activates),
            std::ptr::addr_of_mut!(count),
        )
    };
    if listed.is_err() || count == 0 || activates.is_null() {
        free_catalog(activates);
        return Vec::new();
    }
    let count = usize::try_from(count).unwrap_or(0);
    let mut found = Vec::with_capacity(count);
    for index in 0..count {
        // Sound: the count bounds the array; each entry is a live activate moved out once.
        let entry = unsafe { activates.add(index).read() };
        if let Some(activate) = entry {
            found.push(activate);
        }
    }
    free_catalog(activates);
    found
}

/// Catalog array behind `activates` back to COM.
fn free_catalog(activates: *mut Option<IMFActivate>) {
    if !activates.is_null() {
        // Sound: the array came from the enum call.
        unsafe {
            CoTaskMemFree(Some(activates.cast_const().cast()));
        }
    }
}

/// First activating transform from `standby`, consumed in order.
fn elect(standby: &mut VecDeque<IMFActivate>, codec: Codec) -> Option<Opened> {
    while let Some(next) = standby.pop_front() {
        if let Ok(opened) = activate_one(&next, codec) {
            return Some(opened);
        }
    }
    None
}

/// Live parts behind `activate`.
fn activate_one(activate: &IMFActivate, codec: Codec) -> Result<Opened, D3d11Error> {
    // Sound: the activate is live.
    let transform: IMFTransform = call(unsafe { activate.ActivateObject() }, "ActivateObject")?;
    let (store, async_mft, aware) = attributes(&transform);
    if async_mft {
        unlock(store.as_ref())?;
    }
    let events = if async_mft {
        Some(call(transform.cast(), "QueryInterface")?)
    } else {
        None
    };
    let (input, output) = stream_ids(&transform);
    let name = friendly(activate).unwrap_or_else(|| format!("{codec} encoder MFT"));
    Ok(Opened {
        transform,
        events,
        d3d11_aware: aware,
        name,
        input,
        output,
    })
}

/// Attribute store plus async and D3D11-aware flags.
fn attributes(transform: &IMFTransform) -> (Option<IMFAttributes>, bool, bool) {
    // Sound: the transform is live.
    let Ok(store) = (unsafe { transform.GetAttributes() }) else {
        return (None, false, false);
    };
    // Sound: the store is live; absent keys read as unset.
    let async_mft = unsafe { store.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
    let aware = unsafe { store.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) != 0;
    (Some(store), async_mft, aware)
}

/// Asynchronous unlock behind `store`.
fn unlock(store: Option<&IMFAttributes>) -> Result<(), D3d11Error> {
    let Some(store) = store else {
        return Err(D3d11Error::Unsupported(
            "async MFT without an attribute store".to_owned(),
        ));
    };
    // Sound: the store is live.
    call(
        unsafe { store.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) },
        "SetUINT32",
    )
}

/// Friendly name behind `activate`, absent without the attribute.
fn friendly(activate: &IMFActivate) -> Option<String> {
    let mut name = PWSTR::null();
    let mut len = 0u32;
    // Sound: the activate and out-parameters stay live for the call.
    unsafe {
        activate.GetAllocatedString(
            &MFT_FRIENDLY_NAME_Attribute,
            std::ptr::addr_of_mut!(name),
            std::ptr::addr_of_mut!(len),
        )
    }
    .ok()?;
    // Sound: the string came from the get call above.
    let text = unsafe { name.to_string() }.unwrap_or_default();
    // Sound: the string came from the get call above.
    unsafe {
        CoTaskMemFree(Some(name.as_ptr().cast()));
    }
    (!text.is_empty()).then_some(text)
}

/// Stream ids, zero when the transform numbers its streams.
fn stream_ids(transform: &IMFTransform) -> (u32, u32) {
    let (mut inputs, mut outputs) = ([0u32; 1], [0u32; 1]);
    // Sound: the arrays stay live for the call; fixed streams read as zero.
    if unsafe { transform.GetStreamIDs(&mut inputs, &mut outputs) }.is_ok() {
        (
            inputs.first().copied().unwrap_or(0),
            outputs.first().copied().unwrap_or(0),
        )
    } else {
        (0, 0)
    }
}

/// Catalog type for `subtype` on `stream`, walked through `get`.
fn find_available(
    transform: &IMFTransform,
    stream: u32,
    subtype: &GUID,
    get: Available,
) -> Option<IMFMediaType> {
    for index in 0..AVAILABLE_TYPES {
        // Sound: the transform is live; exhaustion ends the walk.
        let candidate = unsafe { get(transform, stream, index) };
        let Ok(candidate) = candidate else {
            break;
        };
        // Sound: the candidate store is live; a missing key is not a match.
        let found = unsafe { candidate.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or(GUID::zeroed());
        if &found == subtype {
            return Some(candidate);
        }
    }
    None
}

/// Video media type for `subtype` at `width` x `height`.
fn media_type(
    subtype: GUID,
    width: u32,
    height: u32,
    bitrate: Option<u32>,
) -> Result<IMFMediaType, D3d11Error> {
    // Sound: the created type is live for every set below.
    let media = call(unsafe { MFCreateMediaType() }, "MFCreateMediaType")?;
    call(
        unsafe { media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video) },
        "SetGUID",
    )?;
    call(
        unsafe { media.SetGUID(&MF_MT_SUBTYPE, &raw const subtype) },
        "SetGUID",
    )?;
    stamp(&media, width, height, bitrate)?;
    Ok(media)
}

/// Frame geometry plus rate on `media`.
fn stamp(
    media: &IMFMediaType,
    width: u32,
    height: u32,
    bitrate: Option<u32>,
) -> Result<(), D3d11Error> {
    call(
        unsafe { media.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height)) },
        "SetUINT64",
    )?;
    call(
        unsafe { media.SetUINT64(&MF_MT_FRAME_RATE, pack(FRAME_RATE, 1)) },
        "SetUINT64",
    )?;
    call(
        unsafe {
            media.SetUINT32(
                &MF_MT_INTERLACE_MODE,
                u32_flag(MFVideoInterlace_Progressive.0),
            )
        },
        "SetUINT32",
    )?;
    call(
        unsafe { media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)) },
        "SetUINT64",
    )?;
    if let Some(bitrate) = bitrate {
        call(
            unsafe { media.SetUINT32(&MF_MT_AVG_BITRATE, bitrate) },
            "SetUINT32",
        )?;
    }
    Ok(())
}

/// Two 32-bit halves packed into one 64-bit attribute.
fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Packed NV12 byte length for `width` x `height`.
fn nv12_len(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(3)
        / 2
}

/// Bitrate in bits per second scaled to the frame area.
fn bitrate(width: u32, height: u32) -> u32 {
    let estimate = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(3);
    u32::try_from(estimate.clamp(MIN_BITRATE, MAX_BITRATE)).unwrap_or(u32::MAX)
}

/// Flag bits as the comparison word.
fn u32_flag(bits: i32) -> u32 {
    u32::try_from(bits).unwrap_or(0)
}

/// Input sample over `nv12` stamped at `pts`.
fn input_sample(nv12: &[u8], align: u32, pts: i64) -> Result<IMFSample, D3d11Error> {
    let len = u32::try_from(nv12.len())
        .map_err(|_| D3d11Error::Unsupported("frame over 4 GiB".to_owned()))?;
    // Sound: the sample and buffer are live for every set below.
    let sample = call(unsafe { MFCreateSample() }, "MFCreateSample")?;
    let buffer = call(
        unsafe { MFCreateAlignedMemoryBuffer(len, align) },
        "MFCreateAlignedMemoryBuffer",
    )?;
    let mut data: *mut u8 = std::ptr::null_mut();
    call(
        unsafe { buffer.Lock(std::ptr::addr_of_mut!(data), None, None) },
        "Lock",
    )?;
    // Sound: the lock spans `len` bytes.
    unsafe {
        data.copy_from_nonoverlapping(nv12.as_ptr(), nv12.len());
    }
    call(unsafe { buffer.Unlock() }, "Unlock")?;
    call(unsafe { buffer.SetCurrentLength(len) }, "SetCurrentLength")?;
    call(unsafe { sample.AddBuffer(&buffer) }, "AddBuffer")?;
    call(unsafe { sample.SetSampleTime(pts) }, "SetSampleTime")?;
    call(
        unsafe { sample.SetSampleDuration(FRAME_HNS) },
        "SetSampleDuration",
    )?;
    Ok(sample)
}

/// Packet behind one output sample.
fn extract(sample: &IMFSample) -> Result<Packet, D3d11Error> {
    // Sound: the sample is live past output.
    let buffer = call(unsafe { sample.GetBufferByIndex(0) }, "GetBufferByIndex")?;
    let len = call(unsafe { buffer.GetCurrentLength() }, "GetCurrentLength")?;
    let len = usize::try_from(len).unwrap_or(0);
    let mut data: *mut u8 = std::ptr::null_mut();
    call(
        unsafe { buffer.Lock(std::ptr::addr_of_mut!(data), None, None) },
        "Lock",
    )?;
    // Sound: the lock spans the current length.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast_const(), len) }.to_vec();
    call(unsafe { buffer.Unlock() }, "Unlock")?;
    // Sound: the sample store is live; a missing key is not a keyframe.
    let keyframe = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
    // Sound: the sample is live; a missing stamp reads as zero.
    let pts = unsafe { sample.GetSampleTime() }.map_or(Duration::ZERO, pts_duration);
    let mut packet = Packet::new(Bytes::from(bytes)).with_pts(pts);
    if keyframe {
        packet = packet.mark_keyframe();
    }
    Ok(packet)
}

/// Duration behind `hns` 100ns units.
fn pts_duration(hns: i64) -> Duration {
    let hns = u64::try_from(hns).unwrap_or(0);
    Duration::from_nanos(hns.saturating_mul(100))
}

/// Next event type behind `events`, checked for failure.
fn event_type(events: &IMFMediaEventGenerator) -> Result<u32, D3d11Error> {
    // Sound: the event source is live.
    let event = call(
        unsafe { events.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) },
        "GetEvent",
    )?;
    // Sound: the event is live.
    let status = call(unsafe { event.GetStatus() }, "GetStatus")?;
    if status.is_err() {
        return Err(D3d11Error::Call {
            call: "GetEvent",
            hresult: status.0,
        });
    }
    call(unsafe { event.GetType() }, "GetType")
}

/// True when `typ` names `want`.
fn is_event(typ: u32, want: MF_EVENT_TYPE) -> bool {
    u32::try_from(want.0).is_ok_and(|want| typ == want)
}

/// Windows result behind `result` as a tier failure naming `call`.
fn call<T>(result: windows::core::Result<T>, call: &'static str) -> Result<T, D3d11Error> {
    result.map_err(|error| D3d11Error::Call {
        call,
        hresult: error.code().0,
    })
}
