//! Livepack throughput: TS mux/demux plus fMP4 mux against the 60 fps bar.
//!
//! Pure CPU; always runs.

use rust_video_livepack::{
    AccessUnit, AvcConfig, Codec, FragmentMux, PacketAction, TrackConfig, TsDemux, TsMux,
    TsMuxConfig,
};

/// Timed iterations per batch.
const ITERS: usize = 30;

// Batches per leg, each sizing the timed window past 0.15s at the leg's
// pace (mux ~500k, fmp4 ~165k, demux ~495k fps), past timer grain.
const MUX_BATCHES: usize = 3000;
const FMP4_BATCHES: usize = 1000;
const DEMUX_BATCHES: usize = 6000;

/// Tier realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Warmup units ahead of the timed loop.
const WARMUP: usize = 4;

/// Keyframe IDR filler bytes per synthetic keyframe unit.
const KEY_BYTES: usize = 96_000;

/// Inter P-slice filler bytes per synthetic inter unit.
const INTER_BYTES: usize = 24_000;

/// Timed operations as float.
fn ops(batches: usize) -> f64 {
    f64::from(u32::try_from(ITERS * batches).expect("op count fits"))
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

/// Measured fps muxing `timed` after `warmup`, repeated `batches` times.
///
/// Repeats share timestamps; the mux stamps without validating order.
fn mux_fps(mux: &mut TsMux, warmup: &[AccessUnit], timed: &[AccessUnit], batches: usize) -> f64 {
    let mut out = Vec::new();
    for unit in warmup {
        mux.push(unit, None, &mut out).expect("warmup mux");
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for _ in 0..batches {
        for unit in timed {
            match mux.push(unit, None, &mut out).expect("timed mux") {
                PacketAction::Emitted(n) => bytes += n,
                PacketAction::HeldForKeyframe => panic!("gate open"),
            }
            out.clear();
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    assert_eq!(timed.len(), ITERS);
    ops(batches) / elapsed
}

/// Measured fps demuxing pre-muxed `timed` after `warmup`.
///
/// Fresh demux per batch: repeats trip continuity on one instance.
fn demux_fps(warmup: &[Vec<u8>], timed: &[Vec<u8>], batches: usize) -> f64 {
    let mut units = 0;
    let mut elapsed = 0.0;
    for _ in 0..batches {
        let mut demux = TsDemux::new(1 << 20);
        for stream in warmup {
            demux.push(stream).expect("warmup demux");
        }
        let start = std::time::Instant::now();
        for stream in timed {
            units += demux.push(stream).expect("timed demux").len();
        }
        units += demux.finish().into_iter().len();
        elapsed += start.elapsed().as_secs_f64();
    }
    assert_eq!(units, timed.len() * batches);
    assert_eq!(timed.len(), ITERS);
    ops(batches) / elapsed
}

/// H.264 track over baseline parameter sets.
fn avc_track() -> TrackConfig {
    let sps = vec![vec![0x67, 0x42, 0x00, 0x1E, 0xF4, 0x02, 0x80, 0x2D, 0xC8]];
    let pps = vec![vec![0x68, 0xCE, 0x38, 0x80]];
    TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("valid sets"))
}

/// Measured fps fragmenting `timed` after `warmup`, repeated `batches` times.
///
/// Repeats share timestamps; samples carry them unvalidated.
fn fmp4_fps(
    mux: &mut FragmentMux,
    warmup: &[AccessUnit],
    timed: &[AccessUnit],
    batches: usize,
) -> f64 {
    let mut out = Vec::new();
    for unit in warmup {
        mux.push_sample(unit, 3000, None, &mut out)
            .expect("warmup fragment");
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for _ in 0..batches {
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
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    assert_eq!(timed.len(), ITERS);
    ops(batches) / elapsed
}

/// TS mux measured against the realtime bar.
#[test]
fn ts_mux_measures_realtime() {
    let units = h264_units();
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let measured = mux_fps(&mut mux, &units[..WARMUP], &units[WARMUP..], MUX_BATCHES);
    let total = ITERS * MUX_BATCHES;
    println!("livepack ts mux : {measured:.1}fps over {total} ops, bar {BAR:.0}fps");
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
    let measured = demux_fps(&streams[..WARMUP], &streams[WARMUP..], DEMUX_BATCHES);
    let total = ITERS * DEMUX_BATCHES;
    println!("livepack ts demux : {measured:.1}fps over {total} ops, bar {BAR:.0}fps");
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
    let measured = fmp4_fps(&mut mux, &units[..WARMUP], &units[WARMUP..], FMP4_BATCHES);
    let total = ITERS * FMP4_BATCHES;
    println!("livepack fmp4 mux : {measured:.1}fps over {total} ops, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}
