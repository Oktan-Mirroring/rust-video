//! VA-API display behind the caps `Device` trait.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::ffi::CStr;
use std::fs::File;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::error::{SkipReason, VaApiError};
use crate::ffi;

use rust_video_caps::traits::Device;

/// VA-API display over one DRM render node.
///
/// `Send`, never `Sync`: hand the display to another thread when idle,
/// but run every call on one thread. Concurrent VA-API calls on one
/// display are unsupported; the drivers serialize some paths and not
/// others, so the type refuses to share.
#[derive(Debug)]
pub struct VaApiDevice {
    /// Initialized VA-API display; terminated before the node below closes.
    display: ffi::VaDisplay,
    /// Render-node file, held open for the display lifetime.
    _node: OwnedFd,
    /// Driver description.
    name: String,
    /// Thread contract: `Send` without `Sync`.
    _thread: PhantomData<UnsafeCell<()>>,
}

impl VaApiDevice {
    /// DRM render-node directory.
    const DRI_DIR: &'static str = "/dev/dri";

    /// Display over the first H.264-capable render node.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the machine has no render node, no display
    /// initializes, or no display decodes H.264 into NV12.
    pub fn open() -> Result<Self, VaApiError> {
        let mut nodes = render_nodes()?;
        if nodes.is_empty() {
            return Err(SkipReason::NoRenderNode.into());
        }
        nodes.sort();
        let mut initialized = false;
        let mut vld = false;
        for path in &nodes {
            let node = match File::options().read(true).write(true).open(path) {
                Ok(node) => node.into(),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    log::warn!(
                        "va-api: render node {} not permitted ({error})",
                        path.display()
                    );
                    continue;
                }
                Err(error) => {
                    log::debug!("va-api: render node {} refused ({error})", path.display());
                    continue;
                }
            };
            let Some(display) = Display::open(&node) else {
                continue;
            };
            initialized = true;
            for profile in display.h264_profiles() {
                if !display.has_entrypoint(profile, ffi::VA_ENTRYPOINT_VLD) {
                    continue;
                }
                vld = true;
                if display.supports_nv12(profile) {
                    let name = display.vendor();
                    return Ok(Self {
                        display: display.leak(),
                        _node: node,
                        name,
                        _thread: PhantomData,
                    });
                }
            }
        }
        if !initialized {
            return Err(SkipReason::NoDisplay.into());
        }
        if !vld {
            return Err(SkipReason::NoH264Vld.into());
        }
        Err(SkipReason::NoNv12Target.into())
    }

    /// Initialized VA-API display.
    #[must_use]
    pub(crate) fn display(&self) -> ffi::VaDisplay {
        self.display
    }

    /// True when `profile` offers `entrypoint` on this display.
    #[must_use]
    pub(crate) fn supports(
        &self,
        profile: std::os::raw::c_int,
        entrypoint: std::os::raw::c_int,
    ) -> bool {
        entrypoints(self.display, profile).contains(&entrypoint)
    }

    /// Config attribute value, absent when the call fails or unsupported.
    #[must_use]
    pub(crate) fn config_attr(
        &self,
        profile: std::os::raw::c_int,
        entrypoint: std::os::raw::c_int,
        attr_type: std::os::raw::c_int,
    ) -> Option<u32> {
        config_attr(self.display, profile, entrypoint, attr_type)
    }

    /// True when the display exports surfaces as DRM PRIME file descriptors.
    #[must_use]
    pub(crate) fn dmabuf_export(&self) -> bool {
        // Sound: every handle below is live for its call; the probe
        // surface and any exported descriptors close on every path.
        unsafe {
            let mut surface = 0;
            let created = ffi::vaCreateSurfaces(
                self.display,
                ffi::VA_RT_FORMAT_YUV420,
                64,
                64,
                &raw mut surface,
                1,
                std::ptr::null_mut(),
                0,
            );
            if created != ffi::VA_STATUS_SUCCESS {
                return false;
            }
            let mut descriptor = std::mem::MaybeUninit::<ffi::VaDrmPrimeDescriptor>::uninit();
            let status = ffi::vaExportSurfaceHandle(
                self.display,
                surface,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                ffi::VA_EXPORT_SURFACE_READ_ONLY | ffi::VA_EXPORT_SURFACE_SEPARATE_LAYERS,
                descriptor.as_mut_ptr().cast(),
            );
            if status == ffi::VA_STATUS_SUCCESS {
                let descriptor = descriptor.assume_init();
                let count = usize::try_from(descriptor.num_objects).unwrap_or(0).min(4);
                for object in descriptor.objects.iter().take(count) {
                    if object.fd >= 0 {
                        drop(OwnedFd::from_raw_fd(object.fd));
                    }
                }
            }
            ffi::vaDestroySurfaces(self.display, &raw mut surface, 1);
            status == ffi::VA_STATUS_SUCCESS
        }
    }
}

impl Device for VaApiDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for VaApiDevice {
    fn drop(&mut self) {
        // Sound: `display` initialized in `open` and never terminated before drop.
        unsafe {
            ffi::vaTerminate(self.display);
        }
    }
}

// Sound: a move hands exclusive ownership to the new thread; the
// display terminates wherever the value drops, never shared.
unsafe impl Send for VaApiDevice {}

/// Render-node paths under `/dev/dri`.
fn render_nodes() -> Result<Vec<std::path::PathBuf>, VaApiError> {
    let Ok(entries) = std::fs::read_dir(VaApiDevice::DRI_DIR) else {
        return Err(SkipReason::NoRenderNode.into());
    };
    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with("renderD") {
            nodes.push(entry.path());
        }
    }
    Ok(nodes)
}

