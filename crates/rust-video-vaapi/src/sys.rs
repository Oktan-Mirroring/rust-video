//! Linux libva entry points over the portable `ffi` layouts.

#![allow(unsafe_code)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};

use crate::error::VaApiError;
use crate::ffi::{
    VaBufferId, VaConfigAttrib, VaConfigId, VaContextId, VaDisplay, VaImage, VaImageFormat,
    VaImageId, VaStatus, VaSurfaceId,
};

#[link(name = "va")]
#[link(name = "va-drm")]
unsafe extern "C" {
    /// DRM display for `fd`.
    pub fn vaGetDisplayDRM(fd: c_int) -> VaDisplay;
    /// Initialize `dpy`, reporting its version.
    pub fn vaInitialize(dpy: VaDisplay, major: *mut c_int, minor: *mut c_int) -> VaStatus;
    /// Release `dpy`.
    pub fn vaTerminate(dpy: VaDisplay) -> VaStatus;
    /// Driver description string.
    pub fn vaQueryVendorString(dpy: VaDisplay) -> *const c_char;

    /// Profile-list capacity.
    pub fn vaMaxNumProfiles(dpy: VaDisplay) -> c_int;
    /// Entrypoint-list capacity.
    pub fn vaMaxNumEntrypoints(dpy: VaDisplay) -> c_int;
    /// Profiles supported by `dpy`.
    pub fn vaQueryConfigProfiles(
        dpy: VaDisplay,
        profiles: *mut c_int,
        count: *mut c_int,
    ) -> VaStatus;
    /// Entrypoints supported for `profile`.
    pub fn vaQueryConfigEntrypoints(
        dpy: VaDisplay,
        profile: c_int,
        entrypoints: *mut c_int,
        count: *mut c_int,
    ) -> VaStatus;
    /// Attribute values for a profile/entrypoint pair.
    pub fn vaGetConfigAttributes(
        dpy: VaDisplay,
        profile: c_int,
        entrypoint: c_int,
        attribs: *mut VaConfigAttrib,
        count: c_int,
    ) -> VaStatus;
    /// Config for a profile/entrypoint pair.
    pub fn vaCreateConfig(
        dpy: VaDisplay,
        profile: c_int,
        entrypoint: c_int,
        attribs: *mut VaConfigAttrib,
        count: c_int,
        config: *mut VaConfigId,
    ) -> VaStatus;
    /// Release `config`.
    pub fn vaDestroyConfig(dpy: VaDisplay, config: VaConfigId) -> VaStatus;
    /// Render-target surfaces.
    #[allow(clippy::too_many_arguments)]
    pub fn vaCreateSurfaces(
        dpy: VaDisplay,
        format: c_uint,
        width: c_uint,
        height: c_uint,
        surfaces: *mut VaSurfaceId,
        count: c_uint,
        attribs: *mut c_void,
        attrib_count: c_uint,
    ) -> VaStatus;
    /// Release `count` surfaces.
    pub fn vaDestroySurfaces(dpy: VaDisplay, surfaces: *mut VaSurfaceId, count: c_int) -> VaStatus;
    /// Decode context over `render_targets`.
    #[allow(clippy::too_many_arguments)]
    pub fn vaCreateContext(
        dpy: VaDisplay,
        config: VaConfigId,
        width: c_int,
        height: c_int,
        flag: c_int,
        render_targets: *mut VaSurfaceId,
        target_count: c_int,
        context: *mut VaContextId,
    ) -> VaStatus;
    /// Release `context`.
    pub fn vaDestroyContext(dpy: VaDisplay, context: VaContextId) -> VaStatus;
    /// Buffer of `elements` items of `size` bytes holding `data`.
    #[allow(clippy::too_many_arguments)]
    pub fn vaCreateBuffer(
        dpy: VaDisplay,
        context: VaContextId,
        buffer_type: c_int,
        size: c_uint,
        elements: c_uint,
        data: *mut c_void,
        buffer: *mut VaBufferId,
    ) -> VaStatus;
    /// Release `buffer`.
    pub fn vaDestroyBuffer(dpy: VaDisplay, buffer: VaBufferId) -> VaStatus;
    /// Start rendering `target`.
    pub fn vaBeginPicture(dpy: VaDisplay, context: VaContextId, target: VaSurfaceId) -> VaStatus;
    /// Submit `count` buffers.
    pub fn vaRenderPicture(
        dpy: VaDisplay,
        context: VaContextId,
        buffers: *mut VaBufferId,
        count: c_int,
    ) -> VaStatus;
    /// Finish rendering.
    pub fn vaEndPicture(dpy: VaDisplay, context: VaContextId) -> VaStatus;
    /// Block until `surface` finishes decoding.
    pub fn vaSyncSurface(dpy: VaDisplay, surface: VaSurfaceId) -> VaStatus;
    /// Image view over `surface`.
    pub fn vaDeriveImage(dpy: VaDisplay, surface: VaSurfaceId, image: *mut VaImage) -> VaStatus;
    /// Release `image`.
    pub fn vaDestroyImage(dpy: VaDisplay, image: VaImageId) -> VaStatus;
    /// Map `buffer` into client memory.
    pub fn vaMapBuffer(dpy: VaDisplay, buffer: VaBufferId, data: *mut *mut c_void) -> VaStatus;
    /// Unmap `buffer`.
    pub fn vaUnmapBuffer(dpy: VaDisplay, buffer: VaBufferId) -> VaStatus;
    /// Static text for `status`.
    pub fn vaStatusStr(status: VaStatus) -> *const c_char;
    /// Export `surface` into `descriptor` for `mem_type`.
    pub fn vaExportSurfaceHandle(
        dpy: VaDisplay,
        surface: VaSurfaceId,
        mem_type: c_uint,
        flags: c_uint,
        descriptor: *mut c_void,
    ) -> VaStatus;
    /// Image-format-list capacity.
    pub fn vaMaxNumImageFormats(dpy: VaDisplay) -> c_int;
    /// Image formats supported by `dpy`.
    pub fn vaQueryImageFormats(
        dpy: VaDisplay,
        formats: *mut VaImageFormat,
        count: *mut c_int,
    ) -> VaStatus;
    /// Image of `format` at `width` x `height`.
    pub fn vaCreateImage(
        dpy: VaDisplay,
        format: *mut VaImageFormat,
        width: c_int,
        height: c_int,
        image: *mut VaImage,
    ) -> VaStatus;
    /// Copy `image` onto `surface`, scaled to the destination rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn vaPutImage(
        dpy: VaDisplay,
        surface: VaSurfaceId,
        image: VaImageId,
        src_x: c_int,
        src_y: c_int,
        src_width: c_uint,
        src_height: c_uint,
        dest_x: c_int,
        dest_y: c_int,
        dest_width: c_uint,
        dest_height: c_uint,
    ) -> VaStatus;
}

/// VA-API status as a result, carrying the driver status text.
pub(crate) fn status(status: i32) -> Result<(), VaApiError> {
    if status == crate::ffi::VA_STATUS_SUCCESS {
        return Ok(());
    }
    // Sound: `vaStatusStr` returns a static NUL-terminated string.
    let raw = unsafe { vaStatusStr(status) };
    let message = if raw.is_null() {
        format!("status {status}")
    } else {
        // Sound: `vaStatusStr` returned non-null above.
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned()
    };
    Err(VaApiError::Call { status, message })
}
