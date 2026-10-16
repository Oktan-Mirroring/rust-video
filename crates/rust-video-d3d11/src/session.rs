//! Negotiated decode session: decoder, views, staging, readback.

#![allow(unsafe_code)]

use std::sync::Arc;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDecoder,
    ID3D11VideoDecoderOutputView, D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_RESOURCE_MISC_SHARED, D3D11_TEX2D_VDOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VDOV_DIMENSION_TEXTURE2D, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
    D3D11_VIDEO_DECODER_BUFFER_DESC, D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
    D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS, D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
    D3D11_VIDEO_DECODER_BUFFER_TYPE, D3D11_VIDEO_DECODER_CONFIG, D3D11_VIDEO_DECODER_DESC,
    D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC, D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource;
use windows::Win32::Media::DirectShow::DXVA_NoEncrypt;

use crate::device::D3d11Device;
use crate::dpb::SubmittedPicture;
use crate::error::D3d11Error;
use crate::h264::Crop;
use crate::submit::flag_word;
use crate::submit::{CopySlot, SubmitLock};

use bytes::Bytes;

/// Reference surface count.
const VIEWS: u32 = 16;

/// Pipeline margin past the reference surfaces.
const SPARE_VIEWS: u32 = 4;

/// Negotiated H.264 decode session.
pub struct Session {
    /// Hardware decoder.
    decoder: ID3D11VideoDecoder,
    /// Output view per reference surface.
    views: Vec<ID3D11VideoDecoderOutputView>,
    /// Array texture behind the views.
    texture: ID3D11Texture2D,
    /// Copy lane per reference surface.
    slots: Vec<CopySlot>,
    /// Video context behind frame submission.
    video_context: ID3D11VideoContext,
    /// Immediate context behind copies and readback.
    context: ID3D11DeviceContext,
    /// Single-submitter lock over the immediate context.
    submit: Arc<SubmitLock>,
    /// Shared-texture export, absent without NT-handle sharing.
    export: Option<SharedExport>,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Short slice controls behind a raw-2 config.
    slice_short: bool,
}

/// Shared decode texture with its NT handle.
#[derive(Clone)]
pub(crate) struct SharedExport {
    /// Texture owning the handle.
    texture: Arc<ID3D11Texture2D>,
    /// NT handle value.
    handle: isize,
}

impl SharedExport {
    /// NT handle value.
    pub(crate) fn handle(&self) -> isize {
        self.handle
    }
}

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
    /// Returns `Err` without a raw bitstream config, past
    /// the adapter surface floor, or when the driver refuses the decoder.
    pub fn open(device: &D3d11Device, width: u32, height: u32) -> Result<Self, D3d11Error> {
        Self::open_windows(device, width, height)
    }

    /// Session over `device` for `width` x `height` H.264 pictures.
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
        let mut slots = Vec::with_capacity(usize::try_from(VIEWS).unwrap_or(0));
        for _ in 0..VIEWS {
            slots.push(CopySlot::open(device.device(), width, height)?);
        }
        let export = shared_export(&texture);
        let slice_short = config.ConfigBitstreamRaw == 2;
        Ok(Self {
            decoder,
            views,
            texture,
            slots,
            video_context: device.video_context().clone(),
            context: device.context().clone(),
            submit: device.submit_lock().clone(),
            export,
            width,
            height,
            slice_short,
        })
    }

    /// Shared-texture export, absent without NT-handle sharing.
    #[must_use]
    pub(crate) fn export(&self) -> Option<SharedExport> {
        self.export.clone()
    }

    /// Shared decode texture plus `slot`, absent without NT-handle sharing.
    #[must_use]
    pub(crate) fn surface(&self, slot: u8) -> Option<(SharedExport, u8)> {
        if usize::from(slot) >= self.views.len() {
            return None;
        }
        self.export.clone().map(|export| (export, slot))
    }

    /// Decode `submission` onto its surface slot.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a buffer overruns the driver
    /// allocation, or when the driver refuses the submission.
    pub fn submit(&self, submission: &SubmittedPicture) -> Result<(), D3d11Error> {
        self.submit_windows(submission)
    }

    /// Array `slot` copied to its lane staging through the immediate context.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the views or when the driver
    /// refuses the copy.
    pub fn copy_slot(&self, slot: u8) -> Result<(), D3d11Error> {
        let lane = self
            .slots
            .get(usize::from(slot))
            .ok_or_else(|| D3d11Error::BrokenReference("slot outside views".to_owned()))?;
        let list = lane.record(&self.texture, slot)?;
        let _submit = self.submit.enter()?;
        lane.complete(&self.context, &list);
        Ok(())
    }

    /// NV12 bytes for `slot` cropped to the visible rectangle.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a slot outside the views, when the copy
    /// misses its completion budget, or when the map fails.
    pub fn read_slot(&self, slot: u8, crop: Option<Crop>) -> Result<Bytes, D3d11Error> {
        let lane = self
            .slots
            .get(usize::from(slot))
            .ok_or_else(|| D3d11Error::BrokenReference("slot outside views".to_owned()))?;
        lane.wait(&self.context)?;
        lane.read(&self.context, crop)
    }

    /// Decode `submission` onto its surface slot.
    fn submit_windows(&self, submission: &SubmittedPicture) -> Result<(), D3d11Error> {
        let view = self
            .views
            .get(usize::from(submission.slot))
            .ok_or_else(|| D3d11Error::BrokenReference("slot outside views".to_owned()))?;
        let _submit = self.submit.enter()?;
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
}

/// Raw bitstream config without encryption.
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

/// NT-handle export for a shared texture, absent without sharing.
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

/// Plain-data struct as bytes.
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
fn slice_slice<T>(values: &[T]) -> &[u8] {
    // Sound: the slice is live with a plain-data element type.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

/// Short slice controls with cleared trailing pad.
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

#[cfg(test)]
mod tests {
    /// Session negotiation plus staging roundtrip on hardware, skipped without.
    #[test]
    fn negotiate_and_copy_on_hardware() {
        use crate::device::D3d11Device;
        use crate::h264::Crop;

        use super::Session;

        let Ok(device) = D3d11Device::open() else {
            return;
        };
        let session = Session::open(&device, 1920, 1088).expect("1080p session");
        session.copy_slot(0).expect("copy");
        let full = session.read_slot(0, None).expect("readback");
        assert_eq!(full.len(), 1920 * 1088 * 3 / 2);
        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        session.copy_slot(0).expect("copy");
        let visible = session.read_slot(0, Some(crop)).expect("cropped readback");
        assert_eq!(visible.len(), 1920 * 1080 * 3 / 2);
        let rounds = 60u32;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            session.copy_slot(0).expect("copy");
            session.read_slot(0, None).expect("readback");
        }
        let elapsed = start.elapsed();
        let frame_bytes = 1920u128 * 1088 * 3 / 2;
        let micros = elapsed.as_micros().max(1);
        let mb_per_s = frame_bytes * u128::from(rounds) / micros;
        println!("staging copy: {mb_per_s} MB/s over {rounds} 1080p frames in {elapsed:?}");
        assert!(mb_per_s > 186, "staging below the realtime floor");
    }
}
