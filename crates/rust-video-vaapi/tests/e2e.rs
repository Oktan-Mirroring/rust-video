//! VA-API end-to-end: synth vectors in, hashed frames out.

#![cfg(all(target_os = "linux", have_corpus))]

use std::path::{Path, PathBuf};
use std::process::Command;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuExport, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder};
use rust_video_caps::transcode::Route;
use rust_video_harness::{FrameHash, Stream, Synth};
use rust_video_vaapi::{
    EncodeCodec, GopShape, H264Decoder, H265Decoder, Transcoder, VaApiDevice, VaApiEncoder,
    VaApiError, VaApiFrame, Vp9Decoder,
};

/// Corpus root: `$RUST_VIDEO_CORPUS` or the checkout's `corpus/`.
fn corpus() -> PathBuf {
    std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus")
                .canonicalize()
                .expect("workspace corpus dir")
        },
        PathBuf::from,
    )
}

/// Scratch dir for one ffmpeg oracle run, removed after it.
fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.tmp")
        .join(format!("vaapi-e2e-{}-{name}", std::process::id()))
}

/// NAL units of Annex B `bytes`, start codes stripped.
fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut start = None;
    let mut cursor = 0;
    while cursor + 3 <= bytes.len() {
        let long = bytes.get(cursor..cursor + 4) == Some(&[0, 0, 0, 1][..]);
        let short = !long && bytes.get(cursor..cursor + 3) == Some(&[0, 0, 1][..]);
        if long || short {
            if let Some(begin) = start.take() {
                units.push(&bytes[begin..cursor]);
            }
            cursor += if long { 4 } else { 3 };
            start = Some(cursor);
        } else {
            cursor += 1;
        }
    }
    if let Some(begin) = start {
        units.push(&bytes[begin..]);
    }
    units
}

/// Demuxed samples as one Annex B blob.
fn annex_b(stream: &Stream) -> Vec<u8> {
    let mut out = Vec::new();
    for sample in &stream.samples {
        out.extend_from_slice(sample);
    }
    out
}

/// ffmpeg remux of `path` to Annex B `ext`, or `None` without ffmpeg.
fn ffmpeg_remux(path: &Path, scratch: &Path, ext: &str) -> Option<Vec<u8>> {
    std::fs::create_dir_all(scratch).ok()?;
    let out = scratch.join(format!("remux.{ext}"));
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-c:v")
        .arg("copy")
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&out).ok()?;
    std::fs::remove_dir_all(scratch).ok();
    Some(bytes)
}

/// ffmpeg software decode of `path` as planar hashes, or `None` without ffmpeg.
fn ffmpeg_hashes(
    path: &Path,
    scratch: &Path,
    name: &str,
    width: u32,
    height: u32,
) -> Option<Vec<String>> {
    ffmpeg_hashes_capped(path, scratch, name, width, height, None)
}

/// ffmpeg software decode of `path` as planar hashes, first `limit` shown
/// frames when set, or `None` without ffmpeg.
fn ffmpeg_hashes_capped(
    path: &Path,
    scratch: &Path,
    name: &str,
    width: u32,
    height: u32,
    limit: Option<usize>,
) -> Option<Vec<String>> {
    std::fs::create_dir_all(scratch).ok()?;
    let out = scratch.join(format!("{name}.yuv"));
    let mut command = Command::new("ffmpeg");
    command
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:v:0");
    if let Some(limit) = limit {
        command.arg("-frames:v").arg(limit.to_string());
    }
    let status = command
        .arg("-vsync")
        .arg("0")
        .arg("-c:v")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-f")
        .arg("rawvideo")
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&out).ok()?;
    std::fs::remove_dir_all(scratch).ok();
    let stride = width as usize * height as usize;
    if stride == 0 || bytes.len() % (stride * 3 / 2) != 0 {
        return None;
    }
    let frame = stride * 3 / 2;
    Some(
        bytes
            .chunks_exact(frame)
            .map(|plane| FrameHash::of(plane).hex())
            .collect(),
    )
}

