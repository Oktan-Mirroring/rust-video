use crate::{Case, Error, FrameHash};

/// Decoder under test.
pub trait Oracle {
    /// Decode every frame, hashing raw bytes in output order.
    ///
    /// # Errors
    ///
    /// Returns `Err` when decode fails.
    fn decode(&self, case: &Case) -> Result<Vec<FrameHash>, Error>;
}
