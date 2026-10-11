//! `FFmpeg` oracle: real streams through both muxes.
//!
//! Skips when `ffmpeg`/`ffprobe` or the needed encoder is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use rust_video_livepack::{
    AccessUnit, AnnexB, Av1Config, AvcConfig, Codec, FragmentMux, HevcConfig, TrackConfig, TsDemux,
    TsMux, TsMuxConfig,
};

/// Working directory for oracle artifacts.
fn workdir() -> PathBuf {
    let dir = std::env::temp_dir().join("livepack-ffmpeg");
    std::fs::create_dir_all(&dir).expect("workdir");
    dir
}

/// True when `tool -version` runs.
fn have_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// True when the ffmpeg build encodes `encoder`.
fn have_encoder(encoder: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(encoder))
}

/// Run `tool` with `args`, returning stdout; panics on failure.
fn run(tool: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn {tool}: {e}"));
    assert!(
        out.status.success(),
        "{tool} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// `(pts, dts)` per video packet in 90 kHz ticks, 33-bit wrapped.
fn probe_packets_90k(path: &Path) -> Vec<(u64, u64)> {
    let name = path.to_str().expect("utf8 path");
    let base = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=time_base",
            "-of",
            "csv=p=0",
            name,
        ],
    );
    let base = String::from_utf8_lossy(&base);
    let first = base.lines().next().expect("time base");
    let (num, den) = first.trim().split_once('/').expect("time base");
    let num: i64 = num.parse().expect("base num");
    let den: i64 = den.parse().expect("base den");
    let packets = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts,dts",
            "-of",
            "csv=p=0",
            name,
        ],
    );
    let wrap = 1i64 << 33;
    String::from_utf8_lossy(&packets)
        .lines()
        .map(|line| {
            let mut fields = line.split(',');
            let pts: i64 = fields.next().expect("pts").parse().expect("pts");
            let dts: i64 = fields.next().expect("dts").parse().expect("dts");
            let scale = |v: i64| {
                let scaled = (v * num * 90_000 / den) % wrap;
                u64::try_from((scaled + wrap) % wrap).unwrap_or(u64::MAX)
            };
            (scale(pts), scale(dts))
        })
        .collect()
}

/// `stream=codec_name,width,height` plus `format_tags` major/compatible brands.
fn probe_stream(path: &Path) -> String {
    let name = path.to_str().expect("utf8 path");
    String::from_utf8_lossy(&run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height:format_tags=major_brand,compatible_brands",
            "-of",
            "default=noprint_wrappers=1",
            name,
        ],
    ))
    .into_owned()
}

/// Decoded frame count.
fn probe_frame_count(path: &Path) -> usize {
    let name = path.to_str().expect("utf8 path");
    String::from_utf8_lossy(&run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "csv=p=0",
            name,
        ],
    ))
    .lines()
    .next()
    .expect("frame count")
    .trim()
    .parse()
    .expect("frame count")
}

/// NALUs of an annex-B file.
fn annexb_nalus(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).expect("annexb");
    AnnexB::new(&bytes).map(<[u8]>::to_vec).collect()
}

/// NALUs of demuxed units in order.
fn units_nalus(units: &[AccessUnit]) -> Vec<Vec<u8>> {
    units
        .iter()
        .flat_map(|u| AnnexB::new(u.payload()).map(<[u8]>::to_vec))
        .collect()
}

/// Tagged box payload: first size-valid candidate with `tag`.
fn find_tagged(buf: &[u8], tag: [u8; 4]) -> Vec<&[u8]> {
    let mut hits = Vec::new();
    for at in 4..buf.len().saturating_sub(8) {
        if buf[at..at + 4] != tag {
            continue;
        }
        let size = u32::from_be_bytes([buf[at - 4], buf[at - 3], buf[at - 2], buf[at - 1]]);
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        if (8..=(1 << 20)).contains(&size) && at - 4 + size <= buf.len() {
            hits.push(&buf[at + 4..at - 4 + size]);
        }
    }
    hits
}

/// Our demux over a whole file, flushed at the end.
fn demux_file(path: &Path) -> Vec<AccessUnit> {
    let bytes = std::fs::read(path).expect("stream bytes");
    let mut demux = TsDemux::new(8 << 20);
    let mut units = Vec::new();
    for piece in bytes.chunks(4096) {
        units.extend(demux.push(piece).expect("parsed"));
    }
    units.extend(demux.finish());
    units
}

/// Sample durations from DTS deltas; last repeats the first.
fn durations(units: &[AccessUnit]) -> Vec<u32> {
    let wrap = 1u64 << 33;
    let delta = |a: u64, b: u64| u32::try_from((b + wrap - a) % wrap).expect("duration");
    let mut out: Vec<u32> = units
        .windows(2)
        .map(|w| delta(w[0].dts(), w[1].dts()))
        .collect();
    out.push(out.first().copied().unwrap_or(3000));
    out
}

