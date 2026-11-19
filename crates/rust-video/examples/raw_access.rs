//! Raw image access: decode one clip, touch every frame.
//!
//! Each frame reports metadata (extent, format), its NV12 bytes through a
//! checksum, and its export handle. `d3d12` surfaces stay GPU-resident, so
//! they report metadata plus the handle without bytes.
//!
//! Usage: `raw_access [tier] [codec] [file]`. The tier defaults to the
//! first that opens, the codec to `h264`, the file to the codec's bench
//! vector. Anything missing declines by name.

use bytes::Bytes;
use rust_video::Packet;
use rust_video_harness::{bench_dir, AnnexB, Ivf, Obu};

/// Codec under test.
#[derive(Clone, Copy)]
enum Codec {
    H264,
    H265,
    Av1,
    Vp9,
}

impl Codec {
    /// Lowercase CLI name.
    const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Av1 => "av1",
            Self::Vp9 => "vp9",
        }
    }

    /// Parse a CLI codec filter.
    fn parse(text: &str) -> Option<Self> {
        [Self::H264, Self::H265, Self::Av1, Self::Vp9]
            .into_iter()
            .find(|codec| codec.name() == text.to_lowercase())
    }

    /// Default bench vector for the codec.
    fn vector(self) -> std::path::PathBuf {
        bench_dir().join(match self {
            Self::H264 => "h264-1080p_ip.h264",
            Self::H265 => "h265-1080p_ip.h265",
            Self::Av1 => "av1-1080p_ip.ivf",
            Self::Vp9 => "vp9-1080p_ip.ivf",
        })
    }

    /// Coded packets from `path`: IVF and OBU by extension, else pictures by codec.
    fn packets(self, path: &std::path::Path) -> Vec<Packet> {
        let raws = match path.extension().and_then(|ext| ext.to_str()) {
            Some("ivf") => Ivf::read(path).frames,
            Some("obu") => Obu::tu_packets(&std::fs::read(path).unwrap_or_default()),
            _ => match self {
                Self::H264 => AnnexB::picture_packets(path, AnnexB::is_h264_vcl),
                Self::H265 => AnnexB::picture_packets(path, AnnexB::is_hevc_vcl),
                Self::Av1 | Self::Vp9 => Ivf::read(path).frames,
            },
        };
        raws.into_iter()
            .map(|raw| Packet::new(Bytes::from(raw)))
            .collect()
    }
}

/// Decode loop over real tiers; absent without one.
#[cfg(any(
    all(target_os = "windows", feature = "d3d11"),
    all(target_os = "windows", feature = "d3d12"),
    feature = "vulkan"
))]
mod pipe {
    use rust_video::{Decoder, GpuExport, Packet, PictureSize, PixelFormat};

    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    use rust_video_d3d12::{Av1Decoder, GpuSurface, H264Decoder, HevcDecoder};

    /// One decoded frame, shown.
    pub trait Show {
        /// Metadata, bytes, and export handle for frame `index`.
        fn show(&self, index: usize);
    }

    /// One frame line: metadata, byte checksum, export handle.
    pub fn show_frame(
        index: usize,
        size: PictureSize,
        format: PixelFormat,
        bytes: Option<&[u8]>,
        export: &GpuExport,
    ) {
        match bytes {
            Some(bytes) => println!(
                "frame {index}: {}x{} {format:?} bytes={} fnv={:016x} export={export:?}",
                size.width(),
                size.height(),
                bytes.len(),
                fnv(bytes),
            ),
            None => println!(
                "frame {index}: {}x{} {format:?} bytes=gpu-resident export={export:?}",
                size.width(),
                size.height(),
            ),
        }
    }

    /// FNV-1a over `bytes`: proves the read without printing megabytes.
    fn fnv(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    }

    /// Frames shown, counted into `shown`.
    pub fn emit(frames: &[impl Show], shown: &mut usize) {
        for frame in frames {
            frame.show(*shown);
            *shown += 1;
        }
    }

