#![allow(unsafe_code)]
//! VP9 encode over libvpx.
//!
//! Encode only: decode rides the ffmpeg sys layer, which beat the direct
//! bind on every codec (see `docs/sw-codecs.md`), and ffmpeg ships no
//! native LGPL VP9 encoder, so libvpx stays for the encode direction.

use std::mem::MaybeUninit;

use bytes::Bytes;
use vpx_sys as vpx;

use rust_video::frame::{CpuFrame, PixelFormat};
use rust_video::packet::Packet;
use rust_video::traits::Encoder;

use rust_video_caps::common::PictureSize;

use crate::error::SwError;

/// Realtime VP9 encoder.
pub struct Vp9Encoder {
    ctx: vpx::vpx_codec_ctx_t,
    width: u32,
    height: u32,
    next_pts: i64,
}

impl Vp9Encoder {
    /// Realtime deadline, `VPX_DL_REALTIME`.
    const DEADLINE: std::os::raw::c_ulong = 1;
    /// Linked libvpx major the bindings were generated against.
    const ABI_MAJOR: u32 = 1;

    /// CBR encoder for `width`x`height` at `bitrate_kbps` on `threads`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when libvpx drifts from the bound ABI or rejects
    /// the configuration.
    pub fn open(width: u32, height: u32, bitrate_kbps: u32, threads: u32) -> Result<Self, SwError> {
        // SAFETY: version query takes no arguments and touches no state.
        let version = unsafe { vpx::vpx_codec_version() };
        let Ok(version) = u32::try_from(version) else {
            return Err(SwError::Init);
        };
        if (version >> 16) & 0xff != Self::ABI_MAJOR {
            return Err(SwError::Init);
        }
        if PictureSize::new(width, height).is_none() {
            return Err(SwError::Init);
        }
        // SAFETY: the VP9 encoder table is a static libvpx interface.
        let iface = unsafe { vpx::vpx_codec_vp9_cx() };
        let mut cfg = MaybeUninit::<vpx::vpx_codec_enc_cfg_t>::zeroed();
        // SAFETY: defaults fill the whole struct.
        let err = unsafe { vpx::vpx_codec_enc_config_default(iface, cfg.as_mut_ptr(), 0) };
        if err != vpx::VPX_CODEC_OK {
            return Err(SwError::Init);
        }
        // SAFETY: config holds defaults from the call above.
        let mut cfg = unsafe { cfg.assume_init() };
        cfg.g_w = width;
        cfg.g_h = height;
        cfg.g_threads = threads;
        cfg.g_timebase.num = 1;
        cfg.g_timebase.den = 60;
        cfg.rc_target_bitrate = bitrate_kbps;
        cfg.rc_end_usage = vpx::vpx_rc_mode::VPX_CBR;
        cfg.g_pass = vpx::vpx_enc_pass::VPX_RC_ONE_PASS;
        cfg.kf_max_dist = 60;
        let mut ctx = MaybeUninit::<vpx::vpx_codec_ctx_t>::zeroed();
        // SAFETY: iface and cfg are valid; init fills the context.
        let err = unsafe {
            vpx::vpx_codec_enc_init_ver(
                ctx.as_mut_ptr(),
                iface,
                &raw const cfg,
                0,
                vpx::VPX_ENCODER_ABI_VERSION.cast_signed(),
            )
        };
        if err != vpx::VPX_CODEC_OK {
            return Err(SwError::Init);
        }
        // SAFETY: init succeeded; destroy in Drop balances it.
        let mut ctx = unsafe { ctx.assume_init() };
        // SAFETY: control id and value match the realtime preset contract.
        unsafe {
            vpx::vpx_codec_control_(
                &raw mut ctx,
                vpx::vp8e_enc_control_id::VP8E_SET_CPUUSED as _,
                8,
            );
        }
        Ok(Self {
            ctx,
            width,
            height,
            next_pts: 0,
        })
    }

