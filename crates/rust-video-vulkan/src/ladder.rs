//! Vulkan negotiation ladder over an injected backend.
//!
//! Rung order follows `docs/compat.md`: loader, instance version,
//! device properties, extension strings, video capabilities per codec
//! op, then logical device with only used queues.

use std::ffi::{CStr, CString};

use crate::error::{SkipReason, VulkanError};

/// Video operation the ladder negotiates for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecOp {
    /// H.264 decode.
    H264Decode,
    /// H.265 decode.
    H265Decode,
    /// H.264 encode.
    H264Encode,
    /// H.265 encode.
    H265Encode,
}

impl CodecOp {
    /// Device extensions the op needs, in enable order.
    ///
    /// Names are ash 0.38 constants, headers 1.3.281.
    /// <https://docs.rs/ash/0.38.0/ash/vk/index.html>
    #[must_use]
    pub fn extensions(self) -> [&'static CStr; 3] {
        match self {
            Self::H264Decode => [
                ash::vk::KHR_VIDEO_QUEUE_NAME,
                ash::vk::KHR_VIDEO_DECODE_QUEUE_NAME,
                ash::vk::KHR_VIDEO_DECODE_H264_NAME,
            ],
            Self::H265Decode => [
                ash::vk::KHR_VIDEO_QUEUE_NAME,
                ash::vk::KHR_VIDEO_DECODE_QUEUE_NAME,
                ash::vk::KHR_VIDEO_DECODE_H265_NAME,
            ],
            Self::H264Encode => [
                ash::vk::KHR_VIDEO_QUEUE_NAME,
                ash::vk::KHR_VIDEO_ENCODE_QUEUE_NAME,
                ash::vk::KHR_VIDEO_ENCODE_H264_NAME,
            ],
            Self::H265Encode => [
                ash::vk::KHR_VIDEO_QUEUE_NAME,
                ash::vk::KHR_VIDEO_ENCODE_QUEUE_NAME,
                ash::vk::KHR_VIDEO_ENCODE_H265_NAME,
            ],
        }
    }

    /// True for encode ops, false for decode ops.
    #[must_use]
    pub fn is_encode(self) -> bool {
        matches!(self, Self::H264Encode | Self::H265Encode)
    }
}

/// Ladder failure: graceful skip or hard error.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LadderError {
    /// Tier declines the machine.
    #[error("{0}")]
    Skip(#[from] SkipReason),
    /// Unexpected Vulkan failure.
    #[error("{0}")]
    Hard(#[from] VulkanError),
}

impl From<ash::vk::Result> for LadderError {
    fn from(result: ash::vk::Result) -> Self {
        Self::Hard(VulkanError::Vulkan(result))
    }
}

impl From<std::ffi::FromBytesUntilNulError> for LadderError {
    fn from(error: std::ffi::FromBytesUntilNulError) -> Self {
        Self::Hard(VulkanError::StringConversion(error))
    }
}

/// Decode capabilities of one adapter for one codec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DecodeFacts {
    /// Largest coded picture in luma samples.
    pub coded_extent: (u32, u32),
    /// Decoded-picture-buffer slot count.
    pub dpb_slots: u32,
    /// Active reference picture count.
    pub active_refs: u32,
    /// Highest wire level idc the session runs.
    pub max_level: u8,
    /// Video Std header spec version the session pins.
    pub std_version: u32,
    /// Bitstream range granularity in bytes.
    pub min_bitstream_align: u64,
}

/// Encode capabilities of one adapter for one codec.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct EncodeFacts {
    /// Largest coded picture in luma samples.
    pub coded_extent: (u32, u32),
    /// Encoder quality level count.
    pub quality_levels: u32,
    /// Peak bitrate in bits per second.
    pub max_bitrate: u64,
    /// Video Std header spec version the session pins.
    pub std_version: u32,
    /// Winning std profile idc behind the session.
    pub profile: u32,
    /// Feedback queries the pool may request.
    pub feedback: ash::vk::VideoEncodeFeedbackFlagsKHR,
    /// Rate control modes the session may run.
    pub rate_control_modes: ash::vk::VideoEncodeRateControlModeFlagsKHR,
}

