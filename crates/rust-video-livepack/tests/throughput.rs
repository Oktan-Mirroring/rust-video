//! Livepack throughput: TS mux/demux plus fMP4 mux against the 60 fps bar.
//!
//! Pure CPU; always runs.

use rust_video_livepack::{
    AccessUnit, AvcConfig, Codec, FragmentMux, PacketAction, TrackConfig, TsDemux, TsMux,
    TsMuxConfig,
};

/// Timed iterations per measurement.
const ITERS: usize = 30;

/// Tier realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Warmup units ahead of the timed loop.
const WARMUP: usize = 4;

/// Keyframe IDR filler bytes per synthetic keyframe unit.
const KEY_BYTES: usize = 96_000;

/// Inter P-slice filler bytes per synthetic inter unit.
const INTER_BYTES: usize = 24_000;

/// Timed iteration count as float.
fn iters() -> f64 {
    f64::from(u32::try_from(ITERS).expect("iter count fits"))
}

/// Annex-B unit from `(header, filler, len)` NALUs.
fn annexb(nalus: &[(u8, u8, usize)], pts: u64, dts: u64) -> AccessUnit {
    let mut payload = Vec::new();
    for (header, filler, len) in nalus {
        payload.extend_from_slice(&[0, 0, 0, 1, *header]);
        payload.extend(std::iter::repeat_n(*filler, *len));
    }
    AccessUnit::classified(Codec::H264, payload, pts, dts)
}

/// Warmup plus timed units; every tenth is a keyframe.
fn h264_units() -> Vec<AccessUnit> {
    (0..WARMUP + ITERS)
        .map(|i| {
            let ts = u64::try_from(i).expect("index fits") * 3000;
            if i % 10 == 0 {
                annexb(
                    &[(0x67, 0x11, 24), (0x68, 0x22, 8), (0x65, 0x47, KEY_BYTES)],
                    ts,
                    ts,
                )
            } else {
                annexb(&[(0x41, 0x33, INTER_BYTES)], ts, ts)
            }
        })
        .collect()
}

/// Measured fps muxing `timed` after `warmup`.
fn mux_fps(mux: &mut TsMux, warmup: &[AccessUnit], timed: &[AccessUnit]) -> f64 {
    let mut out = Vec::new();
    for unit in warmup {
        mux.push(unit, None, &mut out).expect("warmup mux");
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for unit in timed {
        match mux.push(unit, None, &mut out).expect("timed mux") {
            PacketAction::Emitted(n) => bytes += n,
            PacketAction::HeldForKeyframe => panic!("gate open"),
        }
        out.clear();
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    assert_eq!(timed.len(), ITERS);
    iters() / elapsed
}

/// Measured fps demuxing pre-muxed `timed` after `warmup`.
fn demux_fps(demux: &mut TsDemux, warmup: &[Vec<u8>], timed: &[Vec<u8>]) -> f64 {
    for stream in warmup {
        demux.push(stream).expect("warmup demux");
    }
    let mut units = 0;
    let start = std::time::Instant::now();
    for stream in timed {
        units += demux.push(stream).expect("timed demux").len();
    }
    units += demux.finish().into_iter().len();
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(units, timed.len());
    assert_eq!(timed.len(), ITERS);
    iters() / elapsed
}

/// H.264 track over baseline parameter sets.
fn avc_track() -> TrackConfig {
    let sps = vec![vec![0x67, 0x42, 0x00, 0x1E, 0xF4, 0x02, 0x80, 0x2D, 0xC8]];
    let pps = vec![vec![0x68, 0xCE, 0x38, 0x80]];
    TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid sets"))
}

/// Measured fps fragmenting `timed` after `warmup`.
fn fmp4_fps(mux: &mut FragmentMux, warmup: &[AccessUnit], timed: &[AccessUnit]) -> f64 {
    let mut out = Vec::new();
    for unit in warmup {
        mux.push_sample(unit, 3000, None, &mut out)
            .expect("warmup fragment");
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for unit in timed {
        match mux
            .push_sample(unit, 3000, None, &mut out)
            .expect("timed fragment")
        {
            PacketAction::Emitted(n) => bytes += n,
            PacketAction::HeldForKeyframe => panic!("gate open"),
        }
        out.clear();
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    assert_eq!(timed.len(), ITERS);
    iters() / elapsed
}

/// TS mux measured against the realtime bar.
#[test]
fn ts_mux_measures_realtime() {
    let units = h264_units();
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let measured = mux_fps(&mut mux, &units[..WARMUP], &units[WARMUP..]);
    println!("livepack ts mux : {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// TS demux measured against the realtime bar.
#[test]
fn ts_demux_measures_realtime() {
    let units = h264_units();
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut streams = Vec::with_capacity(units.len());
    for unit in &units {
        let mut stream = Vec::new();
        mux.push(unit, None, &mut stream).expect("packetized");
        streams.push(stream);
    }
    let mut demux = TsDemux::new(1 << 20);
    let measured = demux_fps(&mut demux, &streams[..WARMUP], &streams[WARMUP..]);
    println!("livepack ts demux : {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// fMP4 mux measured against the realtime bar.
#[test]
fn fmp4_mux_measures_realtime() {
    let units = h264_units();
    let mut mux = FragmentMux::new(avc_track(), 1920, 1080).expect("valid track");
    let measured = fmp4_fps(&mut mux, &units[..WARMUP], &units[WARMUP..]);
    println!("livepack fmp4 mux : {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}
