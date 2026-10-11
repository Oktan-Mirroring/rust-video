#![cfg(have_corpus)]

use std::path::PathBuf;

use rust_video::VideoCodec;
use rust_video_harness::Suite;

fn corpus() -> PathBuf {
    std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
        PathBuf::from,
    )
}

#[test]
fn corpus_cases_are_nonempty_files() {
    let suite = Suite::open_at(&corpus()).expect("corpus present by build gate");
    assert!(!suite.cases().is_empty(), "corpus holds no cases");
    for case in suite.cases() {
        let len = std::fs::metadata(case.path()).expect("case readable").len();
        assert!(len > 0, "empty case: {}", case.name());
    }
}

#[test]
fn cases_for_yields_matching_codec_only() {
    let suite = Suite::open_at(&corpus()).expect("corpus present by build gate");
    for case in suite.cases_for(VideoCodec::H264) {
        assert_eq!(case.codec(), VideoCodec::H264);
    }
}