/// ffmpeg remux of `path` to an IVF file under `scratch`, or `None` without ffmpeg.
fn ffmpeg_remux_ivf(path: &Path, scratch: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(scratch).ok()?;
    let out = scratch.join("remux.ivf");
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-c:v")
        .arg("copy")
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    Some(out)
}

/// IVF container bytes as one packet per frame.
///
/// # Panics
///
/// Panics when the magic is absent or a frame overruns the buffer.
fn ivf_packets(ivf: &[u8]) -> Vec<Packet> {
    assert!(ivf.len() >= 32 && &ivf[0..4] == b"DKIF", "ivf magic");
    let header = u16::from_le_bytes([ivf[6], ivf[7]]) as usize;
    let mut packets = Vec::new();
    let mut offset = header;
    while offset + 12 <= ivf.len() {
        let size =
            u32::from_le_bytes(ivf[offset..offset + 4].try_into().expect("ivf size")) as usize;
        offset += 12;
        packets.push(Packet::new(Bytes::copy_from_slice(
            &ivf[offset..offset + size],
        )));
        offset += size;
    }
    packets
}

/// NV12 frame bytes as planar YUV420P.
fn to_planar(nv12: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_len = width * height;
    let (y, uv) = nv12.split_at(y_len);
    let mut planar = Vec::with_capacity(nv12.len());
    planar.extend_from_slice(y);
    planar.extend(uv.iter().step_by(2).copied());
    planar.extend(uv.iter().skip(1).step_by(2).copied());
    planar
}

/// Decode one packet per sample.
fn decode_stream(
    decoder: &mut impl Decoder<Frame = VaApiFrame, Error = VaApiError>,
    stream: &Stream,
) -> Result<Vec<VaApiFrame>, VaApiError> {
    let mut frames = Vec::new();
    for sample in &stream.samples {
        frames.extend(decoder.decode(&Packet::new(Bytes::copy_from_slice(sample)))?);
    }
    frames.extend(decoder.flush()?);
    Ok(frames)
}

/// True when `RUST_VIDEO_ALLOW_SKIP=1` permits skipping.
fn allow_skip() -> bool {
    std::env::var("RUST_VIDEO_ALLOW_SKIP").is_ok_and(|value| value == "1")
}

/// Skip over missing `what`.
///
/// # Panics
///
/// Panics unless `RUST_VIDEO_ALLOW_SKIP=1` is set.
fn skip(what: &str) {
    if allow_skip() {
        println!("SKIP missing {what}");
    } else {
        panic!("missing {what}; rerun with RUST_VIDEO_ALLOW_SKIP=1 to skip");
    }
}

/// Device, or `None` when the tier declines the machine and skips are allowed.
fn device() -> Option<VaApiDevice> {
    match VaApiDevice::open() {
        Ok(device) => {
            eprintln!("va-api device: {}", device.name());
            Some(device)
        }
        Err(VaApiError::Skipped(reason)) => {
            skip(&format!("va-api tier ({reason})"));
            None
        }
        Err(error) => panic!("unexpected va-api failure: {error}"),
    }
}

#[test]
fn demux_matches_ffmpeg_remux() {
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    assert_eq!(stream.samples.len(), 60);
    let first = nal_units(&stream.samples[0]);
    assert!(first.iter().any(|nal| nal.first() == Some(&0x67)));
    let Some(remux) = ffmpeg_remux(&path, &scratch("remux264"), "h264") else {
        skip("ffmpeg remux oracle (install ffmpeg)");
        return;
    };
    let blob = annex_b(&stream);
    let ours = nal_units(&blob);
    let theirs = nal_units(&remux);
    assert_eq!(ours.len(), theirs.len(), "nal unit count");
    for (index, (left, right)) in ours.iter().zip(theirs.iter()).enumerate() {
        assert_eq!(left, right, "nal unit {index}");
    }
}

