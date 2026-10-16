//! IDR encode behind the caps `Encoder` trait.

use bytes::Bytes;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::FallbackCause;

use crate::decoder::VulkanFrame;
use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::ladder::CodecOp;
use crate::session::encode::{EncodeCodec, EncodeSession, IdrStamp};

/// Shared IDR encode engine over one session.
struct Engine {
    /// Encode session behind the frames.
    session: EncodeSession,
    /// Next IDR id.
    idr_id: u16,
    /// Next picture order count.
    poc: i32,
    /// Picture order step per frame.
    poc_step: i32,
    /// Tight NV12 packing scratch.
    tight: Vec<u8>,
    /// Coded bytes scratch.
    coded: Vec<u8>,
}

impl Engine {
    /// Engine over `device` for `op` at `width`x`height`.
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
    ) -> Result<Self, VulkanError> {
        if device.op() != op {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        Ok(Self {
            session: EncodeSession::open(device, codec, width, height)?,
            idr_id: 0,
            poc: 0,
            poc_step,
            tight: Vec::new(),
            coded: Vec::new(),
        })
    }

    /// Coded extent in luma samples.
    fn extent(&self) -> (u32, u32) {
        self.session.extent()
    }

    /// Keyframe packet coded from `frame`.
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
        self.session
            .encode_into(&self.tight, stamp, &mut self.coded)?;
        Ok(self.packet())
    }

    /// Keyframe packet coded from the device export behind `frame`.
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
        self.session.encode_import(export, stamp, &mut self.coded)?;
        Ok(self.packet())
    }

    /// Stamp for the next IDR, advancing the counters.
    fn next_stamp(&mut self) -> IdrStamp {
        let stamp = IdrStamp::new(self.idr_id, self.poc);
        self.idr_id = self.idr_id.wrapping_add(1);
        self.poc += self.poc_step;
        stamp
    }

    /// Keyframe packet over the coded bytes plus session headers.
    fn packet(&self) -> Packet {
        let headers = self.session.headers();
        let mut bytes = Vec::with_capacity(headers.len() + self.coded.len());
        bytes.extend_from_slice(headers);
        bytes.extend_from_slice(&self.coded);
        Packet::new(Bytes::from(bytes)).mark_keyframe()
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
    let stride = plane.stride() as usize;
    let data = plane.data();
    if stride < width {
        return Err(VulkanError::ShortReadback {
            expected: width,
            found: stride,
        });
    }
    let want = stride * rows.saturating_sub(1) + width;
    if data.len() < want {
        return Err(VulkanError::ShortReadback {
            expected: want,
            found: data.len(),
        });
    }
    for row in 0..rows {
        let start = row * stride;
        out.extend_from_slice(&data[start..start + width]);
    }
    Ok(())
}

/// H.264 IDR encoder over a negotiated Vulkan device.
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
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    pub fn open(device: &VulkanDevice, width: u32, height: u32) -> Result<Self, VulkanError> {
        Ok(Self {
            engine: Engine::open(
                device,
                CodecOp::H264Encode,
                EncodeCodec::H264,
                width,
                height,
                2,
            )?,
        })
    }

    /// Coded extent in luma samples.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        self.engine.extent()
    }

    /// Packets coded from the device export behind `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame has no device export, misses the
    /// session size, or the backend refuses the picture.
    pub(crate) fn encode_export(
        &mut self,
        frame: &VulkanFrame,
    ) -> Result<Vec<Packet>, VulkanError> {
        Ok(vec![self.engine.encode_export(frame)?])
    }
}

impl Encoder for H264Encoder {
    type Error = VulkanError;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        Ok(vec![self.engine.encode_frame(frame)?])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        Ok(Vec::new())
    }
}

/// H.265 IDR encoder over a negotiated Vulkan device.
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
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated another operation or the
    /// session fails to open.
    pub fn open(device: &VulkanDevice, width: u32, height: u32) -> Result<Self, VulkanError> {
        Ok(Self {
            engine: Engine::open(
                device,
                CodecOp::H265Encode,
                EncodeCodec::H265,
                width,
                height,
                1,
            )?,
        })
    }

    /// Coded extent in luma samples.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        self.engine.extent()
    }

    /// Packets coded from the device export behind `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame has no device export, misses the
    /// session size, or the backend refuses the picture.
    pub(crate) fn encode_export(
        &mut self,
        frame: &VulkanFrame,
    ) -> Result<Vec<Packet>, VulkanError> {
        Ok(vec![self.engine.encode_export(frame)?])
    }
}

impl Encoder for H265Encoder {
    type Error = VulkanError;

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
    fn pack_copies_visible_bytes_only() {
        let frame = nv12(2, 2, 4, &[1, 2, 0, 0, 3, 4, 0, 0], &[5, 6, 0, 0]);
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
}
