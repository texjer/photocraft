//! The ISOBMFF boxes AVIF decoding needs that avif-parse doesn't hand over: the primary item's
//! colour (`colr`) and orientation (`irot`, `imir`) properties, and the image sequence of an
//! animated AVIF (`avis`): where each frame's AV1 data sits in the file and how long it shows, for
//! the colour track and the alpha track that references it (AV1-ISOBMFF §2, AVIF §7).
//!
//! Every read is bounds-checked; a box that runs past its parent is a malformed-file error.

use crate::Format;
use crate::cicp::Cicp;
use crate::error::CodecError;

const F: Format = Format::Avif;

fn bad(what: &str) -> CodecError {
    CodecError::malformed(F, format!("AVIF {what}"))
}

/// The boxes laid end to end in `b`, as (type, payload).
fn boxes(mut b: &[u8]) -> impl Iterator<Item = Result<([u8; 4], &[u8]), CodecError>> {
    std::iter::from_fn(move || {
        if b.is_empty() {
            return None;
        }
        let r = (|| {
            let size = u32::from_be_bytes(*b.first_chunk::<4>().ok_or_else(|| bad("box header is cut short"))?);
            let kind = *b.get(4..8).and_then(|s| s.first_chunk::<4>()).ok_or_else(|| bad("box header is cut short"))?;
            let (header, size) = match size {
                0 => (8, b.len()),
                1 => {
                    let large = u64::from_be_bytes(*b.get(8..16).and_then(|s| s.first_chunk::<8>()).ok_or_else(|| bad("box header is cut short"))?);
                    (16, usize::try_from(large).map_err(|_| bad("box is too large"))?)
                }
                n => (8, n as usize),
            };
            if size < header || size > b.len() {
                return Err(bad(&format!("`{}` box runs past its parent", String::from_utf8_lossy(&kind))));
            }
            let payload = &b[header..size];
            b = &b[size..];
            Ok((kind, payload))
        })();
        if r.is_err() {
            b = &[];
        }
        Some(r)
    })
}

/// The payload of the first `kind` box in `b`.
fn find<'a>(b: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, CodecError> {
    for r in boxes(b) {
        let (k, p) = r?;
        if &k == kind {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// Big-endian reads at an offset, bounds-checked.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn u8(&self, at: usize) -> Result<u8, CodecError> {
        self.0.get(at).copied().ok_or_else(|| bad("box is cut short"))
    }
    fn u16(&self, at: usize) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes(*self.0.get(at..).and_then(|s| s.first_chunk::<2>()).ok_or_else(|| bad("box is cut short"))?))
    }
    fn u32(&self, at: usize) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(*self.0.get(at..).and_then(|s| s.first_chunk::<4>()).ok_or_else(|| bad("box is cut short"))?))
    }
    fn u64(&self, at: usize) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(*self.0.get(at..).and_then(|s| s.first_chunk::<8>()).ok_or_else(|| bad("box is cut short"))?))
    }
}

/// What a `colr` box says: an ICC profile (`prof`, `rICC`) or a CICP description (`nclx`).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Colour {
    pub icc: Option<Vec<u8>>,
    pub cicp: Option<Cicp>,
}

impl Colour {
    fn read(&mut self, colr: &[u8]) -> Result<(), CodecError> {
        let r = Reader(colr);
        match colr.get(..4) {
            Some(b"nclx") => {
                self.cicp.get_or_insert(Cicp { primaries: r.u16(4)?, transfer: r.u16(6)?, matrix: r.u16(8)?, full_range: r.u8(10)? & 0x80 != 0 });
            }
            Some(b"prof" | b"rICC") if self.icc.is_none() => self.icc = Some(colr[4..].to_vec()),
            _ => {}
        }
        Ok(())
    }
}

/// The primary item's properties that change how its pixels are shown.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct ItemProps {
    pub colour: Colour,
    /// Anticlockwise quarter turns (`irot`), applied before the mirror.
    pub rotation: u8,
    /// `imir`: `Some(0)` flips top to bottom (about the horizontal axis), `Some(1)` left to right.
    pub mirror: Option<u8>,
}