#[test]
fn vaapi_decodes_synth_high() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_stream(&mut decoder, &stream).expect("va-api decode");
    assert_eq!(frames.len(), stream.samples.len());
    let mut hashes = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        let size = frame.identity().size();
        assert_eq!((size.width(), size.height()), (1920, 1080), "frame {index}");
        assert_eq!(frame.identity().format(), PixelFormat::Nv12);
        let nv12 = frame.nv12();
        assert_eq!(nv12.len(), 1920 * 1080 * 3 / 2, "frame {index}");
        hashes.push(FrameHash::of(&to_planar(nv12, 1920, 1080)).hex());
    }
    let Some(expected) = ffmpeg_hashes(&path, &scratch("decode264"), "oracle", 1920, 1080) else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    assert_eq!(hashes.len(), expected.len(), "frame count");
    for (index, (left, right)) in hashes.iter().zip(expected.iter()).enumerate() {
        assert_eq!(left, right, "frame {index}");
    }
}

#[test]
fn vaapi_declines_high_chroma() {
    let Some(device) = device() else {
        return;
    };
    for name in ["h264-high422.mp4", "h264-high444.mp4"] {
        let path = corpus().join("synth/h264").join(name);
        let stream = Stream::demux(&path).expect("demux chroma vector");
        let mut decoder = H264Decoder::open(&device);
        match decode_stream(&mut decoder, &stream) {
            Err(VaApiError::Unsupported(_)) => {}
            Err(error) => panic!("{name}: expected unsupported chroma, got {error}"),
            Ok(_) => panic!("{name}: expected unsupported chroma, decoded"),
        }
    }
}

#[test]
fn demux_matches_ffmpeg_remux_h265() {
    let path = corpus().join("synth/h265/h265-main.mp4");
    let stream = Stream::demux(&path).expect("demux synth main");
    assert_eq!(stream.samples.len(), 60);
    let first = nal_units(&stream.samples[0]);
    assert!(first.iter().any(|nal| nal.first() == Some(&0x40)));
    let Some(remux) = ffmpeg_remux(&path, &scratch("remux265"), "h265") else {
        skip("ffmpeg remux oracle (install ffmpeg)");
        return;
    };
    let blob = annex_b(&stream);
    let ours = nal_units(&blob);
    let theirs = nal_units(&remux);
    assert_eq!(ours.len(), theirs.len(), "nal unit count");
    for (index, (left, right)) in ours.iter().zip(theirs.iter()).enumerate() {
        assert_eq!(left, right, "nal unit {index}");
    }
}

#[test]
fn vaapi_decodes_synth_main() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h265/h265-main.mp4");
    let stream = Stream::demux(&path).expect("demux synth main");
    assert_ne!(stream.samples.len(), 0);
    let mut decoder = H265Decoder::open(&device);
    let frames = decode_stream(&mut decoder, &stream).expect("va-api decode");
    assert_eq!(frames.len(), stream.samples.len());
    for frame in &frames {
        assert_eq!(frame.identity().format(), PixelFormat::Nv12);
    }
}

#[test]
fn vaapi_exports_dmabuf() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut decoder = H264Decoder::open(&device);
    let mut exported = 0;
    for sample in stream.samples.iter().take(4) {
        let frames = decoder
            .decode_surface(&Packet::new(Bytes::copy_from_slice(sample)))
            .expect("va-api decode");
        for (index, frame) in frames.iter().enumerate() {
            assert!(
                matches!(frame.export(), GpuExport::DmaBuf { .. }),
                "frame {index} exports dmabuf"
            );
            exported += 1;
        }
    }
    let frames = decoder.flush_surface().expect("flush");
    for (index, frame) in frames.iter().enumerate() {
        assert!(
            matches!(frame.export(), GpuExport::DmaBuf { .. }),
            "flushed frame {index} exports dmabuf"
        );
        exported += 1;
    }
    assert!(exported > 0);
}

/// First NAL header byte of Annex B `bytes`.
fn first_nal_header(bytes: &[u8]) -> u8 {
    let units = nal_units(bytes);
    units
        .first()
        .and_then(|nal| nal.first())
        .copied()
        .expect("one nal")
}

