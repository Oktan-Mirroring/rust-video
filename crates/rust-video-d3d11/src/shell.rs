//! Probed decoder shells; the picture builders are pending.

use crate::error::{D3d11Error, SkipReason, UnsupportedReason};
use crate::frame::D3d11Frame;
use crate::guid::Codec;
// Crate root resolves the device per platform; the module paths differ.
use crate::D3d11Device;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

/// Probed decoder shell; the picture builder is pending.
macro_rules! shell {
    ($decoder:ident, $codec:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device behind the profile probe.
            device: &'a D3d11Device,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; decode reports the pending builder.
            #[must_use]
            pub fn open(device: &'a D3d11Device) -> Self {
                Self { device }
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d11Frame;
            type Error = D3d11Error;

            fn caps(&self) -> GpuCaps {
                GpuCaps::new(None, false, false)
            }

            fn decode(&mut self, _packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                Err(D3d11Error::Unsupported(
                    UnsupportedReason::PictureBuilderPending {
                        codec: Codec::$codec,
                    },
                ))
            }

            fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
                Ok(Vec::new())
            }
        }
    };
}

#[cfg(not(target_os = "windows"))]
shell!(
    Vp9Decoder,
    Vp9,
    "VP9 decoder; the picture builder is pending."
);
shell!(
    Mpeg2Decoder,
    Mpeg2,
    "MPEG-2 decoder; the picture builder is pending."
);
shell!(
    Vc1Decoder,
    Vc1,
    "VC-1 decoder; the picture builder is pending."
);

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    #[test]
    fn shell_caps_decline_typed() {
        use bytes::Bytes;
        use rust_video_caps::packet::Packet;
        use rust_video_caps::traits::Decoder;
        use rust_video_caps::transcode::GpuCaps;

        let Ok(device) = crate::D3d11Device::open() else {
            return;
        };
        let packet = Packet::new(Bytes::new());
        let mut mpeg2 = super::Mpeg2Decoder::open(&device);
        assert_eq!(mpeg2.caps(), GpuCaps::new(None, false, false));
        assert!(mpeg2.decode(&packet).is_err());
        let mut vc1 = super::Vc1Decoder::open(&device);
        assert_eq!(vc1.caps(), GpuCaps::new(None, false, false));
        assert!(vc1.decode(&packet).is_err());
    }
}
