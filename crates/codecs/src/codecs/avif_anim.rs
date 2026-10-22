//! Animated AVIF writing: every frame through one rav1e encoder (so later frames code only what
//! changed), the alpha plane through a second monochrome one when any frame is see-through, and
//! an `avis` file around them as libavif lays one out (AVIF §7, AV1-ISOBMFF): a `moov` with a
//! colour track and an alpha track that references it, plus the first frame as the primary still
//! item for readers that don't play sequences.
//!
//! Pixels are 8-bit 4:4:4 BT.601 YCbCr, full range, tagged sRGB (CICP 1/13/6), the way the still
//! AVIF encoder writes them; the quality maps to rav1e's quantizer on ravif's curve.

use rav1e::prelude::*;

use crate::Format;
use crate::error::CodecError;
use crate::web::AvifFrame;

const F: Format = Format::Avif;
/// Track and movie timescale: frame durations are stored in milliseconds.
const TIMESCALE: u32 = 1000;
const ALPHA_URN: &[u8] = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0";

fn err(e: impl std::fmt::Display) -> CodecError {
    CodecError::encode(F, e)
}

/// ravif's quality (1–100) to quantizer (0–255) curve, so a Quality reads the same as a still AVIF's.
fn quantizer(quality: u8) -> usize {
    let q = f32::from(quality.clamp(1, 100)) / 100.0;
    let x = if q >= 0.82 {
        (1.0 - q) * 2.6
    } else if q > 0.25 {
        q.mul_add(-0.5, 0.875)
    } else {
        1.0 - q
    };
    (x * 255.0).round() as usize
}

/// A rav1e encoder for `width` × `height` frames, colour (4:4:4) or a monochrome alpha plane.
fn encoder(width: u32, height: u32, quality: u8, speed: u8, alpha: bool) -> Result<Context<u8>, CodecError> {
    let q = quantizer(quality);
    let cfg = EncoderConfig {
        width: width as usize,
        height: height as usize,
        time_base: Rational::new(1, u64::from(TIMESCALE)),
        bit_depth: 8,
        chroma_sampling: if alpha { ChromaSampling::Cs400 } else { ChromaSampling::Cs444 },
        pixel_range: PixelRange::Full,
        color_description: (!alpha).then_some(ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::SRGB,
            matrix_coefficients: MatrixCoefficients::BT601,
        }),
        still_picture: false,
        // No reordering: one packet per frame, in display order.
        low_latency: true,
        quantizer: q,
        min_quantizer: q.min(255) as u8,
        speed_settings: SpeedSettings::from_preset(speed.min(10)),
        ..Default::default()
    };
    Config::new().with_encoder_config(cfg).new_context().map_err(err)
}

/// Feeds `planes` (one per frame, each plane `width` samples a row) through `ctx`, returning each
/// frame's AV1 data and whether it is a key frame.
fn encode_planes(mut ctx: Context<u8>, width: usize, frames: impl Iterator<Item = [Vec<u8>; 3]>, planes: usize) -> Result<Vec<(Vec<u8>, bool)>, CodecError> {
    let mut out = Vec::new();
    let drain = |ctx: &mut Context<u8>, out: &mut Vec<(Vec<u8>, bool)>| -> Result<bool, CodecError> {
        loop {
            match ctx.receive_packet() {
                Ok(p) => out.push((p.data, p.frame_type == FrameType::KEY)),
                Err(EncoderStatus::Encoded) => {}
                Err(EncoderStatus::NeedMoreData) => return Ok(false),
                Err(EncoderStatus::LimitReached) => return Ok(true),
                Err(e) => return Err(err(e)),
            }
        }
    };
    for data in frames {
        let mut f = ctx.new_frame();
        for (plane, samples) in f.planes.iter_mut().zip(&data).take(planes) {
            plane.copy_from_raw_u8(samples, width, 1);
        }
        ctx.send_frame(f).map_err(err)?;
        drain(&mut ctx, &mut out)?;
    }
    ctx.flush();
    while !drain(&mut ctx, &mut out)? {}
    Ok(out)
}