#[test]
fn vaapi_encodes_h264_all_intra() {
    let Some(device) = device() else {
        return;
    };
    let size = PictureSize::new(320, 240).expect("synth size");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H264, size) {
        Ok(encoder) => encoder,
        Err(VaApiError::Unsupported(message)) => {
            skip(&format!("h264 encode entrypoint ({message})"));
            return;
        }
        Err(error) => panic!("unexpected encode failure: {error}"),
    };
    let mut annex_b = Vec::new();
    let mut coded = 0;
    for index in 0..8 {
        let frame = Synth::gradient(size, index);
        for packet in encoder.encode(&frame).expect("encode") {
            assert!(packet.is_keyframe(), "all-intra keyframe");
            annex_b.extend_from_slice(packet.data());
            coded += 1;
        }
    }
    for packet in encoder.flush().expect("flush") {
        annex_b.extend_from_slice(packet.data());
        coded += 1;
    }
    assert_eq!(coded, 8);
    assert_eq!(first_nal_header(&annex_b) & 0x1f, 7, "sps first");
    let mut decoder = H264Decoder::open(&device);
    let mut frames = decoder
        .decode(&Packet::new(Bytes::from(annex_b.clone())))
        .expect("self decode");
    frames.extend(decoder.flush().expect("self flush"));
    assert_eq!(frames.len(), coded);
    let mut hashes = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.identity().size(), size, "frame {index}");
        hashes.push(FrameHash::of(&to_planar(frame.nv12(), 320, 240)).hex());
    }
    let dir = scratch("encode264");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let coded_path = dir.join("coded.h264");
    std::fs::write(&coded_path, &annex_b).expect("coded stream");
    let Some(expected) = ffmpeg_hashes(&coded_path, &dir, "oracle", 320, 240) else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(hashes.len(), expected.len(), "frame count");
    for (index, (left, right)) in hashes.iter().zip(expected.iter()).enumerate() {
        assert_eq!(left, right, "frame {index}");
    }
}

#[test]
fn vaapi_encodes_h265_keyframes() {
    let Some(device) = device() else {
        return;
    };
    let size = PictureSize::new(320, 240).expect("synth size");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H265, size) {
        Ok(encoder) => encoder,
        Err(VaApiError::Unsupported(message)) => {
            skip(&format!("h265 encode entrypoint ({message})"));
            return;
        }
        Err(error) => panic!("unexpected encode failure: {error}"),
    };
    let mut annex_b = Vec::new();
    let mut coded = 0;
    for index in 0..4 {
        let frame = Synth::gradient(size, index);
        for packet in encoder.encode(&frame).expect("encode") {
            assert!(packet.is_keyframe(), "all-intra keyframe");
            assert_ne!(packet.data(), []);
            annex_b.extend_from_slice(packet.data());
            coded += 1;
        }
    }
    for packet in encoder.flush().expect("flush") {
        annex_b.extend_from_slice(packet.data());
        coded += 1;
    }
    assert_eq!(coded, 4);
    assert_eq!((first_nal_header(&annex_b) >> 1) & 0x3f, 32, "vps first");
    let dir = scratch("encode265");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let coded_path = dir.join("coded.h265");
    std::fs::write(&coded_path, &annex_b).expect("coded stream");
    let Some(expected) = ffmpeg_hashes(&coded_path, &dir, "oracle", 320, 240) else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(expected.len(), coded, "ffmpeg frame count");
}

/// Encode `frames` gradient pictures, returning packets and Annex B bytes.
fn encode_low_delay(
    encoder: &mut VaApiEncoder,
    size: PictureSize,
    frames: u32,
) -> (Vec<Packet>, Vec<u8>) {
    let mut packets = Vec::new();
    let mut annex_b = Vec::new();
    for index in 0..frames {
        let frame = Synth::gradient(size, index);
        for packet in encoder.encode(&frame).expect("encode") {
            annex_b.extend_from_slice(packet.data());
            packets.push(packet);
        }
    }
    for packet in encoder.flush().expect("flush") {
        annex_b.extend_from_slice(packet.data());
        packets.push(packet);
    }
    (packets, annex_b)
}

