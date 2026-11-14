//! D3D11 hardware encode over `MediaFoundation` encoder MFTs.

#[cfg(target_os = "windows")]
use crate::decoder::GpuSurface;
#[cfg(target_os = "windows")]
use crate::device::D3d11Device;
#[cfg(target_os = "windows")]
use crate::error::{D3d11Error, UnsupportedReason};
#[cfg(target_os = "windows")]
use crate::guid::Codec;
#[cfg(target_os = "windows")]
use crate::mf::Session;
#[cfg(target_os = "windows")]
use crate::scale::ScaledSurface;

#[cfg(target_os = "windows")]
use rust_video_caps::frame::{CpuFrame, PixelFormat};
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::Encoder;

/// D3D11 hardware encoder over a `MediaFoundation` encoder MFT.
///
/// Encoders run 30 fps progressive with an area-scaled bitrate; input is NV12.
#[cfg(target_os = "windows")]
pub struct D3d11Encoder {
    /// Negotiated MF session.
    session: Session,
    /// Codec the session encodes.
    codec: Codec,
}

#[cfg(target_os = "windows")]
impl D3d11Encoder {
    /// Encoder over `device` for `codec`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without `MediaFoundation`, or without a
    /// `codec` encoder MFT on the machine.
    pub fn open(device: &D3d11Device, codec: Codec) -> Result<Self, D3d11Error> {
        Ok(Self {
            session: Session::open(device, codec)?,
            codec,
        })
    }

    /// Codec the encoder encodes.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// True when the transform reports D3D11 awareness.
    #[must_use]
    pub fn uses_d3d11(&self) -> bool {
        self.session.uses_d3d11()
    }

    /// Name of the serving encoder MFT.
    #[must_use]
    pub fn mft_name(&self) -> &str {
        self.session.mft_name()
    }

    /// True when the session encodes from GPU textures.
    #[must_use]
    pub(crate) fn gpu_ready(&self) -> bool {
        self.session.gpu_bound()
    }

    /// Packets coded from `surface`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a D3D11 bind or when the backend cannot
    /// encode `surface`.
    pub(crate) fn encode_surface(
        &mut self,
        surface: &GpuSurface,
    ) -> Result<Vec<Packet>, D3d11Error> {
        let layout = surface.layout();
        if layout.format() != PixelFormat::Nv12 {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::EncoderInputNotNv12 {
                    format: layout.format(),
                },
            ));
        }
        let size = layout.visible();
        self.session.encode_texture(
            surface.export(),
            u32::from(layout.slice()),
            layout.left(),
            layout.top(),
            size.width(),
            size.height(),
        )
    }

    /// Packets coded from `scaled`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a D3D11 bind or when the backend cannot
    /// encode `scaled`.
    pub(crate) fn encode_scaled(
        &mut self,
        scaled: &ScaledSurface,
    ) -> Result<Vec<Packet>, D3d11Error> {
        let size = scaled.size();
        self.session
            .encode_texture(scaled.texture(), 0, 0, 0, size.width(), size.height())
    }
}

#[cfg(target_os = "windows")]
impl Encoder for D3d11Encoder {
    type Error = D3d11Error;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, D3d11Error> {
        let planes = Planes::of(frame)?;
        let (width, height) = (planes.width(), planes.height());
        self.session.encode(width, height, |dst| planes.fill(dst))
    }

    fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        self.session.flush()
    }
}

/// Validated NV12 plane views over one frame.
#[cfg(target_os = "windows")]
struct Planes<'a> {
    /// Frame width in luma samples.
    width: u32,
    /// Frame height in luma samples.
    height: u32,
    /// Luma bytes in row-major order.
    y: &'a [u8],
    /// Luma bytes per row.
    y_stride: u32,
    /// Chroma bytes in row-major order.
    uv: &'a [u8],
    /// Chroma bytes per row.
    uv_stride: u32,
}

