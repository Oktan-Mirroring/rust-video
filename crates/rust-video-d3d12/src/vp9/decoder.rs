//! VP9 probe shell; the picture builder is pending.

use crate::device::D3d12Device;
use crate::error::{D3d12Error, SkipReason, UnsupportedReason};
use crate::frame::D3d12Frame;
use crate::guid::Codec;

use rust_video_caps::delivery::SubmitStats;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

/// Probed decoder shell; the picture builder is pending.
macro_rules! shell {
    ($decoder:ident, $codec:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device behind the profile probe.
            device: &'a D3d12Device,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; decode reports the pending builder.
            #[must_use]
            pub fn open(device: &'a D3d12Device) -> Self {
                Self { device }
            }

            /// Submit gauges since decoder open.
            #[must_use]
            pub fn submit_stats(&self) -> SubmitStats {
                let _ = self;
                SubmitStats::new()
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d12Frame;
            type Error = D3d12Error;

            fn caps(&self) -> GpuCaps {
                GpuCaps::new(None, false, false)
            }

            fn decode(&mut self, _packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                Err(D3d12Error::Unsupported(
                    UnsupportedReason::PictureBuilderPending {
                        codec: Codec::$codec,
                    },
                ))
            }

            fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
                Ok(Vec::new())
            }
        }
    };
}

shell!(
    Vp9Decoder,
    Vp9,
    "VP9 decoder; the picture builder is pending."
);