impl std::fmt::Debug for EncodeFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodeFacts")
            .field("coded_extent", &self.coded_extent)
            .field("quality_levels", &self.quality_levels)
            .field("max_bitrate", &self.max_bitrate)
            .field("std_version", &self.std_version)
            .field("profile", &self.profile)
            .field("feedback", &self.feedback.as_raw())
            .field("rate_control_modes", &self.rate_control_modes.as_raw())
            .finish()
    }
}

/// Video capabilities of one adapter for `op`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum OpCaps {
    /// Decode caps for a decode op.
    Decode(DecodeFacts),
    /// Encode caps for an encode op.
    Encode(EncodeFacts),
}

/// Adapter identity stable across instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdapterFingerprint {
    /// PCI vendor id.
    pub vendor: u32,
    /// PCI device id.
    pub device: u32,
    /// Device LUID, zero when the driver reports none.
    pub luid: [u8; 8],
}

impl AdapterFingerprint {
    /// FNV-1a hash over the identity fields.
    pub fn uid(self) -> u64 {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0100_0000_01b3;
        let mut hash = OFFSET;
        for word in [self.vendor, self.device] {
            for byte in word.to_ne_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(PRIME);
            }
        }
        for byte in self.luid {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }
}

/// One adapter as the ladder sees it.
#[derive(Clone, Debug)]
pub(crate) struct AdapterFacts<A> {
    /// Backend handle for device creation.
    pub handle: A,
    /// Identity stable across instances.
    pub fingerprint: AdapterFingerprint,
    /// Packed device API version.
    pub api_version: u32,
    /// Device extension strings.
    pub extensions: Vec<CString>,
    /// Queue families with video-decode capability.
    pub decode_queue_families: Vec<u32>,
    /// Queue families with video-encode capability.
    pub encode_queue_families: Vec<u32>,
    /// H.264 caps, when a profile reports them.
    pub h264: Option<DecodeFacts>,
    /// H.265 caps, when a profile reports them.
    pub h265: Option<DecodeFacts>,
    /// H.264 encode caps, when a profile reports them.
    pub h264_encode: Option<EncodeFacts>,
    /// H.265 encode caps, when a profile reports them.
    pub h265_encode: Option<EncodeFacts>,
}

impl<A> AdapterFacts<A> {
    /// Device API against the 1.3 video floor.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when the adapter reports below 1.3.
    pub fn check_floor(&self) -> Result<(), LadderError> {
        let major = ash::vk::api_version_major(self.api_version);
        let minor = ash::vk::api_version_minor(self.api_version);
        if (major, minor) >= (1, 3) {
            Ok(())
        } else {
            Err(SkipReason::BelowApiFloor {
                found: self.api_version,
            }
            .into())
        }
    }

    /// Required extension strings of `op` against the adapter.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when one string is missing.
    pub fn check_extensions(&self, op: CodecOp) -> Result<(), LadderError> {
        for need in op.extensions() {
            let present = self.extensions.iter().any(|have| have.as_c_str() == need);
            if !present {
                let name = need.to_string_lossy().into_owned();
                return Err(SkipReason::MissingExtension { name }.into());
            }
        }
        Ok(())
    }

    /// Decode queue family pick from the adapter.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when no family decodes video.
    pub fn check_queues(&self) -> Result<u32, LadderError> {
        self.decode_queue_families
            .first()
            .copied()
            .ok_or(LadderError::Skip(SkipReason::NoDecodeQueue))
    }

    /// Encode queue family pick from the adapter.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when no family encodes video.
    pub fn check_encode_queues(&self) -> Result<u32, LadderError> {
        self.encode_queue_families
            .first()
            .copied()
            .ok_or(LadderError::Skip(SkipReason::NoEncodeQueue))
    }

