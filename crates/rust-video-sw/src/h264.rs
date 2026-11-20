//! H.264 encode over `OpenH264`.
//!
//! Encode only: `OpenH264` decode diverges from the reference on B-frame
//! content (see `docs/sw-codecs.md`), so no decoder ships here.

use crate::config::{check_common, DEFAULT_AUTO_BITRATE};
use crate::error::SwError;

use bytes::Bytes;
use openh264::encoder::{
    BitRate, ColorPrimaries as NativePrimaries, Complexity, EncodedBitStream,
    Encoder as NativeEncoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
    Level as NativeLevel, MatrixCoefficients as NativeMatrix, Profile as NativeProfile, QpRange,
    RateControlMode, TransferCharacteristics as NativeTransfer, VuiConfig,
};
use openh264::formats::YUVSlices;
use openh264::Timestamp;
use rust_video_caps::color::{ColorConfig, ColorRange};
use rust_video_caps::encode::{
    Codec, Decline, EncodeConfig, Level as ConfigLevel, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::h264::{EncodeLevel, EncodeProfile as H264Profile, Level as H264Level};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Encoder, KeyframeRequest};
use rust_video_caps::transcode::GpuCaps;

/// Slice byte budget when multi-threaded; engages worker threads without tiny slice fragmentation.
const MULTI_SLICE_BYTES: u32 = 16_384;

/// Maximum constant-QP the openh264 range accepts.
const MAX_QP: u8 = 51;

/// Realtime H.264 encoder.
pub struct H264Encoder {
    /// `OpenH264` encoder instance.
    inner: NativeEncoder,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Encoded frame counter driving monotonically increasing timestamps.
    frame_index: u64,
    /// Frame-rate numerator in frames per period.
    fps_num: u32,
    /// Frame-rate denominator in seconds per period.
    fps_den: u32,
}

