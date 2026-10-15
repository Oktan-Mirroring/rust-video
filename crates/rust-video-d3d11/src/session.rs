//! Negotiated decode session: decoder, views, staging, readback.
//!
//! The live implementation targets Windows; other platforms decline at
//! `open` with a typed skip.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::sync::Arc;

#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDecoder,
    ID3D11VideoDecoderOutputView, D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_RESOURCE_MISC_SHARED,
    D3D11_TEX2D_VDOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11_VDOV_DIMENSION_TEXTURE2D, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
    D3D11_VIDEO_DECODER_BUFFER_DESC, D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
    D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS, D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
    D3D11_VIDEO_DECODER_BUFFER_TYPE, D3D11_VIDEO_DECODER_CONFIG, D3D11_VIDEO_DECODER_DESC,
    D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC, D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::IDXGIResource;
#[cfg(target_os = "windows")]
use windows::Win32::Media::DirectShow::DXVA_NoEncrypt;

use crate::device::D3d11Device;
use crate::dpb::SubmittedPicture;
use crate::error::D3d11Error;
#[cfg(not(target_os = "windows"))]
use crate::error::SkipReason;
use crate::h264::Crop;

use bytes::Bytes;

/// Reference surface count.
#[cfg(target_os = "windows")]
const VIEWS: u32 = 16;

/// Pipeline margin past the reference surfaces.
#[cfg(target_os = "windows")]
const SPARE_VIEWS: u32 = 4;

/// Negotiated H.264 decode session.
pub struct Session {
    /// Hardware decoder.
    #[cfg(target_os = "windows")]
    decoder: ID3D11VideoDecoder,
    /// Output view per reference surface.
    #[cfg(target_os = "windows")]
    views: Vec<ID3D11VideoDecoderOutputView>,
    /// Array texture behind the views.
    #[cfg(target_os = "windows")]
    texture: ID3D11Texture2D,
    /// CPU-visible copy target.
    #[cfg(target_os = "windows")]
    staging: ID3D11Texture2D,
    /// Video context behind frame submission.
    #[cfg(target_os = "windows")]
    video_context: ID3D11VideoContext,
    /// Immediate context behind copies and readback.
    #[cfg(target_os = "windows")]
    context: ID3D11DeviceContext,
    /// Shared-texture export, absent without NT-handle sharing.
    #[cfg(target_os = "windows")]
    export: Option<SharedExport>,
    /// Coded width in luma samples.
    #[cfg(target_os = "windows")]
    width: u32,
    /// Coded height in luma samples.
    #[cfg(target_os = "windows")]
    height: u32,
    /// Short slice controls behind a raw-2 config.
    #[cfg(target_os = "windows")]
    slice_short: bool,
}

/// Shared decode texture with its NT handle.
#[cfg(target_os = "windows")]
#[derive(Clone)]
pub(crate) struct SharedExport {
    /// Texture owning the handle.
    texture: Arc<ID3D11Texture2D>,
    /// NT handle value.
    handle: isize,
}

#[cfg(target_os = "windows")]
impl SharedExport {
    /// NT handle value.
    pub(crate) fn handle(&self) -> isize {
        self.handle
    }
}

#[cfg(target_os = "windows")]
impl std::ops::Deref for SharedExport {
    type Target = ID3D11Texture2D;

    fn deref(&self) -> &Self::Target {
        &self.texture
    }
}

