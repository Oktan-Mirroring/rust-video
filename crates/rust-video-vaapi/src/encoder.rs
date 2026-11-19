//! VA-API encode: CQP sessions for H.264 and H.265.
//!
//! All-intra or low-delay IPPP over a one-frame sliding window. CQP
//! keeps constant quality without a bitrate model.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::os::raw::{c_int, c_void};

use crate::buffer::{bytes_of, VaBuffer};
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi;
use crate::h264::decoder::VaApiFrame;
use crate::scale::ScaledSurface;
use crate::sys;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::{FallbackCause, GpuCaps, ShareMechanism};

/// Codec an encoder session writes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EncodeCodec {
    /// H.264, High profile.
    H264,
    /// H.265, Main profile.
    H265,
}

impl EncodeCodec {
    /// VA-API profile for the codec.
    #[must_use]
    pub const fn profile(self) -> c_int {
        match self {
            Self::H264 => ffi::VA_PROFILE_H264_HIGH,
            Self::H265 => ffi::VA_PROFILE_HEVC_MAIN,
        }
    }

    /// Coded-size alignment in luma samples.
    #[must_use]
    const fn block(self) -> u32 {
        match self {
            Self::H264 => 16,
            Self::H265 => 8,
        }
    }
}

impl fmt::Display for EncodeCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::H264 => write!(f, "h264"),
            Self::H265 => write!(f, "h265"),
        }
    }
}

/// GOP shape an encoder session writes.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum GopShape {
    /// Every picture an IDR.
    #[default]
    AllIntra,
    /// IDR every period, P between; no reorder delay.
    LowDelay {
        /// Pictures per IDR; 0 behaves as 1.
        intra_period: u32,
    },
}

/// VA-API hardware encoder over one display.
///
/// `Send`, never `Sync`: hand the session to another thread when idle,
/// but run every call on one thread. The contract matches the display
/// behind it; see `VaApiDevice`.
pub struct VaApiEncoder {
    /// Codec the session writes.
    codec: EncodeCodec,
    /// GOP shape the session writes.
    gop: GopShape,
    /// Previous picture as next P reference, if any.
    prev: Option<Reference>,
    /// Visible size in luma samples.
    visible: PictureSize,
    /// Coded size in luma samples, block-aligned.
    coded: (u32, u32),
    /// Constant quantization.
    qp: u8,
    /// Quantization the packed picture header carries, if packed.
    packed_qp: Option<u8>,
    /// Packed sequence/picture bit shifts and fixed words.
    words: Words,
    /// Level idc baked into the sequence.
    level: u8,
    /// Pictures encoded so far.
    count: u64,
    /// Driver buffers created by this session.
    creates: u64,
    /// Next reconstruction surface.
    next_recon: usize,
    /// Next pipeline slot.
    next_slot: usize,
    /// Submitted picture awaiting harvest, if any.
    pending: Option<PendingJob>,
    /// Coded-bitstream buffers, one per pipeline slot.
    coded_buf: [VaBuffer; 2],
    /// Sequence-parameter buffer, rewritten every picture.
    seq: VaBuffer,
    /// Picture-parameter buffer, rewritten every picture.
    slice_pic: VaBuffer,
    /// Slice-parameter buffer, rewritten every picture.
    slice: VaBuffer,
    /// Packed VPS, H.265 only.
    vps: Option<PackedPair>,
    /// Packed sequence header.
    sps: PackedPair,
    /// Packed picture header.
    pps: PackedPair,
    /// Packed slice header, rewritten every picture.
    slice_packed: PackedPair,
    /// NV12 upload images, one per pipeline slot.
    upload: [ffi::VaImage; 2],
    /// True per slot when its image derives the surface, needing no blit.
    derived: [bool; 2],
    /// Header bit writer, reused across pictures.
    writer: BitWriter,
    /// Scratch render list, reused across pictures.
    render: Vec<ffi::VaBufferId>,
    /// Owned session, dropped after the buffers above.
    session: Session,
    /// Thread contract: `Send` without `Sync`.
    _thread: PhantomData<UnsafeCell<()>>,
}

impl VaApiEncoder {
    /// Default constant quantization.
    const DEFAULT_QP: u8 = 26;
    /// Maximum quantization.
    const MAX_QP: u8 = 51;
    /// Upload surface count; slots alternate across pictures.
    const UPLOADS: usize = 2;
    /// First reconstruction surface index.
    const RECON: usize = 2;
    /// Reconstruction surface count.
    const RECONS: usize = 2;

    /// H.264 sequence-field shifts.
    const H264_SEQ_CHROMA: u32 = 0;
    /// H.264 frame-only shift.
    const H264_SEQ_MBS_ONLY: u32 = 2;
    /// H.264 direct-8x8 shift.
    const H264_SEQ_DIRECT_8X8: u32 = 5;
    /// H.264 order-count-lsb shift.
    const H264_SEQ_LOG2_POC_LSB: u32 = 12;
    /// H.264 IDR shift.
    const H264_PIC_IDR: u32 = 0;
    /// H.264 reference shift.
    const H264_PIC_REFERENCE: u32 = 1;
    /// H.264 entropy-coding shift.
    const H264_PIC_ENTROPY: u32 = 3;
    /// H.264 transform-8x8 shift.
    const H264_PIC_TRANSFORM_8X8: u32 = 8;
    /// H.265 chroma shift.
    const H265_SEQ_CHROMA: u32 = 0;
    /// H.265 strong-smoothing shift.
    const H265_SEQ_STRONG_INTRA: u32 = 10;
    /// H.265 AMP shift.
    const H265_SEQ_AMP: u32 = 11;
    /// H.265 IDR shift.
    const H265_PIC_IDR: u32 = 0;
    /// H.265 coding-type shift.
    const H265_PIC_CODING_TYPE: u32 = 1;
    /// H.265 reference shift.
    const H265_PIC_REFERENCE: u32 = 4;
    /// H.265 sign-hiding shift.
    const H265_PIC_SIGN_DATA: u32 = 6;
    /// H.265 tile-loop shift.
    const H265_PIC_LOOP_TILES: u32 = 15;
    /// H.265 last-slice shift.
    const H265_SLICE_LAST: u32 = 0;
    /// H.264 VUI restriction shift.
    const H264_VUI_RESTRICTION: u32 = 2;
    /// H.264 VUI horizontal-MV-length shift.
    const H264_VUI_MV_H: u32 = 3;
    /// H.264 VUI vertical-MV-length shift.
    const H264_VUI_MV_V: u32 = 8;
    /// H.264 VUI cross-boundary-MV shift.
    const H264_VUI_MV_OVER: u32 = 15;
    /// VUI motion-vector length on both axes.
    const VUI_MV_LEN: u32 = 10;
    /// H.264 VUI word matching the packed VUI.
    const H264_VUI_FIELDS: u32 = (1 << Self::H264_VUI_RESTRICTION)
        | (Self::VUI_MV_LEN << Self::H264_VUI_MV_H)
        | (Self::VUI_MV_LEN << Self::H264_VUI_MV_V)
        | (1 << Self::H264_VUI_MV_OVER);

    /// Encoder over `device` for `codec` at `visible` size.
    ///
    /// The device must outlive the encoder.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver lacks the encode entrypoint, CQP
    /// rate control, packed headers, or NV12 upload for `codec`.
    pub fn open(
        device: &VaApiDevice,
        codec: EncodeCodec,
        visible: PictureSize,
    ) -> Result<Self, VaApiError> {
        let profile = codec.profile();
        if !device.supports(profile, ffi::VA_ENTRYPOINT_ENC_SLICE) {
            return Err(VaApiError::Unsupported(format!(
                "{codec}: {}",
                FallbackCause::EncoderCpuOnly
            )));
        }
        let rt = device.config_attr(
            profile,
            ffi::VA_ENTRYPOINT_ENC_SLICE,
            ffi::VA_CONFIG_ATTRIB_RT_FORMAT,
        );
        if rt.is_none_or(|value| value & ffi::VA_RT_FORMAT_YUV420 == 0) {
            return Err(VaApiError::Unsupported(format!(
                "{codec}: no nv12 encode target"
            )));
        }
        let rc = device.config_attr(
            profile,
            ffi::VA_ENTRYPOINT_ENC_SLICE,
            ffi::VA_CONFIG_ATTRIB_RATE_CONTROL,
        );
        if rc.is_none_or(|value| value & ffi::VA_RC_CQP == 0) {
            return Err(VaApiError::Unsupported(format!(
                "{codec}: no cqp rate control"
            )));
        }
        let packed = device.config_attr(
            profile,
            ffi::VA_ENTRYPOINT_ENC_SLICE,
            ffi::VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS,
        );
        let need = ffi::VA_ENC_PACKED_HEADER_SEQUENCE
            | ffi::VA_ENC_PACKED_HEADER_PICTURE
            | ffi::VA_ENC_PACKED_HEADER_SLICE;
        if packed.is_none_or(|value| value & need != need) {
            return Err(VaApiError::Unsupported(format!(
                "{codec}: no packed headers"
            )));
        }
        let mut image_format = Self::nv12_format(device)?;
        let block = codec.block();
        let coded_size = (
            visible.width().div_ceil(block) * block,
            visible.height().div_ceil(block) * block,
        );
        let session = Session::open(device, profile, coded_size)?;
        let display = device.display();
        let context = session.context;
        let ([first, second], [first_derived, second_derived]) =
            Self::upload_images(display, &session.surfaces, &mut image_format, coded_size)?;
        let words = Words::new(codec);
        let level = Self::level(codec, coded_size);
        let visible_pair = (visible.width(), visible.height());
        let (vps, sps, mut creates) =
            Self::pack_session_headers(codec, coded_size, visible_pair, level, display, context)?;
        // Two coded buffers plus sequence, picture, slice singles plus pps and slice packed pairs below.
        creates += 9;
        Ok(Self {
            codec,
            gop: GopShape::AllIntra,
            prev: None,
            visible,
            coded: coded_size,
            qp: Self::DEFAULT_QP,
            packed_qp: None,
            words,
            level,
            count: 0,
            creates,
            next_recon: 0,
            next_slot: 0,
            pending: None,
            coded_buf: Self::coded_buffers(display, context, coded_size)?,
            seq: VaBuffer::empty(
                display,
                context,
                ffi::VA_ENC_SEQUENCE_PARAMETER_BUFFER_TYPE,
                1,
            )?,
            slice_pic: VaBuffer::empty(
                display,
                context,
                ffi::VA_ENC_PICTURE_PARAMETER_BUFFER_TYPE,
                1,
            )?,
            slice: VaBuffer::empty(display, context, ffi::VA_ENC_SLICE_PARAMETER_BUFFER_TYPE, 1)?,
            vps,
            sps,
            pps: PackedPair::empty(display, context)?,
            slice_packed: PackedPair::empty(display, context)?,
            upload: [first, second],
            derived: [first_derived, second_derived],
            writer: BitWriter::default(),
            render: Vec::new(),
            session,
            _thread: PhantomData,
        })
    }

