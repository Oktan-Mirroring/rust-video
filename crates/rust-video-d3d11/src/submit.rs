//! Immediate-context submit lock plus per-slot deferred copy lanes.
//!
//! <https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-render>
//! <https://docs.rs/windows/0.62.2/windows/Win32/Graphics/Direct3D11/>

// FFI boundary: device-context COM calls live here.
#![allow(unsafe_code)]

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::error::{D3d11Error, UnsupportedReason};
use crate::h264::Crop;

use bytes::Bytes;
use windows::core::Interface;
use windows::Win32::Foundation::{FALSE, S_FALSE, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Asynchronous, ID3D11CommandList, ID3D11Device, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D, D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

/// Completion wait budget before the wait fails.
const QUERY_BUDGET: Duration = Duration::from_secs(5);

/// Single-submitter lock over the immediate context.
///
/// A single thread needs nothing: every crate submission takes this lock
/// internally. Threads sharing one device hold it around their own direct
/// context calls to serialize with crate submissions; waits never hold it.
#[derive(Debug, Default)]
pub struct SubmitLock {
    /// Held across immediate-context submissions, never across waits.
    inner: Mutex<()>,
}

impl SubmitLock {
    /// Unheld submit lock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(()),
        }
    }

    /// Submit right on the immediate context.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a previous holder panicked.
    pub fn enter(&self) -> Result<MutexGuard<'_, ()>, D3d11Error> {
        self.inner
            .lock()
            .map_err(|_| D3d11Error::Unsupported(UnsupportedReason::SubmitLockPoisoned))
    }
}

/// Per-slot copy lane: deferred recorder, staging, completion query.
pub(crate) struct CopySlot {
    /// Deferred recorder behind the copy command list.
    deferred: ID3D11DeviceContext,
    /// CPU-visible copy target.
    staging: ID3D11Texture2D,
    /// Event query behind completion waits.
    signal: ID3D11Asynchronous,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
}

impl CopySlot {
    /// Copy lane over `device` for `width` x `height` pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the deferred context,
    /// the staging texture, or the event query.
    pub(crate) fn open(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, D3d11Error> {
        let mut deferred = None;
        // Sound: the out-pointer stays live for the call; flags are zero.
        unsafe { device.CreateDeferredContext(0, Some(std::ptr::addr_of_mut!(deferred))) }
            .map_err(|error| D3d11Error::Call {
                call: "CreateDeferredContext",
                hresult: error.code().0,
            })?;
        let deferred = deferred.ok_or(D3d11Error::Call {
            call: "CreateDeferredContext",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        })?;
        let staging = create_staging(device, width, height)?;
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut query = None;
        // Sound: the descriptor and out-pointer stay live for the call.
        unsafe {
            device.CreateQuery(
                std::ptr::addr_of!(desc),
                Some(std::ptr::addr_of_mut!(query)),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateQuery",
            hresult: error.code().0,
        })?;
        let query: ID3D11Query = query.ok_or(D3d11Error::Call {
            call: "CreateQuery",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        })?;
        // Sound: the query is live; every query exposes the waitable face.
        let signal: ID3D11Asynchronous = query.cast().map_err(|error| D3d11Error::Call {
            call: "QueryInterface",
            hresult: error.code().0,
        })?;
        Ok(Self {
            deferred,
            staging,
            signal,
            width,
            height,
        })
    }

    /// Command list copying array `slot` into the staging texture.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the command list.
    pub(crate) fn record(
        &self,
        array: &ID3D11Texture2D,
        slot: u8,
    ) -> Result<ID3D11CommandList, D3d11Error> {
        // Sound: both textures are live with matching extents; the slot is a valid slice.
        unsafe {
            self.deferred.CopySubresourceRegion(
                &self.staging,
                0,
                0,
                0,
                0,
                array,
                u32::from(slot),
                None,
            );
        }
        let mut list = None;
        // Sound: the out-pointer stays live for the call.
        unsafe {
            self.deferred
                .FinishCommandList(false, Some(std::ptr::addr_of_mut!(list)))
        }
        .map_err(|error| D3d11Error::Call {
            call: "FinishCommandList",
            hresult: error.code().0,
        })?;
        list.ok_or(D3d11Error::Call {
            call: "FinishCommandList",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        })
    }

    /// `list` through the immediate context, ending the completion query.
    pub(crate) fn complete(&self, context: &ID3D11DeviceContext, list: &ID3D11CommandList) {
        // Sound: the list and query are live; the caller holds the submit lock.
        unsafe {
            context.ExecuteCommandList(list, true);
            context.End(&self.signal);
        }
    }

    /// Completion of the last list, outside the submit lock.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget or reports failure.
    pub(crate) fn wait(&self, context: &ID3D11DeviceContext) -> Result<(), D3d11Error> {
        wait_for_query(context, &self.signal)
    }

    /// True once the GPU passes the last list marker, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU reports failure.
    pub(crate) fn signaled(&self, context: &ID3D11DeviceContext) -> Result<bool, D3d11Error> {
        query_signaled(
            context,
            &self.signal,
            flag_word(D3D11_ASYNC_GETDATA_DONOTFLUSH.0),
        )
    }

    /// Completion marker behind the last submit to this lane.
    pub(crate) fn mark(&self, context: &ID3D11DeviceContext) {
        // Sound: the query is live; the caller holds the submit lock.
        unsafe {
            context.End(&self.signal);
        }
    }

    /// Completion fence behind this lane.
    pub(crate) fn fence(&self, context: &ID3D11DeviceContext) -> SurfaceFence {
        SurfaceFence {
            context: context.clone(),
            signal: self.signal.clone(),
        }
    }

    /// NV12 bytes from staging cropped to the visible rectangle.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the map fails.
    pub(crate) fn read(
        &self,
        context: &ID3D11DeviceContext,
        crop: Option<Crop>,
    ) -> Result<Bytes, D3d11Error> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // Sound: the staging texture is live with read access.
        unsafe {
            context.Map(
                &self.staging,
                0,
                D3D11_MAP_READ,
                0,
                Some(std::ptr::addr_of_mut!(mapped)),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "Map",
            hresult: error.code().0,
        })?;
        let bytes = copy_nv12(&mapped, self.width, self.height, crop);
        // Sound: the staging texture mapped above.
        unsafe {
            context.Unmap(&self.staging, 0);
        }
        bytes
    }
}

