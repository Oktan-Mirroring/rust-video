//! GPU submit plumbing shared by decode and encode.
//! Fence waits, resource creation, pitched NV12 transfers.
//!
//! <https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12fence-seteventoncompletion>
//! <https://docs.rs/windows/0.62.2/windows/Win32/Graphics/Direct3D12/>

// FFI boundary: queue, fence, and resource COM calls live here.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::time::Duration;

use crate::error::{D3d12Error, UnsupportedReason};
use rust_video_caps::reason::UnsupportedReason as SharedUnsupported;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Device, ID3D12DeviceChild,
    ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource, D3D12_COMMAND_LIST_TYPE,
    D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
    D3D12_FENCE_FLAG_NONE, D3D12_HEAP_FLAGS, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_FLAG_SHARED,
    D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE, D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN,
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RANGE, D3D12_RESOURCE_BARRIER,
    D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_NONE,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_TRANSITION_BARRIER,
    D3D12_TEXTURE_COPY_LOCATION, D3D12_TEXTURE_COPY_LOCATION_0,
    D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT, D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
    D3D12_TEXTURE_LAYOUT_ROW_MAJOR, D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_NV12, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Media::MediaFoundation::{
    ID3D12VideoDecodeCommandList, ID3D12VideoEncodeCommandList2, ID3D12VideoProcessCommandList,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Command list behind one submit slot.
pub(crate) trait SlotList: Interface {
    /// List reset over `allocator`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the reset.
    fn reset_list(&self, allocator: &ID3D12CommandAllocator) -> Result<(), D3d12Error>;

    /// List close before execution.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the close.
    fn close_list(&self) -> Result<(), D3d12Error>;
}

impl SlotList for ID3D12VideoDecodeCommandList {
    fn reset_list(&self, allocator: &ID3D12CommandAllocator) -> Result<(), D3d12Error> {
        // Sound: the allocator outlives the reset.
        unsafe { self.Reset(allocator) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })
    }

    fn close_list(&self) -> Result<(), D3d12Error> {
        // Sound: the list holds only recorded barriers and work.
        unsafe { self.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })
    }
}

impl SlotList for ID3D12VideoEncodeCommandList2 {
    fn reset_list(&self, allocator: &ID3D12CommandAllocator) -> Result<(), D3d12Error> {
        // Sound: the allocator outlives the reset.
        unsafe { self.Reset(allocator) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })
    }

    fn close_list(&self) -> Result<(), D3d12Error> {
        // Sound: the list holds only recorded barriers and work.
        unsafe { self.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })
    }
}

impl SlotList for ID3D12VideoProcessCommandList {
    fn reset_list(&self, allocator: &ID3D12CommandAllocator) -> Result<(), D3d12Error> {
        // Sound: the allocator outlives the reset.
        unsafe { self.Reset(allocator) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })
    }

    fn close_list(&self) -> Result<(), D3d12Error> {
        // Sound: the list holds only recorded barriers and work.
        unsafe { self.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })
    }
}

impl SlotList for ID3D12GraphicsCommandList {
    fn reset_list(&self, allocator: &ID3D12CommandAllocator) -> Result<(), D3d12Error> {
        // Sound: the allocator outlives the reset; no initial pipeline.
        unsafe { self.Reset(allocator, None) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })
    }

    fn close_list(&self) -> Result<(), D3d12Error> {
        // Sound: the list holds only recorded barriers and work.
        unsafe { self.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })
    }
}

/// Pitched plane behind NV12 transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Nv12Plane {
    /// Byte offset in the staging buffer.
    pub(crate) offset: u64,
    /// Row stride in bytes.
    pub(crate) pitch: u32,
    /// Row count.
    pub(crate) rows: u32,
}

/// Staging layout for one NV12 texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Nv12Layout {
    /// Luma then chroma planes.
    pub(crate) planes: [Nv12Plane; 2],
    /// Staging buffer size in bytes.
    pub(crate) total: u64,
}

impl Nv12Layout {
    /// Tight byte count under `crop` at `width` by `height`.
    pub(crate) fn tight_len(
        width: u32,
        height: u32,
        crop: Option<crate::h264::frontend::Crop>,
    ) -> usize {
        let (_, _, visible_width, visible_height) = visible_rect(width, height, crop);
        usize::try_from(
            u64::from(visible_width)
                .saturating_mul(u64::from(visible_height))
                .saturating_mul(3)
                / 2,
        )
        .unwrap_or(usize::MAX)
        .min(isize::MAX as usize)
    }