    /// Frames decoded from `packets`, shown, counted.
    #[cfg(any(all(target_os = "windows", feature = "d3d11"), feature = "vulkan"))]
    pub fn drain<D: Decoder>(decoder: &mut D, packets: &[Packet]) -> Result<usize, String>
    where
        D::Frame: Show,
    {
        let mut shown = 0;
        for packet in packets {
            match decoder.decode(packet) {
                Ok(frames) => emit(&frames, &mut shown),
                Err(error) => return Err(error.to_string()),
            }
        }
        match decoder.flush() {
            Ok(frames) => emit(&frames, &mut shown),
            Err(error) => return Err(error.to_string()),
        }
        Ok(shown)
    }

    #[cfg(all(target_os = "windows", feature = "d3d11"))]
    impl Show for rust_video_d3d11::D3d11Frame {
        fn show(&self, index: usize) {
            let identity = self.identity();
            show_frame(
                index,
                identity.size(),
                identity.format(),
                Some(self.nv12()),
                &self.export(),
            );
        }
    }

    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    impl Show for GpuSurface {
        fn show(&self, index: usize) {
            show_frame(index, self.size(), self.format(), None, &self.export());
        }
    }

    /// One d3d12 surface decoder.
    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    pub trait Pump {
        /// Surfaces decoded from `packet`, possibly none.
        fn pump(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, String>;
        /// Surfaces held back for reordering or buffering.
        fn finish(&mut self) -> Result<Vec<GpuSurface>, String>;
    }

    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    impl Pump for H264Decoder<'_> {
        fn pump(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, String> {
            self.decode_surface(packet)
                .map_err(|error| error.to_string())
        }
        fn finish(&mut self) -> Result<Vec<GpuSurface>, String> {
            self.flush_surface().map_err(|error| error.to_string())
        }
    }

    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    impl Pump for HevcDecoder<'_> {
        fn pump(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, String> {
            self.decode_surface(packet)
                .map_err(|error| error.to_string())
        }
        fn finish(&mut self) -> Result<Vec<GpuSurface>, String> {
            self.flush_surface().map_err(|error| error.to_string())
        }
    }

    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    impl Pump for Av1Decoder<'_> {
        fn pump(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, String> {
            self.decode_surface(packet)
                .map_err(|error| error.to_string())
        }
        fn finish(&mut self) -> Result<Vec<GpuSurface>, String> {
            self.flush_surface().map_err(|error| error.to_string())
        }
    }

    /// Surfaces decoded from `packets`, shown, counted.
    #[cfg(all(target_os = "windows", feature = "d3d12"))]
    pub fn drain12(decoder: &mut impl Pump, packets: &[Packet]) -> Result<usize, String> {
        let mut shown = 0;
        for packet in packets {
            emit(&decoder.pump(packet)?, &mut shown);
        }
        emit(&decoder.finish()?, &mut shown);
        Ok(shown)
    }

    #[cfg(feature = "vulkan")]
    impl Show for rust_video_vulkan::VulkanFrame {
        fn show(&self, index: usize) {
            show_frame(
                index,
                self.size(),
                self.format(),
                Some(self.nv12()),
                &self.export(),
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let codec = args
        .get(1)
        .and_then(|text| Codec::parse(text))
        .unwrap_or(Codec::H264);
    let path = args
        .get(2)
        .map_or_else(|| codec.vector(), std::path::PathBuf::from);
    if !path.is_file() {
        println!("declined: no input at {}", path.display());
        return;
    }
    let packets = codec.packets(&path);
    if packets.is_empty() {
        println!("declined: {} holds no pictures", path.display());
        return;
    }
    match args.first().map(String::as_str) {
        Some("d3d11") => report("d3d11", run_d3d11(codec, &packets)),
        Some("d3d12") => report("d3d12", run_d3d12(codec, &packets)),
        Some("vulkan") => report("vulkan", run_vulkan(codec, &packets)),
        Some(other) => println!("declined: unknown tier {other}"),
        None => {
            for tier in ["d3d11", "d3d12", "vulkan"] {
                let outcome = match tier {
                    "d3d11" => run_d3d11(codec, &packets),
                    "d3d12" => run_d3d12(codec, &packets),
                    _ => run_vulkan(codec, &packets),
                };
                let ran = outcome.is_ok();
                report(tier, outcome);
                if ran {
                    return;
                }
            }
        }
    }
}

/// One tier outcome line.
fn report(tier: &str, outcome: Result<usize, String>) {
    match outcome {
        Ok(frames) => println!("{tier}: {frames} frames"),
        Err(error) => println!("{tier}: declined: {error}"),
    }
}

#[cfg(all(target_os = "windows", feature = "d3d11"))]
fn run_d3d11(codec: Codec, packets: &[Packet]) -> Result<usize, String> {
    use rust_video_d3d11::{Av1Decoder, D3d11Device, H264Decoder, HevcDecoder, Vp9Decoder};

    use crate::pipe::drain;

    let device = D3d11Device::open().map_err(|error| error.to_string())?;
    match codec {
        Codec::H264 => drain(&mut H264Decoder::open(&device), packets),
        Codec::H265 => drain(&mut HevcDecoder::open(&device), packets),
        Codec::Av1 => drain(&mut Av1Decoder::open(&device), packets),
        Codec::Vp9 => drain(&mut Vp9Decoder::open(&device), packets),
    }
}

#[cfg(not(all(target_os = "windows", feature = "d3d11")))]
fn run_d3d11(_codec: Codec, _packets: &[Packet]) -> Result<usize, String> {
    Err("needs windows plus the d3d11 feature".to_owned())
}

#[cfg(all(target_os = "windows", feature = "d3d12"))]
fn run_d3d12(codec: Codec, packets: &[Packet]) -> Result<usize, String> {
    use rust_video_d3d12::{Av1Decoder, D3d12Device, H264Decoder, HevcDecoder};

    use crate::pipe::drain12;

    let device = D3d12Device::open().map_err(|error| error.to_string())?;
    match codec {
        Codec::H264 => drain12(&mut H264Decoder::open(&device), packets),
        Codec::H265 => drain12(&mut HevcDecoder::open(&device), packets),
        Codec::Av1 => drain12(&mut Av1Decoder::open(&device), packets),
        Codec::Vp9 => Err("no d3d12 vp9 picture builder".to_owned()),
    }
}

#[cfg(not(all(target_os = "windows", feature = "d3d12")))]
fn run_d3d12(_codec: Codec, _packets: &[Packet]) -> Result<usize, String> {
    Err("needs windows plus the d3d12 feature".to_owned())
}

#[cfg(feature = "vulkan")]
fn run_vulkan(codec: Codec, packets: &[Packet]) -> Result<usize, String> {
    use rust_video_vulkan::{Av1Decoder, CodecOp, H264Decoder, H265Decoder, VulkanDevice};

    use crate::pipe::drain;

    let op = match codec {
        Codec::H264 => CodecOp::H264Decode,
        Codec::H265 => CodecOp::H265Decode,
        Codec::Av1 => CodecOp::Av1Decode,
        Codec::Vp9 => return Err("no vulkan vp9 decoder".to_owned()),
    };
    let device = VulkanDevice::open_or_skip(op).map_err(|error| error.to_string())?;
    match codec {
        Codec::H264 => drain(
            &mut H264Decoder::open(&device).map_err(|error| error.to_string())?,
            packets,
        ),
        Codec::H265 => drain(
            &mut H265Decoder::open(&device).map_err(|error| error.to_string())?,
            packets,
        ),
        Codec::Av1 => drain(
            &mut Av1Decoder::open(&device).map_err(|error| error.to_string())?,
            packets,
        ),
        Codec::Vp9 => Err("no vulkan vp9 decoder".to_owned()),
    }
}

#[cfg(not(feature = "vulkan"))]
fn run_vulkan(_codec: Codec, _packets: &[Packet]) -> Result<usize, String> {
    Err("needs the vulkan feature".to_owned())
}
