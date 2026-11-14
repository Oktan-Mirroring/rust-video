//! AV1 test vectors: committed stream plus temporal-unit packets.

// Stays: AV1 OBU splitter; sole consumer pair, no cross-tier twin.

use std::path::PathBuf;

/// Committed 320x240 AV1 vector path.
pub fn vector() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/valid_320x240.obu")
}

/// OBU header with its total length in bytes.
fn obu_header(data: &[u8]) -> Option<(u8, usize)> {
    let header = *data.first()?;
    if header >> 7 != 0 {
        return None;
    }
    let unit_type = (header >> 3) & 0x0F;
    let extension = header >> 2 & 1 != 0;
    let has_size = header >> 1 & 1 != 0;
    let mut offset = 1;
    if extension {
        offset += 1;
    }
    if !has_size {
        return Some((unit_type, offset));
    }
    let mut size = 0usize;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(offset)?;
        offset += 1;
        size |= usize::from(byte & 0x7F) << shift.min(56);
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
    }
    Some((unit_type, offset + size))
}

/// OBU type plus bytes in order.
pub fn split_obus(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut obus = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let Some((unit_type, total)) = obu_header(&data[offset..]) else {
            break;
        };
        let end = (offset + total).min(data.len());
        obus.push((unit_type, data[offset..end].to_vec()));
        offset = end;
    }
    obus
}

/// Temporal-unit packets: each packet ends with its delimiter.
pub fn tu_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut current = Vec::new();
    for (unit_type, obu) in split_obus(data) {
        current.extend_from_slice(&obu);
        if unit_type == 2 {
            packets.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        packets.push(current);
    }
    packets
}