/// Keyframe indexes in `packets`.
fn keyframes(packets: &[Packet]) -> Vec<usize> {
    packets
        .iter()
        .enumerate()
        .filter(|(_, packet)| packet.is_keyframe())
        .map(|(index, _)| index)
        .collect()
}

/// Per-kind VCL unit counts in Annex B `bytes`.
fn count_vcl(bytes: &[u8], kind_of: fn(u8) -> u8, kinds: &[u8]) -> Vec<usize> {
    let mut counts = vec![0; kinds.len()];
    for nal in nal_units(bytes) {
        let Some(header) = nal.first() else { continue };
        let kind = kind_of(*header);
        if let Some(slot) = kinds.iter().position(|want| *want == kind) {
            counts[slot] += 1;
        }
    }
    counts
}

/// H.264 NAL unit type of a one-byte header.
fn h264_kind(header: u8) -> u8 {
    header & 0x1f
}

/// H.265 NAL unit type of a two-byte header's first byte.
fn h265_kind(header: u8) -> u8 {
    (header >> 1) & 0x3f
}

/// Decode `packets` one by one.
fn decode_packets(
    decoder: &mut impl Decoder<Frame = VaApiFrame, Error = VaApiError>,
    packets: &[Packet],
) -> Vec<VaApiFrame> {
    let mut frames = Vec::new();
    for packet in packets {
        frames.extend(decoder.decode(packet).expect("self decode"));
    }
    frames.extend(decoder.flush().expect("self flush"));
    frames
}

#[test]
fn vaapi_encodes_h264_low_delay_gop60() {
    let Some(device) = device() else {
        return;
    };
    let size = PictureSize::new(320, 240).expect("synth size");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H264, size) {
        Ok(encoder) => encoder.with_gop(GopShape::LowDelay { intra_period: 60 }),
        Err(VaApiError::Unsupported(message)) => {
            skip(&format!("h264 encode entrypoint ({message})"));
            return;
        }
        Err(error) => panic!("unexpected encode failure: {error}"),
    };
    let (packets, annex_b) = encode_low_delay(&mut encoder, size, 65);
    assert_eq!(packets.len(), 65);
    assert_eq!(keyframes(&packets), vec![0, 60]);
    assert_eq!(count_vcl(&annex_b, h264_kind, &[5, 1]), vec![2, 63]);
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_packets(&mut decoder, &packets);
    assert_eq!(frames.len(), 65);
    let hashes: Vec<String> = frames
        .iter()
        .map(|frame| FrameHash::of(&to_planar(frame.nv12(), 320, 240)).hex())
        .collect();
    let dir = scratch("ippp264");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let coded_path = dir.join("coded.h264");
    std::fs::write(&coded_path, &annex_b).expect("coded stream");
    let Some(expected) = ffmpeg_hashes(&coded_path, &dir, "oracle", 320, 240) else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(hashes.len(), expected.len(), "frame count");
    for (index, (left, right)) in hashes.iter().zip(expected.iter()).enumerate() {
        assert_eq!(left, right, "frame {index}");
    }
}

