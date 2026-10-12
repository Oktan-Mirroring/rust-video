//! VP8/VP9 fMP4 packing: muxed sample bytes match the inputs.
//!
//! `FFmpeg` oracle at the bottom runs only when `libvpx-vp9` exists.

use rust_video_livepack::{AccessUnit, Codec, FragmentMux, TrackConfig, Vp8Config, Vp9Config};

/// `vpcC` box: version 1, zero flags, 8-bit 4:2:0, no init data.
fn vpcc() -> [u8; 12] {
    [1, 0, 0, 0, 0, 0x1F, 0xC1, 1, 1, 1, 0, 0]
}

/// `mdat` payloads in box order; nested boxes skipped by size.
fn mdat_payloads(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let size = usize::try_from(u32::from_be_bytes(
            bytes[at..at + 4].try_into().expect("size"),
        ))
        .expect("size");
        if size < 8 || at + size > bytes.len() {
            break;
        }
        if &bytes[at + 4..at + 8] == b"mdat" {
            out.push(&bytes[at + 8..at + size]);
        }
        at += size;
    }
    out
}

/// Mux `units` on `track`; returns init plus one chunk per unit.
fn mux_all(track: TrackConfig, units: &[AccessUnit]) -> Vec<u8> {
    let mut mux = FragmentMux::new(track, 320, 240).expect("mux");
    let mut out = mux.init_segment();
    out.extend_from_slice(&FragmentMux::start_segment());
    for (i, unit) in units.iter().enumerate() {
        mux.push_sample(unit, 3000, None, &mut out)
            .unwrap_or_else(|e| panic!("fragment {i}: {e}"));
    }
    out
}

/// VP9 key plus inter units classified from payload bits.
fn vp9_units() -> Vec<AccessUnit> {
    let mut key = vec![0x80, 0x00];
    key.extend(std::iter::repeat_n(0xAA, 200));
    let mut inter = vec![0x84, 0x00];
    inter.extend(std::iter::repeat_n(0xBB, 150));
    vec![
        AccessUnit::classified(Codec::Vp9, key, 0, 0),
        AccessUnit::classified(Codec::Vp9, inter, 3000, 3000),
    ]
}

/// VP8 key plus inter units classified from the frame tag.
fn vp8_units() -> Vec<AccessUnit> {
    let mut key = vec![0x10, 0x9D, 0x01, 0x2A];
    key.extend(std::iter::repeat_n(0xAA, 200));
    let mut inter = vec![0x01];
    inter.extend(std::iter::repeat_n(0xBB, 150));
    vec![
        AccessUnit::classified(Codec::Vp8, key, 0, 0),
        AccessUnit::classified(Codec::Vp8, inter, 3000, 3000),
    ]
}

#[test]
fn vp9_samples_survive_mux() {
    let units = vp9_units();
    assert!(units[0].is_keyframe());
    assert!(!units[1].is_keyframe());
    let track = TrackConfig::Vp9(Vp9Config::from_vpcc_record(&vpcc()).expect("vpcC"));
    let out = mux_all(track, &units);
    assert!(out.windows(4).any(|w| w == b"vp09"));
    assert!(out.windows(4).any(|w| w == b"vpcC"));
    let mdats = mdat_payloads(&out);
    assert_eq!(mdats.len(), units.len());
    for (got, want) in mdats.iter().zip(&units) {
        assert_eq!(*got, want.payload());
    }
    assert!(out.windows(4).any(|w| w == [0x02, 0x00, 0x00, 0x00]));
}

#[test]
fn vp8_samples_survive_mux() {
    let units = vp8_units();
    assert!(units[0].is_keyframe());
    assert!(!units[1].is_keyframe());
    let track = TrackConfig::Vp8(Vp8Config::from_vpcc_record(&vpcc()).expect("vpcC"));
    let out = mux_all(track, &units);
    assert!(out.windows(4).any(|w| w == b"vp08"));
    assert!(out.windows(4).any(|w| w == b"vpcC"));
    let mdats = mdat_payloads(&out);
    assert_eq!(mdats.len(), units.len());
    for (got, want) in mdats.iter().zip(&units) {
        assert_eq!(*got, want.payload());
    }
}