/// The primary item's `colr`, `irot` and `imir` properties from the top-level `meta` box; empty
/// when the file has none (a sequence-only `avis`).
pub(crate) fn primary_props(file: &[u8]) -> Result<ItemProps, CodecError> {
    let mut props = ItemProps::default();
    let Some(meta) = find(file, b"meta")? else { return Ok(props) };
    let meta = meta.get(4..).ok_or_else(|| bad("meta box is cut short"))?;
    let Some(pitm) = find(meta, b"pitm")? else { return Ok(props) };
    let primary = if Reader(pitm).u8(0)? == 0 { u32::from(Reader(pitm).u16(4)?) } else { Reader(pitm).u32(4)? };
    let Some(iprp) = find(meta, b"iprp")? else { return Ok(props) };
    let Some(ipco) = find(iprp, b"ipco")? else { return Ok(props) };
    let all: Vec<([u8; 4], &[u8])> = boxes(ipco).collect::<Result<_, _>>()?;
    for r in boxes(iprp) {
        let (kind, ipma) = r?;
        if &kind != b"ipma" {
            continue;
        }
        let rd = Reader(ipma);
        let (version, wide_index) = (rd.u8(0)?, rd.u8(3)? & 1 != 0);
        let mut at = 8;
        for _ in 0..rd.u32(4)? {
            let id = if version < 1 {
                at += 2;
                u32::from(rd.u16(at - 2)?)
            } else {
                at += 4;
                rd.u32(at - 4)?
            };
            let count = rd.u8(at)?;
            at += 1;
            for _ in 0..count {
                let index = if wide_index {
                    at += 2;
                    usize::from(rd.u16(at - 2)? & 0x7fff)
                } else {
                    at += 1;
                    usize::from(rd.u8(at - 1)? & 0x7f)
                };
                // Index 0 means "no property"; the rest count from 1.
                let Some((kind, p)) = index.checked_sub(1).and_then(|i| all.get(i)) else { continue };
                if id != primary {
                    continue;
                }
                match kind {
                    b"colr" => props.colour.read(p)?,
                    b"irot" => props.rotation = Reader(p).u8(0)? & 3,
                    b"imir" => props.mirror = Some(Reader(p).u8(0)? & 1),
                    _ => {}
                }
            }
        }
    }
    Ok(props)
}

/// One frame of a track: its AV1 data and how long it shows.
pub(crate) struct Sample<'a> {
    pub data: &'a [u8],
    pub duration_ms: u32,
}

/// An animated AVIF's frames: the colour track, its alpha track when it has one, and the colour
/// description from the colour track's sample entry.
pub(crate) struct Sequence<'a> {
    pub width: u32,
    pub height: u32,
    pub color: Vec<Sample<'a>>,
    pub alpha: Option<Vec<Sample<'a>>>,
    pub colour: Colour,
}

struct Track<'a> {
    id: u32,
    handler: [u8; 4],
    /// The tracks this one is an auxiliary of (`tref`/`auxl`): an alpha track names its colour track.
    aux_of: Vec<u32>,
    width: u32,
    height: u32,
    colour: Colour,
    samples: Vec<Sample<'a>>,
}

/// The image sequence of an animated AVIF, `None` when the file has no `moov` (a still image).
/// At most `max_frames` frames are read; more is an error.
pub(crate) fn sequence(file: &[u8], max_frames: usize) -> Result<Option<Sequence<'_>>, CodecError> {
    let Some(moov) = find(file, b"moov")? else { return Ok(None) };
    let mut tracks = Vec::new();
    for r in boxes(moov) {
        let (kind, trak) = r?;
        if &kind == b"trak" {
            tracks.push(track(file, trak, max_frames)?);
        }
    }
    let Some(ci) = tracks.iter().position(|t| matches!(&t.handler, b"pict" | b"vide") && t.aux_of.is_empty()) else {
        return Err(bad("sequence has no picture track"));
    };
    let color = tracks.swap_remove(ci);
    if color.samples.is_empty() {
        return Err(bad("sequence has no frames"));
    }
    let alpha = tracks.into_iter().find(|t| &t.handler == b"auxv" && t.aux_of.contains(&color.id)).map(|t| t.samples);
    // An alpha track whose frames don't pair with the colour frames is left out rather than misapplied.
    let alpha = alpha.filter(|a| a.len() == color.samples.len());
    Ok(Some(Sequence { width: color.width, height: color.height, color: color.samples, alpha, colour: color.colour }))
}

