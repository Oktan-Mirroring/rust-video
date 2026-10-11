//! Production vpx decoder and encoder probes.

use std::fs;
use std::path::Path;
use std::time::Instant;

use bytes::Bytes;
use rust_video::frame::{CpuFrame, PixelFormat, Plane};
use rust_video::packet::Packet;
use rust_video::traits::{Decoder, Encoder};
use rust_video_caps::common::PictureSize;
use rust_video_sw::{Vp8Decoder, Vp9Decoder, Vp9Encoder};

use crate::common::read_ivf;

/// Codec selector.
#[derive(Clone, Copy)]
pub enum Codec {
    /// VP8 decode.
    Vp8,
    /// VP9 decode or encode.
    Vp9,
}

/// Decode IVF `stream` on `threads`, at most `max` frames, packed raw to `dump`.
pub fn run_decode(codec: Codec, stream: &Path, max: usize, threads: u32, dump: Option<&Path>) {
    let ivf = read_ivf(stream);
    println!("vpxdec: {} frames in stream", ivf.frames.len());
    let mut packed_all = Vec::new();
    let mut frames = 0usize;
    let start = Instant::now();
    match codec {
        Codec::Vp8 => {
            let mut decoder = Vp8Decoder::open(threads).expect("decoder");
            run_loop(&mut decoder, &ivf.frames, max, &mut packed_all, &mut frames);
        }
        Codec::Vp9 => {
            let mut decoder = Vp9Decoder::open(threads).expect("decoder");
            run_loop(&mut decoder, &ivf.frames, max, &mut packed_all, &mut frames);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "vpxdec: {frames} frames in {secs:.2}s = {:.1} fps",
        crate::common::fps(frames, secs)
    );
    if let Some(path) = dump {
        fs::write(path, &packed_all).expect("dump written");
        println!("vpxdec: {} bytes dumped", packed_all.len());
    }
}

/// Packets through `decoder`, packed planes appended to `dump`.
fn run_loop(
    decoder: &mut impl Decoder<Frame = CpuFrame, Error = rust_video_sw::SwError>,
    frames_in: &[Vec<u8>],
    max: usize,
    dump: &mut Vec<u8>,
    count: &mut usize,
) {
    for bytes_in in frames_in {
        if *count >= max {
            break;
        }
        let packet = Packet::new(Bytes::from(bytes_in.clone()));
        for out in decoder.decode(&packet).expect("decode") {
            for plane in out.planes() {
                dump.extend_from_slice(plane.data());
            }
            *count += 1;
        }
    }
    for out in decoder.flush().expect("flush") {
        if *count >= max {
            break;
        }
        for plane in out.planes() {
            dump.extend_from_slice(plane.data());
        }
        *count += 1;
    }
}

/// Encode `frames` I420 frames from `raw` at `w`x`h` into IVF `out` as VP9.
pub fn run_encode(raw: &Path, w: u32, h: u32, frames: usize, out: &Path) {
    let bytes = fs::read(raw).expect("raw readable");
    let size = w as usize * h as usize * 3 / 2;
    assert!(bytes.len() >= size * frames, "raw holds frames");
    let mut encoder = Vp9Encoder::open(w, h, 8_000, 4).expect("encoder");
    let mut coded = ivf_header(w, h);
    let picture = PictureSize::new(w, h).expect("size");
    let start = Instant::now();
    for (n, chunk) in bytes.chunks_exact(size).take(frames).enumerate() {
        let frame = split_frame(chunk, picture);
        for packet in encoder.encode(&frame).expect("encode") {
            push_packet(&mut coded, &packet, n as u64);
        }
    }
    for packet in encoder.flush().expect("flush") {
        push_packet(&mut coded, &packet, frames as u64);
    }
    let secs = start.elapsed().as_secs_f64();
    fs::write(out, &coded).expect("bitstream written");
    println!(
        "vpxenc: {frames} frames in {secs:.2}s = {:.1} fps, {} bytes",
        crate::common::fps(frames, secs),
        coded.len()
    );
}

/// CPU frame borrowing one packed I420 `chunk`.
fn split_frame(chunk: &[u8], picture: PictureSize) -> CpuFrame {
    let width = picture.width();
    let pitch = width as usize * picture.height() as usize;
    let luma = &chunk[0..pitch];
    let chroma_u = &chunk[pitch..pitch + pitch / 4];
    let chroma_v = &chunk[pitch + pitch / 4..pitch * 3 / 2];
    let planes = [
        Plane::new(Bytes::copy_from_slice(luma), width).expect("plane"),
        Plane::new(Bytes::copy_from_slice(chroma_u), width / 2).expect("plane"),
        Plane::new(Bytes::copy_from_slice(chroma_v), width / 2).expect("plane"),
    ];
    CpuFrame::new(picture, PixelFormat::Yuv420P, planes.into()).expect("frame")
}

/// One IVF frame from a coded packet.
fn push_packet(coded: &mut Vec<u8>, packet: &Packet, pts: u64) {
    let size = u32::try_from(packet.data().len()).expect("probe packet size");
    coded.extend_from_slice(&size.to_le_bytes());
    coded.extend_from_slice(&pts.to_le_bytes());
    coded.extend_from_slice(packet.data());
}

/// 32-byte IVF header for VP9 `width`x`height`.
fn ivf_header(width: u32, height: u32) -> Vec<u8> {
    let narrow = u16::try_from(width).expect("probe width");
    let short = u16::try_from(height).expect("probe height");
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(b"DKIF");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"VP90");
    out.extend_from_slice(&narrow.to_le_bytes());
    out.extend_from_slice(&short.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&60u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}
