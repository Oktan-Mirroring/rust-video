//! Display-output slot shared by every codec picture buffer.

use crate::error::{D3d12Error, ParseReason};

use rust_video_caps::reason::ParseReason as SharedParse;

/// Slot number as a byte.
pub(crate) fn slot_u8(slot: usize) -> Result<u8, D3d12Error> {
    u8::try_from(slot)
        .map_err(|_| D3d12Error::Parse(ParseReason::Parse(SharedParse::Range { syntax: "slot" })))
}

/// Outputs for `emits` in display order.
pub(crate) fn outputs_sorted(mut emits: Vec<(i32, usize)>) -> Vec<Output> {
    emits.sort_unstable();
    emits
        .into_iter()
        .filter_map(|(_, slot)| slot_u8(slot).ok().map(|slot| Output { slot }))
        .collect()
}

/// Picture ready for display from `slot`.
#[derive(Debug)]
pub(crate) struct Output {
    /// Output surface slot.
    pub(crate) slot: u8,
}

impl Output {
    /// Output naming `slot`.
    pub(crate) const fn from_slot(slot: u8) -> Self {
        Self { slot }
    }

    /// Output surface slot.
    pub(crate) const fn slot(&self) -> u8 {
        self.slot
    }
}