    /// Tight NV12 bytes from pitched `staged` bytes under `crop`.
    pub(crate) fn tighten(
        &self,
        staged: &[u8],
        width: u32,
        height: u32,
        crop: Option<crate::h264::frontend::Crop>,
    ) -> Result<Vec<u8>, D3d12Error> {
        let mut out = Vec::with_capacity(Self::tight_len(width, height, crop));
        self.tighten_into(staged, width, height, crop, &mut out)?;
        Ok(out)
    }

    /// Pitched `staged` bytes tightened into `out` under `crop`.
    pub(crate) fn tighten_into(
        &self,
        staged: &[u8],
        width: u32,
        height: u32,
        crop: Option<crate::h264::frontend::Crop>,
        out: &mut Vec<u8>,
    ) -> Result<(), D3d12Error> {
        let (left, top, visible_width, visible_height) = visible_rect(width, height, crop);
        let y_len = usize::try_from(u64::from(visible_width) * u64::from(visible_height))
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        out.clear();
        out.reserve(y_len + y_len / 2);
        for (plane, index) in self.planes.iter().zip([0, 1]) {
            let (plane_left, plane_top, plane_width, plane_rows) = if index == 0 {
                (left, top, visible_width, visible_height)
            } else {
                (left, top / 2, visible_width, visible_height / 2)
            };
            copy_plane(
                staged,
                plane,
                plane_left,
                plane_top,
                plane_width,
                plane_rows,
                &mut *out,
            )?;
        }
        Ok(())
    }

    /// Tight `nv12` bytes pitched into `staged`.
    pub(crate) fn stride_into(
        &self,
        nv12: &[u8],
        width: u32,
        height: u32,
        staged: &mut [u8],
    ) -> Result<(), D3d12Error> {
        let y_len = usize::try_from(u64::from(width) * u64::from(height))
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        if nv12.len() < y_len + y_len / 2 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
                SharedUnsupported::Nv12ShortOfExtent,
            )));
        }
        let total = usize::try_from(self.total)
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        if staged.len() < total {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::StagingShortOfLayout,
            ));
        }
        let mut src = 0usize;
        for (plane, rows) in self.planes.iter().zip([height, height / 2]) {
            let pitch = usize::try_from(plane.pitch).unwrap_or(0);
            let offset = usize::try_from(plane.offset).unwrap_or(usize::MAX);
            let width = usize::try_from(width).unwrap_or(0);
            let rows = usize::try_from(rows).unwrap_or(0);
            for row in 0..rows {
                let dst = offset.saturating_add(row.saturating_mul(pitch));
                let end = dst.saturating_add(width);
                if end > staged.len() || src.saturating_add(width) > nv12.len() {
                    return Err(D3d12Error::Unsupported(
                        UnsupportedReason::StagingShortOfLayout,
                    ));
                }
                staged[dst..end].copy_from_slice(&nv12[src..src + width]);
                src += width;
            }
        }
        Ok(())
    }
}

/// Visible rectangle in luma samples under `crop`.
fn visible_rect(
    width: u32,
    height: u32,
    crop: Option<crate::h264::frontend::Crop>,
) -> (u32, u32, u32, u32) {
    crop.map_or((0, 0, width, height), |crop| {
        (
            crop.left,
            crop.top,
            width.saturating_sub(crop.left.saturating_add(crop.right)),
            height.saturating_sub(crop.top.saturating_add(crop.bottom)),
        )
    })
}

/// Pitched `plane` rows from `staged` into `out`.
fn copy_plane(
    staged: &[u8],
    plane: &Nv12Plane,
    left: u32,
    top: u32,
    width: u32,
    rows: u32,
    out: &mut Vec<u8>,
) -> Result<(), D3d12Error> {
    let pitch = usize::try_from(plane.pitch).unwrap_or(0);
    let offset = usize::try_from(plane.offset).unwrap_or(usize::MAX);
    let left = usize::try_from(left).unwrap_or(usize::MAX);
    let top = usize::try_from(top).unwrap_or(usize::MAX);
    let width = usize::try_from(width).unwrap_or(usize::MAX);
    let rows = usize::try_from(rows).unwrap_or(usize::MAX);
    for row in 0..rows {
        let src = offset
            .saturating_add(top.saturating_add(row).saturating_mul(pitch))
            .saturating_add(left);
        let end = src.saturating_add(width);
        if end > staged.len() {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::StagingShortOfLayout,
            ));
        }
        out.extend_from_slice(&staged[src..end]);
    }
    Ok(())
}

