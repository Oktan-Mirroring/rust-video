//! Immediate-context submit lock plus per-slot staging copy lanes.
//!
//! <https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-render>
//! <https://docs.rs/windows/0.62.2/windows/Win32/Graphics/Direct3D11/>

// FFI boundary: device-context COM calls live here.
#![allow(unsafe_code)]

use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::error::{D3d11Error, UnsupportedReason};
use crate::h264::Crop;

use bytes::Bytes;
use windows::core::Interface;
use windows::Win32::Foundation::{FALSE, S_FALSE, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Asynchronous, ID3D11Device, ID3D11DeviceContext, ID3D11Query, ID3D11Texture2D,
    D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
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

/// Per-slot copy lane: staging plus completion query.
pub(crate) struct CopySlot {
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
    /// Returns `Err` when the driver refuses the staging texture or the event query.
    pub(crate) fn open(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, D3d11Error> {
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
            staging,
            signal,
            width,
            height,
        })
    }

    /// Array `slot` into staging through the immediate context, ending the completion query.
    pub(crate) fn copy(&self, context: &ID3D11DeviceContext, array: &ID3D11Texture2D, slot: u8) {
        // Sound: both textures are live with matching extents; the slot is a
        // valid slice; the query is live; the caller holds the submit lock.
        unsafe {
            context.CopySubresourceRegion(&self.staging, 0, 0, 0, 0, array, u32::from(slot), None);
            context.End(&self.signal);
        }
    }

    /// Mark region of array `slot` into staging, ending the completion query.
    ///
    /// Orders the marker behind the video decode like [`copy`](Self::copy)
    /// without moving the frame; staging bytes outside the mark region keep
    /// their previous contents.
    pub(crate) fn copy_light(
        &self,
        context: &ID3D11DeviceContext,
        array: &ID3D11Texture2D,
        slot: u8,
    ) {
        let mark = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: self.width.min(16),
            bottom: self.height.min(16),
            back: 1,
        };
        // Sound: both textures are live; the slot is a valid slice; the mark
        // box fits the lane extent; the query is live; the caller holds the
        // submit lock.
        unsafe {
            context.CopySubresourceRegion(
                &self.staging,
                0,
                0,
                0,
                0,
                array,
                u32::from(slot),
                Some(std::ptr::addr_of!(mark)),
            );
            context.End(&self.signal);
        }
    }

    /// Completion of the last copy, outside the submit lock.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget or reports failure.
    pub(crate) fn wait(&self, context: &ID3D11DeviceContext) -> Result<(), D3d11Error> {
        wait_for_query(context, &self.signal)
    }

    /// True once the GPU passes the last copy marker, without waiting.
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
        let mapped = self.map(context)?;
        let bytes = copy_nv12(&mapped, self.width, self.height, crop);
        self.unmap(context);
        bytes
    }

    /// Mapped staging texture for CPU reads.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the map fails.
    pub(crate) fn map(
        &self,
        context: &ID3D11DeviceContext,
    ) -> Result<D3D11_MAPPED_SUBRESOURCE, D3d11Error> {
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
        Ok(mapped)
    }

    /// Release a mapping from [`map`](Self::map).
    pub(crate) fn unmap(&self, context: &ID3D11DeviceContext) {
        // Sound: the staging texture mapped above.
        unsafe {
            context.Unmap(&self.staging, 0);
        }
    }

    /// Staging texture behind maps.
    pub(crate) fn staging(&self) -> ID3D11Texture2D {
        self.staging.clone()
    }
}

/// Pack shards behind large copies.
const SHARDS: usize = 4;

/// Copies below this size pack on the calling thread.
const SHARD_MIN_BYTES: usize = 256 * 1024;

/// One pack shard: disjoint segments plus its ack.
struct ShardJob {
    /// Source, destination, length per segment.
    segments: [(*const u8, *mut u8, usize); 2],
    /// Segments behind the shard.
    count: usize,
    /// Acked after the last segment lands.
    ack: SyncSender<()>,
}

// Sound: each hop moves sole ownership; segments never alias
// across shards; the packer waits every ack before touching dst.
unsafe impl Send for ShardJob {}

