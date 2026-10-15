//! GPU scale between decode and encode textures over the video processor.
//!
//! <https://docs.rs/windows/0.62.2/windows/Win32/Media/MediaFoundation/>
//! <https://learn.microsoft.com/en-us/windows/win32/api/d3d12video/nf-d3d12video-id3d12videoprocesscommandlist-processframes>

#![allow(unsafe_code)]

use std::mem::ManuallyDrop;

use crate::device::D3d12Device;
use crate::error::D3d12Error;
use crate::submit::{DeviceResources, Fence};
use crate::texture::GpuTexture;

use rust_video_caps::common::PictureSize;
use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12Resource,
    D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ, D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT_NV12, DXGI_RATIONAL,
};
use windows::Win32::Media::MediaFoundation::{
    ID3D12VideoDevice, ID3D12VideoProcessCommandList, ID3D12VideoProcessor,
    D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT, D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
    D3D12_VIDEO_FIELD_TYPE_NONE, D3D12_VIDEO_FORMAT, D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
    D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE, D3D12_VIDEO_PROCESS_DEINTERLACE_FLAG_NONE,
    D3D12_VIDEO_PROCESS_FILTER_FLAG_NONE, D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC, D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE,
    D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT, D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC, D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED,
    D3D12_VIDEO_SAMPLE, D3D12_VIDEO_SCALE_SUPPORT_FLAG_EVEN_DIMENSIONS_ONLY,
    D3D12_VIDEO_SCALE_SUPPORT_FLAG_POW2_ONLY, D3D12_VIDEO_SIZE_RANGE,
};

/// NV12 scale over one source and target extent.
pub(crate) struct Scaler {
    /// Video processor behind the scale.
    processor: ID3D12VideoProcessor,
    /// Video process queue.
    queue: ID3D12CommandQueue,
    /// Video process allocator.
    alloc: ID3D12CommandAllocator,
    /// Video process list.
    list: ID3D12VideoProcessCommandList,
    /// Fence behind the queue.
    fence: Fence,
    /// NV12 target texture.
    target: ID3D12Resource,
    /// Source extent.
    from: PictureSize,
    /// Target extent.
    to: PictureSize,
}

impl Scaler {
    /// Scaler over `device` from `from` to `to`.
    ///
    /// # Errors
    ///
    /// Returns `Err` without video process support for the pair or when
    /// the driver refuses the processor.
    pub(crate) fn open(
        device: &D3d12Device,
        from: PictureSize,
        to: PictureSize,
    ) -> Result<Self, D3d12Error> {
        check_support(device.video(), from, to)?;
        let processor = create_processor(device.video(), from, to)?;
        let resources = DeviceResources::new(device.device());
        let queue = resources.queue(D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)?;
        let alloc = resources.allocator(D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)?;
        let list: ID3D12VideoProcessCommandList =
            resources.command_list(D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS, &alloc)?;
        // Sound: the fresh list is open with no recorded work.
        unsafe { list.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })?;
        let fence = Fence::open(device.device())?;
        let desc = DeviceResources::texture_desc(to.width(), to.height());
        let target = resources.committed(
            D3D12_HEAP_TYPE_DEFAULT,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            "CreateScaleTarget",
        )?;
        Ok(Self {
            processor,
            queue,
            alloc,
            list,
            fence,
            target,
            from,
            to,
        })
    }

    /// Source extent behind the scaler.
    #[must_use]
    pub(crate) fn from(&self) -> PictureSize {
        self.from
    }

    /// Target extent behind the scaler.
    #[must_use]
    pub(crate) fn to(&self) -> PictureSize {
        self.to
    }

    /// `texture` scaled to the target extent on the GPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` for a source outside the scaler extent or when the
    /// driver refuses the scale.
    pub(crate) fn scale(&mut self, texture: &GpuTexture) -> Result<GpuTexture, D3d12Error> {
        if texture.size() != self.from {
            return Err(D3d12Error::Unsupported(
                "scale source outside scaler".to_owned(),
            ));
        }
        // Sound: the queue is idle; reset reuses the allocator and list.
        unsafe { self.alloc.Reset() }.map_err(|error| D3d12Error::Call {
            call: "ResetAllocator",
            hresult: error.code().0,
        })?;
        // Sound: the allocator outlives the reset.
        unsafe { self.list.Reset(&self.alloc) }.map_err(|error| D3d12Error::Call {
            call: "ResetList",
            hresult: error.code().0,
        })?;
        let source = texture.resource();
        // Sound: the barriers name live textures; the list is open.
        unsafe {
            self.list.ResourceBarrier(&[
                DeviceResources::barrier(
                    source,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
                ),
                DeviceResources::barrier(
                    &self.target,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
                ),
            ]);
        }
        let mut input = D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS::default();
        input.InputStream[0].pTexture2D = ManuallyDrop::new(Some(source.clone()));
        input.Transform.SourceRectangle = full_rect(self.from)?;
        input.Transform.DestinationRectangle = full_rect(self.to)?;
        input.Transform.Orientation = D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT;
        input.Flags = D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE;
        let mut output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS::default();
        output.OutputStream[0].pTexture2D = ManuallyDrop::new(Some(self.target.clone()));
        output.TargetRectangle = full_rect(self.to)?;
        // Sound: the argument structs outlive the fence wait below.
        unsafe {
            self.list.ProcessFrames(
                &self.processor,
                std::ptr::from_ref(&output),
                std::slice::from_ref(&input),
            );
            self.list.ResourceBarrier(&[
                DeviceResources::barrier(
                    source,
                    D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
                DeviceResources::barrier(
                    &self.target,
                    D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
                    D3D12_RESOURCE_STATE_COMMON,
                ),
            ]);
        }
        // Sound: the list holds only the barriers and scale above.
        unsafe { self.list.Close() }.map_err(|error| D3d12Error::Call {
            call: "Close",
            hresult: error.code().0,
        })?;
        // Sound: the list is closed; the cast holds the live list.
        let command: ID3D12CommandList = self.list.cast().map_err(|error| D3d12Error::Call {
            call: "CastCommandList",
            hresult: error.code().0,
        })?;
        self.fence.submit(&self.queue, &command)?;
        Ok(texture.rescaled(self.target.clone(), self.to))
    }
}

