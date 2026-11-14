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
    fn gradient_bars_row_zero_at_index_zero() {
        let size = PictureSize::new(4, 2).expect("valid size");
        let frame = Synth::gradient(size, 0);
        let planes = frame.planes();
        assert_eq!(planes[0].data(), &[128, 129, 130, 131, 3, 4, 5, 6]);
        assert_eq!(planes[1].data(), &[128; 4]);
    }
}