    /// Queue family pick from the adapter for `op`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when no family runs `op`.
    pub fn check_queues_for(&self, op: CodecOp) -> Result<u32, LadderError> {
        if op.is_encode() {
            self.check_encode_queues()
        } else {
            self.check_queues()
        }
    }

    /// Video capabilities of the adapter for `op`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when no profile reports caps.
    pub fn check_video_caps(&self, op: CodecOp) -> Result<OpCaps, LadderError> {
        match op {
            CodecOp::H264Decode => self
                .h264
                .map(OpCaps::Decode)
                .ok_or(LadderError::Skip(SkipReason::NoH264Caps)),
            CodecOp::H265Decode => self
                .h265
                .map(OpCaps::Decode)
                .ok_or(LadderError::Skip(SkipReason::NoH265Caps)),
            CodecOp::H264Encode => self
                .h264_encode
                .map(OpCaps::Encode)
                .ok_or(LadderError::Skip(SkipReason::NoH264EncodeCaps)),
            CodecOp::H265Encode => self
                .h265_encode
                .map(OpCaps::Encode)
                .ok_or(LadderError::Skip(SkipReason::NoH265EncodeCaps)),
        }
    }
}

/// Vulkan queries behind the ladder.
pub(crate) trait Backend {
    /// Adapter handle the backend opens devices from.
    type Adapter: Clone;
    /// Opened logical device.
    type Device;

    /// Loader instance version, or `None` on a 1.0 loader.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the loader is absent or the query fails.
    fn instance_version(&self) -> Result<Option<u32>, LadderError>;

    /// Every adapter with its ladder facts.
    ///
    /// # Errors
    ///
    /// Returns `Err` when enumeration fails.
    fn adapters(&self) -> Result<Vec<AdapterFacts<Self::Adapter>>, LadderError>;

    /// Logical device over `queues` families with `op` extensions.
    ///
    /// # Errors
    ///
    /// Returns `Err` when device creation fails.
    fn open_device(
        &self,
        adapter: &Self::Adapter,
        op: CodecOp,
        queues: &[u32],
    ) -> Result<Self::Device, LadderError>;
}

/// Negotiated adapter with its opened device.
pub(crate) struct Negotiated<B: Backend> {
    /// Facts of the picked adapter.
    pub facts: AdapterFacts<B::Adapter>,
    /// Opened logical device.
    pub device: B::Device,
    /// Queue family the device opened.
    pub queue_family: u32,
    /// Capabilities the winning rung reported.
    pub caps: OpCaps,
}

/// Negotiation ladder over one backend.
pub(crate) struct Ladder<B> {
    /// Query backend.
    backend: B,
}

impl<B: Backend> Ladder<B> {
    /// Ladder over `backend`.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Loader instance version, `None` on a 1.0 loader.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the loader is absent.
    pub fn instance_version(&self) -> Result<Option<u32>, LadderError> {
        self.backend.instance_version()
    }

    /// Every adapter the loader enumerates.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when no adapter exists, or on query failure.
    pub fn enumerate(&self) -> Result<Vec<AdapterFacts<B::Adapter>>, LadderError> {
        let adapters = self.backend.adapters()?;
        if adapters.is_empty() {
            return Err(SkipReason::NoPhysicalDevice.into());
        }
        Ok(adapters)
    }