/// `(sps, pps)` NALUs from the first keyframe unit.
fn h264_sets(units: &[AccessUnit]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let key = units.iter().find(|u| u.is_keyframe()).expect("keyframe");
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    for nalu in AnnexB::new(key.payload()) {
        match nalu.first().map(|b| b & 0x1F) {
            Some(7) => sps.push(nalu.to_vec()),
            Some(8) => pps.push(nalu.to_vec()),
            _ => {}
        }
    }
    assert!(!sps.is_empty() && !pps.is_empty());
    (sps, pps)
}

/// Fragmented file: init plus one chunk per unit.
fn fragment(track: TrackConfig, width: u32, height: u32, units: &[AccessUnit], path: &Path) {
    let mut mux = FragmentMux::new(track, width, height).expect("mux");
    let mut out = mux.init_segment();
    out.extend_from_slice(&FragmentMux::start_segment());
    for (unit, duration) in units.iter().zip(durations(units)) {
        mux.push_sample(unit, duration, None, &mut out)
            .expect("fragment");
    }
    std::fs::write(path, out).expect("write");
}

/// IVF frames: `(payload, pts)`.
fn ivf_frames(path: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).expect("ivf");
    assert_eq!(&bytes[..4], b"DKIF");
    assert_eq!(&bytes[8..12], b"AV01");
    let mut frames = Vec::new();
    let mut at = 32;
    while at + 12 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("size"));
        let size = usize::try_from(size).expect("size");
        at += 12;
        frames.push(bytes[at..at + size].to_vec());
        at += size;
    }
    frames
}

#[test]
fn h264_ts_to_fmp4_round_trip() {
    if !have_tool("ffmpeg") || !have_tool("ffprobe") || !have_encoder("libx264") {
        eprintln!("skip: ffmpeg/ffprobe/libx264 missing");
        return;
    }
    let dir = workdir();
    let ts = dir.join("h264.ts");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30:duration=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mpegts",
            ts.to_str().expect("utf8"),
        ],
    );
    let units = demux_file(&ts);
    assert_eq!(units.len(), 30);
    assert!(units[0].is_keyframe());

    let annexb = dir.join("h264.264");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-i",
            ts.to_str().expect("utf8"),
            "-c",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "h264",
            annexb.to_str().expect("utf8"),
        ],
    );
    assert_eq!(units_nalus(&units), annexb_nalus(&annexb));

    let oracle = probe_packets_90k(&ts);
    assert_eq!(oracle.len(), units.len());
    for (i, unit) in units.iter().enumerate() {
        assert_eq!((unit.pts(), unit.dts()), oracle[i], "packet {i}");
    }

    let (sps, pps) = h264_sets(&units);
    let track = TrackConfig::Avc(AvcConfig::from_parameter_sets(sps, pps).expect("avcC"));
    let m4s = dir.join("h264.m4s");
    fragment(track, 320, 240, &units, &m4s);

    let info = probe_stream(&m4s);
    assert!(info.contains("codec_name=h264"), "{info}");
    assert!(info.contains("width=320"), "{info}");
    assert!(info.contains("height=240"), "{info}");
    assert!(info.contains("major_brand=isom"), "{info}");
    assert!(info.contains("cmfc"), "{info}");
    assert_eq!(probe_frame_count(&m4s), 30);
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            m4s.to_str().expect("utf8"),
            "-f",
            "null",
            "-",
        ],
    );

    let back = probe_packets_90k(&m4s);
    assert_eq!(back.len(), units.len());
    for (i, unit) in units.iter().enumerate() {
        assert_eq!((unit.pts(), unit.dts()), back[i], "fragment {i}");
    }
}

#[test]
fn h265_ts_to_fmp4_round_trip() {
    if !have_tool("ffmpeg") || !have_tool("ffprobe") || !have_encoder("libx265") {
        eprintln!("skip: ffmpeg/ffprobe/libx265 missing");
        return;
    }
    let dir = workdir();
    let ts = dir.join("h265.ts");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30:duration=1",
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mpegts",
            ts.to_str().expect("utf8"),
        ],
    );
    let units = demux_file(&ts);
    assert_eq!(units.len(), 30);

    let annexb = dir.join("h265.265");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-i",
            ts.to_str().expect("utf8"),
            "-c",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb",
            "-f",
            "hevc",
            annexb.to_str().expect("utf8"),
        ],
    );
    assert_eq!(units_nalus(&units), annexb_nalus(&annexb));

    let mp4 = dir.join("h265.mp4");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-i",
            ts.to_str().expect("utf8"),
            "-c",
            "copy",
            mp4.to_str().expect("utf8"),
        ],
    );
    let mp4_bytes = std::fs::read(&mp4).expect("mp4");
    let hvcc = find_tagged(&mp4_bytes, *b"hvcC")
        .into_iter()
        .find(|r| r.len() >= 23 && r[0] == 1 && r[22] & 0x03 == 0x03)
        .expect("hvcC")
        .to_vec();
    let track = TrackConfig::Hevc(HevcConfig::from_hvcc_record(hvcc).expect("hvcc"));
    let m4s = dir.join("h265.m4s");
    fragment(track, 320, 240, &units, &m4s);

    let info = probe_stream(&m4s);
    assert!(info.contains("codec_name=hevc"), "{info}");
    assert_eq!(probe_frame_count(&m4s), 30);
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            m4s.to_str().expect("utf8"),
            "-f",
            "null",
            "-",
        ],
    );
}

