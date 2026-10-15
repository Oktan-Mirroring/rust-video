//! GPU submit plumbing shared by decode and encode.
//! Fence waits, resource creation, pitched NV12 transfers.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::ffi::c_void;
#[cfg(target_os = "windows")]
use std::mem::ManuallyDrop;

use crate::error::D3d12Error;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::WAIT_TIMEOUT;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Device, ID3D12Fence,
    ID3D12Resource, D3D12_COMMAND_LIST_TYPE, D3D12_COMMAND_QUEUE_DESC,
    D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_FENCE_FLAG_NONE,
    D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE, D3D12_MEMORY_POOL_UNKNOWN,
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RANGE, D3D12_RESOURCE_BARRIER,
    D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_NONE,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, D3D12_TEXTURE_COPY_LOCATION,
    D3D12_TEXTURE_COPY_LOCATION_0, D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
    D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
    D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_NV12, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};

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
    /// Tight NV12 bytes from pitched `staged` bytes under `crop`.
    pub(crate) fn tighten(
        &self,
        staged: &[u8],
        width: u32,
        height: u32,
        crop: Option<crate::h264::Crop>,
    ) -> Result<Vec<u8>, D3d12Error> {
        let (left, top, visible_width, visible_height) = visible_rect(width, height, crop);
        let y_len = usize::try_from(u64::from(visible_width) * u64::from(visible_height))
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let mut out = Vec::with_capacity(y_len + y_len / 2);
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
                &mut out,
            )?;
        }
        Ok(out)
    }

    /// Pitched staging bytes from tight `nv12` bytes.
    pub(crate) fn stride(
        &self,
        nv12: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, D3d12Error> {
        let y_len = usize::try_from(u64::from(width) * u64::from(height))
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        if nv12.len() < y_len + y_len / 2 {
            return Err(D3d12Error::Unsupported(
                "nv12 short of its extent".to_owned(),
            ));
        }
        let total = usize::try_from(self.total)
            .map_err(|_| D3d12Error::Unsupported("picture too large".to_owned()))?;
        let mut staged = vec![0u8; total];
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
                        "staging short of its layout".to_owned(),
                    ));
                }
                staged[dst..end].copy_from_slice(&nv12[src..src + width]);
                src += width;
            }
        }
        Ok(staged)
    }
}

/// Visible rectangle in luma samples under `crop`.
fn visible_rect(width: u32, height: u32, crop: Option<crate::h264::Crop>) -> (u32, u32, u32, u32) {
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
                "staging short of its layout".to_owned(),
            ));
        }
        out.extend_from_slice(&staged[src..end]);
    }
    Ok(())
}

/// Logical device behind resource creation.
#[cfg(target_os = "windows")]
pub(crate) struct DeviceResources {
    /// Logical device.
    device: ID3D12Device,
}

#[cfg(target_os = "windows")]
impl DeviceResources {
    /// Resources over `device`.
    pub(crate) fn new(device: &ID3D12Device) -> Self {
        Self {
            device: device.clone(),
        }
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
        let props = Self::heap_props(heap);
        let mut out: Option<ID3D12Resource> = None;
        // Sound: `props` and `desc` outlive the call; `out` receives the resource.
        unsafe {
            self.device.CreateCommittedResource(
                std::ptr::addr_of!(props),
                D3D12_HEAP_FLAG_NONE,
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
        out.ok_or_else(|| D3d12Error::Unsupported(format!("{call} returned null")))
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
            return Err(D3d12Error::Unsupported("empty NV12 layout".to_owned()));
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
        let mut mapped: *mut c_void = std::ptr::null_mut();
        // Sound: the upload mapping outlives the copy below.
        unsafe { upload.Map(0, None, Some(std::ptr::addr_of_mut!(mapped))) }.map_err(|error| {
            D3d12Error::Call {
                call: "Map",
                hresult: error.code().0,
            }
        })?;
        if mapped.is_null() {
            return Err(D3d12Error::Unsupported(
                "upload map returned null".to_owned(),
            ));
        }
        // Sound: `mapped` spans `bytes.len()` upload bytes; `bytes` outlives the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
            upload.Unmap(0, None);
        }
        Ok(())
    }

    /// `len` bytes out of the readback mapping.
    pub(crate) fn read_readback(
        readback: &ID3D12Resource,
        len: usize,
    ) -> Result<Vec<u8>, D3d12Error> {
        Self::with_readback(readback, len, |mapped| Ok(mapped.to_vec()))
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
                "readback map returned null".to_owned(),
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
}

/// Fence behind queue waits.
#[cfg(target_os = "windows")]
pub(crate) struct Fence {
    /// Fence behind queue waits.
    fence: ID3D12Fence,
    /// Monotonic fence value.
    value: u64,
}

#[cfg(target_os = "windows")]
impl Fence {
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
        Ok(Self { fence, value: 0 })
    }

    /// `command` through `queue`, then the fence wait.
    pub(crate) fn submit(
        &mut self,
        queue: &ID3D12CommandQueue,
        command: &ID3D12CommandList,
    ) -> Result<(), D3d12Error> {
        // Sound: the command and queue outlive the call.
        unsafe {
            queue.ExecuteCommandLists(&[Some(command.clone())]);
        }
        self.wait(queue)
    }

    /// Signal the fence and spin until it completes.
    fn wait(&mut self, queue: &ID3D12CommandQueue) -> Result<(), D3d12Error> {
        self.value = self.value.wrapping_add(1);
        let value = self.value;
        // Sound: the fence is live; the value is monotonic.
        unsafe { queue.Signal(&self.fence, value) }.map_err(|error| D3d12Error::Call {
            call: "Signal",
            hresult: error.code().0,
        })?;
        for _ in 0..50_000 {
            // Sound: polling a live fence for a signalled value.
            let done = unsafe { self.fence.GetCompletedValue() };
            if done >= value {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
        Err(D3d12Error::Call {
            call: "FenceWait",
            hresult: WAIT_TIMEOUT.0.cast_signed(),
        })
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
        let crop = crate::h264::Crop {
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
    fn stride_round_trips_tight_bytes() {
        let tight: Vec<u8> = (1..=12).collect();
        let staged = layout().stride(&tight, 4, 2).expect("staged");
        assert_eq!(staged.len(), 24);
        let back = layout().tighten(&staged, 4, 2, None).expect("tight");
        assert_eq!(back, tight);
    }

    #[test]
    fn stride_rejects_short_input() {
        assert!(layout().stride(&[0; 11], 4, 2).is_err());
    }
}
