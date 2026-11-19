//! Display-output slot shared by every codec picture buffer.

/// Picture ready for display from `slot`.
#[derive(Debug)]
pub(crate) struct Output {
    /// Output surface slot.
    slot: u8,
}

impl Output {
    /// Output for surface `slot`.
    pub(crate) const fn from_slot(slot: u8) -> Self {
        Self { slot }
    }

    /// Output surface slot.
    pub(crate) const fn slot(&self) -> u8 {
        self.slot
    }
}
