//! Frame printing for the VA-API decode examples.

use rust_video_harness::FrameHash;
use rust_video_vaapi::VaApiFrame;

/// Print `frames`, counting from `counter`.
pub fn print_frames(frames: &[VaApiFrame], counter: &mut usize) {
    for frame in frames {
        let identity = frame.identity();
        println!(
            "frame {counter} {}x{} gen {} hash {}",
            identity.size().width(),
            identity.size().height(),
            identity.generation(),
            &FrameHash::of(frame.nv12()).hex()[..16],
        );
        *counter += 1;
    }
}
