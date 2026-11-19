//! GOP encode behind the caps `Encoder` trait.

use crate::decoder::VulkanFrame;
use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::ladder::CodecOp;
use crate::session::encode::{EncodeCodec, EncodeSession, PictureStamp};

use bytes::Bytes;
use rust_video_caps::delivery::SubmitStats;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::{FallbackCause, GpuCaps, ShareMechanism};
use rust_video_core::planes;

/// H.264 frame-number modulus: `log2_max_frame_num_minus4` is 0.
const MAX_FRAME_NUM: u16 = 16;

/// IDR cadence of one encode session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Gop {
    /// Frames between IDR pictures, at least 1.
    interval: u32,
}

impl Gop {
    /// Cadence coding an IDR every `frames` pictures.
    ///
    /// `0` clamps to 1, coding every picture as IDR.
    #[must_use]
    pub fn interval(frames: u32) -> Self {
        Self {
            interval: frames.max(1),
        }
    }

    /// Cadence coding every picture as IDR.
    #[must_use]
    pub fn all_intra() -> Self {
        Self { interval: 1 }
    }

    /// Frames between IDR pictures.
    pub(crate) fn period(self) -> u32 {
        self.interval
    }

    /// True when picture `index` codes as IDR.
    fn is_idr(self, index: u64) -> bool {
        index.is_multiple_of(u64::from(self.interval))
    }
}

impl Default for Gop {
    /// Short low-latency GOP: an IDR every 30 pictures.
    fn default() -> Self {
        Self { interval: 30 }
    }
}

/// Shared GOP encode engine over one session.
struct Engine {
    /// Encode session behind the frames.
    session: EncodeSession,
    /// Next IDR id.
    idr_id: u16,
    /// Next picture order count.
    poc: i32,
    /// Picture order step per frame.
    poc_step: i32,
    /// Frame number of the last coded picture, H.264 only.
    frame_num: u16,
    /// Index of the next picture.
    index: u64,
    /// IDR cadence.
    gop: Gop,
    /// Tight NV12 packing scratch.
    tight: Vec<u8>,
    /// Coded bytes scratch.
    coded: Vec<u8>,
}