/// Completion fence behind one decode surface.
pub(crate) struct SurfaceFence {
    /// Immediate context behind the wait.
    context: ID3D11DeviceContext,
    /// Event query ended after the submission.
    signal: ID3D11Asynchronous,
}

impl SurfaceFence {
    /// Completion of the submission, outside the submit lock.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget or reports failure.
    pub(crate) fn wait(&self) -> Result<(), D3d11Error> {
        wait_for_query(&self.context, &self.signal)
    }
}

/// Completion of `signal` on `context` within the wait budget.
///
/// # Errors
///
/// Returns `Err` when the GPU misses the wait budget or reports failure.
fn wait_for_query(
    context: &ID3D11DeviceContext,
    signal: &ID3D11Asynchronous,
) -> Result<(), D3d11Error> {
    if query_signaled(context, signal, 0)? {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + QUERY_BUDGET;
    loop {
        if query_signaled(context, signal, flag_word(D3D11_ASYNC_GETDATA_DONOTFLUSH.0))? {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::hint::spin_loop();
    }
    Err(D3d11Error::Call {
        call: "QueryWait",
        hresult: WAIT_TIMEOUT.0.cast_signed(),
    })
}

/// True once the GPU passes the query end marker.
fn query_signaled(
    context: &ID3D11DeviceContext,
    signal: &ID3D11Asynchronous,
    flags: u32,
) -> Result<bool, D3d11Error> {
    let mut done = FALSE;
    // Sound: the query is live; the flag word stays valid for the call.
    let outcome = unsafe {
        context.GetData(
            signal,
            Some(std::ptr::addr_of_mut!(done).cast()),
            u32::try_from(std::mem::size_of_val(&done)).unwrap_or(0),
            flags,
        )
    };
    match outcome {
        Ok(()) => Ok(done.as_bool()),
        Err(error) if error.code() == S_FALSE => Ok(false),
        Err(error) => Err(D3d11Error::Call {
            call: "GetData",
            hresult: error.code().0,
        }),
    }
}

/// CPU-visible texture behind one copy lane.
fn create_staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, D3d11Error> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: flag_word(D3D11_CPU_ACCESS_READ.0),
        MiscFlags: 0,
    };
    let mut staging = None;
    // Sound: the descriptor stays live for the call.
    unsafe {
        device.CreateTexture2D(
            std::ptr::addr_of!(desc),
            None,
            Some(std::ptr::addr_of_mut!(staging)),
        )
    }
    .map_err(|error| D3d11Error::Call {
        call: "CreateTexture2D",
        hresult: error.code().0,
    })?;
    staging.ok_or(D3d11Error::Call {
        call: "CreateTexture2D",
        hresult: windows::Win32::Foundation::E_FAIL.0,
    })
}