/// Encodes `frames` (straight RGBA8, `width` × `height` each) as an animated AVIF at `quality`
/// (1–100) and rav1e `speed` (0–10). `loops` is how many times it plays, 0 for forever.
pub(crate) fn encode(width: u32, height: u32, frames: &[AvifFrame], quality: u8, speed: u8, loops: u16) -> Result<Vec<u8>, CodecError> {
    // rav1e codes an image sequence (unlike a still picture) only from 16 × 16 up.
    if !(16..=65535).contains(&width) || !(16..=65535).contains(&height) {
        return Err(err(format!("an animated AVIF must be 16 to 65535 pixels a side, not {width} × {height}")));
    }
    if frames.is_empty() {
        return Err(err("an animation needs at least one frame"));
    }
    let n = width as usize * height as usize;
    if frames.iter().any(|f| f.rgba.len() != n * 4) {
        return Err(err("every frame must hold width × height RGBA pixels"));
    }
    // BT.601, full range, as `rgb_to_ycbcr` in ravif.
    let (kr, kb) = (0.299f32, 0.114f32);
    let kg = 1.0 - kr - kb;
    let to_yuv = |f: &AvifFrame| {
        let (mut y, mut u, mut v) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
        for p in f.rgba.as_chunks::<4>().0 {
            let [r, g, b] = [p[0], p[1], p[2]].map(f32::from);
            let luma = kr * r + kg * g + kb * b;
            y.push(luma.round().clamp(0.0, 255.0) as u8);
            u.push(((b - luma) * 0.5 / (1.0 - kb) + 128.0).round().clamp(0.0, 255.0) as u8);
            v.push(((r - luma) * 0.5 / (1.0 - kr) + 128.0).round().clamp(0.0, 255.0) as u8);
        }
        [y, u, v]
    };
    let color = encode_planes(encoder(width, height, quality, speed, false)?, width as usize, frames.iter().map(to_yuv), 3)?;
    let see_through = frames.iter().any(|f| f.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255));
    let alpha = if see_through {
        let planes = frames.iter().map(|f| [f.rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect(), Vec::new(), Vec::new()]);
        // Alpha edges show more than colour noise: halfway to lossless, as ravif's default.
        let alpha_quality = ((u16::from(quality) + 100) / 2) as u8;
        Some(encode_planes(encoder(width, height, alpha_quality, speed, true)?, width as usize, planes, 1)?)
    } else {
        None
    };
    if color.len() != frames.len() || alpha.as_ref().is_some_and(|a| a.len() != frames.len()) {
        return Err(err("the AV1 encoder returned a different number of frames"));
    }
    let durations: Vec<u32> = frames.iter().map(|f| if f.delay_ms == 0 { 100 } else { f.delay_ms }).collect();
    let color_seq = encoder(width, height, quality, speed, false)?.container_sequence_header();
    let alpha_seq = see_through.then(|| encoder(width, height, quality, speed, true).map(|c| c.container_sequence_header())).transpose()?;
    let tracks = Tracks {
        width,
        height,
        durations: &durations,
        loops,
        color: &color,
        color_av1c: &color_seq,
        alpha: alpha.as_deref(),
        alpha_av1c: alpha_seq.as_deref(),
    };
    // Offsets into `mdat` depend on the size of the boxes before it, which don't depend on the
    // offsets: lay it out once to measure, then again with the real start.
    let probe = tracks.header(0)?;
    let start = probe.len() as u64 + 8;
    let mut file = tracks.header(start)?;
    let payload: usize = color.iter().chain(alpha.iter().flatten()).map(|(d, _)| d.len()).sum();
    file.extend_from_slice(&u32::try_from(payload + 8).map_err(|_| err("the animation is too large for one AVIF file"))?.to_be_bytes());
    file.extend_from_slice(b"mdat");
    for (d, _) in color.iter().chain(alpha.iter().flatten()) {
        file.extend_from_slice(d);
    }
    Ok(file)
}

