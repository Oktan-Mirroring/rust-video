//! Minimal MP4 demux for H.264 test vectors: video samples as Annex B.

use std::path::Path;

/// H.264 stream: samples in decode order with sets injected at IDRs.
pub struct Stream {
    /// Samples in decode order, Annex B with start codes.
    pub samples: Vec<Vec<u8>>,
}

/// Demux the first video track of the MP4 at `path`.
///
/// # Errors
///
/// Returns `Err` describing the first box that fails to parse.
pub fn demux(path: &Path) -> Result<Stream, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    Demux::new(&bytes).stream()
}

/// MP4 reader over borrowed bytes.
struct Demux<'a> {
    /// File bytes.
    bytes: &'a [u8],
}

impl<'a> Demux<'a> {
    /// Reader over `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// Video stream of the first video track.
    fn stream(&self) -> Result<Stream, String> {
        let mut reader = Reader::new(self.bytes);
        let mut video: Option<Track> = None;
        while let Some(play) = reader.next_box()? {
            if play.fourcc == *b"moov" {
                video = Self::video_track(play.payload)?;
                break;
            }
        }
        let track = video.ok_or_else(|| "no video track".to_owned())?;
        let mut samples = Vec::with_capacity(track.sizes.len());
        let mut first = true;
        let mut injected = track.extradata.is_empty();
        for (offset, size) in track.offsets() {
            let sample = self
                .bytes
                .get(offset..offset + size)
                .ok_or_else(|| "sample past eof".to_owned())?;
            samples.push(track.sample_annex_b(sample, first, &mut injected)?);
            first = false;
        }
        Ok(Stream { samples })
    }

    /// First video track under `moov`.
    fn video_track(moov: &[u8]) -> Result<Option<Track>, String> {
        let mut reader = Reader::new(moov);
        while let Some(play) = reader.next_box()? {
            if play.fourcc != *b"trak" {
                continue;
            }
            if let Some(track) = Self::track(play.payload)? {
                return Ok(Some(track));
            }
        }
        Ok(None)
    }

    /// Track when `trak` carries video, else `None`.
    fn track(trak: &[u8]) -> Result<Option<Track>, String> {
        let mut reader = Reader::new(trak);
        let mut media: Option<&[u8]> = None;
        while let Some(play) = reader.next_box()? {
            if play.fourcc == *b"mdia" {
                media = Some(play.payload);
            }
        }
        let Some(media) = media else {
            return Ok(None);
        };
        let mut reader = Reader::new(media);
        let mut handler: Option<[u8; 4]> = None;
        let mut info: Option<&[u8]> = None;
        while let Some(play) = reader.next_box()? {
            if play.fourcc == *b"hdlr" {
                let tag = play.payload.get(8..12).ok_or("short hdlr")?;
                let mut fourcc = [0u8; 4];
                fourcc.copy_from_slice(tag);
                handler = Some(fourcc);
            } else if play.fourcc == *b"minf" {
                info = Some(play.payload);
            }
        }
        if handler != Some(*b"vide") {
            return Ok(None);
        }
        let Some(info) = info else {
            return Err("video track without minf".to_owned());
        };
        let mut reader = Reader::new(info);
        while let Some(play) = reader.next_box()? {
            if play.fourcc == *b"stbl" {
                return Self::sample_table(play.payload).map(Some);
            }
        }
        Err("video track without stbl".to_owned())
    }

    /// Sample table of a video track.
    fn sample_table(stbl: &[u8]) -> Result<Track, String> {
        let mut reader = Reader::new(stbl);
        let mut track = Track::default();
        let mut found = false;
        while let Some(play) = reader.next_box()? {
            match &play.fourcc {
                b"stsd" => {
                    (track.length_size, track.extradata) = Self::avcc(play.payload)?;
                    found = true;
                }
                b"stsc" => track.chunks = Self::chunk_runs(play.payload)?,
                b"stsz" => track.sizes = Self::sample_sizes(play.payload)?,
                b"stz2" => return Err("compact sample sizes unsupported".to_owned()),
                b"stco" => track.offsets = Self::chunk_offsets(play.payload, false)?,
                b"co64" => track.offsets = Self::chunk_offsets(play.payload, true)?,
                _ => {}
            }
        }
        if !found {
            return Err("video track without avc1".to_owned());
        }
        if track.offsets.len() != track.chunk_count() {
            return Err("chunk table mismatch".to_owned());
        }
        Ok(track)
    }

