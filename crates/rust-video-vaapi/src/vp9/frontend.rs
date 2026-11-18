//! VP9 frame assembly, reference slots, and output orchestration.
//!
//! Portable: splits superframes, tracks the eight reference slots, and
//! resolves show-existing frames. A VA-API backend submits finished frames.

use crate::error::VaApiError;
use crate::frontend::SurfaceSlot;
use crate::vp9::parser::{BitDepth, Header, Parser, Profile};

use bytes::Bytes;

/// Number of VP9 reference slots.
pub use crate::vp9::parser::NUM_REF_FRAMES as NUM_SLOTS;

/// Coded frame with its bitstream bytes.
pub struct PendingFrame {
    /// Parsed frame header.
    header: Header,
    /// Frame bytes, uncompressed header included.
    data: Bytes,
}

/// Coded frame with reference slots resolved.
pub struct Picture {
    /// Parsed frame header.
    header: Header,
    /// Frame bytes, uncompressed header included.
    data: Bytes,
    /// Reference slots with coded sizes.
    refs: [Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS],
}

impl Picture {
    /// Parsed frame header.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Frame bytes, uncompressed header included.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Reference slots with coded sizes.
    #[must_use]
    pub fn refs(&self) -> &[Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS] {
        &self.refs
    }
}

/// Begun frame: a picture to submit or a past frame to show.
pub enum Begun {
    /// Regular picture with resolved references.
    Picture(Box<Picture>),
    /// Show-existing frame on a live slot.
    Existing {
        /// Slot holding the frame to show.
        slot: SurfaceSlot,
        /// Coded size of the frame to show.
        size: (u32, u32),
    },
}

/// Frames finished by one ingest call.
pub struct Ingest {
    /// Frames finished by the new bytes, in decode order.
    frames: Vec<PendingFrame>,
    /// True when the coded size changed mid-stream.
    reconfigured: bool,
}

impl Ingest {
    /// Frames finished by the new bytes, in decode order.
    pub fn frames(&mut self) -> Vec<PendingFrame> {
        std::mem::take(&mut self.frames)
    }

    /// True when the coded size changed mid-stream.
    #[must_use]
    pub fn reconfigured(&self) -> bool {
        self.reconfigured
    }
}

/// VP9 frontend: chunk stream in, ordered frames out.
pub struct Frontend {
    /// Bitstream parser with cached color state.
    parser: Parser,
    /// Active coded size.
    active: Option<(u32, u32)>,
    /// Active bitstream profile.
    profile: Option<Profile>,
    /// Reference slots with coded sizes.
    refs: [Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS],
}

impl Frontend {
    /// Empty frontend, no size active.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: Parser::default(),
            active: None,
            profile: None,
            refs: Default::default(),
        }
    }

    /// Coded picture size in luma samples, if a frame arrived.
    #[must_use]
    pub fn coded_size(&self) -> Option<(u32, u32)> {
        self.active
    }

    /// Active bitstream profile, if a frame arrived.
    #[must_use]
    pub fn profile(&self) -> Option<Profile> {
        self.profile
    }

    /// True when `index` backs a live reference slot.
    #[must_use]
    pub fn slot_in_use(&self, index: u32) -> bool {
        self.refs
            .iter()
            .flatten()
            .any(|(slot, _)| slot.index() == index)
    }

    /// Frames finished by the chunk stream in `bytes`.
    ///
    /// Frame payloads sub-slice `bytes` without copying.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a frame fails to parse.
    pub fn ingest(&mut self, bytes: &Bytes) -> Result<Ingest, VaApiError> {
        let mut out = Ingest {
            frames: Vec::new(),
            reconfigured: false,
        };
        let frames = self
            .parser
            .parse_chunk(bytes.as_ref())
            .map_err(VaApiError::Parse)?;
        for frame in frames {
            let payload: &[u8] = frame.as_ref();
            let base = bytes.as_ptr() as usize;
            let offset = payload.as_ptr() as usize - base;
            debug_assert!(offset + payload.len() <= bytes.len());
            let data = bytes.slice(offset..offset + payload.len());
            let header = frame.header.clone();
            if !header.show.show_existing_frame {
                let size = (header.width, header.height);
                if self.active != Some(size) || self.profile != Some(header.profile) {
                    self.active = Some(size);
                    self.profile = Some(header.profile);
                    self.refs = Default::default();
                    out.reconfigured = true;
                }
            }
            out.frames.push(PendingFrame { header, data });
        }
        Ok(out)
    }

    /// Store `picture` on `slot`, refreshing reference slots.
    pub fn commit(&mut self, picture: &Picture, slot: SurfaceSlot) {
        let size = (picture.header.width, picture.header.height);
        self.refresh(slot, size, picture.header.refresh_frame_flags);
    }

    /// Order `pending` against the reference slots.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame leaves Profile 0 8-bit 4:2:0 or
    /// names a reference slot that holds no frame.
    pub fn begin(&mut self, pending: PendingFrame) -> Result<Begun, VaApiError> {
        Self::check_supported(&pending.header)?;
        if pending.header.show.show_existing_frame {
            let index = usize::from(pending.header.frame_to_show_map_idx);
            let (slot, size) = self.refs.get(index).copied().flatten().ok_or_else(|| {
                VaApiError::BrokenReference("no reference picture to display".to_owned())
            })?;
            return Ok(Begun::Existing { slot, size });
        }
        Ok(Begun::Picture(Box::new(Picture {
            header: pending.header,
            data: pending.data,
            refs: self.refs,
        })))
    }

    /// Point refreshed slots at `slot` with coded `size`.
    fn refresh(&mut self, slot: SurfaceSlot, size: (u32, u32), flags: u8) {
        let mut flags = flags;
        for entry in &mut self.refs {
            if flags & 1 == 1 {
                *entry = Some((slot, size));
            }
            flags >>= 1;
        }
    }

    /// Profile 0 8-bit 4:2:0 gate.
    fn check_supported(header: &Header) -> Result<(), VaApiError> {
        if header.show.show_existing_frame {
            return Ok(());
        }
        if !matches!(header.profile, Profile::Profile0) {
            return Err(VaApiError::Unsupported(format!(
                "vp9 profile {}",
                header.profile as u8
            )));
        }
        if !matches!(header.color.bit_depth, BitDepth::Depth8) {
            return Err(VaApiError::Unsupported(format!(
                "vp9 bit depth {}",
                header.color.bit_depth as u8
            )));
        }
        if !header.color.subsampling_x || !header.color.subsampling_y {
            return Err(VaApiError::Unsupported(
                "vp9 chroma outside 4:2:0".to_owned(),
            ));
        }
        Ok(())
    }
}
