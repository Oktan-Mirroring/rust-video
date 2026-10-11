#![cfg(have_corpus)]

use std::cell::RefCell;
use std::path::PathBuf;

use bytes::Bytes;

use rust_video::{Decoder, Packet, VideoCodec};
use rust_video_harness::{Case, Error, FrameHash, Golden, Oracle, Suite};
use rust_video_vulkan::{CodecOp, H264Decoder, H265Decoder, VulkanDevice};

/// Harness oracle over a Vulkan decoder.
struct VulkanOracle<D> {
    /// Decoder under test, behind interior mutability for `Oracle`.
    decoder: RefCell<D>,
}

impl<D> VulkanOracle<D> {
    /// Oracle over `decoder`.
    fn new(decoder: D) -> Self {
        Self {
            decoder: RefCell::new(decoder),
        }
    }
}

impl Oracle for VulkanOracle<H264Decoder<'_>> {
    fn decode(&self, case: &Case) -> Result<Vec<FrameHash>, Error> {
        let bytes = std::fs::read(case.path())?;
        let mut decoder = self.decoder.borrow_mut();
        let mut hashes = Vec::new();
        for frame in decoder
            .decode(&Packet::new(Bytes::from(bytes)))
            .map_err(|error| Error::Decode(error.to_string()))?
        {
            hashes.push(FrameHash::of(&frame.yuv420p()));
        }
        for frame in decoder
            .flush()
            .map_err(|error| Error::Decode(error.to_string()))?
        {
            hashes.push(FrameHash::of(&frame.yuv420p()));
        }
        Ok(hashes)
    }
}

impl Oracle for VulkanOracle<H265Decoder<'_>> {
    fn decode(&self, case: &Case) -> Result<Vec<FrameHash>, Error> {
        let bytes = std::fs::read(case.path())?;
        let mut decoder = self.decoder.borrow_mut();
        let mut hashes = Vec::new();
        for frame in decoder
            .decode(&Packet::new(Bytes::from(bytes)))
            .map_err(|error| Error::Decode(error.to_string()))?
        {
            hashes.push(FrameHash::of(&frame.yuv420p()));
        }
        for frame in decoder
            .flush()
            .map_err(|error| Error::Decode(error.to_string()))?
        {
            hashes.push(FrameHash::of(&frame.yuv420p()));
        }
        Ok(hashes)
    }
}

/// Corpus root: `RUST_VIDEO_CORPUS`, else the workspace `corpus`.
fn corpus() -> PathBuf {
    std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
        PathBuf::from,
    )
}

/// Device for `op`, declining loudly when the tier skips.
fn device_or_decline(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open_or_skip(op) {
        Ok(device) => Some(device),
        Err(rust_video_vulkan::VulkanError::BackendAbsent(reason)) => {
            println!("declined: {op:?} on this box: {reason}");
            None
        }
        Err(error) => panic!("negotiation failed: {error}"),
    }
}

/// Golden comparison of every `codec` case through `oracle`.
fn check_goldens(codec: VideoCodec, oracle: &dyn Oracle) {
    let suite = Suite::open_at(&corpus()).expect("corpus present by build gate");
    for case in suite.cases_for(codec) {
        let actual = oracle.decode(case).expect("decode runs");
        let golden_path = case.path().with_extension("golden");
        if golden_path.is_file() {
            let golden = Golden::load(&golden_path).expect("golden parses");
            let gaps = golden.check(&actual);
            let shown: Vec<String> = gaps.iter().map(std::string::ToString::to_string).collect();
            assert!(gaps.is_empty(), "mismatches in {}: {shown:?}", case.name());
        } else {
            assert!(!actual.is_empty(), "no frames from {}", case.name());
        }
    }
}

#[test]
fn h264_matches_goldens() {
    let Some(device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    let decoder = H264Decoder::open(&device).expect("engine starts");
    let oracle = VulkanOracle::new(decoder);
    check_goldens(VideoCodec::H264, &oracle);
}

#[test]
fn h265_matches_goldens() {
    let Some(device) = device_or_decline(CodecOp::H265Decode) else {
        return;
    };
    let decoder = H265Decoder::open(&device).expect("engine starts");
    let oracle = VulkanOracle::new(decoder);
    check_goldens(VideoCodec::H265, &oracle);
}
