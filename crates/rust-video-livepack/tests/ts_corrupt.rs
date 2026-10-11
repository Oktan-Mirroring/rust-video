//! Corrupt inputs fail loud; the demuxer stays consistent.

use rust_video_livepack::{AccessUnit, Codec, TsDemux, TsDemuxError, TsMux, TsMuxConfig};

/// Two-frame H.264 stream.
fn stream() -> Vec<u8> {
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut out = Vec::new();
    for (header, pts) in [(0x65u8, 0u64), (0x41, 3000)] {
        let unit = AccessUnit::new(
            Codec::H264,
            vec![0, 0, 0, 1, header, 0xAA, 0xBB, 0xCC],
            pts,
            pts,
            header == 0x65,
        );
        mux.push(&unit, None, &mut out).expect("packetized");
    }
    out
}

#[test]
fn dropped_packet_errors_then_recovers() {
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut full = Vec::new();
    let mut idr = vec![0, 0, 0, 1, 0x65];
    idr.extend(std::iter::repeat_n(0xAA, 500));
    let big = AccessUnit::classified(Codec::H264, idr, 0, 0);
    mux.push(&big, None, &mut full).expect("packetized");
    let small = AccessUnit::new(Codec::H264, vec![0, 0, 0, 1, 0x41, 0xBB], 3000, 3000, false);
    mux.push(&small, None, &mut full).expect("packetized");
    assert_eq!(full.len(), 6 * 188);
    let mut gapped = Vec::new();
    gapped.extend_from_slice(&full[..3 * 188]);
    gapped.extend_from_slice(&full[4 * 188..]);
    let mut demux = TsDemux::new(1 << 20);
    let err = demux.push(&gapped).unwrap_err();
    assert!(matches!(err, TsDemuxError::Discontinuity { .. }), "{err:?}");
    let recovered = demux.push(&[]).expect("resumed");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].pts(), 3000);
}

#[test]
fn bad_pat_crc_errors() {
    let mut stream = stream();
    stream[20] ^= 0xFF;
    let mut demux = TsDemux::new(1 << 20);
    let err = demux.push(&stream).unwrap_err();
    assert_eq!(err, TsDemuxError::BadCrc { pid: 0 });
}

#[test]
fn bad_pes_start_code_errors() {
    let mut stream = stream();
    let adapt = usize::from(stream[2 * 188 + 4]);
    assert!(adapt >= 7);
    stream[2 * 188 + 5 + adapt] ^= 0xFF;
    let mut demux = TsDemux::new(1 << 20);
    let err = demux.push(&stream).unwrap_err();
    assert!(matches!(err, TsDemuxError::BadPacket(_)), "{err:?}");
}

#[test]
fn oversized_pes_errors() {
    let stream = stream();
    let mut demux = TsDemux::new(4);
    let err = demux.push(&stream).unwrap_err();
    assert!(matches!(err, TsDemuxError::PesTooLarge { .. }), "{err:?}");
}

#[test]
fn truncated_tail_buffers_without_error() {
    let stream = stream();
    let cut = stream.len() - 100;
    let mut demux = TsDemux::new(1 << 20);
    let head_units = demux.push(&stream[..cut]).expect("head parses");
    assert_eq!(head_units.len(), 1);
    let tail_units = demux.push(&stream[cut..]).expect("tail completes");
    assert_eq!(tail_units.len(), 1);
}

#[test]
fn duplicate_packet_is_ignored() {
    let stream = stream();
    let mut doubled = Vec::with_capacity(stream.len() + 188);
    doubled.extend_from_slice(&stream[..3 * 188]);
    doubled.extend_from_slice(&stream[2 * 188..3 * 188]);
    doubled.extend_from_slice(&stream[3 * 188..]);
    let mut demux = TsDemux::new(1 << 20);
    let units = demux.push(&doubled).expect("parsed");
    assert_eq!(units.len(), 2);
}

#[test]
fn empty_push_is_silent() {
    let mut demux = TsDemux::new(1 << 20);
    assert!(demux.push(&[]).expect("parsed").is_empty());
}