/// Logical device behind resource creation.
pub(crate) struct DeviceResources {
    /// Logical device.
    device: ID3D12Device,
}

impl DeviceResources {
    /// Probe texture extent in luma samples.
    const PROBE_EXTENT: u32 = 64;

    /// Resources over `device`.
    pub(crate) fn new(device: &ID3D12Device) -> Self {
        Self {
            device: device.clone(),
        }
    }

    /// True when the driver shares a probe texture through an NT handle.
    pub(crate) fn probe_shared_export(&self) -> bool {
        let desc = Self::texture_desc(Self::PROBE_EXTENT, Self::PROBE_EXTENT);
        let Ok(resource) = self.committed_shared(
            D3D12_HEAP_TYPE_DEFAULT,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            "ProbeShare",
        ) else {
            return false;
        };
        // Sound: the resource is live; every resource exposes the device-child face.
        let Ok(child) = resource.cast::<ID3D12DeviceChild>() else {
            return false;
        };
        // Sound: the device and resource are live; default security, unnamed.
        let Ok(handle) = (unsafe {
            self.device
                .CreateSharedHandle(&child, None, GENERIC_ALL.0, PCWSTR::null())
        }) else {
            return false;
        };
        if handle.is_invalid() {
            return false;
        }
        // Sound: the handle is live; the close ends the export.
        let _ = unsafe { CloseHandle(handle) };
        true
    }

    /// Queue of `queue_type` behind submissions.
    pub(crate) fn queue(
        &self,
        queue_type: D3D12_COMMAND_LIST_TYPE,
    ) -> Result<ID3D12CommandQueue, D3d12Error> {
        let desc = D3D12_COMMAND_QUEUE_DESC {
            Type: queue_type,
            Priority: 0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        };
        // Sound: the descriptor outlives the call.
        unsafe { self.device.CreateCommandQueue(std::ptr::addr_of!(desc)) }.map_err(|error| {
            D3d12Error::Call {
                call: "CreateCommandQueue",
                hresult: error.code().0,
            }
        })
    }

    /// Allocator behind a `queue_type` list.
    pub(crate) fn allocator(
        &self,
        queue_type: D3D12_COMMAND_LIST_TYPE,
    ) -> Result<ID3D12CommandAllocator, D3d12Error> {
        // Sound: no inputs beyond the list type.
        unsafe { self.device.CreateCommandAllocator(queue_type) }.map_err(|error| {
            D3d12Error::Call {
                call: "CreateCommandAllocator",
                hresult: error.code().0,
            }
        })
    }

    /// Command list of `queue_type` over `allocator`.
    pub(crate) fn command_list<T>(
        &self,
        queue_type: D3D12_COMMAND_LIST_TYPE,
        allocator: &ID3D12CommandAllocator,
    ) -> Result<T, D3d12Error>
    where
        T: windows::core::Interface,
    {
        // Sound: the allocator outlives the call; no initial pipeline.
        unsafe {
            self.device
                .CreateCommandList(0, queue_type, allocator, None)
        }
        .map_err(|error| D3d12Error::Call {
            call: "CreateCommandList",
            hresult: error.code().0,
        })
    }

    /// Committed resource of `desc` on a `heap` type.
    pub(crate) fn committed(
        &self,
        heap: D3D12_HEAP_TYPE,
        desc: &D3D12_RESOURCE_DESC,
        state: D3D12_RESOURCE_STATES,
        call: &'static str,
    ) -> Result<ID3D12Resource, D3d12Error> {
        self.committed_flags(heap, D3D12_HEAP_FLAG_NONE, desc, state, call)
    }

    /// Committed shareable resource of `desc` on a `heap` type.
    pub(crate) fn committed_shared(
        &self,
        heap: D3D12_HEAP_TYPE,
        desc: &D3D12_RESOURCE_DESC,
        state: D3D12_RESOURCE_STATES,
        call: &'static str,
    ) -> Result<ID3D12Resource, D3d12Error> {
        self.committed_flags(heap, D3D12_HEAP_FLAG_SHARED, desc, state, call)
    }

