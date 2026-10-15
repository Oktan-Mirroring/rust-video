//! D3D11 hardware encode over `MediaFoundation` encoder MFTs.
//!
//! The live path targets Windows; other platforms decline at `open`
//! with a typed skip.

#[cfg(target_os = "windows")]
use crate::decoder::GpuSurface;
use crate::device::D3d11Device;
use crate::error::D3d11Error;
use crate::guid::Codec;
use crate::mf::Session;
#[cfg(target_os = "windows")]
use crate::scale::ScaledSurface;

use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;

/// D3D11 hardware encoder over a `MediaFoundation` encoder MFT.
///
/// Encoders run 30 fps progressive with an area-scaled bitrate; input is NV12.
pub struct D3d11Encoder {
    /// Negotiated MF session.
    session: Session,
    /// Codec the session encodes.
    codec: Codec,
}

impl D3d11Encoder {
    /// Encoder over `device` for `codec`.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, without `MediaFoundation`, or without a
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
    #[cfg(target_os = "windows")]
    pub(crate) fn encode_surface(
        &mut self,
        surface: &GpuSurface,
    ) -> Result<Vec<Packet>, D3d11Error> {
        let size = surface.size();
        self.session.encode_texture(
            surface.export(),
            u32::from(surface.slot()),
            surface.left(),
            surface.top(),
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
    #[cfg(target_os = "windows")]
    pub(crate) fn encode_scaled(
        &mut self,
        scaled: &ScaledSurface,
    ) -> Result<Vec<Packet>, D3d11Error> {
        let size = scaled.size();
        self.session
            .encode_texture(scaled.texture(), 0, 0, 0, size.width(), size.height())
    }
}

impl Encoder for D3d11Encoder {
    type Error = D3d11Error;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, D3d11Error> {
        let (width, height, nv12) = pack_nv12(frame)?;
        self.session.encode(width, height, &nv12)
    }

    fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        self.session.flush()
    }
}

/// Contiguous NV12 bytes plus extents for `frame`.
fn pack_nv12(frame: &CpuFrame) -> Result<(u32, u32, Vec<u8>), D3d11Error> {
    if frame.format() != PixelFormat::Nv12 {
        return Err(D3d11Error::Unsupported(format!(
            "encoder input {:?}; nv12 only",
            frame.format()
        )));
    }
    let width = frame.size().width();
    let height = frame.size().height();
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(D3d11Error::Unsupported("odd NV12 extent".to_owned()));
    }
    let planes = frame.planes();
    let (Some(y), Some(uv)) = (planes.first(), planes.get(1)) else {
        return Err(D3d11Error::Unsupported(
            "nv12 frame without two planes".to_owned(),
        ));
    };
    let capacity = usize::try_from(u64::from(width) * u64::from(height) * 3 / 2).unwrap_or(0);
    let mut nv12 = Vec::with_capacity(capacity);
    copy_plane(y.data(), y.stride(), width, height, &mut nv12)?;
    copy_plane(uv.data(), uv.stride(), width, height / 2, &mut nv12)?;
    Ok((width, height, nv12))
}

/// `rows` of `width` bytes from a strided `data` plane into `out`.
fn copy_plane(
    data: &[u8],
    stride: u32,
    width: u32,
    rows: u32,
    out: &mut Vec<u8>,
) -> Result<(), D3d11Error> {
    let stride_64 = u64::from(stride);
    let width_64 = u64::from(width);
    if stride_64 < width_64 {
        return Err(D3d11Error::Unsupported(
            "plane stride below width".to_owned(),
        ));
    }
    let need = stride_64
        .saturating_mul(u64::from(rows).saturating_sub(1))
        .saturating_add(width_64);
    if u64::try_from(data.len()).unwrap_or(0) < need {
        return Err(D3d11Error::Unsupported(
            "plane short of its stride".to_owned(),
        ));
    }
    let stride = usize::try_from(stride).unwrap_or(0);
    let width = usize::try_from(width).unwrap_or(0);
    let rows = usize::try_from(rows).unwrap_or(0);
    for chunk in data.chunks(stride).take(rows) {
        out.extend_from_slice(&chunk[..width]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{copy_plane, pack_nv12};

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
            pack_nv12(&frame),
            Err(crate::error::D3d11Error::Unsupported(_))
        ));
    }

    #[test]
    fn pack_rejects_odd_extent() {
        let frame = nv12(3, 4, 3, vec![0; 12], vec![0; 6]);
        assert!(pack_nv12(&frame).is_err());
        let frame = nv12(4, 3, 4, vec![0; 12], vec![0; 4]);
        assert!(pack_nv12(&frame).is_err());
    }

    #[test]
    fn pack_copies_tight_planes() {
        let y: Vec<u8> = (0..16).collect();
        let uv: Vec<u8> = (16..24).collect();
        let frame = nv12(4, 4, 4, y, uv);
        let (width, height, nv12) = pack_nv12(&frame).expect("packed");
        assert_eq!((width, height), (4, 4));
        assert_eq!(nv12, (0..24).collect::<Vec<u8>>());
    }

    #[test]
    fn pack_strips_strided_padding() {
        let y = vec![1, 2, 9, 9, 3, 4, 9, 9];
        let uv = vec![5, 6, 9, 9];
        let frame = nv12(2, 2, 4, y, uv);
        let (_, _, nv12) = pack_nv12(&frame).expect("packed");
        assert_eq!(nv12, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn pack_rejects_narrow_stride() {
        let mut out = Vec::new();
        assert!(copy_plane(&[0; 8], 2, 4, 2, &mut out).is_err());
    }

    #[test]
    fn pack_rejects_short_plane() {
        let mut out = Vec::new();
        assert!(copy_plane(&[0; 5], 4, 4, 2, &mut out).is_err());
    }
}