impl Engine {
    /// Engine over `device` for `op` at `width`x`height` with `gop` cadence.
    ///
    /// A session without P support codes all-intra whatever `gop` holds.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    fn open(
        device: &VulkanDevice,
        op: CodecOp,
        codec: EncodeCodec,
        width: u32,
        height: u32,
        poc_step: i32,
        gop: Gop,
    ) -> Result<Self, VulkanError> {
        if device.op() != op {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        let session = EncodeSession::open(device, codec, width, height, gop.period())?;
        let gop = if session.supports_p_frames() {
            gop
        } else {
            Gop::all_intra()
        };
        Ok(Self {
            session,
            idr_id: 0,
            poc: 0,
            poc_step,
            frame_num: 0,
            index: 0,
            gop,
            tight: Vec::new(),
            coded: Vec::new(),
        })
    }

    /// Coded extent in luma samples.
    fn extent(&self) -> (u32, u32) {
        self.session.extent()
    }

    /// Submit-core gauges of the engine.
    fn submit_stats(&self) -> SubmitStats {
        self.session.submit_stats()
    }

    /// Fail the next submit once, exercising the reset path.
    fn inject_submit_fault(&mut self) {
        self.session.inject_fault();
    }

    /// True when the session backs P pictures.
    fn supports_p_frames(&self) -> bool {
        self.session.supports_p_frames()
    }

    /// Share mechanism behind texture import, absent when import is not live.
    fn import_share(&self) -> Option<ShareMechanism> {
        self.session.import_share()
    }

    /// Packet coded from `frame`, keyframe on IDR pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `frame` is not NV12, misses the session
    /// extent, or the submit fails.
    fn encode_frame(&mut self, frame: &CpuFrame) -> Result<Packet, VulkanError> {
        let (width, height) = self.extent();
        if frame.format() != PixelFormat::Nv12 {
            return Err(VulkanError::UnsupportedInput {
                format: frame.format(),
            });
        }
        let size = frame.size();
        if size.width() != width || size.height() != height {
            return Err(VulkanError::OverCaps {
                what: "encode extent",
            });
        }
        pack_nv12(frame, &mut self.tight)?;
        let stamp = self.next_stamp();
        let keyframe = match self
            .session
            .encode_into(&self.tight, stamp, &mut self.coded)
        {
            Ok(keyframe) => keyframe,
            Err(error) => {
                self.reset_cadence();
                return Err(error);
            }
        };
        self.resync(stamp, keyframe);
        Ok(self.packet(keyframe))
    }

    /// Packet coded from the device export behind `frame`, keyframe on IDR.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `frame` has no device export, is not NV12,
    /// misses the session extent, or the submit fails.
    fn encode_export(&mut self, frame: &VulkanFrame) -> Result<Packet, VulkanError> {
        let Some(export) = frame.device_export() else {
            return Err(VulkanError::Unsupported {
                what: FallbackCause::NoSharedExport.to_string(),
            });
        };
        if frame.identity().format() != PixelFormat::Nv12 {
            return Err(VulkanError::UnsupportedInput {
                format: frame.identity().format(),
            });
        }
        let size = frame.identity().size();
        let (width, height) = self.extent();
        if size.width() != width || size.height() != height {
            return Err(VulkanError::OverCaps {
                what: "encode extent",
            });
        }
        let stamp = self.next_stamp();
        let keyframe = match self.session.encode_import(export, stamp, &mut self.coded) {
            Ok(keyframe) => keyframe,
            Err(error) => {
                self.reset_cadence();
                return Err(error);
            }
        };
        self.resync(stamp, keyframe);
        Ok(self.packet(keyframe))
    }

    /// Stamp for the next picture, advancing the counters.
    fn next_stamp(&mut self) -> PictureStamp {
        let is_idr = self.gop.is_idr(self.index);
        if is_idr {
            self.frame_num = 0;
        } else {
            self.frame_num = (self.frame_num + 1) % MAX_FRAME_NUM;
        }
        let stamp = PictureStamp::new(self.idr_id, self.poc, self.frame_num, is_idr);
        if is_idr {
            self.idr_id = self.idr_id.wrapping_add(1);
        }
        self.poc += self.poc_step;
        self.index += 1;
        stamp
    }

    /// Align counters when the session coded an IDR for a P stamp.
    fn resync(&mut self, stamp: PictureStamp, keyframe: bool) {
        if keyframe && !stamp.is_idr() {
            self.frame_num = 0;
            self.idr_id = self.idr_id.wrapping_add(1);
        }
    }

    /// Restart GOP cadence after a failed encode reset the session.
    fn reset_cadence(&mut self) {
        self.idr_id = 0;
        self.poc = 0;
        self.frame_num = 0;
        self.index = 0;
    }

    /// Packet over the coded bytes, headers ahead of keyframes only.
    fn packet(&self, keyframe: bool) -> Packet {
        let headers = if keyframe {
            self.session.headers()
        } else {
            &[]
        };
        let mut bytes = Vec::with_capacity(headers.len() + self.coded.len());
        bytes.extend_from_slice(headers);
        bytes.extend_from_slice(&self.coded);
        let packet = Packet::new(Bytes::from(bytes));
        if keyframe {
            packet.mark_keyframe()
        } else {
            packet
        }
    }
}

/// Tight NV12 rows of `frame` into `out`, honoring plane strides.
///
/// # Errors
///
/// Returns `Err` when a plane is shorter than its strided rows.
fn pack_nv12(frame: &CpuFrame, out: &mut Vec<u8>) -> Result<(), VulkanError> {
    let planes = frame.planes();
    if planes.len() != 2 {
        return Err(VulkanError::UnsupportedInput {
            format: frame.format(),
        });
    }
    let width = frame.size().width() as usize;
    let height = frame.size().height() as usize;
    out.clear();
    out.reserve(width * height * 3 / 2);
    pack_plane(&planes[0], width, height, out)?;
    pack_plane(&planes[1], width, height / 2, out)?;
    Ok(())
}