    /// Encoder retargeted to constant quantization `qp`, clamped to 51.
    #[must_use]
    pub fn with_qp(mut self, qp: u8) -> Self {
        self.qp = qp.min(Self::MAX_QP);
        self
    }

    /// Encoder writing `gop` instead of all-intra.
    #[must_use]
    pub fn with_gop(mut self, gop: GopShape) -> Self {
        self.gop = gop;
        self
    }

    /// Packed VPS (H.265) and sequence headers for the session.
    fn pack_session_headers(
        codec: EncodeCodec,
        coded_size: (u32, u32),
        visible_pair: (u32, u32),
        level: u8,
        display: ffi::VaDisplay,
        context: ffi::VaContextId,
    ) -> Result<(Option<PackedPair>, PackedPair, u64), VaApiError> {
        let mut writer = BitWriter::default();
        let mut creates = 0;
        let mut vps = None;
        if codec == EncodeCodec::H265 {
            Self::hevc_vps(level, &mut writer);
            let (bytes, bits) = writer.take();
            let mut pair = PackedPair::empty(display, context)?;
            creates += 2 + pair.pack(ffi::VA_ENC_PACKED_HEADER_TYPE_RAW_DATA, &bytes, bits)?;
            vps = Some(pair);
        }
        match codec {
            EncodeCodec::H264 => Self::h264_sps(coded_size, visible_pair, level, &mut writer),
            EncodeCodec::H265 => Self::hevc_sps(coded_size, visible_pair, level, &mut writer),
        }
        let (bytes, bits) = writer.take();
        let mut sps = PackedPair::empty(display, context)?;
        creates += 2 + sps.pack(ffi::VA_ENC_PACKED_HEADER_TYPE_SEQUENCE, &bytes, bits)?;
        Ok((vps, sps, creates))
    }

    /// Codec the session writes.
    #[must_use]
    pub const fn codec(&self) -> EncodeCodec {
        self.codec
    }

    /// Driver buffers created by this session since open.
    #[must_use]
    pub const fn buffer_creates(&self) -> u64 {
        self.creates
    }

    /// Visible size in luma samples.
    #[must_use]
    pub const fn size(&self) -> PictureSize {
        self.visible
    }

    /// Packets coded from the device surface behind `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame has no device surface, lives on
    /// another display, misses the session size, or the backend
    /// refuses the picture.
    pub(crate) fn encode_surface(&mut self, frame: &VaApiFrame) -> Result<Vec<Packet>, VaApiError> {
        let Some((display, surface)) = frame.device_handle() else {
            return Err(VaApiError::Unsupported(
                FallbackCause::NoSharedExport.to_string(),
            ));
        };
        self.check_surface(display, frame.identity().size())?;
        self.encode_inner(surface)
    }

    /// Packets coded from `scaled`, possibly none.
    ///
    /// Syncs the scale output first: the scaler returns before the
    /// scale finishes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the surface lives on another display, misses
    /// the session size, or the backend refuses the picture.
    pub(crate) fn encode_scaled(
        &mut self,
        scaled: &ScaledSurface,
    ) -> Result<Vec<Packet>, VaApiError> {
        self.check_surface(scaled.display(), scaled.size())?;
        // Sound: the source surface is live behind the scaler.
        sys::status(unsafe { sys::vaSyncSurface(self.session.display, scaled.surface()) })?;
        self.encode_inner(scaled.surface())
    }

    /// Display and size behind a caller surface.
    fn check_surface(&self, display: ffi::VaDisplay, size: PictureSize) -> Result<(), VaApiError> {
        if display != self.session.display {
            return Err(VaApiError::Unsupported(
                FallbackCause::DeviceMismatch.to_string(),
            ));
        }
        if size != self.visible {
            return Err(VaApiError::Unsupported(format!(
                "{} encode size {}x{} vs session {}x{}",
                self.codec,
                size.width(),
                size.height(),
                self.visible.width(),
                self.visible.height(),
            )));
        }
        Ok(())
    }

    /// Packets coded from `source`, possibly none.
    fn encode_inner(&mut self, source: ffi::VaSurfaceId) -> Result<Vec<Packet>, VaApiError> {
        let mut packets = self.drain_pending()?;
        // The drain freed every slot, so the inline job takes slot zero.
        let job = self.submit_job(source, 0)?;
        packets.push(self.harvest_job(&job)?);
        Ok(packets)
    }

    /// Submit `source` on `slot`, returning the pending harvest.
    fn submit_job(
        &mut self,
        source: ffi::VaSurfaceId,
        slot: usize,
    ) -> Result<PendingJob, VaApiError> {
        let recon = self.session.surfaces[Self::RECON + self.next_recon];
        self.next_recon = (self.next_recon + 1) % Self::RECONS;
        let coded = self.coded_buf[slot].id();
        let numbers = Self::numbers(self.gop, self.count)?;
        let reference = self.prev;
        match self.codec {
            EncodeCodec::H264 => self.render_h264(recon, &numbers, reference, coded)?,
            EncodeCodec::H265 => self.render_h265(recon, &numbers, reference, coded)?,
        }
        let display = self.session.display;
        let context = self.session.context;
        // Sound: the context and source surface are live.
        sys::status(unsafe { sys::vaBeginPicture(display, context, source) })?;
        let count = c_int::try_from(self.render.len()).map_err(|_| Self::range())?;
        let rendered =
            unsafe { sys::vaRenderPicture(display, context, self.render.as_mut_ptr(), count) };
        let end = unsafe { sys::vaEndPicture(display, context) };
        sys::status(rendered)?;
        sys::status(end)?;
        self.prev = Some(Reference {
            surface: recon,
            frame_num: numbers.frame_num,
            poc: numbers.poc,
        });
        self.count += 1;
        Ok(PendingJob {
            source,
            coded,
            idr: numbers.idr,
        })
    }

    /// Packet harvested from a submitted `job`.
    fn harvest_job(&mut self, job: &PendingJob) -> Result<Packet, VaApiError> {
        let display = self.session.display;
        // Sound: the surface was just submitted.
        sys::status(unsafe { sys::vaSyncSurface(display, job.source) })?;
        let bytes = self.harvest(job.coded)?;
        let packet = Packet::new(Bytes::from(bytes));
        Ok(if job.idr {
            packet.mark_keyframe()
        } else {
            packet
        })
    }

    /// Packet held back for the in-flight picture, if any.
    fn drain_pending(&mut self) -> Result<Vec<Packet>, VaApiError> {
        match self.pending.take() {
            Some(job) => Ok(vec![self.harvest_job(&job)?]),
            None => Ok(Vec::new()),
        }
    }

    /// Pictures per IDR under `gop`.
    fn intra_period(&self) -> u32 {
        match self.gop {
            GopShape::AllIntra => 1,
            GopShape::LowDelay { intra_period } => intra_period.max(1),
        }
    }

    /// Numbers for the `count`-th picture under `gop`.
    fn numbers(gop: GopShape, count: u64) -> Result<Numbers, VaApiError> {
        match gop {
            GopShape::AllIntra => Ok(Numbers {
                idr: true,
                frame_num: u16::try_from(count % 16).map_err(|_| Self::range())?,
                idr_pic_id: u16::try_from(count % 65_535).map_err(|_| Self::range())?,
                poc: u16::try_from(count.wrapping_mul(2) % 64).map_err(|_| Self::range())?,
            }),
            GopShape::LowDelay { intra_period } => {
                let period = u64::from(intra_period.max(1));
                let pos = count % period;
                Ok(Numbers {
                    idr: pos == 0,
                    frame_num: u16::try_from(pos % 16).map_err(|_| Self::range())?,
                    idr_pic_id: u16::try_from(count / period % 65_535)
                        .map_err(|_| Self::range())?,
                    poc: u16::try_from(pos.wrapping_mul(2) % 64).map_err(|_| Self::range())?,
                })
            }
        }
    }

