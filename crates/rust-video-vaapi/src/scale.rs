//! VA-API video processing: on-device scale between surface sizes.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::os::raw::c_int;
use std::sync::Arc;

use crate::buffer::{bytes_of, VaBuffer};
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi;
use crate::h264::decoder::VaApiFrame;
use crate::sys;

use rust_video_caps::common::PictureSize;
use rust_video_caps::transcode::FallbackCause;

/// VA-API scaler over one display.
///
/// `Send`, never `Sync`: hand the scaler to another thread when idle,
/// but run every call on one thread. The contract matches the display
/// behind it; see `VaApiDevice`.
pub struct Scaler {
    /// Owned processing session, dropped after the buffers above.
    pipeline: VaBuffer,
    /// Output surface index.
    next: usize,
    /// True per output without unwaited work.
    settled: [bool; Self::OUTPUTS],
    /// Owned session, dropped after the pipeline buffer above.
    session: Session,
    /// Source size in luma samples.
    from: PictureSize,
    /// Target size in luma samples.
    to: PictureSize,
    /// Thread contract: `Send` without `Sync`.
    _thread: PhantomData<UnsafeCell<()>>,
}

impl Scaler {
    /// Output surface count.
    const OUTPUTS: usize = 2;

    /// Scaler over `device` from `from` to `to`.
    ///
    /// The device must outlive the scaler.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver lacks the video-processing
    /// entrypoint or refuses the pipeline for the size pair.
    pub fn open(
        device: &VaApiDevice,
        from: PictureSize,
        to: PictureSize,
    ) -> Result<Self, VaApiError> {
        if !device.supports(ffi::VA_PROFILE_NONE, ffi::VA_ENTRYPOINT_VIDEO_PROC) {
            return Err(VaApiError::Unsupported(
                "video processing entrypoint absent".to_owned(),
            ));
        }
        let session = Session::open(device, to)?;
        let display = device.display();
        let pipeline = VaBuffer::empty(
            display,
            session.inner.context,
            ffi::VA_PROC_PIPELINE_PARAMETER_BUFFER_TYPE,
            size_of::<ffi::VaProcPipelineParameterBuffer>(),
        )?;
        Ok(Self {
            pipeline,
            next: 0,
            settled: [true; Self::OUTPUTS],
            session,
            from,
            to,
            _thread: PhantomData,
        })
    }

    /// Source size in luma samples.
    #[must_use]
    pub const fn from(&self) -> PictureSize {
        self.from
    }

    /// Target size in luma samples.
    #[must_use]
    pub const fn to(&self) -> PictureSize {
        self.to
    }

    /// Device surface behind `frame`, scaled to the target size.
    ///
    /// Returns before the scale finishes; the consumer syncs the
    /// output at use. The reused output waits its own previous work
    /// first, so two outputs overlap one scale with one consume.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame has no device surface, lives on
    /// another display, misses the source size, or the backend
    /// refuses the scale.
    pub fn scale(&mut self, frame: &VaApiFrame) -> Result<ScaledSurface, VaApiError> {
        let Some((display, surface)) = frame.device_handle() else {
            return Err(VaApiError::Unsupported(
                "scale without a device surface".to_owned(),
            ));
        };
        if display != self.session.inner.display {
            return Err(VaApiError::Unsupported(
                FallbackCause::DeviceMismatch.to_string(),
            ));
        }
        let size = frame.identity().size();
        if size != self.from {
            return Err(VaApiError::Unsupported(format!(
                "scale source {}x{} vs pipeline {}x{}",
                size.width(),
                size.height(),
                self.from.width(),
                self.from.height(),
            )));
        }
        let slot = self.next;
        let target = self.session.inner.surfaces[slot];
        if !self.settled[slot] {
            // Sound: the surface was just submitted.
            sys::status(unsafe { sys::vaSyncSurface(self.session.inner.display, target) })?;
            self.settled[slot] = true;
        }
        self.next = (self.next + 1) % Self::OUTPUTS;
        let src_rect = ffi::VaRectangle {
            x: 0,
            y: 0,
            width: u16::try_from(self.from.width()).map_err(|_| {
                VaApiError::Unsupported("dimensions outside driver range".to_owned())
            })?,
            height: u16::try_from(self.from.height()).map_err(|_| {
                VaApiError::Unsupported("dimensions outside driver range".to_owned())
            })?,
        };
        let params = ffi::VaProcPipelineParameterBuffer {
            surface,
            surface_region: (&raw const src_rect).cast(),
            surface_color_standard: ffi::VA_PROC_COLOR_STANDARD_NONE,
            output_region: std::ptr::null(),
            output_background_color: 0,
            output_color_standard: ffi::VA_PROC_COLOR_STANDARD_NONE,
            pipeline_flags: 0,
            filter_flags: 0,
            filters: std::ptr::null_mut(),
            num_filters: 0,
            forward_references: std::ptr::null_mut(),
            num_forward_references: 0,
            backward_references: std::ptr::null_mut(),
            num_backward_references: 0,
            rotation_state: 0,
            blend_state: std::ptr::null(),
            mirror_state: 0,
            additional_outputs: std::ptr::null_mut(),
            num_additional_outputs: 0,
            input_surface_flag: 0,
            output_surface_flag: 0,
            input_color_properties: ffi::VaProcColorProperties::default(),
            output_color_properties: ffi::VaProcColorProperties::default(),
            processing_mode: ffi::VA_PROC_DEFAULT_MODE,
            output_hdr_metadata: std::ptr::null_mut(),
            va_reserved: [0; 16],
        };
        self.pipeline.rewrite_uninit(bytes_of(&params))?;
        let display = self.session.inner.display;
        let context = self.session.inner.context;
        let mut id = self.pipeline.id();
        // Sound: the context and target are live; the pipeline buffer holds one struct.
        sys::status(unsafe { sys::vaBeginPicture(display, context, target) })?;
        let rendered = unsafe { sys::vaRenderPicture(display, context, &raw mut id, 1) };
        let end = unsafe { sys::vaEndPicture(display, context) };
        let outcome = sys::status(rendered).and(sys::status(end));
        if outcome.is_err() {
            // Sound: the target was just submitted.
            let _ = unsafe { sys::vaSyncSurface(display, target) };
        }
        outcome?;
        self.settled[slot] = false;
        Ok(ScaledSurface {
            session: Arc::clone(&self.session.inner),
            surface: target,
            size: self.to,
        })
    }
}

