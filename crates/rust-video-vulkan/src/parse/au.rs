//! Access-unit splitter shared by both codecs.

/// Per-codec rule for where one access unit ends and the next begins.
pub(crate) trait AuBoundary: Sized {
    /// True for slice NAL units; only a slice can close an access unit.
    fn is_slice(&self) -> bool;

    /// True when `nalu` opens a new access unit after `buffered`.
    fn is_new_au(buffered: &[Self], nalu: &Self) -> bool;
}

/// Buffers NAL units and cuts one access unit per boundary.
pub(crate) struct AuSplitter<N> {
    /// NAL units since the last cut.
    buffered: Vec<N>,
}

impl<N: AuBoundary> AuSplitter<N> {
    /// Empty splitter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffered: Vec::new(),
        }
    }

    /// Feed one NAL unit; returns the closed access unit, if any.
    pub fn put(&mut self, nalu: N) -> Option<AccessUnit<N>> {
        if !N::is_new_au(&self.buffered, &nalu) {
            self.buffered.push(nalu);
            return None;
        }
        let end = self
            .buffered
            .iter()
            .rposition(N::is_slice)
            .map_or(0, |index| index + 1);
        let au: Vec<N> = self.buffered.drain(..end).collect();
        self.buffered.push(nalu);
        if au.is_empty() {
            None
        } else {
            Some(AccessUnit(au.into_boxed_slice()))
        }
    }

    /// Trailing NAL units as one final access unit, if any.
    pub fn flush(&mut self) -> Option<AccessUnit<N>> {
        if self.buffered.iter().any(N::is_slice) {
            let au = std::mem::take(&mut self.buffered);
            Some(AccessUnit(au.into_boxed_slice()))
        } else {
            self.buffered.clear();
            None
        }
    }
}

impl<N: AuBoundary> Default for AuSplitter<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// NAL units of one decoded picture.
pub(crate) struct AccessUnit<N>(Box<[N]>);

impl<N> AccessUnit<N> {
    /// NAL units of the picture, owned.
    #[must_use]
    pub fn into_nalus(self) -> Vec<N> {
        Vec::from(self.0)
    }
}

impl AuBoundary for crate::parse::h264::Nalu {
    fn is_slice(&self) -> bool {
        matches!(self.parsed(), crate::parse::h264::ParsedNalu::Slice(_))
    }

    fn is_new_au(buffered: &[Self], nalu: &Self) -> bool {
        use crate::parse::h264::ParsedNalu;
        let ParsedNalu::Slice(slice) = nalu.parsed() else {
            return false;
        };
        let Some(last) = buffered
            .iter()
            .rev()
            .find_map(|buffered| match buffered.parsed() {
                ParsedNalu::Slice(last) => Some(last),
                _ => None,
            })
        else {
            return true;
        };
        slice.header().first_mb_in_slice() == 0
            || last.header().frame_num() != slice.header().frame_num()
            || last.pps().id() != slice.pps().id()
            || one_zero_ref_idc(last.nal().ref_idc(), slice.nal().ref_idc())
            || poc_type_zero_differs(last, slice)
            || last.nal().is_idr() != slice.nal().is_idr()
            || idr_pic_id_differs(last, slice)
    }
}

/// True when exactly one of the two reference idcs is zero.
fn one_zero_ref_idc(last: u8, current: u8) -> bool {
    (last == 0 || current == 0) && last != current
}

/// True when type-0 POC fields differ between the two slices.
fn poc_type_zero_differs(
    last: &crate::parse::h264::Slice,
    current: &crate::parse::h264::Slice,
) -> bool {
    if last.sps().pic_order_cnt_type() != 0 || current.sps().pic_order_cnt_type() != 0 {
        return false;
    }
    last.header().pic_order_cnt_lsb() != current.header().pic_order_cnt_lsb()
        || last.header().delta_pic_order_cnt_bottom()
            != current.header().delta_pic_order_cnt_bottom()
}

/// True for two IDR slices with different IDR ids.
fn idr_pic_id_differs(
    last: &crate::parse::h264::Slice,
    current: &crate::parse::h264::Slice,
) -> bool {
    last.nal().is_idr()
        && current.nal().is_idr()
        && last.header().idr_pic_id() != current.header().idr_pic_id()
}

impl AuBoundary for crate::parse::h265::Nalu {
    fn is_slice(&self) -> bool {
        matches!(self.parsed(), crate::parse::h265::ParsedNalu::Slice(_))
    }

    fn is_new_au(_buffered: &[Self], nalu: &Self) -> bool {
        matches!(
            nalu.parsed(),
            crate::parse::h265::ParsedNalu::Slice(slice) if slice.header().first_slice_in_pic()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessUnit, AuBoundary, AuSplitter};

    /// Scripted NAL: slice or filler, opening a new unit or not.
    struct Script {
        /// Slice NAL.
        slice: bool,
        /// Opens a new unit.
        cut: bool,
    }

    impl AuBoundary for Script {
        fn is_slice(&self) -> bool {
            self.slice
        }

        fn is_new_au(_buffered: &[Self], nalu: &Self) -> bool {
            nalu.slice && nalu.cut
        }
    }

    /// Slice counts of each unit.
    fn slices(units: Vec<AccessUnit<Script>>) -> Vec<usize> {
        units
            .into_iter()
            .map(|unit| unit.into_nalus().iter().filter(|nalu| nalu.slice).count())
            .collect()
    }

    #[test]
    fn cut_returns_buffered_unit() {
        let mut splitter = AuSplitter::new();
        assert!(splitter
            .put(Script {
                slice: false,
                cut: false
            })
            .is_none());
        assert!(splitter
            .put(Script {
                slice: true,
                cut: false
            })
            .is_none());
        let cut = splitter.put(Script {
            slice: true,
            cut: true,
        });
        assert_eq!(slices(vec![cut.expect("unit")]), vec![1]);
        let tail = splitter.flush().expect("tail");
        assert_eq!(slices(vec![tail]), vec![1]);
    }

    #[test]
    fn flush_without_slice_emits_nothing() {
        let mut splitter = AuSplitter::default();
        assert!(splitter
            .put(Script {
                slice: false,
                cut: false
            })
            .is_none());
        assert!(splitter.flush().is_none());
    }
}