impl H264Encoder {
    /// Encoder for `width`x`height` under `config` on `threads`.
    ///
    /// Maps the shared config onto the openh264 builder: rate control selects
    /// the bitrate/quality/off mode plus QP, `gop.keyint` sets the intra
    /// period, preset selects complexity, profile and level pin SPS signaling,
    /// frame rate sets max rate plus timestamps, color writes SPS VUI.
    /// B-frames, extra references, CRF, HDR, and non-VUI color decline typed.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width in luma samples; nonzero and even.
    /// * `height` - coded height in luma samples; nonzero and even.
    /// * `config` - shared encode configuration.
    /// * `threads` - worker threads; zero resolves inside openh264.
    ///
    /// # Errors
    ///
    /// Returns `Err` on zero or odd geometry, thread counts above `u16::MAX`,
    /// structural or tier [`Decline`]s, or openh264 rejection.
    pub fn open(
        width: u32,
        height: u32,
        config: EncodeConfig,
        threads: u32,
    ) -> Result<Self, SwError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(SwError::Init);
        }
        let threads16 = u16::try_from(threads).map_err(|_| SwError::Init)?;
        check_common(Codec::H264, &config)?;
        let profile = Self::map_profile(config.profile)?;
        let level = Self::map_level(config.level)?;
        let vui = Self::map_vui(&config.color)?;
        let mut builder = EncoderConfig::new()
            .debug(false)
            .max_frame_rate(Self::max_frame_rate(
                config.frame_rate.num,
                config.frame_rate.den,
            ))
            .skip_frames(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(config.gop.keyint))
            .num_threads(threads16)
            .complexity(Self::map_preset(config.quality.preset))
            .scene_change_detect(false)
            .adaptive_quantization(false)
            .background_detection(false)
            .profile(profile)
            .vui(vui);
        if let Some(pinned) = level {
            builder = builder.level(pinned);
        }
        builder = Self::apply_rate(builder, config.rate_control)?;
        if threads16 > 1 {
            builder = builder.max_slice_len(MULTI_SLICE_BYTES);
        }
        let api = openh264::OpenH264API::from_source();
        let inner =
            NativeEncoder::with_api_config(api, builder).map_err(|error| SwError::Native {
                code: error.native_code(),
            })?;
        Ok(Self {
            inner,
            width,
            height,
            frame_index: 0,
            fps_num: config.frame_rate.num,
            fps_den: config.frame_rate.den,
        })
    }

    /// openh264 frame rate behind `num`/`den`.
    ///
    /// # Arguments
    ///
    /// * `num` - frames per period.
    /// * `den` - period in seconds.
    #[allow(clippy::cast_precision_loss)] // frame-rate magnitudes; sub-hertz error irrelevant here
    fn max_frame_rate(num: u32, den: u32) -> FrameRate {
        FrameRate::from_hz(num as f32 / den as f32)
    }

    /// Builder carrying `mode`; CBR-family modes pin bitrate mode at target.
    ///
    /// `Auto` replays the legacy CBR recipe at the shared default bitrate.
    /// VBR rides the same recipe at target: peaks stay near target without a
    /// cap knob. CQP parks the range on `qp` with control off; CRF has no
    /// openh264 knob and declines.
    ///
    /// # Arguments
    ///
    /// * `builder` - encoder configuration under construction.
    /// * `mode` - requested rate control.
    ///
    /// # Errors
    ///
    /// Returns `Err` on CRF or QP above the openh264 range.
    fn apply_rate(builder: EncoderConfig, mode: RateControl) -> Result<EncoderConfig, SwError> {
        match mode {
            RateControl::Auto => Ok(builder
                .bitrate(BitRate::from_bps(DEFAULT_AUTO_BITRATE))
                .rate_control_mode(RateControlMode::Bitrate)),
            RateControl::Cbr { bitrate, .. } | RateControl::Vbr { bitrate, .. } => Ok(builder
                .bitrate(BitRate::from_bps(bitrate))
                .rate_control_mode(RateControlMode::Bitrate)),
            RateControl::Cqp { qp } => {
                if qp > MAX_QP {
                    return Err(Decline::RateControlUnsupported { mode }.into());
                }
                Ok(builder
                    .rate_control_mode(RateControlMode::Off)
                    .qp(QpRange::new(qp, qp)))
            }
            RateControl::Crf { .. } => Err(Decline::RateControlUnsupported { mode }.into()),
        }
    }

    /// openh264 profile behind `profile`; 10-bit profiles decline.
    ///
    /// # Arguments
    ///
    /// * `profile` - requested encode profile; H.264 8-bit only.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the profile names another codec or needs 10-bit.
    fn map_profile(profile: Profile) -> Result<NativeProfile, SwError> {
        match profile {
            Profile::H264(H264Profile::Baseline) => Ok(NativeProfile::Baseline),
            Profile::H264(H264Profile::Main) => Ok(NativeProfile::Main),
            Profile::H264(H264Profile::High) => Ok(NativeProfile::High),
            _ => Err(Decline::ProfileUnsupported { profile }.into()),
        }
    }

    /// Pinned openh264 level behind `level`; auto leaves backend selection.
    ///
    /// # Arguments
    ///
    /// * `level` - requested encode level; H.264 only.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the level names another codec.
    fn map_level(level: ConfigLevel) -> Result<Option<NativeLevel>, SwError> {
        match level {
            ConfigLevel::H264(EncodeLevel::Auto) => Ok(None),
            ConfigLevel::H264(EncodeLevel::Pinned(pinned)) => Ok(Some(match pinned {
                H264Level::L1 => NativeLevel::Level_1_0,
                H264Level::L1b => NativeLevel::Level_1_B,
                H264Level::L11 => NativeLevel::Level_1_1,
                H264Level::L12 => NativeLevel::Level_1_2,
                H264Level::L13 => NativeLevel::Level_1_3,
                H264Level::L2 => NativeLevel::Level_2_0,
                H264Level::L21 => NativeLevel::Level_2_1,
                H264Level::L22 => NativeLevel::Level_2_2,
                H264Level::L3 => NativeLevel::Level_3_0,
                H264Level::L31 => NativeLevel::Level_3_1,
                H264Level::L32 => NativeLevel::Level_3_2,
                H264Level::L4 => NativeLevel::Level_4_0,
                H264Level::L41 => NativeLevel::Level_4_1,
                H264Level::L42 => NativeLevel::Level_4_2,
                H264Level::L5 => NativeLevel::Level_5_0,
                H264Level::L51 => NativeLevel::Level_5_1,
                H264Level::L52 => NativeLevel::Level_5_2,
            })),
            _ => Err(Decline::LevelUnsupported { level }.into()),
        }
    }

    /// openh264 complexity behind `preset`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn map_preset(preset: SpeedPreset) -> Complexity {
        match preset {
            SpeedPreset::Realtime => Complexity::Low,
            SpeedPreset::Balanced => Complexity::Medium,
            SpeedPreset::Quality => Complexity::High,
        }
    }

    /// SPS VUI behind `color`, matched on coded Table E-1 values.
    ///
    /// # Arguments
    ///
    /// * `color` - requested color description; VUI-signaled SDR only.
    ///
    /// # Errors
    ///
    /// Returns `Err` on HDR metadata or values outside the VUI set.
    fn map_vui(color: &ColorConfig) -> Result<VuiConfig, SwError> {
        if color.is_hdr() {
            return Err(Decline::HdrUnsupported.into());
        }
        let primaries =
            Self::map_primaries(color.primaries.idc()).ok_or(Decline::PrimariesUnsupported {
                primaries: color.primaries,
            })?;
        let transfer =
            Self::map_transfer(color.transfer.idc()).ok_or(Decline::TransferUnsupported {
                transfer: color.transfer,
            })?;
        let matrix = Self::map_matrix(color.matrix.idc()).ok_or(Decline::MatrixUnsupported {
            matrix: color.matrix,
        })?;
        Ok(VuiConfig::new()
            .color_primaries(primaries)
            .transfer_characteristics(transfer)
            .matrix_coefficients(matrix)
            .full_range(color.range == ColorRange::Full))
    }

    /// openh264 primary behind coded `idc`; `None` outside the VUI set.
    ///
    /// # Arguments
    ///
    /// * `idc` - coded `colour_primaries` value.
    fn map_primaries(idc: u8) -> Option<NativePrimaries> {
        match idc {
            1 => Some(NativePrimaries::Bt709),
            2 => Some(NativePrimaries::Unspecified),
            4 => Some(NativePrimaries::Bt470M),
            5 => Some(NativePrimaries::Bt470BG),
            6 => Some(NativePrimaries::Smpte170M),
            7 => Some(NativePrimaries::Smpte240M),
            8 => Some(NativePrimaries::Film),
            9 => Some(NativePrimaries::Bt2020),
            _ => None,
        }
    }

    /// openh264 transfer behind coded `idc`; `None` outside the VUI set.
    ///
    /// # Arguments
    ///
    /// * `idc` - coded `transfer_characteristics` value.
    fn map_transfer(idc: u8) -> Option<NativeTransfer> {
        match idc {
            1 => Some(NativeTransfer::Bt709),
            2 => Some(NativeTransfer::Unspecified),
            4 => Some(NativeTransfer::Bt470M),
            5 => Some(NativeTransfer::Bt470Bg),
            6 => Some(NativeTransfer::Smpte170M),
            7 => Some(NativeTransfer::Smpte240M),
            8 => Some(NativeTransfer::Linear),
            13 => Some(NativeTransfer::Srgb),
            14 => Some(NativeTransfer::Bt2020_10),
            15 => Some(NativeTransfer::Bt2020_12),
            16 => Some(NativeTransfer::Smpte2084),
            18 => Some(NativeTransfer::Hlg),
            _ => None,
        }
    }

    /// openh264 matrix behind coded `idc`; `None` outside the VUI set.
    ///
    /// # Arguments
    ///
    /// * `idc` - coded `matrix_coefficients` value.
    fn map_matrix(idc: u8) -> Option<NativeMatrix> {
        match idc {
            0 => Some(NativeMatrix::Identity),
            1 => Some(NativeMatrix::Bt709),
            2 => Some(NativeMatrix::Unspecified),
            4 => Some(NativeMatrix::Fcc),
            5 => Some(NativeMatrix::Bt470Bg),
            6 => Some(NativeMatrix::Smpte170M),
            7 => Some(NativeMatrix::Smpte240M),
            8 => Some(NativeMatrix::Ycgco),
            9 => Some(NativeMatrix::Bt2020Ncl),
            10 => Some(NativeMatrix::Bt2020Cl),
            _ => None,
        }
    }

    /// Borrowed 4:2:0 view over a frame.
    ///
    /// # Performance
    ///
    /// Zero copies.
    ///
    /// # Arguments
    ///
    /// * `frame` - YUV420P frame; each plane holds its rows times its stride.
    fn view(frame: &CpuFrame) -> YUVSlices<'_> {
        let planes = frame.planes();
        YUVSlices::new(
            (planes[0].data(), planes[1].data(), planes[2].data()),
            (
                frame.size().width() as usize,
                frame.size().height() as usize,
            ),
            (
                planes[0].stride() as usize,
                planes[1].stride() as usize,
                planes[2].stride() as usize,
            ),
        )
    }

    /// Packet coded from an encoded bitstream.
    ///
    /// # Performance
    ///
    /// One copy.
    ///
    /// # Arguments
    ///
    /// * `stream` - encoded bitstream.
    fn packet(stream: &EncodedBitStream) -> Packet {
        let mut coded = Packet::new(Bytes::from(stream.to_vec()));
        if matches!(stream.frame_type(), FrameType::IDR | FrameType::I) {
            coded = coded.mark_keyframe();
        }
        coded
    }
}