    /// NAL length size and extradata sets from `stsd`.
    fn avcc(stsd: &[u8]) -> Result<(usize, Vec<u8>), String> {
        let mut reader = Reader::new(stsd.get(8..).ok_or("short stsd")?);
        let entry = reader.next_box()?.ok_or("stsd without entry")?;
        if entry.fourcc != *b"avc1" {
            return Err("video entry not avc1".to_owned());
        }
        let mut reader = Reader::new(entry.payload.get(78..).ok_or("short avc1")?);
        while let Some(play) = reader.next_box()? {
            if play.fourcc == *b"avcC" {
                return Self::avcc_config(play.payload);
            }
        }
        Err("avc1 without avcC".to_owned())
    }

    /// NAL length size and extradata sets from `avcC`.
    fn avcc_config(avcc: &[u8]) -> Result<(usize, Vec<u8>), String> {
        let mut reader = Reader::new(avcc);
        let _version = reader.u8()?;
        let _profile = reader.u8()?;
        let _compat = reader.u8()?;
        let _level = reader.u8()?;
        let length_size = usize::from(reader.u8()? & 0x03) + 1;
        let sps_count = reader.u8()? & 0x1f;
        let mut extradata = Vec::new();
        for _ in 0..sps_count {
            let len = reader.u16()? as usize;
            extradata.extend_from_slice(&[0, 0, 0, 1]);
            extradata.extend_from_slice(reader.bytes(len)?);
        }
        let pps_count = reader.u8()?;
        for _ in 0..pps_count {
            let len = reader.u16()? as usize;
            extradata.extend_from_slice(&[0, 0, 0, 1]);
            extradata.extend_from_slice(reader.bytes(len)?);
        }
        Ok((length_size, extradata))
    }

    /// `(first_chunk, samples_per_chunk)` runs from `stsc`.
    fn chunk_runs(stsc: &[u8]) -> Result<Vec<(usize, usize)>, String> {
        let mut reader = Reader::new(stsc.get(4..).ok_or("short stsc")?);
        let count = reader.u32()? as usize;
        let mut runs = Vec::with_capacity(count);
        for _ in 0..count {
            let first = reader.u32()? as usize;
            let per_chunk = reader.u32()? as usize;
            let _description = reader.u32()?;
            runs.push((first, per_chunk));
        }
        Ok(runs)
    }

    /// Sample sizes from `stsz`.
    fn sample_sizes(stsz: &[u8]) -> Result<Vec<usize>, String> {
        let mut reader = Reader::new(stsz.get(4..).ok_or("short stsz")?);
        let uniform = reader.u32()? as usize;
        let count = reader.u32()? as usize;
        if uniform != 0 {
            return Ok(vec![uniform; count]);
        }
        let mut sizes = Vec::with_capacity(count);
        for _ in 0..count {
            sizes.push(reader.u32()? as usize);
        }
        Ok(sizes)
    }

    /// Chunk file offsets from `stco`/`co64`.
    fn chunk_offsets(offsets: &[u8], wide: bool) -> Result<Vec<usize>, String> {
        let mut reader = Reader::new(offsets.get(4..).ok_or("short chunk offsets")?);
        let count = reader.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(if wide {
                usize::try_from(reader.u64()?).map_err(|_| "chunk past memory".to_owned())?
            } else {
                reader.u32()? as usize
            });
        }
        Ok(out)
    }
}

/// Video sample table.
#[derive(Default)]
struct Track {
    /// AVCC NAL length in bytes.
    length_size: usize,
    /// Extradata sets with start codes.
    extradata: Vec<u8>,
    /// `(first_chunk_1based, samples_per_chunk)` runs.
    chunks: Vec<(usize, usize)>,
    /// Sample sizes in decode order.
    sizes: Vec<usize>,
    /// Chunk file offsets.
    offsets: Vec<usize>,
}

impl Track {
    /// Chunk count from the offset table.
    fn chunk_count(&self) -> usize {
        self.offsets.len()
    }

    /// `(offset, size)` per sample in decode order.
    fn offsets(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::with_capacity(self.sizes.len());
        let mut sample = 0;
        for (chunk, offset) in self.offsets.iter().enumerate() {
            let per_chunk = self.samples_per_chunk(chunk + 1);
            let mut cursor = *offset;
            for _ in 0..per_chunk {
                if sample >= self.sizes.len() {
                    break;
                }
                out.push((cursor, self.sizes[sample]));
                cursor += self.sizes[sample];
                sample += 1;
            }
        }
        out
    }

    /// Samples in 1-based `chunk` from the run table.
    fn samples_per_chunk(&self, chunk: usize) -> usize {
        let mut per_chunk = 0;
        for (first, count) in &self.chunks {
            if *first <= chunk {
                per_chunk = *count;
            }
        }
        per_chunk
    }

