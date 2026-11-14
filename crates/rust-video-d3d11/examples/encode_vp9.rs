//! VP9 encode on the D3D11 tier, from synthetic NV12 to packets.

#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "windows")]
use rust_video_d3d11::{Codec, D3d11Device, D3d11Encoder};
#[cfg(target_os = "windows")]
use rust_video_harness::Synth;

#[cfg(target_os = "windows")]
fn main() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d11 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    let mut encoder = match D3d11Encoder::open(&device, Codec::Vp9) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no vp9 encoder ({error})");
            return;
        }
    };
    println!(
        "mft: {} (d3d11: {})",
        encoder.mft_name(),
        encoder.uses_d3d11()
    );
    let frame = Synth::grey_nv12(64, 48);
    let mut packets = 0;
    let mut bytes = 0;
    for _ in 0..8 {
        match encoder.encode(&frame) {
            Ok(out) => {
                packets += out.len();
                bytes += out.iter().map(|packet| packet.data().len()).sum::<usize>();
            }
            Err(error) => {
                println!("ERROR: encode: {error}");
                std::process::exit(1);
            }
        }
    }
    match encoder.flush() {
        Ok(out) => {
            packets += out.len();
            bytes += out.iter().map(|packet| packet.data().len()).sum::<usize>();
        }
        Err(error) => {
            println!("ERROR: flush: {error}");
            std::process::exit(1);
        }
    }
    println!("encoded {packets} packet(s), {bytes} byte(s)");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
