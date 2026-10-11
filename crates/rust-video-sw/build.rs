//! Link search for the ffmpeg system libraries.
//!
//! Unix links the system libavcodec/libavutil straight from the `#[link]`
//! attrs. Windows has no system copy, so `FFMPEG_LIB_DIR` points at the
//! directory holding `avcodec.lib` and `avutil.lib`, mirroring how
//! `env-libvpx-sys` takes `VPX_LIB_DIR`.

/// Build script entry point.
///
/// # Panics
///
/// Panics when targeting Windows without `FFMPEG_LIB_DIR` set.
fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_LIB_DIR");
    let windows = std::env::var("TARGET").is_ok_and(|target| target.contains("windows"));
    if !windows {
        return;
    }
    let Some(dir) = std::env::var_os("FFMPEG_LIB_DIR") else {
        panic!(
            "rust-video-sw: Windows has no system libavcodec; set FFMPEG_LIB_DIR \
             to the directory holding avcodec.lib and avutil.lib \
             (vcpkg: <vcpkg>/installed/x64-windows/lib after \
             `vcpkg install ffmpeg[dav1d] libvpx --triplet x64-windows`)"
        );
    };
    println!("cargo:rustc-link-search=native={}", dir.display());
}