impl Session {
    /// Session over `device` for `width` x `height` H.264 pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, without a raw bitstream config, past
    /// the adapter surface floor, or when the driver refuses the decoder.
    pub fn open(device: &D3d11Device, width: u32, height: u32) -> Result<Self, D3d11Error> {
        #[cfg(target_os = "windows")]
        {
            Self::open_windows(device, width, height)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device, width, height);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Session over `device` for `width` x `height` H.264 pictures.
    #[cfg(target_os = "windows")]
    fn open_windows(device: &D3d11Device, width: u32, height: u32) -> Result<Self, D3d11Error> {
        let guid = crate::device::h264_guid();
        let desc = D3D11_VIDEO_DECODER_DESC {
            Guid: guid,
            SampleWidth: width,
            SampleHeight: height,
            OutputFormat: DXGI_FORMAT_NV12,
        };
        let config = select_config(device.video_device(), &desc)?;
        if u32::from(config.ConfigMinRenderTargetBuffCount) > VIEWS + SPARE_VIEWS {
            return Err(D3d11Error::OverCaps {
                what: "render targets",
            });
        }
        // Sound: the descriptor and config stay live for the call.
        let decoder = unsafe {
            device
                .video_device()
                .CreateVideoDecoder(std::ptr::addr_of!(desc), std::ptr::addr_of!(config))
        }
        .map_err(|error| D3d11Error::Call {
            call: "CreateVideoDecoder",
            hresult: error.code().0,
        })?;
        let texture = create_texture(device, width, height)?;
        let mut views = Vec::with_capacity(usize::try_from(VIEWS).unwrap_or(0));
        for slice in 0..VIEWS {
            let desc = D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC {
                DecodeProfile: guid,
                ViewDimension: D3D11_VDOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VDOV { ArraySlice: slice },
                },
            };
            let mut view = None;
            // Sound: the texture and descriptor stay live for the call.
            unsafe {
                device.video_device().CreateVideoDecoderOutputView(
                    &texture,
                    std::ptr::addr_of!(desc),
                    Some(std::ptr::addr_of_mut!(view)),
                )
            }
            .map_err(|error| D3d11Error::Call {
                call: "CreateVideoDecoderOutputView",
                hresult: error.code().0,
            })?;
            let Some(view) = view else {
                return Err(D3d11Error::Call {
                    call: "CreateVideoDecoderOutputView",
                    hresult: windows::Win32::Foundation::E_FAIL.0,
                });
            };
            views.push(view);
        }
        let staging = create_staging(device, width, height)?;
        let export = shared_export(&texture);
        let slice_short = config.ConfigBitstreamRaw == 2;
        Ok(Self {
            decoder,
            views,
            texture,
            staging,
            video_context: device.video_context().clone(),
            context: device.context().clone(),
            export,
            width,
            height,
            slice_short,
        })
    }

