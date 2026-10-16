//! VA-API encode: all-intra CQP sessions for H.264 and H.265.
//!
//! Every picture is an IDR: no reference management, no reorder delay,
//! every packet a keyframe. CQP keeps constant quality without a
//! bitrate model.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::os::raw::{c_int, c_void};

use crate::buffer::{bytes_of, VaBuffer};
use crate::decoder::VaApiFrame;
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi;
use crate::scale::ScaledSurface;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::FallbackCause;

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

/// VA-API hardware encoder over one display.
///
/// `Send`, never `Sync`: hand the session to another thread when idle,
/// but run every call on one thread. The contract matches the display
/// behind it; see `VaApiDevice`.
pub struct VaApiEncoder {
    /// Codec the session writes.
    codec: EncodeCodec,
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
    /// Next reconstruction surface.
    next_recon: usize,
    /// Coded-bitstream buffer, rewritten every picture.
    coded_buf: VaBuffer,
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
    /// NV12 upload image, reused across pictures.
    upload: ffi::VaImage,
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
    /// Upload surface index.
    const UPLOAD: usize = 0;
    /// First reconstruction surface index.
    const RECON: usize = 1;
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
        let upload = Self::upload_image(display, &mut image_format, coded_size)?;
        let words = Words::new(codec);
        let level = Self::level(codec, coded_size);
        let visible_pair = (visible.width(), visible.height());
        let (vps, sps) =
            Self::pack_session_headers(codec, coded_size, visible_pair, level, display, context)?;
        Ok(Self {
            codec,
            visible,
            coded: coded_size,
            qp: Self::DEFAULT_QP,
            packed_qp: None,
            words,
            level,
            count: 0,
            next_recon: 0,
            coded_buf: VaBuffer::empty(
                display,
                context,
                ffi::VA_ENC_CODED_BUFFER_TYPE,
                Self::coded_len(coded_size)?,
            )?,
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
            upload,
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

    /// Packed VPS (H.265) and sequence headers for the session.
    fn pack_session_headers(
        codec: EncodeCodec,
        coded_size: (u32, u32),
        visible_pair: (u32, u32),
        level: u8,
        display: ffi::VaDisplay,
        context: ffi::VaContextId,
    ) -> Result<(Option<PackedPair>, PackedPair), VaApiError> {
        let mut writer = BitWriter::default();
        let mut vps = None;
        if codec == EncodeCodec::H265 {
            Self::hevc_vps(level, &mut writer);
            let (bytes, bits) = writer.take();
            let mut pair = PackedPair::empty(display, context)?;
            pair.pack(ffi::VA_ENC_PACKED_HEADER_TYPE_RAW_DATA, &bytes, bits)?;
            vps = Some(pair);
        }
        match codec {
            EncodeCodec::H264 => Self::h264_sps(coded_size, visible_pair, level, &mut writer),
            EncodeCodec::H265 => Self::hevc_sps(coded_size, visible_pair, level, &mut writer),
        }
        let (bytes, bits) = writer.take();
        let mut sps = PackedPair::empty(display, context)?;
        sps.pack(ffi::VA_ENC_PACKED_HEADER_TYPE_SEQUENCE, &bytes, bits)?;
        Ok((vps, sps))
    }

    /// Codec the session writes.
    #[must_use]
    pub const fn codec(&self) -> EncodeCodec {
        self.codec
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
    /// # Errors
    ///
    /// Returns `Err` when the surface lives on another display, misses
    /// the session size, or the backend refuses the picture.
    pub(crate) fn encode_scaled(
        &mut self,
        scaled: &ScaledSurface,
    ) -> Result<Vec<Packet>, VaApiError> {
        self.check_surface(scaled.display(), scaled.size())?;
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
        let recon = self.session.surfaces[Self::RECON + self.next_recon];
        self.next_recon = (self.next_recon + 1) % Self::RECONS;
        match self.codec {
            EncodeCodec::H264 => self.render_h264(recon)?,
            EncodeCodec::H265 => self.render_h265(recon)?,
        }
        let display = self.session.display;
        let context = self.session.context;
        // Sound: the context and source surface are live.
        VaApiError::status(unsafe { ffi::vaBeginPicture(display, context, source) })?;
        let count = c_int::try_from(self.render.len()).map_err(|_| Self::range())?;
        let rendered =
            unsafe { ffi::vaRenderPicture(display, context, self.render.as_mut_ptr(), count) };
        let end = unsafe { ffi::vaEndPicture(display, context) };
        VaApiError::status(rendered)?;
        VaApiError::status(end)?;
        // Sound: the surface was just submitted.
        VaApiError::status(unsafe { ffi::vaSyncSurface(display, source) })?;
        let bytes = self.harvest()?;
        self.count += 1;
        Ok(vec![Packet::new(Bytes::from(bytes)).mark_keyframe()])
    }

    /// NV12 frame bytes onto the upload surface.
    fn upload(&mut self, frame: &CpuFrame) -> Result<ffi::VaSurfaceId, VaApiError> {
        if frame.format() != PixelFormat::Nv12 || frame.size() != self.visible {
            return Err(VaApiError::Unsupported(format!(
                "{} encode needs nv12 at {}x{}",
                self.codec,
                self.visible.width(),
                self.visible.height(),
            )));
        }
        let display = self.session.display;
        let surface = self.session.surfaces[Self::UPLOAD];
        self.copy_into(&self.upload, frame)?;
        // Sound: the surface and image are live; the rectangles fit both.
        VaApiError::status(unsafe {
            ffi::vaPutImage(
                display,
                surface,
                self.upload.image_id,
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
        Ok(surface)
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
        VaApiError::status(unsafe {
            ffi::vaCreateImage(display, format, width_c, height_c, image.as_mut_ptr())
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
            || luma.data().len() < luma_stride * height
            || chroma.data().len() < chroma_stride * height / 2
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
        let display = self.session.display;
        // Sound: the image buffer is live until `vaDestroyImage`.
        let mut data: *mut c_void = std::ptr::null_mut();
        VaApiError::status(unsafe { ffi::vaMapBuffer(display, image.buf, &raw mut data) })?;
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
            ffi::vaUnmapBuffer(display, image.buf);
        }
        Ok(())
    }

    /// Coded bytes harvested from the coded buffer.
    fn harvest(&self) -> Result<Vec<u8>, VaApiError> {
        let display = self.session.display;
        // Sound: the coded buffer is live until the session drops.
        let mut mapped: *mut c_void = std::ptr::null_mut();
        VaApiError::status(unsafe {
            ffi::vaMapBuffer(display, self.coded_buf.id(), &raw mut mapped)
        })?;
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
            ffi::vaUnmapBuffer(display, self.coded_buf.id());
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
        self.pps.pack(
            ffi::VA_ENC_PACKED_HEADER_TYPE_PICTURE,
            self.writer.bytes(),
            bits,
        )?;
        self.writer.clear();
        self.packed_qp = Some(qp);
        Ok(())
    }

    /// Render list for one H.264 IDR picture on `recon`.
    fn render_h264(&mut self, recon: ffi::VaSurfaceId) -> Result<(), VaApiError> {
        let frame = self.count;
        let mbw = self.coded.0 / 16;
        let mbh = self.coded.1 / 16;
        let frame_num = u16::try_from(frame % 16).map_err(|_| Self::range())?;
        let idr_pic_id = u16::try_from(frame % 65_535).map_err(|_| Self::range())?;
        let poc = u16::try_from(frame.wrapping_mul(2) % 64).map_err(|_| Self::range())?;
        let qp = self.qp;
        self.pack_pps(qp)?;
        Self::h264_slice(frame_num, idr_pic_id, poc, &mut self.writer);
        let slice_bits = self.writer.bit_len();
        self.slice_packed.pack(
            ffi::VA_ENC_PACKED_HEADER_TYPE_SLICE,
            self.writer.bytes(),
            slice_bits,
        )?;
        self.writer.clear();
        let crop = self.coded != (self.visible.width(), self.visible.height());
        let seq = ffi::VaEncSequenceParameterBufferH264 {
            seq_parameter_set_id: 0,
            level_idc: self.level,
            intra_period: 1,
            intra_idr_period: 1,
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
            vui_parameters_present_flag: 0,
            vui_fields: 0,
            aspect_ratio_idc: 0,
            sar_width: 0,
            sar_height: 0,
            num_units_in_tick: 0,
            time_scale: 0,
            va_reserved: [0; 4],
        };
        self.seq.rewrite(bytes_of(&seq))?;
        let pic = ffi::VaEncPictureParameterBufferH264 {
            curr_pic: ffi::VaPictureH264 {
                picture_id: recon,
                frame_idx: u32::from(frame_num),
                flags: ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE,
                top_field_order_cnt: i32::from(poc),
                bottom_field_order_cnt: i32::from(poc),
                ..Default::default()
            },
            reference_frames: [Self::h264_invalid(); 16],
            coded_buf: self.coded_buf.id(),
            frame_num,
            pic_init_qp: qp,
            pic_fields: self.words.pic,
            ..Default::default()
        };
        self.slice_pic.rewrite(bytes_of(&pic))?;
        let mbs = u64::from(mbw) * u64::from(mbh);
        let slice = ffi::VaEncSliceParameterBufferH264 {
            num_macroblocks: u32::try_from(mbs).map_err(|_| Self::range())?,
            macroblock_info: ffi::VA_INVALID_ID,
            slice_type: 2,
            idr_pic_id,
            pic_order_cnt_lsb: poc,
            ref_pic_list0: [Self::h264_invalid(); 32],
            ref_pic_list1: [Self::h264_invalid(); 32],
            ..Default::default()
        };
        self.slice.rewrite(bytes_of(&slice))?;
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

    /// Render list for one H.265 IDR picture on `recon`.
    fn render_h265(&mut self, recon: ffi::VaSurfaceId) -> Result<(), VaApiError> {
        let ctus_w = self.coded.0.div_ceil(64);
        let ctus_h = self.coded.1.div_ceil(64);
        let ctus =
            u32::try_from(u64::from(ctus_w) * u64::from(ctus_h)).map_err(|_| Self::range())?;
        let qp = self.qp;
        self.pack_pps(qp)?;
        Self::hevc_slice(&mut self.writer);
        let slice_bits = self.writer.bit_len();
        self.slice_packed.pack(
            ffi::VA_ENC_PACKED_HEADER_TYPE_SLICE,
            self.writer.bytes(),
            slice_bits,
        )?;
        self.writer.clear();
        let seq = ffi::VaEncSequenceParameterBufferHevc {
            general_profile_idc: 1,
            general_level_idc: self.level,
            intra_period: 1,
            intra_idr_period: 1,
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
        self.seq.rewrite(bytes_of(&seq))?;
        let pic = ffi::VaEncPictureParameterBufferHevc {
            decoded_curr_pic: ffi::VaPictureHevc {
                picture_id: recon,
                ..Default::default()
            },
            reference_frames: [Self::hevc_invalid(); 15],
            coded_buf: self.coded_buf.id(),
            collocated_ref_pic_index: 0xFF,
            pic_init_qp: qp,
            nal_unit_type: 19,
            pic_fields: self.words.pic,
            ..Default::default()
        };
        self.slice_pic.rewrite(bytes_of(&pic))?;
        let slice = ffi::VaEncSliceParameterBufferHevc {
            num_ctu_in_slice: ctus,
            slice_type: 2,
            ref_pic_list0: [Self::hevc_invalid(); 15],
            ref_pic_list1: [Self::hevc_invalid(); 15],
            max_num_merge_cand: 5,
            slice_fields: self.words.slice,
            ..Default::default()
        };
        self.slice.rewrite(bytes_of(&slice))?;
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

    /// Invalid HEVC reference picture marker.
    fn hevc_invalid() -> ffi::VaPictureHevc {
        ffi::VaPictureHevc {
            picture_id: ffi::VA_INVALID_ID,
            pic_order_cnt: 0,
            flags: ffi::VA_PICTURE_HEVC_INVALID,
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
        writer.bits(0, 1);
        writer.rbsp_trailing();
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
        writer.ue(0);
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
        writer.ue(0);
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

    /// NV12 image format the display uploads through.
    fn nv12_format(device: &VaApiDevice) -> Result<ffi::VaImageFormat, VaApiError> {
        let display = device.display();
        // Sound: the format list is sized by its capacity query.
        unsafe {
            let capacity = usize::try_from(ffi::vaMaxNumImageFormats(display)).unwrap_or(0);
            let mut formats = vec![ffi::VaImageFormat::default(); capacity];
            let mut count = 0;
            VaApiError::status(ffi::vaQueryImageFormats(
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

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, VaApiError> {
        let surface = self.upload(frame)?;
        self.encode_inner(surface)
    }

    fn flush(&mut self) -> Result<Vec<Packet>, VaApiError> {
        Ok(Vec::new())
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
        VaApiError::status(unsafe {
            ffi::vaCreateConfig(
                display,
                profile,
                ffi::VA_ENTRYPOINT_ENC_SLICE,
                attribs.as_mut_ptr(),
                3,
                &raw mut config,
            )
        })?;
        let count = 1 + VaApiEncoder::RECONS;
        let mut surfaces = vec![0; count];
        // Sound: the surface array holds `count` live entries.
        let status = unsafe {
            ffi::vaCreateSurfaces(
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
        if let Err(error) = VaApiError::status(status) {
            // Sound: `config` was created above.
            unsafe {
                ffi::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        let count_c = c_int::try_from(count)
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let mut context = 0;
        // Sound: the surface array outlives context creation.
        let status = unsafe {
            ffi::vaCreateContext(
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
        if let Err(error) = VaApiError::status(status) {
            // Sound: the surfaces and config were created above.
            unsafe {
                ffi::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                ffi::vaDestroyConfig(display, config);
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
            ffi::vaDestroyContext(self.display, self.context);
            ffi::vaDestroySurfaces(self.display, self.surfaces.as_mut_ptr(), self.count_c);
            ffi::vaDestroyConfig(self.display, self.config);
        }
    }
}

// Sound: a move hands exclusive ownership to the new thread; the
// session destroys wherever the value drops, never shared.
unsafe impl Send for VaApiEncoder {}

impl Drop for VaApiEncoder {
    fn drop(&mut self) {
        // Sound: the image was created in `open`; the session outlives
        // this drop, so the display is still initialized.
        unsafe {
            ffi::vaDestroyImage(self.session.display, self.upload.image_id);
        }
    }
}

/// Session-fixed packed parameter words.
struct Words {
    /// Packed sequence flags.
    seq: u32,
    /// Packed picture flags.
    pic: u32,
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
                pic: (1u32 << VaApiEncoder::H264_PIC_IDR)
                    | (1u32 << VaApiEncoder::H264_PIC_REFERENCE)
                    | (1u32 << VaApiEncoder::H264_PIC_ENTROPY)
                    | (1u32 << VaApiEncoder::H264_PIC_TRANSFORM_8X8),
                slice: 0,
            },
            EncodeCodec::H265 => Self {
                seq: (1u32 << VaApiEncoder::H265_SEQ_CHROMA)
                    | (1u32 << VaApiEncoder::H265_SEQ_STRONG_INTRA)
                    | (1u32 << VaApiEncoder::H265_SEQ_AMP),
                pic: (1u32 << VaApiEncoder::H265_PIC_IDR)
                    | (1u32 << VaApiEncoder::H265_PIC_CODING_TYPE)
                    | (1u32 << VaApiEncoder::H265_PIC_REFERENCE)
                    | (1u32 << VaApiEncoder::H265_PIC_SIGN_DATA)
                    | (1u32 << VaApiEncoder::H265_PIC_LOOP_TILES),
                slice: 1 << VaApiEncoder::H265_SLICE_LAST,
            },
        }
    }
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
    fn pack(&mut self, header_type: u32, bytes: &[u8], bit_len: u32) -> Result<(), VaApiError> {
        let param = ffi::VaEncPackedHeaderParameterBuffer {
            header_type,
            bit_length: bit_len,
            has_emulation_bytes: 0,
            va_reserved: [0; 4],
        };
        self.param.rewrite(bytes_of(&param))?;
        self.data.rewrite(bytes)
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
    use std::io::Cursor;

    use super::{BitWriter, EncodeCodec, VaApiEncoder, Words};
    use crate::h264::parser::{Nalu, Parser, SliceType};

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
        assert_eq!(h264.pic, 267);
        assert_eq!(h264.slice, 0);
        let h265 = Words::new(EncodeCodec::H265);
        assert_eq!(h265.seq, 3073);
        assert_eq!(h265.pic, 32_851);
        assert_eq!(h265.slice, 1);
    }

    #[test]
    fn level_covers_picture_sizes() {
        assert_eq!(VaApiEncoder::level(EncodeCodec::H264, (320, 240)), 22);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H264, (1920, 1080)), 41);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H265, (320, 240)), 60);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H265, (1920, 1080)), 120);
        assert_eq!(VaApiEncoder::level(EncodeCodec::H265, (3840, 2160)), 150);
    }

    /// First NAL unit of Annex B `bytes`.
    fn first(bytes: &[u8]) -> Nalu<'_> {
        let mut cursor = Cursor::new(bytes);
        Nalu::next(&mut cursor).expect("annex b unit")
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
        let mut parser = Parser::default();
        let sps = parser.parse_sps(&first(&sps_bytes)).expect("sps").clone();
        assert_eq!(sps.profile_idc, 100);
        assert_eq!((sps.width(), sps.height()), (320, 240));
        assert_eq!(sps.pic_order_cnt_type, 0);
        assert_eq!(sps.log2_max_frame_num_minus4, 0);
        assert!(sps.frame_field.frame_mbs_only);

        VaApiEncoder::h264_pps(26, &mut writer);
        let (pps_bytes, _) = writer.take();
        let pps_bytes = annexed(&pps_bytes);
        let pps = parser.parse_pps(&first(&pps_bytes)).expect("pps");
        assert!(pps.coding.entropy_coding_mode_flag);
        assert!(pps.coding.transform_8x8_mode_flag);
        assert_eq!(pps.pic_init_qp_minus26, 0);

        VaApiEncoder::h264_slice(3, 7, 6, &mut writer);
        let (slice_bytes, slice_bits) = writer.take();
        let slice_bytes = annexed(&slice_bytes);
        let slice = parser
            .parse_slice_header(first(&slice_bytes))
            .expect("slice");
        assert_eq!(slice.header.slice_type, SliceType::I);
        assert_eq!(slice.header.frame_num, 3);
        assert_eq!(slice.header.idr_pic_id, 7);
        assert_eq!(slice.header.pic_order_cnt_lsb, 6);
        assert!(usize::try_from(slice_bits).expect("small header") < slice_bytes.len() * 8);
    }

    #[test]
    fn h264_sps_carries_crop_window() {
        let mut writer = BitWriter::default();
        VaApiEncoder::h264_sps((320, 208), (320, 200), 22, &mut writer);
        let (bytes, _) = writer.take();
        let bytes = annexed(&bytes);
        let mut parser = Parser::default();
        let sps = parser.parse_sps(&first(&bytes)).expect("sps").clone();
        assert_eq!((sps.width(), sps.height()), (320, 208));
        let rect = sps.visible_rectangle();
        assert_eq!(
            (rect.max.x - rect.min.x, rect.max.y - rect.min.y),
            (320, 200)
        );
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
        assert_eq!(&pps[..], &[0, 0, 1, 68, 1, 0xC1, 0x71, 0x80, 0x04]);
        VaApiEncoder::hevc_slice(&mut writer);
        let (slice, _) = writer.take();
        assert_eq!(&slice[..], &[0, 0, 1, 38, 1, 0xAF, 0x80]);
    }
}