    /// Committed resource of `desc` on a `heap` type with `flags`.
    fn committed_flags(
        &self,
        heap: D3D12_HEAP_TYPE,
        flags: D3D12_HEAP_FLAGS,
        desc: &D3D12_RESOURCE_DESC,
        state: D3D12_RESOURCE_STATES,
        call: &'static str,
    ) -> Result<ID3D12Resource, D3d12Error> {
        let props = Self::heap_props(heap);
        let mut out: Option<ID3D12Resource> = None;
        // Sound: `props` and `desc` outlive the call; `out` receives the resource.
        unsafe {
            self.device.CreateCommittedResource(
                std::ptr::addr_of!(props),
                flags,
                std::ptr::from_ref(desc),
                state,
                None,
                std::ptr::addr_of_mut!(out),
            )
        }
        .map_err(|error| D3d12Error::Call {
            call,
            hresult: error.code().0,
        })?;
        out.ok_or(D3d12Error::Unsupported(
            UnsupportedReason::CallReturnedNull { call },
        ))
    }

    /// Pitch layout plus driver footprints for a `width` by `height` NV12 texture.
    pub(crate) fn nv12_layout(
        &self,
        width: u32,
        height: u32,
    ) -> Result<(Nv12Layout, [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; 2]), D3d12Error> {
        let desc = Self::texture_desc(width, height);
        let mut footprints = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 2];
        let mut rows = [0u32; 2];
        let mut row_bytes = [0u64; 2];
        let mut total = 0u64;
        // Sound: the descriptor and out-arrays outlive the call; NV12 carries two planes.
        unsafe {
            self.device.GetCopyableFootprints(
                std::ptr::addr_of!(desc),
                0,
                2,
                0,
                Some(footprints.as_mut_ptr()),
                Some(rows.as_mut_ptr()),
                Some(row_bytes.as_mut_ptr()),
                Some(std::ptr::addr_of_mut!(total)),
            );
        }
        if total == 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::EmptyNv12Layout));
        }
        let planes = [
            Nv12Plane {
                offset: footprints[0].Offset,
                pitch: footprints[0].Footprint.RowPitch,
                rows: rows[0],
            },
            Nv12Plane {
                offset: footprints[1].Offset,
                pitch: footprints[1].Footprint.RowPitch,
                rows: rows[1],
            },
        ];
        let end = |plane: &Nv12Plane| {
            plane
                .offset
                .saturating_add(u64::from(plane.pitch).saturating_mul(u64::from(plane.rows)))
        };
        let span = end(&planes[0]).max(end(&planes[1])).max(total);
        let layout = Nv12Layout {
            planes,
            total: span,
        };
        Ok((layout, footprints))
    }

    /// Heap properties for a `heap` type.
    pub(crate) fn heap_props(heap: D3D12_HEAP_TYPE) -> D3D12_HEAP_PROPERTIES {
        D3D12_HEAP_PROPERTIES {
            Type: heap,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        }
    }

    /// Buffer descriptor of `size` bytes.
    pub(crate) fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        }
    }

    /// NV12 texture descriptor of `width` by `height`.
    pub(crate) fn texture_desc(width: u32, height: u32) -> D3D12_RESOURCE_DESC {
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        }
    }

    /// Transition barrier for `resource` between states.
    pub(crate) fn barrier(
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(resource.clone())),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    /// Copy location for `subresource` of `resource`.
    pub(crate) fn subresource_location(
        resource: &ID3D12Resource,
        subresource: u32,
    ) -> D3D12_TEXTURE_COPY_LOCATION {
        D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: subresource,
            },
        }
    }

    /// Copy location for `placed` in `resource`.
    pub(crate) fn footprint_location(
        resource: &ID3D12Resource,
        placed: &D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    ) -> D3D12_TEXTURE_COPY_LOCATION {
        D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: *placed,
            },
        }
    }

    /// `bytes` into the upload mapping.
    pub(crate) fn write_upload(upload: &ID3D12Resource, bytes: &[u8]) -> Result<(), D3d12Error> {
        Self::with_upload(upload, bytes.len(), |mapped| {
            mapped.copy_from_slice(bytes);
            Ok(())
        })
    }

    /// `f` over the `len`-byte upload mapping.
    pub(crate) fn with_upload<T>(
        upload: &ID3D12Resource,
        len: usize,
        f: impl FnOnce(&mut [u8]) -> Result<T, D3d12Error>,
    ) -> Result<T, D3d12Error> {
        let mut mapped: *mut c_void = std::ptr::null_mut();
        // Sound: the upload mapping outlives `f` below.
        unsafe { upload.Map(0, None, Some(std::ptr::addr_of_mut!(mapped))) }.map_err(|error| {
            D3d12Error::Call {
                call: "Map",
                hresult: error.code().0,
            }
        })?;
        if mapped.is_null() {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::UploadMapReturnedNull,
            ));
        }
        // Sound: `mapped` spans `len` upload bytes; the mapping outlives `f`.
        let mapped = unsafe { std::slice::from_raw_parts_mut(mapped.cast::<u8>(), len) };
        let out = f(mapped);
        // Sound: the mapping is live; `out` owns its bytes.
        unsafe {
            upload.Unmap(0, None);
        }
        out
    }

    /// `f` over the `len`-byte readback mapping.
    pub(crate) fn with_readback<T>(
        readback: &ID3D12Resource,
        len: usize,
        f: impl FnOnce(&[u8]) -> Result<T, D3d12Error>,
    ) -> Result<T, D3d12Error> {
        let range = D3D12_RANGE { Begin: 0, End: len };
        let mut read: *mut c_void = std::ptr::null_mut();
        // Sound: the readback mapping outlives `f` below.
        unsafe {
            readback.Map(
                0,
                Some(std::ptr::addr_of!(range)),
                Some(std::ptr::addr_of_mut!(read)),
            )
        }
        .map_err(|error| D3d12Error::Call {
            call: "Map",
            hresult: error.code().0,
        })?;
        if read.is_null() {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ReadbackMapReturnedNull,
            ));
        }
        // Sound: `read` spans `len` readback bytes; the mapping outlives `f`.
        let mapped = unsafe { std::slice::from_raw_parts(read.cast::<u8>(), len) };
        let out = f(mapped);
        // Sound: the mapping is live; `out` owns its bytes.
        unsafe {
            readback.Unmap(0, None);
        }
        out
    }

    /// `len`-byte mapping pointer behind `readback`, released separately.
    pub(crate) fn map_readback(
        readback: &ID3D12Resource,
        len: usize,
    ) -> Result<*const u8, D3d12Error> {
        let range = D3D12_RANGE { Begin: 0, End: len };
        let mut read: *mut c_void = std::ptr::null_mut();
        // Sound: the caller releases the mapping after its copy lands.
        unsafe {
            readback.Map(
                0,
                Some(std::ptr::addr_of!(range)),
                Some(std::ptr::addr_of_mut!(read)),
            )
        }
        .map_err(|error| D3d12Error::Call {
            call: "Map",
            hresult: error.code().0,
        })?;
        if read.is_null() {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ReadbackMapReturnedNull,
            ));
        }
        Ok(read.cast_const().cast::<u8>())
    }

    /// Release a mapping from [`map_readback`](Self::map_readback).
    pub(crate) fn unmap_readback(readback: &ID3D12Resource) {
        // Sound: the mapping is live; the copy already landed.
        unsafe {
            readback.Unmap(0, None);
        }
    }
}