/// Working directory for oracle artifacts.
fn workdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("livepack-vp9");
    std::fs::create_dir_all(&dir).expect("workdir");
    dir
}

/// True when `tool -version` runs.
fn have_tool(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// True when the ffmpeg build encodes `encoder`.
fn have_encoder(encoder: &str) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(encoder))
}

/// Run `tool` with `args`, returning stdout; panics on failure.
fn run(tool: &str, args: &[&str]) -> Vec<u8> {
    let out = std::process::Command::new(tool)
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

/// IVF frames with the VP9 `VP90` fourcc: `(payload, size)` order.
fn ivf_frames_vp9(path: &std::path::Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).expect("ivf");
    assert_eq!(&bytes[..4], b"DKIF");
    assert_eq!(&bytes[8..12], b"VP90");
    let mut frames = Vec::new();
    let mut at = 32;
    while at + 12 <= bytes.len() {
        let size = usize::try_from(u32::from_le_bytes(
            bytes[at..at + 4].try_into().expect("size"),
        ))
        .expect("size");
        at += 12;
        frames.push(bytes[at..at + size].to_vec());
        at += size;
    }
    frames
}

/// Tagged box payload: first size-valid candidate with `tag`.
fn find_tagged(buf: &[u8], tag: [u8; 4]) -> Vec<&[u8]> {
    let mut hits = Vec::new();
    for at in 4..buf.len().saturating_sub(8) {
        if buf[at..at + 4] != tag {
            continue;
        }
        let size = usize::try_from(u32::from_be_bytes([
            buf[at - 4],
            buf[at - 3],
            buf[at - 2],
            buf[at - 1],
        ]))
        .unwrap_or(usize::MAX);
        if (8..=(1 << 20)).contains(&size) && at - 4 + size <= buf.len() {
            hits.push(&buf[at + 4..at - 4 + size]);
        }
    }
    hits
}

#[test]
fn vp9_ffmpeg_oracle() {
    if !have_tool("ffmpeg") || !have_tool("ffprobe") || !have_encoder("libvpx-vp9") {
        eprintln!("skip: ffmpeg/ffprobe/libvpx-vp9 missing");
        return;
    }
    let dir = workdir();
    let mp4 = dir.join("vp9.mp4");
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
            "libvpx-vp9",
            "-pix_fmt",
            "yuv420p",
            mp4.to_str().expect("utf8"),
        ],
    );
    let ivf = dir.join("vp9.ivf");
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
    let frames = ivf_frames_vp9(&ivf);
    assert_eq!(frames.len(), 15);
    let units: Vec<AccessUnit> = frames
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            let ts = u64::try_from(i).expect("ts") * 3000;
            AccessUnit::classified(Codec::Vp9, payload, ts, ts)
        })
        .collect();
    assert!(units[0].is_keyframe());

    let mp4_bytes = std::fs::read(&mp4).expect("mp4");
    let vpc = find_tagged(&mp4_bytes, *b"vpcC")
        .into_iter()
        .find(|r| r.len() == 12 && r[0] == 1)
        .expect("vpcC");
    let track = TrackConfig::Vp9(Vp9Config::from_vpcc_record(vpc).expect("vpcC"));
    let out = mux_all(track, &units);
    let m4s = dir.join("vp9.m4s");
    std::fs::write(&m4s, &out).expect("write");

    let info = String::from_utf8_lossy(&run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height",
            "-of",
            "default=noprint_wrappers=1",
            m4s.to_str().expect("utf8"),
        ],
    ))
    .into_owned();
    assert!(info.contains("codec_name=vp9"), "{info}");
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