#[cfg(target_os = "windows")]
impl<'a> Planes<'a> {
    /// Validated views over `frame`.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a foreign format, an odd extent, a missing
    /// plane, a stride below the width, or a plane short of its stride.
    fn of(frame: &'a CpuFrame) -> Result<Self, D3d11Error> {
        if frame.format() != PixelFormat::Nv12 {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::EncoderInputNotNv12 {
                    format: frame.format(),
                },
            ));
        }
        let width = frame.size().width();
        let height = frame.size().height();
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(D3d11Error::Unsupported(UnsupportedReason::OddNv12Extent));
        }
        let planes = frame.planes();
        let (Some(y), Some(uv)) = (planes.first(), planes.get(1)) else {
            return Err(D3d11Error::Unsupported(
                UnsupportedReason::Nv12WithoutTwoPlanes,
            ));
        };
        check_plane(y.data(), y.stride(), width, height)?;
        check_plane(uv.data(), uv.stride(), width, height / 2)?;
        Ok(Self {
            width,
            height,
            y: y.data(),
            y_stride: y.stride(),
            uv: uv.data(),
            uv_stride: uv.stride(),
        })
    }

    /// Frame width in luma samples.
    fn width(&self) -> u32 {
        self.width
    }

    /// Frame height in luma samples.
    fn height(&self) -> u32 {
        self.height
    }

    /// Contiguous NV12 bytes into `dst`.
    ///
    /// # Panics
    ///
    /// Panics when `dst` is shorter than the packed frame.
    fn fill(&self, dst: &mut [u8]) {
        let y_len = usize::try_from(u64::from(self.width) * u64::from(self.height)).unwrap_or(0);
        let (y_dst, uv_dst) = dst.split_at_mut(y_len);
        copy_rows(self.y, self.y_stride, self.width, self.height, y_dst);
        copy_rows(self.uv, self.uv_stride, self.width, self.height / 2, uv_dst);
    }
}

/// Validated `rows` of `width` bytes inside a strided `data` plane.
///
/// # Errors
///
/// Returns `Err` for a stride below the width or a plane short of its stride.
#[cfg(target_os = "windows")]
fn check_plane(data: &[u8], stride: u32, width: u32, rows: u32) -> Result<(), D3d11Error> {
    let stride_64 = u64::from(stride);
    let width_64 = u64::from(width);
    if stride_64 < width_64 {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::PlaneStrideBelowWidth,
        ));
    }
    let need = stride_64
        .saturating_mul(u64::from(rows).saturating_sub(1))
        .saturating_add(width_64);
    if u64::try_from(data.len()).unwrap_or(0) < need {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::PlaneShortOfStride,
        ));
    }
    Ok(())
}

/// `rows` of `width` bytes from a validated strided `data` plane into `dst`.
#[cfg(target_os = "windows")]
fn copy_rows(data: &[u8], stride: u32, width: u32, rows: u32, dst: &mut [u8]) {
    let stride = usize::try_from(stride).unwrap_or(0);
    let width = usize::try_from(width).unwrap_or(0);
    let rows = usize::try_from(rows).unwrap_or(0);
    if stride == width {
        let total = width.saturating_mul(rows);
        dst[..total].copy_from_slice(&data[..total]);
        return;
    }
    for (row, chunk) in data.chunks(stride).take(rows).enumerate() {
        dst[row * width..(row + 1) * width].copy_from_slice(&chunk[..width]);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::{check_plane, Planes};

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};

    /// NV12 frame over `y` and `uv` with `stride`.
    fn nv12(width: u32, height: u32, stride: u32, y: Vec<u8>, uv: Vec<u8>) -> CpuFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(y), stride).expect("luma"),
                Plane::new(Bytes::from(uv), stride).expect("chroma"),
            ],
        )
        .expect("frame")
    }

    #[test]
    fn pack_rejects_foreign_format() {
        let size = PictureSize::new(4, 4).expect("nonzero extent");
        let frame = CpuFrame::new(
            size,
            PixelFormat::Rgba8,
            vec![Plane::new(Bytes::from(vec![0; 64]), 16).expect("plane")],
        )
        .expect("frame");
        assert!(matches!(
            Planes::of(&frame),
            Err(crate::error::D3d11Error::Unsupported(_))
        ));
    }

    #[test]
    fn pack_rejects_odd_extent() {
        let frame = nv12(3, 4, 3, vec![0; 12], vec![0; 6]);
        assert!(Planes::of(&frame).is_err());
        let frame = nv12(4, 3, 4, vec![0; 12], vec![0; 4]);
        assert!(Planes::of(&frame).is_err());
    }

    #[test]
    fn pack_copies_tight_planes() {
        let y: Vec<u8> = (0..16).collect();
        let uv: Vec<u8> = (16..24).collect();
        let frame = nv12(4, 4, 4, y, uv);
        let planes = Planes::of(&frame).expect("views");
        assert_eq!((planes.width(), planes.height()), (4, 4));
        let mut dst = vec![0; 24];
        planes.fill(&mut dst);
        assert_eq!(dst, (0..24).collect::<Vec<u8>>());
    }

    #[test]
    fn pack_strips_strided_padding() {
        let y = vec![1, 2, 9, 9, 3, 4, 9, 9];
        let uv = vec![5, 6, 9, 9];
        let frame = nv12(2, 2, 4, y, uv);
        let planes = Planes::of(&frame).expect("views");
        let mut dst = vec![0; 6];
        planes.fill(&mut dst);
        assert_eq!(dst, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn pack_rejects_narrow_stride() {
        assert!(check_plane(&[0; 8], 2, 4, 2).is_err());
    }

    #[test]
    fn pack_rejects_short_plane() {
        assert!(check_plane(&[0; 5], 4, 4, 2).is_err());
    }
}
