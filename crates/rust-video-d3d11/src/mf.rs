//! `MediaFoundation` encoder session behind the D3D11 encode path.
//!
//! The live implementation targets Windows; other platforms decline at
//! `open` with a typed skip.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::collections::VecDeque;
#[cfg(target_os = "windows")]
use std::mem::ManuallyDrop;
#[cfg(target_os = "windows")]
use std::sync::OnceLock;
#[cfg(target_os = "windows")]
use std::time::Duration;

use crate::device::D3d11Device;
use crate::error::D3d11Error;
#[cfg(not(target_os = "windows"))]
use crate::error::SkipReason;
use crate::guid::Codec;

#[cfg(target_os = "windows")]
use bytes::Bytes;
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::core::GUID;
#[cfg(target_os = "windows")]
use windows::core::PWSTR;
#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform,
    METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MFCreateAlignedMemoryBuffer, MFCreateMediaType, MFCreateSample, MFMediaType_Video,
    MFSampleExtension_CleanPoint, MFStartup, MFTEnumEx, MFT_FRIENDLY_NAME_Attribute,
    MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_H265, MFVideoFormat_HEVC,
    MFVideoFormat_NV12, MFVideoFormat_VP90, MFVideoInterlace_Progressive,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SYNCMFT,
    MFT_INPUT_STREAM_INFO, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_TYPE,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
    MF_EVENT_TYPE, MF_E_NOTACCEPTING, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

/// Output frame rate in frames per second.
#[cfg(target_os = "windows")]
const FRAME_RATE: u32 = 30;

/// Sample duration in 100ns units at the output frame rate.
#[cfg(target_os = "windows")]
const FRAME_HNS: i64 = 10_000_000 / 30;

/// Input buffer alignment in bytes without a stream report.
#[cfg(target_os = "windows")]
const SAMPLE_ALIGN: u32 = 16;

/// Bitrate floor in bits per second.
#[cfg(target_os = "windows")]
const MIN_BITRATE: u64 = 100_000;

/// Bitrate ceiling in bits per second.
#[cfg(target_os = "windows")]
const MAX_BITRATE: u64 = 50_000_000;

/// Catalog walk bound in entries.
#[cfg(target_os = "windows")]
const AVAILABLE_TYPES: u32 = 64;

/// Catalog entry reader behind the type walk.
#[cfg(target_os = "windows")]
type Available = unsafe fn(&IMFTransform, u32, u32) -> windows::core::Result<IMFMediaType>;

/// Negotiated encoder session over one `MediaFoundation` transform.
pub struct Session {
    /// Encoder transform.
    #[cfg(target_os = "windows")]
    transform: IMFTransform,
    /// Input stream id.
    #[cfg(target_os = "windows")]
    input: u32,
    /// Output stream id.
    #[cfg(target_os = "windows")]
    output: u32,
    /// Transform allocates its own output samples.
    #[cfg(target_os = "windows")]
    provides: bool,
    /// Output buffer length in bytes.
    #[cfg(target_os = "windows")]
    out_size: u32,
    /// Output buffer alignment in bytes.
    #[cfg(target_os = "windows")]
    out_align: u32,
    /// Input buffer alignment in bytes.
    #[cfg(target_os = "windows")]
    in_align: u32,
    /// Async event source, absent on synchronous transforms.
    #[cfg(target_os = "windows")]
    events: Option<IMFMediaEventGenerator>,
    /// Transform D3D11 awareness.
    #[cfg(target_os = "windows")]
    d3d11_aware: bool,
    /// Serving encoder MFT name.
    #[cfg(target_os = "windows")]
    name: String,
    /// Untried encoder activates in priority order.
    #[cfg(target_os = "windows")]
    standby: VecDeque<IMFActivate>,
    /// Output subtype behind the negotiated media type.
    #[cfg(target_os = "windows")]
    subtype: GUID,
    /// Codec the session encodes.
    #[cfg(target_os = "windows")]
    codec: Codec,
    /// Negotiated extent, absent before the first frame.
    #[cfg(target_os = "windows")]
    size: Option<(u32, u32)>,
    /// Next input timestamp in 100ns units.
    #[cfg(target_os = "windows")]
    pts: i64,
    /// Queued input credits from the event pump.
    #[cfg(target_os = "windows")]
    credits: u32,
}

