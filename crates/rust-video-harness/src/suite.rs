use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use rust_video::VideoCodec;

use crate::Error;

/// One vector file with its codec.
pub struct Case {
    name: String,
    codec: VideoCodec,
    path: PathBuf,
}

impl Case {
    /// File name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Codec.
    #[must_use]
    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// Vector path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Corpus on disk.
pub struct Suite {
    cases: Vec<Case>,
}

impl Suite {
    /// Open `RUST_VIDEO_CORPUS`, else `./corpus`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the directory is absent or unreadable.
    pub fn open() -> Result<Self, Error> {
        let root =
            env::var_os("RUST_VIDEO_CORPUS").map_or_else(|| PathBuf::from("corpus"), PathBuf::from);
        Self::open_at(&root)
    }

    /// Open an explicit corpus root.
    ///
    /// Scans `<root>/<codec>/` for known codec names; anything else is ignored.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the directory is absent or unreadable.
    pub fn open_at(root: &Path) -> Result<Self, Error> {
        if !root.is_dir() {
            return Err(Error::MissingCorpus(root.to_path_buf()));
        }
        let mut cases = Vec::new();
        for dir in fs::read_dir(root)? {
            let dir = dir?;
            if !dir.path().is_dir() {
                continue;
            }
            let Some(codec) = Self::codec_of_dir(&dir.file_name()) else {
                continue;
            };
            for file in fs::read_dir(dir.path())? {
                let file = file?;
                if !file.path().is_file() {
                    continue;
                }
                cases.push(Case {
                    name: file.file_name().to_string_lossy().into_owned(),
                    codec,
                    path: file.path(),
                });
            }
        }
        cases.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self { cases })
    }

    /// All cases in path order.
    #[must_use]
    pub fn cases(&self) -> &[Case] {
        &self.cases
    }

    /// Cases for one codec.
    pub fn cases_for(&self, codec: VideoCodec) -> impl Iterator<Item = &Case> {
        self.cases.iter().filter(move |case| case.codec == codec)
    }

    fn codec_of_dir(name: &OsStr) -> Option<VideoCodec> {
        match name.to_str()? {
            "h264" => Some(VideoCodec::H264),
            "h265" => Some(VideoCodec::H265),
            "av1" => Some(VideoCodec::Av1),
            "vp8" => Some(VideoCodec::Vp8),
            "vp9" => Some(VideoCodec::Vp9),
            "av2" => Some(VideoCodec::Av2),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use rust_video::VideoCodec;

    use super::Suite;
    use crate::Error;

    #[test]
    fn codec_of_dir_maps_known_names() {
        assert_eq!(
            Suite::codec_of_dir(OsStr::new("h264")),
            Some(VideoCodec::H264)
        );
        assert_eq!(
            Suite::codec_of_dir(OsStr::new("av1")),
            Some(VideoCodec::Av1)
        );
        assert_eq!(
            Suite::codec_of_dir(OsStr::new("vp8")),
            Some(VideoCodec::Vp8)
        );
        assert_eq!(Suite::codec_of_dir(OsStr::new("yuv")), None);
    }

    #[test]
    fn open_at_rejects_missing_root() {
        let missing = std::env::temp_dir().join("rust-video-no-such-corpus");
        assert!(matches!(
            Suite::open_at(&missing),
            Err(Error::MissingCorpus(_))
        ));
    }
}
