use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vulkan::{CodecOp, H264Decoder, VulkanDevice};

fn main() {
    let vector = std::env::var("VKPROBE_VECTOR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("h264-high.h264")
            .to_string_lossy()
            .into_owned()
    });
    let out_path = std::env::var("VKPROBE_OUT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("vk_all.yuv")
            .to_string_lossy()
            .into_owned()
    });
    let Ok(bytes) = std::fs::read(&vector) else {
        println!("vkprobe: no vector at {vector}; skipped");
        return;
    };
    let bytes = match std::env::var("VKPROBE_TRUNC_NALS")
        .ok()
        .and_then(|text| text.parse().ok())
    {
        Some(k) => truncate_nals(&bytes, k),
        None => bytes,
    };
    let device = match VulkanDevice::open(CodecOp::H264Decode) {
        Ok(Some(device)) => device,
        Ok(None) => {
            println!("vkprobe: no vulkan device; skipped");
            return;
        }
        Err(error) => {
            eprintln!("vkprobe: negotiation failed ({error})");
            std::process::exit(1);
        }
    };
    let mut decoder = H264Decoder::open(&device).expect("decoder");
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut count = 0;
    for frame in decoder
        .decode(&Packet::new(Bytes::from(bytes)))
        .expect("decode")
    {
        frame.yuv420p_into(&mut scratch);
        out.extend_from_slice(&scratch);
        count += 1;
    }
    for frame in decoder.flush().expect("flush") {
        frame.yuv420p_into(&mut scratch);
        out.extend_from_slice(&scratch);
        count += 1;
    }
    std::fs::write(&out_path, &out).expect("write");
    eprintln!("decoded={count} bytes={}", out.len());
}

/// First `k` NAL units of Annex-B `bytes`, re-emitted with 4-byte codes.
fn truncate_nals(bytes: &[u8], k: usize) -> Vec<u8> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == 0
            && bytes[i + 1] == 0
            && (bytes[i + 2] == 1 || (bytes[i + 2] == 0 && bytes[i + 3] == 1))
        {
            let code = if bytes[i + 2] == 1 { 3 } else { 4 };
            starts.push(i);
            i += code;
        } else {
            i += 1;
        }
    }
    starts.push(bytes.len());
    let units = starts.len() - 1;
    let k = k.min(units);
    let mut out = Vec::new();
    let mut j = 0;
    let mut taken = 0;
    while j < starts.len() - 1 && taken < k {
        let unit = &bytes[starts[j]..starts[j + 1]];
        let body = if unit[2] == 1 { &unit[3..] } else { &unit[4..] };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(body);
        j += 1;
        taken += 1;
    }
    if std::env::var("VKPROBE_DUP_LAST").is_ok() && k > 0 {
        let unit = &bytes[starts[k - 1]..starts[k]];
        let body = if unit[2] == 1 { &unit[3..] } else { &unit[4..] };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(body);
    }
    out
}
