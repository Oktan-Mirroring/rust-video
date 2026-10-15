//! D3D12 hardware encode behind the caps `Encoder` trait.
//!
//! NV12 input through the video encoder into driver bitstreams.

use crate::device::D3d12Device;
use crate::encode::EncodeSession;
use crate::error::D3d12Error;
use crate::guid::Codec;

use bytes::Bytes;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;

/// D3D12 hardware encoder over the video encode session.
///
/// Encoders take NV12 input and code intra-only: every frame is an
/// IDR; reference pictures stay pending.
///
/// # Usage Notes
///
/// Driver 596.36 emits corrupt H.264 IDR slices on noise-like input,
/// byte-identical through this tier and ffmpeg `h264_d3d12va` (decoder
/// reports intra4x4 mode -1 at MB 0,0); parameter sets are exonerated.
/// Corruption spans high/main profiles, CQP/default rate control, and
/// GOP lengths; only main profile with CQP and intra-only stays clean.
/// Retest on 617.42 or later; flat input is unaffected.
pub struct D3d12Encoder<'a> {
    /// Device owning the encode session.
    device: &'a D3d12Device,
    /// Codec the encoder encodes.
    codec: Codec,
    /// Live encode session, created from the first frame.
    session: Option<EncodeSession>,
    /// Geometry behind the live session.
    active: Option<(u32, u32)>,
}

impl<'a> D3d12Encoder<'a> {
    /// Encoder over `device` for `codec`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the adapter reports no `codec` video encode.
    pub fn open(device: &'a D3d12Device, codec: Codec) -> Result<Self, D3d12Error> {
        if !device.supports_encode(codec) {
            return Err(D3d12Error::Unsupported(format!(
                "no {codec} d3d12 video encode on this adapter"
            )));
        }
        Ok(Self {
            device,
            codec,
            session: None,
            active: None,
        })
    }

    /// Codec the encoder encodes.
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// Live session at `width` x `height`, reopened on change.
    fn ensure_session(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&mut EncodeSession, D3d12Error> {
        if self.active != Some((width, height)) {
            let session = EncodeSession::open(self.device, self.codec, width, height)?;
            self.session = Some(session);
            self.active = Some((width, height));
        }
        self.session.as_mut().ok_or(D3d12Error::NoSession)
    }

    /// Packets coded from `texture`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the texture misses the session extent or the
    /// backend cannot encode it.
    #[cfg(target_os = "windows")]
    pub(crate) fn encode_texture(
        &mut self,
        texture: &crate::texture::GpuTexture,
    ) -> Result<Vec<Packet>, D3d12Error> {
        let size = texture.size();
        let (width, height) = (size.width(), size.height());
        let session = self.ensure_session(width, height)?;
        let bitstream = session.encode_texture(texture.resource(), width, height)?;
        Ok(vec![Packet::new(Bytes::from(bitstream))])
    }
}

impl Encoder for D3d12Encoder<'_> {
    type Error = D3d12Error;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, D3d12Error> {
        let (width, height, nv12) = pack_nv12(frame)?;
        let session = self.ensure_session(width, height)?;
        let bitstream = session.encode(&nv12)?;
        Ok(vec![Packet::new(Bytes::from(bitstream))])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, D3d12Error> {
        Ok(Vec::new())
    }
}

/// Contiguous NV12 bytes plus extents for `frame`.
fn pack_nv12(frame: &CpuFrame) -> Result<(u32, u32, Vec<u8>), D3d12Error> {
    if frame.format() != PixelFormat::Nv12 {
        return Err(D3d12Error::Unsupported(format!(
            "encoder input {:?}; nv12 only",
            frame.format()
        )));
    }
    let width = frame.size().width();
    let height = frame.size().height();
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(D3d12Error::Unsupported("odd NV12 extent".to_owned()));
    }
    let planes = frame.planes();
    let (Some(y), Some(uv)) = (planes.first(), planes.get(1)) else {
        return Err(D3d12Error::Unsupported(
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
) -> Result<(), D3d12Error> {
    let stride_64 = u64::from(stride);
    let width_64 = u64::from(width);
    if stride_64 < width_64 {
        return Err(D3d12Error::Unsupported(
            "plane stride below width".to_owned(),
        ));
    }
    let need = stride_64
        .saturating_mul(u64::from(rows).saturating_sub(1))
        .saturating_add(width_64);
    if u64::try_from(data.len()).unwrap_or(0) < need {
        return Err(D3d12Error::Unsupported(
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
            Err(crate::error::D3d12Error::Unsupported(_))
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
