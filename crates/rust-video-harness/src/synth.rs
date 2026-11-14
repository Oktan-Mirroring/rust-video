use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

/// Synthetic NV12 frames for tier targets.
pub struct Synth;

impl Synth {
    /// Mid-grey NV12 frame of `width` x `height`.
    ///
    /// # Panics
    ///
    /// Panics when `width` or `height` is zero.
    #[must_use]
    pub fn grey_nv12(width: u32, height: u32) -> CpuFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let y_len = width as usize * height as usize;
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(vec![0x80; y_len]), width).expect("luma"),
                Plane::new(Bytes::from(vec![0x80; y_len / 2]), width).expect("chroma"),
            ],
        )
        .expect("frame")
    }

    /// Deterministic noise NV12 frame of `width` x `height` from `seed`.
    ///
    /// # Panics
    ///
    /// Panics when `width` or `height` is zero.
    #[must_use]
    pub fn noise(width: u32, height: u32, seed: u64) -> CpuFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let y_len = width as usize * height as usize;
        let mut rng = XorShift::new(seed);
        let mut luma = vec![0_u8; y_len];
        rng.fill(&mut luma);
        let mut chroma = vec![0_u8; y_len / 2];
        rng.fill(&mut chroma);
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(luma), width).expect("luma"),
                Plane::new(Bytes::from(chroma), width).expect("chroma"),
            ],
        )
        .expect("frame")
    }

    /// Black planar frame of `width` x `height`.
    ///
    /// # Panics
    ///
    /// Panics when `width` or `height` is zero.
    #[must_use]
    pub fn black_yuv420p(width: u32, height: u32) -> CpuFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let luma_len = usize::try_from(width).unwrap_or(0) * usize::try_from(height).unwrap_or(0);
        let chroma_len = luma_len / 4;
        let chroma_stride = width / 2;
        CpuFrame::new(
            size,
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![0_u8; luma_len]), width).expect("valid luma"),
                Plane::new(Bytes::from(vec![128_u8; chroma_len]), chroma_stride)
                    .expect("valid chroma"),
                Plane::new(Bytes::from(vec![128_u8; chroma_len]), chroma_stride)
                    .expect("valid chroma"),
            ],
        )
        .expect("three planes")
    }

    /// Gradient frame with a moving bar at `index`.
    ///
    /// # Panics
    ///
    /// Panics when the coded planes fail layout checks.
    #[must_use]
    pub fn gradient(size: PictureSize, index: u32) -> CpuFrame {
        let width = usize::try_from(size.width()).unwrap_or(0);
        let height = usize::try_from(size.height()).unwrap_or(0);
        let mut luma = vec![0u8; width * height];
        let bar = if height == 0 {
            0
        } else {
            (index as usize * 8) % height
        };
        for y in 0..height {
            for x in 0..width {
                let value = x + y * 3 + index as usize * 5 + if y == bar { 128 } else { 0 };
                luma[y * width + x] = u8::try_from(value % 256).unwrap_or(0);
            }
        }
        let chroma = vec![128u8; width * height / 2];
        let stride = size.width();
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(luma), stride).expect("luma plane"),
                Plane::new(Bytes::from(chroma), stride).expect("chroma plane"),
            ],
        )
        .expect("synth frame")
    }
}

/// Deterministic xorshift64* byte stream.
struct XorShift {
    /// Nonzero generator state.
    state: u64,
}

impl XorShift {
    /// Stream from `seed`, forced off the zero lock.
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    /// Next `u64` in the stream.
    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Fill `bytes` from the stream, eight at a time.
    fn fill(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            let len = chunk.len();
            chunk.copy_from_slice(&word[..len]);
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::PixelFormat;

    use super::Synth;

    #[test]
    fn grey_nv12_fills_mid_grey() {
        let frame = Synth::grey_nv12(4, 2);
        assert_eq!(frame.format(), PixelFormat::Nv12);
        let planes = frame.planes();
        assert_eq!(planes.len(), 2);
        assert_eq!(planes[0].data(), &[0x80; 8]);
        assert_eq!(planes[0].stride(), 4);
        assert_eq!(planes[1].data(), &[0x80; 4]);
        assert_eq!(planes[1].stride(), 4);
    }

    #[test]
    fn black_yuv420p_fills_black() {
        let frame = Synth::black_yuv420p(4, 2);
        assert_eq!(frame.format(), PixelFormat::Yuv420P);
        let planes = frame.planes();
        assert_eq!(planes.len(), 3);
        assert_eq!(planes[0].data(), &[0; 8]);
        assert_eq!(planes[0].stride(), 4);
        assert_eq!(planes[1].data(), &[128; 2]);
        assert_eq!(planes[1].stride(), 2);
        assert_eq!(planes[2].data(), &[128; 2]);
        assert_eq!(planes[2].stride(), 2);
    }

    #[test]
    fn noise_is_deterministic_per_seed() {
        let first = Synth::noise(16, 16, 7);
        let second = Synth::noise(16, 16, 7);
        let other = Synth::noise(16, 16, 8);
        assert_eq!(first.planes()[0].data(), second.planes()[0].data());
        assert_eq!(first.planes()[1].data(), second.planes()[1].data());
        assert_ne!(first.planes()[0].data(), other.planes()[0].data());
        assert_eq!(first.format(), PixelFormat::Nv12);
        assert_eq!(first.planes()[0].data().len(), 256);
        assert_eq!(first.planes()[1].data().len(), 128);
    }

    #[test]
    fn gradient_bars_row_zero_at_index_zero() {
        let size = PictureSize::new(4, 2).expect("valid size");
        let frame = Synth::gradient(size, 0);
        let planes = frame.planes();
        assert_eq!(planes[0].data(), &[128, 129, 130, 131, 3, 4, 5, 6]);
        assert_eq!(planes[1].data(), &[128; 4]);
    }
}
