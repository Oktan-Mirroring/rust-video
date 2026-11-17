//! VP9 test vectors: committed stream plus per-frame packets.

// Stays: byte-slice IVF splitter; the harness Ivf twin reads from a path.

use std::path::PathBuf;

/// Committed 320x240 VP9 vector path.
pub fn vector() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/valid_320x240.ivf")
}

/// Frame payloads of `data` in file order, without IVF headers.
pub fn frame_packets(data: &[u8]) -> Vec<Vec<u8>> {
    assert!(data.len() >= 32 && data[..4] == *b"DKIF", "vector is IVF");
    let head = usize::from(u16::from_le_bytes([data[6], data[7]]).max(32));
    let mut packets = Vec::new();
    let mut offset = head;
    while let Some(entry) = data.get(offset..offset + 12) {
        let size = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let size = usize::try_from(size).expect("payload fits");
        let Some(payload) = data.get(offset + 12..offset + 12 + size) else {
            break;
        };
        packets.push(payload.to_vec());
        offset += 12 + size;
    }
    packets
}
