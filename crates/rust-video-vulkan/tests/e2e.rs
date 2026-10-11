#![cfg(have_corpus)]

use std::cell::RefCell;
use std::path::PathBuf;

use bytes::Bytes;

use rust_video::{Decoder, Packet, VideoCodec};
use rust_video_harness::{Case, Error, FrameHash, Golden, Oracle, Suite};
use rust_video_vulkan::{CodecOp, H264Decoder, VulkanDevice};

/// Harness oracle over the Vulkan H.264 decoder.
struct VulkanOracle {
    /// Decoder under test, behind interior mutability for `Oracle`.
    decoder: RefCell<H264Decoder>,
}

impl VulkanOracle {
    /// Oracle over a negotiated device.
    fn open(device: &VulkanDevice) -> Result<Self, Error> {
        let decoder =
            H264Decoder::open(device).map_err(|error| Error::Decode(error.to_string()))?;
        Ok(Self {
            decoder: RefCell::new(decoder),
        })
    }
}

impl Oracle for VulkanOracle {
    fn decode(&self, case: &Case) -> Result<Vec<FrameHash>, Error> {
        let bytes = std::fs::read(case.path())?;
        let mut decoder = self.decoder.borrow_mut();
        let mut hashes = Vec::new();
        for frame in decoder
            .decode(&Packet::new(Bytes::from(bytes)))
            .map_err(|error| Error::Decode(error.to_string()))?
        {
            hashes.push(FrameHash::of(frame.nv12()));
        }
        for frame in decoder
            .flush()
            .map_err(|error| Error::Decode(error.to_string()))?
        {
            hashes.push(FrameHash::of(frame.nv12()));
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

#[test]
fn h264_matches_goldens() {
    let Some(device) = VulkanDevice::open(CodecOp::H264Decode).expect("negotiation runs") else {
        panic!("vulkan tier skipped on an e2e runner");
    };
    let oracle = VulkanOracle::open(&device).expect("engine starts");
    let suite = Suite::open_at(&corpus()).expect("corpus present by build gate");
    for case in suite.cases_for(VideoCodec::H264) {
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
