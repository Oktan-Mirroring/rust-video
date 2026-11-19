//! H.264 encode over `OpenH264`.
//!
//! Encode only: `OpenH264` decode diverges from the reference on B-frame
//! content (see `docs/sw-codecs.md`), so no decoder ships here.

use crate::error::SwError;

use bytes::Bytes;
use openh264::encoder::{
    BitRate, Complexity, EncodedBitStream, Encoder, EncoderConfig, FrameRate, FrameType,
    IntraFramePeriod, RateControlMode,
};
use openh264::formats::YUVSlices;
use openh264::Timestamp;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder as CapsEncoder;
use rust_video_caps::transcode::GpuCaps;

/// Slice byte budget when multi-threaded; engages worker threads without tiny slice fragmentation.
const MULTI_SLICE_BYTES: u32 = 16_384;

/// Nominal realtime frame rate in Hz for CBR rate control.
const TARGET_FPS: f32 = 60.0;

/// Keyframe cadence in pictures.
const GOP_FRAMES: u32 = 60;

/// Realtime H.264 encoder.
pub struct H264Encoder {
    /// `OpenH264` encoder instance.
    inner: Encoder,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Encoded frame counter driving monotonically increasing timestamps.
    frame_index: u64,
}

impl H264Encoder {
    /// CBR encoder for `width`x`height` at `bitrate_bps` on `threads`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the geometry is zero or odd, or `OpenH264`
    /// rejects the configuration.
    pub fn open(width: u32, height: u32, bitrate_bps: u32, threads: u16) -> Result<Self, SwError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(SwError::Init);
        }
        let mut config = EncoderConfig::new()
            .debug(false)
            .bitrate(BitRate::from_bps(bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(TARGET_FPS))
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(GOP_FRAMES))
            .num_threads(threads)
            .complexity(Complexity::Low)
            .scene_change_detect(false)
            .adaptive_quantization(false)
            .background_detection(false);
        if threads > 1 {
            config = config.max_slice_len(MULTI_SLICE_BYTES);
        }
        let api = openh264::OpenH264API::from_source();
        let inner = Encoder::with_api_config(api, config).map_err(|error| SwError::Native {
            code: error.native_code(),
        })?;
        Ok(Self {
            inner,
            width,
            height,
            frame_index: 0,
        })
    }

    /// Borrowed 4:2:0 view over a frame.
    ///
    /// # Performance
    ///
    /// Zero copies.
    ///
    /// # Arguments
    ///
    /// * `frame` - YUV420P frame; each plane holds its rows times its stride.
    fn view(frame: &CpuFrame) -> YUVSlices<'_> {
        let planes = frame.planes();
        YUVSlices::new(
            (planes[0].data(), planes[1].data(), planes[2].data()),
            (
                frame.size().width() as usize,
                frame.size().height() as usize,
            ),
            (
                planes[0].stride() as usize,
                planes[1].stride() as usize,
                planes[2].stride() as usize,
            ),
        )
    }

    /// Packet coded from an encoded bitstream.
    ///
    /// # Performance
    ///
    /// One copy.
    ///
    /// # Arguments
    ///
    /// * `stream` - encoded bitstream.
    fn packet(stream: &EncodedBitStream) -> Packet {
        let mut coded = Packet::new(Bytes::from(stream.to_vec()));
        if matches!(stream.frame_type(), FrameType::IDR | FrameType::I) {
            coded = coded.mark_keyframe();
        }
        coded
    }
}

impl CapsEncoder for H264Encoder {
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        SwError::check_yuv420p(frame, self.width, self.height)?;
        let source = Self::view(frame);
        let ts = Timestamp::from_millis(self.frame_index.saturating_mul(1_000) / 60);
        self.frame_index = self.frame_index.saturating_add(1);
        let stream = self
            .inner
            .encode_at(&source, ts)
            .map_err(|error| SwError::Native {
                code: error.native_code(),
            })?;
        Ok(vec![Self::packet(&stream)])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        Ok(Vec::new())
    }
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
    use crate::fixtures::{frame_16, frame_64, frame_with_y};

    fn assert_send_sync<T: Send + Sync>() {}

    fn strided_64() -> CpuFrame {
        let mut y = vec![0u8; 64 * 80];
        for row in y.chunks_exact_mut(80) {
            row[..64].fill(0x80);
        }
        let mut chroma = vec![0u8; 32 * 40];
        for row in chroma.chunks_exact_mut(40) {
            row[..32].fill(0x80);
        }
        let chroma = Bytes::from(chroma);
        let planes = [
            Plane::new(Bytes::from(y), 80).expect("plane"),
            Plane::new(chroma.clone(), 40).expect("plane"),
            Plane::new(chroma, 40).expect("plane"),
        ];
        let size = PictureSize::new(64, 64).expect("size");
        CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
    }

    #[test]
    fn encoder_is_send_sync() {
        assert_send_sync::<H264Encoder>();
    }

    #[test]
    fn encoder_caps_report_no_share() {
        use rust_video_caps::transcode::GpuCaps;

        let encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        assert_eq!(encoder.caps(), GpuCaps::new(None, false, false));
    }

    #[test]
    fn strided_input_matches_tight_input_bytes() {
        let mut tight = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        let mut strided = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        let tight_packets = tight.encode(&frame_64()).expect("encode");
        let strided_packets = strided.encode(&strided_64()).expect("encode");
        assert_eq!(tight_packets, strided_packets);
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
        assert_ne!(first[0].data(), []);
        let second = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(second[0].data(), []);
        assert_eq!(encoder.flush().expect("flush"), []);
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(H264Encoder::open(0, 64, 500_000, 1).is_err());
        assert!(H264Encoder::open(64, 0, 500_000, 1).is_err());
    }

    #[test]
    fn encode_rejects_odd_geometry() {
        assert!(H264Encoder::open(63, 64, 500_000, 1).is_err());
        assert!(H264Encoder::open(64, 63, 500_000, 1).is_err());
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
