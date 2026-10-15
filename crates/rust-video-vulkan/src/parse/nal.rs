//! Annex-B NAL unit splitter.

/// One Annex-B NAL unit with its leading start code intact.
pub(crate) struct Nalu {
    /// Start code plus header plus payload.
    bytes: Vec<u8>,
}

impl Nalu {
    /// Start code plus header plus payload.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Header plus payload with the 3- or 4-byte start code cut.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        if self.bytes.len() >= 4 && self.bytes[2] == 0 {
            &self.bytes[4..]
        } else if self.bytes.len() >= 3 {
            &self.bytes[3..]
        } else {
            &[]
        }
    }
}

/// Start-code scanner buffering partial NAL units across pushes.
pub(crate) struct NaluSplitter {
    /// Bytes not yet emitted.
    buffer: Vec<u8>,
    /// Rescan floor; keeps a split start code reparseable.
    floor: usize,
}

impl NaluSplitter {
    /// Empty splitter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            floor: 0,
        }
    }

    /// Complete NAL units out of `chunk`.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Nalu> {
        self.buffer.extend_from_slice(chunk);
        self.trim_junk();
        let mut out = Vec::new();
        self.drain_complete(&mut out);
        self.floor = self.buffer.len().saturating_sub(4);
        out
    }

    /// Trailing bytes as one final NAL unit, if any.
    pub fn flush(&mut self) -> Vec<Nalu> {
        self.trim_junk();
        let mut out = Vec::new();
        self.drain_complete(&mut out);
        if unit_contains_payload(&self.buffer) {
            out.push(Nalu {
                bytes: std::mem::take(&mut self.buffer),
            });
        } else {
            self.buffer.clear();
        }
        self.floor = 0;
        out
    }
}

impl Default for NaluSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl NaluSplitter {
    /// Complete units out of the buffer into `out`.
    fn drain_complete(&mut self, out: &mut Vec<Nalu>) {
        while let Some(cut) = find_start(&self.buffer, self.floor) {
            let unit: Vec<u8> = self.buffer.drain(..cut).collect();
            if unit_contains_payload(&unit) {
                out.push(Nalu { bytes: unit });
            }
            self.floor = 0;
        }
    }

    /// Drop bytes before the first start code.
    fn trim_junk(&mut self) {
        if starts_with_code(&self.buffer) {
            return;
        }
        let mut i = 0;
        while i + 2 < self.buffer.len() {
            if self.buffer[i] == 0 && self.buffer[i + 1] == 0 && self.buffer[i + 2] == 1 {
                let start = if i > 0 && self.buffer[i - 1] == 0 {
                    i - 1
                } else {
                    i
                };
                self.buffer.drain(..start);
                self.floor = 0;
                return;
            }
            i += 1;
        }
        let keep = self.buffer.len().min(3);
        self.buffer.drain(..self.buffer.len() - keep);
        self.floor = 0;
    }
}

/// True when `buffer` opens with a 3- or 4-byte start code.
fn starts_with_code(buffer: &[u8]) -> bool {
    if buffer.len() >= 4 && buffer[..4] == [0, 0, 0, 1] {
        return true;
    }
    buffer.len() >= 3 && buffer[..3] == [0, 0, 1]
}

/// Offset of the next start code at or after `from`, past the one opening the unit.
fn find_start(buffer: &[u8], from: usize) -> Option<usize> {
    let mut i = from.max(3);
    while i + 2 < buffer.len() {
        if buffer[i] == 0 && buffer[i + 1] == 0 && buffer[i + 2] == 1 {
            if buffer[i - 1] == 0 {
                return Some(i - 1);
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// True when `unit` holds a start code plus at least one payload byte.
fn unit_contains_payload(unit: &[u8]) -> bool {
    if unit.len() >= 4 && unit[0] == 0 && unit[1] == 0 && unit[2] == 0 && unit[3] == 1 {
        return unit.len() > 4;
    }
    if unit.len() >= 3 && unit[0] == 0 && unit[1] == 0 && unit[2] == 1 {
        return unit.len() > 3;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::NaluSplitter;

    /// Payload bytes of each emitted unit.
    fn payloads(units: &[super::Nalu]) -> Vec<Vec<u8>> {
        units.iter().map(|unit| unit.payload().to_vec()).collect()
    }

    #[test]
    fn splits_mixed_start_codes() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2]);
        assert_eq!(payloads(&units), vec![vec![0x67, 1]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x68, 2]]);
    }

    #[test]
    fn start_code_split_across_pushes_survives() {
        let mut splitter = NaluSplitter::new();
        assert!(splitter.push(&[0, 0, 0, 1, 0x67]).is_empty());
        let units = splitter.push(&[9, 0, 0]);
        assert!(units.is_empty());
        let units = splitter.push(&[1, 0x68]);
        assert_eq!(payloads(&units), vec![vec![0x67, 9]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x68]]);
    }

    #[test]
    fn leading_junk_before_first_start_code_drops() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0xFF, 0xFF, 0, 0, 1, 0x65]);
        assert!(units.is_empty());
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x65]]);
    }

    #[test]
    fn flush_of_empty_emits_nothing() {
        let mut splitter = NaluSplitter::default();
        assert!(splitter.flush().is_empty());
    }
}