impl Session {
    /// Session over `device` for `codec` behind the first matching encoder MFT.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, without `MediaFoundation`, or without a
    /// `codec` encoder MFT on the machine.
    pub fn open(device: &D3d11Device, codec: Codec) -> Result<Self, D3d11Error> {
        #[cfg(target_os = "windows")]
        {
            Self::open_windows(device, codec)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device, codec);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Session over `device` for `codec`.
    #[cfg(target_os = "windows")]
    fn open_windows(device: &D3d11Device, codec: Codec) -> Result<Self, D3d11Error> {
        let _com = ComGuard::enter();
        startup()?;
        let _ = device;
        let mut standby = VecDeque::new();
        let mut subtype = MFVideoFormat_H264;
        for candidate in subtypes(codec) {
            let matches = enumerate(&candidate);
            if !matches.is_empty() {
                standby = matches.into_iter().collect();
                subtype = candidate;
                break;
            }
        }
        let Some(opened) = elect(&mut standby, codec) else {
            return Err(missing_encoder(codec));
        };
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
    #[cfg(target_os = "windows")]
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
        #[cfg(target_os = "windows")]
        {
            self.d3d11_aware
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = self;
            false
        }
    }

    /// Name of the serving encoder MFT.
    #[must_use]
    pub fn mft_name(&self) -> &str {
        #[cfg(target_os = "windows")]
        {
            &self.name
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = self;
            "unavailable"
        }
    }

    /// Packets coded from one NV12 frame of `width` x `height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, when the bytes miss the extent, or when
    /// the transform refuses the frame.
    pub fn encode(
        &mut self,
        width: u32,
        height: u32,
        nv12: &[u8],
    ) -> Result<Vec<Packet>, D3d11Error> {
        #[cfg(target_os = "windows")]
        {
            self.encode_windows(width, height, nv12)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self, width, height, nv12);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Packets coded from one NV12 frame of `width` x `height`.
    #[cfg(target_os = "windows")]
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
        let mut packets = Vec::new();
        if self.size != Some((width, height)) {
            if self.size.is_some() {
                packets.append(&mut self.drain()?);
                self.message(MFT_MESSAGE_COMMAND_FLUSH)?;
            }
            self.configure(width, height)?;
        }
        let sample = input_sample(nv12, self.in_align, self.pts)?;
        self.pts = self.pts.wrapping_add(FRAME_HNS);
        if self.events.is_some() {
            self.submit_async(&sample, &mut packets)?;
        } else {
            self.submit_sync(&sample, &mut packets)?;
        }
        Ok(packets)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows or when the transform refuses the drain.
    pub fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        #[cfg(target_os = "windows")]
        {
            let _com = ComGuard::enter();
            if self.size.is_none() {
                return Ok(Vec::new());
            }
            self.drain()
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = self;
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Media types for `width` x `height`, falling back across MFTs.
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
    fn configure_try(&mut self, width: u32, height: u32) -> Result<(), D3d11Error> {
        let target = bitrate(width, height);
        let output_type = find_available(
            &self.transform,
            self.output,
            &self.subtype,
            IMFTransform::GetOutputAvailableType,
        )
        .map_or_else(|| media_type(self.subtype, width, height, Some(target)), Ok)?;
        stamp(&output_type, width, height, Some(target))?;
        // Sound: the transform and type are live; output precedes input.
        call(
            unsafe { self.transform.SetOutputType(self.output, &output_type, 0) },
            "SetOutputType",
        )?;
        let input_type = find_available(
            &self.transform,
            self.input,
            &MFVideoFormat_NV12,
            IMFTransform::GetInputAvailableType,
        )
        .map_or_else(|| media_type(MFVideoFormat_NV12, width, height, None), Ok)?;
        stamp(&input_type, width, height, None)?;
        // Sound: the transform and type are live past the output set.
        call(
            unsafe { self.transform.SetInputType(self.input, &input_type, 0) },
            "SetInputType",
        )?;
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

    /// Output buffer shape behind the current output type.
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
    fn message(&self, message: MFT_MESSAGE_TYPE) -> Result<(), D3d11Error> {
        // Sound: the transform is live.
        call(
            unsafe { self.transform.ProcessMessage(message, 0) },
            "ProcessMessage",
        )
    }

    /// `sample` through a synchronous transform into `packets`.
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
    fn event_source(&self) -> Result<IMFMediaEventGenerator, D3d11Error> {
        self.events
            .clone()
            .ok_or_else(|| D3d11Error::Unsupported("async MFT without an event source".to_owned()))
    }

    /// Drained outputs with the session left streaming.
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
    fn collect_sync(&mut self, packets: &mut Vec<Packet>) -> Result<(), D3d11Error> {
        while self.try_output(packets)? {}
        Ok(())
    }

    /// One output attempt; true when a sample may follow.
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
struct ComGuard {
    /// Thread entry owned by this guard.
    owned: bool,
}

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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

/// `MediaFoundation` platform, started once per process.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
fn missing_encoder(codec: Codec) -> D3d11Error {
    D3d11Error::Unsupported(format!("no {codec} encoder MFT on this machine"))
}

/// Output subtypes for `codec`, preferred first.
#[cfg(target_os = "windows")]
fn subtypes(codec: Codec) -> Vec<GUID> {
    match codec {
        Codec::H264 => vec![MFVideoFormat_H264],
        Codec::Hevc => vec![MFVideoFormat_HEVC, MFVideoFormat_H265],
        Codec::Vp9 => vec![MFVideoFormat_VP90],
        Codec::Av1 => vec![MFVideoFormat_AV1],
        Codec::Mpeg2 | Codec::Vc1 => Vec::new(),
    }
}

/// Encoder activates for `subtype`, hardware before software.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
fn free_catalog(activates: *mut Option<IMFActivate>) {
    if !activates.is_null() {
        // Sound: the array came from the enum call.
        unsafe {
            CoTaskMemFree(Some(activates.cast_const().cast()));
        }
    }
}

/// First activating transform from `standby`, consumed in order.
#[cfg(target_os = "windows")]
fn elect(standby: &mut VecDeque<IMFActivate>, codec: Codec) -> Option<Opened> {
    while let Some(next) = standby.pop_front() {
        if let Ok(opened) = activate_one(&next, codec) {
            return Some(opened);
        }
    }
    None
}

/// Live parts behind `activate`.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Packed NV12 byte length for `width` x `height`.
#[cfg(target_os = "windows")]
fn nv12_len(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(3)
        / 2
}

/// Bitrate in bits per second scaled to the frame area.
#[cfg(target_os = "windows")]
fn bitrate(width: u32, height: u32) -> u32 {
    let estimate = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(3);
    u32::try_from(estimate.clamp(MIN_BITRATE, MAX_BITRATE)).unwrap_or(u32::MAX)
}

/// Flag bits as the comparison word.
#[cfg(target_os = "windows")]
fn u32_flag(bits: i32) -> u32 {
    u32::try_from(bits).unwrap_or(0)
}

/// Input sample over `nv12` stamped at `pts`.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
fn pts_duration(hns: i64) -> Duration {
    let hns = u64::try_from(hns).unwrap_or(0);
    Duration::from_nanos(hns.saturating_mul(100))
}

/// Next event type behind `events`, checked for failure.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
fn is_event(typ: u32, want: MF_EVENT_TYPE) -> bool {
    u32::try_from(want.0).is_ok_and(|want| typ == want)
}

/// Windows result behind `result` as a tier failure naming `call`.
#[cfg(target_os = "windows")]
fn call<T>(result: windows::core::Result<T>, call: &'static str) -> Result<T, D3d11Error> {
    result.map_err(|error| D3d11Error::Call {
        call,
        hresult: error.code().0,
    })
}