/// A box: size, type, body.
fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let len: usize = 8 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut b = Vec::with_capacity(len);
    b.extend_from_slice(&(len as u32).to_be_bytes());
    b.extend_from_slice(kind);
    for p in parts {
        b.extend_from_slice(p);
    }
    b
}

/// A full box: a box whose body starts with a version and 24 bits of flags.
fn full(kind: &[u8; 4], version: u8, flags: u32, parts: &[&[u8]]) -> Vec<u8> {
    let vf = (u32::from(version) << 24 | (flags & 0xff_ffff)).to_be_bytes();
    let mut all: Vec<&[u8]> = vec![&vf];
    all.extend_from_slice(parts);
    bx(kind, &all)
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// The identity transform of `mvhd` and `tkhd`.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn matrix() -> Vec<u8> {
    MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// The `nclx` colour box for sRGB, BT.601 matrix, full range.
fn colr() -> Vec<u8> {
    bx(b"colr", &[b"nclx", &1u16.to_be_bytes(), &13u16.to_be_bytes(), &6u16.to_be_bytes(), &[0x80]])
}

struct Tracks<'a> {
    width: u32,
    height: u32,
    durations: &'a [u32],
    loops: u16,
    color: &'a [(Vec<u8>, bool)],
    color_av1c: &'a [u8],
    alpha: Option<&'a [(Vec<u8>, bool)]>,
    alpha_av1c: Option<&'a [u8]>,
}

