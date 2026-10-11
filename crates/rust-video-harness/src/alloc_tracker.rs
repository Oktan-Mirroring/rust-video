#![allow(unsafe_code)]
//! Allocation counting for zero-copy tests.
//!
//! Install [`CountingAlloc`] as the test binary allocator, then assert the
//! measured path allocates nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// System allocator wrapper counting allocations.
#[derive(Clone, Copy, Debug, Default)]
pub struct CountingAlloc;

impl CountingAlloc {
    /// Allocations served since the last reset.
    #[must_use]
    pub fn allocation_count() -> u64 {
        ALLOCATION_COUNT.load(Ordering::Relaxed)
    }

    /// Bytes currently outstanding.
    #[must_use]
    pub fn allocated_bytes() -> usize {
        ALLOCATED_BYTES.load(Ordering::Relaxed)
    }

    /// Zero both counters.
    pub fn reset() {
        ALLOCATION_COUNT.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    }
}

// SAFETY: every call delegates to `System`; the counters are atomic adds only.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` comes from the caller, forwarded unchanged.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` come from the caller, forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) };
        ALLOCATED_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

/// Run `measured` after `warmup` and fail on any allocation inside `measured`.
///
/// # Arguments
///
/// * `warmup` - one unlabeled run settling caches and lazy state.
/// * `measured` - the path under test; must not allocate.
///
/// # Panics
///
/// Panics when `measured` allocates.
pub fn assert_no_allocs_after_warmup(warmup: impl FnOnce(), measured: impl FnOnce()) {
    warmup();
    CountingAlloc::reset();
    measured();
    assert_eq!(CountingAlloc::allocation_count(), 0);
}

#[cfg(test)]
mod tests {
    use super::CountingAlloc;

    #[global_allocator]
    static ALLOC: CountingAlloc = CountingAlloc;

    #[test]
    fn counts_heap_allocations() {
        CountingAlloc::reset();
        let before = CountingAlloc::allocation_count();
        let mut owned = Vec::new();
        owned.extend_from_slice(&[0u8; 64]);
        assert_eq!(owned.len(), 64);
        assert!(CountingAlloc::allocation_count() > before);
    }
}
