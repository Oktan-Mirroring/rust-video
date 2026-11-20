//! D3D11 hardware encode over `MediaFoundation` encoder MFTs.

use crate::device::D3d11Device;
use crate::encode::{Session, Vp9Tuning};
use crate::error::{D3d11Error, UnsupportedReason};
use crate::guid::Codec;
use crate::scale::ScaledSurface;
use crate::texture::GpuSurface;

use rust_video_caps::encode::{EncodeConfig, Level, Profile, RateControl};
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::reason::UnsupportedReason as StreamCore;
use rust_video_caps::traits::{EncodeSurface, Encoder};
use rust_video_caps::transcode::{GpuCaps, ShareMechanism};
use rust_video_core::planes;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

/// D3D11 hardware encoder over a `MediaFoundation` encoder MFT.
///
/// Automatic rate control runs the legacy recipe: 30 fps progressive
/// with an area-scaled bitrate; explicit configs map onto the media
/// types and codec API. Input is NV12.
pub struct D3d11Encoder {
    /// Negotiated MF session.
    session: Session,
    /// Codec the session encodes.
    codec: Codec,
}

impl D3d11Encoder {
    /// Legacy recipe config for `codec`, absent without an encode profile.
    ///
    /// Automatic rate control over the codec default profile and a
    /// tier-picked level; [`open`](Self::open) encodes the legacy
    /// recipe from it. MPEG-2, VC-1, and AV2 name no encode profile.
    ///
    /// # Arguments
    ///
    /// * `codec` - codec family behind the config.
    #[must_use]
    pub fn auto_config(codec: Codec) -> Option<EncodeConfig> {
        use rust_video_caps::{av1, h264, h265, vp9};

        let (profile, level) = match codec {
            Codec::H264 => (
                Profile::H264(h264::EncodeProfile::Main),
                Level::H264(h264::EncodeLevel::Auto),
            ),
            Codec::H265 => (
                Profile::H265(h265::EncodeProfile::Main),
                Level::H265(h265::EncodeLevel::Auto),
            ),
            Codec::Av1 => (
                Profile::Av1(av1::EncodeProfile::Main),
                Level::Av1(av1::EncodeLevel::Auto),
            ),
            Codec::Vp9 => (
                Profile::Vp9(vp9::EncodeProfile::P0),
                Level::Vp9(vp9::EncodeLevel::Auto),
            ),
            Codec::Vp8 => (Profile::Vp8, Level::Vp8),
            Codec::Mpeg2 | Codec::Vc1 | Codec::Av2 => return None,
        };
        Some(EncodeConfig {
            rate_control: RateControl::Auto,
            profile,
            level,
            ..EncodeConfig::default()
        })
    }

    /// Encoder over `device` for `config`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `config` fails validation or exceeds the
    /// tier, without `MediaFoundation`, or without a `config` codec
    /// encoder MFT on the machine.
    ///
    /// # Arguments
    ///
    /// * `config` - rate, structure, quality, profile, level, depth,
    ///   chroma, frame rate, and color behind the session.
    pub fn open(device: &D3d11Device, config: EncodeConfig) -> Result<Self, D3d11Error> {
        Ok(Self {
            session: Session::open(device, config)?,
            codec: config.codec(),
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

    /// Applied VP9 speed tuning, absent off the VP9 path or without the property.
    #[must_use]
    pub fn vp9_tuning(&self) -> Option<Vp9Tuning> {
        self.session.vp9_tuning()
    }

    /// True when the session encodes from GPU textures.
    #[must_use]
    pub(crate) fn gpu_ready(&self) -> bool {
        self.session.gpu_bound()
    }

    /// Packets coded from an NV12 `texture` region, possibly none.
    ///
    /// The region must sit on even offsets with an even extent.
    ///
    /// # Errors
    ///
    /// Returns `Err` for an odd region or when the backend cannot
    /// encode `texture`.
    pub fn encode_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        slice: u32,
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<Packet>, D3d11Error> {
        if left.is_multiple_of(2)
            && top.is_multiple_of(2)
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
        {
            self.session
                .encode_texture(texture, slice, left, top, width, height)
        } else {
            Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::OddNv12Extent,
            )))
        }
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

impl Encoder for D3d11Encoder {
    type Error = D3d11Error;

    fn caps(&self) -> GpuCaps {
        let live = self.gpu_ready();
        GpuCaps::new(live.then_some(ShareMechanism::NtHandle), false, live)
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, D3d11Error> {
        let planes = Planes::of(frame)?;
        let (width, height) = (planes.width(), planes.height());
        self.session.encode(width, height, |dst| planes.fill(dst))
    }

    fn flush(&mut self) -> Result<Vec<Packet>, D3d11Error> {
        self.session.flush()
    }
}

impl EncodeSurface for D3d11Encoder {
    type Surface = GpuSurface;
    type Error = D3d11Error;

    fn encode_surface(&mut self, surface: &GpuSurface) -> Result<Vec<Packet>, D3d11Error> {
        if surface.format() != PixelFormat::Nv12 {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::EncoderInputNotNv12 {
                    format: surface.format(),
                },
            )));
        }
        let size = surface.size();
        let (left, top) = surface.origin();
        self.session.encode_texture(
            surface.texture(),
            surface.slice(),
            left,
            top,
            size.width(),
            size.height(),
        )
    }
}