    /// First adapter passing every rung, with its device opened.
    ///
    /// Runs the loader rung first: an absent loader skips before any
    /// adapter query. Reports the first adapter's failing rung when
    /// none passes.
    ///
    /// # Errors
    ///
    /// Returns `Err` on skip when the tier declines, or on query failure.
    pub fn negotiate(&self, op: CodecOp) -> Result<Negotiated<B>, LadderError> {
        let _loader = self.instance_version()?;
        let adapters = self.enumerate()?;
        let mut failure: Option<LadderError> = None;
        for facts in &adapters {
            match self.pick(facts, op) {
                Ok((device, queue_family, caps)) => {
                    return Ok(Negotiated {
                        facts: facts.clone(),
                        device,
                        queue_family,
                        caps,
                    });
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        Err(failure.unwrap_or(LadderError::Skip(SkipReason::NoPhysicalDevice)))
    }

    /// All rungs for one adapter plus device open.
    fn pick(
        &self,
        facts: &AdapterFacts<B::Adapter>,
        op: CodecOp,
    ) -> Result<(B::Device, u32, OpCaps), LadderError> {
        facts.check_floor()?;
        facts.check_extensions(op)?;
        let caps = facts.check_video_caps(op)?;
        let queue = facts.check_queues_for(op)?;
        let device = self.backend.open_device(&facts.handle, op, &[queue])?;
        Ok((device, queue, caps))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::CString;

    use super::{
        AdapterFacts, AdapterFingerprint, Backend, CodecOp, DecodeFacts, EncodeFacts, Ladder,
        LadderError, OpCaps,
    };
    use crate::error::{SkipReason, VulkanError};

    /// Opened fake device: the op and queue families it was given.
    #[derive(Debug, PartialEq)]
    struct FakeDevice {
        /// Op passed to open.
        op: CodecOp,
        /// Queue families passed to open.
        queues: Vec<u32>,
    }

    /// Scripted loader state.
    #[derive(Clone, Copy)]
    enum Loader {
        /// No loader library.
        Absent,
        /// Loader reporting `Option<u32>`, `None` on 1.0 loaders.
        Present(Option<u32>),
    }

    /// Scripted backend driving each rung red or green.
    struct FakeBackend {
        /// Loader state.
        loader: Loader,
        /// Scripted adapters.
        adapters: Vec<AdapterFacts<usize>>,
        /// Hard failure from `adapters`, when set.
        adapters_error: Option<ash::vk::Result>,
        /// Op and queues each `open_device` call received.
        opened: RefCell<Vec<(CodecOp, Vec<u32>)>>,
    }

    impl FakeBackend {
        /// Backend with `adapters` behind a 1.3 loader.
        fn with(adapters: Vec<AdapterFacts<usize>>) -> Self {
            Self::scripted(
                Loader::Present(Some(ash::vk::make_api_version(0, 1, 3, 0))),
                adapters,
                None,
            )
        }

        /// Backend with explicit loader, adapters, and failure.
        fn scripted(
            loader: Loader,
            adapters: Vec<AdapterFacts<usize>>,
            adapters_error: Option<ash::vk::Result>,
        ) -> Self {
            Self {
                loader,
                adapters,
                adapters_error,
                opened: RefCell::new(Vec::new()),
            }
        }
    }

    impl Backend for FakeBackend {
        type Adapter = usize;
        type Device = FakeDevice;

        fn instance_version(&self) -> Result<Option<u32>, LadderError> {
            match self.loader {
                Loader::Absent => Err(LadderError::Skip(SkipReason::LoaderAbsent)),
                Loader::Present(version) => Ok(version),
            }
        }

        fn adapters(&self) -> Result<Vec<AdapterFacts<usize>>, LadderError> {
            if let Some(result) = self.adapters_error {
                return Err(LadderError::Hard(VulkanError::Vulkan(result)));
            }
            Ok(self.adapters.clone())
        }

        fn open_device(
            &self,
            _adapter: &usize,
            op: CodecOp,
            queues: &[u32],
        ) -> Result<FakeDevice, LadderError> {
            self.opened.borrow_mut().push((op, queues.to_vec()));
            Ok(FakeDevice {
                op,
                queues: queues.to_vec(),
            })
        }
    }

    /// Decode caps the fake reports.
    fn caps() -> DecodeFacts {
        DecodeFacts {
            coded_extent: (1920, 1080),
            dpb_slots: 16,
            active_refs: 8,
            max_level: 52,
            std_version: 1,
            min_bitstream_align: 256,
        }
    }

    /// Encode caps the fake reports.
    fn encode_caps() -> EncodeFacts {
        EncodeFacts {
            coded_extent: (1920, 1080),
            quality_levels: 4,
            max_bitrate: 100_000_000,
            std_version: 1,
            profile: 100,
            feedback: ash::vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
            rate_control_modes: ash::vk::VideoEncodeRateControlModeFlagsKHR::CBR,
        }
    }

    /// Extension strings `op` needs.
    fn extensions_for(op: CodecOp) -> Vec<CString> {
        op.extensions()
            .iter()
            .map(|name| CString::from(*name))
            .collect()
    }

    #[test]
    fn fingerprint_uid_stable_and_distinct() {
        let left = AdapterFingerprint {
            vendor: 0x10de,
            device: 0x2b85,
            luid: [9; 8],
        };
        let same = AdapterFingerprint {
            vendor: 0x10de,
            device: 0x2b85,
            luid: [9; 8],
        };
        let other = AdapterFingerprint {
            vendor: 0x1002,
            device: 0x15bf,
            luid: [0; 8],
        };
        assert_eq!(left.uid(), same.uid());
        assert_ne!(left.uid(), other.uid());
    }

    /// Adapter passing every rung for `op`.
    fn green_for(op: CodecOp) -> AdapterFacts<usize> {
        AdapterFacts {
            handle: 0,
            fingerprint: AdapterFingerprint {
                vendor: 0,
                device: 0,
                luid: [0; 8],
            },
            api_version: ash::vk::make_api_version(0, 1, 3, 0),
            extensions: extensions_for(op),
            decode_queue_families: vec![2],
            encode_queue_families: vec![4],
            h264: Some(caps()),
            h265: Some(caps()),
            h264_encode: Some(encode_caps()),
            h265_encode: Some(encode_caps()),
        }
    }

    /// Adapter passing every H.264 decode rung.
    fn green() -> AdapterFacts<usize> {
        green_for(CodecOp::H264Decode)
    }

    /// Skip reason of a failed ladder step.
    fn skip<T>(result: Result<T, LadderError>) -> SkipReason {
        match result {
            Ok(_) => panic!("ok, want skip"),
            Err(LadderError::Skip(reason)) => reason,
            Err(LadderError::Hard(error)) => panic!("hard error, want skip: {error}"),
        }
    }

    #[test]
    fn loader_version_reports_fake() {
        let ladder = Ladder::new(FakeBackend::with(vec![green()]));
        let version = ladder.instance_version().expect("loader present");
        assert_eq!(version.map(ash::vk::api_version_major), Some(1));
    }

    #[test]
    fn loader_absent_skips() {
        let backend = FakeBackend::scripted(Loader::Absent, vec![green()], None);
        let ladder = Ladder::new(backend);
        assert_eq!(skip(ladder.instance_version()), SkipReason::LoaderAbsent);
    }

    #[test]
    fn empty_enumeration_skips() {
        let ladder = Ladder::new(FakeBackend::with(Vec::new()));
        assert_eq!(skip(ladder.enumerate()), SkipReason::NoPhysicalDevice);
    }

    #[test]
    fn enumeration_failure_is_hard() {
        let backend = FakeBackend::scripted(
            Loader::Present(Some(ash::vk::make_api_version(0, 1, 3, 0))),
            Vec::new(),
            Some(ash::vk::Result::ERROR_OUT_OF_HOST_MEMORY),
        );
        let ladder = Ladder::new(backend);
        assert!(matches!(
            ladder.enumerate().unwrap_err(),
            LadderError::Hard(_)
        ));
    }

    #[test]
    fn floor_passes_above_1_3() {
        let mut facts = green();
        facts.api_version = ash::vk::make_api_version(0, 1, 4, 0);
        assert!(facts.check_floor().is_ok());
        assert!(green().check_floor().is_ok());
    }

    #[test]
    fn floor_skips_below_1_3() {
        let mut facts = green();
        facts.api_version = ash::vk::make_api_version(0, 1, 2, 0);
        assert_eq!(
            skip(facts.check_floor()),
            SkipReason::BelowApiFloor {
                found: facts.api_version,
            }
        );
    }

    #[test]
    fn extensions_pass_when_all_present() {
        assert!(green().check_extensions(CodecOp::H264Decode).is_ok());
    }

    #[test]
    fn extensions_skip_on_first_missing() {
        let mut facts = green();
        facts.extensions.pop();
        assert_eq!(
            skip(facts.check_extensions(CodecOp::H264Decode)),
            SkipReason::MissingExtension {
                name: "VK_KHR_video_decode_h264".to_owned(),
            }
        );
    }

    #[test]
    fn queues_pick_first_decode_family() {
        let mut facts = green();
        facts.decode_queue_families = vec![3, 5];
        assert_eq!(facts.check_queues().expect("decode family"), 3);
    }

    #[test]
    fn queues_skip_without_decode_family() {
        let mut facts = green();
        facts.decode_queue_families.clear();
        assert_eq!(skip(facts.check_queues()), SkipReason::NoDecodeQueue);
    }

    #[test]
    fn video_caps_pass_with_profile() {
        assert_eq!(
            green().check_video_caps(CodecOp::H264Decode).expect("caps"),
            OpCaps::Decode(caps())
        );
    }

    #[test]
    fn video_caps_skip_without_profile() {
        let mut facts = green();
        facts.h264 = None;
        assert_eq!(
            skip(facts.check_video_caps(CodecOp::H264Decode)),
            SkipReason::NoH264Caps
        );
    }

    #[test]
    fn ops_split_encode_from_decode() {
        assert!(!CodecOp::H264Decode.is_encode());
        assert!(!CodecOp::H265Decode.is_encode());
        assert!(CodecOp::H264Encode.is_encode());
        assert!(CodecOp::H265Encode.is_encode());
    }

    #[test]
    fn extensions_name_each_codec_op() {
        let h265: Vec<String> = CodecOp::H265Decode
            .extensions()
            .iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            h265.as_slice(),
            [
                "VK_KHR_video_queue",
                "VK_KHR_video_decode_queue",
                "VK_KHR_video_decode_h265",
            ]
        );
        let encode: Vec<String> = CodecOp::H264Encode
            .extensions()
            .iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            encode.as_slice(),
            [
                "VK_KHR_video_queue",
                "VK_KHR_video_encode_queue",
                "VK_KHR_video_encode_h264",
            ]
        );
        let hevc: Vec<String> = CodecOp::H265Encode
            .extensions()
            .iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            hevc.as_slice(),
            [
                "VK_KHR_video_queue",
                "VK_KHR_video_encode_queue",
                "VK_KHR_video_encode_h265",
            ]
        );
    }

    #[test]
    fn encode_queues_pick_first_encode_family() {
        let mut facts = green_for(CodecOp::H264Encode);
        facts.encode_queue_families = vec![5, 7];
        assert_eq!(facts.check_encode_queues().expect("encode family"), 5);
    }

    #[test]
    fn encode_queues_skip_without_encode_family() {
        let mut facts = green_for(CodecOp::H264Encode);
        facts.encode_queue_families.clear();
        assert_eq!(skip(facts.check_encode_queues()), SkipReason::NoEncodeQueue);
    }

    #[test]
    fn queues_for_routes_by_op() {
        let facts = green_for(CodecOp::H264Encode);
        assert_eq!(
            facts.check_queues_for(CodecOp::H264Decode).expect("decode"),
            2
        );
        assert_eq!(
            facts.check_queues_for(CodecOp::H265Decode).expect("decode"),
            2
        );
        assert_eq!(
            facts.check_queues_for(CodecOp::H264Encode).expect("encode"),
            4
        );
    }

    #[test]
    fn h265_caps_pass_with_profile() {
        assert_eq!(
            green_for(CodecOp::H265Decode)
                .check_video_caps(CodecOp::H265Decode)
                .expect("caps"),
            OpCaps::Decode(caps())
        );
    }

    #[test]
    fn h265_caps_skip_without_profile() {
        let mut facts = green_for(CodecOp::H265Decode);
        facts.h265 = None;
        assert_eq!(
            skip(facts.check_video_caps(CodecOp::H265Decode)),
            SkipReason::NoH265Caps
        );
    }

    #[test]
    fn encode_caps_pass_with_profile() {
        assert_eq!(
            green_for(CodecOp::H264Encode)
                .check_video_caps(CodecOp::H264Encode)
                .expect("caps"),
            OpCaps::Encode(encode_caps())
        );
    }

    #[test]
    fn encode_caps_skip_without_profile() {
        let mut facts = green_for(CodecOp::H264Encode);
        facts.h264_encode = None;
        assert_eq!(
            skip(facts.check_video_caps(CodecOp::H264Encode)),
            SkipReason::NoH264EncodeCaps
        );
    }

    #[test]
    fn h265_encode_caps_cover_both_rungs() {
        assert_eq!(
            green_for(CodecOp::H265Encode)
                .check_video_caps(CodecOp::H265Encode)
                .expect("caps"),
            OpCaps::Encode(encode_caps())
        );
        let mut facts = green_for(CodecOp::H265Encode);
        facts.h265_encode = None;
        assert_eq!(
            skip(facts.check_video_caps(CodecOp::H265Encode)),
            SkipReason::NoH265EncodeCaps
        );
    }

    #[test]
    fn negotiate_opens_first_green_adapter() {
        let mut red = green();
        red.h264 = None;
        let mut second = green();
        second.handle = 1;
        let backend = FakeBackend::with(vec![red, second]);
        let ladder = Ladder::new(backend);
        let won = ladder.negotiate(CodecOp::H264Decode).expect("green wins");
        assert_eq!(won.facts.handle, 1);
        assert_eq!(won.device.queues, vec![2]);
    }

    #[test]
    fn negotiate_skips_before_adapters_without_loader() {
        let backend = FakeBackend::scripted(Loader::Absent, vec![green()], None);
        let ladder = Ladder::new(backend);
        assert_eq!(
            skip(ladder.negotiate(CodecOp::H264Decode)),
            SkipReason::LoaderAbsent
        );
        assert!(ladder.backend.opened.borrow().is_empty());
    }

    #[test]
    fn negotiate_reports_first_failure() {
        let mut red = green();
        red.h264 = None;
        let backend = FakeBackend::with(vec![red]);
        let ladder = Ladder::new(backend);
        assert_eq!(
            skip(ladder.negotiate(CodecOp::H264Decode)),
            SkipReason::NoH264Caps
        );
    }

    #[test]
    fn negotiate_opens_only_the_decode_queue() {
        let backend = FakeBackend::with(vec![green()]);
        let ladder = Ladder::new(backend);
        ladder.negotiate(CodecOp::H264Decode).expect("green");
        assert_eq!(
            ladder.backend.opened.borrow().as_slice(),
            &[(CodecOp::H264Decode, vec![2])]
        );
    }

    #[test]
    fn negotiate_encode_opens_only_the_encode_queue() {
        let backend = FakeBackend::with(vec![green_for(CodecOp::H264Encode)]);
        let ladder = Ladder::new(backend);
        let won = ladder.negotiate(CodecOp::H264Encode).expect("green");
        assert_eq!(won.device.op, CodecOp::H264Encode);
        assert_eq!(won.device.queues, vec![4]);
        assert_eq!(
            ladder.backend.opened.borrow().as_slice(),
            &[(CodecOp::H264Encode, vec![4])]
        );
    }

    #[test]
    fn negotiate_h265_reports_first_failure() {
        let mut red = green_for(CodecOp::H265Decode);
        red.h265 = None;
        let backend = FakeBackend::with(vec![red]);
        let ladder = Ladder::new(backend);
        assert_eq!(
            skip(ladder.negotiate(CodecOp::H265Decode)),
            SkipReason::NoH265Caps
        );
    }
}
