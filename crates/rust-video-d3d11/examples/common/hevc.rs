//! HEVC test vectors: committed stream plus per-picture packets.
//!
//! Single-slice pictures; multi-slice pictures would split early.

// Stays: d3d11 picture splitter; d3d12 carries a synth instead, no twin.

use std::path::PathBuf;

/// Committed 320x240 HEVC vector path.
pub fn vector() -> PathBuf {
    rust_video_harness::decode_dir().join("inter_320x240.hevc")
}

/// Start code offsets in `data`.
fn start_codes(data: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut zeros = 0;
    for (index, byte) in data.iter().enumerate() {
        if *byte == 1 && zeros >= 2 {
            offsets.push(index + 1);
        }
        zeros = if *byte == 0 { zeros + 1 } else { 0 };
    }
    offsets
}

/// NAL units with start codes stripped.
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let offsets = start_codes(data);
    let mut nals = Vec::with_capacity(offsets.len());
    for (position, offset) in offsets.iter().enumerate() {
        let end = offsets.get(position + 1).map_or(data.len(), |next| {
            let mut end = *next - 3;
            while data[end - 1] == 0 {
                end -= 1;
            }
            end
        });
        nals.push(&data[*offset..end]);
    }
    nals
}

/// True for VCL NAL units.
fn is_vcl(nal: &[u8]) -> bool {
    nal.first().is_some_and(|header| (header >> 1) & 0x3F <= 31)
}

/// Picture packets: parameter sets ride with the previous picture.
pub fn picture_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut current = Vec::new();
    let mut seen_vcl = false;
    for nal in split_nals(data) {
        let vcl = is_vcl(nal);
        if vcl && seen_vcl {
            packets.push(std::mem::take(&mut current));
            seen_vcl = false;
        }
        seen_vcl |= vcl;
        current.extend_from_slice(&[0, 0, 0, 1]);
        current.extend_from_slice(nal);
    }
    if !current.is_empty() {
        packets.push(current);
    }
    packets
}