#[test]
fn vaapi_encodes_h265_low_delay_gop60() {
    let Some(device) = device() else {
        return;
    };
    let size = PictureSize::new(320, 240).expect("synth size");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H265, size) {
        Ok(encoder) => encoder.with_gop(GopShape::LowDelay { intra_period: 60 }),
        Err(VaApiError::Unsupported(message)) => {
            skip(&format!("h265 encode entrypoint ({message})"));
            return;
        }
        Err(error) => panic!("unexpected encode failure: {error}"),
    };
    let (packets, annex_b) = encode_low_delay(&mut encoder, size, 65);
    assert_eq!(packets.len(), 65);
    assert_eq!(keyframes(&packets), vec![0, 60]);
    assert_eq!(count_vcl(&annex_b, h265_kind, &[19, 1]), vec![2, 63]);
    let mut decoder = H265Decoder::open(&device);
    let frames = decode_packets(&mut decoder, &packets);
    assert_eq!(frames.len(), 65);
    let hashes: Vec<String> = frames
        .iter()
        .map(|frame| FrameHash::of(&to_planar(frame.nv12(), 320, 240)).hex())
        .collect();
    let dir = scratch("ippp265");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let coded_path = dir.join("coded.h265");
    std::fs::write(&coded_path, &annex_b).expect("coded stream");
    let Some(expected) = ffmpeg_hashes(&coded_path, &dir, "oracle", 320, 240) else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(hashes.len(), expected.len(), "frame count");
    for (index, (left, right)) in hashes.iter().zip(expected.iter()).enumerate() {
        assert_eq!(left, right, "frame {index}");
    }
}

#[test]
fn vaapi_low_delay_beats_all_intra_bitrate() {
    let Some(device) = device() else {
        return;
    };
    let size = PictureSize::new(320, 240).expect("synth size");
    for codec in [EncodeCodec::H264, EncodeCodec::H265] {
        let open = |gop| match VaApiEncoder::open(&device, codec, size) {
            Ok(encoder) => Some(encoder.with_gop(gop)),
            Err(VaApiError::Unsupported(message)) => {
                skip(&format!("{codec} encode entrypoint ({message})"));
                None
            }
            Err(error) => panic!("unexpected encode failure: {error}"),
        };
        let Some(mut intra) = open(GopShape::AllIntra) else {
            continue;
        };
        let Some(mut low_delay) = open(GopShape::LowDelay { intra_period: 60 }) else {
            continue;
        };
        let (intra_packets, _) = encode_low_delay(&mut intra, size, 60);
        let (low_packets, _) = encode_low_delay(&mut low_delay, size, 60);
        assert_eq!(keyframes(&intra_packets).len(), 60);
        assert_eq!(keyframes(&low_packets), vec![0]);
        let intra_bytes: usize = intra_packets.iter().map(|packet| packet.data().len()).sum();
        let low_bytes: usize = low_packets.iter().map(|packet| packet.data().len()).sum();
        let mbps = |bytes: usize| {
            let bytes = u32::try_from(bytes).expect("small stream");
            f64::from(bytes) * 8.0 * 30.0 / 60.0 / 1e6
        };
        let intra_mbps = mbps(intra_bytes);
        let low_mbps = mbps(low_bytes);
        eprintln!(
            "{codec} 60f qp26: all-intra {intra_bytes}B ({intra_mbps:.3}Mbps), ippp {low_bytes}B ({low_mbps:.3}Mbps)",
        );
        assert!(
            low_bytes < intra_bytes,
            "{codec}: ippp {low_bytes}B should beat all-intra {intra_bytes}B"
        );
    }
}

#[test]
fn vaapi_pipelines_submit_depth() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut decoder = H264Decoder::open(&device);
    let mut frames = Vec::new();
    for sample in stream.samples.iter().take(9) {
        frames.extend(
            decoder
                .decode_surface(&Packet::new(Bytes::copy_from_slice(sample)))
                .expect("va-api decode"),
        );
    }
    let stats = decoder.submit_stats().expect("backend open");
    eprintln!(
        "submit stats: submitted={} peak={} resets={} creates={}",
        stats.submitted(),
        stats.peak(),
        stats.resets(),
        stats.buffer_creates(),
    );
    assert_eq!(stats.submitted(), 8);
    assert!(stats.peak() > 1, "pipelined peak {}", stats.peak());
    assert_eq!(stats.resets(), 0);
    for (index, frame) in frames.iter().enumerate() {
        assert!(
            matches!(frame.export(), GpuExport::DmaBuf { .. }),
            "frame {index} exports dmabuf"
        );
    }
    let shown = frames.len();
    drop(frames);
    let tail = decoder.flush_surface().expect("flush");
    for (index, frame) in tail.iter().enumerate() {
        assert!(
            matches!(frame.export(), GpuExport::DmaBuf { .. }),
            "flushed frame {index} exports dmabuf"
        );
    }
    assert_eq!(shown + tail.len(), 9);
}

