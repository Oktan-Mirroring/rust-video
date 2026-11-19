//! Annex-B parsers feeding the video sessions.

pub(crate) mod au;
pub(crate) mod av1;
pub(crate) mod h264;
pub(crate) mod h265;
pub(crate) mod refs;

#[cfg(test)]
pub(crate) mod test_support {
    //! Byte vectors shared by parser tests.

    /// Bytes of a hex string.
    pub fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// H.264 NAL units out of Annex-B `payloads`.
    pub fn split264(payloads: &[Vec<u8>]) -> Vec<rust_video_h264::Nalu> {
        let mut splitter = rust_video_h264::NaluSplitter::new();
        let mut units = splitter.push(&stream(payloads));
        units.extend(splitter.flush());
        units
    }

    /// H.265 NAL units out of Annex-B `payloads`.
    pub fn split265(payloads: &[Vec<u8>]) -> Vec<rust_video_h265::Nalu> {
        let mut splitter = rust_video_h265::NaluSplitter::new();
        let mut units = splitter.push(&stream(payloads));
        units.extend(splitter.flush());
        units
    }

    /// Annex-B stream of `payloads` with 3-byte start codes.
    fn stream(payloads: &[Vec<u8>]) -> Vec<u8> {
        let mut stream = Vec::new();
        for payload in payloads {
            stream.extend_from_slice(&[0, 0, 1]);
            stream.extend_from_slice(payload);
        }
        stream
    }
}
