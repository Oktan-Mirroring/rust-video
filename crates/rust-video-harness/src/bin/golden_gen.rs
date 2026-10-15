//! Golden generator over the vector corpus.
//!
//! ffmpeg oracle: probe, decode, hash, cover. See
//! `docs/testing/goldens.md` for the coverage rule.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use rust_video_harness::FrameHash;

struct Probe {
    profile: String,
    pix_fmt: String,
    width: u32,
    height: u32,
}

struct Vector {
    codec_dir: String,
    path: PathBuf,
    probe: Probe,
    synth: Option<String>,
}

struct Req {
    codec: &'static str,
    name: &'static str,
    matches: fn(&Vector) -> bool,
    blocked: Option<&'static str>,
    synth: Option<&'static str>,
}

struct Synth {
    name: &'static str,
    codec_dir: &'static str,
    width: u32,
    height: u32,
    pix_fmt: &'static str,
    encoder: &'static str,
    extra: &'static [&'static str],
}

fn norm(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_420(v: &Vector) -> bool {
    v.probe.pix_fmt.contains("420")
}

fn is_422(v: &Vector) -> bool {
    v.probe.pix_fmt.contains("422")
}

fn is_444(v: &Vector) -> bool {
    v.probe.pix_fmt.contains("444")
}

fn is_d10(v: &Vector) -> bool {
    v.probe.pix_fmt.contains("10")
}

fn is_d12(v: &Vector) -> bool {
    v.probe.pix_fmt.contains("12")
}

const fn always(_: &Vector) -> bool {
    true
}

fn req(
    codec: &'static str,
    name: &'static str,
    matches: fn(&Vector) -> bool,
    synth: Option<&'static str>,
) -> Req {
    Req {
        codec,
        name,
        matches,
        blocked: None,
        synth,
    }
}

fn h264_reqs() -> Vec<Req> {
    vec![
        req("h264", "pix-420", is_420, Some("h264-high")),
        req("h264", "pix-422", is_422, Some("h264-high422")),
        req("h264", "pix-444", is_444, Some("h264-high444")),
        req(
            "h264",
            "hot-1080p-high",
            |v| v.probe.width == 1920 && v.probe.height == 1080 && norm(&v.probe.profile) == "high",
            Some("h264-high"),
        ),
    ]
}

fn h265_reqs() -> Vec<Req> {
    vec![
        req(
            "h265",
            "profile-main",
            |v| norm(&v.probe.profile) == "main",
            Some("h265-main"),
        ),
        req(
            "h265",
            "profile-main10",
            |v| norm(&v.probe.profile).contains("main10"),
            Some("h265-main10"),
        ),
        req("h265", "depth-10", is_d10, Some("h265-main10")),
        req(
            "h265",
            "hot-4k-main10",
            |v| {
                v.probe.width == 3840
                    && v.probe.height == 2160
                    && norm(&v.probe.profile).contains("main10")
            },
            Some("h265-4k-main10"),
        ),
    ]
}

fn av1_reqs() -> Vec<Req> {
    vec![
        req(
            "av1",
            "profile-main",
            |v| norm(&v.probe.profile) == "main",
            Some("av1-main"),
        ),
        req(
            "av1",
            "profile-high",
            |v| norm(&v.probe.profile) == "high",
            Some("av1-high444"),
        ),
        req(
            "av1",
            "profile-professional",
            |v| norm(&v.probe.profile) == "professional",
            Some("av1-pro42212"),
        ),
        req("av1", "pix-444", is_444, Some("av1-high444")),
        req("av1", "depth-10", is_d10, Some("av1-main10")),
        req("av1", "depth-12", is_d12, Some("av1-pro42212")),
        req(
            "av1",
            "hot-1080p",
            |v| v.probe.width == 1920 && v.probe.height == 1080,
            Some("av1-1080p"),
        ),
        req(
            "av1",
            "hot-grain",
            |v| v.path.to_string_lossy().contains("grain"),
            Some("av1-grain"),
        ),
    ]
}

fn vp9_reqs() -> Vec<Req> {
    vec![
        req("vp9", "p0-420-8", |v| v.probe.pix_fmt == "yuv420p", None),
        req(
            "vp9",
            "p1-444-8",
            |v| is_444(v) && !is_d10(v) && !is_d12(v),
            Some("vp9-p1-444"),
        ),
        req(
            "vp9",
            "p2-420-10",
            |v| v.probe.pix_fmt == "yuv420p10le",
            None,
        ),
        req(
            "vp9",
            "hot-1080p",
            |v| v.probe.width == 1920 && v.probe.height == 1080,
            None,
        ),
    ]
}

fn vp8_reqs() -> Vec<Req> {
    vec![req("vp8", "any", always, None)]
}

fn reqs() -> Vec<Req> {
    let mut out = Vec::new();
    out.extend(h264_reqs());
    out.extend(h265_reqs());
    out.extend(av1_reqs());
    out.extend(vp9_reqs());
    out.extend(vp8_reqs());
    out.push(Req {
        codec: "av2",
        name: "any",
        matches: always,
        blocked: Some("no public vectors or encoder"),
        synth: None,
    });
    out
}

fn synth(
    name: &'static str,
    codec_dir: &'static str,
    width: u32,
    height: u32,
    pix_fmt: &'static str,
    encoder: &'static str,
    extra: &'static [&'static str],
) -> Synth {
    Synth {
        name,
        codec_dir,
        width,
        height,
        pix_fmt,
        encoder,
        extra,
    }
}

