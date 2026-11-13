//! Host-side downstream proof: the error enum matches behind a wildcard.

use rust_video_mediacodec::MediaCodecError;

/// Downstream match over host-visible variants plus a wildcard.
#[test]
fn downstream_match_compiles_on_host() {
    let error = MediaCodecError::BufferLost;
    let seen = match error {
        MediaCodecError::BufferLost => "lost",
        MediaCodecError::InputStarved => "starved",
        _ => "other",
    };
    assert_eq!(seen, "lost");
}
