//! Backend failure vocabulary.

use std::fmt;

/// Software backend failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SwError {
    /// Backend rejected its configuration.
    Init,
    /// Backend rejected one input unit.
    Rejected,
    /// Backend returned a frame the facade cannot hold.
    BadFrame,
}

impl fmt::Display for SwError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init => write!(f, "backend init failed"),
            Self::Rejected => write!(f, "backend rejected input"),
            Self::BadFrame => write!(f, "backend returned unusable frame"),
        }
    }
}

impl std::error::Error for SwError {}