/// Tight `rows` of `width` bytes from the strided `plane` into `out`.
///
/// # Errors
///
/// Returns `Err` when the plane is shorter than its strided rows.
fn pack_plane(
    plane: &rust_video_caps::frame::Plane,
    width: usize,
    rows: usize,
    out: &mut Vec<u8>,
) -> Result<(), VulkanError> {
    let width_32 = u32::try_from(width).unwrap_or(u32::MAX);
    let rows_32 = u32::try_from(rows).unwrap_or(u32::MAX);
    planes::check_plane(plane.data(), plane.stride(), width_32, rows_32).map_err(|fault| {
        match fault {
            planes::PlaneFault::StrideBelowWidth { stride, width } => VulkanError::ShortReadback {
                expected: usize::try_from(width).unwrap_or(usize::MAX),
                found: usize::try_from(stride).unwrap_or(usize::MAX),
            },
            planes::PlaneFault::ShortOfStride { need, len } => VulkanError::ShortReadback {
                expected: usize::try_from(need).unwrap_or(usize::MAX),
                found: len,
            },
        }
    })?;
    planes::copy_rows_extend(plane.data(), plane.stride(), width_32, rows_32, out);
    Ok(())
}

/// H.264 GOP encoder over a negotiated Vulkan device.
///
/// Thread-local: the session submits on its creating thread while
/// queue access takes the device lock.
pub struct H264Encoder {
    /// Shared encode engine.
    engine: Engine,
}

impl H264Encoder {
    /// Encoder over `device` at `width`x`height`, negotiated for H.264 encode.
    ///
    /// Codes the default GOP cadence.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    pub fn open(device: &VulkanDevice, width: u32, height: u32) -> Result<Self, VulkanError> {
        Self::open_with_gop(device, width, height, Gop::default())
    }

    /// Encoder over `device` at `width`x`height` with `gop` cadence.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    pub fn open_with_gop(
        device: &VulkanDevice,
        width: u32,
        height: u32,
        gop: Gop,
    ) -> Result<Self, VulkanError> {
        Ok(Self {
            engine: Engine::open(
                device,
                CodecOp::H264Encode,
                EncodeCodec::H264,
                width,
                height,
                2,
                gop,
            )?,
        })
    }

    /// Coded extent in luma samples.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        self.engine.extent()
    }

    /// True when the session backs P pictures.
    #[must_use]
    pub fn supports_p_frames(&self) -> bool {
        self.engine.supports_p_frames()
    }

    /// Submit-core gauges of the encoder.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        self.engine.submit_stats()
    }

    /// Fail the next submit once, exercising the reset path.
    pub fn inject_submit_fault(&mut self) {
        self.engine.inject_submit_fault();
    }

    /// Packets coded from the device export behind `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame has no device export, misses the
    /// session size, or the backend refuses the picture.
    pub fn encode_export(&mut self, frame: &VulkanFrame) -> Result<Vec<Packet>, VulkanError> {
        Ok(vec![self.engine.encode_export(frame)?])
    }
}

impl Encoder for H264Encoder {
    type Error = VulkanError;

    fn caps(&self) -> GpuCaps {
        let share = self.engine.import_share();
        GpuCaps::new(share, false, share.is_some())
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        Ok(vec![self.engine.encode_frame(frame)?])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        Ok(Vec::new())
    }
}

/// H.265 GOP encoder over a negotiated Vulkan device.
///
/// Thread-local: the session submits on its creating thread while
/// queue access takes the device lock.
pub struct H265Encoder {
    /// Shared encode engine.
    engine: Engine,
}

impl H265Encoder {
    /// Encoder over `device` at `width`x`height`, negotiated for H.265 encode.
    ///
    /// Codes the default GOP cadence.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    pub fn open(device: &VulkanDevice, width: u32, height: u32) -> Result<Self, VulkanError> {
        Self::open_with_gop(device, width, height, Gop::default())
    }

    /// Encoder over `device` at `width`x`height` with `gop` cadence.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    pub fn open_with_gop(
        device: &VulkanDevice,
        width: u32,
        height: u32,
        gop: Gop,
    ) -> Result<Self, VulkanError> {
        Ok(Self {
            engine: Engine::open(
                device,
                CodecOp::H265Encode,
                EncodeCodec::H265,
                width,
                height,
                1,
                gop,
            )?,
        })
    }

    /// Coded extent in luma samples.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        self.engine.extent()
    }

    /// True when the session backs P pictures.
    #[must_use]
    pub fn supports_p_frames(&self) -> bool {
        self.engine.supports_p_frames()
    }

    /// Submit-core gauges of the encoder.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        self.engine.submit_stats()
    }

    /// Fail the next submit once, exercising the reset path.
    pub fn inject_submit_fault(&mut self) {
        self.engine.inject_submit_fault();
    }

    /// Packets coded from the device export behind `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame has no device export, misses the
    /// session size, or the backend refuses the picture.
    pub fn encode_export(&mut self, frame: &VulkanFrame) -> Result<Vec<Packet>, VulkanError> {
        Ok(vec![self.engine.encode_export(frame)?])
    }
}

