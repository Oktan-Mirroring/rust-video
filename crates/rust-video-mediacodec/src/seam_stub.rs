//! Host stub behind the codec seam.

use crate::decoder::{DecodeState, MediaCodecDecoder, OutputLayout};
use crate::encoder::{EncodeSpec, EncodeState, InputLayout, MediaCodecEncoder};
use crate::error::MediaCodecError;
use crate::format::FormatSpec;
use crate::overlap::SubmitOutcome;
use crate::seam::{CodecSeam, SeamItem};
use crate::surface::SurfaceSlots;

/// Stub session direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StubRole {
    /// Decode: packets in, frames out.
    Decode,
    /// Encode: frames in, packets out.
    Encode,
}

impl StubRole {
    /// Loud missing-codec failure for `mime`.
    fn failure(self, mime: &'static str) -> MediaCodecError {
        match self {
            Self::Decode => MediaCodecError::NoDecoder { mime },
            Self::Encode => MediaCodecError::NoEncoder { mime },
        }
    }
}

/// Host stub behind the codec seam.
///
/// Off-Android opens succeed so host tests exercise open paths; every
/// use fails loud with the missing-codec error, except `submit_nowait`,
/// which answers `Starved` so split-phase submits read backpressure.
#[derive(Debug)]
pub(crate) struct StubSeam {
    /// Mime the lookup would use.
    mime: &'static str,
    /// Session direction.
    role: StubRole,
}

impl StubSeam {
    /// Decode stub failing loud for `mime`.
    pub(crate) fn decoder(mime: &'static str) -> Self {
        Self {
            mime,
            role: StubRole::Decode,
        }
    }

    /// Encode stub failing loud for `mime`.
    pub(crate) fn encoder(mime: &'static str) -> Self {
        Self {
            mime,
            role: StubRole::Encode,
        }
    }
}

impl CodecSeam for StubSeam {
    /// Loud missing-codec failure; nothing ever queues.
    fn submit(&mut self, _payload: &[u8], _pts_us: u64) -> Result<Vec<SeamItem>, MediaCodecError> {
        Err(self.role.failure(self.mime))
    }

    /// Permanent backpressure; the stub never queues.
    fn submit_nowait(
        &mut self,
        _payload: &[u8],
        _pts_us: u64,
    ) -> Result<(SubmitOutcome, Vec<SeamItem>), MediaCodecError> {
        Ok((SubmitOutcome::Starved, Vec::new()))
    }

    /// Loud missing-codec failure; nothing ever drains.
    fn read(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
        Err(self.role.failure(self.mime))
    }

    /// Loud missing-codec failure; nothing ever finishes.
    fn finish_input(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
        Err(self.role.failure(self.mime))
    }

    /// Loud missing-codec failure; nothing ever resets.
    fn reset(&mut self) -> Result<Vec<SeamItem>, MediaCodecError> {
        Err(self.role.failure(self.mime))
    }

    /// Loud missing-codec failure; nothing ever keys.
    fn request_sync(&mut self) -> Result<(), MediaCodecError> {
        Err(self.role.failure(self.mime))
    }
}

impl MediaCodecDecoder {
    /// Decoder over the host stub for `spec`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without both `csd` blobs on H.264/H.265.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec input format.
    pub(crate) fn open_stub(spec: &FormatSpec) -> Result<Self, MediaCodecError> {
        spec.check_csd()?;
        let output = OutputLayout::tentative(spec).ok_or(MediaCodecError::BadExtent {
            width: i32::try_from(spec.width()).unwrap_or(i32::MAX),
            height: i32::try_from(spec.height()).unwrap_or(i32::MAX),
        })?;
        Ok(Self::from_backend(
            DecodeState::Started,
            output,
            SurfaceSlots::new(SurfaceSlots::DEFAULT_CAPACITY),
            Box::new(StubSeam::decoder(spec.mime())),
        ))
    }
}

impl MediaCodecEncoder {
    /// Encoder over the host stub for `spec`.
    ///
    /// # Errors
    ///
    /// Returns `ConfigDeclined` when the config exceeds the tier.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec output format.
    ///
    /// # Usage Notes
    ///
    /// The config maps against an unknown device: ungated faults decline
    /// here, gated keys stay allowed for the device open to judge. The
    /// stub carries the tentative layout and fails loud at the first
    /// encode.
    pub(crate) fn open_stub(spec: &EncodeSpec) -> Result<Self, MediaCodecError> {
        spec.format_entries(None)?;
        let input = InputLayout::tentative(spec).ok_or(MediaCodecError::BadExtent {
            width: i32::try_from(spec.width()).unwrap_or(i32::MAX),
            height: i32::try_from(spec.height()).unwrap_or(i32::MAX),
        })?;
        Ok(Self::from_backend(
            EncodeState::Started,
            spec.clone(),
            input,
            Box::new(StubSeam::encoder(spec.mime())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{StubRole, StubSeam};
    use crate::error::MediaCodecError;
    use crate::overlap::SubmitOutcome;
    use crate::seam::CodecSeam;

    #[test]
    fn role_names_missing_side() {
        assert_eq!(
            StubRole::Decode.failure("video/avc"),
            MediaCodecError::NoDecoder { mime: "video/avc" }
        );
        assert_eq!(
            StubRole::Encode.failure("video/hevc"),
            MediaCodecError::NoEncoder { mime: "video/hevc" }
        );
    }

    #[test]
    fn blocking_calls_fail_loud_per_role() {
        let mut decode = StubSeam::decoder("video/avc");
        assert_eq!(
            decode.submit(&[0], 0),
            Err(MediaCodecError::NoDecoder { mime: "video/avc" })
        );
        assert_eq!(
            decode.read(),
            Err(MediaCodecError::NoDecoder { mime: "video/avc" })
        );
        assert_eq!(
            decode.reset(),
            Err(MediaCodecError::NoDecoder { mime: "video/avc" })
        );
        assert_eq!(
            decode.request_sync(),
            Err(MediaCodecError::NoDecoder { mime: "video/avc" })
        );
        let mut encode = StubSeam::encoder("video/hevc");
        assert_eq!(
            encode.submit(&[0], 0),
            Err(MediaCodecError::NoEncoder { mime: "video/hevc" })
        );
        assert_eq!(
            encode.read(),
            Err(MediaCodecError::NoEncoder { mime: "video/hevc" })
        );
        assert_eq!(
            encode.reset(),
            Err(MediaCodecError::NoEncoder { mime: "video/hevc" })
        );
        assert_eq!(
            encode.request_sync(),
            Err(MediaCodecError::NoEncoder { mime: "video/hevc" })
        );
    }

    #[test]
    fn nowait_answers_permanent_backpressure() {
        let mut decode = StubSeam::decoder("video/avc");
        assert_eq!(
            decode
                .submit_nowait(&[0], 0)
                .expect("backpressure, not error"),
            (SubmitOutcome::Starved, Vec::new())
        );
        let mut encode = StubSeam::encoder("video/hevc");
        assert_eq!(
            encode
                .submit_nowait(&[0], 0)
                .expect("backpressure, not error"),
            (SubmitOutcome::Starved, Vec::new())
        );
    }
}
