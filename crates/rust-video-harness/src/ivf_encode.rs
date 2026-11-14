use std::{fs, path::Path, time::Instant};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;

/// IVF-wrapped throughput probe over an owned encoder.
pub struct IvfEncode<E> {
    /// Encoder under probe.
    encoder: E,
    /// Log tag.
    tag: &'static str,
    /// IVF codec fourcc.
    fourcc: [u8; 4],
}

impl<E: Encoder> IvfEncode<E> {
    /// Probe over `encoder`, logging as `tag` with IVF `fourcc`.
    #[must_use]
    pub fn new(encoder: E, tag: &'static str, fourcc: [u8; 4]) -> Self {
        Self {
            encoder,
            tag,
            fourcc,
        }
    }

    /// Encode `frames` I420 frames from `raw` at `width`x`height` into IVF `out`.
    ///
    /// # Panics
    ///
    /// Panics on short input, encode failure, or an unwritable `out`.
    pub fn run(&mut self, raw: &Path, width: u32, height: u32, frames: usize, out: &Path) {
        let bytes = fs::read(raw).expect("raw readable");
        let size = width as usize * height as usize * 3 / 2;
        assert!(bytes.len() >= size * frames, "raw holds frames");
        let mut coded = Self::ivf_header(self.fourcc, width, height);
        let picture = PictureSize::new(width, height).expect("size");
        let start = Instant::now();
        for (n, chunk) in bytes.chunks_exact(size).take(frames).enumerate() {
            let frame = Self::split_frame(chunk, picture);
            for packet in self.encoder.encode(&frame).expect("encode") {
                Self::push_packet(&mut coded, &packet, n as u64);
            }
        }
        for packet in self.encoder.flush().expect("flush") {
            Self::push_packet(&mut coded, &packet, frames as u64);
        }
        let secs = start.elapsed().as_secs_f64();
        fs::write(out, &coded).expect("bitstream written");
        let tag = self.tag;
        println!(
            "{tag}: {frames} frames in {secs:.2}s = {:.1} fps, {} bytes",
            Self::fps(frames, secs),
            coded.len()
        );
    }

    /// CPU frame borrowing one packed I420 `chunk`.
    fn split_frame(chunk: &[u8], picture: PictureSize) -> CpuFrame {
        let width = picture.width();
        let pitch = width as usize * picture.height() as usize;
        let luma = &chunk[0..pitch];
        let chroma_u = &chunk[pitch..pitch + pitch / 4];
        let chroma_v = &chunk[pitch + pitch / 4..pitch * 3 / 2];
        let planes = [
            Plane::new(Bytes::copy_from_slice(luma), width).expect("plane"),
            Plane::new(Bytes::copy_from_slice(chroma_u), width / 2).expect("plane"),
            Plane::new(Bytes::copy_from_slice(chroma_v), width / 2).expect("plane"),
        ];
        CpuFrame::new(picture, PixelFormat::Yuv420P, planes.into()).expect("frame")
    }

    /// One IVF frame from a coded packet.
    fn push_packet(coded: &mut Vec<u8>, packet: &Packet, pts: u64) {
        let size = u32::try_from(packet.data().len()).expect("probe packet size");
        coded.extend_from_slice(&size.to_le_bytes());
        coded.extend_from_slice(&pts.to_le_bytes());
        coded.extend_from_slice(packet.data());
    }

    /// 32-byte IVF header for `fourcc` at `width`x`height`.
    fn ivf_header(fourcc: [u8; 4], width: u32, height: u32) -> Vec<u8> {
        let narrow = u16::try_from(width).expect("probe width");
        let short = u16::try_from(height).expect("probe height");
        let mut out = Vec::with_capacity(32);
        out.extend_from_slice(b"DKIF");
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&fourcc);
        out.extend_from_slice(&narrow.to_le_bytes());
        out.extend_from_slice(&short.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&60u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    /// Frames per second from a frame count and elapsed seconds.
    fn fps(frames: usize, secs: f64) -> f64 {
        let count = u32::try_from(frames).expect("probe frame count");
        f64::from(count) / secs
    }
}