impl Encoder for H264Encoder {
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        SwError::check_yuv420p(frame, self.width, self.height)?;
        let source = Self::view(frame);
        let millis = u128::from(self.frame_index)
            .saturating_mul(1000)
            .saturating_mul(u128::from(self.fps_den))
            / u128::from(self.fps_num);
        let ts = Timestamp::from_millis(u64::try_from(millis).unwrap_or(u64::MAX));
        self.frame_index = self.frame_index.saturating_add(1);
        let stream = self
            .inner
            .encode_at(&source, ts)
            .map_err(|error| SwError::Native {
                code: error.native_code(),
            })?;
        Ok(vec![Self::packet(&stream)])
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        Ok(Vec::new())
    }
}

impl KeyframeRequest for H264Encoder {
    type Error = SwError;

    fn request_keyframe(&mut self) -> Result<(), Self::Error> {
        self.inner.force_intra_frame();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use openh264::decoder::{DecodeOptions, Decoder, DecoderConfig, Flush};
    use openh264::formats::YUVSource;
    use rust_video_caps::color::{ColorPrimaries, ColorRange, ContentLight};
    use rust_video_caps::common::{ChromaSampling, PictureSize};
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl, SpeedPreset,
    };
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::h264::{EncodeLevel, EncodeProfile};
    use rust_video_caps::traits::{Encoder as _, KeyframeRequest as _};
    use rust_video_h264::{is_idr, nal_unit_type, split_nals};