/// Validated NV12 plane views over one frame.
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

impl<'a> Planes<'a> {
    /// Validated views over `frame`.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a foreign format, an odd extent, a missing
    /// plane, a stride below the width, or a plane short of its stride.
    fn of(frame: &'a CpuFrame) -> Result<Self, D3d11Error> {
        if frame.format() != PixelFormat::Nv12 {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::EncoderInputNotNv12 {
                    format: frame.format(),
                },
            )));
        }
        let width = frame.size().width();
        let height = frame.size().height();
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::OddNv12Extent,
            )));
        }
        let planes = frame.planes();
        let (Some(y), Some(uv)) = (planes.first(), planes.get(1)) else {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                StreamCore::Nv12WithoutTwoPlanes,
            )));
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
        planes::copy_rows(self.y, self.y_stride, self.width, self.height, y_dst);
        planes::copy_rows(self.uv, self.uv_stride, self.width, self.height / 2, uv_dst);
    }
}

/// Validated `rows` of `width` bytes inside a strided `data` plane.
///
/// # Errors
///
/// Returns `Err` for a stride below the width or a plane short of its stride.
fn check_plane(data: &[u8], stride: u32, width: u32, rows: u32) -> Result<(), D3d11Error> {
    planes::check_plane(data, stride, width, rows).map_err(|fault| match fault {
        planes::PlaneFault::StrideBelowWidth { .. } => {
            D3d11Error::Unsupported(UnsupportedReason::Stream(StreamCore::PlaneStrideBelowWidth))
        }
        planes::PlaneFault::ShortOfStride { .. } => {
            D3d11Error::Unsupported(UnsupportedReason::Stream(StreamCore::PlaneShortOfStride))
        }
    })
}

#[cfg(test)]
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

    #[test]
    fn auto_config_covers_mft_codecs() {
        use crate::guid::Codec;

        for codec in [Codec::H264, Codec::H265, Codec::Vp9, Codec::Av1, Codec::Vp8] {
            let config = super::D3d11Encoder::auto_config(codec).expect("auto config");
            assert_eq!(config.codec(), codec);
            assert!(matches!(
                config.rate_control,
                rust_video_caps::encode::RateControl::Auto
            ));
        }
        for codec in [Codec::Mpeg2, Codec::Vc1, Codec::Av2] {
            assert!(super::D3d11Encoder::auto_config(codec).is_none());
        }
    }

    #[test]
    fn caps_agree_with_texture_import() {
        use rust_video_caps::traits::{EncodeSurface, Encoder};

        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let config = super::D3d11Encoder::auto_config(crate::guid::Codec::H264).expect("h264 auto");
        let Ok(mut encoder) = super::D3d11Encoder::open(&device, config) else {
            return;
        };
        let caps = encoder.caps();
        assert_eq!(caps.share().is_some(), caps.encode_import());
        assert!(!caps.decode_export());
        let mut decoder = crate::H264Decoder::open(&device);
        let frame = rust_video_harness::Synth::noise(320, 240, 0);
        let mut surfaces = Vec::new();
        for packet in encoder.encode(&frame).expect("encode runs") {
            surfaces.extend(decoder.decode_surface(&packet).expect("decode runs"));
        }
        let surface = surfaces.pop().expect("first picture surfaces");
        if caps.encode_import() {
            encoder.encode_surface(&surface).expect("import runs");
        } else {
            let Err(crate::error::D3d11Error::Unsupported(reason)) =
                encoder.encode_surface(&surface)
            else {
                panic!("unbound import declines");
            };
            assert!(matches!(
                reason,
                crate::error::UnsupportedReason::EncoderWithoutGpuImport
            ));
        }
    }
}