fn h264_synths() -> Vec<Synth> {
    vec![
        synth(
            "h264-high",
            "h264",
            1920,
            1080,
            "yuv420p",
            "libx264",
            &["-preset", "veryfast"],
        ),
        synth(
            "h264-high422",
            "h264",
            640,
            480,
            "yuv422p",
            "libx264",
            &["-preset", "veryfast"],
        ),
        synth(
            "h264-high444",
            "h264",
            640,
            480,
            "yuv444p",
            "libx264",
            &["-preset", "veryfast"],
        ),
    ]
}

fn h265_synths() -> Vec<Synth> {
    vec![
        synth(
            "h265-main",
            "h265",
            640,
            480,
            "yuv420p",
            "libx265",
            &["-preset", "veryfast"],
        ),
        synth(
            "h265-main10",
            "h265",
            640,
            480,
            "yuv420p10le",
            "libx265",
            &["-preset", "veryfast"],
        ),
        synth(
            "h265-4k-main10",
            "h265",
            3840,
            2160,
            "yuv420p10le",
            "libx265",
            &["-preset", "ultrafast"],
        ),
    ]
}

fn av1_synths() -> Vec<Synth> {
    vec![
        synth("av1-main", "av1", 640, 480, "yuv420p", "libsvtav1", &[]),
        synth(
            "av1-main10",
            "av1",
            640,
            480,
            "yuv420p10le",
            "libsvtav1",
            &[],
        ),
        synth(
            "av1-1080p",
            "av1",
            1920,
            1080,
            "yuv420p",
            "libsvtav1",
            &["-preset", "10"],
        ),
        synth(
            "av1-high444",
            "av1",
            640,
            480,
            "yuv444p",
            "libaom-av1",
            &["-cpu-used", "8"],
        ),
        synth(
            "av1-pro42212",
            "av1",
            640,
            480,
            "yuv422p12le",
            "libaom-av1",
            &["-cpu-used", "8"],
        ),
        synth(
            "av1-grain",
            "av1",
            640,
            480,
            "yuv420p",
            "libaom-av1",
            &["-cpu-used", "8", "-aom-params", "film-grain-test=8"],
        ),
    ]
}

fn vp9_synths() -> Vec<Synth> {
    vec![synth(
        "vp9-p1-444",
        "vp9",
        640,
        480,
        "yuv444p",
        "libvpx-vp9",
        &["-cpu-used", "8", "-deadline", "realtime"],
    )]
}