#[test]
fn vaapi_reuses_decode_buffers() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_stream(&mut decoder, &stream).expect("va-api decode");
    assert_eq!(frames.len(), stream.samples.len());
    let stats = decoder.submit_stats().expect("backend open");
    eprintln!(
        "decode reuse: submitted={} creates={}",
        stats.submitted(),
        stats.buffer_creates(),
    );
    assert_eq!(stats.submitted(), stream.samples.len() as u64);
    assert!(
        stats.buffer_creates() < stats.submitted(),
        "reused buffers, not per-picture: {} creates for {} pictures",
        stats.buffer_creates(),
        stats.submitted(),
    );
}

#[test]
fn vaapi_reuses_encode_buffers() {
    let Some(device) = device() else {
        return;
    };
    let size = PictureSize::new(320, 240).expect("synth size");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H264, size) {
        Ok(encoder) => encoder,
        Err(VaApiError::Unsupported(message)) => {
            skip(&format!("h264 encode entrypoint ({message})"));
            return;
        }
        Err(error) => panic!("unexpected encode failure: {error}"),
    };
    let mut coded = 0;
    for index in 0..8 {
        let frame = Synth::gradient(size, index);
        coded += encoder.encode(&frame).expect("encode").len();
    }
    coded += encoder.flush().expect("flush").len();
    assert_eq!(coded, 8);
    eprintln!("encode reuse: creates={}", encoder.buffer_creates());
    assert!(
        encoder.buffer_creates() < 8 * 4,
        "reused buffers, not per-picture: {} creates for 8 pictures",
        encoder.buffer_creates(),
    );
}

#[test]
fn vaapi_recovers_after_bad_packet() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut decoder = H264Decoder::open(&device);
    let mut frames = Vec::new();
    for sample in stream.samples.iter().take(2) {
        frames.extend(
            decoder
                .decode(&Packet::new(Bytes::copy_from_slice(sample)))
                .expect("va-api decode"),
        );
    }
    let bad = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x67, 0x42, 0x00]));
    assert!(
        decoder.decode(&bad).is_err(),
        "truncated sequence set fails"
    );
    for sample in &stream.samples[2..] {
        frames.extend(
            decoder
                .decode(&Packet::new(Bytes::copy_from_slice(sample)))
                .expect("va-api decode after bad packet"),
        );
    }
    frames.extend(decoder.flush().expect("flush after bad packet"));
    assert_eq!(frames.len(), stream.samples.len());
}

#[test]
fn vaapi_transcodes_scaled_on_device() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = Transcoder::open(&device, EncodeCodec::H264)
        .with_target(PictureSize::new(640, 360).expect("scale target"));
    if !transcoder.caps().ready() {
        skip("transcode export+encode caps");
        return;
    }
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut coded = 0;
    let mut annex_b = Vec::new();
    for sample in &stream.samples {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::copy_from_slice(sample)))
            .expect("transcode");
        assert_eq!(outcome.plan().route(), Route::Gpu);
        for packet in outcome.packets() {
            assert!(packet.is_keyframe(), "all-intra keyframe");
            annex_b.extend_from_slice(packet.data());
            coded += 1;
        }
    }
    let outcome = transcoder.flush().expect("flush");
    for packet in outcome.packets() {
        annex_b.extend_from_slice(packet.data());
        coded += 1;
    }
    assert_eq!(coded, stream.samples.len());
    let mut decoder = H264Decoder::open(&device);
    let mut frames = decoder
        .decode(&Packet::new(Bytes::from(annex_b)))
        .expect("self decode");
    frames.extend(decoder.flush().expect("self flush"));
    assert_eq!(frames.len(), coded);
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.identity().size(),
            PictureSize::new(640, 360).expect("scale target"),
            "frame {index}"
        );
    }
}

