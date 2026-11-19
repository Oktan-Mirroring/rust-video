//! VP9 grammar fault behind tier errors.

use std::fmt::{Display, Formatter};

/// VP9 grammar fault behind tier errors.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Fault {
    /// Frame marker against the codec bits.
    FrameMarkerMismatch,
    /// Reserved profile bits.
    ReservedProfile,
    /// Sync code against the codec bytes.
    SyncCodeMismatch,
    /// Value outside its field width.
    Range {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Frame ends before the element completes.
    Truncated {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Element wider than 32 bits.
    TooWide {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Reference names an empty slot.
    NamesEmptySlot {
        /// Reference naming the empty slot.
        reference: &'static str,
    },
}

impl Display for Fault {
    /// Fault message body behind tier error wrappers.
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameMarkerMismatch => write!(f, "frame marker mismatch"),
            Self::ReservedProfile => write!(f, "reserved profile"),
            Self::SyncCodeMismatch => write!(f, "sync code mismatch"),
            Self::Range { syntax } => write!(f, "{syntax} range"),
            Self::Truncated { syntax } => write!(f, "truncated {syntax}"),
            Self::TooWide { syntax } => write!(f, "{syntax} wider than 32 bits"),
            Self::NamesEmptySlot { reference } => write!(f, "{reference} names empty slot"),
        }
    }
}

impl std::error::Error for Fault {}