fn synths() -> BTreeMap<&'static str, Synth> {
    let mut list = Vec::new();
    list.extend(h264_synths());
    list.extend(h265_synths());
    list.extend(av1_synths());
    list.extend(vp9_synths());
    list.into_iter().map(|s| (s.name, s)).collect()
}

fn spawn(cmd: &mut Command) -> Result<(bool, String, Vec<u8>), String> {
    let out = cmd.output().map_err(|e| format!("spawn failed: {e}"))?;
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((out.status.success(), text, out.stdout))
}

fn children(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{} unreadable: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    out.sort();
    Ok(out)
}

fn keep<T>(path: &Path, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(why) => {
            println!("skip {}: {why}", path.display());
            None
        }
    }
}

fn ffmpeg_version() -> Result<String, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-version"]);
    let (ok, _, out) = spawn(&mut cmd)?;
    if !ok {
        return Err("ffmpeg -version failed".to_owned());
    }
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .next()
        .unwrap_or("unknown")
        .to_owned())
}

fn probe(path: &Path) -> Result<Probe, String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=profile,pix_fmt,width,height",
        "-of",
        "default=noprint_wrappers=1:nokey=0",
    ]);
    cmd.arg(path);
    let (ok, err, out) = spawn(&mut cmd)?;
    if !ok {
        return Err(format!("ffprobe {} failed: {err}", path.display()));
    }
    let line = String::from_utf8_lossy(&out);
    let mut fields = BTreeMap::new();
    for entry in line.lines() {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(format!("ffprobe {} bad entry: {entry}", path.display()));
        };
        fields.insert(key, value);
    }
    let get = |key: &str| fields.get(key).copied().unwrap_or("unknown");
    let width: u32 = get("width")
        .parse()
        .map_err(|_| format!("bad width in {line}"))?;
    let height: u32 = get("height")
        .parse()
        .map_err(|_| format!("bad height in {line}"))?;
    Ok(Probe {
        profile: get("profile").to_owned(),
        pix_fmt: get("pix_fmt").to_owned(),
        width,
        height,
    })
}

fn frame_root_from(tmpdir: Option<&OsStr>) -> PathBuf {
    tmpdir
        .filter(|set| !set.is_empty())
        .map_or_else(|| PathBuf::from(".tmp"), PathBuf::from)
}

fn frame_root() -> Result<PathBuf, String> {
    let root = frame_root_from(env::var_os("TMPDIR").as_deref());
    fs::create_dir_all(&root).map_err(|e| format!("frame root failed: {e}"))?;
    Ok(root)
}

fn raw_frame_size(path: &Path, pix_fmt: &str, slot: usize, root: &Path) -> Result<usize, String> {
    let probe = root.join(format!(
        "rust-video-golden-{}-{slot}.probe",
        std::process::id()
    ));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-noautoscale",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        "rawvideo",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
    ]);
    cmd.arg(&probe);
    cmd.stdin(Stdio::null());
    let out = cmd.output().map_err(|e| format!("spawn failed: {e}"))?;
    let size = fs::metadata(&probe).map_or(0, |found| found.len());
    let _ = fs::remove_file(&probe);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("decode {} failed: {err}", path.display()));
    }
    if size == 0 {
        return Err(format!("decode {} yielded no frames", path.display()));
    }
    usize::try_from(size).map_err(|_| format!("decode {} frame too large", path.display()))
}

enum Shapes {
    Unknown,
    Uniform,
    Varied,
}

fn parse_shapes(text: &str) -> Shapes {
    let mut first: Option<&str> = None;
    let mut count = 0;
    for line in text.lines() {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
            return Shapes::Unknown;
        }
        count += 1;
        match first {
            None => first = Some(line),
            Some(want) if want == line => {}
            Some(_) => return Shapes::Varied,
        }
    }
    if count == 0 {
        Shapes::Unknown
    } else {
        Shapes::Uniform
    }
}

