use std::path::PathBuf;

/// Committed bench-vector directory.
#[must_use]
pub fn bench_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors/bench")
}

/// Committed decode-vector directory: shared 320x240 corpus.
#[must_use]
pub fn decode_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors/decode")
}

#[cfg(test)]
mod tests {
    use super::{bench_dir, decode_dir};

    #[test]
    fn bench_vectors_exist() {
        for name in [
            "h265-1080p-nvenc.h265",
            "av1-1080p.obu",
            "vp9-1080p.ivf",
            "vp8-1080p.ivf",
        ] {
            assert!(bench_dir().join(name).is_file(), "missing {name}");
        }
    }

    #[test]
    fn decode_vectors_exist() {
        for name in [
            "bframes_320x240.h264",
            "grain_320x240.obu",
            "inter_320x240.hevc",
            "intra_320x240.hevc",
            "mslice_320x240.hevc",
            "sl_320x240.hevc",
            "tiled_320x240.obu",
            "valid_320x240.h264",
            "valid_320x240.ivf",
            "valid_320x240.obu",
        ] {
            assert!(decode_dir().join(name).is_file(), "missing {name}");
        }
    }
}