/// Completion ticket behind one queue submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ticket {
    /// Fence value the queue signals.
    value: u64,
}

impl Ticket {
    /// Fence value the queue signals.
    #[must_use]
    pub(crate) fn value(self) -> u64 {
        self.value
    }
}

/// Fence behind queue waits.
pub(crate) struct Fence {
    /// Fence behind queue waits.
    inner: ID3D12Fence,
    /// Monotonic fence value.
    value: u64,
    /// Completion event behind waits.
    event: HANDLE,
}

impl Fence {
    /// Fence completion wait budget in milliseconds.
    const WAIT_MS: u32 = 5_000;

    /// Fence over `device`.
    pub(crate) fn open(device: &ID3D12Device) -> Result<Self, D3d12Error> {
        // Sound: no inputs beyond the initial value and flags.
        let fence: ID3D12Fence =
            unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.map_err(|error| {
                D3d12Error::Call {
                    call: "CreateFence",
                    hresult: error.code().0,
                }
            })?;
        // Sound: default security, auto-reset, unnamed.
        let event =
            unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(|error| {
                D3d12Error::Call {
                    call: "CreateEvent",
                    hresult: error.code().0,
                }
            })?;
        Ok(Self {
            inner: fence,
            value: 0,
            event,
        })
    }

    /// Closed `command` through `queue`, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the queue refuses the signal.
    pub(crate) fn execute(
        &mut self,
        queue: &ID3D12CommandQueue,
        command: &ID3D12CommandList,
    ) -> Result<Ticket, D3d12Error> {
        // Sound: the command and queue outlive the call.
        unsafe {
            queue.ExecuteCommandLists(&[Some(command.clone())]);
        }
        self.signal(queue)
    }

