//! Vulkan pipeline: decode planes, in-place chroma clear, export encode.

// FFI boundary: command-buffer calls live here.
#![allow(unsafe_code)]

use crate::content::{Clip, Codec, Op};
use crate::measure::{median_of_5, Sample};
use crate::Outcome;

use ash::vk;
use rust_video::Encoder;
use rust_video::Packet;
use rust_video_vulkan::{
    Av1Decoder, CodecOp, H264Decoder, H264Encoder, H265Decoder, H265Encoder, VulkanDevice,
    VulkanError, VulkanFrame,
};

/// Fence wait budget in nanoseconds.
const WAIT_NS: u64 = 5_000_000_000;

/// Decode source behind one combo.
enum Decode<'a> {
    /// H.264 decoder.
    H264(H264Decoder<'a>),
    /// H.265 decoder.
    H265(H265Decoder<'a>),
    /// AV1 decoder.
    Av1(Av1Decoder<'a>),
}

/// Encode sink behind one combo.
enum Encode {
    /// H.264 encoder.
    H264(H264Encoder),
    /// H.265 encoder.
    H265(H265Encoder),
}

impl Decode<'_> {
    /// Frames decoded from `packet`, possibly none.
    fn decode(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, String> {
        match self {
            Self::H264(decoder) => decoder.decode_surface(packet).map_err(err),
            Self::H265(decoder) => decoder.decode_surface(packet).map_err(err),
            Self::Av1(decoder) => decoder.decode_surface(packet).map_err(err),
        }
    }

    /// Frames held back for reordering or buffering.
    fn flush(&mut self) -> Result<Vec<VulkanFrame>, String> {
        match self {
            Self::H264(decoder) => decoder.flush_surface().map_err(err),
            Self::H265(decoder) => decoder.flush_surface().map_err(err),
            Self::Av1(decoder) => decoder.flush_surface().map_err(err),
        }
    }
}

impl Encode {
    /// Packets coded from the device export behind `frame`.
    fn encode_export(&mut self, frame: &VulkanFrame) -> Result<Vec<Packet>, String> {
        match self {
            Self::H264(encoder) => encoder.encode_export(frame).map_err(err),
            Self::H265(encoder) => encoder.encode_export(frame).map_err(err),
        }
    }

    /// Packets held back for reordering or buffering.
    fn flush(&mut self) -> Result<Vec<Packet>, String> {
        match self {
            Self::H264(encoder) => encoder.flush().map_err(err),
            Self::H265(encoder) => encoder.flush().map_err(err),
        }
    }
}

/// Vulkan backend; devices open per combo.
pub struct Backend;

impl Backend {
    /// Backend when H.264 decode negotiates.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tier declines the machine.
    pub fn open() -> Result<Self, String> {
        match VulkanDevice::open_or_skip(CodecOp::H264Decode) {
            Ok(_) => Ok(Self),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Decode, clear chroma on the GPU, re-encode: median of 5.
    pub fn run(codec: Codec, op: Op, clip: &Clip) -> Outcome {
        if op != Op::Gray {
            return Outcome::Declined(
                "vulkan covers grayscale in place; downscale needs frame wrap".to_owned(),
            );
        }
        if codec == Codec::Vp9 {
            return Outcome::Declined("tier without vp9 decode".to_owned());
        }
        let packets = clip.packets();
        let first = match Self::preflight(codec, op, &packets) {
            Ok(first) => first,
            Err(error) => return Outcome::Declined(error),
        };
        let (fps, spread, frames, reran) = median_of_5(|| Self::pass(codec, &packets));
        Outcome::Ran {
            fps,
            spread,
            frames,
            reran,
            note: first,
        }
    }

    /// One validated pass plus the combo note, or the decline reason.
    fn preflight(codec: Codec, op: Op, packets: &[Packet]) -> Result<String, String> {
        let (decode_device, encode_device) = Self::devices(codec)?;
        let clear = Clear::open(&decode_device)?;
        let filtered = Self::filter_count(codec, &decode_device, &clear, packets)?;
        let mut source = Self::source(codec, &decode_device)?;
        let mut sink = Sink::new(&encode_device);
        let mut frames = 0;
        let mut out = Vec::new();
        for packet in packets {
            let decoded = source
                .decode(packet)
                .map_err(|error| format!("decoded {frames} before decline: {error}"))?;
            frames += decoded.len();
            for frame in &decoded {
                clear.apply(frame)?;
                sink.encode(frame, &mut out).map_err(|error| {
                    format!("decode+filter ok ({filtered}f), encode declined: {error}")
                })?;
            }
        }
        for frame in &source.flush()? {
            frames += 1;
            clear.apply(frame)?;
            sink.encode(frame, &mut out)?;
        }
        sink.flush(&mut out)?;
        if frames == 0 {
            return Err("decoder emitted no frames".to_owned());
        }
        if out.is_empty() {
            return Err("encoder emitted no bytes".to_owned());
        }
        crate::dump("vulkan", codec, op, sink.ext(), &out);
        Ok(format!(
            "{}, {frames} frames, {} bytes",
            sink.note(codec),
            out.len()
        ))
    }

    /// One timed pass; failures score zero frames.
    fn pass(codec: Codec, packets: &[Packet]) -> Sample {
        let zero = Sample {
            frames: 0,
            fps: 0.0,
        };
        let Ok((decode_device, encode_device)) = Self::devices(codec) else {
            return zero;
        };
        let Ok(mut source) = Self::source(codec, &decode_device) else {
            return zero;
        };
        let mut sink = Sink::new(&encode_device);
        let Ok(clear) = Clear::open(&decode_device) else {
            return zero;
        };
        let start = std::time::Instant::now();
        let mut frames = 0;
        let mut out = Vec::new();
        for packet in packets {
            let Ok(decoded) = source.decode(packet) else {
                return zero;
            };
            for frame in &decoded {
                if clear.apply(frame).is_err() {
                    return zero;
                }
                if sink.encode(frame, &mut out).is_err() {
                    return zero;
                }
            }
            out.clear();
            frames += decoded.len();
        }
        let elapsed = start.elapsed().as_secs_f64();
        let fps = f64::from(u32::try_from(frames).unwrap_or(u32::MAX)) / elapsed.max(1e-9);
        Sample { frames, fps }
    }

    /// Decode plus encode devices for `codec`.
    fn devices(codec: Codec) -> Result<(VulkanDevice, VulkanDevice), String> {
        let decode_op = match codec {
            Codec::H264 => CodecOp::H264Decode,
            Codec::H265 => CodecOp::H265Decode,
            Codec::Av1 => CodecOp::Av1Decode,
            Codec::Vp9 => return Err("tier without vp9 decode".to_owned()),
        };
        let encode_op = match codec {
            Codec::H264 => CodecOp::H264Encode,
            Codec::H265 | Codec::Av1 => CodecOp::H265Encode,
            Codec::Vp9 => return Err("tier without vp9 decode".to_owned()),
        };
        let decode = open_device(decode_op)?;
        let encode = open_device(encode_op).or_else(|_| open_device(CodecOp::H264Encode))?;
        Ok((decode, encode))
    }

    /// Frames decoded and filtered over `packets`, dropped for pool recycle.
    fn filter_count(
        codec: Codec,
        device: &VulkanDevice,
        clear: &Clear,
        packets: &[Packet],
    ) -> Result<usize, String> {
        let mut source = Self::source(codec, device)?;
        let mut filtered = 0;
        for packet in packets {
            for frame in &source.decode(packet)? {
                clear.apply(frame)?;
                filtered += 1;
            }
        }
        for frame in &source.flush()? {
            clear.apply(frame)?;
            filtered += 1;
        }
        Ok(filtered)
    }

    /// Decode source for `codec` on `device`.
    fn source(codec: Codec, device: &VulkanDevice) -> Result<Decode<'_>, String> {
        match codec {
            Codec::H264 => H264Decoder::open(device).map(Decode::H264).map_err(err),
            Codec::H265 => H265Decoder::open(device).map(Decode::H265).map_err(err),
            Codec::Av1 => Av1Decoder::open(device).map(Decode::Av1).map_err(err),
            Codec::Vp9 => Err("tier without vp9 decode".to_owned()),
        }
    }
}

/// In-place chroma clear over the decode queue.
struct Clear<'a> {
    /// Decode device behind the queue.
    device: &'a VulkanDevice,
    /// Command pool behind the buffer.
    pool: vk::CommandPool,
    /// Command buffer behind the clear.
    cmd: vk::CommandBuffer,
    /// Fence behind the submit.
    fence: vk::Fence,
}

impl<'a> Clear<'a> {
    /// Clear over the decode queue of `device`.
    fn open(device: &'a VulkanDevice) -> Result<Self, String> {
        let vk = device.vk_device();
        // Sound: the create infos stay live for each call.
        let pool = unsafe {
            vk.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(device.queue_family())
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .map_err(|error| format!("create command pool: {error}"))?;
        // Sound: the pool is live.
        let cmd = unsafe {
            vk.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|error| format!("allocate command buffer: {error}"))?
        .first()
        .copied()
        .ok_or_else(|| "no command buffer".to_owned())?;
        // Sound: the create info stays live for the call.
        let fence = unsafe { vk.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|error| format!("create fence: {error}"))?;
        Ok(Self {
            device,
            pool,
            cmd,
            fence,
        })
    }

    /// Chroma of `frame` cleared to neutral grey.
    fn apply(&self, frame: &VulkanFrame) -> Result<(), String> {
        let planes = frame
            .planes()
            .ok_or_else(|| "readback frame without planes".to_owned())?;
        let vk = self.device.vk_device();
        // Sound: the fence and buffer are idle past the wait below.
        unsafe {
            vk.reset_fences(std::slice::from_ref(&self.fence))
                .map_err(|error| format!("reset fence: {error}"))?;
            vk.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())
                .map_err(|error| format!("reset command buffer: {error}"))?;
            vk.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(|error| format!("begin command buffer: {error}"))?;
            let barrier = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(planes.uv_image())
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            vk.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                std::slice::from_ref(&barrier),
            );
            vk.end_command_buffer(self.cmd)
                .map_err(|error| format!("end command buffer: {error}"))?;
        }
        {
            let _guard = self
                .device
                .queue_lock()
                .lock()
                .map_err(|_| "queue lock poisoned".to_owned())?;
            // Sound: the buffer closed above; the queue and fence are live.
            unsafe {
                vk.queue_submit(
                    self.device.queue(),
                    &[vk::SubmitInfo::default().command_buffers(std::slice::from_ref(&self.cmd))],
                    self.fence,
                )
                .map_err(|error| format!("queue submit: {error}"))?;
            }
        }
        // Sound: the fence is live with a bounded wait.
        unsafe { vk.wait_for_fences(std::slice::from_ref(&self.fence), true, WAIT_NS) }
            .map_err(|error| format!("fence wait: {error}"))?;
        Ok(())
    }
}

impl Drop for Clear<'_> {
    /// Destroys the fence and pool past the last wait.
    fn drop(&mut self) {
        let vk = self.device.vk_device();
        // Sound: every submit waited its fence; nothing is in flight.
        unsafe {
            vk.destroy_fence(self.fence, None);
            vk.destroy_command_pool(self.pool, None);
        }
    }
}

