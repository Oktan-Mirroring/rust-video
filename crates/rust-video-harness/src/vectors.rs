use std::path::PathBuf;

/// Committed bench-vector directory.
#[must_use]
pub fn bench_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors/bench")
}

#[cfg(test)]
mod tests {
    use super::bench_dir;

    #[test]
    fn bench_vectors_exist() {
        for name in [
            "h265-1080p.h265",
            "av1-1080p.obu",
            "vp9-1080p.ivf",
            "vp8-1080p.ivf",
        ] {
            assert!(bench_dir().join(name).is_file(), "missing {name}");
        }
    }
}