/// Persistent shard-copy team behind large packs.
pub(crate) struct PackTeam {
    /// One inbox per shard worker.
    inbox: Mutex<Vec<SyncSender<ShardJob>>>,
    /// Shard workers, joined on shutdown.
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl PackTeam {
    /// Shard team over nothing; packs arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        let mut inbox = Vec::with_capacity(SHARDS);
        let mut workers = Vec::with_capacity(SHARDS);
        for index in 0..SHARDS {
            let (tx, rx) = sync_channel::<ShardJob>(1);
            let worker = std::thread::Builder::new()
                .name(format!("d3d11-pack-{index}"))
                .spawn(move || {
                    for job in rx {
                        for (src, dst, len) in job.segments.iter().take(job.count) {
                            // Sound: the packer carved disjoint ranges
                            // inside live source and destination.
                            unsafe {
                                (*src).copy_to(*dst, *len);
                            }
                        }
                        let _ = job.ack.send(());
                    }
                });
            let Ok(worker) = worker else {
                break;
            };
            inbox.push(tx);
            workers.push(worker);
        }
        Self {
            inbox: Mutex::new(inbox),
            workers: Mutex::new(workers),
        }
    }

    /// `runs` scattered over the shards into `dst`, waiting all.
    fn scatter(&self, dst: &mut Vec<u8>, runs: &FastGeom, base: *const u8) {
        let total = runs.y_len + runs.c_len;
        dst.clear();
        dst.reserve(total);
        let target = dst.as_mut_ptr();
        let inbox = self
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shards = inbox.len();
        let mut acks = Vec::with_capacity(shards);
        for (index, tx) in inbox.iter().enumerate() {
            let start = total * index / shards;
            let end = total * (index + 1) / shards;
            let mut segments = [(std::ptr::null(), std::ptr::null_mut(), 0); 2];
            let mut count;
            if start < runs.y_len {
                let cut = end.min(runs.y_len);
                // Sound: the run bounds the segment.
                segments[0] = (
                    // Sound: the mapped base plus run offsets stays inside the staging texture.
                    unsafe { base.add(runs.y_src_off + start) },
                    // Sound: the destination reserved the total.
                    unsafe { target.add(start) },
                    cut - start,
                );
                count = 1;
                if end > runs.y_len {
                    segments[1] = (
                        unsafe { base.add(runs.c_src_off) },
                        unsafe { target.add(runs.y_len) },
                        end - runs.y_len,
                    );
                    count = 2;
                }
            } else {
                segments[0] = (
                    unsafe { base.add(runs.c_src_off + (start - runs.y_len)) },
                    unsafe { target.add(start) },
                    end - start,
                );
                count = 1;
            }
            let (ack_tx, ack_rx) = sync_channel::<()>(0);
            let job = ShardJob {
                segments,
                count,
                ack: ack_tx,
            };
            if tx.send(job).is_err() {
                for (src, dst, len) in segments.iter().take(count) {
                    // Sound: the dead shard's ranges stay disjoint.
                    unsafe {
                        (*src).copy_to(*dst, *len);
                    }
                }
                continue;
            }
            acks.push(ack_rx);
        }
        for ack in acks {
            let _ = ack.recv();
        }
        // Sound: every acked shard wrote its disjoint range.
        unsafe {
            dst.set_len(total);
        }
    }

