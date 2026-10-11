#![allow(unsafe_code)]
//! Minimal libavcodec/libavutil bindings for software codecs.
//!
//! Hand-written surface over the system libraries; no bindgen.

use std::ffi::{c_char, c_int, c_void};

/// Native H.264 codec id.
pub const CODEC_ID_H264: c_int = 27;
/// Native VP8 codec id.
pub const CODEC_ID_VP8: c_int = 139;
/// Native VP9 codec id.
pub const CODEC_ID_VP9: c_int = 167;
/// Native HEVC codec id.
pub const CODEC_ID_HEVC: c_int = 173;
/// Native AV1 codec id.
pub const CODEC_ID_AV1: c_int = 225;
/// Planar 8-bit 4:2:0 pixel format.
pub const PIX_FMT_YUV420P: c_int = 0;
/// Linked libavcodec major these layouts were read from.
pub const ABI_MAJOR: u32 = 61;
/// Linked libavutil major the frame layout was read from.
pub const UTIL_MAJOR: u32 = 59;
/// Receiver needs more input before the next frame.
pub const AGAIN: c_int = -11;
/// End of stream reached after a null send.
pub const EOF: c_int = -541_478_725;
/// Missing timestamp for parser input.
pub const NOPTS: i64 = i64::MIN;
/// Search flag reaching the encoder wrapper's private options.
pub const OPT_SEARCH_CHILDREN: c_int = 1;

/// Opaque codec descriptor.
#[repr(C)]
pub struct AVCodec {
    _opaque: [u8; 0],
}

/// Opaque codec instance; configured through options only.
#[repr(C)]
pub struct AVCodecContext {
    _opaque: [u8; 0],
}

/// Opaque stream splitter.
#[repr(C)]
pub struct AVCodecParserContext {
    _opaque: [u8; 0],
}

/// Coded packet; payload span, stream index, and flags are exposed.
#[repr(C)]
pub struct AVPacket {
    _head: [u8; 24],
    /// Payload bytes.
    pub data: *mut u8,
    /// Payload length.
    pub size: c_int,
    /// Stream index.
    pub stream_index: c_int,
    /// Flag bits (`FLAG_KEY` marks a keyframe).
    pub flags: c_int,
    _tail: [u8; 60],
}

/// Keyframe bit in `AVPacket::flags`.
pub const FLAG_KEY: c_int = 1;

/// Decoded frame; only geometry, format, planes, and pts are exposed.
#[repr(C)]
pub struct AVFrame {
    /// Plane base pointers.
    pub data: [*mut u8; 8],
    /// Plane row strides.
    pub linesize: [c_int; 8],
    /// Extended plane pointers.
    pub extended_data: *mut *mut u8,
    /// Visible width.
    pub width: c_int,
    /// Visible height.
    pub height: c_int,
    /// Audio sample count, unused here.
    pub nb_samples: c_int,
    /// Pixel format id.
    pub format: c_int,
    _mid: [u8; 16],
    /// Presentation timestamp in time-base units.
    pub pts: i64,
    _tail: [u8; 296],
}

const _: () = assert!(size_of::<AVPacket>() == 104);
const _: () = assert!(size_of::<AVFrame>() == 440);
const _: () = assert!(std::mem::offset_of!(AVPacket, data) == 24);
const _: () = assert!(std::mem::offset_of!(AVPacket, size) == 32);
const _: () = assert!(std::mem::offset_of!(AVPacket, stream_index) == 36);
const _: () = assert!(std::mem::offset_of!(AVPacket, flags) == 40);
const _: () = assert!(std::mem::offset_of!(AVFrame, data) == 0);
const _: () = assert!(std::mem::offset_of!(AVFrame, linesize) == 64);
const _: () = assert!(std::mem::offset_of!(AVFrame, width) == 104);
const _: () = assert!(std::mem::offset_of!(AVFrame, height) == 108);
const _: () = assert!(std::mem::offset_of!(AVFrame, format) == 116);
const _: () = assert!(std::mem::offset_of!(AVFrame, pts) == 136);