impl Encoder for H265Encoder {
    type Error = VulkanError;

    fn caps(&self) -> GpuCaps {
        let share = self.engine.import_share();
        GpuCaps::new(share, false, share.is_some())
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        Ok(vec![self.engine.encode_frame(frame)?])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

    /// NV12 frame with `stride` over `y` and `uv` rows.
    fn nv12(width: u32, height: u32, stride: u32, y: &[u8], uv: &[u8]) -> CpuFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::copy_from_slice(y), stride).expect("stride"),
                Plane::new(Bytes::copy_from_slice(uv), stride).expect("stride"),
            ],
        )
        .expect("two planes")
    }

    #[test]
    fn gop_idr_cadence_exact() {
        let gop = super::Gop::interval(4);
        let marks: Vec<bool> = (0..10).map(|index| gop.is_idr(index)).collect();
        assert_eq!(
            marks,
            vec![true, false, false, false, true, false, false, false, true, false]
        );
    }

    #[test]
    fn gop_all_intra_and_default() {
        let intra = super::Gop::all_intra();
        assert!((0..4).all(|index| intra.is_idr(index)));
        let clamped = super::Gop::interval(0);
        assert!((0..4).all(|index| clamped.is_idr(index)));
        let default = super::Gop::default();
        assert!(default.is_idr(0));
        assert!(default.is_idr(30));
        assert!(!default.is_idr(29));
    }

    #[test]
    fn pack_copies_visible_bytes_only() {
        let frame = nv12(2, 2, 4, &[1, 2, 0, 0, 3, 4, 0, 0], &[5, 6, 0, 0]);
        let mut out = Vec::new();
        super::pack_nv12(&frame, &mut out).expect("in bounds");
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn pack_copies_tight_stride_whole() {
        let frame = nv12(2, 2, 2, &[1, 2, 3, 4], &[5, 6]);
        let mut out = Vec::new();
        super::pack_nv12(&frame, &mut out).expect("in bounds");
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn pack_rejects_short_plane() {
        let frame = nv12(2, 2, 4, &[1, 2], &[5, 6, 0, 0]);
        let mut out = Vec::new();
        assert!(matches!(
            super::pack_nv12(&frame, &mut out),
            Err(super::VulkanError::ShortReadback { .. })
        ));
    }

    #[test]
    fn pack_rejects_narrow_stride() {
        let size = PictureSize::new(4, 2).expect("nonzero extent");
        let frame = CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from_static(&[0; 8]), 2).expect("stride"),
                Plane::new(Bytes::from_static(&[0; 4]), 2).expect("stride"),
            ],
        )
        .expect("two planes");
        let mut out = Vec::new();
        assert!(matches!(
            super::pack_nv12(&frame, &mut out),
            Err(super::VulkanError::ShortReadback { .. })
        ));
    }

    #[test]
    fn caps_agree_with_import_liveness() {
        use rust_video_caps::traits::Encoder;
        #[cfg(not(unix))]
        use rust_video_caps::transcode::GpuCaps;

        use crate::device::VulkanDevice;
        use crate::error::VulkanError;
        use crate::ladder::CodecOp;

        let device = match VulkanDevice::open_or_skip(CodecOp::H264Encode) {
            Ok(device) => device,
            Err(VulkanError::BackendAbsent(reason)) => {
                println!("declined: h264 encode on this box: {reason}");
                return;
            }
            Err(error) => panic!("negotiation failed: {error}"),
        };
        let encoder = match super::H264Encoder::open(&device, 320, 240) {
            Ok(encoder) => encoder,
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM h264 encode open: {error}");
                return;
            }
            Err(error) => panic!("engine starts: {error}"),
        };
        let caps = encoder.caps();
        assert_eq!(caps.share().is_some(), caps.encode_import());
        assert!(!caps.decode_export());
        assert_eq!(caps.encode_import(), device.surface_import());
        assert_eq!(
            caps.share(),
            device.share_mechanism().filter(|_| device.surface_import())
        );
        #[cfg(not(unix))]
        assert_eq!(caps, GpuCaps::new(None, false, false));
    }
}