impl Tracks<'_> {
    /// One pass through every frame, in milliseconds.
    fn duration(&self) -> u32 {
        self.durations.iter().fold(0u32, |a, d| a.saturating_add(*d))
    }

    /// How long the animation plays in all: every pass, or all ones for forever.
    fn presentation(&self) -> u32 {
        match self.loops {
            0 => u32::MAX,
            n => self.duration().saturating_mul(u32::from(n)),
        }
    }

    /// Everything before `mdat`, for an `mdat` whose payload starts at file offset `start`.
    fn header(&self, start: u64) -> Result<Vec<u8>, CodecError> {
        let off = |o: u64| u32::try_from(o).map_err(|_| err("the animation is too large for one AVIF file"));
        let color_len: u64 = self.color.iter().map(|(d, _)| d.len() as u64).sum();
        let color_at = off(start)?;
        let alpha_at = off(start + color_len)?;
        let mut compatible = Vec::new();
        for b in [b"avif", b"avis", b"msf1", b"iso8", b"mif1", b"miaf", b"MA1B"] {
            compatible.extend_from_slice(b);
        }
        let ftyp = bx(b"ftyp", &[b"avis", &be32(0), &compatible]);
        let meta = self.meta(color_at, alpha_at)?;
        let moov = self.moov(color_at, alpha_at);
        Ok([ftyp, meta, moov].concat())
    }

    /// The first frame as the primary still item (and its alpha as an auxiliary item), pointing
    /// at the same bytes as the tracks' first samples.
    fn meta(&self, color_at: u32, alpha_at: u32) -> Result<Vec<u8>, CodecError> {
        let first_len = |s: &[(Vec<u8>, bool)]| s.first().map_or(0, |(d, _)| d.len() as u32);
        let hdlr = full(b"hdlr", 0, 0, &[&be32(0), b"pict", &[0; 12], b"\0"]);
        let pitm = full(b"pitm", 0, 0, &[&1u16.to_be_bytes()]);
        // iloc v0: 4-byte offsets and lengths, no base offset; one extent per item.
        let item = |id: u16, at: u32, len: u32| [id.to_be_bytes().as_slice(), &0u16.to_be_bytes(), &1u16.to_be_bytes(), &be32(at), &be32(len)].concat();
        let mut items = item(1, color_at, first_len(self.color));
        let count: u16 = if self.alpha.is_some() { 2 } else { 1 };
        if let Some(a) = self.alpha {
            items.extend(item(2, alpha_at, first_len(a)));
        }
        let iloc = full(b"iloc", 0, 0, &[&[0x44, 0x00], &count.to_be_bytes(), &items]);
        let infe = |id: u16| full(b"infe", 2, 0, &[&id.to_be_bytes(), &0u16.to_be_bytes(), b"av01", b"\0"]);
        let mut infes = infe(1);
        if self.alpha.is_some() {
            infes.extend(infe(2));
        }
        let iinf = full(b"iinf", 0, 0, &[&count.to_be_bytes(), &infes]);
        let ispe = full(b"ispe", 0, 0, &[&be32(self.width), &be32(self.height)]);
        let pixi = |channels: u8| full(b"pixi", 0, 0, &[&[channels], &vec![8u8; usize::from(channels)]]);
        let av1c = |c: &[u8]| bx(b"av1C", &[c]);
        // Properties, numbered from 1: 1 ispe, 2 pixi(3), 3 av1C, 4 colr; then 5 pixi(1), 6 av1C, 7 auxC.
        let mut ipco = [ispe, pixi(3), av1c(self.color_av1c), colr()].concat();
        // ipma entries: item id, count, then property indices (high bit = essential).
        let mut ipma_items = [1u16.to_be_bytes().as_slice(), &[4, 1, 2, 0x83, 4]].concat();
        let mut iref = Vec::new();
        if let Some(c) = self.alpha_av1c {
            ipco.extend([pixi(1), av1c(c), full(b"auxC", 0, 0, &[ALPHA_URN])].concat());
            ipma_items.extend([2u16.to_be_bytes().as_slice(), &[4, 1, 5, 0x86, 0x87]].concat());
            // The alpha item is an auxiliary image of the colour item.
            iref = full(b"iref", 0, 0, &[&bx(b"auxl", &[&2u16.to_be_bytes(), &1u16.to_be_bytes(), &1u16.to_be_bytes()])]);
        }
        let ipma = full(b"ipma", 0, 0, &[&be32(u32::from(count)), &ipma_items]);
        let iprp = bx(b"iprp", &[&bx(b"ipco", &[&ipco]), &ipma]);
        Ok(full(b"meta", 0, 0, &[&hdlr, &pitm, &iloc, &iinf, &iref, &iprp]))
    }

    fn moov(&self, color_at: u32, alpha_at: u32) -> Vec<u8> {
        let duration = self.duration();
        let next_track = if self.alpha.is_some() { 3 } else { 2 };
        let mvhd = full(
            b"mvhd",
            0,
            0,
            &[
                &be32(0),
                &be32(0),
                &be32(TIMESCALE),
                &be32(self.presentation()),
                &be32(0x0001_0000),
                &0x0100u16.to_be_bytes(),
                &[0; 10],
                &matrix(),
                &[0; 24],
                &be32(next_track),
            ],
        );
        let mut tracks = vec![self.trak(1, None, self.color, self.color_av1c, color_at, duration)];
        if let (Some(a), Some(c)) = (self.alpha, self.alpha_av1c) {
            tracks.push(self.trak(2, Some(1), a, c, alpha_at, duration));
        }
        bx(b"moov", &[&mvhd, &tracks.concat()])
    }

    /// One track: `aux_of` names the colour track for the alpha one.
    fn trak(&self, id: u32, aux_of: Option<u32>, samples: &[(Vec<u8>, bool)], av1c: &[u8], at: u32, duration: u32) -> Vec<u8> {
        let fixed = |v: u32| be32(v << 16);
        let tkhd = full(
            b"tkhd",
            0,
            1,
            &[
                &be32(0),
                &be32(0),
                &be32(id),
                &be32(0),
                &be32(self.presentation()),
                &[0; 8],
                &[0; 4],
                &[0; 4],
                &matrix(),
                &fixed(self.width),
                &fixed(self.height),
            ],
        );
        let tref = aux_of.map(|c| bx(b"tref", &[&bx(b"auxl", &[&be32(c)])])).unwrap_or_default();
        // One pass of the media, repeated (flag 1) for as long as the track header's duration
        // says: the play count, as libavif writes and reads it.
        let elst = full(b"elst", 0, 1, &[&be32(1), &be32(duration), &be32(0), &be32(0x0001_0000)]);
        let edts = bx(b"edts", &[&elst]);
        let mdhd = full(b"mdhd", 0, 0, &[&be32(0), &be32(0), &be32(TIMESCALE), &be32(duration), &0x55c4u16.to_be_bytes(), &[0; 2]]);
        let hdlr = full(b"hdlr", 0, 0, &[&be32(0), if aux_of.is_some() { b"auxv" } else { b"pict" }, &[0; 12], b"\0"]);
        let vmhd = full(b"vmhd", 0, 1, &[&[0; 8]]);
        let dinf = bx(b"dinf", &[&full(b"dref", 0, 0, &[&be32(1), &full(b"url ", 0, 1, &[])])]);
        // The av01 visual sample entry: 78 bytes of fields, then its boxes.
        let mut name = [0u8; 32];
        name[..4].copy_from_slice(b"\x03AOM");
        let ccst = full(b"ccst", 0, 0, &[&be32(0x7c00_0000)]);
        let extra = match aux_of {
            Some(_) => full(b"auxi", 0, 0, &[ALPHA_URN]),
            None => colr(),
        };
        let av01 = bx(
            b"av01",
            &[
                &[0; 6],
                &1u16.to_be_bytes(),
                &[0; 16],
                &(self.width as u16).to_be_bytes(),
                &(self.height as u16).to_be_bytes(),
                &be32(0x0048_0000),
                &be32(0x0048_0000),
                &be32(0),
                &1u16.to_be_bytes(),
                &name,
                &0x0018u16.to_be_bytes(),
                &0xffffu16.to_be_bytes(),
                &bx(b"av1C", &[av1c]),
                &extra,
                &ccst,
            ],
        );
        let stsd = full(b"stsd", 0, 0, &[&be32(1), &av01]);
        // Runs of equal durations.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &d in self.durations {
            match runs.last_mut() {
                Some((n, last)) if *last == d => *n += 1,
                _ => runs.push((1, d)),
            }
        }
        let stts_body: Vec<u8> = runs.iter().flat_map(|(n, d)| [be32(*n), be32(*d)].concat()).collect();
        let stts = full(b"stts", 0, 0, &[&be32(runs.len() as u32), &stts_body]);
        let n = samples.len() as u32;
        let stsc = full(b"stsc", 0, 0, &[&be32(1), &be32(1), &be32(n), &be32(1)]);
        let sizes: Vec<u8> = samples.iter().flat_map(|(d, _)| be32(d.len() as u32)).collect();
        let stsz = full(b"stsz", 0, 0, &[&be32(0), &be32(n), &sizes]);
        let stco = full(b"stco", 0, 0, &[&be32(1), &be32(at)]);
        let keys: Vec<u8> = samples.iter().enumerate().filter(|(_, (_, key))| *key).flat_map(|(i, _)| be32(i as u32 + 1)).collect();
        let stss = full(b"stss", 0, 0, &[&be32((keys.len() / 4) as u32), &keys]);
        let stbl = bx(b"stbl", &[&stsd, &stts, &stsc, &stsz, &stco, &stss]);
        let minf = bx(b"minf", &[&vmhd, &dinf, &stbl]);
        let mdia = bx(b"mdia", &[&mdhd, &hdlr, &minf]);
        bx(b"trak", &[&tkhd, &tref, &edts, &mdia])
    }
}
