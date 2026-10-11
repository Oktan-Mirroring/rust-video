use std::{fmt, io, path::PathBuf};

/// Harness failure.
#[derive(Debug)]
pub enum Error {
    /// Filesystem failure.
    Io(io::Error),
    /// Corpus directory absent.
    MissingCorpus(PathBuf),
    /// Golden file malformed.
    Golden(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "io: {error}"),
            Self::MissingCorpus(path) => write!(f, "corpus missing: {}", path.display()),
            Self::Golden(why) => write!(f, "golden: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::MissingCorpus(_) | Self::Golden(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
