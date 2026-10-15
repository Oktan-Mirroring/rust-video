//! Backend failure vocabulary.

use std::fmt;

#[cfg(any(feature = "ffmpeg", feature = "gpl", feature = "h264-openh264"))]
use rust_video_caps::frame::{CpuFrame, PixelFormat};

/// Software backend failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SwError {
    /// Backend rejected its configuration.
    Init,
    /// Backend rejected one input unit.
    Rejected,
    /// Backend returned a frame the facade cannot hold.
    BadFrame,
    /// Backend call failed with its native status code.
    Native {
        /// Native backend status code.
        code: i64,
    },
}

impl SwError {
    /// Rejection unless `frame` matches `width`x`height` YUV420P planes.
    ///
    /// # Arguments
    ///
    /// * `frame` - candidate frame.
    /// * `width` - expected coded width.
    /// * `height` - expected coded height.
    ///
    /// # Errors
    ///
    /// Returns `Err` when format, size, stride, or plane length mismatches.
    #[cfg(any(feature = "ffmpeg", feature = "gpl", feature = "h264-openh264"))]
    pub(crate) fn check_yuv420p(frame: &CpuFrame, width: u32, height: u32) -> Result<(), Self> {
        if frame.format() != PixelFormat::Yuv420P
            || frame.size().width() != width
            || frame.size().height() != height
        {
            return Err(Self::Rejected);
        }
        if frame.planes().len() != PixelFormat::Yuv420P.plane_count() {
            return Err(Self::Rejected);
        }
        let width = u64::from(width);
        let height = u64::from(height);
        for (plane, shift) in frame.planes().iter().zip([0, 1, 1]) {
            let stride = u64::from(plane.stride());
            let rows = height >> shift;
            let cols = width >> shift;
            let len = u64::try_from(plane.data().len()).map_err(|_| Self::Rejected)?;
            if stride < cols || len != stride * rows {
                return Err(Self::Rejected);
            }
        }
        Ok(())
    }
}

impl fmt::Display for SwError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init => write!(f, "backend init failed"),
            Self::Rejected => write!(f, "backend rejected input"),
            Self::BadFrame => write!(f, "backend returned unusable frame"),
            Self::Native { code } => write!(f, "backend call failed: {code}"),
        }
    }
}

impl std::error::Error for SwError {}
