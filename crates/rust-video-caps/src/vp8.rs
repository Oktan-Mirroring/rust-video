//! VP8 constants.
//!
//! Values from RFC 6386; cites name the section. VP8 has no profiles,
//! levels, or tiles.

use crate::common::{PictureSize, Validate, Violation};

/// Maximum frame dimension in samples (S9.1).
pub const MAX_DIM: u32 = 16_384;

/// Token partition count (S19.1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Partitions {
    /// One partition.
    One,
    /// Two partitions.
    Two,
    /// Four partitions.
    Four,
    /// Eight partitions.
    Eight,
}

impl Partitions {
    /// Resolve the 2-bit log2 partition count.
    #[must_use]
    pub fn from_log2(bits: u8) -> Option<Self> {
        match bits {
            0 => Some(Self::One),
            1 => Some(Self::Two),
            2 => Some(Self::Four),
            3 => Some(Self::Eight),
            _ => None,
        }
    }

    /// Partition count.
    #[must_use]
    pub fn count(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
            Self::Eight => 8,
        }
    }
}

/// VP8 coding parameters: 8-bit 4:2:0 only (S1).
pub struct Params {
    /// Picture size.
    pub size: PictureSize,
}

impl Validate for Params {
    /// Check the `16_384` dimension cap (S9.1).
    ///
    /// # Errors
    ///
    /// Returns `SizeRuleExceeded` above the cap.
    fn validate(&self) -> Result<(), Violation> {
        let axis = self.size.width().max(self.size.height());
        if axis > MAX_DIM {
            return Err(Violation::SizeRuleExceeded {
                actual: axis,
                max: u64::from(MAX_DIM),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::common::{PictureSize, Validate, Violation};

    use super::{Params, Partitions, MAX_DIM};

    #[test]
    fn max_dim_anchor() {
        assert_eq!(MAX_DIM, 16_384);
    }

    #[test]
    fn partitions_resolve() {
        assert_eq!(Partitions::from_log2(2), Some(Partitions::Four));
        assert_eq!(Partitions::from_log2(4), None);
        assert_eq!(Partitions::Eight.count(), 8);
    }

    #[test]
    fn oversize_fails() {
        let params = Params {
            size: PictureSize::new(20_000, 100).expect("wide"),
        };
        assert!(matches!(
            params.validate(),
            Err(Violation::SizeRuleExceeded { .. })
        ));
    }
}