    /// Coded packets ready now.
    fn drain(&mut self) -> Vec<Packet> {
        let mut out = Vec::new();
        let mut iter: vpx::vpx_codec_iter_t = std::ptr::null();
        loop {
            // SAFETY: iterator protocol per `vpx_codec_get_cx_data` docs.
            let pkt = unsafe { vpx::vpx_codec_get_cx_data(&raw mut self.ctx, &raw mut iter) };
            if pkt.is_null() {
                return out;
            }
            // SAFETY: packet is valid until the next encoder call.
            let pkt = unsafe { &*pkt };
            if pkt.kind != vpx::vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                continue;
            }
            // SAFETY: kind tag selects the frame union arm.
            let frame = unsafe { pkt.data.frame };
            // SAFETY: coded bytes span live encoder memory, copied now.
            let bytes = unsafe { std::slice::from_raw_parts(frame.buf as *const u8, frame.sz) };
            let mut packet = Packet::new(Bytes::copy_from_slice(bytes));
            if frame.flags & vpx::VPX_FRAME_IS_KEY != 0 {
                packet = packet.mark_keyframe();
            }
            out.push(packet);
        }
    }
}

impl Encoder for Vp9Encoder {
    type Error = SwError;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        if frame.format() != PixelFormat::Yuv420P
            || frame.size().width() != self.width
            || frame.size().height() != self.height
        {
            return Err(SwError::Rejected);
        }
        let image = MaybeUninit::<vpx::vpx_image_t>::zeroed();
        // SAFETY: only the planes, strides, and geometry below are read.
        let mut image = unsafe { image.assume_init() };
        image.fmt = vpx::vpx_img_fmt::VPX_IMG_FMT_I420;
        image.w = self.width;
        image.h = self.height;
        image.d_w = self.width;
        image.d_h = self.height;
        image.x_chroma_shift = 1;
        image.y_chroma_shift = 1;
        for (slot, plane) in frame.planes().iter().enumerate() {
            image.planes[slot] = plane.data().as_ptr().cast_mut();
            image.stride[slot] = plane.stride().cast_signed();
        }
        let pts = self.next_pts;
        self.next_pts += 1;
        // SAFETY: image points at the live frame; flags and deadline are valid.
        let err = unsafe {
            vpx::vpx_codec_encode(
                &raw mut self.ctx,
                &raw const image,
                pts,
                1,
                0,
                Self::DEADLINE,
            )
        };
        if err != vpx::VPX_CODEC_OK {
            return Err(SwError::Rejected);
        }
        Ok(self.drain())
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        // SAFETY: null frame flushes delayed packets.
        let err = unsafe {
            vpx::vpx_codec_encode(
                &raw mut self.ctx,
                std::ptr::null(),
                self.next_pts,
                1,
                0,
                Self::DEADLINE,
            )
        };
        if err != vpx::VPX_CODEC_OK {
            return Err(SwError::Rejected);
        }
        Ok(self.drain())
    }
}

impl Drop for Vp9Encoder {
    fn drop(&mut self) {
        // SAFETY: context initialized at open and destroyed once.
        unsafe { vpx::vpx_codec_destroy(&raw mut self.ctx) };
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video::traits::Encoder;

    use rust_video_caps::common::PictureSize;

    use super::Vp9Encoder;

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

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert!(!packets.is_empty());
        assert!(packets[0].is_keyframe());
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn roundtrip_through_ffmpeg_decoder() {
        use rust_video::traits::Decoder as _;

        use crate::ffmpeg::Vp9Decoder;

        let mut encoder = Vp9Encoder::open(64, 64, 500, 2).expect("encoder");
        let mut decoder = Vp9Decoder::open(2).expect("decoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert!(!packets.is_empty());
        assert!(packets[0].is_keyframe());
        let mut frames = Vec::new();
        for packet in &packets {
            frames.extend(decoder.decode(packet).expect("decode"));
        }
        frames.extend(decoder.flush().expect("flush"));
        assert_eq!(frames.len(), 2);
        for frame in &frames {
            assert_eq!(frame.format(), PixelFormat::Yuv420P);
            assert_eq!(frame.size().width(), 64);
            assert_eq!(frame.size().height(), 64);
        }
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder");
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
        assert!(Vp9Encoder::open(0, 64, 500, 1).is_err());
        assert!(Vp9Encoder::open(64, 0, 500, 1).is_err());
    }
}