fn track<'a>(file: &'a [u8], trak: &[u8], max_frames: usize) -> Result<Track<'a>, CodecError> {
    let tkhd = find(trak, b"tkhd")?.ok_or_else(|| bad("track has no header"))?;
    let id = Reader(tkhd).u32(if Reader(tkhd).u8(0)? == 1 { 20 } else { 12 })?;
    let mut aux_of = Vec::new();
    if let Some(tref) = find(trak, b"tref")? {
        for r in boxes(tref) {
            let (kind, ids) = r?;
            if &kind == b"auxl" {
                aux_of.extend(ids.as_chunks::<4>().0.iter().map(|c| u32::from_be_bytes(*c)));
            }
        }
    }
    let mdia = find(trak, b"mdia")?.ok_or_else(|| bad("track has no media"))?;
    let mdhd = find(mdia, b"mdhd")?.ok_or_else(|| bad("track has no media header"))?;
    let timescale = Reader(mdhd).u32(if Reader(mdhd).u8(0)? == 1 { 20 } else { 12 })?;
    let hdlr = find(mdia, b"hdlr")?.ok_or_else(|| bad("track has no handler"))?;
    let handler = *hdlr.get(8..12).and_then(|s| s.first_chunk::<4>()).ok_or_else(|| bad("handler box is cut short"))?;
    let stbl = find(mdia, b"minf")?.map(|m| find(m, b"stbl")).transpose()?.flatten().ok_or_else(|| bad("track has no sample table"))?;

    // The first sample entry: an `av01` visual sample entry (78 bytes of fields, then boxes).
    let stsd = find(stbl, b"stsd")?.ok_or_else(|| bad("track has no sample description"))?;
    let (entry_kind, entry) =
        boxes(stsd.get(8..).ok_or_else(|| bad("sample description is cut short"))?).next().ok_or_else(|| bad("track has no sample entry"))??;
    let (mut width, mut height, mut colour) = (0, 0, Colour::default());
    if &entry_kind == b"av01" {
        width = u32::from(Reader(entry).u16(24)?);
        height = u32::from(Reader(entry).u16(26)?);
        for r in boxes(entry.get(78..).unwrap_or(&[])) {
            let (kind, p) = r?;
            if &kind == b"colr" {
                colour.read(p)?;
            }
        }
    } else if matches!(&handler, b"pict" | b"vide" | b"auxv") {
        return Err(CodecError::unsupported(F, format!("a `{}` image sequence (only AV1 is read)", String::from_utf8_lossy(&entry_kind))));
    }

    // Sample sizes, durations and chunk offsets give each frame's bytes and time.
    let stsz = Reader(find(stbl, b"stsz")?.ok_or_else(|| bad("track has no sample sizes"))?);
    let (fixed, count) = (stsz.u32(4)?, stsz.u32(8)? as usize);
    if count > max_frames {
        return Err(CodecError::LimitExceeded(format!("more than {max_frames} frames")));
    }
    let size = |i: usize| -> Result<usize, CodecError> { Ok(if fixed != 0 { fixed } else { stsz.u32(12 + 4 * i)? } as usize) };
    let mut durations = Vec::with_capacity(count);
    if let Some(stts) = find(stbl, b"stts")? {
        let rd = Reader(stts);
        for e in 0..rd.u32(4)? as usize {
            let (n, delta) = (rd.u32(8 + 8 * e)? as usize, rd.u32(12 + 8 * e)?);
            let ms = if timescale == 0 { 0 } else { (u64::from(delta) * 1000 / u64::from(timescale)).min(u64::from(u32::MAX)) as u32 };
            durations.extend(std::iter::repeat_n(ms, n.min(count - durations.len().min(count))));
            if durations.len() >= count {
                break;
            }
        }
    }
    let offsets: Vec<u64> = if let Some(stco) = find(stbl, b"stco")? {
        let rd = Reader(stco);
        (0..rd.u32(4)? as usize).map(|i| rd.u32(8 + 4 * i).map(u64::from)).collect::<Result<_, _>>()?
    } else if let Some(co64) = find(stbl, b"co64")? {
        let rd = Reader(co64);
        (0..rd.u32(4)? as usize).map(|i| rd.u64(8 + 8 * i)).collect::<Result<_, _>>()?
    } else {
        return Err(bad("track has no chunk offsets"));
    };
    let stsc = Reader(find(stbl, b"stsc")?.ok_or_else(|| bad("track has no sample-to-chunk table"))?);
    let runs: Vec<(u32, u32)> = (0..stsc.u32(4)? as usize).map(|e| Ok((stsc.u32(8 + 12 * e)?, stsc.u32(12 + 12 * e)?))).collect::<Result<_, CodecError>>()?;
    let mut samples = Vec::with_capacity(count);
    for (chunk, &offset) in offsets.iter().enumerate() {
        // The last run starting at or before this (1-based) chunk says how many samples it holds.
        let per_chunk = runs.iter().rev().find(|(first, _)| *first as usize <= chunk + 1).map_or(0, |r| r.1);
        let mut at = usize::try_from(offset).map_err(|_| bad("chunk offset is out of range"))?;
        for _ in 0..per_chunk {
            if samples.len() == count {
                break;
            }
            let n = size(samples.len())?;
            let data = at.checked_add(n).and_then(|end| file.get(at..end)).ok_or_else(|| bad("frame data runs past the end of the file"))?;
            samples.push(Sample { data, duration_ms: durations.get(samples.len()).copied().unwrap_or(0) });
            at += n;
        }
    }
    if samples.len() != count {
        return Err(bad("sample table lists more frames than its chunks hold"));
    }
    Ok(Track { id, handler, aux_of, width, height, colour, samples })
}