/// NV12 bytes from a mapped staging texture under `crop`.
fn copy_nv12(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
    crop: Option<Crop>,
) -> Result<Bytes, D3d11Error> {
    if mapped.RowPitch < width {
        return Err(D3d11Error::Call {
            call: "Map",
            hresult: windows::Win32::Foundation::E_FAIL.0,
        });
    }
    let (left, top, visible_width, visible_height) = visible_rect(width, height, crop);
    let pitch = usize::try_from(mapped.RowPitch).unwrap_or(0);
    let left = usize::try_from(left).unwrap_or(0);
    let top = usize::try_from(top).unwrap_or(0);
    let visible_width = usize::try_from(visible_width).unwrap_or(0);
    let visible_height = usize::try_from(visible_height).unwrap_or(0);
    let height = usize::try_from(height).unwrap_or(0);
    let capacity = visible_width
        .saturating_mul(visible_height)
        .saturating_mul(3)
        / 2;
    let mut bytes = Vec::with_capacity(capacity);
    // Sound: the pitch and extents bound every row inside the mapped staging texture.
    unsafe {
        let base = mapped.pData.cast::<u8>();
        if pitch == visible_width && left == 0 {
            let y_src = base.add(top * pitch);
            bytes.extend_from_slice(std::slice::from_raw_parts(
                y_src,
                visible_width * visible_height,
            ));
            let chroma = base.add(height * pitch + top * pitch / 2);
            bytes.extend_from_slice(std::slice::from_raw_parts(
                chroma,
                visible_width * (visible_height / 2),
            ));
            return Ok(Bytes::from(bytes));
        }
        for row in 0..visible_height {
            let src = base.add(top * pitch + row * pitch + left);
            bytes.extend_from_slice(std::slice::from_raw_parts(src, visible_width));
        }
        let chroma = base.add(height * pitch);
        for row in 0..visible_height / 2 {
            let src = chroma.add(top * pitch / 2 + row * pitch + left);
            bytes.extend_from_slice(std::slice::from_raw_parts(src, visible_width));
        }
    }
    Ok(Bytes::from(bytes))
}

/// Visible rectangle inside the coded extent.
fn visible_rect(width: u32, height: u32, crop: Option<Crop>) -> (u32, u32, u32, u32) {
    crop.map_or((0, 0, width, height), |crop| {
        (
            crop.left,
            crop.top,
            width - crop.left - crop.right,
            height - crop.top - crop.bottom,
        )
    })
}

/// Flag bits as the descriptor word.
pub(crate) fn flag_word(bits: i32) -> u32 {
    u32::try_from(bits).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{copy_nv12, visible_rect};

    use crate::h264::Crop;

    use windows::Win32::Graphics::Direct3D11::D3D11_MAPPED_SUBRESOURCE;

    /// Mapped view over `bytes` with `pitch`.
    fn mapped(bytes: &mut [u8], pitch: u32) -> D3D11_MAPPED_SUBRESOURCE {
        D3D11_MAPPED_SUBRESOURCE {
            pData: bytes.as_mut_ptr().cast(),
            RowPitch: pitch,
            DepthPitch: 0,
        }
    }

    #[test]
    fn copy_strips_pitch_padding() {
        let mut staged = vec![
            1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0, 9, 10, 11, 12, 0, 0, 0, 0,
        ];
        let view = mapped(&mut staged, 8);
        let tight = copy_nv12(&view, 4, 2, None).expect("tight");
        assert_eq!(tight.as_ref(), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn copy_applies_crop() {
        let mut staged = vec![
            1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0, 9, 10, 11, 12, 0, 0, 0, 0,
        ];
        let view = mapped(&mut staged, 8);
        let crop = Crop {
            left: 2,
            right: 0,
            top: 0,
            bottom: 0,
        };
        let tight = copy_nv12(&view, 4, 2, Some(crop)).expect("tight");
        assert_eq!(tight.as_ref(), &[3, 4, 7, 8, 11, 12]);
    }

    #[test]
    fn copy_tight_pitch() {
        let mut staged: Vec<u8> = (1..=24).collect();
        let view = mapped(&mut staged, 4);
        let tight = copy_nv12(&view, 4, 4, None).expect("tight");
        assert_eq!(tight.as_ref(), &(1..=24).collect::<Vec<u8>>());
    }

    #[test]
    fn copy_tight_pitch_applies_crop() {
        let mut staged: Vec<u8> = (1..=24).collect();
        let view = mapped(&mut staged, 4);
        let crop = Crop {
            left: 0,
            right: 0,
            top: 2,
            bottom: 0,
        };
        let tight = copy_nv12(&view, 4, 4, Some(crop)).expect("tight");
        assert_eq!(
            tight.as_ref(),
            &[9, 10, 11, 12, 13, 14, 15, 16, 21, 22, 23, 24]
        );
    }

    #[test]
    fn copy_rejects_narrow_pitch() {
        let mut staged = vec![0; 12];
        let view = mapped(&mut staged, 2);
        assert!(copy_nv12(&view, 4, 2, None).is_err());
    }

    #[test]
    fn visible_rect_insets_crop() {
        assert_eq!(visible_rect(1920, 1088, None), (0, 0, 1920, 1088));
        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        assert_eq!(visible_rect(1920, 1088, Some(crop)), (0, 0, 1920, 1080));
    }
}