/// Encode sink with a lazily opened session.
struct Sink<'a> {
    /// Encode device behind the session.
    device: &'a VulkanDevice,
    /// Live session plus encode name.
    inner: Option<(Encode, &'static str)>,
}

impl<'a> Sink<'a> {
    /// Sink over `device`, opened on the first frame.
    fn new(device: &'a VulkanDevice) -> Self {
        Self {
            device,
            inner: None,
        }
    }

    /// Coded bytes for filtered `frame` into `out`.
    fn encode(&mut self, frame: &VulkanFrame, out: &mut Vec<u8>) -> Result<(), String> {
        let size = frame.identity().size();
        let extent = (size.width(), size.height());
        let matches = match self.inner.as_ref() {
            Some((Encode::H264(encoder), _)) => encoder.extent() == extent,
            Some((Encode::H265(encoder), _)) => encoder.extent() == extent,
            None => false,
        };
        if !matches {
            self.inner = Some(self.open(extent)?);
        }
        let Some((encoder, _)) = self.inner.as_mut() else {
            return Err("no encode session".to_owned());
        };
        for packet in encoder.encode_export(frame)? {
            out.extend_from_slice(packet.data());
        }
        Ok(())
    }

    /// Session for `extent`, H.265 then H.264.
    fn open(&self, extent: (u32, u32)) -> Result<(Encode, &'static str), String> {
        if let Ok(encoder) = H265Encoder::open(self.device, extent.0, extent.1) {
            return Ok((Encode::H265(encoder), "h265"));
        }
        H264Encoder::open(self.device, extent.0, extent.1)
            .map(|encoder| (Encode::H264(encoder), "h264"))
            .map_err(err)
    }

    /// Drained bytes into `out`.
    fn flush(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        if let Some((encoder, _)) = self.inner.as_mut() {
            for packet in encoder.flush()? {
                out.extend_from_slice(packet.data());
            }
        }
        Ok(())
    }

    /// Dump extension for the live session.
    fn ext(&self) -> &'static str {
        match self.inner.as_ref().map(|(_, name)| *name) {
            Some("h265") => "h265",
            _ => "h264",
        }
    }

    /// Combo note for `codec` with the live encode name.
    fn note(&self, codec: Codec) -> String {
        let target = self.inner.as_ref().map_or("?", |(_, name)| *name);
        let suffix = if codec == Codec::Av1 {
            " (no av1 encoder)"
        } else {
            ""
        };
        format!("{}->{target} via vulkan video{suffix}", codec.name())
    }
}

/// Device negotiated for `op`, or the decline reason.
fn open_device(op: CodecOp) -> Result<VulkanDevice, String> {
    match VulkanDevice::open_or_skip(op) {
        Ok(device) => Ok(device),
        Err(VulkanError::BackendAbsent(reason)) => Err(reason.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Display string of a tier error.
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