    /// Length-prefixed sample as Annex B, sets before the stream's first IDR NAL.
    ///
    /// The stream's first sample carries the sets at its head when it
    /// holds no IDR NAL at all. `injected` spans samples: the sets go
    /// out once.
    fn sample_annex_b(
        &self,
        sample: &[u8],
        first: bool,
        injected: &mut bool,
    ) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(sample.len() + self.extradata.len() + 16);
        let mut cursor = 0;
        while cursor < sample.len() {
            let end = cursor + self.length_size;
            let prefix = sample.get(cursor..end).ok_or("short nal length")?;
            let mut len = 0usize;
            for byte in prefix {
                len = len * 256 + usize::from(*byte);
            }
            let body = sample.get(end..end + len).ok_or("short nal body")?;
            if !*injected && body.first().is_some_and(|header| header & 0x1f == 5) {
                out.extend_from_slice(&self.extradata);
                *injected = true;
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(body);
            cursor = end + len;
        }
        if !*injected && first {
            let mut headed = self.extradata.clone();
            headed.extend_from_slice(&out);
            *injected = true;
            return Ok(headed);
        }
        Ok(out)
    }
}

/// One box: type plus payload.
struct Play<'a> {
    /// Box type.
    fourcc: [u8; 4],
    /// Box payload.
    payload: &'a [u8],
}

/// Bounds-checked big-endian reader.
struct Reader<'a> {
    /// Remaining bytes.
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Reader over `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// Next box, or `None` at end of bytes.
    fn next_box(&mut self) -> Result<Option<Play<'a>>, String> {
        if self.bytes.is_empty() {
            return Ok(None);
        }
        let size = self.u32()? as usize;
        let tag = self.bytes(4)?;
        let fourcc = [tag[0], tag[1], tag[2], tag[3]];
        let payload_len = match size {
            0 => self.bytes.len(),
            1 => {
                let wide = usize::try_from(self.u64()?).map_err(|_| "box past memory")?;
                wide.saturating_sub(16)
            }
            _ => size.saturating_sub(8),
        };
        let payload = self.bytes(payload_len)?;
        Ok(Some(Play { fourcc, payload }))
    }

    /// One byte.
    fn u8(&mut self) -> Result<u8, String> {
        self.bytes(1).map(|slice| slice[0])
    }

    /// Big-endian 16-bit value.
    fn u16(&mut self) -> Result<u16, String> {
        self.bytes(2)
            .map(|slice| u16::from_be_bytes([slice[0], slice[1]]))
    }

    /// Big-endian 32-bit value.
    fn u32(&mut self) -> Result<u32, String> {
        self.bytes(4)
            .map(|slice| u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    /// Big-endian 64-bit value.
    fn u64(&mut self) -> Result<u64, String> {
        self.bytes(8).map(|slice| {
            u64::from_be_bytes([
                slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
            ])
        })
    }

    /// `len` bytes.
    fn bytes(&mut self, len: usize) -> Result<&'a [u8], String> {
        if self.bytes.len() < len {
            return Err("box past end".to_owned());
        }
        let (head, tail) = self.bytes.split_at(len);
        self.bytes = tail;
        Ok(head)
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, Track};

    #[test]
    fn reader_counts_bytes() {
        let mut reader = Reader::new(&[0, 0, 0, 12, b'a', b'b', b'c', b'd', 1, 2, 3, 4]);
        let play = reader.next_box().expect("box").expect("some");
        assert_eq!(play.fourcc, *b"abcd");
        assert_eq!(play.payload, &[1, 2, 3, 4]);
        assert!(reader.next_box().expect("eof").is_none());
    }

    #[test]
    fn reader_rejects_truncation() {
        let mut reader = Reader::new(&[0, 0, 0, 20, b'a', b'b', b'c', b'd']);
        assert!(reader.next_box().is_err());
    }

    #[test]
    fn annex_b_prefixes_each_nal() {
        let track = Track {
            length_size: 4,
            ..Track::default()
        };
        let sample = [0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 1, 0x68];
        let annex_b = track
            .sample_annex_b(&sample, false, &mut true)
            .expect("convert");
        assert_eq!(annex_b, vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68]);
    }

    #[test]
    fn annex_b_injects_sets_before_idr() {
        let track = Track {
            length_size: 4,
            extradata: vec![0, 0, 0, 1, 0x67],
            ..Track::default()
        };
        let sample = [0, 0, 0, 1, 0x06, 0, 0, 0, 1, 0x65];
        let annex_b = track
            .sample_annex_b(&sample, false, &mut false)
            .expect("convert");
        assert_eq!(
            annex_b,
            vec![0, 0, 0, 1, 0x06, 0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65]
        );
    }

    #[test]
    fn annex_b_rejects_short_body() {
        let track = Track {
            length_size: 4,
            ..Track::default()
        };
        assert!(track
            .sample_annex_b(&[0, 0, 0, 9, 0x67], false, &mut true)
            .is_err());
    }
}