    /// True once the GPU signals `ticket`.
    pub(crate) fn poll(&self, ticket: Ticket) -> bool {
        // Sound: polling a live fence for a signalled value.
        let completed = unsafe { self.inner.GetCompletedValue() };
        completed >= ticket.value()
    }

    /// `queue` signaled past the last submission.
    pub(crate) fn signal(&mut self, queue: &ID3D12CommandQueue) -> Result<Ticket, D3d12Error> {
        self.value = self.value.wrapping_add(1);
        let value = self.value;
        // Sound: the fence is live; the value is monotonic.
        unsafe { queue.Signal(&self.inner, value) }.map_err(|error| D3d12Error::Call {
            call: "Signal",
            hresult: error.code().0,
        })?;
        Ok(Ticket { value })
    }

    /// Owned fence handle behind surface waits.
    pub(crate) fn handle(&self) -> ID3D12Fence {
        self.inner.clone()
    }

    /// `ticket` completion through the fence event, outside any lock.
    pub(crate) fn wait(&self, ticket: Ticket) -> Result<(), D3d12Error> {
        if self.poll(ticket) {
            return Ok(());
        }
        Self::signal_and_wait(&self.inner, ticket.value(), self.event)
    }

    /// `ticket` completion through the fence event before `timeout`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the event call fails.
    ///
    /// # Arguments
    ///
    /// * `ticket` - fence value the queue signals.
    /// * `timeout` - wait budget.
    ///
    /// # Returns
    ///
    /// True when the GPU signaled `ticket` before `timeout` lapsed.
    pub(crate) fn wait_timeout(
        &self,
        ticket: Ticket,
        timeout: Duration,
    ) -> Result<bool, D3d12Error> {
        if self.poll(ticket) {
            return Ok(true);
        }
        let budget = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        Self::signal_and_wait_timeout(&self.inner, ticket.value(), self.event, budget)
    }

    /// `value` completion on `fence` through a transient event.
    pub(crate) fn wait_on(fence: &ID3D12Fence, value: u64) -> Result<(), D3d12Error> {
        // Sound: default security, auto-reset, unnamed.
        let event =
            unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(|error| {
                D3d12Error::Call {
                    call: "CreateEvent",
                    hresult: error.code().0,
                }
            })?;
        let outcome = Self::signal_and_wait(fence, value, event);
        // Sound: the event is live; the close owns no further state.
        let _ = unsafe { CloseHandle(event) };
        outcome
    }

    /// `value` completion on `fence` through `event`.
    fn signal_and_wait(fence: &ID3D12Fence, value: u64, event: HANDLE) -> Result<(), D3d12Error> {
        if Self::signal_and_wait_timeout(fence, value, event, Self::WAIT_MS)? {
            return Ok(());
        }
        Err(D3d12Error::Call {
            call: "FenceWait",
            hresult: WAIT_TIMEOUT.0.cast_signed(),
        })
    }

    /// `value` completion on `fence` through `event` before `timeout_ms`.
    fn signal_and_wait_timeout(
        fence: &ID3D12Fence,
        value: u64,
        event: HANDLE,
        timeout_ms: u32,
    ) -> Result<bool, D3d12Error> {
        // Sound: the fence and event are live.
        unsafe { fence.SetEventOnCompletion(value, event) }.map_err(|error| D3d12Error::Call {
            call: "SetEventOnCompletion",
            hresult: error.code().0,
        })?;
        // Sound: the event is live with a bounded wait.
        let outcome = unsafe { WaitForSingleObject(event, timeout_ms) };
        if outcome == WAIT_OBJECT_0 {
            return Ok(true);
        }
        if outcome == WAIT_TIMEOUT {
            return Ok(false);
        }
        Err(D3d12Error::Call {
            call: "WaitForSingleObject",
            hresult: outcome.0.cast_signed(),
        })
    }
}

impl Drop for Fence {
    fn drop(&mut self) {
        // Sound: the event is live; the close owns no further state.
        let _ = unsafe { CloseHandle(self.event) };
    }
}

