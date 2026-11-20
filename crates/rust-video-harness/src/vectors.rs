use std::path::PathBuf;

/// Committed bench-vector directory.
#[must_use]
pub fn bench_dir() -> PathBuf {
    vectors_root().join("bench")
}

/// Committed decode-vector directory: shared 320x240 corpus.
#[must_use]
pub fn decode_dir() -> PathBuf {
    vectors_root().join("decode")
}

/// Vector root: `$RUST_VIDEO_VECTORS` when set, else the committed directory.
fn vectors_root() -> PathBuf {
    root_with(std::env::var_os("RUST_VIDEO_VECTORS").map(PathBuf::from))
}

/// Vector root over an explicit override.
fn root_with(over: Option<PathBuf>) -> PathBuf {
    over.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors"))
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
    fn override_replaces_manifest_root() {
        use super::root_with;
        use std::path::PathBuf;
        assert_eq!(
            root_with(Some(PathBuf::from("/data/local/tmp/vectors"))),
            PathBuf::from("/data/local/tmp/vectors")
        );
        assert!(root_with(None).join("bench").is_dir());
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