    /// Shard workers joined, inboxes closed.
    pub(crate) fn shutdown(&self) {
        self.inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for worker in self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            let _ = worker.join();
        }
    }

    /// True with at least one live shard worker.
    fn armed(&self) -> bool {
        self.inbox.lock().is_ok_and(|inbox| !inbox.is_empty())
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
pub(crate) fn copy_nv12(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
    crop: Option<Crop>,
) -> Result<Bytes, D3d11Error> {
    let capacity = packed_len(width, height, crop);
    let mut bytes = Vec::with_capacity(capacity);
    copy_nv12_into(mapped, width, height, crop, &mut bytes)?;
    Ok(Bytes::from(bytes))
}

/// Mapped staging texture packed under `crop` into `dst`, cleared first.
pub(crate) fn copy_nv12_into(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
    crop: Option<Crop>,
    dst: &mut Vec<u8>,
) -> Result<(), D3d11Error> {
    if let Some(runs) = FastGeom::derive(mapped, width, height, crop)? {
        dst.clear();
        // Sound: the pitch and extents bound every row inside the mapped staging texture.
        unsafe {
            let base = mapped.pData.cast::<u8>();
            dst.extend_from_slice(std::slice::from_raw_parts(
                base.add(runs.y_src_off),
                runs.y_len,
            ));
            dst.extend_from_slice(std::slice::from_raw_parts(
                base.add(runs.c_src_off),
                runs.c_len,
            ));
        }
        return Ok(());
    }
    copy_nv12_rows(mapped, width, height, crop, dst)
}

/// Mapped staging texture packed over `team` into `dst`, cleared first.
///
/// Small or left-cropped packs run on the calling thread.
pub(crate) fn copy_nv12_team(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
    crop: Option<Crop>,
    dst: &mut Vec<u8>,
    team: &PackTeam,
) -> Result<(), D3d11Error> {
    if packed_len(width, height, crop) < SHARD_MIN_BYTES || !team.armed() {
        return copy_nv12_into(mapped, width, height, crop, dst);
    }
    if let Some(runs) = FastGeom::derive(mapped, width, height, crop)? {
        // Sound: the mapped base stays live across the scatter.
        team.scatter(dst, &runs, mapped.pData.cast::<u8>().cast_const());
        return Ok(());
    }
    copy_nv12_rows(mapped, width, height, crop, dst)
}

/// Row-by-row pack for left-cropped or padded pitches.
fn copy_nv12_rows(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
    crop: Option<Crop>,
    dst: &mut Vec<u8>,
) -> Result<(), D3d11Error> {
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
    dst.clear();
    // Sound: the pitch and extents bound every row inside the mapped staging texture.
    unsafe {
        let base = mapped.pData.cast::<u8>();
        for row in 0..visible_height {
            let src = base.add(top * pitch + row * pitch + left);
            dst.extend_from_slice(std::slice::from_raw_parts(src, visible_width));
        }
        let chroma = base.add(height * pitch);
        for row in 0..visible_height / 2 {
            let src = chroma.add(top * pitch / 2 + row * pitch + left);
            dst.extend_from_slice(std::slice::from_raw_parts(src, visible_width));
        }
    }
    Ok(())
}

/// Fast-path geometry: contiguous luma plus chroma runs.
struct FastGeom {
    /// Luma run offset from the mapped base.
    y_src_off: usize,
    /// Chroma run offset from the mapped base.
    c_src_off: usize,
    /// Luma run length.
    y_len: usize,
    /// Chroma run length.
    c_len: usize,
}

impl FastGeom {
    /// Geometry when the pitch packs tight without a left crop, else none.
    fn derive(
        mapped: &D3D11_MAPPED_SUBRESOURCE,
        width: u32,
        height: u32,
        crop: Option<Crop>,
    ) -> Result<Option<Self>, D3d11Error> {
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
        if pitch != visible_width || left != 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            y_src_off: top * pitch,
            c_src_off: height * pitch + top * pitch / 2,
            y_len: visible_width * visible_height,
            c_len: visible_width * (visible_height / 2),
        }))
    }
}

/// Packed NV12 length inside the coded extent under `crop`.
fn packed_len(width: u32, height: u32, crop: Option<Crop>) -> usize {
    let (_, _, visible_width, visible_height) = visible_rect(width, height, crop);
    usize::try_from(visible_width)
        .unwrap_or(0)
        .saturating_mul(usize::try_from(visible_height).unwrap_or(0))
        .saturating_mul(3)
        / 2
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
    fn team_pack_matches_single() {
        use super::{copy_nv12_into, copy_nv12_team, PackTeam};

        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        let mut staged = vec![0u8; 1920 * 1088 * 3 / 2];
        for (index, byte) in staged.iter_mut().enumerate() {
            *byte = u8::try_from(index % 251).expect("byte range");
        }
        let team = PackTeam::spawn();
        let view = mapped(&mut staged, 1920);
        let mut single = Vec::new();
        copy_nv12_into(&view, 1920, 1088, Some(crop), &mut single).expect("single pack");
        let mut sharded = Vec::new();
        copy_nv12_team(&view, 1920, 1088, Some(crop), &mut sharded, &team).expect("team pack");
        team.shutdown();
        assert_eq!(sharded, single);
        let left = Crop {
            left: 2,
            right: 0,
            top: 0,
            bottom: 8,
        };
        let mut single = Vec::new();
        copy_nv12_into(&view, 1920, 1088, Some(left), &mut single).expect("single pack");
        let team = PackTeam::spawn();
        let mut sharded = Vec::new();
        copy_nv12_team(&view, 1920, 1088, Some(left), &mut sharded, &team).expect("team pack");
        team.shutdown();
        assert_eq!(sharded, single);
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
