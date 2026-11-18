//! Decode-context seam: submit, read, wait, and pool access.

use std::mem::MaybeUninit;
use std::os::raw::c_int;
use std::sync::Arc;

use crate::error::VaApiError;

/// Native VA-API surface handle (`VASurfaceID`).
///
/// Threading: a single thread needs nothing; threads sharing one
/// device serialize context calls with crate submissions: sequence
/// driver calls made through the handle with `submit`, `sync`,
/// `wait`, and `export`, never overlapping them.
///
/// Lifetime: the handle borrows the decode pool and stays valid
/// while the owning decoder lives; a frame's surface stays valid
/// while the frame lives. Using a handle after its pool drops is
/// undefined behavior.
pub type SurfaceId = u32;

/// Native VA-API decode-context handle (`VAContextID`).
///
/// Threading: a single thread needs nothing; threads sharing one
/// device serialize context calls with crate submissions: sequence
/// driver calls made through the handle with `submit` and `sync`,
/// never overlapping them.
///
/// Lifetime: the handle borrows the backend and stays valid only
/// while the owning decoder lives; using it after the decoder
/// drops is undefined behavior.
pub type ContextId = u32;

/// Decode-context operations behind one seam.
///
/// Submit, NV12 read, surface wait, and pool access; construction,
/// gauges, and native-handle escape stay on the context itself.
pub trait VaSeam {
    /// Decode one picture into `target` from typed slice parameters.
    ///
    /// The picture joins at most four in-flight submits; the set it
    /// reuses was synced against its own previous surface, so the
    /// buffers are safe to rewrite. Callers sync at consumption:
    /// `read_nv12` and the DMA-BUF export both sync first.
    /// `picture` fills the picture-parameter buffer, `matrix` the
    /// matrix buffer when present, and `slices` the slice-parameter
    /// buffers in order. `data` must answer every index below
    /// `slices`. A failed submit quiets the target and leaves the
    /// context reusable.
    ///
    /// # Errors
    ///
    /// Returns `Err` when any VA-API call fails.
    fn submit_slices<'d, T, F>(
        &mut self,
        target: SurfaceId,
        picture: &[MaybeUninit<u8>],
        matrix: Option<&[MaybeUninit<u8>]>,
        slices: &'d [T],
        data: F,
    ) -> Result<(), VaApiError>
    where
        F: Fn(usize) -> &'d [u8];

    /// NV12 bytes of `surface` cropped to `crop` `(x, y, width, height)`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver image is not NV12, when its
    /// layout overruns its bytes, or when any call fails.
    fn read_nv12(
        &self,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError>;

    /// Block until `surface` finishes decoding.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver reports the surface failed.
    fn sync(&self, surface: SurfaceId) -> Result<(), VaApiError>;

    /// Block until `surface` finishes, dropping any failure.
    fn sync_quiet(&self, surface: SurfaceId) {
        let _ = self.sync(surface);
    }

    /// True when `surface` already finished, without blocking.
    ///
    /// Errors read as unfinished; the later sync surfaces them.
    #[must_use]
    fn poll(&self, surface: SurfaceId) -> bool;

    /// Pool surfaces, borrowed from the pool.
    #[must_use]
    fn surfaces(&self) -> &[SurfaceId];

    /// Lease for pool slot `index`, keeping it out of the free set.
    #[must_use]
    fn surface_guard(&self, index: u32) -> Option<Arc<()>>;

    /// True when a live frame holds pool slot `index`.
    #[must_use]
    fn surface_live(&self, index: u32) -> bool;

    /// Pool size.
    #[must_use]
    fn pool_size(&self) -> usize;

    /// VA-API profile the config was built for.
    #[must_use]
    fn profile(&self) -> c_int;

    /// Coded size in luma samples.
    #[must_use]
    fn coded_size(&self) -> (u32, u32);
}
