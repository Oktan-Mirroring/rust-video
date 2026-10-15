//! Cropped-stream GPU transcode: passthrough plus scale.
//!
//! Skips without hardware.

#[path = "../examples/common/bits.rs"]
mod bits;
#[path = "../examples/common/h264.rs"]
mod h264;

use h264::{
    annex_b_h264_packets, annex_b_h264_packets_cropped, annex_b_h264_packets_cropped_sized,
};

#[cfg(not(target_os = "windows"))]
use rust_video_d3d12::SkipReason;
use rust_video_d3d12::{Codec, D3d12Device, D3d12Error, H264Decoder, Transcoder};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::{Outcome, Route};

/// Live D3D12 device, absent without hardware.
fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("d3d12 skipped: {error}");
            #[cfg(not(target_os = "windows"))]
            assert!(matches!(
                error,
                D3d12Error::Skipped(SkipReason::PlatformUnsupported)
            ));
            None
        }
    }
}

/// Live H.264 transcoder, absent without hardware.
fn transcoder(device: &D3d12Device) -> Option<Transcoder<'_>> {
    Transcoder::open(device, Codec::H264, Codec::H264).ok()
}

/// Bitstream bytes behind `outcome`.
fn bytes_of(outcome: &Outcome) -> usize {
    outcome
        .packets()
        .iter()
        .map(|packet| packet.data().len())
        .sum()
}

/// Bitstream bytes through `transcoder`, every hop on the GPU route.
fn drive(transcoder: &mut Transcoder) -> usize {
    let packets = annex_b_h264_packets()
        .into_iter()
        .chain(annex_b_h264_packets_cropped())
        .collect();
    drive_raw(transcoder, packets)
}

/// Bitstream bytes for `raws` through `transcoder`, every hop on the GPU route.
fn drive_raw(transcoder: &mut Transcoder, raws: Vec<Vec<u8>>) -> usize {
    let mut bytes = 0;
    for raw in raws {
        let packet = Packet::new(Bytes::from(raw));
        let outcome = transcoder.transcode(&packet).expect("cropped transcode");
        assert!(matches!(outcome.plan().route(), Route::Gpu));
        bytes += bytes_of(&outcome);
    }
    let outcome = transcoder.flush().expect("cropped flush");
    assert!(matches!(outcome.plan().route(), Route::Gpu));
    bytes + bytes_of(&outcome)
}

/// 320x224-visible stream through GPU passthrough.
#[test]
fn cropped_stream_transcodes_on_gpu_passthrough() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    assert!(drive(&mut transcoder) > 0);
}

/// 640x360-visible stream through GPU passthrough on a padded session.
#[test]
fn cropped_360p_transcodes_on_gpu_passthrough() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    let raws = annex_b_h264_packets_cropped_sized(39, 22, 4, 22);
    assert!(drive_raw(&mut transcoder, raws) > 0);
}

/// 1920x1080-visible stream through GPU passthrough on a padded session.
#[test]
fn cropped_1080p_transcodes_on_gpu_passthrough() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    let raws = annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    assert!(drive_raw(&mut transcoder, raws) > 0);
}

/// Transcoded bitstream bytes for `raws` through `transcoder`.
fn transcode_bytes(transcoder: &mut Transcoder, raws: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for raw in raws {
        let packet = Packet::new(Bytes::from(raw));
        let outcome = transcoder.transcode(&packet).expect("cropped transcode");
        assert!(matches!(outcome.plan().route(), Route::Gpu));
        for packet in outcome.packets() {
            bytes.extend_from_slice(packet.data());
        }
    }
    let outcome = transcoder.flush().expect("cropped flush");
    assert!(matches!(outcome.plan().route(), Route::Gpu));
    for packet in outcome.packets() {
        bytes.extend_from_slice(packet.data());
    }
    bytes
}

/// Luma mean of `nv12` at `width` by `height`.
fn luma_mean(nv12: &[u8], width: usize, height: usize) -> u64 {
    let y_len = width * height;
    let sum: u64 = nv12[..y_len].iter().map(|sample| u64::from(*sample)).sum();
    sum / u64::try_from(y_len).expect("nonzero extent")
}

/// Cropped-stream output decodes back to nonzero pixels.
#[test]
fn cropped_stream_output_holds_pixels() {
    let Some(device) = device() else {
        return;
    };
    let Some(mut transcoder) = transcoder(&device) else {
        return;
    };
    let raws = annex_b_h264_packets()
        .into_iter()
        .chain(annex_b_h264_packets_cropped())
        .collect();
    let bytes = transcode_bytes(&mut transcoder, raws);
    assert!(!bytes.is_empty());
    let mut decoder = H264Decoder::open(&device);
    let packet = Packet::new(Bytes::from(bytes));
    let mut frames = decoder.decode(&packet).expect("oracle decode");
    frames.extend(decoder.flush().expect("oracle drain"));
    assert!(!frames.is_empty());
    for frame in &frames {
        let size = frame.identity().size();
        let (width, height) = (size.width() as usize, size.height() as usize);
        assert_eq!((width, height), (320, 224));
        assert_eq!(frame.nv12().len(), width * height * 3 / 2);
        assert!(
            frame.nv12().iter().any(|sample| *sample != 0),
            "transcoded content is nonzero"
        );
        println!(
            "xcode oracle: {}x{} luma mean {}",
            width,
            height,
            luma_mean(frame.nv12(), width, height)
        );
    }
}

/// 320x224-visible stream through the GPU scale hop.
#[test]
fn cropped_stream_transcodes_on_gpu_scale() {
    let Some(device) = device() else {
        return;
    };
    let target = PictureSize::new(160, 112).expect("nonzero extent");
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder.with_target(target),
        Err(_) => return,
    };
    let mut bytes = 0;
    let packets = annex_b_h264_packets()
        .into_iter()
        .chain(annex_b_h264_packets_cropped());
    for raw in packets {
        let packet = Packet::new(Bytes::from(raw));
        match transcoder.transcode(&packet) {
            Ok(outcome) => {
                assert!(matches!(outcome.plan().route(), Route::Gpu));
                bytes += bytes_of(&outcome);
            }
            Err(D3d12Error::Unsupported(error)) if error.contains("scale") => {
                println!("d3d12 scale skipped: {error}");
                return;
            }
            Err(error) => panic!("cropped scale transcode: {error}"),
        }
    }
    let outcome = transcoder.flush().expect("cropped scale flush");
    assert!(matches!(outcome.plan().route(), Route::Gpu));
    bytes += bytes_of(&outcome);
    assert!(bytes > 0);
}