    use super::H264Encoder;
    use crate::error::SwError;
    use crate::fixtures::{first_sps_of, frame_16, frame_64, frame_noise_64, frame_with_y};

    fn assert_send_sync<T: Send + Sync>() {}

    /// Config with a CBR target of `bps` at 60 fps and a 60-frame GOP.
    fn cbr(bps: u32) -> EncodeConfig {
        EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: bps,
                maxrate: bps,
                bufsize: bps,
            },
            frame_rate: FrameRate { num: 60, den: 1 },
            ..Default::default()
        }
    }

    fn strided_64() -> CpuFrame {
        let mut y = vec![0u8; 64 * 80];
        for row in y.chunks_exact_mut(80) {
            row[..64].fill(0x80);
        }
        let mut chroma = vec![0u8; 32 * 40];
        for row in chroma.chunks_exact_mut(40) {
            row[..32].fill(0x80);
        }
        let chroma = Bytes::from(chroma);
        let planes = [
            Plane::new(Bytes::from(y), 80).expect("plane"),
            Plane::new(chroma.clone(), 40).expect("plane"),
            Plane::new(chroma, 40).expect("plane"),
        ];
        let size = PictureSize::new(64, 64).expect("size");
        CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
    }

    /// SPS behind the first packet of one `config` encode.
    fn first_sps(config: EncodeConfig) -> rust_video_h264::Sps {
        let mut encoder = H264Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        first_sps_of(&packets)
    }

    #[test]
    fn encoder_is_send_sync() {
        assert_send_sync::<H264Encoder>();
    }

    #[test]
    fn encoder_caps_report_no_share() {
        use rust_video_caps::transcode::GpuCaps;

        let encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        assert_eq!(encoder.caps(), GpuCaps::new(None, false, false));
    }

    #[test]
    fn strided_input_matches_tight_input_bytes() {
        let mut tight = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let mut strided = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let tight_packets = tight.encode(&frame_64()).expect("encode");
        let strided_packets = strided.encode(&strided_64()).expect("encode");
        assert_eq!(tight_packets, strided_packets);
    }

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let first = encoder.encode(&frame_64()).expect("encode");
        assert_eq!(first.len(), 1);
        assert!(first[0].is_keyframe());
        assert_ne!(first[0].data(), []);
        let second = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(second[0].data(), []);
        assert_eq!(encoder.flush().expect("flush"), []);
    }

    #[test]
    fn keyframe_request_forces_single_idr() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let first = encoder.encode(&frame_64()).expect("encode");
        assert!(first[0].is_keyframe());
        let second = encoder.encode(&frame_64()).expect("encode");
        assert!(!second[0].is_keyframe());
        encoder.request_keyframe().expect("request");
        let forced = encoder.encode(&frame_64()).expect("encode");
        assert!(forced[0].is_keyframe());
        let types: Vec<u8> = split_nals(forced[0].data())
            .iter()
            .map(|nal| nal_unit_type(nal[0]))
            .collect();
        assert!(
            types.iter().any(|kind| is_idr(*kind)),
            "idr present: {types:?}"
        );
        let fourth = encoder.encode(&frame_64()).expect("encode");
        assert!(!fourth[0].is_keyframe());
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(H264Encoder::open(0, 64, EncodeConfig::default(), 1).is_err());
        assert!(H264Encoder::open(64, 0, EncodeConfig::default(), 1).is_err());
    }

    #[test]
    fn encode_rejects_odd_geometry() {
        assert!(H264Encoder::open(63, 64, EncodeConfig::default(), 1).is_err());
        assert!(H264Encoder::open(64, 63, EncodeConfig::default(), 1).is_err());
    }

    #[test]
    fn encode_rejects_thread_overflow() {
        assert!(matches!(
            H264Encoder::open(64, 64, EncodeConfig::default(), u32::from(u16::MAX) + 1),
            Err(SwError::Init)
        ));
    }

    #[test]
    fn own_output_decodes_back() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        let api = openh264::OpenH264API::from_source();
        let mut decoder = Decoder::with_api_config(api, config).expect("decoder");
        let options = DecodeOptions::new().flush_after_decode(Flush::NoFlush);
        // The openh264 decoder holds the Main-profile IDR until flush;
        // Baseline releases it immediately. Either path yields one frame.
        let mut dims = Vec::new();
        if let Some(out) = decoder
            .decode_with_options(packets[0].data(), options)
            .expect("decode")
        {
            dims.push(out.dimensions());
        }
        for held in decoder.flush_remaining().expect("flush") {
            dims.push(held.dimensions());
        }
        assert_eq!(dims, [(64, 64)]);
    }

    #[test]
    fn auto_matches_explicit_recipe_bytes() {
        let explicit = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 5_000_000,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            ..Default::default()
        };
        let auto = EncodeConfig {
            rate_control: RateControl::Auto,
            ..Default::default()
        };
        // Grey frames: openh264 fails multi-frame noise where 5 Mbps
        // oversubscribes the content by tens of bits per pixel.
        let mut explicit_enc = H264Encoder::open(64, 64, explicit, 1).expect("encoder");
        let mut auto_enc = H264Encoder::open(64, 64, auto, 1).expect("encoder");
        for _ in 0..3 {
            let frame = frame_64();
            let left = explicit_enc.encode(&frame).expect("encode");
            let right = auto_enc.encode(&frame).expect("encode");
            assert_eq!(left, right);
        }
    }

    #[test]
    fn bitrate_changes_output() {
        let mut low = H264Encoder::open(64, 64, cbr(500_000), 1).expect("encoder");
        let mut high = H264Encoder::open(64, 64, cbr(2_000_000), 1).expect("encoder");
        let frame = frame_noise_64(7);
        let low_packets = low.encode(&frame).expect("encode");
        let high_packets = high.encode(&frame).expect("encode");
        assert_ne!(low_packets[0].data(), high_packets[0].data());
    }

    #[test]
    fn preset_changes_output() {
        let mut fast = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let slow_config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..Default::default()
        };
        let mut slow = H264Encoder::open(64, 64, slow_config, 1).expect("encoder");
        let frame = frame_noise_64(11);
        let fast_packets = fast.encode(&frame).expect("encode");
        let slow_packets = slow.encode(&frame).expect("encode");
        assert_ne!(fast_packets[0].data(), slow_packets[0].data());
    }

    #[test]
    fn color_changes_output() {
        let mut limited = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let mut color = EncodeConfig::default().color;
        color.range = ColorRange::Full;
        let full_config = EncodeConfig {
            color,
            ..Default::default()
        };
        let mut full = H264Encoder::open(64, 64, full_config, 1).expect("encoder");
        let frame = frame_64();
        let limited_packets = limited.encode(&frame).expect("encode");
        let full_packets = full.encode(&frame).expect("encode");
        assert_ne!(limited_packets[0].data(), full_packets[0].data());
    }

    #[test]
    fn gop_cadence_places_idrs() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 30,
                min_keyint: 30,
                b_frames: 0,
                refs: 1,
            },
            ..cbr(500_000)
        };
        let mut encoder = H264Encoder::open(64, 64, config, 1).expect("encoder");
        let mut keyed = Vec::new();
        for seed in 0..65u64 {
            let packets = encoder.encode(&frame_noise_64(seed)).expect("encode");
            assert_eq!(packets.len(), 1);
            keyed.push(packets[0].is_keyframe());
        }
        assert_eq!(keyed.len(), 65);
        for (index, keyframe) in keyed.iter().enumerate() {
            assert_eq!(
                *keyframe,
                index % 30 == 0,
                "keyframe exactly every 30: {index}"
            );
        }
    }

    #[test]
    fn default_gop_places_idrs_at_sixty() {
        let mut encoder = H264Encoder::open(64, 64, cbr(500_000), 1).expect("encoder");
        for seed in 0..61u64 {
            let packets = encoder.encode(&frame_noise_64(seed)).expect("encode");
            assert_eq!(packets.len(), 1);
            assert_eq!(
                packets[0].is_keyframe(),
                seed == 0 || seed == 60,
                "default GOP places IDRs at 0 and 60: {seed}"
            );
        }
    }

    #[test]
    fn output_holds_sps_pps_idr() {
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        let types: Vec<u8> = split_nals(packets[0].data())
            .iter()
            .map(|nal| nal_unit_type(nal[0]))
            .collect();
        assert!(types.contains(&7), "sps present: {types:?}");
        assert!(types.contains(&8), "pps present: {types:?}");
        assert!(
            types.iter().any(|kind| is_idr(*kind)),
            "idr present: {types:?}"
        );
    }

    #[test]
    fn profile_mapping_lands_in_sps() {
        for (profile, idc) in [
            (EncodeProfile::Baseline, 66),
            (EncodeProfile::Main, 77),
            (EncodeProfile::High, 100),
        ] {
            let config = EncodeConfig {
                profile: Profile::H264(profile),
                ..Default::default()
            };
            let sps = first_sps(config);
            assert_eq!(sps.profile_idc, idc, "{profile:?}");
        }
    }

    #[test]
    fn pinned_level_lands_in_sps() {
        let config = EncodeConfig {
            level: Level::H264(EncodeLevel::Pinned(rust_video_caps::h264::Level::L31)),
            ..Default::default()
        };
        let sps = first_sps(config);
        assert_eq!(sps.level_idc, 31);
    }

    #[test]
    fn vui_carries_default_color() {
        let sps = first_sps(EncodeConfig::default());
        let signal = sps
            .vui
            .expect("vui present")
            .signal
            .expect("signal present");
        assert!(!signal.full_range);
        let colour = signal.colour.expect("colour present");
        assert_eq!(colour.primaries, 1);
        assert_eq!(colour.transfer, 1);
        assert_eq!(colour.matrix, 1);
    }

    #[test]
    fn vui_carries_full_range() {
        let mut color = EncodeConfig::default().color;
        color.range = ColorRange::Full;
        let sps = first_sps(EncodeConfig {
            color,
            ..Default::default()
        });
        let signal = sps
            .vui
            .expect("vui present")
            .signal
            .expect("signal present");
        assert!(signal.full_range);
    }

    #[test]
    fn cqp_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..Default::default()
        };
        let mut encoder = H264Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn vbr_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Vbr {
                bitrate: 500_000,
                maxrate: 1_000_000,
            },
            ..Default::default()
        };
        let mut encoder = H264Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn foreign_codec_profile_declines() {
        let config = EncodeConfig {
            profile: Profile::Vp8,
            level: Level::Vp8,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn ten_bit_depth_declines() {
        let config = EncodeConfig {
            profile: Profile::H264(EncodeProfile::High10),
            bit_depth: 10,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BitDepthUnsupported { actual: 10, max: 8 },
            })
        ));
    }

    #[test]
    fn high10_profile_declines() {
        let config = EncodeConfig {
            profile: Profile::H264(EncodeProfile::High10),
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn non_420_chroma_declines() {
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ChromaUnsupported { .. },
            })
        ));
    }

    #[test]
    fn b_frames_decline() {
        let config = EncodeConfig {
            gop: Gop {
                b_frames: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BFramesUnsupported { actual: 2, max: 0 },
            })
        ));
    }

    #[test]
    fn extra_refs_decline() {
        let config = EncodeConfig {
            gop: Gop {
                refs: 3,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RefsUnsupported { actual: 3, max: 1 },
            })
        ));
    }

    #[test]
    fn crf_declines() {
        let mode = RateControl::Crf { value: 23 };
        let config = EncodeConfig {
            rate_control: mode,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported {
                    mode: RateControl::Crf { value: 23 }
                },
            })
        ));
    }

    #[test]
    fn wild_qp_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 60 },
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn hdr_color_declines() {
        let mut color = EncodeConfig::default().color;
        color.light = Some(ContentLight {
            max_cll: 1000,
            max_fall: 400,
        });
        let config = EncodeConfig {
            color,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::HdrUnsupported,
            })
        ));
    }

    #[test]
    fn unsignaled_primaries_decline() {
        let mut color = EncodeConfig::default().color;
        color.primaries = ColorPrimaries::SmpteSt428;
        let config = EncodeConfig {
            color,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::PrimariesUnsupported { .. },
            })
        ));
    }

    #[test]
    fn zero_gop_declines() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 0,
                min_keyint: 60,
                b_frames: 0,
                refs: 1,
            },
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::GopInvalid { .. },
            })
        ));
    }

    #[test]
    fn zero_bitrate_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 0,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlInvalid { .. },
            })
        ));
    }

    #[test]
    fn profile_level_mismatch_declines() {
        let config = EncodeConfig {
            level: Level::Vp8,
            ..Default::default()
        };
        assert!(matches!(
            H264Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ProfileLevelMismatch { .. },
            })
        ));
    }
}
