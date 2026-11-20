//! IDR cadence of one encode session.

/// IDR cadence of one encode session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Gop {
    /// Frames between IDR pictures, at least 1.
    interval: u32,
}

impl Gop {
    /// Cadence coding an IDR every `frames` pictures.
    ///
    /// `0` clamps to 1, coding every picture as IDR.
    #[must_use]
    pub fn interval(frames: u32) -> Self {
        Self {
            interval: frames.max(1),
        }
    }

    /// Cadence coding every picture as IDR.
    #[must_use]
    pub fn all_intra() -> Self {
        Self { interval: 1 }
    }

    /// Frames between IDR pictures.
    #[must_use]
    pub const fn period(self) -> u32 {
        self.interval
    }
}

impl Default for Gop {
    /// All-intra cadence: every picture codes as IDR.
    fn default() -> Self {
        Self::all_intra()
    }
}

#[cfg(test)]
mod tests {
    use super::Gop;

    #[test]
    fn interval_clamps_zero_to_all_intra() {
        assert_eq!(Gop::interval(0).period(), 1);
        assert_eq!(Gop::interval(30).period(), 30);
    }

    #[test]
    fn all_intra_codes_every_picture_as_idr() {
        assert_eq!(Gop::all_intra().period(), 1);
    }

    #[test]
    fn default_is_all_intra() {
        assert_eq!(Gop::default().period(), Gop::all_intra().period());
    }
}
