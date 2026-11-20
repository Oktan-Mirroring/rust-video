/// AV1 OBU splitting and temporal-unit grouping.
pub struct Obu;

impl Obu {
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

    /// First sequence-header OBU bytes, absent without one.
    #[must_use]
    pub fn sequence_header(data: &[u8]) -> Option<Vec<u8>> {
        Self::split_obus(data)
            .into_iter()
            .find(|(unit_type, _)| *unit_type == 1)
            .map(|(_, bytes)| bytes)
    }

    /// OBU type plus bytes in order.
    #[must_use]
    pub fn split_obus(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut obus = Vec::new();
        let mut offset = 0;
        while offset < data.len() {
            let Some((unit_type, total)) = Self::obu_header(&data[offset..]) else {
                break;
            };
            let end = (offset + total).min(data.len());
            obus.push((unit_type, data[offset..end].to_vec()));
            offset = end;
        }
        obus
    }

    /// Temporal-unit packets: each packet ends with its delimiter.
    #[must_use]
    pub fn tu_packets(data: &[u8]) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut current = Vec::new();
        for (unit_type, obu) in Self::split_obus(data) {
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
}

#[cfg(test)]
mod tests {
    use super::Obu;

    #[test]
    fn units_split_on_delimiter() {
        let frame = [0x1Au8, 0x01, 0xAA];
        let delimiter = [0x12u8, 0x00];
        let mut stream = Vec::new();
        stream.extend_from_slice(&frame);
        stream.extend_from_slice(&delimiter);
        stream.extend_from_slice(&frame);
        stream.extend_from_slice(&delimiter);
        let obus = Obu::split_obus(&stream);
        assert_eq!(obus.len(), 4);
        assert_eq!(obus[0].0, 3);
        assert_eq!(obus[1].0, 2);
        let packets = Obu::tu_packets(&stream);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].len(), frame.len() + delimiter.len());
    }
}