#[test]
fn vaapi_transcodes_on_device() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = Transcoder::open(&device, EncodeCodec::H264);
    if !transcoder.caps().ready() {
        skip("transcode export+encode caps");
        return;
    }
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut coded = 0;
    for sample in &stream.samples {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::copy_from_slice(sample)))
            .expect("transcode");
        assert_eq!(outcome.plan().route(), Route::Gpu);
        for packet in outcome.packets() {
            assert!(packet.is_keyframe(), "all-intra keyframe");
            coded += 1;
        }
    }
    let outcome = transcoder.flush().expect("flush");
    coded += outcome.packets().len();
    assert_eq!(coded, stream.samples.len());
}

#[test]
fn vaapi_transcodes_low_delay_gop60() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = Transcoder::open(&device, EncodeCodec::H264)
        .with_gop(GopShape::LowDelay { intra_period: 60 });
    if !transcoder.caps().ready() {
        skip("transcode export+encode caps");
        return;
    }
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = Stream::demux(&path).expect("demux synth high");
    let mut packets = Vec::new();
    let mut annex_b = Vec::new();
    for sample in &stream.samples {
        let outcome = transcoder
            .transcode(&Packet::new(Bytes::copy_from_slice(sample)))
            .expect("transcode");
        assert_eq!(outcome.plan().route(), Route::Gpu);
        for packet in outcome.packets() {
            annex_b.extend_from_slice(packet.data());
            packets.push(packet.clone());
        }
    }
    let outcome = transcoder.flush().expect("flush");
    for packet in outcome.packets() {
        annex_b.extend_from_slice(packet.data());
        packets.push(packet.clone());
    }
    assert_eq!(packets.len(), stream.samples.len());
    assert_eq!(keyframes(&packets), vec![0]);
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_packets(&mut decoder, &packets);
    assert_eq!(frames.len(), packets.len());
}

#[test]
fn vaapi_decodes_vp9_bbb_600() {
    const FRAMES: usize = 600;
    const WIDTH: u32 = 426;
    const HEIGHT: u32 = 240;
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("vp9/vp90-2-bbb_426x240_tile_1x1_180kbps.webm");
    let dir = scratch("decodevp9");
    let Some(ivf_path) = ffmpeg_remux_ivf(&path, &dir) else {
        skip("ffmpeg remux oracle (install ffmpeg)");
        return;
    };
    let ivf = std::fs::read(&ivf_path).expect("ivf bytes");
    let packets = ivf_packets(&ivf);
    assert!(packets.len() > FRAMES, "stream holds 600 frames");
    let mut decoder = Vp9Decoder::open(&device);
    let mut frames = Vec::new();
    for packet in &packets {
        frames.extend(decoder.decode(packet).expect("va-api decode"));
        if frames.len() >= FRAMES {
            break;
        }
    }
    assert!(frames.len() >= FRAMES, "shown frame count");
    frames.truncate(FRAMES);
    let mut hashes = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        let size = frame.identity().size();
        assert_eq!(
            (size.width(), size.height()),
            (WIDTH, HEIGHT),
            "frame {index}"
        );
        assert_eq!(frame.identity().format(), PixelFormat::Nv12);
        let nv12 = frame.nv12();
        assert_eq!(
            nv12.len(),
            WIDTH as usize * HEIGHT as usize * 3 / 2,
            "frame {index}"
        );
        hashes.push(FrameHash::of(&to_planar(nv12, WIDTH as usize, HEIGHT as usize)).hex());
    }
    let Some(expected) = ffmpeg_hashes_capped(&path, &dir, "oracle", WIDTH, HEIGHT, Some(FRAMES))
    else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(hashes.len(), expected.len(), "frame count");
    for (index, (left, right)) in hashes.iter().zip(expected.iter()).enumerate() {
        assert_eq!(left, right, "frame {index}");
    }
}
