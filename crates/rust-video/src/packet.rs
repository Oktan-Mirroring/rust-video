//! Coded packet vocabulary.

use std::time::Duration;

/// Coded bytes with timing and keyframe flag.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Packet {
    data: Vec<u8>,
    pts: Option<Duration>,
    dts: Option<Duration>,
    keyframe: bool,
}

impl Packet {
    /// Packet carrying `data` with no timestamps and no keyframe flag.
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            pts: None,
            dts: None,
            keyframe: false,
        }
    }

    /// Packet with presentation timestamp `pts`.
    #[must_use]
    pub fn with_pts(mut self, pts: Duration) -> Self {
        self.pts = Some(pts);
        self
    }

    /// Packet with decode timestamp `dts`.
    #[must_use]
    pub fn with_dts(mut self, dts: Duration) -> Self {
        self.dts = Some(dts);
        self
    }

    /// Packet flagged as a keyframe.
    #[must_use]
    pub fn mark_keyframe(mut self) -> Self {
        self.keyframe = true;
        self
    }

    /// Coded bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Presentation timestamp, if stamped.
    #[must_use]
    pub fn pts(&self) -> Option<Duration> {
        self.pts
    }

    /// Decode timestamp, if stamped.
    #[must_use]
    pub fn dts(&self) -> Option<Duration> {
        self.dts
    }

    /// Keyframe flag.
    #[must_use]
    pub fn is_keyframe(&self) -> bool {
        self.keyframe
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Packet;

    #[test]
    fn new_packet_has_no_stamps_or_flag() {
        let packet = Packet::new(vec![0x65, 0x88]);
        assert_eq!(packet.data(), &[0x65, 0x88]);
        assert_eq!(packet.pts(), None);
        assert_eq!(packet.dts(), None);
        assert!(!packet.is_keyframe());
    }

    #[test]
    fn builders_stamp_time_and_flag() {
        let pts = Duration::from_millis(40);
        let dts = Duration::from_millis(20);
        let packet = Packet::new(vec![1])
            .with_pts(pts)
            .with_dts(dts)
            .mark_keyframe();
        assert_eq!(packet.pts(), Some(pts));
        assert_eq!(packet.dts(), Some(dts));
        assert!(packet.is_keyframe());
    }
}