/// Initialized VA-API display, terminated on drop unless leaked.
struct Display {
    /// Initialized display handle.
    display: ffi::VaDisplay,
}

impl Display {
    /// Display over `node`, or `None` when initialization fails.
    fn open(node: &OwnedFd) -> Option<Self> {
        // Sound: `vaGetDisplayDRM` borrows the live descriptor without taking
        // ownership; `vaInitialize` runs before any other call on the display.
        let display = unsafe { ffi::vaGetDisplayDRM(node.as_raw_fd()) };
        if display.is_null() {
            return None;
        }
        let mut major = 0;
        let mut minor = 0;
        // Sound: `display` is non-null and freshly opened.
        let status = unsafe { ffi::vaInitialize(display, &raw mut major, &raw mut minor) };
        if status != ffi::VA_STATUS_SUCCESS {
            // Sound: `display` came from `vaGetDisplayDRM` on this node.
            unsafe {
                ffi::vaTerminate(display);
            }
            return None;
        }
        Some(Self { display })
    }

    /// Display handle, escaping termination by the owner.
    fn leak(self) -> ffi::VaDisplay {
        let display = self.display;
        std::mem::forget(self);
        display
    }

    /// Driver description.
    fn vendor(&self) -> String {
        // Sound: the driver owns the string for the display lifetime.
        let raw = unsafe { ffi::vaQueryVendorString(self.display) };
        if raw.is_null() {
            return "vaapi".to_owned();
        }
        // Sound: `vaQueryVendorString` returns a NUL-terminated string.
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned()
    }

    /// H.264 profiles the display reports, most capable first.
    fn h264_profiles(&self) -> Vec<std::os::raw::c_int> {
        // Sound: all calls below take the initialized display; each list is
        // sized by its `vaMaxNum*` capacity query.
        unsafe {
            let capacity = usize::try_from(ffi::vaMaxNumProfiles(self.display)).unwrap_or(0);
            let mut profiles = vec![0; capacity];
            let mut count = 0;
            let status =
                ffi::vaQueryConfigProfiles(self.display, profiles.as_mut_ptr(), &raw mut count);
            if status != ffi::VA_STATUS_SUCCESS {
                return Vec::new();
            }
            profiles.truncate(usize::try_from(count).unwrap_or(0));
            let mut h264 = Vec::new();
            for profile in [
                ffi::VA_PROFILE_H264_HIGH,
                ffi::VA_PROFILE_H264_MAIN,
                ffi::VA_PROFILE_H264_CONSTRAINED_BASELINE,
            ] {
                if profiles.contains(&profile) {
                    h264.push(profile);
                }
            }
            h264
        }
    }

    /// True when `profile` offers `entrypoint`.
    fn has_entrypoint(
        &self,
        profile: std::os::raw::c_int,
        entrypoint: std::os::raw::c_int,
    ) -> bool {
        entrypoints(self.display, profile).contains(&entrypoint)
    }

    /// True when `profile` decodes into NV12.
    fn supports_nv12(&self, profile: std::os::raw::c_int) -> bool {
        config_attr(
            self.display,
            profile,
            ffi::VA_ENTRYPOINT_VLD,
            ffi::VA_CONFIG_ATTRIB_RT_FORMAT,
        )
        .is_some_and(|value| value & ffi::VA_RT_FORMAT_YUV420 != 0)
    }
}

/// Entrypoints `profile` offers on `display`.
fn entrypoints(display: ffi::VaDisplay, profile: std::os::raw::c_int) -> Vec<std::os::raw::c_int> {
    // Sound: the entrypoint list is sized by its capacity query.
    unsafe {
        let capacity = usize::try_from(ffi::vaMaxNumEntrypoints(display)).unwrap_or(0);
        let mut entrypoints = vec![0; capacity];
        let mut count = 0;
        let status = ffi::vaQueryConfigEntrypoints(
            display,
            profile,
            entrypoints.as_mut_ptr(),
            &raw mut count,
        );
        if status != ffi::VA_STATUS_SUCCESS {
            return Vec::new();
        }
        let reported = usize::try_from(count).unwrap_or(0).min(entrypoints.len());
        entrypoints.truncate(reported);
        entrypoints
    }
}

/// Config attribute value, absent when the call fails or unsupported.
fn config_attr(
    display: ffi::VaDisplay,
    profile: std::os::raw::c_int,
    entrypoint: std::os::raw::c_int,
    attr_type: std::os::raw::c_int,
) -> Option<u32> {
    // Sound: the single attribute is fully initialized.
    unsafe {
        let mut attrib = ffi::VaConfigAttrib {
            attr_type,
            value: 0,
        };
        let status = ffi::vaGetConfigAttributes(display, profile, entrypoint, &raw mut attrib, 1);
        (status == ffi::VA_STATUS_SUCCESS && attrib.value != ffi::VA_ATTRIB_NOT_SUPPORTED)
            .then_some(attrib.value)
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // Sound: `display` initialized in `open` and never terminated before drop.
        unsafe {
            ffi::vaTerminate(self.display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VaApiDevice;

    #[test]
    fn device_moves_across_threads() {
        fn is_send<T: Send>() {}
        is_send::<VaApiDevice>();
    }

    #[test]
    fn open_selects_an_h264_node() {
        match VaApiDevice::open() {
            Ok(device) => {
                assert!(!rust_video_caps::traits::Device::name(&device).is_empty());
            }
            Err(crate::error::VaApiError::Skipped(reason)) => {
                eprintln!("skipped: no va-api hardware ({reason})");
            }
            Err(error) => panic!("unexpected va-api failure: {error}"),
        }
    }
}