#[test]
fn av1_mp4_to_fmp4_round_trip() {
    if !have_tool("ffmpeg") || !have_tool("ffprobe") || !have_encoder("libsvtav1") {
        eprintln!("skip: ffmpeg/ffprobe/libsvtav1 missing");
        return;
    }
    let dir = workdir();
    let mp4 = dir.join("av1.mp4");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30:duration=0.5",
            "-c:v",
            "libsvtav1",
            "-preset",
            "12",
            "-pix_fmt",
            "yuv420p",
            mp4.to_str().expect("utf8"),
        ],
    );
    let ivf = dir.join("av1.ivf");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-i",
            mp4.to_str().expect("utf8"),
            "-c",
            "copy",
            "-f",
            "ivf",
            ivf.to_str().expect("utf8"),
        ],
    );
    let frames = ivf_frames(&ivf);
    let stamps = probe_packets_90k(&mp4);
    assert_eq!(frames.len(), stamps.len());
    assert_eq!(frames.len(), 15);
    let units: Vec<AccessUnit> = frames
        .into_iter()
        .zip(stamps)
        .map(|(payload, (pts, dts))| AccessUnit::classified(Codec::Av1, payload, pts, dts))
        .collect();
    assert!(units[0].is_keyframe());

    let mp4_bytes = std::fs::read(&mp4).expect("mp4");
    let av1c = find_tagged(&mp4_bytes, *b"av1C")
        .into_iter()
        .find(|r| r.len() >= 4 && r[0] == 0x81)
        .expect("av1C");
    let track = TrackConfig::Av1(Av1Config::from_av1c_record(av1c).expect("av1C"));
    let m4s = dir.join("av1.m4s");
    fragment(track, 320, 240, &units, &m4s);

    let info = probe_stream(&m4s);
    assert!(info.contains("codec_name=av1"), "{info}");
    assert_eq!(probe_frame_count(&m4s), 15);
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            m4s.to_str().expect("utf8"),
            "-f",
            "null",
            "-",
        ],
    );
}

#[test]
fn ts_mux_accepted_by_ffmpeg() {
    if !have_tool("ffmpeg") || !have_tool("ffprobe") {
        eprintln!("skip: ffmpeg/ffprobe missing");
        return;
    }
    let dir = workdir();
    let sps = vec![0x67, 0x42, 0x00, 0x1E, 0xF4, 0x02, 0x80, 0x2D, 0xC8];
    let pps = vec![0x68, 0xCE, 0x38, 0x80];
    let mut idr = [
        vec![0, 0, 0, 1],
        sps,
        vec![0, 0, 0, 1],
        pps,
        vec![0, 0, 0, 1, 0x65],
    ]
    .concat();
    idr.extend(std::iter::repeat_n(0xAA, 700));
    let mut units = vec![AccessUnit::classified(Codec::H264, idr, 0, 0)];
    for i in 1..8u64 {
        let mut inter = vec![0, 0, 0, 1, 0x41];
        inter.extend(std::iter::repeat_n(0xBB, 600));
        units.push(AccessUnit::new(
            Codec::H264,
            inter,
            i * 3000,
            i * 3000,
            false,
        ));
    }
    let mut mux = TsMux::new(TsMuxConfig::default()).expect("valid config");
    let mut stream = Vec::new();
    for unit in &units {
        mux.push(unit, None, &mut stream).expect("packetized");
    }
    let ts = dir.join("ours.ts");
    std::fs::write(&ts, &stream).expect("write");

    let programs = String::from_utf8_lossy(&run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_entries",
            "program=program_num,pmt_pid",
            "-of",
            "csv=p=0",
            ts.to_str().expect("utf8"),
        ],
    ))
    .into_owned();
    assert!(programs.contains("1,4096"), "{programs}");

    let annexb = dir.join("ours.264");
    run(
        "ffmpeg",
        &[
            "-y",
            "-v",
            "error",
            "-i",
            ts.to_str().expect("utf8"),
            "-c",
            "copy",
            "-f",
            "h264",
            annexb.to_str().expect("utf8"),
        ],
    );
    assert_eq!(annexb_nalus(&annexb), units_nalus(&units));
}
