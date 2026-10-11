//! Annex-B parsers feeding the video sessions.

pub(crate) mod au;
pub(crate) mod bits;
pub(crate) mod h264;
pub(crate) mod h265;
pub(crate) mod nal;
pub(crate) mod refs;

#[cfg(test)]
pub(crate) mod test_support {
    //! Byte vectors shared by parser tests.

    use super::nal::{Nalu, NaluSplitter};

    /// Bytes of a hex string.
    pub fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// NAL units out of Annex-B `payloads`.
    pub fn split(payloads: &[Vec<u8>]) -> Vec<Nalu> {
        let mut splitter = NaluSplitter::new();
        let mut stream = Vec::new();
        for payload in payloads {
            stream.extend_from_slice(&[0, 0, 1]);
            stream.extend_from_slice(payload);
        }
        let mut units = splitter.push(&stream);
        units.extend(splitter.flush());
        units
    }
}