#[link(name = "avcodec")]
unsafe extern "C" {
    /// Linked library version.
    pub fn avcodec_version() -> u32;
    /// Decoder descriptor for `id`, or null.
    pub fn avcodec_find_decoder(id: c_int) -> *const AVCodec;
    /// Encoder descriptor named `name`, or null.
    pub fn avcodec_find_encoder_by_name(name: *const c_char) -> *const AVCodec;
    /// Decoder descriptor named `name`, or null.
    pub fn avcodec_find_decoder_by_name(name: *const c_char) -> *const AVCodec;
    /// Context for `codec`, or null.
    pub fn avcodec_alloc_context3(codec: *const AVCodec) -> *mut AVCodecContext;
    /// Option value on `obj`; zero on success.
    pub fn av_opt_set_int(
        obj: *mut c_void,
        name: *const c_char,
        value: i64,
        search: c_int,
    ) -> c_int;
    /// String option value on `obj`; zero on success.
    pub fn av_opt_set(
        obj: *mut c_void,
        name: *const c_char,
        value: *const c_char,
        search: c_int,
    ) -> c_int;
    /// Context opened for `codec`; zero on success.
    pub fn avcodec_open2(
        ctx: *mut AVCodecContext,
        codec: *const AVCodec,
        options: *mut *mut c_void,
    ) -> c_int;
    /// Parser for `codec_id`, or null.
    pub fn av_parser_init(codec_id: c_int) -> *mut AVCodecParserContext;
    /// Bytes consumed; completed frame published through `pout`.
    pub fn av_parser_parse2(
        parser: *mut AVCodecParserContext,
        ctx: *mut AVCodecContext,
        pout: *mut *mut u8,
        pout_size: *mut c_int,
        buf: *const u8,
        buf_size: c_int,
        pts: i64,
        dts: i64,
        pos: i64,
    ) -> c_int;
    /// Parser released.
    pub fn av_parser_close(parser: *mut AVCodecParserContext);
    /// Blank packet, or null.
    pub fn av_packet_alloc() -> *mut AVPacket;
    /// Refcounted payload of `size` on `packet`; zero on success.
    pub fn av_new_packet(packet: *mut AVPacket, size: c_int) -> c_int;
    /// Packet queued; zero on success.
    pub fn avcodec_send_packet(ctx: *mut AVCodecContext, packet: *const AVPacket) -> c_int;
    /// Frame queued for encode; zero on success.
    pub fn avcodec_send_frame(ctx: *mut AVCodecContext, frame: *const AVFrame) -> c_int;
    /// Next packet into `packet`; zero on success.
    pub fn avcodec_receive_packet(ctx: *mut AVCodecContext, packet: *mut AVPacket) -> c_int;
    /// Next frame into `frame`; zero on success.
    pub fn avcodec_receive_frame(ctx: *mut AVCodecContext, frame: *mut AVFrame) -> c_int;
    /// Context reset for a new stream.
    pub fn avcodec_flush_buffers(ctx: *mut AVCodecContext);
    /// Packet payload released.
    pub fn av_packet_unref(packet: *mut AVPacket);
    /// Packet released.
    pub fn av_packet_free(packet: *mut *mut AVPacket);
    /// Context released.
    pub fn avcodec_free_context(ctx: *mut *mut AVCodecContext);
}

#[link(name = "avutil")]
unsafe extern "C" {
    /// Linked utility library version.
    pub fn avutil_version() -> u32;
    /// Blank frame, or null.
    pub fn av_frame_alloc() -> *mut AVFrame;
    /// Buffer for `frame` with `align`; zero on success.
    pub fn av_frame_get_buffer(frame: *mut AVFrame, align: c_int) -> c_int;
    /// Frame contents released.
    pub fn av_frame_unref(frame: *mut AVFrame);
    /// Frame released.
    pub fn av_frame_free(frame: *mut *mut AVFrame);
}