/// Allocator plus list behind one submit slot.
pub(crate) struct CommandSlot<L> {
    /// Allocator behind the list.
    allocator: ID3D12CommandAllocator,
    /// List behind submissions.
    list: L,
}

impl<L: SlotList> CommandSlot<L> {
    /// Slot over `resources` for a `queue_type` queue.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the allocator, the list,
    /// or the fresh-list close.
    pub(crate) fn open(
        resources: &DeviceResources,
        queue_type: D3D12_COMMAND_LIST_TYPE,
    ) -> Result<Self, D3d12Error> {
        let allocator = resources.allocator(queue_type)?;
        let list: L = resources.command_list(queue_type, &allocator)?;
        let slot = Self { allocator, list };
        // Sound: the fresh list is open with no recorded work.
        slot.list.close_list()?;
        Ok(slot)
    }

    /// List behind submissions.
    pub(crate) fn list(&self) -> &L {
        &self.list
    }

    /// Pair reset for one submission.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the reset.
    pub(crate) fn reset(&mut self) -> Result<(), D3d12Error> {
        // Sound: the slot is idle; reset reuses the allocator and list.
        unsafe { self.allocator.Reset() }.map_err(|error| D3d12Error::Call {
            call: "ResetAllocator",
            hresult: error.code().0,
        })?;
        self.list.reset_list(&self.allocator)
    }

    /// Closed list through `queue`, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the close, the cast, or the
    /// signal. Close and cast failures reset the pair first; a signal
    /// failure leaves submitted work in flight for the session drop.
    pub(crate) fn close_execute(
        &mut self,
        queue: &ID3D12CommandQueue,
        fence: &mut Fence,
    ) -> Result<Ticket, D3d12Error> {
        if let Err(error) = self.list.close_list() {
            let _ = self.reset();
            return Err(error);
        }
        // Sound: the list is closed; the cast holds the live list.
        let command: ID3D12CommandList = match self.list.cast() {
            Ok(command) => command,
            Err(error) => {
                let _ = self.reset();
                return Err(D3d12Error::Call {
                    call: "CastCommandList",
                    hresult: error.code().0,
                });
            }
        };
        fence.execute(queue, &command)
    }
}

#[cfg(test)]
mod tests {
    use super::{Nv12Layout, Nv12Plane};

    /// Layout over a 4x2 frame with padded pitches.
    fn layout() -> Nv12Layout {
        Nv12Layout {
            planes: [
                Nv12Plane {
                    offset: 0,
                    pitch: 8,
                    rows: 2,
                },
                Nv12Plane {
                    offset: 16,
                    pitch: 8,
                    rows: 1,
                },
            ],
            total: 24,
        }
    }

    #[test]
    fn tighten_strips_pitch_padding() {
        let staged = vec![
            1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0, 9, 10, 11, 12, 0, 0, 0, 0,
        ];
        let tight = layout().tighten(&staged, 4, 2, None).expect("tight");
        assert_eq!(tight, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn tighten_applies_crop() {
        let staged = vec![
            1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0, 9, 10, 11, 12, 0, 0, 0, 0,
        ];
        let crop = crate::h264::frontend::Crop {
            left: 2,
            right: 0,
            top: 0,
            bottom: 0,
        };
        let tight = layout().tighten(&staged, 4, 2, Some(crop)).expect("tight");
        assert_eq!(tight, vec![3, 4, 7, 8, 11, 12]);
    }

    #[test]
    fn tighten_rejects_short_staging() {
        assert!(layout().tighten(&[0; 10], 4, 2, None).is_err());
    }

    #[test]
    fn stride_into_round_trips_tight_bytes() {
        let tight: Vec<u8> = (1..=12).collect();
        let mut staged = vec![0u8; 24];
        layout()
            .stride_into(&tight, 4, 2, &mut staged)
            .expect("staged");
        assert_eq!(staged.len(), 24);
        let back = layout().tighten(&staged, 4, 2, None).expect("tight");
        assert_eq!(back, tight);
    }

    #[test]
    fn stride_into_rejects_short_input() {
        let mut staged = vec![0u8; 24];
        assert!(layout().stride_into(&[0; 11], 4, 2, &mut staged).is_err());
    }

    #[test]
    fn stride_into_rejects_short_staging() {
        let tight: Vec<u8> = (1..=12).collect();
        let mut staged = vec![0u8; 10];
        assert!(layout().stride_into(&tight, 4, 2, &mut staged).is_err());
    }
}