    /// NV12 frame bytes onto the `slot` upload surface.
    fn upload(&mut self, frame: &CpuFrame, slot: usize) -> Result<ffi::VaSurfaceId, VaApiError> {
        if frame.format() != PixelFormat::Nv12 || frame.size() != self.visible {
            return Err(VaApiError::Unsupported(format!(
                "{} encode needs nv12 at {}x{}",
                self.codec,
                self.visible.width(),
                self.visible.height(),
            )));
        }
        let display = self.session.display;
        let surface = self.session.surfaces[slot];
        self.copy_into(&self.upload[slot], frame)?;
        if !self.derived[slot] {
            // Sound: the surface and image are live; the rectangles fit both.
            sys::status(unsafe {
                sys::vaPutImage(
                    display,
                    surface,
                    self.upload[slot].image_id,
                    0,
                    0,
                    self.visible.width(),
                    self.visible.height(),
                    0,
                    0,
                    self.visible.width(),
                    self.visible.height(),
                )
            })?;
        }
        Ok(surface)
    }

    /// Coded-bitstream buffers for both slots.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses a coded buffer.
    fn coded_buffers(
        display: ffi::VaDisplay,
        context: ffi::VaContextId,
        coded: (u32, u32),
    ) -> Result<[VaBuffer; 2], VaApiError> {
        let len = Self::coded_len(coded)?;
        Ok([
            VaBuffer::empty(display, context, ffi::VA_ENC_CODED_BUFFER_TYPE, len)?,
            VaBuffer::empty(display, context, ffi::VA_ENC_CODED_BUFFER_TYPE, len)?,
        ])
    }

    /// Upload images for both slots, derived where the driver allows.
    ///
    /// Each image pairs with its upload surface; the flags report the
    /// derived slots, which skip the blit.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses both the derive and the
    /// NV12 upload for a slot.
    fn upload_images(
        display: ffi::VaDisplay,
        surfaces: &[ffi::VaSurfaceId],
        format: &mut ffi::VaImageFormat,
        coded: (u32, u32),
    ) -> Result<([ffi::VaImage; 2], [bool; 2]), VaApiError> {
        let mut slot = |index: usize| -> Result<(ffi::VaImage, bool), VaApiError> {
            let surface = surfaces[index];
            match Self::derive_image(display, surface) {
                Ok(image) => Ok((image, true)),
                Err(_) => Ok((Self::upload_image(display, format, coded)?, false)),
            }
        };
        let (first, first_derived) = slot(0)?;
        let (second, second_derived) = slot(1)?;
        Ok(([first, second], [first_derived, second_derived]))
    }

    /// NV12 image derived from `surface`, zero-copy when the driver allows.
    fn derive_image(
        display: ffi::VaDisplay,
        surface: ffi::VaSurfaceId,
    ) -> Result<ffi::VaImage, VaApiError> {
        let mut image = std::mem::MaybeUninit::<ffi::VaImage>::uninit();
        // Sound: the display and surface are live; the image outlives the call.
        sys::status(unsafe { sys::vaDeriveImage(display, surface, image.as_mut_ptr()) })?;
        // Sound: `vaDeriveImage` succeeded, so the image is initialized.
        Ok(unsafe { image.assume_init() })
    }

    /// NV12 upload image at the coded size.
    fn upload_image(
        display: ffi::VaDisplay,
        format: &mut ffi::VaImageFormat,
        coded: (u32, u32),
    ) -> Result<ffi::VaImage, VaApiError> {
        let width_c = c_int::try_from(coded.0).map_err(|_| Self::range())?;
        let height_c = c_int::try_from(coded.1).map_err(|_| Self::range())?;
        let mut image = std::mem::MaybeUninit::<ffi::VaImage>::uninit();
        // Sound: the display is initialized; the format and image outlive the call.
        sys::status(unsafe {
            sys::vaCreateImage(display, format, width_c, height_c, image.as_mut_ptr())
        })?;
        // Sound: `vaCreateImage` succeeded, so the image is initialized.
        Ok(unsafe { image.assume_init() })
    }

    /// NV12 planes of `frame` into the mapped `image`.
    fn copy_into(&self, image: &ffi::VaImage, frame: &CpuFrame) -> Result<(), VaApiError> {
        if image.num_planes < 2 {
            return Err(VaApiError::Unsupported("upload image not nv12".to_owned()));
        }
        let planes = frame.planes();
        if planes.len() != 2 {
            return Err(VaApiError::Unsupported("upload frame not nv12".to_owned()));
        }
        let width = usize::try_from(self.visible.width()).map_err(|_| Self::range())?;
        let height = usize::try_from(self.visible.height()).map_err(|_| Self::range())?;
        let luma = &planes[0];
        let chroma = &planes[1];
        let luma_stride = usize::try_from(luma.stride()).map_err(|_| Self::range())?;
        let chroma_stride = usize::try_from(chroma.stride()).map_err(|_| Self::range())?;
        if luma_stride < width
            || chroma_stride < width
            || luma.data().len() < luma_stride.saturating_mul(height)
            || chroma.data().len() < chroma_stride.saturating_mul(height) / 2
        {
            return Err(VaApiError::Unsupported(
                "upload planes short of their strides".to_owned(),
            ));
        }
        let len = u64::from(image.data_size);
        let end_y = u64::from(image.offsets[0])
            .saturating_add(u64::from(image.pitches[0]) * u64::from(self.visible.height()));
        let end_uv = u64::from(image.offsets[1])
            .saturating_add(u64::from(image.pitches[1]) * u64::from(self.visible.height() / 2));
        if end_y > len || end_uv > len {
            return Err(VaApiError::Unsupported("upload image layout".to_owned()));
        }
        let y_base = usize::try_from(image.offsets[0]).map_err(|_| Self::range())?;
        let y_pitch = usize::try_from(image.pitches[0]).map_err(|_| Self::range())?;
        let uv_base = usize::try_from(image.offsets[1]).map_err(|_| Self::range())?;
        let uv_pitch = usize::try_from(image.pitches[1]).map_err(|_| Self::range())?;
        if width > y_pitch || width > uv_pitch {
            return Err(VaApiError::Unsupported("upload image layout".to_owned()));
        }
        let display = self.session.display;
        // Sound: the image buffer is live until `vaDestroyImage`.
        let mut data: *mut c_void = std::ptr::null_mut();
        sys::status(unsafe { sys::vaMapBuffer(display, image.buf, &raw mut data) })?;
        // Sound: every row below was bounds-checked against both sides above.
        unsafe {
            let base = data.cast::<u8>();
            for row in 0..height {
                let src = luma.data().as_ptr().add(row * luma_stride);
                let dst = base.add(y_base + y_pitch * row);
                std::ptr::copy_nonoverlapping(src, dst, width);
            }
            for row in 0..height / 2 {
                let src = chroma.data().as_ptr().add(row * chroma_stride);
                let dst = base.add(uv_base + uv_pitch * row);
                std::ptr::copy_nonoverlapping(src, dst, width);
            }
            sys::vaUnmapBuffer(display, image.buf);
        }
        Ok(())
    }

    /// Coded bytes harvested from the coded buffer.
    fn harvest(&self, coded: ffi::VaBufferId) -> Result<Vec<u8>, VaApiError> {
        let display = self.session.display;
        // Sound: the coded buffer is live until the session drops.
        let mut mapped: *mut c_void = std::ptr::null_mut();
        sys::status(unsafe { sys::vaMapBuffer(display, coded, &raw mut mapped) })?;
        let mut out = Vec::with_capacity(1 << 16);
        let mut segment = mapped.cast_const().cast::<ffi::VaCodedBufferSegment>();
        let mut failed = false;
        // Sound: the driver links mapped coded segments until null.
        unsafe {
            while !segment.is_null() {
                let current = &*segment;
                if current.bit_offset % 8 != 0 {
                    failed = true;
                    break;
                }
                let skip = (current.bit_offset / 8) as usize;
                let len = current.size as usize;
                if len > skip && !current.buf.is_null() {
                    let bytes = std::slice::from_raw_parts(
                        current.buf.cast_const().cast::<u8>().add(skip),
                        len - skip,
                    );
                    out.extend_from_slice(bytes);
                }
                segment = current
                    .next
                    .cast_const()
                    .cast::<ffi::VaCodedBufferSegment>();
            }
            sys::vaUnmapBuffer(display, coded);
        }
        if failed {
            return Err(VaApiError::Unsupported(
                "unaligned coded segment".to_owned(),
            ));
        }
        if out.is_empty() {
            return Err(VaApiError::Unsupported(
                "encoder produced no bytes".to_owned(),
            ));
        }
        Ok(out)
    }

    /// Picture header packed for `qp`, skipped when already packed.
    fn pack_pps(&mut self, qp: u8) -> Result<(), VaApiError> {
        if self.packed_qp == Some(qp) {
            return Ok(());
        }
        match self.codec {
            EncodeCodec::H264 => Self::h264_pps(qp, &mut self.writer),
            EncodeCodec::H265 => Self::hevc_pps(qp, &mut self.writer),
        }
        let bits = self.writer.bit_len();
        let recreated = self.pps.pack(
            ffi::VA_ENC_PACKED_HEADER_TYPE_PICTURE,
            self.writer.bytes(),
            bits,
        )?;
        self.creates += recreated;
        self.writer.clear();
        self.packed_qp = Some(qp);
        Ok(())
    }

