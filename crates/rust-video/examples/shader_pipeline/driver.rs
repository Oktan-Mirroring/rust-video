//! Shared combo driver: preflight, timed passes, median.

#[cfg(any(
    all(target_os = "windows", feature = "d3d11"),
    all(target_os = "windows", feature = "d3d12")
))]
pub(crate) mod surface {
    //! Surface-combo run over per-backend primitives.

    use super::Pipeline;
    use crate::content::{Clip, Codec, Op};
    use crate::measure::{median_of_5, Sample};
    use crate::Outcome;

    use rust_video::Packet;

    /// Decode source behind one combo.
    pub(crate) trait DecodeSurface {
        /// Decoded surface type.
        type Surface;

        /// Surfaces decoded from `packet`, possibly none.
        fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Surface>, String>;

        /// Surfaces held back for reordering or buffering.
        fn flush(&mut self) -> Result<Vec<Self::Surface>, String>;
    }

    /// Encoder drain behind one combo.
    pub(crate) trait DrainEncoder {
        /// Packets held back for reordering or buffering.
        fn drain(&mut self) -> Result<Vec<Packet>, String>;
    }

    /// Surface pipeline: shared combo run over per-backend primitives.
    pub(crate) trait SurfacePipeline {
        /// Decoded surface type.
        type Surface;

        /// Decode source behind one combo.
        type Source<'s>: DecodeSurface<Surface = Self::Surface>
        where
            Self: 's;

        /// GPU filter behind one combo.
        type Filter<'s>
        where
            Self: 's;

        /// GPU-bound encoder behind one combo.
        type Encoder<'s>: DrainEncoder
        where
            Self: 's;

        /// Backend name for the preflight dump.
        fn name(&self) -> &'static str;

        /// Decode source for `codec`.
        fn source(&self, codec: Codec) -> Result<Self::Source<'_>, String>;

        /// Filter for `op`.
        fn filter(&self, op: Op) -> Result<Self::Filter<'_>, String>;

        /// GPU-bound encoder for `codec`.
        fn encoder(&self, codec: Codec) -> Result<Self::Encoder<'_>, String>;

        /// Combo note: encode path.
        fn note(codec: Codec, encoder: &Self::Encoder<'_>) -> String;

        /// Dump extension for `encoder`.
        fn ext(encoder: &Self::Encoder<'_>) -> &'static str;

        /// Coded bytes for one filtered `surface` into `out`.
        fn encode_surface(
            &self,
            filter: &mut Self::Filter<'_>,
            encoder: &mut Self::Encoder<'_>,
            surface: &Self::Surface,
            out: &mut Vec<u8>,
        ) -> Result<(), String>;

        /// One validated pass plus the combo note, or the decline reason.
        fn preflight(&self, codec: Codec, op: Op, packets: &[Packet]) -> Result<String, String> {
            let mut source = self.source(codec)?;
            let mut filter = self.filter(op)?;
            let mut encoder = self.encoder(codec)?;
            let note = Self::note(codec, &encoder);
            let mut surfaces = 0;
            let mut out = Vec::new();
            for packet in packets {
                let decoded = source.decode(packet)?;
                surfaces += decoded.len();
                for surface in &decoded {
                    self.encode_surface(&mut filter, &mut encoder, surface, &mut out)?;
                }
            }
            for surface in &source.flush()? {
                surfaces += 1;
                self.encode_surface(&mut filter, &mut encoder, surface, &mut out)?;
            }
            for packet in encoder.drain()? {
                out.extend_from_slice(packet.data());
            }
            if surfaces == 0 {
                return Err("decoder emitted no surfaces".to_owned());
            }
            if out.is_empty() {
                return Err("encoder emitted no bytes".to_owned());
            }
            crate::dump(self.name(), codec, op, Self::ext(&encoder), &out);
            Ok(format!("{note}, {surfaces} surfaces, {} bytes", out.len()))
        }

        /// One timed pass; failures score zero frames.
        fn pass(&self, codec: Codec, op: Op, packets: &[Packet]) -> Sample {
            let zero = Sample {
                frames: 0,
                fps: 0.0,
            };
            let Ok(mut source) = self.source(codec) else {
                return zero;
            };
            let Ok(mut filter) = self.filter(op) else {
                return zero;
            };
            let Ok(mut encoder) = self.encoder(codec) else {
                return zero;
            };
            let start = std::time::Instant::now();
            let mut frames = 0;
            let mut out = Vec::new();
            for packet in packets {
                let Ok(decoded) = source.decode(packet) else {
                    return zero;
                };
                for surface in &decoded {
                    if self
                        .encode_surface(&mut filter, &mut encoder, surface, &mut out)
                        .is_err()
                    {
                        return zero;
                    }
                }
                out.clear();
                frames += decoded.len();
            }
            let elapsed = start.elapsed().as_secs_f64();
            let fps = f64::from(u32::try_from(frames).unwrap_or(u32::MAX)) / elapsed.max(1e-9);
            Sample { frames, fps }
        }

        /// Decode, filter, re-encode: median of 5.
        fn run(&self, codec: Codec, op: Op, clip: &Clip) -> Outcome {
            let packets = clip.packets();
            let first = match self.preflight(codec, op, &packets) {
                Ok(first) => first,
                Err(error) => return Outcome::Declined(error),
            };
            let (fps, spread, frames, reran) = median_of_5(|| self.pass(codec, op, &packets));
            Outcome::Ran {
                fps,
                spread,
                frames,
                reran,
                note: first,
            }
        }
    }

    impl<T: SurfacePipeline> Pipeline for T {
        fn run(&self, codec: Codec, op: Op, clip: &Clip) -> Outcome {
            SurfacePipeline::run(self, codec, op, clip)
        }
    }
}

use crate::content::{Clip, Codec, Op};
use crate::Outcome;

/// Decode, filter, re-encode over one backend.
pub(crate) trait Pipeline {
    /// Decode, filter, re-encode: median of 5.
    fn run(&self, codec: Codec, op: Op, clip: &Clip) -> Outcome;
}