fn frame_shapes(path: &Path) -> Shapes {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "frame=width,height,pix_fmt",
        "-of",
        "csv=p=0",
    ]);
    cmd.arg(path);
    let Ok(out) = cmd.output() else {
        return Shapes::Unknown;
    };
    if !out.status.success() {
        return Shapes::Unknown;
    }
    parse_shapes(&String::from_utf8_lossy(&out.stdout))
}

/// Spill ceiling for the file fallback; a vector past it skips loudly.
const SPILL_CAP: u64 = 256 * 1024 * 1024;

fn dir_size(dir: &Path) -> Result<u64, String> {
    let mut total = 0;
    let entries = fs::read_dir(dir).map_err(|e| format!("{} unreadable: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("frame entry failed: {e}"))?;
        total += entry.metadata().map_or(0, |found| found.len());
    }
    Ok(total)
}

fn file_hashes(
    path: &Path,
    pix_fmt: &str,
    slot: usize,
    root: &Path,
) -> Result<Vec<FrameHash>, String> {
    let tag = format!("rust-video-golden-{}-{slot}", std::process::id());
    let dir = root.join(&tag);
    if dir.is_dir() {
        fs::remove_dir_all(&dir).map_err(|e| format!("stale frame dir failed: {e}"))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("frame dir failed: {e}"))?;
    let pattern = dir.join("f%05d.bin");
    let err_path = root.join(format!("{tag}.stderr"));
    let err_file = fs::File::create(&err_path).map_err(|e| format!("stderr file failed: {e}"))?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-noautoscale",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        "rawvideo",
        "-vsync",
        "0",
        "-f",
        "image2",
    ]);
    cmd.arg(&pattern);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(err_file);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            let _ = fs::remove_file(&err_path);
            return Err(format!("spawn failed: {e}"));
        }
    };
    loop {
        match child.try_wait() {
            Ok(None) => {
                let size = match dir_size(&dir) {
                    Ok(size) => size,
                    Err(why) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = fs::remove_dir_all(&dir);
                        let _ = fs::remove_file(&err_path);
                        return Err(why);
                    }
                };
                if size > SPILL_CAP {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_dir_all(&dir);
                    let _ = fs::remove_file(&err_path);
                    return Err(format!("decode {} spill cap exceeded", path.display()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(Some(status)) => {
                if !status.success() {
                    let err = fs::read_to_string(&err_path).unwrap_or_default();
                    let _ = fs::remove_dir_all(&dir);
                    let _ = fs::remove_file(&err_path);
                    return Err(format!("decode {} failed: {err}", path.display()));
                }
                break;
            }
            Err(e) => {
                let _ = child.kill();
                let _ = fs::remove_dir_all(&dir);
                let _ = fs::remove_file(&err_path);
                return Err(format!("wait failed: {e}"));
            }
        }
    }
    let mut names: Vec<PathBuf> = children(&dir)?
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    let mut hashes = Vec::with_capacity(names.len());
    for name in &names {
        let bytes = fs::read(name).map_err(|e| format!("frame read failed: {e}"))?;
        hashes.push(FrameHash::of(&bytes));
        fs::remove_file(name).map_err(|e| format!("frame cleanup failed: {e}"))?;
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("frame cleanup failed: {e}"))?;
    let _ = fs::remove_file(&err_path);
    if hashes.is_empty() {
        return Err(format!("decode {} yielded no frames", path.display()));
    }
    Ok(hashes)
}

fn pipe_hashes(
    path: &Path,
    pix_fmt: &str,
    slot: usize,
    root: &Path,
    size: usize,
) -> Result<Vec<FrameHash>, String> {
    use std::io::Read as _;
    let tag = format!("rust-video-golden-{}-{slot}", std::process::id());
    let err_path = root.join(format!("{tag}.stderr"));
    let err_file = fs::File::create(&err_path).map_err(|e| format!("stderr file failed: {e}"))?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"]);
    cmd.arg(path);
    cmd.args([
        "-map",
        "0:v:0",
        "-noautoscale",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        "rawvideo",
        "-vsync",
        "0",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(err_file);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = fs::remove_file(&err_path);
            return Err(format!("spawn failed: {e}"));
        }
    };
    let mut hashes = Vec::new();
    let mut frame = vec![0u8; size];
    let mut first = [0u8; 1];
    let failed = match child.stdout.take() {
        Some(stdout) => {
            let mut pipe = std::io::BufReader::new(stdout);
            loop {
                match pipe.read(&mut first) {
                    Ok(0) => break None,
                    Ok(_) => {
                        frame[0] = first[0];
                        match pipe.read_exact(&mut frame[1..]) {
                            Ok(()) => hashes.push(FrameHash::of(&frame)),
                            Err(_) => {
                                break Some(format!(
                                    "decode {} truncated mid-frame",
                                    path.display()
                                ));
                            }
                        }
                    }
                    Err(_) => {
                        break Some(format!("decode {} pipe read failed", path.display()));
                    }
                }
            }
        }
        None => Some(format!("decode {} pipe missing", path.display())),
    };
    let status = child.wait().map_err(|e| format!("wait failed: {e}"))?;
    if let Some(why) = failed {
        let _ = fs::remove_file(&err_path);
        return Err(why);
    }
    if !status.success() {
        let err = fs::read_to_string(&err_path).unwrap_or_default();
        let _ = fs::remove_file(&err_path);
        return Err(format!("decode {} failed: {err}", path.display()));
    }
    let _ = fs::remove_file(&err_path);
    if hashes.is_empty() {
        return Err(format!("decode {} yielded no frames", path.display()));
    }
    Ok(hashes)
}

fn decode_hashes(path: &Path, pix_fmt: &str, slot: usize) -> Result<Vec<FrameHash>, String> {
    let root = frame_root()?;
    if matches!(frame_shapes(path), Shapes::Uniform) {
        match raw_frame_size(path, pix_fmt, slot, &root) {
            Ok(size) => match pipe_hashes(path, pix_fmt, slot, &root, size) {
                Ok(hashes) => return Ok(hashes),
                Err(why) => println!("pipe retry as files {}: {why}", path.display()),
            },
            Err(why) => println!("pipe retry as files {}: {why}", path.display()),
        }
    }
    file_hashes(path, pix_fmt, slot, &root)
}

fn scan_dir(dir: &Path, codec: &str, synth: bool) -> Result<Vec<Vector>, String> {
    let files: Vec<PathBuf> = children(dir)?.into_iter().filter(|p| p.is_file()).collect();
    let mut out = Vec::new();
    for path in files {
        let Some(probe) = keep(&path, probe(&path)) else {
            continue;
        };
        let recipe = synth.then(|| stem(&path));
        out.push(Vector {
            codec_dir: codec.to_owned(),
            path,
            probe,
            synth: recipe,
        });
    }
    Ok(out)
}

fn scan_corpus(root: &Path) -> Result<Vec<Vector>, String> {
    let dirs: Vec<PathBuf> = children(root)?.into_iter().filter(|p| p.is_dir()).collect();
    let mut out = Vec::new();
    for dir in dirs {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "synth" {
            let subs: Vec<PathBuf> = children(&dir)?.into_iter().filter(|p| p.is_dir()).collect();
            for sub in subs {
                let codec = sub.file_name().and_then(|n| n.to_str()).unwrap_or("");
                out.extend(scan_dir(&sub, codec, true)?);
            }
            continue;
        }
        out.extend(scan_dir(&dir, name, false)?);
    }
    Ok(out)
}

fn run_synth(corpus: &Path, synth: &Synth) -> Result<PathBuf, String> {
    let dir = corpus.join("synth").join(synth.codec_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("synth dir failed: {e}"))?;
    let out = dir.join(format!("{}.mp4", synth.name));
    if out.is_file() {
        return Ok(out);
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"]);
    cmd.arg(format!(
        "testsrc2=s={}x{}:d=2:r=30",
        synth.width, synth.height
    ));
    cmd.args(["-vf"]);
    cmd.arg(format!("format={}", synth.pix_fmt));
    cmd.args(["-c:v", synth.encoder]);
    cmd.args(synth.extra);
    cmd.arg(&out);
    let (ok, err, _) = spawn(&mut cmd)?;
    if !ok {
        let _ = fs::remove_file(&out);
        return Err(format!("synth {} failed: {err}", synth.name));
    }
    Ok(out)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("vector")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn generate(goldens: &Path, vectors: &[Vector], ffmpeg: &str) -> Result<usize, String> {
    let fresh = goldens.with_extension("new");
    let old = goldens.with_extension("old");
    if fresh.is_dir() {
        fs::remove_dir_all(&fresh).map_err(|e| format!("stale .new cleanup failed: {e}"))?;
    }
    fs::create_dir_all(&fresh).map_err(|e| format!("fresh dir failed: {e}"))?;
    if goldens.is_dir() {
        for path in children(goldens)? {
            if path.is_file() && path.extension().is_some_and(|ext| ext != "sha256") {
                let name = path.file_name().ok_or("golden keep nameless")?;
                fs::copy(&path, fresh.join(name))
                    .map_err(|e| format!("golden keep failed: {e}"))?;
            }
        }
    }
    let mut made = 0;
    let mut skipped = 0;
    let mut seen: Vec<PathBuf> = Vec::new();
    for (slot, vector) in vectors.iter().enumerate() {
        let Some(hashes) = keep(
            &vector.path,
            decode_hashes(&vector.path, &vector.probe.pix_fmt, slot),
        ) else {
            skipped += 1;
            continue;
        };
        let dir = fresh.join(&vector.codec_dir);
        fs::create_dir_all(&dir).map_err(|e| format!("golden dir failed: {e}"))?;
        let mut target = dir.join(format!("{}.sha256", stem(&vector.path)));
        let mut dup = 1;
        while seen.contains(&target) {
            dup += 1;
            target = dir.join(format!("{}-{dup}.sha256", stem(&vector.path)));
        }
        seen.push(target.clone());
        let mut text = format!(
            "# rust-video golden v1\n# vector: {}\n# ffmpeg: {ffmpeg}\n# probe: {} {} {}x{}\n# frames: {}\n",
            vector.path.display(),
            vector.probe.profile,
            vector.probe.pix_fmt,
            vector.probe.width,
            vector.probe.height,
            hashes.len(),
        );
        if let Some(recipe) = &vector.synth {
            writeln!(text, "# synth: {recipe}").expect("String write");
        }
        for hash in &hashes {
            text.push_str(&hash.hex());
            text.push('\n');
        }
        let tmp = target.with_extension("tmp");
        fs::write(&tmp, text).map_err(|e| format!("golden write failed: {e}"))?;
        fs::rename(&tmp, &target).map_err(|e| format!("golden swap failed: {e}"))?;
        made += 1;
    }
    println!("skipped: {skipped}");
    if made == 0 {
        return Err("floor failed: no goldens made".to_owned());
    }
    if goldens.is_dir() {
        if old.is_dir() {
            fs::remove_dir_all(&old).map_err(|e| format!("stale .old cleanup failed: {e}"))?;
        }
        fs::rename(goldens, &old).map_err(|e| format!("swap to .old failed: {e}"))?;
    }
    fs::rename(&fresh, goldens).map_err(|e| format!("swap failed: {e}"))?;
    if old.is_dir() {
        fs::remove_dir_all(&old).map_err(|e| format!("old cleanup failed: {e}"))?;
    }
    Ok(made)
}

fn coverage<'a>(
    vectors: &'a [Vector],
    reqs: &'a [Req],
) -> (Vec<(&'a Req, &'a Vector)>, Vec<&'a Req>) {
    let mut covered = Vec::new();
    let mut missing = Vec::new();
    for req in reqs {
        if req.blocked.is_some() {
            continue;
        }
        match vectors
            .iter()
            .find(|v| v.codec_dir == req.codec && (req.matches)(v))
        {
            Some(hit) => covered.push((req, hit)),
            None => missing.push(req),
        }
    }
    (covered, missing)
}

fn print_matrix() {
    println!("codec/option -> synth|fetch|blocked");
    for req in reqs() {
        let via = match (req.blocked, req.synth) {
            (Some(why), _) => why,
            (None, Some(name)) => name,
            (None, None) => "fetch",
        };
        println!("{}/{} -> {via}", req.codec, req.name);
    }
}

fn usage() -> ! {
    eprintln!("usage: golden-gen [--corpus DIR] [--goldens DIR] [--synth] [--list-required]");
    std::process::exit(2);
}

fn main() {
    if let Err(why) = run() {
        eprintln!("golden-gen: {why}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut corpus = PathBuf::from("corpus");
    let mut goldens = PathBuf::from("crates/rust-video-harness/goldens");
    let mut synth = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--corpus" => {
                corpus = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("missing --corpus DIR")?;
            }
            "--goldens" => {
                goldens = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("missing --goldens DIR")?;
            }
            "--synth" => synth = true,
            "--list-required" => {
                print_matrix();
                return Ok(());
            }
            _ => usage(),
        }
    }
    if !corpus.is_dir() {
        return Err(format!(
            "corpus {} absent: run just vectors first",
            corpus.display()
        ));
    }
    let ffmpeg = ffmpeg_version()?;
    if synth {
        let recipes = synths();
        let wanted = reqs();
        let vectors = scan_corpus(&corpus)?;
        let (_, missing) = coverage(&vectors, &wanted);
        let mut ran = Vec::new();
        for req in missing {
            let Some(name) = req.synth else { continue };
            if ran.contains(&name) {
                continue;
            }
            let Some(recipe) = recipes.get(name) else {
                println!("synth recipe missing for {name}");
                continue;
            };
            match run_synth(&corpus, recipe) {
                Ok(_) => ran.push(name),
                Err(why) => println!("synth skipped: {why}"),
            }
        }
        if !ran.is_empty() {
            println!("synthesized: {}", ran.join(", "));
        }
    }
    let vectors = scan_corpus(&corpus)?;
    let made = generate(&goldens, &vectors, &ffmpeg)?;
    println!("goldens: {made}");
    let wanted = reqs();
    let (covered, missing) = coverage(&vectors, &wanted);
    for (req, hit) in &covered {
        println!("cover {}/{} <- {}", req.codec, req.name, hit.path.display());
    }
    for req in reqs().iter().filter(|r| r.blocked.is_some()) {
        println!(
            "BLOCKED {}/{}: {}",
            req.codec,
            req.name,
            req.blocked.unwrap_or("?")
        );
    }
    if missing.is_empty() {
        return Ok(());
    }
    let mut msg = String::from("uncovered options:\n");
    for req in missing {
        let fix = req.synth.map_or_else(
            || format!("fetch a {} vector exercising {}", req.codec, req.name),
            |s| format!("run with --synth ({s}) or fetch"),
        );
        writeln!(msg, "- {}/{}: {fix}", req.codec, req.name).expect("String write");
    }
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::{frame_root_from, parse_shapes, Shapes};
    use std::path::PathBuf;

    #[test]
    fn root_prefers_tmpdir() {
        assert_eq!(
            frame_root_from(Some("/scratch/tmp".as_ref())),
            PathBuf::from("/scratch/tmp")
        );
    }

    #[test]
    fn root_falls_back_to_repo_tmp() {
        assert_eq!(frame_root_from(None), PathBuf::from(".tmp"));
        assert_eq!(frame_root_from(Some("".as_ref())), PathBuf::from(".tmp"));
    }

    #[test]
    fn shapes_classifies_uniformity() {
        assert!(matches!(
            parse_shapes("352,288,yuv420p\n352,288,yuv420p\n"),
            Shapes::Uniform
        ));
        assert!(matches!(
            parse_shapes("352,288,yuv420p\n176,144,yuv420p\n"),
            Shapes::Varied
        ));
        assert!(matches!(parse_shapes(""), Shapes::Unknown));
        assert!(matches!(parse_shapes("352,288\n"), Shapes::Unknown));
    }
}