    /// Render list for one H.264 picture on `recon`.
    fn render_h264(
        &mut self,
        recon: ffi::VaSurfaceId,
        numbers: &Numbers,
        reference: Option<Reference>,
        coded: ffi::VaBufferId,
    ) -> Result<(), VaApiError> {
        let mbw = self.coded.0 / 16;
        let mbh = self.coded.1 / 16;
        let qp = self.qp;
        self.pack_pps(qp)?;
        if numbers.idr {
            Self::h264_slice(
                numbers.frame_num,
                numbers.idr_pic_id,
                numbers.poc,
                &mut self.writer,
            );
        } else {
            Self::h264_p_slice(numbers.frame_num, numbers.poc, &mut self.writer);
        }
        let slice_bits = self.writer.bit_len();
        let recreated = self.slice_packed.pack(
            ffi::VA_ENC_PACKED_HEADER_TYPE_SLICE,
            self.writer.bytes(),
            slice_bits,
        )?;
        self.creates += recreated;
        self.writer.clear();
        let seq = self.h264_seq(mbw, mbh)?;
        self.creates += u64::from(self.seq.rewrite_uninit(bytes_of(&seq))?);
        let mut dpb = [Self::h264_invalid(); 16];
        if let Some(prev) = reference.filter(|_| !numbers.idr) {
            dpb[0] = Self::h264_ref(prev);
        }
        let pic_fields = if numbers.idr {
            self.words.pic_idr
        } else {
            self.words.pic_p
        };
        let pic = ffi::VaEncPictureParameterBufferH264 {
            curr_pic: ffi::VaPictureH264 {
                picture_id: recon,
                frame_idx: u32::from(numbers.frame_num),
                flags: ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE,
                top_field_order_cnt: i32::from(numbers.poc),
                bottom_field_order_cnt: i32::from(numbers.poc),
                ..Default::default()
            },
            reference_frames: dpb,
            coded_buf: coded,
            frame_num: numbers.frame_num,
            pic_init_qp: qp,
            pic_fields,
            ..Default::default()
        };
        self.creates += u64::from(self.slice_pic.rewrite_uninit(bytes_of(&pic))?);
        let mbs = u64::from(mbw) * u64::from(mbh);
        let mut list0 = [Self::h264_invalid(); 32];
        if let Some(prev) = reference.filter(|_| !numbers.idr) {
            list0[0] = Self::h264_ref(prev);
        }
        let slice = ffi::VaEncSliceParameterBufferH264 {
            num_macroblocks: u32::try_from(mbs).map_err(|_| Self::range())?,
            macroblock_info: ffi::VA_INVALID_ID,
            slice_type: if numbers.idr { 2 } else { 0 },
            idr_pic_id: if numbers.idr { numbers.idr_pic_id } else { 0 },
            pic_order_cnt_lsb: numbers.poc,
            ref_pic_list0: list0,
            ref_pic_list1: [Self::h264_invalid(); 32],
            ..Default::default()
        };
        self.creates += u64::from(self.slice.rewrite_uninit(bytes_of(&slice))?);
        self.render.clear();
        self.render.extend([
            self.seq.id(),
            self.sps.param.id(),
            self.sps.data.id(),
            self.pps.param.id(),
            self.pps.data.id(),
            self.slice_pic.id(),
            self.slice.id(),
            self.slice_packed.param.id(),
            self.slice_packed.data.id(),
        ]);
        Ok(())
    }

    /// Render list for one H.265 picture on `recon`.
    fn render_h265(
        &mut self,
        recon: ffi::VaSurfaceId,
        numbers: &Numbers,
        reference: Option<Reference>,
        coded: ffi::VaBufferId,
    ) -> Result<(), VaApiError> {
        let ctus_w = self.coded.0.div_ceil(64);
        let ctus_h = self.coded.1.div_ceil(64);
        let ctus =
            u32::try_from(u64::from(ctus_w) * u64::from(ctus_h)).map_err(|_| Self::range())?;
        let qp = self.qp;
        self.pack_pps(qp)?;
        if numbers.idr {
            Self::hevc_slice(&mut self.writer);
        } else {
            Self::hevc_p_slice(numbers.poc, &mut self.writer);
        }
        let slice_bits = self.writer.bit_len();
        let recreated = self.slice_packed.pack(
            ffi::VA_ENC_PACKED_HEADER_TYPE_SLICE,
            self.writer.bytes(),
            slice_bits,
        )?;
        self.creates += recreated;
        self.writer.clear();
        let period = self.intra_period();
        let seq = ffi::VaEncSequenceParameterBufferHevc {
            general_profile_idc: 1,
            general_level_idc: self.level,
            intra_period: period,
            intra_idr_period: period,
            ip_period: 1,
            pic_width_in_luma_samples: u16::try_from(self.coded.0).map_err(|_| Self::range())?,
            pic_height_in_luma_samples: u16::try_from(self.coded.1).map_err(|_| Self::range())?,
            seq_fields: self.words.seq,
            log2_diff_max_min_luma_coding_block_size: 3,
            log2_diff_max_min_transform_block_size: 3,
            max_transform_hierarchy_depth_inter: 2,
            max_transform_hierarchy_depth_intra: 2,
            ..Default::default()
        };
        self.creates += u64::from(self.seq.rewrite_uninit(bytes_of(&seq))?);
        let mut dpb = [Self::hevc_invalid(); 15];
        if let Some(prev) = reference.filter(|_| !numbers.idr) {
            dpb[0] = Self::hevc_ref(prev);
        }
        let pic_fields = if numbers.idr {
            self.words.pic_idr
        } else {
            self.words.pic_p
        };
        let pic = ffi::VaEncPictureParameterBufferHevc {
            decoded_curr_pic: ffi::VaPictureHevc {
                picture_id: recon,
                pic_order_cnt: i32::from(numbers.poc),
                ..Default::default()
            },
            reference_frames: dpb,
            coded_buf: coded,
            collocated_ref_pic_index: 0xFF,
            pic_init_qp: qp,
            nal_unit_type: if numbers.idr { 19 } else { 1 },
            pic_fields,
            ..Default::default()
        };
        self.creates += u64::from(self.slice_pic.rewrite_uninit(bytes_of(&pic))?);
        let mut list0 = [Self::hevc_invalid(); 15];
        if let Some(prev) = reference.filter(|_| !numbers.idr) {
            list0[0] = Self::hevc_ref(prev);
        }
        let slice = ffi::VaEncSliceParameterBufferHevc {
            num_ctu_in_slice: ctus,
            slice_type: if numbers.idr { 2 } else { 1 },
            ref_pic_list0: list0,
            ref_pic_list1: [Self::hevc_invalid(); 15],
            max_num_merge_cand: 5,
            slice_fields: self.words.slice,
            ..Default::default()
        };
        self.creates += u64::from(self.slice.rewrite_uninit(bytes_of(&slice))?);
        self.render.clear();
        if let Some(vps) = &self.vps {
            self.render.push(vps.param.id());
            self.render.push(vps.data.id());
        }
        self.render.extend([
            self.seq.id(),
            self.sps.param.id(),
            self.sps.data.id(),
            self.pps.param.id(),
            self.pps.data.id(),
            self.slice_pic.id(),
            self.slice.id(),
            self.slice_packed.param.id(),
            self.slice_packed.data.id(),
        ]);
        Ok(())
    }

    /// Invalid H.264 reference picture marker.
    fn h264_invalid() -> ffi::VaPictureH264 {
        ffi::VaPictureH264 {
            picture_id: ffi::VA_INVALID_ID,
            frame_idx: 0,
            flags: ffi::VA_PICTURE_H264_INVALID,
            top_field_order_cnt: 0,
            bottom_field_order_cnt: 0,
            reserved: [0; 4],
        }
    }

    /// H.264 sequence parameters at `mbw` by `mbh` macroblocks.
    fn h264_seq(
        &self,
        mbw: u32,
        mbh: u32,
    ) -> Result<ffi::VaEncSequenceParameterBufferH264, VaApiError> {
        let crop = self.coded != (self.visible.width(), self.visible.height());
        let period = self.intra_period();
        Ok(ffi::VaEncSequenceParameterBufferH264 {
            seq_parameter_set_id: 0,
            level_idc: self.level,
            intra_period: period,
            intra_idr_period: period,
            ip_period: 1,
            bits_per_second: 0,
            max_num_ref_frames: 1,
            picture_width_in_mbs: u16::try_from(mbw).map_err(|_| Self::range())?,
            picture_height_in_mbs: u16::try_from(mbh).map_err(|_| Self::range())?,
            seq_fields: self.words.seq,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: [0; 256],
            frame_cropping_flag: u8::from(crop),
            frame_crop_left_offset: 0,
            frame_crop_right_offset: (self.coded.0 - self.visible.width()) / 2,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: (self.coded.1 - self.visible.height()) / 2,
            vui_parameters_present_flag: 1,
            vui_fields: Self::H264_VUI_FIELDS,
            aspect_ratio_idc: 0,
            sar_width: 0,
            sar_height: 0,
            num_units_in_tick: 0,
            time_scale: 0,
            va_reserved: [0; 4],
        })
    }

    /// H.264 short-term reference from `prev`.
    fn h264_ref(prev: Reference) -> ffi::VaPictureH264 {
        ffi::VaPictureH264 {
            picture_id: prev.surface,
            frame_idx: u32::from(prev.frame_num),
            flags: ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            top_field_order_cnt: i32::from(prev.poc),
            bottom_field_order_cnt: i32::from(prev.poc),
            reserved: [0; 4],
        }
    }

