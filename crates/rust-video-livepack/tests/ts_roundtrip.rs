//! Packetize-depacketize round trips with identical NALUs.

use rust_video_livepack::{AccessUnit, Codec, TsDemux, TsMux, TsMuxConfig};

/// Annex-B unit from `(header, filler, len)` NALUs.
fn annexb(codec: Codec, nalus: &[(u8, u8, usize)], pts: u64, dts: u64) -> AccessUnit {
    let mut payload = Vec::new();
    for (header, filler, len) in nalus {
        payload.extend_from_slice(&[0, 0, 0, 1, *header]);
        payload.extend(std::iter::repeat_n(*filler, *len));
    }
    AccessUnit::classified(codec, payload, pts, dts)
}

/// H.264 keyframe plus inter frames, one with DTS lag.
fn h264_units() -> Vec<AccessUnit> {
    vec![
        annexb(
            Codec::H264,
            &[(0x67, 0x11, 24), (0x68, 0x22, 8), (0x65, 0x47, 5000)],
            0,
            0,
        ),
        annexb(Codec::H264, &[(0x41, 0x33, 3000)], 6000, 3000),
        annexb(Codec::H264, &[(0x41, 0x44, 700)], 9000, 6000),
    ]
}

/// H.265 keyframe plus inter frame.
fn h265_units() -> Vec<AccessUnit> {
    vec![
        annexb(
            Codec::H265,
            &[
                (0x40, 0x55, 24),
                (0x42, 0x66, 32),
                (0x44, 0x77, 8),
                (0x26, 0x47, 8000),
            ],
            9000,
            9000,
        ),
        annexb(Codec::H265, &[(0x02, 0x88, 1500)], 15_000, 12_000),
    ]
}

/// Mux then demux in `chunk`-byte feeds.
fn round_trip(codec: Codec, units: &[AccessUnit], chunk: usize) -> Vec<AccessUnit> {
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut stream = Vec::new();
    for unit in units {
        mux.push(unit, None, &mut stream).expect("packetized");
    }
    assert_eq!(stream.len() % 188, 0);
    let mut demux = TsDemux::new(1 << 20);
    let mut out = Vec::new();
    for piece in stream.chunks(chunk) {
        out.extend(demux.push(piece).expect("parsed"));
    }
    assert_eq!(out.len(), units.len());
    for (got, want) in out.iter().zip(units) {
        assert_eq!(got.codec(), codec);
        assert_eq!(got.payload(), want.payload());
        assert_eq!(got.pts(), want.pts());
        assert_eq!(got.dts(), want.dts());
        assert_eq!(got.is_keyframe(), want.is_keyframe());
    }
    out
}

#[test]
fn h264_round_trips_identical() {
    let units = h264_units();
    assert!(units[0].is_keyframe());
    assert!(!units[1].is_keyframe());
    for chunk in [7, 188, 1000, 65536] {
        round_trip(Codec::H264, &units, chunk);
    }
}

#[test]
fn h265_round_trips_identical() {
    let units = h265_units();
    assert!(units[0].is_keyframe());
    assert!(!units[1].is_keyframe());
    for chunk in [13, 189, 4096] {
        round_trip(Codec::H265, &units, chunk);
    }
}

#[test]
fn unbounded_pes_completes_at_next_header() {
    let big = annexb(Codec::H264, &[(0x65, 0x99, 70_000)], 0, 0);
    let small = annexb(Codec::H264, &[(0x41, 0xAA, 100)], 3000, 3000);
    let out = round_trip(Codec::H264, &[big, small], 1009);
    assert_eq!(out.len(), 2);
}

#[test]
fn timestamps_survive_wrap() {
    let wrap = (1 << 33) - 3000;
    let units = vec![
        annexb(Codec::H264, &[(0x65, 0xBB, 200)], wrap, wrap),
        annexb(Codec::H264, &[(0x41, 0xCC, 200)], 0, 0),
    ];
    let out = round_trip(Codec::H264, &units, 500);
    assert_eq!(out[0].pts(), wrap);
    assert_eq!(out[1].pts(), 0);
}

#[test]
fn finish_flushes_trailing_unbounded_pes() {
    let big = annexb(Codec::H264, &[(0x65, 0x99, 70_000)], 0, 0);
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut stream = Vec::new();
    mux.push(&big, None, &mut stream).expect("packetized");
    let mut demux = TsDemux::new(1 << 20);
    assert!(demux.push(&stream).expect("parsed").is_empty());
    let unit = demux.finish().expect("flushed");
    assert_eq!(unit.payload(), big.payload());
    assert!(demux.finish().is_none());
}

#[test]
fn pcr_tracks_decode_clock() {
    let units = h264_units();
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut stream = Vec::new();
    for unit in &units {
        mux.push(unit, None, &mut stream).expect("packetized");
    }
    let mut demux = TsDemux::new(1 << 20);
    demux.push(&stream).expect("parsed");
    let pcr = demux.last_pcr().expect("pcr stamped");
    assert_eq!(pcr.base().get(), 6000);
    assert_eq!(pcr.ext(), 0);
}