/// Scaled device surface at the pipeline target size.
///
/// A live surface keeps its own resources alive: dropping the decoder never invalidates outstanding surfaces, and dropping a surface releases its slot.
pub struct ScaledSurface {
    /// Owned processing session behind the surface; the context and
    /// output surfaces stay alive past scaler drop while held.
    session: Arc<SessionInner>,
    /// Surface handle.
    surface: ffi::VaSurfaceId,
    /// Surface size in luma samples.
    size: PictureSize,
}

impl ScaledSurface {
    /// Display owning the surface.
    #[must_use]
    pub(crate) fn display(&self) -> ffi::VaDisplay {
        self.session.display
    }

    /// Surface handle.
    #[must_use]
    pub(crate) fn surface(&self) -> ffi::VaSurfaceId {
        self.surface
    }

    /// Surface size in luma samples.
    #[must_use]
    pub const fn size(&self) -> PictureSize {
        self.size
    }
}

/// Owned processing session over a shared inner.
struct Session {
    /// Config, context, and output surfaces, shared with live outputs.
    inner: Arc<SessionInner>,
}

/// Owned processing session: config, context, and output surfaces.
///
/// Shared with live outputs: dropping the scaler destroys the
/// session only once the last output holding it drops.
struct SessionInner {
    /// Initialized display.
    display: ffi::VaDisplay,
    /// Processing configuration.
    config: ffi::VaConfigId,
    /// Processing context.
    context: ffi::VaContextId,
    /// Output surfaces at the target size.
    surfaces: Vec<ffi::VaSurfaceId>,
    /// Surface count as the driver takes it.
    count_c: c_int,
}

impl Session {
    /// Session rendering into `to` surfaces.
    fn open(device: &VaApiDevice, to: PictureSize) -> Result<Self, VaApiError> {
        let display = device.display();
        let width_c = c_int::try_from(to.width())
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let height_c = c_int::try_from(to.height())
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let mut config = 0;
        // Sound: the display is initialized; no attributes travel with a
        // processing config.
        sys::status(unsafe {
            sys::vaCreateConfig(
                display,
                ffi::VA_PROFILE_NONE,
                ffi::VA_ENTRYPOINT_VIDEO_PROC,
                std::ptr::null_mut(),
                0,
                &raw mut config,
            )
        })?;
        let count = Scaler::OUTPUTS;
        let mut surfaces = vec![0; count];
        // Sound: the surface array holds `count` live entries.
        let status = unsafe {
            sys::vaCreateSurfaces(
                display,
                ffi::VA_RT_FORMAT_YUV420,
                to.width(),
                to.height(),
                surfaces.as_mut_ptr(),
                u32::try_from(count).map_err(|_| {
                    VaApiError::Unsupported("dimensions outside driver range".to_owned())
                })?,
                std::ptr::null_mut(),
                0,
            )
        };
        if let Err(error) = sys::status(status) {
            // Sound: `config` was created above.
            unsafe {
                sys::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        let count_c = c_int::try_from(count)
            .map_err(|_| VaApiError::Unsupported("dimensions outside driver range".to_owned()))?;
        let mut context = 0;
        // Sound: the surface array outlives context creation.
        let status = unsafe {
            sys::vaCreateContext(
                display,
                config,
                width_c,
                height_c,
                ffi::VA_PROGRESSIVE,
                surfaces.as_mut_ptr(),
                count_c,
                &raw mut context,
            )
        };
        if let Err(error) = sys::status(status) {
            // Sound: the surfaces and config were created above.
            unsafe {
                sys::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                sys::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        Ok(Self {
            inner: Arc::new(SessionInner {
                display,
                config,
                context,
                surfaces,
                count_c,
            }),
        })
    }
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        // Sound: context, surfaces, and config were created in `open`.
        unsafe {
            sys::vaDestroyContext(self.display, self.context);
            sys::vaDestroySurfaces(self.display, self.surfaces.as_mut_ptr(), self.count_c);
            sys::vaDestroyConfig(self.display, self.config);
        }
    }
}

// Sound: moves hand ownership to the new thread; the last drop
// destroys the driver objects, never shared.
unsafe impl Send for SessionInner {}

// Sound: every field is read-only after `open`; shared references
// only copy handles out.
unsafe impl Sync for SessionInner {}

// Sound: a move hands exclusive ownership to the new thread; the
// session destroys wherever the value drops, never shared.
unsafe impl Send for Scaler {}