    /// Invalid HEVC reference picture marker.
    fn hevc_invalid() -> ffi::VaPictureHevc {
        ffi::VaPictureHevc {
            picture_id: ffi::VA_INVALID_ID,
            pic_order_cnt: 0,
            flags: ffi::VA_PICTURE_HEVC_INVALID,
            reserved: [0; 4],
        }
    }

    /// HEVC short-term reference from `prev`.
    fn hevc_ref(prev: Reference) -> ffi::VaPictureHevc {
        ffi::VaPictureHevc {
            picture_id: prev.surface,
            pic_order_cnt: i32::from(prev.poc),
            flags: ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE,
            reserved: [0; 4],
        }
    }

    /// H.264 sequence header for `coded` showing `visible` at `level`.
    fn h264_sps(coded: (u32, u32), visible: (u32, u32), level: u8, writer: &mut BitWriter) {
        writer.start_code(0x67);
        writer.bits(100, 8);
        writer.bits(0, 8);
        writer.bits(u32::from(level), 8);
        writer.ue(0);
        writer.ue(1);
        writer.ue(0);
        writer.ue(0);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.ue(0);
        writer.ue(2);
        writer.ue(1);
        writer.bits(0, 1);
        writer.ue(coded.0 / 16 - 1);
        writer.ue(coded.1 / 16 - 1);
        writer.bits(1, 1);
        writer.bits(1, 1);
        let crop = u32::from(coded != visible);
        writer.bits(crop, 1);
        if crop != 0 {
            writer.ue(0);
            writer.ue((coded.0 - visible.0) / 2);
            writer.ue(0);
            writer.ue((coded.1 - visible.1) / 2);
        }
        writer.bits(1, 1);
        Self::h264_vui(writer);
        writer.rbsp_trailing();
    }

    /// H.264 low-delay VUI: no reorder, two frame buffers.
    fn h264_vui(writer: &mut BitWriter) {
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(1, 1);
        writer.bits(1, 1);
        writer.ue(0);
        writer.ue(0);
        writer.ue(Self::VUI_MV_LEN);
        writer.ue(Self::VUI_MV_LEN);
        writer.ue(0);
        writer.ue(2);
    }

    /// H.264 picture header for `qp`.
    fn h264_pps(qp: u8, writer: &mut BitWriter) {
        writer.start_code(0x68);
        writer.ue(0);
        writer.ue(0);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bits(0, 1);
        writer.bits(0, 2);
        writer.se(i32::from(qp) - 26);
        writer.se(0);
        writer.se(0);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.se(0);
        writer.rbsp_trailing();
    }

    /// H.264 IDR slice header.
    fn h264_slice(frame_num: u16, idr_pic_id: u16, poc_lsb: u16, writer: &mut BitWriter) {
        writer.start_code(0x65);
        writer.ue(0);
        writer.ue(2);
        writer.ue(0);
        writer.bits(u32::from(frame_num), 4);
        writer.ue(u32::from(idr_pic_id));
        writer.bits(u32::from(poc_lsb), 6);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.se(0);
        writer.align_zero();
    }

    /// H.264 P slice header with default sliding-window marking.
    fn h264_p_slice(frame_num: u16, poc_lsb: u16, writer: &mut BitWriter) {
        writer.start_code(0x41);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bits(u32::from(frame_num), 4);
        writer.bits(u32::from(poc_lsb), 6);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.se(0);
        writer.align_zero();
    }

    /// H.265 profile-tier-level for `level`.
    fn hevc_ptl(level: u8, writer: &mut BitWriter) {
        writer.bits(0, 2);
        writer.bits(0, 1);
        writer.bits(1, 5);
        writer.bits(0x6000_0000, 32);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.bits(1, 1);
        writer.bits(1, 1);
        writer.bits(0, 32);
        writer.bits(0, 12);
        writer.bits(u32::from(level), 8);
    }

    /// H.265 video header for `level`.
    fn hevc_vps(level: u8, writer: &mut BitWriter) {
        writer.start_code16(32);
        writer.bits(0, 4);
        writer.bits(3, 2);
        writer.bits(0, 6);
        writer.bits(0, 3);
        writer.bits(1, 1);
        writer.bits(0xFFFF, 16);
        Self::hevc_ptl(level, writer);
        writer.bits(0, 1);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bits(0, 6);
        writer.ue(0);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.rbsp_trailing();
    }

    /// H.265 sequence header for `coded` showing `visible` at `level`.
    fn hevc_sps(coded: (u32, u32), visible: (u32, u32), level: u8, writer: &mut BitWriter) {
        writer.start_code16(33);
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bits(1, 1);
        Self::hevc_ptl(level, writer);
        writer.ue(0);
        writer.ue(1);
        writer.ue(coded.0);
        writer.ue(coded.1);
        let crop = u32::from(coded != visible);
        writer.bits(crop, 1);
        if crop != 0 {
            writer.ue(0);
            writer.ue((coded.0 - visible.0) / 2);
            writer.ue(0);
            writer.ue((coded.1 - visible.1) / 2);
        }
        writer.ue(0);
        writer.ue(0);
        writer.ue(2);
        writer.bits(0, 1);
        writer.ue(1);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        writer.ue(3);
        writer.ue(2);
        writer.ue(2);
        writer.bits(0, 1);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.ue(1);
        writer.ue(1);
        writer.ue(0);
        writer.ue(1);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.rbsp_trailing();
    }

    /// H.265 picture header for `qp`.
    fn hevc_pps(qp: u8, writer: &mut BitWriter) {
        writer.start_code16(34);
        writer.ue(0);
        writer.ue(0);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 3);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.ue(0);
        writer.se(i32::from(qp) - 26);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.se(0);
        writer.se(0);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.bits(0, 1);
        writer.bits(0, 1);
        writer.rbsp_trailing();
    }

    /// H.265 single-slice IDR header; a first slice carries no address.
    fn hevc_slice(writer: &mut BitWriter) {
        writer.start_code16(19);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.ue(2);
        writer.se(0);
        writer.se(0);
        writer.se(0);
        writer.align_zero();
    }

    /// H.265 single-slice P header over the SPS reference set.
    fn hevc_p_slice(poc_lsb: u16, writer: &mut BitWriter) {
        writer.start_code16(1);
        writer.bits(1, 1);
        writer.ue(0);
        writer.ue(1);
        writer.bits(u32::from(poc_lsb), 6);
        writer.bits(1, 1);
        writer.bits(0, 1);
        writer.ue(0);
        writer.se(0);
        writer.align_zero();
    }

    /// NV12 image format the display uploads through.
    fn nv12_format(device: &VaApiDevice) -> Result<ffi::VaImageFormat, VaApiError> {
        let display = device.display();
        // Sound: the format list is sized by its capacity query.
        unsafe {
            let capacity = usize::try_from(sys::vaMaxNumImageFormats(display)).unwrap_or(0);
            let mut formats = vec![ffi::VaImageFormat::default(); capacity];
            let mut count = 0;
            sys::status(sys::vaQueryImageFormats(
                display,
                formats.as_mut_ptr(),
                &raw mut count,
            ))?;
            let reported = usize::try_from(count).unwrap_or(0).min(formats.len());
            formats[..reported]
                .iter()
                .find(|format| format.fourcc == ffi::VA_FOURCC_NV12)
                .copied()
                .ok_or_else(|| VaApiError::Unsupported("nv12 image format absent".to_owned()))
        }
    }

    /// Level idc covering `coded` for `codec`.
    fn level(codec: EncodeCodec, coded_size: (u32, u32)) -> u8 {
        let samples = u64::from(coded_size.0) * u64::from(coded_size.1);
        match codec {
            EncodeCodec::H264 => {
                let mbs = samples / 256;
                if mbs <= 396 {
                    22
                } else if mbs <= 1620 {
                    30
                } else if mbs <= 3600 {
                    31
                } else if mbs <= 5120 {
                    32
                } else if mbs <= 8192 {
                    41
                } else if mbs <= 22_080 {
                    50
                } else {
                    51
                }
            }
            EncodeCodec::H265 => {
                if samples <= 36_864 {
                    30
                } else if samples <= 122_880 {
                    60
                } else if samples <= 245_760 {
                    63
                } else if samples <= 552_960 {
                    90
                } else if samples <= 983_040 {
                    93
                } else if samples <= 2_228_224 {
                    120
                } else if samples <= 8_912_896 {
                    150
                } else {
                    153
                }
            }
        }
    }

    /// Coded-buffer bytes for `coded`.
    fn coded_len(coded: (u32, u32)) -> Result<usize, VaApiError> {
        let bytes = u64::from(coded.0) * u64::from(coded.1) * 3;
        let bytes = bytes.max(1 << 20);
        usize::try_from(bytes).map_err(|_| Self::range())
    }

    /// Dimensions outside the driver range.
    fn range() -> VaApiError {
        VaApiError::Unsupported("dimensions outside driver range".to_owned())
    }
}

impl Encoder for VaApiEncoder {
    type Error = VaApiError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(Some(ShareMechanism::DmaBuf), false, true)
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, VaApiError> {
        let slot = self.next_slot;
        self.next_slot = (self.next_slot + 1) % Self::UPLOADS;
        let surface = self.upload(frame, slot)?;
        let packets = self.drain_pending()?;
        self.pending = Some(self.submit_job(surface, slot)?);
        Ok(packets)
    }

    fn flush(&mut self) -> Result<Vec<Packet>, VaApiError> {
        self.drain_pending()
    }
}