/// Video processor support for `from` to `to` in NV12.
fn check_support(
    video: &ID3D12VideoDevice,
    from: PictureSize,
    to: PictureSize,
) -> Result<(), D3d12Error> {
    let format = D3D12_VIDEO_FORMAT {
        Format: DXGI_FORMAT_NV12,
        ColorSpace: DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    };
    let rate = DXGI_RATIONAL {
        Numerator: 30,
        Denominator: 1,
    };
    let mut support = D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT {
        InputSample: D3D12_VIDEO_SAMPLE {
            Width: from.width(),
            Height: from.height(),
            Format: format,
        },
        InputFieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
        InputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
        InputFrameRate: rate,
        OutputFormat: format,
        OutputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
        OutputFrameRate: rate,
        ..Default::default()
    };
    crate::device::feature_support(
        video,
        D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
        &mut support,
        "ProcessSupport",
    )?;
    if !support
        .SupportFlags
        .contains(D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED)
    {
        return Err(D3d12Error::Unsupported(
            "video process unsupported".to_owned(),
        ));
    }
    let range = support.ScaleSupport.OutputSizeRange;
    let flags = support.ScaleSupport.Flags;
    if to.width() < range.MinWidth
        || to.width() > range.MaxWidth
        || to.height() < range.MinHeight
        || to.height() > range.MaxHeight
    {
        return Err(D3d12Error::Unsupported(
            "scale target outside process range".to_owned(),
        ));
    }
    if flags.contains(D3D12_VIDEO_SCALE_SUPPORT_FLAG_POW2_ONLY)
        && (!to.width().is_power_of_two() || !to.height().is_power_of_two())
    {
        return Err(D3d12Error::Unsupported(
            "scale target outside power-of-two".to_owned(),
        ));
    }
    if flags.contains(D3D12_VIDEO_SCALE_SUPPORT_FLAG_EVEN_DIMENSIONS_ONLY)
        && (!to.width().is_multiple_of(2) || !to.height().is_multiple_of(2))
    {
        return Err(D3d12Error::Unsupported(
            "scale target outside even extents".to_owned(),
        ));
    }
    Ok(())
}

/// Video processor from `from` to `to` in NV12.
fn create_processor(
    video: &ID3D12VideoDevice,
    from: PictureSize,
    to: PictureSize,
) -> Result<ID3D12VideoProcessor, D3d12Error> {
    let rate = DXGI_RATIONAL {
        Numerator: 30,
        Denominator: 1,
    };
    let output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC {
        Format: DXGI_FORMAT_NV12,
        ColorSpace: DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        AlphaFillMode: D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE,
        FrameRate: rate,
        ..Default::default()
    };
    let input = D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC {
        Format: DXGI_FORMAT_NV12,
        ColorSpace: DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        FrameRate: rate,
        SourceSizeRange: D3D12_VIDEO_SIZE_RANGE {
            MaxWidth: from.width(),
            MaxHeight: from.height(),
            MinWidth: from.width(),
            MinHeight: from.height(),
        },
        DestinationSizeRange: D3D12_VIDEO_SIZE_RANGE {
            MaxWidth: to.width(),
            MaxHeight: to.height(),
            MinWidth: to.width(),
            MinHeight: to.height(),
        },
        FilterFlags: D3D12_VIDEO_PROCESS_FILTER_FLAG_NONE,
        StereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_MONO,
        FieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
        DeinterlaceMode: D3D12_VIDEO_PROCESS_DEINTERLACE_FLAG_NONE,
        ..Default::default()
    };
    // Sound: the descriptors outlive the call.
    unsafe {
        video.CreateVideoProcessor(0, std::ptr::addr_of!(output), std::slice::from_ref(&input))
    }
    .map_err(|error| D3d12Error::Call {
        call: "CreateVideoProcessor",
        hresult: error.code().0,
    })
}

/// Full-frame rectangle for `size`.
fn full_rect(size: PictureSize) -> Result<RECT, D3d12Error> {
    let right =
        i32::try_from(size.width()).map_err(|_| D3d12Error::Parse("extent range".to_owned()))?;
    let bottom =
        i32::try_from(size.height()).map_err(|_| D3d12Error::Parse("extent range".to_owned()))?;
    Ok(RECT {
        left: 0,
        top: 0,
        right,
        bottom,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn open_probes_video_process_support() {
        let Ok(device) = crate::device::D3d12Device::open() else {
            return;
        };
        let from = rust_video_caps::common::PictureSize::new(320, 240).expect("nonzero extent");
        let to = rust_video_caps::common::PictureSize::new(160, 120).expect("nonzero extent");
        match super::Scaler::open(&device, from, to) {
            Ok(scaler) => {
                assert_eq!(scaler.from(), from);
                assert_eq!(scaler.to(), to);
            }
            Err(crate::error::D3d12Error::Unsupported(_)) => {}
            Err(error) => panic!("scale probe failed: {error}"),
        }
    }
}
