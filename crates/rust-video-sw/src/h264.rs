//! H.264 encode over `OpenH264`.
//!
//! Encode only: `OpenH264` decode diverges from the reference on B-frame
//! content (see `docs/sw-codecs.md`), so no decoder ships here.

use bytes::Bytes;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameType, RateControlMode};
use openh264::formats::YUVBuffer;

use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder as CapsEncoder;

use crate::error::SwError;

/// Realtime H.264 encoder.
pub struct H264Encoder {
    inner: Encoder,
    width: u32,
    height: u32,
}

impl H264Encoder {
    /// CBR encoder for `width`x`height` at `bitrate_bps` on `threads`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `OpenH264` rejects the configuration.
    pub fn open(width: u32, height: u32, bitrate_bps: u32, threads: u16) -> Result<Self, SwError> {
        if width == 0 || height == 0 {
            return Err(SwError::Init);
        }
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate_bps))
            .rate_control_mode(RateControlMode::Bitrate)
            .num_threads(threads);
        let api = openh264::OpenH264API::from_source();
        let inner = Encoder::with_api_config(api, config).map_err(|_| SwError::Init)?;
        Ok(Self {
            inner,
            width,
            height,
        })
    }
}

impl CapsEncoder for H264Encoder {
    type Error = SwError;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        SwError::check_yuv420p(frame, self.width, self.height)?;
        let packed = pack(frame)?;
        let source = YUVBuffer::from_vec(packed, self.width as usize, self.height as usize);
        let stream = self.inner.encode(&source).map_err(|_| SwError::Rejected)?;
        let mut coded = Packet::new(Bytes::from(stream.to_vec()));
        if matches!(stream.frame_type(), FrameType::IDR | FrameType::I) {
            coded = coded.mark_keyframe();
        }
        Ok(vec![coded])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        Ok(Vec::new())
    }
}

/// Tightly packed I420 bytes from a CPU frame.
///
/// # Arguments
///
/// * `frame` - source frame.
///
/// # Errors
///
/// Returns `Err` when a plane holds fewer bytes than its rows need.
fn pack(frame: &CpuFrame) -> Result<Vec<u8>, SwError> {
    let width = frame.size().width() as usize;
    let height = frame.size().height() as usize;
    let mut out = Vec::with_capacity(width * height * 3 / 2);
    for (plane, rows, cols) in [
        (&frame.planes()[0], height, width),
        (&frame.planes()[1], height / 2, width / 2),
        (&frame.planes()[2], height / 2, width / 2),
    ] {
        let stride = plane.stride() as usize;
        for row in 0..rows {
            let start = row.checked_mul(stride).ok_or(SwError::Rejected)?;
            let end = start.checked_add(cols).ok_or(SwError::Rejected)?;
            let cell = plane.data().get(start..end).ok_or(SwError::Rejected)?;
            out.extend_from_slice(cell);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use openh264::decoder::{DecodeOptions, Decoder, DecoderConfig, Flush};
    use openh264::formats::YUVSource;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::traits::Encoder as _;

    use super::H264Encoder;

    fn frame_64() -> CpuFrame {
        let y = Bytes::from(vec![0x80; 64 * 64]);
        let c = Bytes::from(vec![0x80; 32 * 32]);
        let planes = [
            Plane::new(y, 64).expect("plane"),
            Plane::new(c.clone(), 32).expect("plane"),
            Plane::new(c, 32).expect("plane"),
        ];
        let size = PictureSize::new(64, 64).expect("size");
        CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
    }

    fn frame_with_y(y_len: usize) -> CpuFrame {
        let y = Bytes::from(vec![0x80; y_len]);
        let c = Bytes::from(vec![0x80; 32 * 32]);
        let planes = [
            Plane::new(y, 64).expect("plane"),
            Plane::new(c.clone(), 32).expect("plane"),
            Plane::new(c, 32).expect("plane"),
        ];
        let size = PictureSize::new(64, 64).expect("size");
        CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
    }

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        let first = encoder.encode(&frame_64()).expect("encode");
        assert_eq!(first.len(), 1);
        assert!(first[0].is_keyframe());
        assert!(!first[0].data().is_empty());
        let second = encoder.encode(&frame_64()).expect("encode");
        assert!(!second[0].data().is_empty());
        assert!(encoder.flush().expect("flush").is_empty());
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        let tiny = Bytes::from(vec![0x80; 16 * 16]);
        let chroma = Bytes::from(vec![0x80; 8 * 8]);
        let planes = [
            Plane::new(tiny, 16).expect("plane"),
            Plane::new(chroma.clone(), 8).expect("plane"),
            Plane::new(chroma, 8).expect("plane"),
        ];
        let size = PictureSize::new(16, 16).expect("size");
        let frame = CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame");
        assert!(encoder.encode(&frame).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(H264Encoder::open(0, 64, 500_000, 1).is_err());
        assert!(H264Encoder::open(64, 0, 500_000, 1).is_err());
    }

    #[test]
    fn own_output_decodes_back() {
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        let api = openh264::OpenH264API::from_source();
        let mut decoder = Decoder::with_api_config(api, config).expect("decoder");
        let options = DecodeOptions::new().flush_after_decode(Flush::NoFlush);
        let out = decoder
            .decode_with_options(packets[0].data(), options)
            .expect("decode")
            .expect("frame");
        assert_eq!(out.dimensions(), (64, 64));
    }
}