/// Owned encode session: config, context, and surfaces.
struct Session {
    /// Initialized display.
    display: ffi::VaDisplay,
    /// Encode configuration.
    config: ffi::VaConfigId,
    /// Encode context.
    context: ffi::VaContextId,
    /// Upload surface plus reconstruction surfaces.
    surfaces: Vec<ffi::VaSurfaceId>,
    /// Surface count as the driver takes it.
    count_c: c_int,
}

impl Session {
    /// Session for `profile` at `coded`, with upload and recon surfaces.
    fn open(device: &VaApiDevice, profile: c_int, coded: (u32, u32)) -> Result<Self, VaApiError> {
        let display = device.display();
        let width_c = c_int::try_from(coded.0)
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let height_c = c_int::try_from(coded.1)
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let mut attribs = [
            ffi::VaConfigAttrib {
                attr_type: ffi::VA_CONFIG_ATTRIB_RT_FORMAT,
                value: ffi::VA_RT_FORMAT_YUV420,
            },
            ffi::VaConfigAttrib {
                attr_type: ffi::VA_CONFIG_ATTRIB_RATE_CONTROL,
                value: ffi::VA_RC_CQP,
            },
            ffi::VaConfigAttrib {
                attr_type: ffi::VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS,
                value: ffi::VA_ENC_PACKED_HEADER_SEQUENCE
                    | ffi::VA_ENC_PACKED_HEADER_PICTURE
                    | ffi::VA_ENC_PACKED_HEADER_SLICE,
            },
        ];
        let mut config = 0;
        // Sound: the display is initialized; the attributes outlive the call.
        sys::status(unsafe {
            sys::vaCreateConfig(
                display,
                profile,
                ffi::VA_ENTRYPOINT_ENC_SLICE,
                attribs.as_mut_ptr(),
                3,
                &raw mut config,
            )
        })?;
        let count = VaApiEncoder::UPLOADS + VaApiEncoder::RECONS;
        let mut surfaces = vec![0; count];
        // Sound: the surface array holds `count` live entries.
        let status = unsafe {
            sys::vaCreateSurfaces(
                display,
                ffi::VA_RT_FORMAT_YUV420,
                coded.0,
                coded.1,
                surfaces.as_mut_ptr(),
                u32::try_from(count).map_err(|_| {
                    VaApiError::Unsupported("dimensions outside driver range".to_owned())
                })?,
                std::ptr::null_mut(),
                0,
            )
        };
        if let Err(error) = sys::status(status) {
            // Sound: `config` was created above.
            unsafe {
                sys::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        let count_c = c_int::try_from(count)
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let mut context = 0;
        // Sound: the surface array outlives context creation.
        let status = unsafe {
            sys::vaCreateContext(
                display,
                config,
                width_c,
                height_c,
                ffi::VA_PROGRESSIVE,
                surfaces.as_mut_ptr(),
                count_c,
                &raw mut context,
            )
        };
        if let Err(error) = sys::status(status) {
            // Sound: the surfaces and config were created above.
            unsafe {
                sys::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                sys::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        Ok(Self {
            display,
            config,
            context,
            surfaces,
            count_c,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Sound: context, surfaces, and config were created in `open`.
        unsafe {
            sys::vaDestroyContext(self.display, self.context);
            sys::vaDestroySurfaces(self.display, self.surfaces.as_mut_ptr(), self.count_c);
            sys::vaDestroyConfig(self.display, self.config);
        }
    }
}

// Sound: a move hands exclusive ownership to the new thread; the
// session destroys wherever the value drops, never shared.
unsafe impl Send for VaApiEncoder {}

impl Drop for VaApiEncoder {
    fn drop(&mut self) {
        // Sound: the images were created in `open`; the session outlives
        // this drop, so the display is still initialized.
        unsafe {
            for image in &self.upload {
                sys::vaDestroyImage(self.session.display, image.image_id);
            }
        }
    }
}

/// Session-fixed packed parameter words.
struct Words {
    /// Packed sequence flags.
    seq: u32,
    /// Packed IDR picture flags.
    pic_idr: u32,
    /// Packed P picture flags.
    pic_p: u32,
    /// Packed slice flags.
    slice: u32,
}

impl Words {
    /// Fixed words for `codec`; per-picture values ride struct fields.
    fn new(codec: EncodeCodec) -> Self {
        match codec {
            EncodeCodec::H264 => Self {
                seq: (1u32 << VaApiEncoder::H264_SEQ_CHROMA)
                    | (1u32 << VaApiEncoder::H264_SEQ_MBS_ONLY)
                    | (1u32 << VaApiEncoder::H264_SEQ_DIRECT_8X8)
                    | (2u32 << VaApiEncoder::H264_SEQ_LOG2_POC_LSB),
                pic_idr: (1u32 << VaApiEncoder::H264_PIC_IDR)
                    | (1u32 << VaApiEncoder::H264_PIC_REFERENCE)
                    | (1u32 << VaApiEncoder::H264_PIC_ENTROPY)
                    | (1u32 << VaApiEncoder::H264_PIC_TRANSFORM_8X8),
                pic_p: (1u32 << VaApiEncoder::H264_PIC_REFERENCE)
                    | (1u32 << VaApiEncoder::H264_PIC_ENTROPY)
                    | (1u32 << VaApiEncoder::H264_PIC_TRANSFORM_8X8),
                slice: 0,
            },
            EncodeCodec::H265 => Self {
                seq: (1u32 << VaApiEncoder::H265_SEQ_CHROMA)
                    | (1u32 << VaApiEncoder::H265_SEQ_STRONG_INTRA)
                    | (1u32 << VaApiEncoder::H265_SEQ_AMP),
                pic_idr: (1u32 << VaApiEncoder::H265_PIC_IDR)
                    | (1u32 << VaApiEncoder::H265_PIC_CODING_TYPE)
                    | (1u32 << VaApiEncoder::H265_PIC_REFERENCE)
                    | (1u32 << VaApiEncoder::H265_PIC_SIGN_DATA)
                    | (1u32 << VaApiEncoder::H265_PIC_LOOP_TILES),
                pic_p: (2u32 << VaApiEncoder::H265_PIC_CODING_TYPE)
                    | (1u32 << VaApiEncoder::H265_PIC_REFERENCE)
                    | (1u32 << VaApiEncoder::H265_PIC_SIGN_DATA)
                    | (1u32 << VaApiEncoder::H265_PIC_LOOP_TILES),
                slice: 1 << VaApiEncoder::H265_SLICE_LAST,
            },
        }
    }
}

/// Numbers for one picture in coding order.
struct Numbers {
    /// Picture starts a new GOP.
    idr: bool,
    /// Frame number modulo the sequence range.
    frame_num: u16,
    /// IDR identifier, counting IDRs.
    idr_pic_id: u16,
    /// Order count modulo the lsb range.
    poc: u16,
}

/// Previous picture as next P reference.
#[derive(Clone, Copy)]
struct Reference {
    /// Reconstruction surface holding the picture.
    surface: ffi::VaSurfaceId,
    /// Frame number of the picture.
    frame_num: u16,
    /// Order count of the picture.
    poc: u16,
}

/// Submitted picture awaiting harvest.
struct PendingJob {
    /// Input surface the sync waits on.
    source: ffi::VaSurfaceId,
    /// Coded buffer the harvest maps.
    coded: ffi::VaBufferId,
    /// Picture starts a new GOP.
    idr: bool,
}

/// Packed header parameter and data buffers.
struct PackedPair {
    /// Header parameter buffer.
    param: VaBuffer,
    /// Header data buffer.
    data: VaBuffer,
}

impl PackedPair {
    /// Empty pair, packed before first render.
    fn empty(display: ffi::VaDisplay, context: ffi::VaContextId) -> Result<Self, VaApiError> {
        Ok(Self {
            param: VaBuffer::empty(
                display,
                context,
                ffi::VA_ENC_PACKED_HEADER_PARAMETER_BUFFER_TYPE,
                1,
            )?,
            data: VaBuffer::empty(
                display,
                context,
                ffi::VA_ENC_PACKED_HEADER_DATA_BUFFER_TYPE,
                1,
            )?,
        })
    }

    /// Pack `bytes` of `bit_len` bits as `header_type`.
    ///
    /// The return counts recreated buffers.
    fn pack(&mut self, header_type: u32, bytes: &[u8], bit_len: u32) -> Result<u64, VaApiError> {
        let param = ffi::VaEncPackedHeaderParameterBuffer {
            header_type,
            bit_length: bit_len,
            has_emulation_bytes: 0,
            va_reserved: [0; 4],
        };
        let mut recreated = u64::from(self.param.rewrite_uninit(bytes_of(&param))?);
        recreated += u64::from(self.data.rewrite(bytes)?);
        Ok(recreated)
    }
}

/// Header bit writer without emulation prevention; the driver inserts it.
#[derive(Debug, Default)]
struct BitWriter {
    /// Finished bytes.
    bytes: Vec<u8>,
    /// Open byte, left-aligned.
    pending: u8,
    /// Bits in the open byte.
    filled: u8,
    /// Payload bits written, trailing included, padding excluded.
    bits: u32,
}

impl BitWriter {
    /// One bit.
    fn bit(&mut self, value: bool) {
        self.pending = (self.pending << 1) | u8::from(value);
        self.filled += 1;
        self.bits += 1;
        if self.filled == 8 {
            self.bytes.push(self.pending);
            self.pending = 0;
            self.filled = 0;
        }
    }

    /// Low `count` bits of `value`, most significant first.
    fn bits(&mut self, value: u32, count: u8) {
        debug_assert!(count <= 32);
        debug_assert!(count == 32 || u64::from(value) < (1 << count));
        for index in (0..count).rev() {
            self.bit(value & (1 << index) != 0);
        }
    }

    /// Unsigned Exp-Golomb code for `value`.
    fn ue(&mut self, value: u32) {
        self.code(u64::from(value));
    }

    /// Signed Exp-Golomb code for `value`.
    fn se(&mut self, value: i32) {
        let magnitude = u64::from(value.unsigned_abs());
        self.code(if value > 0 {
            magnitude * 2 - 1
        } else {
            magnitude * 2
        });
    }

    /// Exp-Golomb code for `code_num`.
    fn code(&mut self, code_num: u64) {
        let code = code_num + 1;
        let len = 64 - code.leading_zeros();
        for _ in 1..len {
            self.bit(false);
        }
        for index in (0..len).rev() {
            self.bit(code & (1 << index) != 0);
        }
    }

    /// Start code plus a one-byte H.264 NAL header.
    fn start_code(&mut self, nal: u8) {
        debug_assert_eq!(self.filled, 0);
        self.bytes.extend([0, 0, 1, nal]);
        self.bits += 32;
    }

    /// Start code plus a two-byte H.265 NAL header of `typ`.
    fn start_code16(&mut self, typ: u8) {
        debug_assert!(typ < 64);
        debug_assert_eq!(self.filled, 0);
        self.bytes.extend([0, 0, 1, typ << 1, 1]);
        self.bits += 40;
    }

    /// RBSP trailing bits, counted in the payload.
    fn rbsp_trailing(&mut self) {
        self.bit(true);
        while self.filled != 0 {
            self.bit(false);
        }
    }

    /// Zero padding to the byte, excluded from the payload.
    fn align_zero(&mut self) {
        if self.filled > 0 {
            self.bytes.push(self.pending << (8 - self.filled));
            self.pending = 0;
            self.filled = 0;
        }
    }

    /// Finished bytes; call after trailing or padding.
    fn bytes(&self) -> &[u8] {
        debug_assert_eq!(self.filled, 0);
        &self.bytes
    }

    /// Payload bit count.
    fn bit_len(&self) -> u32 {
        self.bits
    }

    /// Finished bytes and payload bits, resetting the writer.
    fn take(&mut self) -> (Vec<u8>, u32) {
        debug_assert_eq!(self.filled, 0);
        let bytes = std::mem::take(&mut self.bytes);
        let bits = self.bits;
        self.bits = 0;
        (bytes, bits)
    }

    /// Empty writer, reusing the allocation.
    fn clear(&mut self) {
        self.bytes.clear();
        self.pending = 0;
        self.filled = 0;
        self.bits = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{BitWriter, EncodeCodec, GopShape, VaApiEncoder, Words};
    use crate::error::VaApiError;
    use crate::h264::Gate;
    use crate::h265::Gate as Gate265;

    #[test]
    fn caps_agree_with_texture_import() {
        use rust_video_caps::common::PictureSize;
        use rust_video_caps::traits::Encoder;
        use rust_video_caps::transcode::{GpuCaps, ShareMechanism};
        use rust_video_harness::{H264Synth, Packets};

        let Ok(device) = crate::device::VaApiDevice::open() else {
            return;
        };
        let size = PictureSize::new(320, 240).expect("synth size");
        let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H264, size) {
            Ok(encoder) => encoder,
            Err(VaApiError::Unsupported(_)) => return,
            Err(error) => panic!("unexpected encode failure: {error}"),
        };
        assert_eq!(
            encoder.caps(),
            GpuCaps::new(Some(ShareMechanism::DmaBuf), false, true)
        );
        let mut decoder = crate::h264::decoder::H264Decoder::open(&device);
        let raws = H264Synth::annex_b_h264_packets();
        let packets = Packets::from_raws(&raws);
        let mut surfaces = Vec::new();
        for packet in &packets {
            surfaces.extend(decoder.decode_surface(packet).expect("synth decodes"));
        }
        surfaces.extend(decoder.flush_surface().expect("decode drains"));
        let surface = surfaces.pop().expect("first picture surfaces");
        encoder.encode_surface(&surface).expect("import runs");
    }

    #[test]
    fn writer_packs_bits_msb_first() {
        let mut writer = BitWriter::default();
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.align_zero();
        assert_eq!(writer.bytes(), &[0xA0]);
        assert_eq!(writer.bit_len(), 3);
    }

    #[test]
    fn writer_counts_rbsp_trailing() {
        let mut writer = BitWriter::default();
        writer.bits(0b101, 3);
        writer.rbsp_trailing();
        assert_eq!(writer.bytes(), &[0xB0]);
        assert_eq!(writer.bit_len(), 8);
    }

    #[test]
    fn writer_codes_exp_golomb() {
        for (value, byte, len) in [(0, 0x80, 1), (1, 0x40, 3), (2, 0x60, 3), (3, 0x20, 5)] {
            let mut writer = BitWriter::default();
            writer.ue(value);
            writer.align_zero();
            assert_eq!(writer.bytes(), &[byte]);
            assert_eq!(writer.bit_len(), len);
        }
        for (value, byte) in [(0, 0x80), (1, 0x40), (-1, 0x60), (2, 0x20), (-2, 0x28)] {
            let mut writer = BitWriter::default();
            writer.se(value);
            writer.align_zero();
            assert_eq!(writer.bytes(), &[byte]);
        }
    }

    #[test]
    fn words_pin_packed_shifts() {
        let h264 = Words::new(EncodeCodec::H264);
        assert_eq!(h264.seq, 8229);
        assert_eq!(h264.pic_idr, 267);
        assert_eq!(h264.pic_p, 266);
        assert_eq!(h264.slice, 0);
        let h265 = Words::new(EncodeCodec::H265);
        assert_eq!(h265.seq, 3073);
        assert_eq!(h265.pic_idr, 32_851);
        assert_eq!(h265.pic_p, 32_852);
        assert_eq!(h265.slice, 1);
        assert_eq!(VaApiEncoder::H264_VUI_FIELDS, 35_412);
    }

    #[test]
    fn numbers_shape_gop_periods() {
        let idr = |count| VaApiEncoder::numbers(GopShape::AllIntra, count).expect("small count");
        assert!(idr(0).idr);
        assert_eq!((idr(17).frame_num, idr(17).poc), (1, 34));
        assert_eq!(idr(65_535).idr_pic_id, 0);
        let low = |count| {
            VaApiEncoder::numbers(GopShape::LowDelay { intra_period: 60 }, count)
                .expect("small count")
        };
        let first = low(0);
        assert!(first.idr);
        assert_eq!((first.frame_num, first.idr_pic_id, first.poc), (0, 0, 0));
        let second = low(1);
        assert!(!second.idr);
        assert_eq!((second.frame_num, second.poc), (1, 2));
        assert!(!low(59).idr);
        let next = low(60);
        assert!(next.idr);
        assert_eq!((next.frame_num, next.idr_pic_id, next.poc), (0, 1, 0));
        assert!(!low(61).idr);
        let degenerate =
            VaApiEncoder::numbers(GopShape::LowDelay { intra_period: 0 }, 7).expect("small count");
        assert!(degenerate.idr);
    }

    #[test]
    fn level_covers_picture_sizes() {
        assert_eq!(VaApiEncoder::level(EncodeCodec::H264, (320, 240)), 22);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H264, (1920, 1080)), 41);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H265, (320, 240)), 60);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H265, (1920, 1080)), 120);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H265, (3840, 2160)), 150);
    }

    /// RBSP body of Annex B `bytes`: start code and header stripped.
    fn body(bytes: &[u8]) -> Vec<u8> {
        let start = bytes
            .windows(3)
            .position(|window| window == [0x00, 0x00, 0x01])
            .expect("start code")
            + 3;
        rust_video_h264::rbsp(&bytes[start + 1..]).into_owned()
    }

    /// Header byte of Annex B `bytes`.
    fn header(bytes: &[u8]) -> u8 {
        let start = bytes
            .windows(3)
            .position(|window| window == [0x00, 0x00, 0x01])
            .expect("start code")
            + 3;
        bytes[start]
    }

    /// RBSP body of H.265 Annex B `bytes`: start code and header stripped.
    fn body265(bytes: &[u8]) -> Vec<u8> {
        let start = bytes
            .windows(3)
            .position(|window| window == [0x00, 0x00, 0x01])
            .expect("start code")
            + 3;
        rust_video_h265::rbsp(&bytes[start + 2..]).into_owned()
    }

    /// Two-byte header of H.265 Annex B `bytes`.
    fn header265(bytes: &[u8]) -> [u8; 2] {
        let start = bytes
            .windows(3)
            .position(|window| window == [0x00, 0x00, 0x01])
            .expect("start code")
            + 3;
        [bytes[start], bytes[start + 1]]
    }

    /// Raw header bytes with emulation prevention inserted for the parser.
    fn annexed(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw.len() + 4);
        out.extend_from_slice(&raw[..4]);
        let mut zeros = 0;
        for byte in &raw[4..] {
            if zeros == 2 && *byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(*byte);
            zeros = if *byte == 0 { zeros + 1 } else { 0 };
        }
        out
    }

    #[test]
    fn h264_headers_round_trip_through_parser() {
        let mut writer = BitWriter::default();
        VaApiEncoder::h264_sps((320, 240), (320, 240), 22, &mut writer);
        let (sps_bytes, sps_bits) = writer.take();
        assert_eq!(
            usize::try_from(sps_bits).expect("small header"),
            sps_bytes.len() * 8
        );
        let sps_bytes = annexed(&sps_bytes);
        let sps_body = body(&sps_bytes);
        let sps = rust_video_h264::parse_sps::<VaApiError, Gate>(&sps_body).expect("sps");
        assert_eq!(sps.profile_idc, 100);
        assert_eq!((sps.width(), sps.height()), (320, 240));
        assert!(matches!(sps.poc_type, rust_video_h264::PocType::Lsb));
        assert_eq!(sps.log2_max_frame_num_minus4, 0);
        let vui = sps.vui.as_ref().expect("low-delay vui");
        assert_eq!(vui.max_num_reorder_frames, Some(0));
        assert_eq!(vui.max_dec_frame_buffering, Some(2));

        VaApiEncoder::h264_pps(26, &mut writer);
        let (pps_bytes, _) = writer.take();
        let pps_bytes = annexed(&pps_bytes);
        let pps_body = body(&pps_bytes);
        let pps =
            rust_video_h264::parse_pps::<VaApiError, Gate>(&pps_body, Some(&sps)).expect("pps");
        assert!(pps.entropy());
        assert!(pps.transform_8x8());
        assert_eq!(pps.pic_init_qp, 0);

        VaApiEncoder::h264_slice(3, 7, 6, &mut writer);
        let (slice_bytes, slice_bits) = writer.take();
        let slice_bytes = annexed(&slice_bytes);
        let head = header(&slice_bytes);
        let slice_body = body(&slice_bytes);
        let mut reader = rust_video_h264::BitReader::new(&slice_body);
        let prefix =
            rust_video_h264::parse_slice_prefix::<VaApiError>(&mut reader).expect("prefix");
        let slice = rust_video_h264::parse_slice_rest::<VaApiError, Gate>(
            &mut reader,
            prefix,
            head & 0x1F == 5,
            (head >> 5) & 3,
            &sps,
            &pps,
        )
        .expect("slice");
        assert_eq!(slice.slice_type, rust_video_h264::SliceType::I);
        assert_eq!(slice.frame_num, 3);
        assert_eq!(slice.idr_pic_id, Some(7));
        assert_eq!(slice.poc, rust_video_h264::SlicePoc::Lsb(6, 0));
        assert!(usize::try_from(slice_bits).expect("small header") < slice_bytes.len() * 8);
    }

    #[test]
    fn h264_p_slice_round_trips_through_parser() {
        let mut writer = BitWriter::default();
        VaApiEncoder::h264_sps((320, 240), (320, 240), 22, &mut writer);
        let (sps_bytes, _) = writer.take();
        VaApiEncoder::h264_pps(26, &mut writer);
        let (pps_bytes, _) = writer.take();
        VaApiEncoder::h264_p_slice(3, 6, &mut writer);
        let (slice_bytes, slice_bits) = writer.take();
        let sps_bytes = annexed(&sps_bytes);
        let pps_bytes = annexed(&pps_bytes);
        let slice_bytes = annexed(&slice_bytes);
        let sps = rust_video_h264::parse_sps::<VaApiError, Gate>(&body(&sps_bytes)).expect("sps");
        let pps = rust_video_h264::parse_pps::<VaApiError, Gate>(&body(&pps_bytes), Some(&sps))
            .expect("pps");
        let head = header(&slice_bytes);
        let slice_body = body(&slice_bytes);
        let mut reader = rust_video_h264::BitReader::new(&slice_body);
        let prefix =
            rust_video_h264::parse_slice_prefix::<VaApiError>(&mut reader).expect("prefix");
        let slice = rust_video_h264::parse_slice_rest::<VaApiError, Gate>(
            &mut reader,
            prefix,
            head & 0x1F == 5,
            (head >> 5) & 3,
            &sps,
            &pps,
        )
        .expect("p slice");
        assert_eq!(slice.slice_type, rust_video_h264::SliceType::P);
        assert_eq!(slice.frame_num, 3);
        assert_eq!(slice.poc, rust_video_h264::SlicePoc::Lsb(6, 0));
        assert!(usize::try_from(slice_bits).expect("small header") < slice_bytes.len() * 8);
    }

    #[test]
    fn h264_sps_carries_crop_window() {
        let mut writer = BitWriter::default();
        VaApiEncoder::h264_sps((320, 208), (320, 200), 22, &mut writer);
        let (bytes, _) = writer.take();
        let bytes = annexed(&bytes);
        let sps = rust_video_h264::parse_sps::<VaApiError, Gate>(&body(&bytes)).expect("sps");
        assert_eq!((sps.width(), sps.height()), (320, 208));
        assert_eq!((sps.visible_width(), sps.visible_height()), (320, 200));
    }

    #[test]
    fn hevc_headers_pin_layout() {
        let mut writer = BitWriter::default();
        VaApiEncoder::hevc_vps(120, &mut writer);
        let (vps, _) = writer.take();
        assert_eq!(&vps[..5], &[0, 0, 1, 64, 1]);
        assert_eq!(vps[20], 120);
        VaApiEncoder::hevc_sps((320, 240), (320, 240), 120, &mut writer);
        let (sps, _) = writer.take();
        assert_eq!(&sps[..5], &[0, 0, 1, 66, 1]);
        assert_eq!(sps[17], 120);
        VaApiEncoder::hevc_pps(26, &mut writer);
        let (pps, _) = writer.take();
        assert_eq!(&pps[..], &[0, 0, 1, 68, 1, 0xC1, 0x71, 0x80, 0x12]);
        VaApiEncoder::hevc_slice(&mut writer);
        let (slice, _) = writer.take();
        assert_eq!(&slice[..], &[0, 0, 1, 38, 1, 0xAF, 0x80]);
    }

    #[test]
    fn hevc_headers_round_trip_through_parser() {
        let mut writer = BitWriter::default();
        VaApiEncoder::hevc_vps(120, &mut writer);
        let (vps_bytes, _) = writer.take();
        VaApiEncoder::hevc_sps((320, 240), (320, 240), 120, &mut writer);
        let (sps_bytes, _) = writer.take();
        VaApiEncoder::hevc_pps(26, &mut writer);
        let (pps_bytes, _) = writer.take();
        VaApiEncoder::hevc_slice(&mut writer);
        let (idr_bytes, _) = writer.take();
        VaApiEncoder::hevc_p_slice(6, &mut writer);
        let (p_bytes, p_bits) = writer.take();
        let vps_bytes = annexed(&vps_bytes);
        let sps_bytes = annexed(&sps_bytes);
        let pps_bytes = annexed(&pps_bytes);
        let idr_bytes = annexed(&idr_bytes);
        let p_bytes = annexed(&p_bytes);
        rust_video_h265::parse_vps::<VaApiError>(&body265(&vps_bytes)).expect("vps");
        let sps =
            rust_video_h265::parse_sps::<VaApiError, Gate265>(&body265(&sps_bytes)).expect("sps");
        assert_eq!((sps.width, sps.height), (320, 240));
        assert_eq!(sps.rps.len(), 1);
        assert_eq!(sps.ordering_buffering[0], 1);
        assert_eq!(sps.ordering_reorder[0], 0);
        let rps = &sps.rps[0];
        assert_eq!((rps.negative_count, rps.positive_count), (1, 0));
        assert_eq!(rps.negative[0], -2);
        assert!(rps.negative_used[0]);
        let pps =
            rust_video_h265::parse_pps::<VaApiError, Gate265>(&body265(&pps_bytes), Some(&sps))
                .expect("pps");
        assert_eq!(pps.pic_init_qp_minus26, 0);
        let head = header265(&idr_bytes);
        let idr_body = body265(&idr_bytes);
        let mut reader = rust_video_h265::BitReader::new(&idr_body);
        let idr_type = (head[0] >> 1) & 0x3F;
        let prefix = rust_video_h265::parse_slice_prefix::<VaApiError>(&mut reader, idr_type)
            .expect("prefix");
        let idr = rust_video_h265::parse_slice_rest::<VaApiError>(
            &mut reader,
            prefix,
            idr_type,
            &sps,
            &pps,
        )
        .expect("idr slice");
        assert_eq!(idr.slice_type, rust_video_h265::SliceType::I);
        let head = header265(&p_bytes);
        let p_body = body265(&p_bytes);
        let mut reader = rust_video_h265::BitReader::new(&p_body);
        let nal_type = (head[0] >> 1) & 0x3F;
        let prefix = rust_video_h265::parse_slice_prefix::<VaApiError>(&mut reader, nal_type)
            .expect("prefix");
        let p = rust_video_h265::parse_slice_rest::<VaApiError>(
            &mut reader,
            prefix,
            nal_type,
            &sps,
            &pps,
        )
        .expect("p slice");
        assert_eq!(p.slice_type, rust_video_h265::SliceType::P);
        assert_eq!(p.poc_lsb, Some(6));
        assert_eq!(p.rps_bits.and_then(|bits| bits.table_idx), Some(0));
        let tail = rust_video_h265::parse_slice_tail::<VaApiError>(&p_body, nal_type, &sps, &pps)
            .expect("tail");
        assert!(tail.bit_size > 16);
        assert!(usize::try_from(p_bits).expect("small header") < p_bytes.len() * 8);
    }
}