    /// Shared-texture export, absent without NT-handle sharing.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub(crate) fn export(&self) -> Option<SharedExport> {
        self.export.clone()
    }

    /// Decode `submission` onto its surface slot.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows, when a buffer overruns the driver
    /// allocation, or when the driver refuses the submission.
    pub fn submit(&self, submission: &SubmittedPicture) -> Result<(), D3d11Error> {
        #[cfg(target_os = "windows")]
        {
            self.submit_windows(submission)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (
                self,
                &submission.params,
                &submission.qmatrix,
                &submission.slices,
                &submission.bitstream,
                submission.slot,
            );
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// NV12 bytes for `slot` cropped to the visible rectangle.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Windows or when the copy or map fails.
    pub fn readback(&self, slot: u8, crop: Option<Crop>) -> Result<Bytes, D3d11Error> {
        #[cfg(target_os = "windows")]
        {
            self.readback_windows(slot, crop)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self, slot, crop);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Decode `submission` onto its surface slot.
    #[cfg(target_os = "windows")]
    fn submit_windows(&self, submission: &SubmittedPicture) -> Result<(), D3d11Error> {
        let view = self
            .views
            .get(usize::from(submission.slot))
            .ok_or_else(|| D3d11Error::BrokenReference("slot outside views".to_owned()))?;
        // Sound: the decoder and view are live; no content key.
        unsafe {
            self.video_context
                .DecoderBeginFrame(&self.decoder, view, 0, None)
        }
        .map_err(|error| D3d11Error::Call {
            call: "DecoderBeginFrame",
            hresult: error.code().0,
        })?;
        let result = self.submit_buffers(submission);
        // Sound: the frame began above.
        let end = unsafe { self.video_context.DecoderEndFrame(&self.decoder) };
        match (result, end) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(D3d11Error::Call {
                call: "DecoderEndFrame",
                hresult: error.code().0,
            }),
        }
    }

    /// Picture, matrix, bitstream, and slice buffers in one submit.
    #[cfg(target_os = "windows")]
    fn submit_buffers(&self, submission: &SubmittedPicture) -> Result<(), D3d11Error> {
        let params = struct_slice(&submission.params);
        let matrix = struct_slice(&submission.qmatrix);
        let short;
        let slices = if self.slice_short {
            short = short_bytes(&submission.slices);
            short.as_slice()
        } else {
            slice_slice(&submission.slices)
        };
        let macroblocks = (self.width / 16) * (self.height / 16);
        let buffers = [
            (D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS, params, 0),
            (
                D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
                matrix,
                0,
            ),
            (
                D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
                submission.bitstream.as_slice(),
                macroblocks,
            ),
            (
                D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
                slices,
                macroblocks,
            ),
        ];
        let mut descs = Vec::with_capacity(buffers.len());
        for (buffer_type, bytes, macroblocks) in buffers.iter().copied() {
            descs.push(self.fill_buffer(buffer_type, bytes, macroblocks)?);
        }
        // Sound: the descriptors borrow released driver buffers.
        unsafe {
            self.video_context
                .SubmitDecoderBuffers(&self.decoder, &descs)
        }
        .map_err(|error| D3d11Error::Call {
            call: "SubmitDecoderBuffers",
            hresult: error.code().0,
        })
    }

    /// Driver buffer of `buffer_type` filled with `bytes` over `macroblocks`.
    #[cfg(target_os = "windows")]
    fn fill_buffer(
        &self,
        buffer_type: D3D11_VIDEO_DECODER_BUFFER_TYPE,
        bytes: &[u8],
        macroblocks: u32,
    ) -> Result<D3D11_VIDEO_DECODER_BUFFER_DESC, D3d11Error> {
        let mut size = 0u32;
        let mut buffer: *mut std::ffi::c_void = std::ptr::null_mut();
        // Sound: the out-parameters stay live for the call.
        unsafe {
            self.video_context.GetDecoderBuffer(
                &self.decoder,
                buffer_type,
                std::ptr::addr_of_mut!(size),
                std::ptr::addr_of_mut!(buffer),
            )
        }
        .map_err(|error| D3d11Error::Call {
            call: "GetDecoderBuffer",
            hresult: error.code().0,
        })?;
        if usize::try_from(size).is_ok_and(|capacity| bytes.len() > capacity) {
            // Sound: the buffer came from the get call above.
            let _ = unsafe {
                self.video_context
                    .ReleaseDecoderBuffer(&self.decoder, buffer_type)
            };
            return Err(D3d11Error::BitstreamTooLarge {
                len: bytes.len(),
                capacity: u64::from(size),
            });
        }
        // Sound: the driver buffer holds `size` bytes with `size` above the length.
        unsafe {
            buffer
                .cast::<u8>()
                .copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        }
        // Sound: the buffer came from the get call above.
        unsafe {
            self.video_context
                .ReleaseDecoderBuffer(&self.decoder, buffer_type)
        }
        .map_err(|error| D3d11Error::Call {
            call: "ReleaseDecoderBuffer",
            hresult: error.code().0,
        })?;
        let data_size = u32::try_from(bytes.len()).map_err(|_| D3d11Error::BitstreamTooLarge {
            len: bytes.len(),
            capacity: u64::from(size),
        })?;
        Ok(D3D11_VIDEO_DECODER_BUFFER_DESC {
            BufferType: buffer_type,
            DataSize: data_size,
            NumMBsInBuffer: macroblocks,
            ..Default::default()
        })
    }

    /// NV12 bytes for `slot` cropped to the visible rectangle.
    #[cfg(target_os = "windows")]
    fn readback_windows(&self, slot: u8, crop: Option<Crop>) -> Result<Bytes, D3d11Error> {
        // Sound: both textures are live with matching extents; the slot is a valid slice.
        unsafe {
            self.context.CopySubresourceRegion(
                &self.staging,
                0,
                0,
                0,
                0,
                &self.texture,
                u32::from(slot),
                None,
            );
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // Sound: the staging texture is live with read access.
        unsafe {
            self.context.Map(
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
            self.context.Unmap(&self.staging, 0);
        }
        bytes
    }
}

/// Raw bitstream config without encryption.
#[cfg(target_os = "windows")]
fn select_config(
    video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
    desc: &D3D11_VIDEO_DECODER_DESC,
) -> Result<D3D11_VIDEO_DECODER_CONFIG, D3d11Error> {
    // Sound: the descriptor stays live for the count call.
    let count = unsafe { video_device.GetVideoDecoderConfigCount(desc) }.map_err(|error| {
        D3d11Error::Call {
            call: "GetVideoDecoderConfigCount",
            hresult: error.code().0,
        }
    })?;
    let mut best: Option<(u32, D3D11_VIDEO_DECODER_CONFIG)> = None;
    for index in 0..count {
        let mut config = D3D11_VIDEO_DECODER_CONFIG::default();
        // Sound: the descriptor and config stay live for the call.
        unsafe { video_device.GetVideoDecoderConfig(desc, index, std::ptr::addr_of_mut!(config)) }
            .map_err(|error| D3d11Error::Call {
                call: "GetVideoDecoderConfig",
                hresult: error.code().0,
            })?;
        let mut score = match config.ConfigBitstreamRaw {
            1 => 1,
            2 => 2,
            _ => continue,
        };
        if config.guidConfigBitstreamEncryption == DXVA_NoEncrypt {
            score += 16;
        }
        if best.is_none_or(|(best_score, _)| score > best_score) {
            best = Some((score, config));
        }
    }
    best.map(|(_, config)| config).ok_or(D3d11Error::OverCaps {
        what: "decoder config",
    })
}

/// Array texture behind the decode views, shared when the driver allows.
#[cfg(target_os = "windows")]
fn create_texture(
    device: &D3d11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, D3d11Error> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: VIEWS + SPARE_VIEWS,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: flag_word(D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0),
        CPUAccessFlags: 0,
        MiscFlags: flag_word(D3D11_RESOURCE_MISC_SHARED.0),
    };
    // Sound: the descriptor stays live for each call.
    let mut texture = None;
    let shared = unsafe {
        device.device().CreateTexture2D(
            std::ptr::addr_of!(desc),
            None,
            Some(std::ptr::addr_of_mut!(texture)),
        )
    };
    if let (Ok(()), Some(texture)) = (shared, texture) {
        return Ok(texture);
    }
    let desc = D3D11_TEXTURE2D_DESC {
        MiscFlags: 0,
        ..desc
    };
    let mut texture = None;
    unsafe {
        device.device().CreateTexture2D(
            std::ptr::addr_of!(desc),
            None,
            Some(std::ptr::addr_of_mut!(texture)),
        )
    }
    .map_err(|error| D3d11Error::Call {
        call: "CreateTexture2D",
        hresult: error.code().0,
    })?;
    texture.ok_or(D3d11Error::Call {
        call: "CreateTexture2D",
        hresult: windows::Win32::Foundation::E_FAIL.0,
    })
}

/// CPU-visible texture behind readback.
#[cfg(target_os = "windows")]
fn create_staging(
    device: &D3d11Device,
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
        device.device().CreateTexture2D(
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

/// NT-handle export for a shared texture, absent without sharing.
#[cfg(target_os = "windows")]
fn shared_export(texture: &ID3D11Texture2D) -> Option<SharedExport> {
    let resource: IDXGIResource = texture.cast().ok()?;
    // Sound: the resource is live.
    let handle = unsafe { resource.GetSharedHandle() }.ok()?;
    if handle.0.is_null() {
        return None;
    }
    Some(SharedExport {
        texture: Arc::new(texture.clone()),
        handle: handle.0 as isize,
    })
}

/// Flag bits as the descriptor word.
#[cfg(target_os = "windows")]
fn flag_word(bits: i32) -> u32 {
    u32::try_from(bits).unwrap_or(0)
}

/// Plain-data struct as bytes.
#[cfg(target_os = "windows")]
fn struct_slice<T>(value: &T) -> &[u8] {
    // Sound: the value is a live `repr(C)` plain-data struct.
    unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(value).cast::<u8>(),
            std::mem::size_of_val(value),
        )
    }
}

/// Slice control entries as bytes.
#[cfg(target_os = "windows")]
fn slice_slice<T>(values: &[T]) -> &[u8] {
    // Sound: the slice is live with a plain-data element type.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

/// Short slice controls with cleared trailing pad.
#[cfg(target_os = "windows")]
fn short_bytes(slices: &[crate::dxva::SliceLongH264]) -> Vec<u8> {
    let mut out = Vec::with_capacity(slices.len() * 12);
    for slice in slices {
        out.extend_from_slice(&slice.location.to_le_bytes());
        out.extend_from_slice(&slice.bytes.to_le_bytes());
        out.extend_from_slice(&slice.chopping.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// NV12 bytes from a mapped staging texture under `crop`.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
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

#[cfg(test)]
mod tests {
    /// Session negotiation plus staging roundtrip on hardware, skipped without.
    #[cfg(target_os = "windows")]
    #[test]
    fn negotiate_and_copy_on_hardware() {
        use crate::device::D3d11Device;
        use crate::h264::Crop;

        use super::Session;

        let Ok(device) = D3d11Device::open() else {
            return;
        };
        let session = Session::open(&device, 1920, 1088).expect("1080p session");
        let full = session.readback(0, None).expect("readback");
        assert_eq!(full.len(), 1920 * 1088 * 3 / 2);
        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        let visible = session.readback(0, Some(crop)).expect("cropped readback");
        assert_eq!(visible.len(), 1920 * 1080 * 3 / 2);
        let rounds = 60u32;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            session.readback(0, None).expect("readback");
        }
        let elapsed = start.elapsed();
        let frame_bytes = 1920u128 * 1088 * 3 / 2;
        let micros = elapsed.as_micros().max(1);
        let mb_per_s = frame_bytes * u128::from(rounds) / micros;
        println!("staging copy: {mb_per_s} MB/s over {rounds} 1080p frames in {elapsed:?}");
        assert!(mb_per_s > 186, "staging below the realtime floor");
    }
}
